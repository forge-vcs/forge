use anyhow::{anyhow, bail, Context, Result};
use forge_content::{is_secret_risk_path, ContentBackend, SnapshotContent, FORGE_TREE_PREFIX};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    Blob,
    Tree,
}

impl ObjectKind {
    fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Blob => "blob",
            ObjectKind::Tree => "tree",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId {
    kind: String,
    digest: String,
}

impl ObjectId {
    pub fn new(kind: ObjectKind, payload: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"forge-object\n");
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"\n");
        hasher.update(SCHEMA_VERSION.to_string().as_bytes());
        hasher.update(b"\n");
        hasher.update(payload.len().to_string().as_bytes());
        hasher.update(b"\n");
        hasher.update(payload);
        Self {
            kind: kind.as_str().to_string(),
            digest: hex_lower(&hasher.finalize()),
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        let parts: Vec<&str> = value.split(':').collect();
        if parts.len() != 4 || parts[0] != "f1" || parts[2] != "sha256" {
            bail!("malformed native object id");
        }
        match parts[1] {
            "blob" | "tree" => {}
            _ => bail!("unsupported native object type"),
        }
        if parts[3].len() != 64 || !parts[3].bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("malformed native object digest");
        }
        Ok(Self {
            kind: parts[1].to_string(),
            digest: parts[3].to_ascii_lowercase(),
        })
    }

    pub fn kind(&self) -> Result<ObjectKind> {
        match self.kind.as_str() {
            "blob" => Ok(ObjectKind::Blob),
            "tree" => Ok(ObjectKind::Tree),
            _ => bail!("unsupported native object type"),
        }
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "f1:{}:sha256:{}", self.kind, self.digest)
    }
}

#[derive(Debug, Clone)]
pub struct NativeContentBackend;

impl ContentBackend for NativeContentBackend {
    fn snapshot_worktree(&self, repo_root: &Path) -> Result<SnapshotContent> {
        let store = NativeObjectStore::new(repo_root);
        let files = scan_worktree(repo_root)?;
        let root = write_tree(&store, repo_root, &files, "")?;
        Ok(SnapshotContent {
            content_ref: format!("{FORGE_TREE_PREFIX}{root}"),
            changed_paths: changed_paths(repo_root)?,
        })
    }

    fn restore_snapshot(&self, repo_root: &Path, content_ref: &str) -> Result<()> {
        let root = object_id_from_content_ref(content_ref)?;
        let store = NativeObjectStore::new(repo_root);
        store.verify_content_ref(content_ref)?;
        let mut target_paths = BTreeSet::new();
        materialize_tree(&store, repo_root, &root, "", &mut target_paths)?;
        for path in materialized_paths(repo_root)? {
            if !target_paths.contains(&path) {
                let full = repo_root.join(&path);
                if full.is_file() || full.is_symlink() {
                    fs::remove_file(&full).with_context(|| format!("remove {}", full.display()))?;
                }
            }
        }
        Ok(())
    }
}

pub fn materialize_content_ref(
    repo_root: &Path,
    destination: &Path,
    content_ref: &str,
) -> Result<()> {
    let root = object_id_from_content_ref(content_ref)?;
    let store = NativeObjectStore::new(repo_root);
    let mut target_paths = BTreeSet::new();
    materialize_tree(&store, destination, &root, "", &mut target_paths)
}

#[derive(Debug, Clone)]
pub struct NativeObjectStore {
    root: PathBuf,
}

impl NativeObjectStore {
    pub fn new(repo_root: &Path) -> Self {
        Self {
            root: repo_root.to_path_buf(),
        }
    }

    pub fn write_object(&self, kind: ObjectKind, payload: &[u8]) -> Result<ObjectId> {
        let id = ObjectId::new(kind, payload);
        let path = self.object_path(&id);
        if path.exists() {
            self.read_object(&id)?;
            return Ok(id);
        }

        let parent = path.parent().context("object path has no parent")?;
        // Record which ancestor directories do not yet exist so their creation can be
        // made durable after the object is written: a freshly created shard directory's
        // own entry is not durable until the directory it lives in is fsynced.
        let newly_created = missing_dirs(parent);
        fs::create_dir_all(parent)?;
        fs::create_dir_all(self.tmp_dir())?;
        let mut temp = tempfile::NamedTempFile::new_in(self.tmp_dir())?;
        temp.write_all(payload)?;
        temp.as_file_mut().sync_all()?;
        temp.persist(&path).map_err(|error| error.error)?;
        // The object's directory entry must reach disk before any DB row references it.
        // A swallowed failure here is exactly the durability hole this fix closes.
        sync_dir(parent)?;
        // Make each newly created ancestor directory durable by fsyncing the directory
        // that gained the new entry.
        for dir in &newly_created {
            if let Some(grandparent) = dir.parent() {
                sync_dir(grandparent)?;
            }
        }
        Ok(id)
    }

    pub fn read_object(&self, id: &ObjectId) -> Result<Vec<u8>> {
        let path = self.object_path(id);
        let payload =
            fs::read(&path).with_context(|| format!("missing native content object {}", id))?;
        let actual = ObjectId::new(id.kind()?, &payload);
        if &actual != id {
            bail!("hash mismatch for native content object {}", id);
        }
        Ok(payload)
    }

    pub fn verify_content_ref(&self, content_ref: &str) -> Result<BTreeSet<ObjectId>> {
        let root = object_id_from_content_ref(content_ref)?;
        let mut seen = BTreeSet::new();
        self.verify_reachable(&root, &mut seen)?;
        Ok(seen)
    }

    pub fn all_object_ids(&self) -> Result<BTreeSet<ObjectId>> {
        let mut ids = BTreeSet::new();
        let dir = self.root.join(".forge/objects/sha256");
        if !dir.exists() {
            return Ok(ids);
        }
        for prefix in fs::read_dir(dir)? {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(prefix.path())? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let digest = entry.file_name().to_string_lossy().into_owned();
                let bytes = fs::read(entry.path())?;
                for kind in [ObjectKind::Blob, ObjectKind::Tree] {
                    let id = ObjectId::new(kind, &bytes);
                    if id.digest == digest {
                        ids.insert(id);
                    }
                }
            }
        }
        Ok(ids)
    }

    fn verify_reachable(&self, id: &ObjectId, seen: &mut BTreeSet<ObjectId>) -> Result<()> {
        if !seen.insert(id.clone()) {
            return Ok(());
        }
        let payload = self.read_object(id)?;
        if id.kind()? == ObjectKind::Tree {
            let tree: TreeObject = serde_json::from_slice(&payload)
                .with_context(|| format!("malformed native tree object {}", id))?;
            if tree.schema_version != SCHEMA_VERSION {
                bail!("unsupported native tree schema version");
            }
            for entry in tree.entries {
                validate_tree_entry(&entry)?;
                let child = ObjectId::parse(&entry.object)?;
                ensure_child_kind(&entry, &child)?;
                self.verify_reachable(&child, seen)?;
            }
        }
        Ok(())
    }

    fn object_path(&self, id: &ObjectId) -> PathBuf {
        self.root
            .join(".forge/objects/sha256")
            .join(&id.digest()[..2])
            .join(id.digest())
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join(".forge/tmp")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TreeObject {
    schema_version: u32,
    entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TreeEntry {
    name: String,
    kind: TreeEntryKind,
    mode: u32,
    object: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TreeEntryKind {
    File,
    Dir,
}

#[derive(Debug, Clone)]
struct FileEntry {
    path: String,
    executable: bool,
}

fn object_id_from_content_ref(content_ref: &str) -> Result<ObjectId> {
    ObjectId::parse(
        content_ref
            .strip_prefix(FORGE_TREE_PREFIX)
            .ok_or_else(|| anyhow!("unsupported content ref"))?,
    )
}

fn write_tree(
    store: &NativeObjectStore,
    repo_root: &Path,
    files: &[FileEntry],
    prefix: &str,
) -> Result<ObjectId> {
    let mut grouped: BTreeMap<String, Vec<FileEntry>> = BTreeMap::new();
    let mut direct_files = Vec::new();

    for file in files {
        let rest = if prefix.is_empty() {
            file.path.as_str()
        } else if let Some(rest) = file.path.strip_prefix(&format!("{prefix}/")) {
            rest
        } else {
            continue;
        };

        if let Some((dir, _)) = rest.split_once('/') {
            grouped
                .entry(dir.to_string())
                .or_default()
                .push(file.clone());
        } else {
            direct_files.push(file.clone());
        }
    }

    let mut entries = Vec::new();
    for file in direct_files {
        let bytes = fs::read(repo_root.join(&file.path))?;
        let blob = store.write_object(ObjectKind::Blob, &bytes)?;
        let name = file
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&file.path)
            .to_string();
        entries.push(TreeEntry {
            name,
            kind: TreeEntryKind::File,
            mode: if file.executable { 0o100755 } else { 0o100644 },
            object: blob.to_string(),
        });
    }

    for (dir, children) in grouped {
        let child_prefix = if prefix.is_empty() {
            dir.clone()
        } else {
            format!("{prefix}/{dir}")
        };
        let child = write_tree(store, repo_root, &children, &child_prefix)?;
        entries.push(TreeEntry {
            name: dir,
            kind: TreeEntryKind::Dir,
            mode: 0o040000,
            object: child.to_string(),
        });
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let payload = serde_json::to_vec(&TreeObject {
        schema_version: SCHEMA_VERSION,
        entries,
    })?;
    store.write_object(ObjectKind::Tree, &payload)
}

fn materialize_tree(
    store: &NativeObjectStore,
    repo_root: &Path,
    tree_id: &ObjectId,
    prefix: &str,
    target_paths: &mut BTreeSet<String>,
) -> Result<()> {
    if tree_id.kind()? != ObjectKind::Tree {
        bail!("native content root is not a tree");
    }
    let payload = store.read_object(tree_id)?;
    let tree: TreeObject = serde_json::from_slice(&payload)?;
    if tree.schema_version != SCHEMA_VERSION {
        bail!("unsupported native tree schema version");
    }

    for entry in tree.entries {
        validate_tree_entry(&entry)?;
        let rel = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        if is_ignored_by_policy(&rel) {
            continue;
        }
        let child = ObjectId::parse(&entry.object)?;
        ensure_child_kind(&entry, &child)?;
        match entry.kind {
            TreeEntryKind::File => {
                let bytes = store.read_object(&child)?;
                let full = repo_root.join(&rel);
                if full.is_dir() {
                    fs::remove_dir_all(&full)
                        .with_context(|| format!("remove directory {}", full.display()))?;
                }
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&full, bytes)?;
                set_file_mode(&full, entry.mode)?;
                target_paths.insert(rel);
            }
            TreeEntryKind::Dir => {
                let full = repo_root.join(&rel);
                if full.is_file() || full.is_symlink() {
                    fs::remove_file(&full)
                        .with_context(|| format!("remove file {}", full.display()))?;
                }
                fs::create_dir_all(full)?;
                materialize_tree(store, repo_root, &child, &rel, target_paths)?;
            }
        }
    }
    Ok(())
}

fn scan_worktree(repo_root: &Path) -> Result<Vec<FileEntry>> {
    let mut files = Vec::new();
    for path in snapshot_candidate_paths(repo_root)? {
        if is_ignored_by_policy(&path) {
            continue;
        }
        let full = repo_root.join(&path);
        let metadata = match fs::metadata(&full) {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        files.push(FileEntry {
            path,
            executable: is_executable(&metadata),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn snapshot_candidate_paths(repo_root: &Path) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    for args in [
        ["ls-files"].as_slice(),
        ["ls-files", "--others", "--exclude-standard"].as_slice(),
    ] {
        let output = git(repo_root, args)?;
        paths.extend(output.lines().map(str::to_string));
    }
    Ok(paths.into_iter().collect())
}

fn materialized_paths(repo_root: &Path) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    for file in scan_worktree(repo_root)? {
        paths.insert(file.path);
    }
    Ok(paths)
}

fn changed_paths(repo_root: &Path) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    if let Ok(output) = git(repo_root, &["diff", "--name-only", "HEAD", "--", "."]) {
        paths.extend(output.lines().map(str::to_string));
    }
    if let Ok(output) = git(repo_root, &["ls-files", "--others", "--exclude-standard"]) {
        paths.extend(output.lines().map(str::to_string));
    }
    paths.retain(|path| !is_ignored_by_policy(path));
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("failed to run git {args:?}"))?;
    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn is_ignored_by_policy(path: &str) -> bool {
    path == ".git"
        || path.starts_with(".git/")
        || path == ".forge"
        || path.starts_with(".forge/")
        || is_secret_risk_path(path)
}

fn validate_tree_entry(entry: &TreeEntry) -> Result<()> {
    if entry.name.is_empty()
        || entry.name == "."
        || entry.name == ".."
        || entry.name.contains('/')
        || entry.name.contains('\\')
        || Path::new(&entry.name).is_absolute()
    {
        bail!("unsafe native tree entry name");
    }
    Ok(())
}

fn ensure_child_kind(entry: &TreeEntry, child: &ObjectId) -> Result<()> {
    match (entry.kind, child.kind()?) {
        (TreeEntryKind::File, ObjectKind::Blob) | (TreeEntryKind::Dir, ObjectKind::Tree) => Ok(()),
        _ => bail!("native tree entry points at wrong object type"),
    }
}

/// Fsync a directory so a newly created or renamed entry within it is durable.
/// Errors propagate — a swallowed directory sync silently breaks crash-durability,
/// which is the hole this replaces. This is a deliberate fail-hard: a write whose
/// directory entry may not survive a crash must not report success.
/// Known limitation: a few filesystems (some network/overlay mounts) reject fsync on a
/// directory fd (EINVAL/ENOTSUP), where directory durability is meaningless anyway; on
/// those `.forge` locations write_object will now error. `.forge` is expected to be on a
/// local filesystem (Phase 1b's WAL work makes that constraint explicit), so tolerating
/// those errnos as a degraded no-op is deferred rather than guessed at here.
fn sync_dir(path: &Path) -> Result<()> {
    let dir = File::open(path)
        .with_context(|| format!("open directory for fsync: {}", path.display()))?;
    dir.sync_all()
        .with_context(|| format!("fsync directory: {}", path.display()))?;
    Ok(())
}

/// Ancestor directories of `dir` (inclusive) that do not yet exist, ordered
/// shallowest-first. Lets the caller fsync only the directories whose creation is
/// new, so already-durable directories are not re-synced on every write.
fn missing_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut missing = Vec::new();
    let mut current = Some(dir);
    while let Some(path) = current {
        if path.exists() {
            break;
        }
        missing.push(path.to_path_buf());
        current = path.parent();
    }
    missing.reverse();
    missing
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_file_mode(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_ids_are_domain_separated() {
        let blob = ObjectId::new(ObjectKind::Blob, b"same");
        let tree = ObjectId::new(ObjectKind::Tree, b"same");
        assert_eq!(blob, ObjectId::new(ObjectKind::Blob, b"same"));
        assert_ne!(blob, tree);
        assert!(ObjectId::parse(&blob.to_string()).is_ok());
        assert!(ObjectId::parse("f1:blob:sha256:not-hex").is_err());
    }

    #[test]
    fn loose_object_write_is_idempotent_and_verified() {
        let temp = tempfile::tempdir().unwrap();
        let store = NativeObjectStore::new(temp.path());
        let id = store.write_object(ObjectKind::Blob, b"payload").unwrap();
        let again = store.write_object(ObjectKind::Blob, b"payload").unwrap();
        assert_eq!(id, again);
        assert_eq!(store.read_object(&id).unwrap(), b"payload");

        fs::write(store.object_path(&id), b"corrupt").unwrap();
        assert!(store
            .read_object(&id)
            .unwrap_err()
            .to_string()
            .contains("hash mismatch"));
    }

    #[test]
    fn sync_dir_propagates_error_on_missing_path() {
        // Directory fsync rarely fails on a healthy FS (and on macOS a dir fsync is a
        // near-noop), so exercise the error path through a mockable seam: a path that
        // cannot be opened must surface an Err rather than being swallowed.
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");
        assert!(sync_dir(&missing).is_err());
    }

    #[test]
    fn missing_dirs_lists_uncreated_ancestors_shallowest_first() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = a.join("b");
        let c = b.join("c");
        // temp.path() already exists; a, b, c do not.
        assert_eq!(missing_dirs(&c), vec![a.clone(), b.clone(), c.clone()]);
        fs::create_dir_all(&c).unwrap();
        assert!(missing_dirs(&c).is_empty());
    }

    #[test]
    fn write_object_into_fresh_shard_is_durable_and_roundtrips() {
        // A fresh store means the sha256/<prefix>/ shard dir is newly created, exercising
        // the ancestor-fsync path; the object must round-trip after write_object returns Ok.
        let temp = tempfile::tempdir().unwrap();
        let store = NativeObjectStore::new(temp.path());
        let id = store
            .write_object(ObjectKind::Blob, b"durable-payload")
            .unwrap();
        assert_eq!(store.read_object(&id).unwrap(), b"durable-payload");
    }
}
