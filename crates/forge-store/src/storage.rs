use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const GC_PROTECTION_WINDOW_DAYS: u64 = 7;
const DEFAULT_STORAGE_BUDGET_BYTES: u64 = 1_073_741_824;

#[derive(Debug, Clone, Serialize, Default)]
pub struct StorageAccounting {
    pub total_bytes: u64,
    pub loose_objects: StorageCategoryAccounting,
    pub packs: StorageCategoryAccounting,
    pub database: StorageCategoryAccounting,
    pub temp: StorageCategoryAccounting,
    pub worktrees: StorageCategoryAccounting,
    pub evidence_outputs: StorageCategoryAccounting,
    pub other: StorageCategoryAccounting,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct StorageCategoryAccounting {
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoragePolicy {
    pub protection_window_days: u64,
    pub storage_budget_bytes: u64,
    pub automatic_eviction: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageBudgetStatus {
    pub limit_bytes: u64,
    pub used_bytes: u64,
    pub over_budget: bool,
    pub over_by_bytes: u64,
}

pub fn storage_accounting(cwd: &Path) -> Result<StorageAccounting> {
    let context = crate::open_repository(cwd)?;
    storage_accounting_for_root(&context.root_path)
}

pub fn storage_budget_status(cwd: &Path) -> Result<StorageBudgetStatus> {
    let context = crate::open_repository(cwd)?;
    let connection = crate::open_connection(&context.database_path)?;
    let storage = storage_accounting_for_root(&context.root_path)?;
    let policy = storage_policy(&connection)?;
    Ok(storage_budget_status_for(&storage, &policy))
}

pub(crate) fn storage_accounting_for_root(root: &Path) -> Result<StorageAccounting> {
    let forge_dir = root.join(".forge");
    let total = account_path(&forge_dir)?;
    let loose_objects = account_path(&forge_dir.join("objects"))?;
    let packs = account_path(&forge_dir.join("packs"))?;
    let temp = account_path(&forge_dir.join("tmp"))?;
    let worktrees = account_path(&forge_dir.join("worktrees"))?;
    let evidence_outputs = account_multiple_paths(&[
        forge_dir.join("evidence"),
        forge_dir.join("evidence-outputs"),
        forge_dir.join("outputs"),
    ])?;
    let database = account_multiple_paths(&[
        forge_dir.join("forge.db"),
        forge_dir.join("forge.db-wal"),
        forge_dir.join("forge.db-shm"),
        forge_dir.join("forge.db-journal"),
    ])?;

    let known_bytes = loose_objects
        .bytes
        .saturating_add(packs.bytes)
        .saturating_add(database.bytes)
        .saturating_add(temp.bytes)
        .saturating_add(worktrees.bytes)
        .saturating_add(evidence_outputs.bytes);
    let known_files = loose_objects
        .files
        .saturating_add(packs.files)
        .saturating_add(database.files)
        .saturating_add(temp.files)
        .saturating_add(worktrees.files)
        .saturating_add(evidence_outputs.files);

    Ok(StorageAccounting {
        total_bytes: total.bytes,
        loose_objects,
        packs,
        database,
        temp,
        worktrees,
        evidence_outputs,
        other: StorageCategoryAccounting {
            bytes: total.bytes.saturating_sub(known_bytes),
            files: total.files.saturating_sub(known_files),
        },
    })
}

pub(crate) fn storage_policy(connection: &Connection) -> Result<StoragePolicy> {
    let row = connection
        .query_row(
            "SELECT protection_window_days, storage_budget_bytes, automatic_eviction
             FROM storage_policy
             WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((protection_window_days, storage_budget_bytes, automatic_eviction)) = row else {
        return Ok(default_storage_policy());
    };
    Ok(StoragePolicy {
        protection_window_days: protection_window_days.max(1) as u64,
        storage_budget_bytes: storage_budget_bytes.max(0) as u64,
        automatic_eviction: automatic_eviction != 0,
    })
}

fn default_storage_policy() -> StoragePolicy {
    StoragePolicy {
        protection_window_days: GC_PROTECTION_WINDOW_DAYS,
        storage_budget_bytes: DEFAULT_STORAGE_BUDGET_BYTES,
        automatic_eviction: false,
    }
}

pub(crate) fn storage_budget_status_for(
    storage: &StorageAccounting,
    policy: &StoragePolicy,
) -> StorageBudgetStatus {
    let over_by_bytes = storage
        .total_bytes
        .saturating_sub(policy.storage_budget_bytes);
    StorageBudgetStatus {
        limit_bytes: policy.storage_budget_bytes,
        used_bytes: storage.total_bytes,
        over_budget: over_by_bytes > 0,
        over_by_bytes,
    }
}

fn account_multiple_paths(paths: &[PathBuf]) -> Result<StorageCategoryAccounting> {
    let mut total = StorageCategoryAccounting::default();
    for path in paths {
        total.add(account_path(path)?);
    }
    Ok(total)
}

fn account_path(path: &Path) -> Result<StorageCategoryAccounting> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StorageCategoryAccounting::default());
        }
        Err(error) => return Err(error).with_context(|| "read storage metadata"),
    };
    if metadata.is_file() {
        return Ok(StorageCategoryAccounting {
            bytes: metadata.len(),
            files: 1,
        });
    }
    if metadata.file_type().is_symlink() {
        return Ok(StorageCategoryAccounting {
            bytes: metadata.len(),
            files: 1,
        });
    }
    if !metadata.is_dir() {
        return Ok(StorageCategoryAccounting::default());
    }

    let mut total = StorageCategoryAccounting::default();
    for entry in fs::read_dir(path).with_context(|| "read storage directory")? {
        total.add(account_path(&entry?.path())?);
    }
    Ok(total)
}

impl StorageCategoryAccounting {
    fn add(&mut self, other: StorageCategoryAccounting) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.files = self.files.saturating_add(other.files);
    }
}
