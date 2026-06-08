use crate::{
    is_executable, missing_dirs, scan_worktree, sync_dir, BlobOverlay, FileFingerprint, ObjectId,
    ObjectKind, SYMLINK_MODE,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const STATUS_CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StatusCache {
    schema_version: u32,
    entries: BTreeMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    signature: FileSignature,
    object_id: String,
    mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileSignature {
    kind: String,
    size: u64,
    modified_secs: u64,
    modified_nanos: u32,
    changed_secs: i64,
    changed_nanos: i64,
    device: u64,
    inode: u64,
    executable: bool,
}

pub(crate) fn working_fingerprints(
    repo_root: &Path,
) -> Result<(BTreeMap<String, FileFingerprint>, BlobOverlay)> {
    let existing = read_cache(repo_root);
    let mut next_entries = BTreeMap::new();
    let mut fingerprints = BTreeMap::new();
    let mut overlay = BTreeMap::new();

    for file in scan_worktree(repo_root)? {
        let full = repo_root.join(&file.path);
        let signature = file_signature(&full, file.symlink_target.is_some())?;
        if let Some(entry) = existing
            .as_ref()
            .and_then(|cache| cache.entries.get(&file.path))
            .filter(|entry| entry.signature == signature)
        {
            fingerprints.insert(file.path.clone(), (entry.object_id.clone(), entry.mode));
            next_entries.insert(file.path, entry.clone());
            continue;
        }

        let (bytes, mode) = match file.symlink_target {
            Some(target) => (target.into_bytes(), SYMLINK_MODE),
            None => {
                let bytes = fs::read(&full)?;
                (
                    bytes,
                    if signature.executable {
                        0o100755
                    } else {
                        0o100644
                    },
                )
            }
        };
        let object_id = ObjectId::new(ObjectKind::Blob, &bytes).to_string();
        overlay.insert(object_id.clone(), bytes);
        fingerprints.insert(file.path.clone(), (object_id.clone(), mode));
        next_entries.insert(
            file.path,
            CacheEntry {
                signature,
                object_id,
                mode,
            },
        );
    }

    let _ = write_cache(
        repo_root,
        &StatusCache {
            schema_version: STATUS_CACHE_SCHEMA_VERSION,
            entries: next_entries,
        },
    );
    Ok((fingerprints, overlay))
}

fn read_cache(repo_root: &Path) -> Option<StatusCache> {
    let bytes = fs::read(cache_path(repo_root)).ok()?;
    let cache: StatusCache = serde_json::from_slice(&bytes).ok()?;
    (cache.schema_version == STATUS_CACHE_SCHEMA_VERSION).then_some(cache)
}

fn write_cache(repo_root: &Path, cache: &StatusCache) -> Result<()> {
    let forge_dir = repo_root.join(".forge");
    let tmp_dir = forge_dir.join("tmp");
    let newly_created_forge_dirs = missing_dirs(&forge_dir);
    let newly_created_tmp_dirs = missing_dirs(&tmp_dir);
    fs::create_dir_all(&tmp_dir)?;
    sync_newly_created_dirs(&newly_created_forge_dirs)?;
    sync_newly_created_dirs(&newly_created_tmp_dirs)?;

    let mut temp = tempfile::NamedTempFile::new_in(&tmp_dir)?;
    temp.write_all(&serde_json::to_vec(cache)?)?;
    temp.as_file_mut().sync_all()?;
    temp.persist(cache_path(repo_root))
        .map_err(|error| error.error)?;
    sync_dir(&forge_dir)?;
    Ok(())
}

fn sync_newly_created_dirs(dirs: &[PathBuf]) -> Result<()> {
    for dir in dirs {
        if let Some(parent) = dir.parent() {
            sync_dir(parent)?;
        }
    }
    Ok(())
}

fn file_signature(path: &Path, symlink: bool) -> Result<FileSignature> {
    let metadata = fs::symlink_metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(FileSignature {
        kind: if symlink { "symlink" } else { "file" }.to_string(),
        size: metadata.len(),
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        changed_secs: changed_secs(&metadata),
        changed_nanos: changed_nanos(&metadata),
        device: device(&metadata),
        inode: inode(&metadata),
        executable: !symlink && is_executable(&metadata),
    })
}

#[cfg(unix)]
fn changed_secs(metadata: &fs::Metadata) -> i64 {
    metadata.ctime()
}

#[cfg(not(unix))]
fn changed_secs(_metadata: &fs::Metadata) -> i64 {
    0
}

#[cfg(unix)]
fn changed_nanos(metadata: &fs::Metadata) -> i64 {
    metadata.ctime_nsec()
}

#[cfg(not(unix))]
fn changed_nanos(_metadata: &fs::Metadata) -> i64 {
    0
}

#[cfg(unix)]
fn device(metadata: &fs::Metadata) -> u64 {
    metadata.dev()
}

#[cfg(not(unix))]
fn device(_metadata: &fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn inode(metadata: &fs::Metadata) -> u64 {
    metadata.ino()
}

#[cfg(not(unix))]
fn inode(_metadata: &fs::Metadata) -> u64 {
    0
}

fn cache_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".forge/status-cache.json")
}
