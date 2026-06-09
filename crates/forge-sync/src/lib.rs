use anyhow::{bail, Result};
use forge_content_native::{NativeObjectStore, NativeRefStore};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const SYNC_PROTOCOL_VERSION: &str = "forge-sync.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncManifest {
    pub protocol_version: String,
    pub cli_schema_version: String,
    pub repo_id: String,
    pub content_backend: String,
    pub current_operation_id: String,
    pub current_view_id: String,
    pub native_head: Option<String>,
    pub native_objects: Vec<SyncObjectRef>,
    pub ledger_counts: Vec<LedgerTableCount>,
    pub local_key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncObjectRef {
    pub object_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerTableCount {
    pub table: String,
    pub rows: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncExportReport {
    pub protocol_version: String,
    pub output_path: String,
    pub content_backend: String,
    pub native_object_count: usize,
    pub ledger_table_count: usize,
    pub native_head: Option<String>,
    pub local_key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncInspectReport {
    pub protocol_version: String,
    pub content_backend: String,
    pub native_object_count: usize,
    pub ledger_table_count: usize,
    pub native_head: Option<String>,
    pub local_key_fingerprint: Option<String>,
}

pub fn build_manifest(cwd: &Path) -> Result<SyncManifest> {
    let context = forge_store::open_repository(cwd)?;
    let connection = Connection::open(&context.database_path)?;
    let (native_head, native_objects) = if context.content_backend == "native" {
        let store = NativeObjectStore::new(&context.root_path);
        let refs = NativeRefStore::new(&context.root_path);
        let mut objects = Vec::new();
        for id in store.all_object_ids()? {
            objects.push(SyncObjectRef {
                kind: format!("{:?}", id.kind()?).to_lowercase(),
                object_id: id.to_string(),
            });
        }
        (refs.read_head()?.map(|head| head.to_string()), objects)
    } else {
        (None, Vec::new())
    };

    Ok(SyncManifest {
        protocol_version: SYNC_PROTOCOL_VERSION.to_string(),
        cli_schema_version: forge_protocol::SCHEMA_VERSION.to_string(),
        repo_id: context.repo_id,
        content_backend: context.content_backend,
        current_operation_id: context.current_operation_id,
        current_view_id: context.current_view_id,
        native_head,
        native_objects,
        ledger_counts: ledger_counts(&connection)?,
        local_key_fingerprint: latest_key_fingerprint(&connection)?,
    })
}

pub fn export_manifest(cwd: &Path, output_path: &Path) -> Result<SyncExportReport> {
    let manifest = build_manifest(cwd)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(output_path, bytes)?;
    Ok(SyncExportReport {
        protocol_version: manifest.protocol_version,
        output_path: output_path.display().to_string(),
        content_backend: manifest.content_backend,
        native_object_count: manifest.native_objects.len(),
        ledger_table_count: manifest.ledger_counts.len(),
        native_head: manifest.native_head,
        local_key_fingerprint: manifest.local_key_fingerprint,
    })
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
        ledger_table_count: manifest.ledger_counts.len(),
        native_head: manifest.native_head,
        local_key_fingerprint: manifest.local_key_fingerprint,
    })
}

fn ledger_counts(connection: &Connection) -> Result<Vec<LedgerTableCount>> {
    let mut counts = Vec::new();
    for table in [
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
    ] {
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

#[cfg(test)]
mod tests {
    use super::*;

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
                native_head: None,
                native_objects: Vec::new(),
                ledger_counts: Vec::new(),
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
}
