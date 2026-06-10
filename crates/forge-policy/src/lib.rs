//! Declarative, content-bound, multi-gate check engine (NER-135, Phase 4).
//!
//! Replaces the historic single-`exit==0`-on-latest-evidence policy. A
//! [`CheckSpec`] is a flat, ANDed list of command [`Gate`]s declared per-intent;
//! [`evaluate`] aggregates over the **proposed snapshot's full evidence set** and
//! returns an overall status plus a per-gate verdict ([`GateResult`]).
//!
//! ## Verdict rule (the heart of the engine)
//!
//! For each gate (a `(program, args)` identity), consider the evidence facts whose
//! identity matches. The **latest** such fact *on the proposed snapshot* (by
//! `created_at_ms`, then `seq` — the store's rowid tiebreak) decides
//! `passed`/`failed`. Matching evidence that exists only on a *different* snapshot
//! is `stale`; no matching evidence at all is `missing`. "Latest matching wins"
//! lets a legitimate same-tree re-run supersede a prior result while a *different*
//! command (e.g. `echo ok`) can never satisfy a `cargo test` gate.
//!
//! ## Default mode (no declared gates)
//!
//! When [`CheckSpec::gates`] is empty, the engine synthesizes one implicit gate per
//! distinct command identity observed *on the proposed snapshot* and passes iff at
//! least one exists and all pass. Evidence only on a prior snapshot is `stale`; none
//! at all is `missing`. This closes the "failing-test-then-`echo ok`" footgun even
//! for intents that declared no gates (NER-135 R9). A lone trivial success (e.g.
//! `run -- true`) still passes the default — the acknowledged trivial case.
//!
//! ## What a verdict does NOT bind (Honesty Note)
//!
//! A verdict binds `(program, args, snapshot_id, exit_code)` only — NOT the
//! execution environment, cwd, or executable contents. It is content-bound and
//! un-bypassable-by-trivial-command, *not* tamper-proof (Phase 5 hash-chaining) and
//! *not* environment/exec-hash bound (Phase 5+).

use serde::{Deserialize, Serialize};

/// One declared command gate: a verification whose matching evidence must pass on
/// the proposed snapshot. Identity is `(program, args)` string equality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub program: String,
    pub args: Vec<String>,
    /// When `true`, this is a structured gate (NER-136 §U6): in addition to a zero
    /// exit code, the deciding evidence's *parsed* failure count must be exactly zero
    /// ("0 failing tests"). `#[serde(default)]` keeps Phase 4 (exit-code-only) specs
    /// readable on an upgraded DB.
    #[serde(default)]
    pub require_structured_pass: bool,
}

/// The per-intent check spec: a flat, ANDed list of command gates. An empty list
/// selects default mode (synthesize gates from observed evidence). Persisted as
/// JSON in `intents.check_spec_json`, so it round-trips via serde.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSpec {
    pub gates: Vec<Gate>,
}

/// One evidence row projected into the engine's input. `seq` is the rowid tiebreak,
/// mirroring the store's `ORDER BY created_at_ms DESC, rowid DESC` "latest" rule.
#[derive(Debug, Clone)]
pub struct EvidenceFact {
    pub evidence_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub exit_code: i64,
    pub snapshot_id: Option<String>,
    pub created_at_ms: i64,
    pub seq: i64,
    /// The parsed test-failure count from the row's structured outcome (NER-136 §U5),
    /// or `None` when no parser matched. A structured gate reads this; an exit-code
    /// gate ignores it.
    pub structured_failures: Option<u64>,
}

/// The per-gate verdict. `passed`/`failed` from the latest matching evidence on the
/// proposed snapshot; `stale` when matching evidence exists only on another
/// snapshot; `missing` when no matching evidence exists on the snapshot, OR (for a
/// structured gate, NER-136) when the deciding evidence has a zero exit code but
/// produced no parseable failure count — the declared count was never produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateVerdict {
    Passed,
    Failed,
    Missing,
    Stale,
}

/// A machine-readable diagnostic explaining *how* a gate's verdict was reached
/// (NER-254). The `verdict` field carries the pass/fail/missing/stale bit; this
/// field disambiguates the *reason*, splitting the historically overloaded `missing`
/// verdict into "no evidence at all" vs. "evidence with exit 0 but no parser matched".
///
/// Additive in `forge.cli.v0`: serialized snake_case alongside `verdict` on every
/// `GateResult`, and so reaches `check` and `compare` JSON automatically. The enum
/// can grow new variants later additively; existing variants must not change meaning.
///
/// `verdict_detail` explains the *evaluation mode*, not the pass/fail bit:
/// `exit_code_only` covers both a passing and a failing plain (non-structured) gate
/// (the `verdict` field already distinguishes them), and `parsed` covers both a
/// passing and a failing structured gate whose parsed count decided the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictDetail {
    /// No matching evidence on the proposed snapshot, and none elsewhere — the
    /// `missing`-by-absence case.
    NoEvidence,
    /// Matching evidence exists, but only on a *different* snapshot (verdict `stale`).
    StaleOffSnapshot,
    /// Decided by exit code alone, regardless of `require_structured`: a plain
    /// (non-structured) gate, OR a structured gate whose deciding evidence has a
    /// nonzero exit (the parsed count was never consulted because the nonzero exit
    /// already failed it). Covers both the pass and the fail of a plain gate.
    ExitCodeOnly,
    /// A structured gate whose *present* parsed failure count decided the verdict:
    /// `Some(0) => Passed` and `Some(n) => Failed` both land here (the count was
    /// produced and used).
    Parsed,
    /// A structured gate whose deciding evidence has exit 0 but produced no parseable
    /// failure count (`structured_failures == None`) — the overloaded `missing` case
    /// the NER-254 diagnostic disambiguates: evidence exists, exit 0, no parser
    /// matched the program's output. This is the silent-failure signal from NER-253.
    StructuredRequiredButUnparsed,
}

/// The verdict for one gate, with the deciding evidence (when any) for traceability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateResult {
    pub program: String,
    pub args: Vec<String>,
    pub verdict: GateVerdict,
    pub evidence_id: Option<String>,
    pub exit_code: Option<i64>,
    /// The parsed test-failure count of the deciding evidence (the latest matching
    /// row on the proposed snapshot), or `None` when the gate is `missing`/`stale` or
    /// the deciding row produced no parsed count (NER-137 D6). Lets a consumer
    /// distinguish "failed on exit code" from "failed on parsed count" from
    /// `check --json` and the compare output. Carried for *every* gate (not only
    /// structured ones) for observability; the verdict logic is unchanged.
    pub structured_failures: Option<u64>,
    /// A machine-readable diagnostic explaining *how* this gate's verdict was reached
    /// (NER-254). Additive in `forge.cli.v0`; disambiguates the overloaded `missing`
    /// verdict. Computed in lockstep with `verdict` so there is one source of truth.
    pub verdict_detail: VerdictDetail,
}

/// The aggregate check outcome: an overall `status` string
/// (`passed`/`failed`/`missing`/`stale`), a human `reason`, and the per-gate
/// verdicts (emit-only in v0 — not persisted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckOutcome {
    pub status: String,
    pub reason: String,
    pub gates: Vec<GateResult>,
}

impl CheckOutcome {
    /// Whether the overall check passed (all required gates green).
    pub fn passed(&self) -> bool {
        self.status == STATUS_PASSED
    }

    /// Identity strings (`"program arg…"`) for every gate that did not pass, for the
    /// `CHECK_NOT_PASSED` error's `unmet` list. The caller is responsible for any
    /// secret redaction before these reach a machine-visible payload.
    pub fn unmet_identities(&self) -> Vec<String> {
        self.gates
            .iter()
            .filter(|gate| gate.verdict != GateVerdict::Passed)
            .map(|gate| identity_string(&gate.program, &gate.args))
            .collect()
    }
}

const STATUS_PASSED: &str = "passed";
const STATUS_FAILED: &str = "failed";
const STATUS_MISSING: &str = "missing";
const STATUS_STALE: &str = "stale";

/// The stale-default reason carries this substring so the historic
/// snapshot-mismatch contract (and its test) is preserved.
const STALE_REASON: &str = "latest evidence does not match proposal revision snapshot";

/// Evaluate the check for a proposal's snapshot against its spec and the attempt's
/// full evidence set. The single source of truth for pass/fail/missing/stale.
pub fn evaluate(
    spec: &CheckSpec,
    proposed_snapshot_id: &str,
    facts: &[EvidenceFact],
) -> CheckOutcome {
    if spec.gates.is_empty() {
        evaluate_default(proposed_snapshot_id, facts)
    } else {
        evaluate_declared(&spec.gates, proposed_snapshot_id, facts)
    }
}

/// Declared-gate mode: every gate must pass. Overall precedence
/// `failed > missing > stale > passed`.
fn evaluate_declared(
    gates: &[Gate],
    proposed_snapshot_id: &str,
    facts: &[EvidenceFact],
) -> CheckOutcome {
    let results: Vec<GateResult> = gates
        .iter()
        .map(|gate| {
            verdict_for(
                &gate.program,
                &gate.args,
                gate.require_structured_pass,
                proposed_snapshot_id,
                facts,
            )
        })
        .collect();
    let status = rollup(&results);
    let reason = summarize(status, &results);
    CheckOutcome {
        status: status.to_string(),
        reason,
        gates: results,
    }
}

/// Default mode (no declared gates): synthesize one gate per distinct command
/// identity observed on the proposed snapshot. This path does NOT use the
/// declared-gate rollup — with no declared gates there is nothing to be `missing`;
/// "no evidence on snapshot" is decided directly as `stale` (evidence elsewhere) or
/// `missing` (none at all).
fn evaluate_default(proposed_snapshot_id: &str, facts: &[EvidenceFact]) -> CheckOutcome {
    let on_snapshot = facts
        .iter()
        .any(|fact| fact.snapshot_id.as_deref() == Some(proposed_snapshot_id));
    if !on_snapshot {
        return if facts.is_empty() {
            CheckOutcome {
                status: STATUS_MISSING.to_string(),
                reason: "no command evidence recorded for the proposed snapshot".to_string(),
                gates: Vec::new(),
            }
        } else {
            CheckOutcome {
                status: STATUS_STALE.to_string(),
                reason: STALE_REASON.to_string(),
                gates: Vec::new(),
            }
        };
    }

    // Distinct identities observed on the proposed snapshot, in first-seen order.
    let mut identities: Vec<(&str, &[String])> = Vec::new();
    for fact in facts
        .iter()
        .filter(|fact| fact.snapshot_id.as_deref() == Some(proposed_snapshot_id))
    {
        let identity = (fact.program.as_str(), fact.args.as_slice());
        if !identities
            .iter()
            .any(|(program, args)| *program == identity.0 && *args == identity.1)
        {
            identities.push(identity);
        }
    }
    let results: Vec<GateResult> = identities
        .iter()
        .map(|(program, args)| verdict_for(program, args, false, proposed_snapshot_id, facts))
        .collect();
    // Synthesized gates are all passed/failed (each exists on the snapshot), so the
    // status is passed unless any failed.
    let status = if results.iter().any(|r| r.verdict == GateVerdict::Failed) {
        STATUS_FAILED
    } else {
        STATUS_PASSED
    };
    let reason = summarize(status, &results);
    CheckOutcome {
        status: status.to_string(),
        reason,
        gates: results,
    }
}

/// The verdict for a single gate identity against the proposed snapshot. When
/// `require_structured` is set, a zero exit code is necessary but not sufficient: the
/// deciding evidence's parsed failure count must also be exactly zero (conjunctive —
/// the stronger claim wins, so any disagreement is `Failed`); an absent parsed count
/// is `Missing` (the gate asked for a count that was never produced).
fn verdict_for(
    program: &str,
    args: &[String],
    require_structured: bool,
    proposed_snapshot_id: &str,
    facts: &[EvidenceFact],
) -> GateResult {
    let matching: Vec<&EvidenceFact> = facts
        .iter()
        .filter(|fact| fact.program == program && fact.args.as_slice() == args)
        .collect();

    let latest_on_snapshot = latest(
        matching
            .iter()
            .copied()
            .filter(|fact| fact.snapshot_id.as_deref() == Some(proposed_snapshot_id)),
    );

    let (verdict, verdict_detail, evidence_id, exit_code, structured_failures) =
        if let Some(fact) = latest_on_snapshot {
            // Compute verdict and its detail together so they cannot drift. A nonzero
            // exit fails before `require_structured` is consulted, so it is reported as
            // `ExitCodeOnly` (the parsed count never decided anything).
            let (verdict, detail) = if fact.exit_code != 0 {
                (GateVerdict::Failed, VerdictDetail::ExitCodeOnly)
            } else if require_structured {
                match fact.structured_failures {
                    Some(0) => (GateVerdict::Passed, VerdictDetail::Parsed),
                    Some(_) => (GateVerdict::Failed, VerdictDetail::Parsed),
                    None => (
                        GateVerdict::Missing,
                        VerdictDetail::StructuredRequiredButUnparsed,
                    ),
                }
            } else {
                (GateVerdict::Passed, VerdictDetail::ExitCodeOnly)
            };
            (
                verdict,
                detail,
                Some(fact.evidence_id.clone()),
                Some(fact.exit_code),
                fact.structured_failures,
            )
        } else if !matching.is_empty() {
            // Ran, but only on a different tree: carry the latest off-snapshot evidence id
            // for context. No on-snapshot deciding row, so no structured count to report.
            let latest_any = latest(matching.iter().copied());
            (
                GateVerdict::Stale,
                VerdictDetail::StaleOffSnapshot,
                latest_any.map(|fact| fact.evidence_id.clone()),
                None,
                None,
            )
        } else {
            (
                GateVerdict::Missing,
                VerdictDetail::NoEvidence,
                None,
                None,
                None,
            )
        };

    GateResult {
        program: program.to_string(),
        args: args.to_vec(),
        verdict,
        evidence_id,
        exit_code,
        structured_failures,
        verdict_detail,
    }
}

/// Overall status for declared gates: `failed > missing > stale > passed`.
fn rollup(results: &[GateResult]) -> &'static str {
    if results.iter().any(|r| r.verdict == GateVerdict::Failed) {
        STATUS_FAILED
    } else if results.iter().any(|r| r.verdict == GateVerdict::Missing) {
        STATUS_MISSING
    } else if results.iter().any(|r| r.verdict == GateVerdict::Stale) {
        STATUS_STALE
    } else {
        STATUS_PASSED
    }
}

/// Pick the latest fact by `(created_at_ms, seq)`.
fn latest<'a>(facts: impl Iterator<Item = &'a EvidenceFact>) -> Option<&'a EvidenceFact> {
    facts.fold(None, |best, fact| match best {
        Some(current) if (current.created_at_ms, current.seq) >= (fact.created_at_ms, fact.seq) => {
            Some(current)
        }
        _ => Some(fact),
    })
}

/// The canonical `"program arg…"` identity string for a `(program, args)` gate.
/// Public so the store can render the same identity into a provenance trailer without
/// duplicating the format (NER-137 code-review).
pub fn identity_string(program: &str, args: &[String]) -> String {
    if args.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", args.join(" "))
    }
}

fn summarize(status: &str, results: &[GateResult]) -> String {
    if status == STATUS_PASSED {
        return format!(
            "all {} required gate(s) passed on the proposed snapshot",
            results.len()
        );
    }
    let count = |verdict: GateVerdict| results.iter().filter(|r| r.verdict == verdict).count();
    let mut reason = format!(
        "required gates not satisfied: {} failed, {} missing, {} stale",
        count(GateVerdict::Failed),
        count(GateVerdict::Missing),
        count(GateVerdict::Stale),
    );
    // Call out the NER-254 disambiguation: a `missing` that is really "evidence
    // exists, exit 0, but no structured parser matched the program's output" — the
    // operator needs a different fix (the named program has no registered parser)
    // than a plain absence of evidence.
    let unparsed = results
        .iter()
        .filter(|r| r.verdict_detail == VerdictDetail::StructuredRequiredButUnparsed)
        .count();
    if unparsed > 0 {
        reason.push_str(&format!(
            " ({unparsed} gate(s) need a structured result but none was parsed)"
        ));
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAP: &str = "snapshot_proposed";
    const OTHER: &str = "snapshot_other";

    fn fact(
        id: &str,
        program: &str,
        args: &[&str],
        exit_code: i64,
        snapshot: Option<&str>,
        seq: i64,
    ) -> EvidenceFact {
        EvidenceFact {
            evidence_id: id.to_string(),
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            exit_code,
            snapshot_id: snapshot.map(str::to_string),
            created_at_ms: seq, // monotonic with seq for these tests
            seq,
            structured_failures: None,
        }
    }

    fn gate(program: &str, args: &[&str]) -> Gate {
        Gate {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            require_structured_pass: false,
        }
    }

    fn structured_gate(program: &str, args: &[&str]) -> Gate {
        Gate {
            require_structured_pass: true,
            ..gate(program, args)
        }
    }

    fn fact_with_failures(
        id: &str,
        program: &str,
        args: &[&str],
        exit_code: i64,
        snapshot: Option<&str>,
        seq: i64,
        structured_failures: Option<u64>,
    ) -> EvidenceFact {
        EvidenceFact {
            structured_failures,
            ..fact(id, program, args, exit_code, snapshot, seq)
        }
    }

    #[test]
    fn structured_gate_passes_on_zero_parsed_failures() {
        let facts = vec![fact_with_failures(
            "e1",
            "cargo",
            &["test"],
            0,
            Some(SNAP),
            1,
            Some(0),
        )];
        let outcome = evaluate(
            &spec(vec![structured_gate("cargo", &["test"])]),
            SNAP,
            &facts,
        );
        assert_eq!(outcome.status, "passed");
        // NER-254: a parsed count decided the verdict.
        assert_eq!(outcome.gates[0].verdict_detail, VerdictDetail::Parsed);
    }

    #[test]
    fn structured_gate_fails_on_parsed_failures_despite_zero_exit() {
        // exit_code == 0 but the parser found failures — the stronger claim wins.
        let facts = vec![fact_with_failures(
            "e1",
            "cargo",
            &["test"],
            0,
            Some(SNAP),
            1,
            Some(2),
        )];
        let outcome = evaluate(
            &spec(vec![structured_gate("cargo", &["test"])]),
            SNAP,
            &facts,
        );
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Failed);
        // NER-254: the present count decided it, so detail is `parsed` (not exit-code).
        assert_eq!(outcome.gates[0].verdict_detail, VerdictDetail::Parsed);
    }

    #[test]
    fn structured_gate_fails_on_nonzero_exit_even_if_zero_failures() {
        let facts = vec![fact_with_failures(
            "e1",
            "cargo",
            &["test"],
            101,
            Some(SNAP),
            1,
            Some(0),
        )];
        let outcome = evaluate(
            &spec(vec![structured_gate("cargo", &["test"])]),
            SNAP,
            &facts,
        );
        assert_eq!(outcome.status, "failed");
        // NER-254: a nonzero exit fails before the parsed count is consulted, so the
        // detail is `exit_code_only` even though this is a structured gate.
        assert_eq!(outcome.gates[0].verdict_detail, VerdictDetail::ExitCodeOnly);
    }

    #[test]
    fn structured_gate_is_missing_when_count_unparsed() {
        // Zero exit, but no parsed count for a declared structured gate -> missing.
        let facts = vec![fact_with_failures(
            "e1",
            "cargo",
            &["test"],
            0,
            Some(SNAP),
            1,
            None,
        )];
        let outcome = evaluate(
            &spec(vec![structured_gate("cargo", &["test"])]),
            SNAP,
            &facts,
        );
        assert_eq!(outcome.status, "missing");
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Missing);
        // NER-254: this is the overloaded `missing` the diagnostic disambiguates —
        // evidence exists, exit 0, but no structured parser matched the output.
        assert_eq!(
            outcome.gates[0].verdict_detail,
            VerdictDetail::StructuredRequiredButUnparsed
        );
        // The human reason calls out the unparsed-structured case (NER-254).
        assert!(
            outcome.reason.contains("structured result"),
            "reason should mention the unparsed-structured case: {}",
            outcome.reason
        );
    }

    fn spec(gates: Vec<Gate>) -> CheckSpec {
        CheckSpec { gates }
    }

    #[test]
    fn declared_gate_passes_with_passing_evidence_on_snapshot() {
        let facts = vec![fact("e1", "cargo", &["test"], 0, Some(SNAP), 1)];
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &facts);
        assert_eq!(outcome.status, "passed");
        assert_eq!(outcome.gates.len(), 1);
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Passed);
        assert_eq!(outcome.gates[0].evidence_id.as_deref(), Some("e1"));
        // NER-254: a plain (non-structured) pass is decided by exit code alone.
        assert_eq!(outcome.gates[0].verdict_detail, VerdictDetail::ExitCodeOnly);
    }

    #[test]
    fn plain_gate_failing_on_nonzero_exit_is_exit_code_only() {
        // NER-254: a plain gate that fails on a nonzero exit still reports the
        // evaluation mode `exit_code_only` — the `verdict` field carries the fail bit.
        let facts = vec![fact("e1", "cargo", &["test"], 7, Some(SNAP), 1)];
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &facts);
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Failed);
        assert_eq!(outcome.gates[0].verdict_detail, VerdictDetail::ExitCodeOnly);
    }

    #[test]
    fn declared_gate_is_stale_when_only_off_snapshot_evidence() {
        let facts = vec![fact("e1", "cargo", &["test"], 0, Some(OTHER), 1)];
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &facts);
        assert_eq!(outcome.status, "stale");
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Stale);
        // NER-254: stale because evidence is only off-snapshot.
        assert_eq!(
            outcome.gates[0].verdict_detail,
            VerdictDetail::StaleOffSnapshot
        );
    }

    #[test]
    fn declared_gate_is_missing_when_no_matching_evidence() {
        let facts = vec![fact("e1", "echo", &["ok"], 0, Some(SNAP), 1)];
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &facts);
        assert_eq!(outcome.status, "missing");
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Missing);
        // NER-254: missing because no matching evidence exists at all.
        assert_eq!(outcome.gates[0].verdict_detail, VerdictDetail::NoEvidence);
    }

    #[test]
    fn run_true_cannot_satisfy_a_declared_gate() {
        // The `run -- true` bypass: only `true` ran, but the gate names `cargo test`.
        let facts = vec![fact("e1", "true", &[], 0, Some(SNAP), 1)];
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &facts);
        assert_eq!(outcome.status, "missing");
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Missing);
    }

    #[test]
    fn failing_test_then_echo_ok_does_not_flip_declared_gate_green() {
        // The footgun: a newer trivial success must not mask the failing gate.
        let facts = vec![
            fact("e1", "cargo", &["test"], 7, Some(SNAP), 1),
            fact("e2", "echo", &["ok"], 0, Some(SNAP), 2),
        ];
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &facts);
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Failed);
        assert_eq!(outcome.gates[0].exit_code, Some(7));
    }

    #[test]
    fn two_declared_gates_require_all_to_pass() {
        let pass_both = vec![
            fact("e1", "cargo", &["test"], 0, Some(SNAP), 1),
            fact("e2", "cargo", &["clippy"], 0, Some(SNAP), 2),
        ];
        let outcome = evaluate(
            &spec(vec![gate("cargo", &["test"]), gate("cargo", &["clippy"])]),
            SNAP,
            &pass_both,
        );
        assert_eq!(outcome.status, "passed");
        assert_eq!(outcome.gates.len(), 2);

        let one_fails = vec![
            fact("e1", "cargo", &["test"], 0, Some(SNAP), 1),
            fact("e2", "cargo", &["clippy"], 1, Some(SNAP), 2),
        ];
        let outcome = evaluate(
            &spec(vec![gate("cargo", &["test"]), gate("cargo", &["clippy"])]),
            SNAP,
            &one_fails,
        );
        assert_eq!(outcome.status, "failed");
    }

    #[test]
    fn latest_matching_wins_for_same_tree_rerun() {
        // cargo test failed then was re-run and passed on the same snapshot.
        let facts = vec![
            fact("e1", "cargo", &["test"], 7, Some(SNAP), 1),
            fact("e2", "cargo", &["test"], 0, Some(SNAP), 2),
        ];
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &facts);
        assert_eq!(outcome.status, "passed");
        assert_eq!(outcome.gates[0].evidence_id.as_deref(), Some("e2"));
    }

    #[test]
    fn precedence_failed_beats_missing_beats_stale() {
        // failed + missing -> failed
        let facts = vec![fact("e1", "a", &[], 1, Some(SNAP), 1)];
        let outcome = evaluate(&spec(vec![gate("a", &[]), gate("b", &[])]), SNAP, &facts);
        assert_eq!(outcome.status, "failed"); // a failed, b missing

        // missing + stale -> missing
        let facts = vec![fact("e1", "b", &[], 0, Some(OTHER), 1)];
        let outcome = evaluate(&spec(vec![gate("a", &[]), gate("b", &[])]), SNAP, &facts);
        assert_eq!(outcome.status, "missing"); // a missing, b stale
    }

    #[test]
    fn default_mode_single_trivial_success_passes() {
        let facts = vec![fact("e1", "true", &[], 0, Some(SNAP), 1)];
        let outcome = evaluate(&CheckSpec::default(), SNAP, &facts);
        assert_eq!(outcome.status, "passed");
    }

    #[test]
    fn default_mode_failing_then_echo_ok_fails() {
        // The footgun closed for undeclared intents too (R9): distinct identities,
        // the failing one is a synthesized gate.
        let facts = vec![
            fact("e1", "sh", &["-c", "exit 7"], 7, Some(SNAP), 1),
            fact("e2", "echo", &["ok"], 0, Some(SNAP), 2),
        ];
        let outcome = evaluate(&CheckSpec::default(), SNAP, &facts);
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.gates.len(), 2);
    }

    #[test]
    fn default_mode_missing_and_stale() {
        // No evidence at all -> missing.
        let outcome = evaluate(&CheckSpec::default(), SNAP, &[]);
        assert_eq!(outcome.status, "missing");

        // Evidence only on a prior snapshot -> stale, with the preserved reason.
        let facts = vec![fact("e1", "cargo", &["test"], 0, Some(OTHER), 1)];
        let outcome = evaluate(&CheckSpec::default(), SNAP, &facts);
        assert_eq!(outcome.status, "stale");
        assert!(outcome.reason.contains("does not match proposal revision"));
    }

    #[test]
    fn unmet_identities_lists_non_passed_gates() {
        let facts = vec![fact("e1", "cargo", &["test"], 7, Some(SNAP), 1)];
        let outcome = evaluate(
            &spec(vec![gate("cargo", &["test"]), gate("cargo", &["clippy"])]),
            SNAP,
            &facts,
        );
        let unmet = outcome.unmet_identities();
        assert!(unmet.contains(&"cargo test".to_string()));
        assert!(unmet.contains(&"cargo clippy".to_string()));
    }

    #[test]
    fn gate_result_carries_structured_failures_of_deciding_row() {
        // D6 (NER-137): the parsed failure count of the deciding row is surfaced on the
        // GateResult — even for a plain exit-code gate — so compare/`check --json` can
        // tell "failed on exit code" from "failed on parsed count".
        let pass = vec![fact_with_failures(
            "e1",
            "cargo",
            &["test"],
            0,
            Some(SNAP),
            1,
            Some(0),
        )];
        let outcome = evaluate(
            &spec(vec![structured_gate("cargo", &["test"])]),
            SNAP,
            &pass,
        );
        assert_eq!(outcome.gates[0].structured_failures, Some(0));

        let fail = vec![fact_with_failures(
            "e1",
            "cargo",
            &["test"],
            0,
            Some(SNAP),
            1,
            Some(2),
        )];
        // A plain (non-structured) gate still reports the parsed count for observability.
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &fail);
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Passed); // exit 0, plain gate
        assert_eq!(outcome.gates[0].structured_failures, Some(2));
    }

    #[test]
    fn gate_result_structured_failures_is_none_when_missing_or_stale() {
        // missing: no matching evidence on the snapshot.
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &[]);
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Missing);
        assert_eq!(outcome.gates[0].structured_failures, None);

        // stale: matching evidence only on another snapshot.
        let facts = vec![fact_with_failures(
            "e1",
            "cargo",
            &["test"],
            0,
            Some(OTHER),
            1,
            Some(0),
        )];
        let outcome = evaluate(&spec(vec![gate("cargo", &["test"])]), SNAP, &facts);
        assert_eq!(outcome.gates[0].verdict, GateVerdict::Stale);
        assert_eq!(outcome.gates[0].structured_failures, None);
    }
}
