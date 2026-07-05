use super::*;

/// One declared gate as surfaced by `forge intent show`/`list` (NER-257). A serde
/// projection of [`forge_policy::Gate`] that renames `require_structured_pass` to the
/// stable `structured` key. `program`/`args` are run through the same per-arg
/// `key=value` secret redactor [`redact_gate_result`] applies on the check surface, so a
/// secret-like gate token never leaks through this egress (the stored
/// `intents.check_spec_json` is raw).
#[derive(Debug, Clone, Serialize)]
pub struct IntentGate {
    pub program: String,
    pub args: Vec<String>,
    pub structured: bool,
}

/// One intent as surfaced by `forge intent list` (NER-257): id, title/text, a status
/// derived from its linked attempts (no `intents.status` column exists), the declared
/// gate spec, and the linked attempt ids.
#[derive(Debug, Clone, Serialize)]
pub struct IntentSummary {
    pub intent_id: String,
    pub title: String,
    pub status: String,
    pub gates: Vec<IntentGate>,
    pub attempt_ids: Vec<String>,
}

/// One intent's full detail as surfaced by `forge intent show <id>` (NER-257). Same
/// shape as [`IntentSummary`] today; a distinct type leaves room for the detail view to
/// diverge (e.g. per-attempt status) without changing the list contract.
#[derive(Debug, Clone, Serialize)]
pub struct IntentDetail {
    pub intent_id: String,
    pub title: String,
    pub status: String,
    pub gates: Vec<IntentGate>,
    pub attempt_ids: Vec<String>,
}

/// Project an intent's parsed [`forge_policy::CheckSpec`] into the egress
/// [`IntentGate`]s, renaming `require_structured_pass` to `structured` and applying the
/// same per-arg `key=value` secret redaction [`redact_gate_result`] uses on the check
/// surface (the stored `check_spec_json` is raw, so a secret-like gate token must be
/// scrubbed before this egress — NER-257 secret-safety).
fn intent_gates(conn: &Connection, intent_id: &str) -> Result<Vec<IntentGate>> {
    let spec = intent_check_spec(conn, intent_id)?;
    Ok(spec
        .gates
        .into_iter()
        .map(|gate| IntentGate {
            program: forge_content::redact_secret_like_text(&gate.program).0,
            args: gate
                .args
                .iter()
                .map(|arg| forge_content::redact_secret_like_text(arg).0)
                .collect(),
            structured: gate.require_structured_pass,
        })
        .collect())
}

/// Derive an intent-level status from its linked attempts (NER-257): the `intents` table
/// has no status column, so `accepted` if any linked attempt has an accepted decision,
/// else `open`. Honest and migration-free; the derived field is documented as such.
fn intent_derived_status(conn: &Connection, repo_id: &str, intent_id: &str) -> Result<String> {
    // An accepted decision joins back to its attempt via proposal → attempt, and the
    // attempt to its intent. Repo-scoped on every table so a multi-repo DB never leaks
    // another repo's decision.
    let accepted: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM decisions d
             JOIN proposals p ON p.id = d.proposal_id AND p.repo_id = d.repo_id
             JOIN attempts a ON a.id = p.attempt_id AND a.repo_id = d.repo_id
             WHERE d.repo_id = ?1 AND a.intent_id = ?2 AND d.decision = 'accepted'
         )",
        params![repo_id, intent_id],
        |row| row.get(0),
    )?;
    Ok(if accepted { "accepted" } else { "open" }.to_string())
}

/// List every intent in the repo (NER-257), oldest first, each with its title, a status
/// derived from its linked attempts, the declared (secret-redacted) gate spec, and the
/// linked attempt ids. Repo-scoped — never leaks another repo's intents/attempts.
pub fn intents_list(cwd: &Path) -> Result<Vec<IntentSummary>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let mut statement = connection.prepare(
        "SELECT id, text FROM intents WHERE repo_id = ?1 ORDER BY created_at_ms ASC, id ASC",
    )?;
    let intent_rows: Vec<(String, String)> = statement
        .query_map(params![context.repo_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut summaries = Vec::with_capacity(intent_rows.len());
    for (intent_id, title) in intent_rows {
        let gates = intent_gates(&connection, &intent_id)?;
        let attempt_ids = attempts_for_intent(&connection, &context.repo_id, &intent_id)?
            .into_iter()
            .map(|attempt| attempt.attempt_id)
            .collect();
        let status = intent_derived_status(&connection, &context.repo_id, &intent_id)?;
        summaries.push(IntentSummary {
            intent_id,
            title,
            status,
            gates,
            attempt_ids,
        });
    }
    Ok(summaries)
}

/// Detail for one intent (NER-257). The repo-scoped existence check (two-column
/// `repo_id`+`id` WHERE, mirroring [`attempt_by_id`]) is mandatory BEFORE handing the id
/// to [`intent_check_spec`]/[`intent_gates`], which query by `id` alone — so a
/// cross-repo or unknown id is rejected with `UnknownIntent` rather than reading another
/// repo's spec.
pub fn intent_detail(cwd: &Path, intent_id: &str) -> Result<IntentDetail> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    intent_detail_on(&connection, &context, intent_id)
}

pub(crate) fn intent_detail_on(
    connection: &Connection,
    context: &RepositoryContext,
    intent_id: &str,
) -> Result<IntentDetail> {
    let title: Option<String> = connection
        .query_row(
            "SELECT text FROM intents WHERE repo_id = ?1 AND id = ?2",
            params![context.repo_id, intent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(title) = title else {
        return Err(ForgeError::UnknownIntent {
            selector: intent_id.to_string(),
        }
        .into());
    };
    let gates = intent_gates(connection, intent_id)?;
    let attempt_ids = attempts_for_intent(connection, &context.repo_id, intent_id)?
        .into_iter()
        .map(|attempt| attempt.attempt_id)
        .collect();
    let status = intent_derived_status(connection, &context.repo_id, intent_id)?;
    Ok(IntentDetail {
        intent_id: intent_id.to_string(),
        title,
        status,
        gates,
        attempt_ids,
    })
}
