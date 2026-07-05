use super::*;

/// Which attempts to compare (NER-137). With neither field set, `compare_attempts`
/// returns every intent that has ≥1 attempt; `intent_id` filters to one intent;
/// `attempt_id` scopes to that attempt's intent. Unknown selectors raise the existing
/// `UnknownIntent`/`UnknownAttempt` typed errors — multiple intents are *grouped*,
/// not an ambiguity error.
#[derive(Debug, Clone, Default)]
pub struct CompareSelector {
    pub intent_id: Option<String>,
    pub attempt_id: Option<String>,
}

/// The compare/rank result: competing attempts grouped per intent, each group ranked
/// (NER-137 R1/R2). The headline read surface that lets a human or agent select a
/// winner from verified data and chain `compare → accept` headlessly.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptComparison {
    pub intents: Vec<IntentComparison>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntentComparison {
    pub intent_id: String,
    pub intent: String,
    pub attempts: Vec<AttemptCompareRow>,
}

/// One competing attempt's compare row. Ranking is **advisory**; the per-gate results,
/// metrics, integrity label, and raw changed paths are **authoritative** and always
/// present. `rank` is `None` for an attempt that is unrankable — its deciding evidence
/// failed the (cheap per-row) integrity check (`integrity == "tampered"`) or it has no
/// proposal yet — so a headless consumer that selects by numeric-minimum rank can
/// never pick a tampered attempt (NER-137 R4).
#[derive(Debug, Clone, Serialize)]
pub struct AttemptCompareRow {
    pub attempt_id: String,
    pub status: String,
    pub proposal: Option<ComparedProposal>,
    /// Secret-redacted file-level diff summary of the proposal vs its base — the paths
    /// the snapshot changed (the per-attempt "diff summary"). The richer pairwise
    /// file/hunk content diff is the CLI's backend-routed `compare --diff` path.
    pub changed_paths: Vec<String>,
    pub changed_count: usize,
    pub gates: Vec<forge_policy::GateResult>,
    pub check_status: Option<String>,
    pub metrics: StructuredMetrics,
    /// `"verified"` (deciding rows pass the cheap per-row check), `"legacy_unverified"`
    /// (a pre-Phase-5 grandfathered row), `"tampered"` (a deciding row failed the cheap
    /// check — recorded here, NOT propagated, so one bad attempt does not blank the
    /// comparison), or `"no_evidence"` (no deciding evidence / no proposal). The deep
    /// recompute-row-hash case is `doctor`'s op-walk, not this cheap label.
    pub integrity: String,
    pub decision_status: Option<String>,
    pub publication_status: Option<String>,
    pub rank: Option<u32>,
    pub rank_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparedProposal {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
}

/// Parsed numeric metrics aggregated over the proposal-snapshot evidence (NER-137).
/// Test counts drive the ranking metric tier; clippy findings are surfaced but do not
/// influence the order in v0 (R3 narrowing — callers re-rank on these raw metrics).
#[derive(Debug, Clone, Serialize, Default)]
pub struct StructuredMetrics {
    pub tests_passed: Option<u64>,
    pub tests_failed: Option<u64>,
    pub tests_ignored: Option<u64>,
    pub clippy_findings: Option<u64>,
}

pub(crate) const INTEGRITY_VERIFIED: &str = "verified";
pub(crate) const INTEGRITY_LEGACY: &str = "legacy_unverified";
pub(crate) const INTEGRITY_TAMPERED: &str = "tampered";
pub(crate) const INTEGRITY_NO_EVIDENCE: &str = "no_evidence";

/// Evidence-based attempt comparison and ranking (NER-137, Phase 6). Read-only — opens
/// a throwaway connection, takes **no** advisory lock (it never writes). Its ranking is
/// a snapshot a concurrent lock-free `run` can invalidate, so it is **advisory**;
/// `accept`/`decide` keep the authoritative in-txn gate. Ranks on Phase 5
/// cheaply-verified evidence: a deciding row that fails the per-row check labels the
/// attempt `tampered` and leaves it unranked (`rank: null`).
pub fn compare_attempts(cwd: &Path, selector: CompareSelector) -> Result<AttemptComparison> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let marker = evidence_high_water(&connection)?;

    let intent_ids = resolve_compare_intents(&context, &connection, &selector)?;
    let mut intents = Vec::new();
    for (intent_id, intent_text) in intent_ids {
        let attempts = attempts_for_intent(&connection, &context.repo_id, &intent_id)?;
        let mut rows = Vec::new();
        for attempt in attempts {
            rows.push(build_compare_row(&context, &connection, &attempt, marker)?);
        }
        rank_compare_rows(&mut rows);
        intents.push(IntentComparison {
            intent_id,
            intent: intent_text,
            attempts: rows,
        });
    }
    Ok(AttemptComparison { intents })
}

/// Resolve the `(intent_id, intent_text)` groups a `CompareSelector` names. An
/// `attempt_id` maps to its intent; an `intent_id` filters to that one; neither
/// returns every intent with ≥1 attempt, ordered by first attempt.
fn resolve_compare_intents(
    context: &RepositoryContext,
    conn: &Connection,
    selector: &CompareSelector,
) -> Result<Vec<(String, String)>> {
    if let Some(attempt_id) = &selector.attempt_id {
        let attempt =
            attempt_by_id(context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
                selector: attempt_id.clone(),
            })?;
        return Ok(vec![(attempt.intent_id, attempt.intent)]);
    }
    if let Some(intent_id) = &selector.intent_id {
        let text: Option<String> = conn
            .query_row(
                "SELECT text FROM intents WHERE id = ?1",
                params![intent_id],
                |row| row.get(0),
            )
            .optional()?;
        let text = text.ok_or_else(|| ForgeError::UnknownIntent {
            selector: intent_id.clone(),
        })?;
        return Ok(vec![(intent_id.clone(), text)]);
    }
    // All intents that have ≥1 attempt, ordered by the intent's first attempt so the
    // grouping is deterministic.
    let mut statement = conn.prepare(
        "SELECT i.id, i.text, MIN(a.created_at_ms) AS first_attempt
         FROM intents i
         JOIN attempts a ON a.intent_id = i.id
         WHERE i.repo_id = ?1
         GROUP BY i.id, i.text
         ORDER BY first_attempt ASC, i.id ASC",
    )?;
    let rows = statement.query_map(params![context.repo_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub(crate) fn attempts_for_intent(
    conn: &Connection,
    repo_id: &str,
    intent_id: &str,
) -> Result<Vec<AttemptRecord>> {
    let mut statement = conn.prepare(
        "SELECT a.id, a.intent_id, i.text, a.base_head, a.status
         FROM attempts a
         JOIN intents i ON i.id = a.intent_id
         WHERE a.repo_id = ?1 AND a.intent_id = ?2
         ORDER BY a.created_at_ms ASC, a.id ASC",
    )?;
    let rows = statement.query_map(params![repo_id, intent_id], |row| {
        Ok(AttemptRecord {
            attempt_id: row.get(0)?,
            intent_id: row.get(1)?,
            intent: row.get(2)?,
            base_head: row.get(3)?,
            status: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Build one attempt's compare row. Runs the Phase 4 policy evaluation for per-gate
/// verdicts and the Phase 5 per-row integrity check, **recording** (not propagating) a
/// tamper as a label. No git is invoked here (the store stays git-free, PRD §23.4); the
/// per-attempt diff summary is the stored, secret-redacted `changed_paths`.
fn build_compare_row(
    context: &RepositoryContext,
    conn: &Connection,
    attempt: &AttemptRecord,
    marker: i64,
) -> Result<AttemptCompareRow> {
    let proposal = latest_proposal_for_attempt(context, &attempt.attempt_id)?;
    let Some(proposal) = proposal else {
        return Ok(AttemptCompareRow {
            attempt_id: attempt.attempt_id.clone(),
            status: attempt.status.clone(),
            proposal: None,
            changed_paths: Vec::new(),
            changed_count: 0,
            gates: Vec::new(),
            check_status: None,
            metrics: StructuredMetrics::default(),
            integrity: INTEGRITY_NO_EVIDENCE.to_string(),
            decision_status: None,
            publication_status: None,
            rank: None,
            rank_reason: "no proposal yet".to_string(),
        });
    };

    let spec = intent_check_spec(conn, &attempt.intent_id)?;
    let facts = evidence_facts_on(conn, &attempt.attempt_id)?;
    let outcome = forge_policy::evaluate(&spec, &proposal.snapshot_id, &facts);
    let integrity = aggregate_integrity(conn, &outcome, marker)?;
    let metrics = compare_structured_metrics(conn, &attempt.attempt_id, &proposal.snapshot_id)?;

    let (kept_paths, _dropped) = forge_content::filter_secret_risk(&proposal.changed_paths);
    let changed_count = kept_paths.len();
    let gates = outcome.gates.into_iter().map(redact_gate_result).collect();
    let decision_status =
        latest_decision_for_proposal_revision(context, &proposal.proposal_revision_id)?;
    let publication_status =
        latest_publication_for_proposal_revision(context, &proposal.proposal_revision_id)?
            .map(|_| "published".to_string());

    Ok(AttemptCompareRow {
        attempt_id: attempt.attempt_id.clone(),
        status: attempt.status.clone(),
        proposal: Some(ComparedProposal {
            proposal_id: proposal.proposal_id,
            proposal_revision_id: proposal.proposal_revision_id,
            snapshot_id: proposal.snapshot_id,
            base_head: proposal.base_head,
            content_ref: proposal.content_ref,
        }),
        changed_paths: kept_paths,
        changed_count,
        gates,
        check_status: Some(outcome.status),
        metrics,
        integrity,
        decision_status,
        publication_status,
        rank: None, // assigned by rank_compare_rows
        rank_reason: String::new(),
    })
}

/// Aggregate the per-row integrity of the gates' deciding evidence into one label,
/// fail-closed toward the strongest signal: any tampered deciding row → `tampered`;
/// else any legacy → `legacy_unverified`; else if at least one gate has deciding
/// evidence → `verified`; else `no_evidence` (e.g. a `missing` gate set).
fn aggregate_integrity(
    conn: &Connection,
    outcome: &forge_policy::CheckOutcome,
    marker: i64,
) -> Result<String> {
    let mut any_deciding = false;
    let mut any_legacy = false;
    for gate in &outcome.gates {
        if let Some(evidence_id) = &gate.evidence_id {
            any_deciding = true;
            match verify_evidence_integrity(conn, evidence_id, marker)? {
                IntegrityStatus::Tampered(_) => return Ok(INTEGRITY_TAMPERED.to_string()),
                IntegrityStatus::LegacyUnverified => any_legacy = true,
                IntegrityStatus::Verified => {}
            }
        }
    }
    Ok(if any_legacy {
        INTEGRITY_LEGACY.to_string()
    } else if any_deciding {
        INTEGRITY_VERIFIED.to_string()
    } else {
        INTEGRITY_NO_EVIDENCE.to_string()
    })
}

/// Aggregate parsed structured metrics over the proposal-snapshot evidence: sum test
/// counts across test binaries/rows and clippy findings across clippy rows.
fn compare_structured_metrics(
    conn: &Connection,
    attempt_id: &str,
    snapshot_id: &str,
) -> Result<StructuredMetrics> {
    let mut statement = conn.prepare(
        "SELECT structured_json FROM evidence
         WHERE attempt_id = ?1 AND snapshot_id = ?2 AND structured_json IS NOT NULL
         ORDER BY created_at_ms ASC, rowid ASC",
    )?;
    let rows = statement.query_map(params![attempt_id, snapshot_id], |row| {
        row.get::<_, String>(0)
    })?;
    let mut metrics = StructuredMetrics::default();
    for json in rows {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&json?) else {
            continue;
        };
        let get = |key: &str| value.get(key).and_then(serde_json::Value::as_u64);
        metrics.tests_passed = add_opt(metrics.tests_passed, get("passed"));
        metrics.tests_failed = add_opt(metrics.tests_failed, get("failed"));
        metrics.tests_ignored = add_opt(metrics.tests_ignored, get("ignored"));
        metrics.clippy_findings = add_opt(metrics.clippy_findings, get("findings"));
    }
    Ok(metrics)
}

fn add_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(x + y),
    }
}

/// Which of the two adjacent rows the caller is describing: the higher-ranked `Above`
/// (phrase the discriminator as its advantage) or the lower-ranked `Below` (phrase it as
/// the reason it landed lower).
enum TieSide {
    Above,
    Below,
}

/// Name the FIRST sort-key field (after the gate tier, assumed equal here) on which the
/// higher-ranked `above` strictly beats `below` — i.e. the tie-break that actually placed
/// `below` after `above`. The label is phrased from `side`'s perspective: `Above` reads as
/// the winner's advantage, `Below` as why the loser ranked lower. Returns `None` only when
/// the two rows are equal on every tie-break field (a stable-order tie), so the caller can
/// fall back to a neutral phrasing rather than claim a discriminator that did not apply
/// (NER-256).
fn tie_break_discriminator(
    above: &AttemptCompareRow,
    below: &AttemptCompareRow,
    side: TieSide,
) -> Option<String> {
    let integrity_rank = |row: &AttemptCompareRow| match row.integrity.as_str() {
        INTEGRITY_VERIFIED => 0u8,
        INTEGRITY_LEGACY => 1,
        _ => 2,
    };
    if integrity_rank(below) != integrity_rank(above) {
        let target = match side {
            TieSide::Above => above,
            TieSide::Below => below,
        };
        let label = match target.integrity.as_str() {
            INTEGRITY_VERIFIED => "verified evidence",
            INTEGRITY_LEGACY => "legacy_unverified evidence",
            _ => "no-evidence integrity",
        };
        return Some(label.to_string());
    }
    let empty = |row: &AttemptCompareRow| row.changed_count == 0;
    if empty(below) != empty(above) {
        // The winner has the non-empty diff; the loser has the empty one.
        return Some(match side {
            TieSide::Above => "non-empty diff".to_string(),
            TieSide::Below => "empty diff".to_string(),
        });
    }
    let below_failed = below.metrics.tests_failed.unwrap_or(u64::MAX);
    let above_failed = above.metrics.tests_failed.unwrap_or(u64::MAX);
    if below_failed != above_failed {
        return Some(match side {
            TieSide::Above => format!("fewer failing tests ({above_failed} vs {below_failed})"),
            TieSide::Below => format!("more failing tests ({below_failed} vs {above_failed})"),
        });
    }
    let below_passed = below.metrics.tests_passed.unwrap_or(0);
    let above_passed = above.metrics.tests_passed.unwrap_or(0);
    if below_passed != above_passed {
        return Some(match side {
            TieSide::Above => format!("more passing tests ({above_passed} vs {below_passed})"),
            TieSide::Below => format!("fewer passing tests ({below_passed} vs {above_passed})"),
        });
    }
    None
}

/// Assign a deterministic total-order rank to the rankable rows of one intent group
/// (NER-137 R3/R4). Rankable = has a proposal AND not `tampered`. Order: all-required-
/// gates-passing first; within a tier fewer parsed test failures, then more parsed
/// passing; stable ties keep first-attempt order (input is created-order). Tampered /
/// no-proposal rows get `rank: null` and are placed after the ranked rows.
pub(crate) fn rank_compare_rows(rows: &mut Vec<AttemptCompareRow>) {
    let rankable =
        |row: &AttemptCompareRow| row.proposal.is_some() && row.integrity != INTEGRITY_TAMPERED;
    // Partition while preserving input (created) order for stable ties.
    let mut ranked: Vec<AttemptCompareRow> = Vec::new();
    let mut unranked: Vec<AttemptCompareRow> = Vec::new();
    for row in rows.drain(..) {
        if rankable(&row) {
            ranked.push(row);
        } else {
            unranked.push(row);
        }
    }
    // NER-256 tie-break, applied ONLY after gates_passing so a passing gate still
    // strictly outranks a non-passing one. When the gate status ties (e.g. all gates
    // missing/unmet), prefer (a) higher-integrity evidence, then (b) a non-empty diff,
    // then (c) fewer test failures / more passes — so a zero-change, no-evidence attempt
    // can no longer outrank a verified attempt with a real diff purely on created order.
    // Tampered rows never reach this sort (partitioned into `unranked`), so integrity
    // here is only verified / legacy_unverified / no_evidence.
    let sort_key = |row: &AttemptCompareRow| {
        let gates_passing = if row.check_status.as_deref() == Some("passed") {
            0u8
        } else {
            1
        };
        let integrity_rank = match row.integrity.as_str() {
            INTEGRITY_VERIFIED => 0u8,
            INTEGRITY_LEGACY => 1,
            _ => 2, // INTEGRITY_NO_EVIDENCE
        };
        let empty_diff = if row.changed_count > 0 { 0u8 } else { 1 };
        (
            gates_passing,
            integrity_rank,
            empty_diff,
            row.metrics.tests_failed.unwrap_or(u64::MAX),
            std::cmp::Reverse(row.metrics.tests_passed.unwrap_or(0)),
        )
    };
    ranked.sort_by_key(sort_key);
    // Did EVERY ranked attempt land in the same gate tier? Only then is "gates tie" an
    // honest description for a non-passing row: if any attempt passed its gates, the
    // non-passing remainder did not tie on gates — they lost on `gates_passing` (NER-256
    // correctness review). When the gate tier truly ties, the per-row reason names the
    // discriminator that actually separated THIS row from the one ranked just above it
    // (integrity, diff, or test counts) rather than a fixed integrity+diff label that may
    // not have differentiated anything (NER-256 adversarial review).
    let gates_truly_tie = ranked
        .first()
        .map(|first| {
            let first_gate = sort_key(first).0;
            ranked.iter().all(|row| sort_key(row).0 == first_gate)
        })
        .unwrap_or(true);
    for index in 0..ranked.len() {
        let rank = (index + 1) as u32;
        let passed = ranked[index].check_status.as_deref() == Some("passed");
        let mut reason = if passed {
            format!(
                "rank {rank}: all required gates passing ({} failing tests, {} passing)",
                ranked[index].metrics.tests_failed.unwrap_or(0),
                ranked[index].metrics.tests_passed.unwrap_or(0)
            )
        } else {
            let status = ranked[index]
                .check_status
                .as_deref()
                .unwrap_or("unknown")
                .to_string();
            if !gates_truly_tie {
                // A passing-gate attempt outranked this one: gates were NOT tied, this row
                // simply lost. Keep the old, accurate phrasing for the non-passing remainder.
                format!("rank {rank}: required gates not satisfied (check status: {status})")
            } else {
                // Genuine gate tie. Name the field that actually broke the tie. For a
                // non-winner, compare against the row ranked immediately above (why it
                // landed below). For the rank-1 winner, compare against the row below it
                // (why it ranked first). The discriminator is phrased from THIS row's
                // perspective so a rank-only consumer reads the real differentiator —
                // integrity, diff, or test counts — not a fixed integrity+diff label.
                let label = match index.checked_sub(1) {
                    Some(prev) => {
                        tie_break_discriminator(&ranked[prev], &ranked[index], TieSide::Below)
                    }
                    None => ranked.get(index + 1).and_then(|next| {
                        tie_break_discriminator(&ranked[index], next, TieSide::Above)
                    }),
                };
                match label {
                    Some(label) => format!(
                        "rank {rank}: gates tie (check status: {status}); ranked by {label}"
                    ),
                    // No neighbour differed (stable-order tie) or single-row group.
                    None => format!(
                        "rank {rank}: gates tie (check status: {status}); ranked by created order"
                    ),
                }
            }
        };
        // A legacy_unverified attempt is rankable (its deciding evidence predates
        // Phase 5 and was never hash-verified), so a rank-only consumer must still see
        // that caveat in the explanation (NER-137 code-review).
        if ranked[index].integrity == INTEGRITY_LEGACY {
            reason.push_str(
                " — NOTE: deciding evidence is legacy_unverified (pre-Phase-5, not hash-verified)",
            );
        }
        ranked[index].rank = Some(rank);
        ranked[index].rank_reason = reason;
    }
    for row in &mut unranked {
        row.rank = None;
        row.rank_reason = if row.proposal.is_none() {
            "unranked: no proposal yet".to_string()
        } else {
            "unranked: deciding evidence failed the integrity check (tampered) — verify with `doctor`".to_string()
        };
    }
    ranked.append(&mut unranked);
    *rows = ranked;
}
