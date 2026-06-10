use anyhow::{anyhow, bail, Context, Result};
use forge_content_native::{NativeObjectStore, NativeRefStore, ObjectId, ObjectKind};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const SYNC_PROTOCOL_VERSION: &str = "forge-sync.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncManifest {
    pub protocol_version: String,
    pub cli_schema_version: String,
    pub repo_id: String,
    pub content_backend: String,
    pub current_operation_id: String,
    pub current_view_id: String,
    #[serde(default)]
    pub attached_attempt_id: Option<String>,
    #[serde(default)]
    pub expected_content_ref: Option<String>,
    pub native_head: Option<String>,
    pub native_objects: Vec<SyncObjectRef>,
    #[serde(default)]
    pub native_payloads: Vec<SyncObjectPayload>,
    pub ledger_counts: Vec<LedgerTableCount>,
    #[serde(default)]
    pub ledger_rows: Vec<LedgerTableRows>,
    pub local_key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncObjectRef {
    pub object_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncObjectPayload {
    pub object_id: String,
    pub kind: String,
    pub payload_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerTableCount {
    pub table: String,
    pub rows: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerTableRows {
    pub table: String,
    pub rows: Vec<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncExportReport {
    pub protocol_version: String,
    pub output_path: String,
    pub content_backend: String,
    pub incremental: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_path: Option<String>,
    pub native_object_count: usize,
    pub native_payload_count: usize,
    pub ledger_table_count: usize,
    pub ledger_row_count: usize,
    pub native_head: Option<String>,
    pub local_key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncInspectReport {
    pub protocol_version: String,
    pub content_backend: String,
    pub native_object_count: usize,
    pub native_payload_count: usize,
    pub ledger_table_count: usize,
    pub ledger_row_count: usize,
    pub native_head: Option<String>,
    pub local_key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncImportReport {
    pub protocol_version: String,
    pub content_backend: String,
    pub imported_native_objects: usize,
    pub imported_ledger_rows: usize,
    pub native_head: Option<String>,
    pub current_operation_id: String,
    pub current_view_id: String,
    pub local_key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncCloneReport {
    pub protocol_version: String,
    pub repository_id: String,
    pub root_path: String,
    pub content_backend: String,
    pub imported_native_objects: usize,
    pub imported_ledger_rows: usize,
    pub native_head: Option<String>,
    pub current_operation_id: String,
    pub current_view_id: String,
    pub local_key_fingerprint: Option<String>,
}

pub fn build_manifest(cwd: &Path) -> Result<SyncManifest> {
    let context = forge_store::open_repository(cwd)?;
    let connection = Connection::open(&context.database_path)?;
    let (native_head, native_objects, native_payloads) = if context.content_backend == "native" {
        let store = NativeObjectStore::new(&context.root_path);
        let refs = NativeRefStore::new(&context.root_path);
        let mut objects = Vec::new();
        let mut payloads = Vec::new();
        for id in store.all_object_ids()? {
            let kind = object_kind_label(id.kind()?);
            let payload = store.read_object(&id)?;
            objects.push(SyncObjectRef {
                kind: kind.to_string(),
                object_id: id.to_string(),
            });
            payloads.push(SyncObjectPayload {
                object_id: id.to_string(),
                kind: kind.to_string(),
                payload_hex: hex_encode(&payload),
            });
        }
        (
            refs.read_head()?.map(|head| head.to_string()),
            objects,
            payloads,
        )
    } else {
        (None, Vec::new(), Vec::new())
    };

    Ok(SyncManifest {
        protocol_version: SYNC_PROTOCOL_VERSION.to_string(),
        cli_schema_version: forge_protocol::SCHEMA_VERSION.to_string(),
        repo_id: context.repo_id,
        content_backend: context.content_backend,
        current_operation_id: context.current_operation_id,
        current_view_id: context.current_view_id,
        attached_attempt_id: context.attached_attempt_id,
        expected_content_ref: expected_content_ref(&connection)?,
        native_head,
        native_objects,
        native_payloads,
        ledger_counts: ledger_counts(&connection)?,
        ledger_rows: ledger_rows(&connection)?,
        local_key_fingerprint: latest_key_fingerprint(&connection)?,
    })
}

pub fn export_manifest(cwd: &Path, output_path: &Path) -> Result<SyncExportReport> {
    export_manifest_since(cwd, output_path, None)
}

pub fn export_manifest_since(
    cwd: &Path,
    output_path: &Path,
    since_path: Option<&Path>,
) -> Result<SyncExportReport> {
    let since_path_text = since_path.map(|path| path.display().to_string());
    let since_manifest = since_path
        .map(read_supported_manifest)
        .transpose()
        .context("read incremental sync base")?;
    let (manifest, report) = export_manifest_delta(
        cwd,
        since_manifest.as_ref(),
        output_path.display().to_string(),
        since_path_text,
    )?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(output_path, bytes)?;
    Ok(report)
}

pub fn export_manifest_for_transport_since(
    cwd: &Path,
    since_manifest: Option<&SyncManifest>,
) -> Result<(SyncManifest, SyncExportReport)> {
    export_manifest_delta(cwd, since_manifest, "<transport>".to_string(), None)
}

fn export_manifest_delta(
    cwd: &Path,
    since_manifest: Option<&SyncManifest>,
    output_path: String,
    since_path: Option<String>,
) -> Result<(SyncManifest, SyncExportReport)> {
    let mut manifest = build_manifest(cwd)?;
    if let Some(base) = since_manifest {
        ensure_supported_manifest(base)?;
        prune_manifest_since(&mut manifest, base)?;
    }
    let report = SyncExportReport {
        protocol_version: manifest.protocol_version.clone(),
        output_path,
        content_backend: manifest.content_backend.clone(),
        incremental: since_manifest.is_some(),
        since_path,
        native_object_count: manifest.native_objects.len(),
        native_payload_count: manifest.native_payloads.len(),
        ledger_table_count: manifest.ledger_counts.len(),
        ledger_row_count: manifest
            .ledger_rows
            .iter()
            .map(|table| table.rows.len())
            .sum(),
        native_head: manifest.native_head.clone(),
        local_key_fingerprint: manifest.local_key_fingerprint.clone(),
    };
    Ok((manifest, report))
}

fn prune_manifest_since(manifest: &mut SyncManifest, base: &SyncManifest) -> Result<()> {
    if manifest.content_backend != base.content_backend {
        bail!(
            "sync incremental export requires matching content backends (source {}, base {})",
            manifest.content_backend,
            base.content_backend
        );
    }
    if manifest.repo_id != base.repo_id {
        bail!(
            "sync incremental export requires matching repo ids (source {}, base {})",
            manifest.repo_id,
            base.repo_id
        );
    }

    let base_objects: HashSet<&str> = base
        .native_objects
        .iter()
        .map(|object| object.object_id.as_str())
        .collect();
    manifest
        .native_objects
        .retain(|object| !base_objects.contains(object.object_id.as_str()));
    manifest
        .native_payloads
        .retain(|payload| !base_objects.contains(payload.object_id.as_str()));

    for table in &mut manifest.ledger_rows {
        let Some(base_table) = base
            .ledger_rows
            .iter()
            .find(|base_table| base_table.table == table.table)
        else {
            continue;
        };
        let base_rows: HashSet<String> = base_table
            .rows
            .iter()
            .map(|row| ledger_row_identity(&table.table, row))
            .collect::<Result<_>>()?;
        let mut retained = Vec::new();
        for row in table.rows.drain(..) {
            let identity = ledger_row_identity(&table.table, &row)?;
            if !base_rows.contains(&identity) {
                retained.push(row);
            }
        }
        table.rows = retained;
    }
    Ok(())
}

fn ledger_row_identity(
    table: &str,
    row: &serde_json::Map<String, serde_json::Value>,
) -> Result<String> {
    let column = if table == "attempt_workspaces" {
        "attempt_id"
    } else {
        "id"
    };
    let value = row
        .get(column)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("sync ledger row missing identity {table}.{column}"))?;
    Ok(format!("{table}:{column}:{value}"))
}

pub fn inspect_manifest(path: &Path) -> Result<SyncInspectReport> {
    let bytes = fs::read(path)?;
    let manifest: SyncManifest = serde_json::from_slice(&bytes)?;
    if manifest.protocol_version != SYNC_PROTOCOL_VERSION {
        bail!(
            "unsupported sync protocol version {}",
            manifest.protocol_version
        );
    }
    Ok(SyncInspectReport {
        protocol_version: manifest.protocol_version,
        content_backend: manifest.content_backend,
        native_object_count: manifest.native_objects.len(),
        native_payload_count: manifest.native_payloads.len(),
        ledger_table_count: manifest.ledger_counts.len(),
        ledger_row_count: manifest
            .ledger_rows
            .iter()
            .map(|table| table.rows.len())
            .sum(),
        native_head: manifest.native_head,
        local_key_fingerprint: manifest.local_key_fingerprint,
    })
}

pub fn import_manifest(cwd: &Path, path: &Path) -> Result<SyncImportReport> {
    let manifest = read_supported_manifest(path)?;
    import_manifest_value(cwd, &manifest)
}

pub fn import_manifest_value(cwd: &Path, manifest: &SyncManifest) -> Result<SyncImportReport> {
    ensure_supported_manifest(manifest)?;
    let context = forge_store::open_repository(cwd)?;
    if context.content_backend != "native" {
        bail!("sync import requires a native content repository");
    }
    let imported_ledger_rows = apply_manifest(
        &context.root_path,
        &context.database_path,
        &context.repo_id,
        manifest,
        CurrentStateMode::Update,
    )?;

    Ok(SyncImportReport {
        protocol_version: manifest.protocol_version.clone(),
        content_backend: manifest.content_backend.clone(),
        imported_native_objects: manifest.native_payloads.len(),
        imported_ledger_rows,
        native_head: manifest.native_head.clone(),
        current_operation_id: manifest.current_operation_id.clone(),
        current_view_id: manifest.current_view_id.clone(),
        local_key_fingerprint: manifest.local_key_fingerprint.clone(),
    })
}

pub fn import_native_objects(cwd: &Path, manifest: &SyncManifest) -> Result<usize> {
    let context = forge_store::open_repository(cwd)?;
    if context.content_backend != "native" {
        bail!("sync object import requires a native content repository");
    }
    let store = NativeObjectStore::new(&context.root_path);
    write_manifest_objects(&store, manifest)?;
    Ok(manifest.native_payloads.len())
}

pub fn import_ledger_rows_from_manifest(cwd: &Path, manifest: &SyncManifest) -> Result<usize> {
    let context = forge_store::open_repository(cwd)?;
    let mut connection = Connection::open(&context.database_path)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    import_ledger_rows(&mut connection, &context.repo_id, manifest)
}

pub fn clone_manifest(cwd: &Path, path: &Path) -> Result<SyncCloneReport> {
    let manifest = read_supported_manifest(path)?;
    if manifest.native_head.is_none() {
        bail!("sync clone requires a native head");
    }
    let clone = forge_store::prepare_native_sync_clone(cwd, &manifest.repo_id)?;
    let root_path = Path::new(&clone.root_path);
    let database_path = Path::new(&clone.database_path);
    let imported_ledger_rows = apply_manifest(
        root_path,
        database_path,
        &manifest.repo_id,
        &manifest,
        CurrentStateMode::Insert,
    )?;
    Ok(SyncCloneReport {
        protocol_version: manifest.protocol_version,
        repository_id: clone.repository_id,
        root_path: clone.root_path,
        content_backend: clone.content_backend,
        imported_native_objects: manifest.native_payloads.len(),
        imported_ledger_rows,
        native_head: manifest.native_head,
        current_operation_id: manifest.current_operation_id,
        current_view_id: manifest.current_view_id,
        local_key_fingerprint: manifest.local_key_fingerprint,
    })
}

pub fn manifest_head_descends_from(
    manifest: &SyncManifest,
    ancestor_head: Option<&str>,
) -> Result<bool> {
    let Some(ancestor_head) = ancestor_head else {
        return Ok(true);
    };
    let Some(tip) = manifest.native_head.as_deref() else {
        return Ok(false);
    };
    if tip == ancestor_head {
        return Ok(true);
    }

    let mut commits = std::collections::HashMap::new();
    for payload in &manifest.native_payloads {
        if payload.kind != "commit" {
            continue;
        }
        let bytes = hex_decode(&payload.payload_hex)?;
        let commit: forge_content_native::CommitObject = serde_json::from_slice(&bytes)?;
        commits.insert(payload.object_id.as_str(), commit);
    }

    let mut stack = vec![tip];
    let mut seen = HashSet::new();
    while let Some(commit_id) = stack.pop() {
        if commit_id == ancestor_head {
            return Ok(true);
        }
        if !seen.insert(commit_id) {
            continue;
        }
        let Some(commit) = commits.get(commit_id) else {
            bail!("sync manifest is missing native commit payload {commit_id}");
        };
        for parent in &commit.parents {
            stack.push(parent.as_str());
        }
    }
    Ok(false)
}

pub fn manifest_head_content_ref(manifest: &SyncManifest) -> Result<Option<String>> {
    let Some(head) = manifest.native_head.as_deref() else {
        return Ok(None);
    };
    Ok(Some(manifest_commit_content_ref(manifest, head)?))
}

pub fn manifest_commit_content_ref(manifest: &SyncManifest, commit_id: &str) -> Result<String> {
    for payload in &manifest.native_payloads {
        if payload.object_id != commit_id {
            continue;
        }
        if payload.kind != "commit" {
            bail!("sync native commit id does not name a commit payload");
        }
        let bytes = hex_decode(&payload.payload_hex)?;
        let commit: forge_content_native::CommitObject = serde_json::from_slice(&bytes)?;
        return Ok(format!(
            "{}{}",
            forge_content::FORGE_TREE_PREFIX,
            commit.tree
        ));
    }
    bail!("sync manifest is missing native commit payload {commit_id}");
}

pub fn manifest_common_ancestor_head(
    left: &SyncManifest,
    right: &SyncManifest,
) -> Result<Option<String>> {
    let Some(left_head) = left.native_head.as_deref() else {
        return Ok(None);
    };
    let Some(right_head) = right.native_head.as_deref() else {
        return Ok(None);
    };
    let commits = manifest_commits([left, right])?;
    let left_ancestors = ancestor_depths(left_head, &commits)?;
    let right_ancestors = ancestor_depths(right_head, &commits)?;
    let common = left_ancestors
        .keys()
        .copied()
        .filter(|commit_id| right_ancestors.contains_key(commit_id))
        .collect::<Vec<_>>();
    let mut lowest = Vec::new();
    for candidate in common.iter().copied() {
        let mut dominated = false;
        for other in common.iter().copied() {
            if other != candidate && is_ancestor(candidate, other, &commits)? {
                dominated = true;
                break;
            }
        }
        if !dominated {
            lowest.push(candidate);
        }
    }
    lowest.sort_by(|left_id, right_id| {
        let left_distance = left_ancestors[*left_id] + right_ancestors[*left_id];
        let right_distance = left_ancestors[*right_id] + right_ancestors[*right_id];
        left_distance
            .cmp(&right_distance)
            .then_with(|| right_ancestors[*left_id].cmp(&right_ancestors[*right_id]))
            .then_with(|| left_id.cmp(right_id))
    });
    Ok(lowest.first().map(|commit_id| (*commit_id).to_string()))
}

fn manifest_commits<'a, I>(
    manifests: I,
) -> Result<HashMap<&'a str, forge_content_native::CommitObject>>
where
    I: IntoIterator<Item = &'a SyncManifest>,
{
    let mut commits = HashMap::new();
    for manifest in manifests {
        for payload in &manifest.native_payloads {
            if payload.kind != "commit" {
                continue;
            }
            let bytes = hex_decode(&payload.payload_hex)?;
            let commit: forge_content_native::CommitObject = serde_json::from_slice(&bytes)?;
            commits.insert(payload.object_id.as_str(), commit);
        }
    }
    Ok(commits)
}

fn ancestor_depths<'a>(
    head: &'a str,
    commits: &'a HashMap<&'a str, forge_content_native::CommitObject>,
) -> Result<HashMap<&'a str, usize>> {
    let mut depths = HashMap::new();
    let mut queue = VecDeque::from([(head, 0usize)]);
    while let Some((commit_id, depth)) = queue.pop_front() {
        if depths.contains_key(commit_id) {
            continue;
        }
        depths.insert(commit_id, depth);
        let Some(commit) = commits.get(commit_id) else {
            bail!("sync manifest is missing native commit payload {commit_id}");
        };
        for parent in &commit.parents {
            queue.push_back((parent.as_str(), depth + 1));
        }
    }
    Ok(depths)
}

fn is_ancestor(
    ancestor: &str,
    descendant: &str,
    commits: &HashMap<&str, forge_content_native::CommitObject>,
) -> Result<bool> {
    let mut queue = VecDeque::from([descendant]);
    let mut seen = HashSet::new();
    while let Some(commit_id) = queue.pop_front() {
        if commit_id == ancestor {
            return Ok(true);
        }
        if !seen.insert(commit_id) {
            continue;
        }
        let Some(commit) = commits.get(commit_id) else {
            bail!("sync manifest is missing native commit payload {commit_id}");
        };
        for parent in &commit.parents {
            queue.push_back(parent.as_str());
        }
    }
    Ok(false)
}

/// Read a sync manifest from disk and fail closed unless it is the current native
/// Forge sync protocol. Public so the CLI can preflight materialized pulls before
/// importing a peer bundle.
pub fn read_supported_manifest(path: &Path) -> Result<SyncManifest> {
    let bytes = fs::read(path)?;
    let manifest: SyncManifest = serde_json::from_slice(&bytes)?;
    ensure_supported_manifest(&manifest)?;
    Ok(manifest)
}

fn ensure_supported_manifest(manifest: &SyncManifest) -> Result<()> {
    if manifest.protocol_version != SYNC_PROTOCOL_VERSION {
        bail!(
            "unsupported sync protocol version {}",
            manifest.protocol_version
        );
    }
    if manifest.content_backend != "native" {
        bail!("sync import only supports native content bundles");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum CurrentStateMode {
    Insert,
    Update,
}

fn apply_manifest(
    root_path: &Path,
    database_path: &Path,
    repo_id: &str,
    manifest: &SyncManifest,
    current_state_mode: CurrentStateMode,
) -> Result<usize> {
    let store = NativeObjectStore::new(root_path);
    write_manifest_objects(&store, manifest)?;
    let mut connection = Connection::open(database_path)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let imported_ledger_rows = import_ledger_rows(&mut connection, repo_id, manifest)?;
    if let Some(head) = manifest.native_head.as_deref() {
        let head_id = ObjectId::parse(head)?;
        if head_id.kind()? != ObjectKind::Commit {
            bail!("sync native head does not name a commit");
        }
        store
            .read_object(&head_id)
            .with_context(|| format!("sync native head object {head} is missing"))?;
        NativeRefStore::new(root_path).set_head(&head_id)?;
    }
    set_current_state(&connection, repo_id, manifest, current_state_mode)?;
    Ok(imported_ledger_rows)
}

fn write_manifest_objects(store: &NativeObjectStore, manifest: &SyncManifest) -> Result<()> {
    for payload in &manifest.native_payloads {
        let id = ObjectId::parse(&payload.object_id)?;
        let kind = object_kind_from_label(&payload.kind)?;
        if id.kind()? != kind {
            bail!(
                "sync object kind does not match object id {}",
                payload.object_id
            );
        }
        let bytes = hex_decode(&payload.payload_hex)?;
        let written = store.write_object(kind, &bytes)?;
        if written.to_string() != payload.object_id {
            bail!("sync object payload does not hash to {}", payload.object_id);
        }
    }
    Ok(())
}

fn ledger_counts(connection: &Connection) -> Result<Vec<LedgerTableCount>> {
    let mut counts = Vec::new();
    for table in LEDGER_COUNT_TABLES {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let rows = connection.query_row(&sql, [], |row| row.get(0))?;
        counts.push(LedgerTableCount {
            table: table.to_string(),
            rows,
        });
    }
    Ok(counts)
}

fn latest_key_fingerprint(connection: &Connection) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT key_fingerprint
             FROM ledger_signatures
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

const LEDGER_COUNT_TABLES: &[&str] = &[
    "repositories",
    "operations",
    "views",
    "intents",
    "attempts",
    "snapshots",
    "evidence",
    "proposals",
    "proposal_revisions",
    "check_results",
    "decisions",
    "publications",
    "ledger_signatures",
    "conflict_sets",
    "path_conflicts",
    "attempt_workspaces",
];

#[derive(Debug, Clone, Copy)]
struct LedgerTableSpec {
    table: &'static str,
    columns: &'static [&'static str],
}

const LEDGER_ROW_TABLES: &[LedgerTableSpec] = &[
    LedgerTableSpec {
        table: "operations",
        columns: &[
            "id",
            "repo_id",
            "request_id",
            "command",
            "status",
            "kind",
            "parent_operation_id",
            "resulting_view_id",
            "error_json",
            "content_hash",
            "created_at_ms",
        ],
    },
    LedgerTableSpec {
        table: "views",
        columns: &[
            "id",
            "repo_id",
            "operation_id",
            "kind",
            "state_json",
            "created_at_ms",
        ],
    },
    LedgerTableSpec {
        table: "intents",
        columns: &["id", "repo_id", "text", "created_at_ms", "check_spec_json"],
    },
    LedgerTableSpec {
        table: "attempts",
        columns: &[
            "id",
            "repo_id",
            "intent_id",
            "base_head",
            "status",
            "created_at_ms",
            "actor",
        ],
    },
    LedgerTableSpec {
        table: "snapshots",
        columns: &[
            "id",
            "repo_id",
            "attempt_id",
            "parent_snapshot_id",
            "content_ref",
            "changed_paths_json",
            "created_at_ms",
        ],
    },
    LedgerTableSpec {
        table: "evidence",
        columns: &[
            "id",
            "repo_id",
            "attempt_id",
            "snapshot_id",
            "command",
            "args_json",
            "cwd",
            "exit_code",
            "started_at_ms",
            "ended_at_ms",
            "stdout_excerpt",
            "stderr_excerpt",
            "stdout_truncated",
            "stderr_truncated",
            "timed_out",
            "sensitivity",
            "visibility",
            "trust",
            "created_at_ms",
            "content_hash",
            "structured_json",
            "actor",
        ],
    },
    LedgerTableSpec {
        table: "proposals",
        columns: &[
            "id",
            "repo_id",
            "attempt_id",
            "snapshot_id",
            "base_head",
            "content_ref",
            "status",
            "created_at_ms",
        ],
    },
    LedgerTableSpec {
        table: "proposal_revisions",
        columns: &[
            "id",
            "proposal_id",
            "snapshot_id",
            "content_ref",
            "changed_paths_json",
            "created_at_ms",
        ],
    },
    LedgerTableSpec {
        table: "check_results",
        columns: &[
            "id",
            "repo_id",
            "proposal_id",
            "proposal_revision_id",
            "status",
            "reason",
            "evidence_id",
            "created_at_ms",
        ],
    },
    LedgerTableSpec {
        table: "decisions",
        columns: &[
            "id",
            "repo_id",
            "proposal_id",
            "proposal_revision_id",
            "decision",
            "created_at_ms",
            "content_hash",
            "actor",
            "commit_id",
        ],
    },
    LedgerTableSpec {
        table: "publications",
        columns: &[
            "id",
            "repo_id",
            "proposal_id",
            "proposal_revision_id",
            "branch_name",
            "commit_id",
            "created_at_ms",
            "actor",
        ],
    },
    LedgerTableSpec {
        table: "ledger_signatures",
        columns: &[
            "id",
            "repo_id",
            "subject_kind",
            "subject_id",
            "signed_digest",
            "signature_alg",
            "public_key",
            "key_fingerprint",
            "signature",
            "trust_level",
            "created_at_ms",
        ],
    },
    LedgerTableSpec {
        table: "conflict_sets",
        columns: &[
            "id",
            "repo_id",
            "context",
            "paths_json",
            "created_at_ms",
            "base_content_ref",
            "ours_content_ref",
            "theirs_content_ref",
            "generated_by_operation_id",
            "resolver_backend",
            "status",
            "content_hash",
        ],
    },
    LedgerTableSpec {
        table: "path_conflicts",
        columns: &[
            "id",
            "conflict_set_id",
            "path",
            "path_fingerprint",
            "base_path",
            "ours_path",
            "theirs_path",
            "kind",
            "base_ref",
            "ours_ref",
            "theirs_ref",
            "base_status",
            "ours_status",
            "theirs_status",
            "base_mode",
            "ours_mode",
            "theirs_mode",
            "resolution_ref",
            "status",
            "created_at_ms",
        ],
    },
    LedgerTableSpec {
        table: "attempt_workspaces",
        columns: &[
            "attempt_id",
            "repo_id",
            "workspace_rel_path",
            "status",
            "materialized_content_ref",
            "created_at_ms",
            "updated_at_ms",
        ],
    },
];

fn ledger_rows(connection: &Connection) -> Result<Vec<LedgerTableRows>> {
    let mut tables = Vec::new();
    for spec in LEDGER_ROW_TABLES {
        tables.push(LedgerTableRows {
            table: spec.table.to_string(),
            rows: select_rows(connection, spec)?,
        });
    }
    Ok(tables)
}

fn select_rows(
    connection: &Connection,
    spec: &LedgerTableSpec,
) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
    let sql = format!(
        "SELECT {} FROM {} ORDER BY rowid",
        spec.columns.join(", "),
        spec.table
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut output = Vec::new();
    while let Some(row) = rows.next()? {
        let mut object = serde_json::Map::new();
        for (index, column) in spec.columns.iter().enumerate() {
            object.insert((*column).to_string(), json_from_sql(row.get_ref(index)?)?);
        }
        output.push(object);
    }
    Ok(output)
}

fn import_ledger_rows(
    connection: &mut Connection,
    target_repo_id: &str,
    manifest: &SyncManifest,
) -> Result<usize> {
    let tx = connection.transaction()?;
    let mut imported = 0;
    for spec in LEDGER_ROW_TABLES {
        let Some(table) = manifest
            .ledger_rows
            .iter()
            .find(|table| table.table == spec.table)
        else {
            continue;
        };
        for row in &table.rows {
            imported += insert_row(&tx, spec, row, target_repo_id)?;
        }
    }
    mark_imported_signature_keys(&tx, target_repo_id)?;
    tx.commit()?;
    Ok(imported)
}

fn mark_imported_signature_keys(conn: &Connection, repo_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO signing_keys (
            repo_id, key_fingerprint, public_key, trust_origin, created_at_ms, updated_at_ms
         )
         SELECT
            repo_id,
            key_fingerprint,
            MIN(public_key),
            'peer',
            MIN(created_at_ms),
            ?2
         FROM ledger_signatures
         WHERE repo_id = ?1
         GROUP BY repo_id, key_fingerprint
         ON CONFLICT(repo_id, key_fingerprint) DO UPDATE SET
            public_key = CASE
                WHEN signing_keys.trust_origin = 'local' THEN signing_keys.public_key
                ELSE excluded.public_key
            END,
            trust_origin = CASE
                WHEN signing_keys.trust_origin = 'local' THEN 'local'
                ELSE 'peer'
            END,
            updated_at_ms = excluded.updated_at_ms",
        params![repo_id, now_ms()],
    )?;
    Ok(())
}

fn set_current_state(
    connection: &Connection,
    repo_id: &str,
    manifest: &SyncManifest,
    mode: CurrentStateMode,
) -> Result<()> {
    match mode {
        CurrentStateMode::Insert => {
            connection.execute(
                "INSERT INTO current_state (
                    singleton, repo_id, current_operation_id, current_view_id,
                    attached_attempt_id, expected_content_ref, updated_at_ms
                 ) VALUES (1, ?1, ?2, ?3, NULL, NULL, ?4)",
                params![
                    repo_id,
                    manifest.current_operation_id,
                    manifest.current_view_id,
                    now_ms(),
                ],
            )?;
        }
        CurrentStateMode::Update => {
            connection.execute(
                "UPDATE current_state
                 SET current_operation_id = ?1,
                     current_view_id = ?2,
                     updated_at_ms = ?3
                 WHERE singleton = 1",
                params![
                    manifest.current_operation_id,
                    manifest.current_view_id,
                    now_ms(),
                ],
            )?;
        }
    }
    Ok(())
}

fn insert_row(
    connection: &Connection,
    spec: &LedgerTableSpec,
    row: &serde_json::Map<String, serde_json::Value>,
    target_repo_id: &str,
) -> Result<usize> {
    if spec.table == "ledger_signatures" {
        validate_ledger_signature_key(row)?;
    }
    let placeholders = std::iter::repeat_n("?", spec.columns.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT OR IGNORE INTO {} ({}) VALUES ({})",
        spec.table,
        spec.columns.join(", "),
        placeholders
    );
    let mut values = Vec::with_capacity(spec.columns.len());
    for column in spec.columns {
        if *column == "repo_id" {
            values.push(SqlValue::Text(target_repo_id.to_string()));
        } else {
            let value = row
                .get(*column)
                .ok_or_else(|| anyhow!("sync ledger row missing {}.{}", spec.table, column))?;
            values.push(sql_value_from_json(value)?);
        }
    }
    connection
        .execute(&sql, params_from_iter(values.iter()))
        .map_err(Into::into)
}

fn validate_ledger_signature_key(row: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let public_key = row
        .get("public_key")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("sync ledger row missing ledger_signatures.public_key"))?;
    let key_fingerprint = row
        .get("key_fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("sync ledger row missing ledger_signatures.key_fingerprint"))?;
    let public_key_bytes = hex_decode(public_key)
        .with_context(|| "sync ledger signature public_key must be lowercase hex")?;
    let recomputed = forge_store::signing_key_fingerprint_for_public_key(&public_key_bytes);
    if recomputed != key_fingerprint {
        bail!(
            "sync ledger signature key_fingerprint does not match public_key: expected {recomputed}, got {key_fingerprint}"
        );
    }
    Ok(())
}

fn expected_content_ref(connection: &Connection) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT expected_content_ref FROM current_state WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(Into::into)
}

fn object_kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Blob => "blob",
        ObjectKind::Tree => "tree",
        ObjectKind::Commit => "commit",
    }
}

fn object_kind_from_label(value: &str) -> Result<ObjectKind> {
    match value {
        "blob" => Ok(ObjectKind::Blob),
        "tree" => Ok(ObjectKind::Tree),
        "commit" => Ok(ObjectKind::Commit),
        _ => bail!("unsupported native object kind {value}"),
    }
}

fn json_from_sql(value: ValueRef<'_>) -> Result<serde_json::Value> {
    Ok(match value {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(value) => serde_json::Value::Number(value.into()),
        ValueRef::Real(value) => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| anyhow!("non-finite SQLite real value"))?,
        ValueRef::Text(value) => serde_json::Value::String(std::str::from_utf8(value)?.to_string()),
        ValueRef::Blob(value) => serde_json::Value::String(hex_encode(value)),
    })
}

fn sql_value_from_json(value: &serde_json::Value) -> Result<SqlValue> {
    Ok(match value {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        serde_json::Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                SqlValue::Integer(integer)
            } else if let Some(real) = value.as_f64() {
                SqlValue::Real(real)
            } else {
                bail!("unsupported JSON number in sync ledger row");
            }
        }
        serde_json::Value::String(value) => SqlValue::Text(value.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            SqlValue::Text(serde_json::to_string(value)?)
        }
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn hex_decode(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        bail!("malformed hex payload");
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(chunk)?;
        bytes.push(u8::from_str_radix(text, 16).context("malformed hex payload")?);
    }
    Ok(bytes)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest(head: &str, commits: &[(&str, &[&str])]) -> SyncManifest {
        SyncManifest {
            protocol_version: SYNC_PROTOCOL_VERSION.to_string(),
            cli_schema_version: forge_protocol::SCHEMA_VERSION.to_string(),
            repo_id: "repo_test".to_string(),
            content_backend: "native".to_string(),
            current_operation_id: "op_test".to_string(),
            current_view_id: "view_test".to_string(),
            attached_attempt_id: None,
            expected_content_ref: None,
            native_head: Some(head.to_string()),
            native_objects: Vec::new(),
            native_payloads: commits
                .iter()
                .map(|(id, parents)| {
                    let commit = forge_content_native::CommitObject {
                        schema_version: forge_content_native::COMMIT_SCHEMA_VERSION,
                        tree: format!("tree_{id}"),
                        parents: parents.iter().map(|parent| (*parent).to_string()).collect(),
                        intent_id: None,
                        proposal_revision_id: None,
                        decision_id: None,
                        evidence_digest: None,
                        actor: None,
                        authored_time: None,
                    };
                    SyncObjectPayload {
                        object_id: (*id).to_string(),
                        kind: "commit".to_string(),
                        payload_hex: hex_encode(&serde_json::to_vec(&commit).unwrap()),
                    }
                })
                .collect(),
            ledger_counts: Vec::new(),
            ledger_rows: Vec::new(),
            local_key_fingerprint: None,
        }
    }

    #[test]
    fn inspect_rejects_non_manifest_json() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bad.json");
        fs::write(&path, "{}").unwrap();
        assert!(inspect_manifest(&path).is_err());
    }

    #[test]
    fn inspect_rejects_unsupported_protocol_version() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("future.json");
        fs::write(
            &path,
            serde_json::to_vec(&SyncManifest {
                protocol_version: "forge-sync.v99".to_string(),
                cli_schema_version: forge_protocol::SCHEMA_VERSION.to_string(),
                repo_id: "repo_test".to_string(),
                content_backend: "native".to_string(),
                current_operation_id: "op_test".to_string(),
                current_view_id: "view_test".to_string(),
                attached_attempt_id: None,
                expected_content_ref: None,
                native_head: None,
                native_objects: Vec::new(),
                native_payloads: Vec::new(),
                ledger_counts: Vec::new(),
                ledger_rows: Vec::new(),
                local_key_fingerprint: None,
            })
            .unwrap(),
        )
        .unwrap();
        let error = inspect_manifest(&path).unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported sync protocol version"));
    }

    #[test]
    fn transport_export_report_uses_transport_boundary() {
        let temp = tempfile::tempdir().unwrap();
        forge_store::init_repository(temp.path(), None, "native".to_string()).unwrap();

        let (manifest, report) = export_manifest_for_transport_since(temp.path(), None).unwrap();

        assert_eq!(manifest.protocol_version, SYNC_PROTOCOL_VERSION);
        assert_eq!(report.protocol_version, SYNC_PROTOCOL_VERSION);
        assert_eq!(report.output_path, "<transport>");
        assert!(!report.incremental);
        assert_eq!(report.since_path, None);
        assert_eq!(report.content_backend, "native");
    }

    #[test]
    fn transport_import_rejects_unsupported_manifest_version() {
        let temp = tempfile::tempdir().unwrap();
        forge_store::init_repository(temp.path(), None, "native".to_string()).unwrap();
        let mut manifest = test_manifest("C", &[("C", &[])]);
        manifest.protocol_version = "forge-sync.v99".to_string();

        let error = import_manifest_value(temp.path(), &manifest).unwrap_err();

        assert!(error
            .to_string()
            .contains("unsupported sync protocol version"));
    }

    #[test]
    fn transport_incremental_export_rejects_unsupported_base_version() {
        let temp = tempfile::tempdir().unwrap();
        forge_store::init_repository(temp.path(), None, "native".to_string()).unwrap();
        let mut base = test_manifest("C", &[("C", &[])]);
        base.protocol_version = "forge-sync.v99".to_string();

        let error = export_manifest_for_transport_since(temp.path(), Some(&base)).unwrap_err();

        assert!(error
            .to_string()
            .contains("unsupported sync protocol version"));
    }

    #[test]
    fn common_ancestor_prefers_lowest_ancestor_over_distance_tie() {
        let left = test_manifest("L", &[("G", &[]), ("P", &["G"]), ("L", &["G", "P"])]);
        let right = test_manifest("R", &[("G", &[]), ("P", &["G"]), ("R", &["G", "P"])]);

        let base = manifest_common_ancestor_head(&left, &right).unwrap();

        assert_eq!(base.as_deref(), Some("P"));
    }

    #[test]
    fn common_ancestor_returns_none_when_native_head_is_absent() {
        let mut left = test_manifest("L", &[("L", &[])]);
        let right = test_manifest("R", &[("R", &[])]);

        left.native_head = None;
        assert_eq!(
            manifest_common_ancestor_head(&left, &right)
                .unwrap()
                .as_deref(),
            None
        );

        left.native_head = Some("L".to_string());
        let mut right_without_head = right;
        right_without_head.native_head = None;
        assert_eq!(
            manifest_common_ancestor_head(&left, &right_without_head)
                .unwrap()
                .as_deref(),
            None
        );
    }

    #[test]
    fn common_ancestor_returns_none_for_disjoint_histories() {
        let left = test_manifest("L", &[("A", &[]), ("L", &["A"])]);
        let right = test_manifest("R", &[("B", &[]), ("R", &["B"])]);

        let base = manifest_common_ancestor_head(&left, &right).unwrap();

        assert_eq!(base.as_deref(), None);
    }
}
