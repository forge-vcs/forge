use anyhow::{anyhow, bail, Context, Result};
use forge_content::{is_ignored_by_policy, ContentBackend, SnapshotContent, FORGE_TREE_PREFIX};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
// `Command` is now used only by the `#[cfg(test)]` differential harness (slice-1 parity
// proofs); native base/changed-paths no longer shell git (NER-138 Phase 7 slice 2).
#[cfg(test)]
use std::process::Command;

const SCHEMA_VERSION: u32 = 1;

/// Re-exported from `forge_content` so `forge_store::doctor` keeps referencing
/// `forge_content_native::RESTORE_TEMP_PREFIX`, while the canonical definition and
/// its matching `is_restore_temp_path` exclusion predicate live in the shared base
/// crate both backends depend on (NER-132 U4).
pub use forge_content::RESTORE_TEMP_PREFIX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    Blob,
    Tree,
    /// An intent-aware commit/Change node (NER-138 Phase 7 slice 2). The handoff
    /// names this "Commit/Change"; it is implemented as a single kind — there is no
    /// separate `Change` kind. Domain-separated from `Blob`/`Tree` via the `as_str`
    /// tag below so the same payload hashes to a distinct id per kind.
    Commit,
}

impl ObjectKind {
    fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Blob => "blob",
            ObjectKind::Tree => "tree",
            ObjectKind::Commit => "commit",
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
            "blob" | "tree" | "commit" => {}
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
            "commit" => Ok(ObjectKind::Commit),
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
        // NER-138 Phase 7 slice 2: changed_paths is now a native name-level diff of the
        // base HEAD tree against the freshly-built worktree tree — reusing `root` rather
        // than re-walking/re-hashing the worktree.
        let changed = changed_paths(&store, repo_root, &root)?;
        Ok(SnapshotContent {
            content_ref: format!("{FORGE_TREE_PREFIX}{root}"),
            changed_paths: changed,
        })
    }

    fn restore_snapshot(&self, repo_root: &Path, content_ref: &str) -> Result<()> {
        let root = object_id_from_content_ref(content_ref)?;
        let store = NativeObjectStore::new(repo_root);
        store.verify_content_ref(content_ref)?;
        let mut target_paths = BTreeSet::new();
        let mut synced_dirs = BTreeSet::new();
        materialize_tree(
            &store,
            repo_root,
            &root,
            "",
            &mut target_paths,
            &mut synced_dirs,
        )?;
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

    // NER-138 Phase 7 slice 2: native base anchoring. `current_base` returns the native
    // history tip — the ref store's `HEAD` — lazily creating a genesis root commit over
    // the start-time worktree on first call (see `ensure_head`). The git delegation (and
    // the Phase-3 "do NOT emit forge-tree: base refs yet" guard) are gone: a native repo
    // now anchors on its own `f1:commit:` id, not a git commit, stable across worktree
    // edits. S1: returns an opaque revision id; no filesystem paths in error context.
    fn current_base(&self, repo_root: &Path) -> Result<String> {
        Ok(ensure_head(repo_root)?.to_string())
    }

    // NER-138 Phase 7 slice 2: resolve a native base commit to the restorable content ref
    // that materializes its tree (for `attempt attach`). S2: the tree was built by the
    // policy-filtered walker, so it already excludes `is_ignored_by_policy` paths
    // (.env/keys never materialized). S1: a parse/read failure surfaces a path-free error
    // (`read_commit` carries only the opaque object id).
    fn base_content_ref(&self, repo_root: &Path, base: &str) -> Result<String> {
        let store = NativeObjectStore::new(repo_root);
        let commit = store.read_commit(&ObjectId::parse(base)?)?;
        Ok(format!("{FORGE_TREE_PREFIX}{}", commit.tree))
    }
}

/// Ensure the native ref store has a `HEAD` and return it (NER-138 Phase 7 slice 2).
///
/// If a tip already exists, return it. Otherwise create the **genesis root commit** over
/// the current worktree tree (parentless, null justification), persist it, point `HEAD` at
/// it, and return its id. This is an intentional "ensure the base anchor exists" side
/// effect, not a pure read: in the normal lifecycle the first `current_base` caller is
/// `start`, so the genesis captures the *start-time* worktree as the base — never a
/// mid-`save` dirty tree, because `save` requires an active attempt that `start` created.
/// Every `current_base` caller is a mutating command holding the advisory lock
/// (acquire-once); `ensure_head` itself never acquires the lock, so genesis creation
/// cannot deadlock or race. Idempotent: the commit is content-addressed and `set_head` is
/// an atomic overwrite, so a repeated call returns the same id.
fn ensure_head(repo_root: &Path) -> Result<ObjectId> {
    let refs = NativeRefStore::new(repo_root);
    if let Some(head) = refs.read_head()? {
        return Ok(head);
    }
    let store = NativeObjectStore::new(repo_root);
    let files = scan_worktree(repo_root)?;
    let tree = write_tree(&store, repo_root, &files, "")?;
    let genesis = CommitObject {
        schema_version: SCHEMA_VERSION,
        tree: tree.to_string(),
        parents: Vec::new(),
        intent_id: None,
        proposal_revision_id: None,
        decision_id: None,
        evidence_digest: None,
        // Genesis has no decider; `actor`/`authored_time` stay `None` and (via
        // skip_serializing_if) are omitted, so the genesis hash is byte-identical to slice 2.
        actor: None,
        authored_time: None,
    };
    // Store-before-DB: the genesis object + HEAD are durable before `current_base`
    // returns, so the `attempts.base_head` row that records this id is written only after
    // its referent is on disk.
    let id = store.write_commit(&genesis)?;
    refs.set_head(&id)?;
    Ok(id)
}

pub fn materialize_content_ref(
    repo_root: &Path,
    destination: &Path,
    content_ref: &str,
) -> Result<()> {
    let root = object_id_from_content_ref(content_ref)?;
    let store = NativeObjectStore::new(repo_root);
    let mut target_paths = BTreeSet::new();
    let mut synced_dirs = BTreeSet::new();
    materialize_tree(
        &store,
        destination,
        &root,
        "",
        &mut target_paths,
        &mut synced_dirs,
    )
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

    /// Write a native commit object (NER-138 Phase 7 slice 2), returning its
    /// content-addressed id. Inherits `write_object`'s store-before-DB durability
    /// (temp + fsync + atomic rename + parent-dir fsync) verbatim.
    pub fn write_commit(&self, commit: &CommitObject) -> Result<ObjectId> {
        let payload = serde_json::to_vec(commit)?;
        self.write_object(ObjectKind::Commit, &payload)
    }

    /// Read and validate a native commit object. S1: never interpolates a filesystem
    /// path — `read_object`'s context carries only the opaque object id.
    pub fn read_commit(&self, id: &ObjectId) -> Result<CommitObject> {
        if id.kind()? != ObjectKind::Commit {
            bail!("native object is not a commit");
        }
        let payload = self.read_object(id)?;
        let commit: CommitObject = serde_json::from_slice(&payload)
            .with_context(|| format!("malformed native commit object {}", id))?;
        if commit.schema_version != SCHEMA_VERSION {
            bail!("unsupported native commit schema version");
        }
        Ok(commit)
    }

    pub fn verify_content_ref(&self, content_ref: &str) -> Result<BTreeSet<ObjectId>> {
        let root = object_id_from_content_ref(content_ref)?;
        let mut seen = BTreeSet::new();
        self.verify_reachable(&root, &mut seen)?;
        Ok(seen)
    }

    /// All native objects reachable from the ref-store `HEAD` (NER-138 Phase 7 slice 2):
    /// every commit on HEAD's ancestry (commit → parents), each commit's tree, and every
    /// blob/subtree those trees reach. Returns an empty set when no `HEAD` exists yet (a
    /// git-backend repo, or a native repo before its first base anchoring). Used by gc
    /// reachability so the live history tip and the base anchor that every attempt's
    /// `base_head` points at are never reported as unreachable garbage. A `seen` set guards
    /// against revisiting a commit (diamond/merge ancestry).
    pub fn reachable_from_head(&self) -> Result<BTreeSet<ObjectId>> {
        let mut reachable = BTreeSet::new();
        let Some(head) = NativeRefStore::new(&self.root).read_head()? else {
            return Ok(reachable);
        };
        let mut stack = vec![head];
        while let Some(commit_id) = stack.pop() {
            if !reachable.insert(commit_id.clone()) {
                continue; // already visited
            }
            let commit = self.read_commit(&commit_id)?;
            // Mark the commit's tree and everything it reaches.
            self.verify_reachable(&ObjectId::parse(&commit.tree)?, &mut reachable)?;
            for parent in &commit.parents {
                stack.push(ObjectId::parse(parent)?);
            }
        }
        Ok(reachable)
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
                // Recover each loose object's kind by re-hashing its bytes under every
                // kind and matching the digest; domain separation guarantees at most one
                // match. NER-138 Phase 7 slice 2 adds `Commit` to the scan. Slice 3's
                // object-kind headers replace this multi-hash scan (kill the double/triple
                // hash; see lib.rs object-kind-header work).
                for kind in [ObjectKind::Blob, ObjectKind::Tree, ObjectKind::Commit] {
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

/// The native ref store (NER-138 Phase 7 slice 2): a small, crash-atomic, lock-agnostic
/// holder for the history tip (`HEAD` → a commit id). Slice 2 writes `HEAD` exactly once
/// (the genesis commit); advancing it as commits are recorded at `accept` is slice 3.
///
/// Writes inherit `NativeObjectStore::write_object`'s durability discipline verbatim
/// (temp in `.forge/tmp` + `sync_all` + atomic rename + parent-dir fsync incl.
/// newly-created ancestors; a swallowed dir fsync is the durability hole this avoids).
/// The store NEVER acquires `.forge/forge.lock` — its callers (mutating commands) already
/// hold it (acquire-once-never-nested), so creating the genesis from inside `current_base`
/// cannot deadlock or race. The ref file lives under `.forge/`, so `is_ignored_by_policy`
/// already excludes it from every snapshot/export.
#[derive(Debug, Clone)]
pub struct NativeRefStore {
    root: PathBuf,
}

impl NativeRefStore {
    pub fn new(repo_root: &Path) -> Self {
        Self {
            root: repo_root.to_path_buf(),
        }
    }

    fn head_path(&self) -> PathBuf {
        self.root.join(".forge/refs/HEAD")
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join(".forge/tmp")
    }

    /// The current history tip, or `None` if no tip has been written yet. S1: a read or
    /// parse failure surfaces a path-free error (only the `io::ErrorKind` or the malformed
    /// contents/kind, never the filesystem path).
    pub fn read_head(&self) -> Result<Option<ObjectId>> {
        let raw = match fs::read_to_string(self.head_path()) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(anyhow!("failed to read native HEAD: {}", error.kind())),
        };
        let id = ObjectId::parse(raw.trim())?;
        if id.kind()? != ObjectKind::Commit {
            bail!("native HEAD does not name a commit");
        }
        Ok(Some(id))
    }

    /// Atomically set the history tip. Crash-atomic (temp + fsync + rename + dir fsync) so
    /// a committed `base_head`/`decisions.commit_id` row never references a HEAD that did
    /// not reach disk. S1: the underlying `io::Error`s are path-free by construction.
    pub fn set_head(&self, id: &ObjectId) -> Result<()> {
        let path = self.head_path();
        let parent = path.parent().context("native HEAD path has no parent")?;
        // Record newly-created ancestors so their own dir entries can be made durable
        // below (mirrors write_object): a freshly created dir's entry is not durable
        // until the dir it lives in is fsynced.
        let newly_created = missing_dirs(parent);
        fs::create_dir_all(parent)?;
        fs::create_dir_all(self.tmp_dir())?;
        let mut temp = tempfile::NamedTempFile::new_in(self.tmp_dir())?;
        temp.write_all(id.to_string().as_bytes())?;
        temp.as_file_mut().sync_all()?;
        temp.persist(&path).map_err(|error| error.error)?;
        sync_dir(parent)?;
        for dir in &newly_created {
            if let Some(grandparent) = dir.parent() {
                sync_dir(grandparent)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TreeObject {
    schema_version: u32,
    entries: Vec<TreeEntry>,
}

/// An opaque lowercase 64-hex digest (e.g. an evidence `content_hash`). Constructing one
/// validates the shape, so the commit-build path (slice 3's `accept`) can only assign a
/// real digest — excerpt text is structurally unrepresentable in
/// [`CommitObject::evidence_digest`] (the commit payload is written via `write_object` and
/// never passes through `redact_evidence_excerpt`, so this newtype is the secret-hygiene
/// guard). `#[serde(transparent)]` so it serializes/deserializes as the bare hex string —
/// byte-identical to the prior `Option<String>` field, preserving genesis-hash stability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hex64(String);

impl Hex64 {
    /// Validate and wrap a lowercase 64-hex digest. Errors (path-free) on any other shape,
    /// so a non-digest (e.g. excerpt text) can never reach the commit payload.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            bail!("evidence digest must be exactly 64 lowercase hex characters");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Hex64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A native commit/Change object (NER-138 Phase 7 slice 2; justified commits land in
/// slice 3). Content-addressed and domain-separated via `ObjectId::new(ObjectKind::Commit,
/// ..)`. `pub` (with `pub` fields) so `forge_store` can build justified commits at `accept`
/// and walk them for `log`/checkout/`doctor` (slice 3).
///
/// **Genesis-hash stability (slice 3, critical):** the two slice-3 fields `actor` and
/// `authored_time` carry `#[serde(skip_serializing_if = "Option::is_none")]`, so a genesis
/// commit (all-`None`) serializes byte-identically to slice 2 — its `ObjectId` is unchanged
/// and existing repos' `base_head` does not desync into spurious `STALE_BASE`. Justified
/// commits (slice 3) populate `actor` + `authored_time` in the HASHED bytes so Phase 9
/// signing attests who/when (a later registry bump cannot retroactively bring earlier
/// justified commits under signed/decider-bound provenance).
///
/// `evidence_digest`, when present, is a [`Hex64`] (an opaque lowercase-hex digest such as
/// the ledger's evidence `content_hash`) — never an excerpt or any free text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitObject {
    pub schema_version: u32,
    pub tree: String,
    #[serde(default)]
    pub parents: Vec<String>,
    #[serde(default)]
    pub intent_id: Option<String>,
    #[serde(default)]
    pub proposal_revision_id: Option<String>,
    #[serde(default)]
    pub decision_id: Option<String>,
    #[serde(default)]
    pub evidence_digest: Option<Hex64>,
    /// The decider (`decisions.actor`). `None` for genesis. Hashed for justified commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Wall-clock authored time (ms). `None` for genesis. Hashed for justified commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_time: Option<i64>,
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
    synced_dirs: &mut BTreeSet<PathBuf>,
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
                let parent = full
                    .parent()
                    .ok_or_else(|| anyhow!("restore target {} has no parent", full.display()))?;
                // Record which ancestor directories are newly created so their own
                // entries can be made durable below (mirrors write_object) — a freshly
                // created dir's entry is not durable until the dir it lives in is fsynced.
                let newly_created = missing_dirs(parent);
                fs::create_dir_all(parent)?;
                // Crash-atomic restore (NER-132 U4): write to a temp file in the
                // destination's own parent directory — guaranteeing a
                // same-filesystem rename even when `.forge` is a separate mount —
                // set its mode, fsync it, then atomically rename into place. The
                // `.forge-restore-` prefix lets `doctor` reclaim a temp orphaned by
                // a crash mid-restore (tempfile's Drop does not run on a hard kill).
                let mut temp = tempfile::Builder::new()
                    .prefix(RESTORE_TEMP_PREFIX)
                    .tempfile_in(parent)
                    .with_context(|| format!("create restore temp file in {}", parent.display()))?;
                temp.write_all(&bytes)?;
                set_file_mode(temp.path(), entry.mode)?;
                temp.as_file().sync_all()?;
                temp.persist(&full)
                    .map_err(|error| error.error)
                    .with_context(|| format!("persist restored file {}", full.display()))?;
                // The renamed file's directory entry must reach disk for the
                // restore to be durable; fsync each parent directory once per
                // restore to bound the fsync cost on large worktrees.
                if synced_dirs.insert(parent.to_path_buf()) {
                    sync_dir(parent)?;
                }
                // Make each newly created ancestor directory durable by fsyncing the
                // directory that gained the new entry (mirrors write_object), deduped
                // across the whole restore so each dir is fsynced at most once.
                for dir in &newly_created {
                    if let Some(grandparent) = dir.parent() {
                        if synced_dirs.insert(grandparent.to_path_buf()) {
                            sync_dir(grandparent)?;
                        }
                    }
                }
                target_paths.insert(rel);
                // Crash boundary (NER-132 U6, debug-only): this file is fully
                // renamed (whole, never torn) and its temp consumed; a crash here
                // leaves a partially-restored worktree with no orphaned temp.
                forge_content::maybe_crash("mid_restore");
            }
            TreeEntryKind::Dir => {
                let full = repo_root.join(&rel);
                if full.is_file() || full.is_symlink() {
                    fs::remove_file(&full)
                        .with_context(|| format!("remove file {}", full.display()))?;
                }
                fs::create_dir_all(full)?;
                materialize_tree(store, repo_root, &child, &rel, target_paths, synced_dirs)?;
            }
        }
    }
    Ok(())
}

fn scan_worktree(repo_root: &Path) -> Result<Vec<FileEntry>> {
    let mut files = Vec::new();
    for path in walk_worktree(repo_root)? {
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
    // Defensive: the serial `ignore` Walk yields each path once, so this dedup is a no-op
    // today — but it guarantees a future change (a second walk root, follow_links, or a
    // parallel walker) can never feed `write_tree` two entries with the same name.
    files.dedup_by(|a, b| a.path == b.path);
    Ok(files)
}

/// Enumerate snapshot-candidate worktree paths natively, without the `git` binary
/// (NER-138 Phase 7 slice 1). Uses the `ignore` crate to honor repo-local `.gitignore`
/// (nested, with negation) and `.forgeignore`; the authoritative secret/internal
/// exclusion is left to `is_ignored_by_policy` in the caller (`scan_worktree`).
///
/// Exclusion precedence (highest wins):
///   `is_ignored_by_policy` (`.forge/`, `.git/`, `.forge-restore-*`, secret-risk —
///       always wins, not negatable)
///     > `.forgeignore`  (Forge-specific; a `!`-negation can re-include a `.gitignore` drop)
///     > `.gitignore`    (repo-local, nested, with negation)
///     > built-in defaults
///
/// The walker is intentionally **environment-independent**: it does NOT consult
/// `.git/info/exclude` or the user's global `core.excludesfile`, so a repo's native
/// snapshot set is reproducible across machines (the Phase 7 goal). This is a deliberate,
/// documented divergence from `git ls-files --others --exclude-standard`. Resolves the
/// `PRD.md` `.forgeignore` open question for the native backend.
fn walk_worktree(repo_root: &Path) -> Result<Vec<String>> {
    let mut builder = WalkBuilder::new(repo_root);
    builder
        .hidden(false) // git lists dotfiles (.gitignore, .gitattributes, .github/)
        .ignore(false) // git does not honor the `ignore` crate's own `.ignore` files
        .git_ignore(true) // honor repo-local .gitignore (nested, with negation)
        .git_exclude(false) // env-independent: do not read .git/info/exclude
        .git_global(false) // env-independent: do not read global core.excludesfile
        .parents(false) // the repo root is the ignore boundary (matches git)
        .require_git(false) // honor .gitignore even without a .git directory
        .follow_links(false) // do not traverse into symlinked directories
        .add_custom_ignore_filename(".forgeignore") // higher precedence than .gitignore
        .sort_by_file_name(|a, b| a.cmp(b));

    // Prune descent into `is_ignored_by_policy` directories (`.git`, `.forge`) at the walk
    // layer so we never recurse through e.g. thousands of `.git` internals just to discard
    // them. This *reuses* the shared predicate (it is not a fork): the post-walk
    // `is_ignored_by_policy` filter in `scan_worktree` remains the authoritative backstop.
    let prune_root = repo_root.to_path_buf();
    builder.filter_entry(move |entry| match rel_path(&prune_root, entry.path()) {
        Some(rel) => !is_ignored_by_policy(&rel),
        None => true, // the walk root itself has no relative path; always descend it
    });

    let mut paths = Vec::new();
    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => match map_walk_error(&error) {
                Some(mapped) => return Err(mapped),
                None => continue,
            },
        };
        // Skip directories; yield regular files AND symlinks so the downstream
        // `fs::metadata`/`is_file` gate (which follows links) decides symlink-to-file
        // capture — preserving today's behavior (Phase 7 does not add symlink content).
        match entry.file_type() {
            Some(file_type) if file_type.is_dir() => continue,
            None => continue, // no file type (e.g. the stdin sentinel) — never a worktree file
            _ => {}
        }
        if let Some(rel) = rel_path(repo_root, entry.path()) {
            paths.push(rel);
        }
    }
    Ok(paths)
}

/// Map a walk error to either `None` (skip — a benign mid-walk disappearance) or a
/// **path-free** `anyhow` error (security invariant S1: no filesystem path may reach the
/// untyped envelope `message`, which would bypass typed-error secret-path redaction).
/// `ignore::Error`'s own `Display` embeds the offending path, so we never forward it —
/// only the path-free `io::ErrorKind` is surfaced. A `NotFound` (a file that vanished
/// between enumeration and read, realistic under a concurrent agent fleet) is benign and
/// skipped, mirroring the `fs::metadata` skip in `scan_worktree`.
fn map_walk_error(error: &ignore::Error) -> Option<anyhow::Error> {
    match error.io_error() {
        Some(io) if io.kind() == std::io::ErrorKind::NotFound => None,
        Some(io) => Some(anyhow!("failed to walk worktree: {}", io.kind())),
        None => Some(anyhow!("failed to walk worktree")),
    }
}

/// Repo-relative, forward-slash path for a walked entry, or `None` for the walk root
/// itself. Joins only `Normal` components so a filename containing a backslash on Unix is
/// preserved and Windows separators normalize to `/` — matching `is_secret_risk_path`'s
/// `rsplit('/')` and the tree builder's path handling.
///
/// Non-UTF-8 bytes in a filename are best-effort: `to_string_lossy` substitutes U+FFFD, so
/// such a path may not round-trip and the file is dropped at the downstream `fs::metadata`
/// gate. This is no worse than the prior git `ls-files` `.lines()` parsing (which C-quoted
/// such names); faithful non-UTF-8 capture is out of slice-1 scope. The secret backstop is
/// unaffected — `is_secret_risk_path` lowercases the (lossy) filename before matching.
fn rel_path(repo_root: &Path, full: &Path) -> Option<String> {
    let rel = full.strip_prefix(repo_root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(os) => Some(os.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

fn materialized_paths(repo_root: &Path) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    for file in scan_worktree(repo_root)? {
        paths.insert(file.path);
    }
    Ok(paths)
}

/// Name-level diff of the base commit's tree against the freshly-built worktree tree
/// (NER-138 Phase 7 slice 2), replacing the prior `git diff --name-only HEAD` +
/// `git ls-files --others` shell-out. A path is reported when its `(blob id, mode)` differs
/// between the two trees: added (worktree-only), removed (base-only), or modified (same
/// path, different blob **or** a changed executable bit). Mode is part of the key so a
/// `chmod +x` with unchanged content still surfaces — matching `git diff --name-only HEAD`,
/// which lists a mode-only change. **Name granularity only — hunk-level diff is Phase 8.**
/// This is reproducibility-over-parity: it does not chase exact `git diff` output.
///
/// The base tree is `ensure_head`'s commit tree (the genesis, established at `start`); by
/// `save` time it is the start-time anchor, so the diff reflects edits since `start`. If
/// the base tree equals the worktree tree (nothing changed, or a just-created genesis),
/// the result is empty. Policy-excluded paths cannot appear in either tree (the walker
/// filters them), but the final `retain` mirrors the git backend's backstop.
fn changed_paths(
    store: &NativeObjectStore,
    repo_root: &Path,
    worktree_root: &ObjectId,
) -> Result<Vec<String>> {
    let head = ensure_head(repo_root)?;
    let base_tree = ObjectId::parse(&store.read_commit(&head)?.tree)?;
    if &base_tree == worktree_root {
        return Ok(Vec::new());
    }
    let base = flatten_tree(store, &base_tree)?;
    let worktree = flatten_tree(store, worktree_root)?;
    let mut changed = BTreeSet::new();
    for (path, fingerprint) in &worktree {
        if base.get(path) != Some(fingerprint) {
            changed.insert(path.clone()); // added, or content/mode modified
        }
    }
    for path in base.keys() {
        if !worktree.contains_key(path) {
            changed.insert(path.clone()); // removed
        }
    }
    Ok(changed
        .into_iter()
        .filter(|path| !is_ignored_by_policy(path))
        .collect())
}

/// A file leaf's change-detection fingerprint: its blob object id AND its mode, so a
/// mode-only change (e.g. `chmod +x`) is detected even though the blob content (hence the
/// blob id) is unchanged.
type FileFingerprint = (String, u32);

/// Flatten a native tree into a map of repo-relative file path → `(blob id, mode)`,
/// recursing into directory entries. Used by `changed_paths` for the name-level
/// base-vs-worktree diff.
fn flatten_tree(
    store: &NativeObjectStore,
    tree_id: &ObjectId,
) -> Result<BTreeMap<String, FileFingerprint>> {
    let mut out = BTreeMap::new();
    flatten_tree_into(store, tree_id, "", &mut out)?;
    Ok(out)
}

fn flatten_tree_into(
    store: &NativeObjectStore,
    tree_id: &ObjectId,
    prefix: &str,
    out: &mut BTreeMap<String, FileFingerprint>,
) -> Result<()> {
    let payload = store.read_object(tree_id)?;
    let tree: TreeObject = serde_json::from_slice(&payload)
        .with_context(|| format!("malformed native tree object {}", tree_id))?;
    if tree.schema_version != SCHEMA_VERSION {
        bail!("unsupported native tree schema version");
    }
    for entry in tree.entries {
        validate_tree_entry(&entry)?;
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        match entry.kind {
            TreeEntryKind::File => {
                out.insert(path, (entry.object, entry.mode));
            }
            TreeEntryKind::Dir => {
                let child = ObjectId::parse(&entry.object)?;
                // Mirror verify_reachable/materialize_tree: a Dir entry must point at a
                // Tree, so a corrupt entry surfaces a typed "wrong object type" error
                // rather than a downstream serde failure.
                ensure_child_kind(&entry, &child)?;
                flatten_tree_into(store, &child, &path, out)?;
            }
        }
    }
    Ok(())
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
        let commit = ObjectId::new(ObjectKind::Commit, b"same");
        assert_eq!(blob, ObjectId::new(ObjectKind::Blob, b"same"));
        // The same payload hashes to three distinct ids — the domain set is `commit`-aware
        // (NER-138 Phase 7 slice 2).
        assert_ne!(blob, tree);
        assert_ne!(blob, commit);
        assert_ne!(tree, commit);
        assert!(ObjectId::parse(&blob.to_string()).is_ok());
        assert!(ObjectId::parse(&commit.to_string()).is_ok());
        assert_eq!(commit.kind().unwrap(), ObjectKind::Commit);
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
    fn wal_sidecars_are_excluded_by_policy() {
        // WAL (enabled in forge-store::open_connection) makes `forge.db` travel
        // with `-wal`/`-shm` sidecars holding committed-but-uncheckpointed data,
        // including evidence excerpts. They must never leak into a snapshot/export.
        assert!(is_ignored_by_policy(".forge/forge.db"));
        assert!(is_ignored_by_policy(".forge/forge.db-wal"));
        assert!(is_ignored_by_policy(".forge/forge.db-shm"));
        // The NER-132 advisory lock file is covered by the same blanket `.forge/`
        // prefix; pin it so a future refactor of the exclusion rule cannot drop it.
        assert!(is_ignored_by_policy(".forge/forge.lock"));
        assert!(is_ignored_by_policy(".forge"));
        // Restore temps live in worktree dirs (NER-132 U4), not under .forge, so they
        // need their own exclusion — an orphan from a crash-interrupted restore must
        // never land in a snapshot/export.
        assert!(is_ignored_by_policy(".forge-restore-abc123"));
        assert!(is_ignored_by_policy("src/nested/.forge-restore-xyz"));
        // Symmetric secret/internal-path assertions: both backends route to the shared
        // `forge_content::is_ignored_by_policy`, so the exclusion set cannot drift
        // (NER-133 U6).
        assert!(is_ignored_by_policy(".env"));
        assert!(is_ignored_by_policy("certs/server.pem"));
        assert!(is_ignored_by_policy(".git"));
        assert!(is_ignored_by_policy(".git/config"));
        // A normal worktree file is still snapshot-eligible.
        assert!(!is_ignored_by_policy("README.md"));
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

    #[test]
    fn restore_roundtrips_atomically_and_leaves_no_temp() {
        // Snapshot a source worktree, then materialize it into a fresh destination
        // and over an existing (stale) file. The crash-atomic file arm uses
        // temp+rename, so on a clean restore: content round-trips, the stale file
        // is fully replaced, and no `.forge-restore-*` temp survives.
        let src = tempfile::tempdir().unwrap();
        // The native walker (NER-138 Phase 7 slice 1) enumerates worktree paths via the
        // `ignore` crate, so a `.git` directory is no longer required to snapshot; the
        // untracked files below are picked up directly from the filesystem.
        fs::create_dir_all(src.path().join("dir")).unwrap();
        fs::write(src.path().join("top.txt"), b"top-new").unwrap();
        fs::write(src.path().join("dir/nested.txt"), b"nested-new").unwrap();
        let backend = NativeContentBackend;
        let content = backend.snapshot_worktree(src.path()).unwrap();

        let dest = tempfile::tempdir().unwrap();
        // A stale file at a target path must be replaced wholesale (never torn).
        fs::write(dest.path().join("top.txt"), b"stale-old-and-longer").unwrap();
        materialize_content_ref(src.path(), dest.path(), &content.content_ref).unwrap();

        assert_eq!(fs::read(dest.path().join("top.txt")).unwrap(), b"top-new");
        assert_eq!(
            fs::read(dest.path().join("dir/nested.txt")).unwrap(),
            b"nested-new"
        );

        // No restore temp may linger in the destination tree after a clean restore.
        let mut leftover = Vec::new();
        for dir in [dest.path().to_path_buf(), dest.path().join("dir")] {
            for entry in fs::read_dir(&dir).unwrap() {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                if name.starts_with(RESTORE_TEMP_PREFIX) {
                    leftover.push(name);
                }
            }
        }
        assert!(
            leftover.is_empty(),
            "restore left orphaned temp files: {leftover:?}"
        );
    }

    // --- NER-138 Phase 7 slice 1: native walker differential harness ---
    //
    // These tests prove the native `ignore`-crate walker's snapshot set equals the prior
    // git-based set across a parity corpus (incl. secret-risk exclusion), and assert each
    // index-vs-filesystem divergence class explicitly rather than masking it. The harness
    // is the safety net that justifies removing the `git ls-files` shell-out from the
    // native snapshot path (R5). Most are git-backed; the `.forgeignore` and special-byte
    // cases are native-only (git knows nothing of `.forgeignore`, and the special-byte
    // case is exactly the C-quote leak the native walker structurally cures).

    /// Run a git command in `repo`, asserting success. Test setup only.
    #[cfg(test)]
    fn run_git(repo: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo)
                .output()
                .unwrap()
                .status
                .success(),
            "git {args:?} failed"
        );
    }

    /// Run a git command and capture its stdout. Lives ONLY in the test module (already
    /// `#[cfg(test)]`): the `git_based_scan` differential harness shells git to reproduce
    /// the prior git-based set, but NER-138 Phase 7 slice 2 removed the last production
    /// caller — native snapshot/base/changed_paths are git-binary-free (pinned by
    /// `native_production_paths_shell_no_git`).
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

    #[cfg(test)]
    fn init_git_repo(repo: &Path) {
        run_git(repo, &["init"]);
        run_git(repo, &["config", "user.email", "t@example.com"]);
        run_git(repo, &["config", "user.name", "forge-test"]);
    }

    /// The pre-slice-1 git-based candidate enumeration, retained ONLY as the differential
    /// harness's reference set. Mirrors the removed `snapshot_candidate_paths` (the union of
    /// `git ls-files` and `git ls-files --others --exclude-standard`) followed by the same
    /// downstream filters `scan_worktree` applies (`is_ignored_by_policy` + `is_file`).
    /// Production no longer shells git for snapshotting (R1); this lives in the test module
    /// so the harness can prove native-walk set == prior git-based set.
    #[cfg(test)]
    fn git_based_scan(repo: &Path) -> Vec<String> {
        let mut candidates = BTreeSet::new();
        for args in [
            ["ls-files"].as_slice(),
            ["ls-files", "--others", "--exclude-standard"].as_slice(),
        ] {
            candidates.extend(git(repo, args).unwrap().lines().map(str::to_string));
        }
        let mut files: Vec<String> = candidates
            .into_iter()
            .filter(|path| !is_ignored_by_policy(path))
            .filter(|path| matches!(fs::metadata(repo.join(path)), Ok(meta) if meta.is_file()))
            .collect();
        files.sort();
        files
    }

    /// The native walker's final scan set (post policy backstop + `is_file` gate), sorted.
    #[cfg(test)]
    fn native_scan(repo: &Path) -> Vec<String> {
        let mut files: Vec<String> = scan_worktree(repo)
            .unwrap()
            .into_iter()
            .map(|file| file.path)
            .collect();
        files.sort();
        files
    }

    #[test]
    fn native_walk_matches_git_based_set_on_parity_corpus() {
        // Parity corpus: only paths whose membership is identical between git's index-based
        // enumeration and a filesystem walk (no index-only paths — those are asserted as
        // divergences below). Includes tracked, untracked, gitignored (+ negation),
        // secret-risk, internal, and restore-temp-at-depth cases.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_git_repo(repo);

        fs::create_dir_all(repo.join("src/nested")).unwrap();
        fs::write(repo.join("README.md"), b"readme").unwrap();
        fs::write(repo.join("src/main.rs"), b"fn main() {}").unwrap();
        fs::write(repo.join("src/nested/deep.txt"), b"deep").unwrap();
        fs::write(repo.join(".gitattributes"), b"* text=auto\n").unwrap();
        // *.log ignored, but keep.log re-included by a negation (parent not excluded).
        fs::write(repo.join(".gitignore"), b"*.log\n!keep.log\n").unwrap();
        run_git(repo, &["add", "-A"]);
        run_git(repo, &["commit", "-m", "init"]);

        // Untracked, non-ignored.
        fs::write(repo.join("untracked.txt"), b"u").unwrap();
        // Untracked + gitignored (excluded) and the negated re-include (kept).
        fs::write(repo.join("debug.log"), b"d").unwrap();
        fs::write(repo.join("keep.log"), b"k").unwrap();
        // Secret-risk + internal: excluded by is_ignored_by_policy in BOTH pipelines.
        fs::write(repo.join(".env"), b"SECRET=x").unwrap();
        fs::create_dir_all(repo.join("certs")).unwrap();
        fs::write(repo.join("certs/server.pem"), b"key").unwrap();
        fs::create_dir_all(repo.join(".forge")).unwrap();
        fs::write(repo.join(".forge/forge.db"), b"db").unwrap();
        // Orphaned restore temp at depth (policy-excluded at any depth, not via .gitignore).
        fs::write(repo.join("src/nested/.forge-restore-abc"), b"tmp").unwrap();

        assert_eq!(
            native_scan(repo),
            git_based_scan(repo),
            "native walk set must equal the prior git-based set on the parity corpus"
        );

        // Pin the content the equality locks in.
        let native = native_scan(repo);
        for kept in [
            "README.md",
            "src/main.rs",
            "src/nested/deep.txt",
            ".gitattributes",
            ".gitignore",
            "untracked.txt",
            "keep.log",
        ] {
            assert!(
                native.contains(&kept.to_string()),
                "expected {kept} in {native:?}"
            );
        }
        for dropped in [".env", "certs/server.pem", "debug.log", ".forge/forge.db"] {
            assert!(
                !native.contains(&dropped.to_string()),
                "unexpected {dropped} in {native:?}"
            );
        }
        assert!(
            !native.iter().any(|p| p.contains(".forge-restore-")),
            "restore temp leaked into snapshot set: {native:?}"
        );
    }

    #[test]
    fn walk_does_not_recurse_into_git_or_forge() {
        // filter_entry must prune `.git`/`.forge` descent so the walk never yields their
        // internals (a real `.git` holds thousands of files; statting them every save is
        // pure waste, and they must never reach the snapshot anyway).
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_git_repo(repo);
        fs::write(repo.join("README.md"), b"x").unwrap();
        fs::create_dir_all(repo.join(".forge/objects/sha256/ab")).unwrap();
        fs::write(repo.join(".forge/forge.db"), b"db").unwrap();
        fs::write(repo.join(".forge/objects/sha256/ab/deadbeef"), b"obj").unwrap();

        let walked = walk_worktree(repo).unwrap();
        assert!(
            walked.iter().all(|p| {
                p != ".git" && !p.starts_with(".git/") && p != ".forge" && !p.starts_with(".forge/")
            }),
            "walk recursed into .git/ or .forge/: {walked:?}"
        );
        assert!(walked.contains(&"README.md".to_string()));
    }

    #[test]
    fn force_added_gitignored_file_is_index_only_divergence() {
        // Divergence class 1: git's index lists a force-added (`add -f`) file even though
        // .gitignore matches it; the native filesystem walk drops it (no index concept).
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_git_repo(repo);
        fs::write(repo.join(".gitignore"), b"forced.bin\n").unwrap();
        fs::write(repo.join("forced.bin"), b"x").unwrap();
        run_git(repo, &["add", "-f", "forced.bin"]);
        run_git(repo, &["add", ".gitignore"]);
        run_git(repo, &["commit", "-m", "force"]);

        assert!(
            git_based_scan(repo).contains(&"forced.bin".to_string()),
            "git index lists the force-added file"
        );
        assert!(
            !native_scan(repo).contains(&"forced.bin".to_string()),
            "native filesystem walk drops the gitignored file (no index concept)"
        );
        // The force-added path is the ONLY difference: the two scans agree on everything else.
        let strip = |set: Vec<String>| -> Vec<String> {
            set.into_iter().filter(|p| p != "forced.bin").collect()
        };
        assert_eq!(strip(native_scan(repo)), strip(git_based_scan(repo)));
    }

    #[test]
    fn tracked_then_later_ignored_file_is_index_only_divergence() {
        // Divergence class 2: a normally-committed file later matched by an added
        // .gitignore rule — git's ls-files still lists it (tracked); native walk drops it.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_git_repo(repo);
        fs::write(repo.join("data.gen"), b"x").unwrap();
        run_git(repo, &["add", "data.gen"]);
        run_git(repo, &["commit", "-m", "track"]);
        fs::write(repo.join(".gitignore"), b"*.gen\n").unwrap();

        assert!(git_based_scan(repo).contains(&"data.gen".to_string()));
        assert!(!native_scan(repo).contains(&"data.gen".to_string()));
        // The now-ignored tracked path is the ONLY difference between the two scans.
        let strip = |set: Vec<String>| -> Vec<String> {
            set.into_iter().filter(|p| p != "data.gen").collect()
        };
        assert_eq!(strip(native_scan(repo)), strip(git_based_scan(repo)));
    }

    #[test]
    fn tracked_then_deleted_from_disk_converges_after_metadata_gate() {
        // Divergence class 3: git's raw `ls-files` lists a tracked path even after it is
        // deleted from the worktree (still in the index); the native walk cannot see a
        // nonexistent file. Both *scan* pipelines converge because the `fs::metadata`
        // `is_file` gate drops the now-missing path from the git reference too.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_git_repo(repo);
        fs::write(repo.join("gone.txt"), b"x").unwrap();
        fs::write(repo.join("stay.txt"), b"y").unwrap();
        run_git(repo, &["add", "-A"]);
        run_git(repo, &["commit", "-m", "add"]);
        fs::remove_file(repo.join("gone.txt")).unwrap();

        assert!(
            git(repo, &["ls-files"])
                .unwrap()
                .lines()
                .any(|l| l == "gone.txt"),
            "git index still lists the deleted path"
        );
        assert!(!native_scan(repo).contains(&"gone.txt".to_string()));
        // After the metadata gate the two scans agree (both keep stay.txt, drop gone.txt).
        assert_eq!(native_scan(repo), git_based_scan(repo));
    }

    #[test]
    fn walk_is_environment_independent_of_info_exclude() {
        // Divergence class 4 (load-bearing for Phase 7): the native walker is intentionally
        // MORE inclusive than `git ls-files --others --exclude-standard` — it ignores
        // `.git/info/exclude` and the user's global core.excludesfile so a repo's snapshot
        // set is reproducible across machines. This pins that git_exclude(false)/git_global(false)
        // are real: a path excluded ONLY via `.git/info/exclude` is dropped by git but KEPT by
        // the native walk. If a crate upgrade or edit flipped those toggles, this catches it.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_git_repo(repo);
        fs::write(repo.join("keep-me.txt"), b"x").unwrap();
        fs::write(repo.join(".git/info/exclude"), b"keep-me.txt\n").unwrap();

        let git_others = git(repo, &["ls-files", "--others", "--exclude-standard"]).unwrap();
        assert!(
            !git_others.lines().any(|l| l == "keep-me.txt"),
            "git --exclude-standard drops a .git/info/exclude path"
        );
        assert!(
            native_scan(repo).contains(&"keep-me.txt".to_string()),
            "native walk must ignore .git/info/exclude (reproducibility): {:?}",
            native_scan(repo)
        );
    }

    // Divergence classes intentionally NOT asserted, with rationale (plan R5/U3):
    //   • Case-folded `.gitignore` on a case-insensitive filesystem (macOS): a rule like
    //     `SECRET.txt` drops `secret.txt` under git's core.ignorecase, but the native walk
    //     matches case-sensitively, so membership can differ. Asserting it would be
    //     platform-dependent (green on Linux CI, divergent on macOS), so it is documented as
    //     an accepted divergence rather than pinned by a flaky test. Secret hygiene is
    //     unaffected: `is_secret_risk_path` lowercases the filename, so a case-variant secret
    //     name is still excluded by the policy backstop.
    //   • Submodule gitlinks: `git ls-files` lists a gitlink path; a filesystem walk
    //     descends/skips it differently. A real submodule fixture in a unit test is
    //     impractical (a second repo + offline `submodule add`), so this class is scoped out
    //     by comment per the plan's allowance.

    #[test]
    fn nested_subdir_gitignore_is_honored() {
        // R2's headline claim is "nested, with negation". A .gitignore in a SUBDIRECTORY
        // (not just the root) must scope to that directory — the deceptively-hard part the
        // `ignore` crate exists to handle. Pin it so a crate-version bump can't regress it.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_git_repo(repo);
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/.gitignore"), b"local.tmp\n!keep.tmp\n").unwrap();
        fs::write(repo.join("src/local.tmp"), b"x").unwrap();
        fs::write(repo.join("src/keep.tmp"), b"x").unwrap();
        fs::write(repo.join("src/main.rs"), b"x").unwrap();
        fs::write(repo.join("local.tmp"), b"x").unwrap(); // root: NOT covered by src/.gitignore

        let native = native_scan(repo);
        assert!(
            !native.contains(&"src/local.tmp".to_string()),
            "nested .gitignore must exclude src/local.tmp: {native:?}"
        );
        assert!(
            native.contains(&"src/keep.tmp".to_string()),
            "nested negation must re-include src/keep.tmp: {native:?}"
        );
        assert!(native.contains(&"src/main.rs".to_string()));
        assert!(
            native.contains(&"local.tmp".to_string()),
            "root local.tmp is outside src/.gitignore's scope: {native:?}"
        );
        // The nested rule is repo-local, so native and git agree.
        assert_eq!(native, git_based_scan(repo));
    }

    #[test]
    fn empty_worktree_snapshots_and_roundtrips() {
        // A fresh dir with no files: walk_worktree returns empty, snapshot_worktree builds an
        // empty root tree, and the snapshot materializes. No git binary needed for the walk.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        assert!(walk_worktree(repo).unwrap().is_empty());
        let content = NativeContentBackend
            .snapshot_worktree(repo)
            .expect("empty worktree snapshots");
        assert!(content.content_ref.starts_with(FORGE_TREE_PREFIX));
        let dest = tempfile::tempdir().unwrap();
        materialize_content_ref(repo, dest.path(), &content.content_ref)
            .expect("empty snapshot materializes");
    }

    #[cfg(unix)]
    #[test]
    fn tracked_symlink_to_file_is_captured() {
        // Regression guard for the file_type trap: a symlink's own file_type is is_symlink,
        // so a walk-layer is_file() filter would drop a tracked symlink-to-file that today's
        // fs::metadata (which follows the link) captures. The walker must yield symlinks.
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_git_repo(repo);
        fs::write(repo.join("target.txt"), b"content").unwrap();
        symlink("target.txt", repo.join("link.txt")).unwrap();
        run_git(repo, &["add", "-A"]);
        run_git(repo, &["commit", "-m", "link"]);

        let native = native_scan(repo);
        assert!(
            native.contains(&"link.txt".to_string()),
            "tracked symlink-to-file must be captured: {native:?}"
        );
        assert!(native.contains(&"target.txt".to_string()));
        assert_eq!(native, git_based_scan(repo));
    }

    #[test]
    fn native_walk_excludes_secret_with_special_byte_in_name() {
        // The structural cure for the C-quote class (Phase 6 §5): the native walker passes
        // the REAL filename to is_secret_risk_path, never a git-C-quoted string. `.env.café`
        // matches starts_with(".env.") on its real name and is excluded; git ls-tree/ls-files
        // would C-quote it (`".env.caf\303\251"`) and the leading quote would defeat the
        // prefix match — the leak this design avoids. No git repo needed (require_git(false)).
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join(".env.café"), b"SECRET=1").unwrap();
        fs::write(repo.join("plain.txt"), b"x").unwrap();
        let native = native_scan(repo);
        assert!(
            !native.iter().any(|p| p.contains(".env.caf")),
            "special-byte secret name must be excluded: {native:?}"
        );
        assert!(native.contains(&"plain.txt".to_string()));
    }

    #[test]
    fn walk_error_is_path_free_and_skips_not_found() {
        // S1: a walk error must never leak a filesystem path (which would bypass typed-error
        // redaction). `ignore::Error`'s own Display embeds the path, so map_walk_error emits
        // only the path-free io::ErrorKind. A NotFound is benign (mid-walk delete) → skipped.
        use std::io::{Error as IoError, ErrorKind};
        let secret = PathBuf::from("/tmp/.env.supersecret-leak");

        let perm = ignore::Error::WithPath {
            path: secret.clone(),
            err: Box::new(ignore::Error::Io(IoError::new(
                ErrorKind::PermissionDenied,
                secret.display().to_string(),
            ))),
        };
        let mapped = map_walk_error(&perm).expect("non-NotFound walk errors surface");
        let top = mapped.to_string();
        let chain = format!("{mapped:#}");
        assert!(
            !top.contains(".env.supersecret") && !chain.contains(".env.supersecret"),
            "S1: walk error leaked a path: top={top:?} chain={chain:?}"
        );
        assert!(top.contains("permission denied"));

        let gone = ignore::Error::WithPath {
            path: secret,
            err: Box::new(ignore::Error::Io(IoError::from(ErrorKind::NotFound))),
        };
        assert!(
            map_walk_error(&gone).is_none(),
            "NotFound is benign and skipped, not surfaced as an error"
        );
    }

    // --- NER-138 Phase 7 slice 1: .forgeignore precedence (native-only) ---

    #[test]
    fn forgeignore_excludes_paths_gitignore_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join(".forgeignore"), b"*.tmp\n").unwrap();
        fs::write(repo.join("scratch.tmp"), b"x").unwrap();
        fs::write(repo.join("keep.txt"), b"x").unwrap();
        let walked = walk_worktree(repo).unwrap();
        assert!(
            !walked.contains(&"scratch.tmp".to_string()),
            ".forgeignore *.tmp must exclude scratch.tmp: {walked:?}"
        );
        assert!(walked.contains(&"keep.txt".to_string()));
    }

    #[test]
    fn forgeignore_negation_reincludes_gitignored_path() {
        // .forgeignore has higher precedence than .gitignore (add_custom_ignore_filename),
        // so a ! negation there re-includes a path .gitignore excluded.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join(".gitignore"), b"*.log\n").unwrap();
        fs::write(repo.join(".forgeignore"), b"!keep.log\n").unwrap();
        fs::write(repo.join("debug.log"), b"x").unwrap();
        fs::write(repo.join("keep.log"), b"x").unwrap();
        let walked = walk_worktree(repo).unwrap();
        assert!(
            !walked.contains(&"debug.log".to_string()),
            ".gitignore still excludes debug.log: {walked:?}"
        );
        assert!(
            walked.contains(&"keep.log".to_string()),
            ".forgeignore ! must re-include keep.log (higher precedence): {walked:?}"
        );
    }

    #[test]
    fn forgeignore_cannot_reinclude_policy_excluded_secret() {
        // The is_ignored_by_policy backstop runs AFTER the ignore engine and is not
        // negatable: even an explicit !.env in .forgeignore cannot re-include a secret.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join(".forgeignore"), b"!.env\n").unwrap();
        fs::write(repo.join(".env"), b"SECRET=1").unwrap();
        fs::write(repo.join("ok.txt"), b"x").unwrap();
        let native = native_scan(repo);
        assert!(
            !native.iter().any(|p| p == ".env"),
            ".env must stay excluded by the always-wins policy backstop: {native:?}"
        );
        assert!(native.contains(&"ok.txt".to_string()));
    }

    // --- NER-138 Phase 7 slice 2: native commit objects, ref store, base anchoring ---

    #[test]
    fn commit_object_roundtrips_and_is_recovered_by_all_object_ids() {
        let temp = tempfile::tempdir().unwrap();
        let store = NativeObjectStore::new(temp.path());
        // Genesis shape: empty parents, all-None justification (what slice 2 writes).
        let genesis = CommitObject {
            schema_version: SCHEMA_VERSION,
            tree: format!("f1:tree:sha256:{}", "a".repeat(64)),
            parents: Vec::new(),
            intent_id: None,
            proposal_revision_id: None,
            decision_id: None,
            evidence_digest: None,
            actor: None,
            authored_time: None,
        };
        let gid = store.write_commit(&genesis).unwrap();
        assert!(gid.to_string().starts_with("f1:commit:sha256:"));
        assert_eq!(store.read_commit(&gid).unwrap(), genesis);
        // Fully-populated (justified) shape — exercises every field including the slice-3
        // actor + authored_time in the hashed bytes. evidence_digest is a Hex64, so excerpt
        // text is structurally unrepresentable.
        let justified = CommitObject {
            schema_version: SCHEMA_VERSION,
            tree: format!("f1:tree:sha256:{}", "b".repeat(64)),
            parents: vec![gid.to_string()],
            intent_id: Some("intent_x".to_string()),
            proposal_revision_id: Some("revision_x".to_string()),
            decision_id: Some("decision_x".to_string()),
            evidence_digest: Some(Hex64::new("c".repeat(64)).unwrap()),
            actor: Some("agent:tester".to_string()),
            authored_time: Some(1_700_000_000_000),
        };
        let jid = store.write_commit(&justified).unwrap();
        assert_eq!(store.read_commit(&jid).unwrap(), justified);
        assert!(
            justified
                .evidence_digest
                .as_ref()
                .unwrap()
                .as_str()
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "evidence_digest is an opaque hex digest, never excerpt text"
        );
        // The triple-kind all_object_ids scan recovers both commit ids.
        let ids = store.all_object_ids().unwrap();
        assert!(ids.contains(&gid) && ids.contains(&jid));
    }

    /// GENESIS-HASH STABILITY (slice 3, critical): adding `actor`/`authored_time`
    /// (skip_serializing_if) and retyping `evidence_digest` to `Hex64` must NOT change the
    /// bytes a genesis commit serializes to — otherwise every existing native repo's
    /// `base_head` desyncs into spurious `STALE_BASE`. The expected JSON is a hard-coded
    /// literal of what slice 2 wrote (NOT recomputed from the struct), per the adversarial
    /// doc-review finding. Because the `ObjectId` is `hash(preimage(these bytes))`, equal
    /// bytes ⇒ equal id — so byte-equality is the genesis-stability proof.
    #[test]
    fn genesis_commit_serialization_is_byte_identical_to_slice_2() {
        let tree = format!("f1:tree:sha256:{}", "0".repeat(64));
        // EXACTLY what a slice-2 genesis serialized to: 7 fields, the 4 justification
        // Options as null, no actor/authored_time keys.
        let expected = format!(
            r#"{{"schema_version":1,"tree":"{tree}","parents":[],"intent_id":null,"proposal_revision_id":null,"decision_id":null,"evidence_digest":null}}"#
        );
        let genesis = CommitObject {
            schema_version: SCHEMA_VERSION,
            tree: tree.clone(),
            parents: Vec::new(),
            intent_id: None,
            proposal_revision_id: None,
            decision_id: None,
            evidence_digest: None,
            actor: None,
            authored_time: None,
        };
        let serialized = serde_json::to_string(&genesis).unwrap();
        assert_eq!(
            serialized, expected,
            "genesis serialization drifted — existing repos' base_head would desync"
        );
        // And the id derived from those exact bytes is stable (illustrates the implication).
        let id = ObjectId::new(ObjectKind::Commit, serialized.as_bytes());
        assert!(id.to_string().starts_with("f1:commit:sha256:"));
    }

    /// The slice-3 `actor`/`authored_time` are in the HASHED bytes (Phase 9 signs who/when):
    /// two justified commits identical except `actor` MUST have distinct ids, and a
    /// justified commit MUST differ from the genesis-shaped commit over the same tree.
    #[test]
    fn justified_commit_fields_are_hashed() {
        let temp = tempfile::tempdir().unwrap();
        let store = NativeObjectStore::new(temp.path());
        let base = CommitObject {
            schema_version: SCHEMA_VERSION,
            tree: format!("f1:tree:sha256:{}", "a".repeat(64)),
            parents: vec![format!("f1:commit:sha256:{}", "b".repeat(64))],
            intent_id: Some("intent_x".to_string()),
            proposal_revision_id: Some("revision_x".to_string()),
            decision_id: Some("decision_x".to_string()),
            evidence_digest: Some(Hex64::new("c".repeat(64)).unwrap()),
            actor: Some("agent:alice".to_string()),
            authored_time: Some(1_700_000_000_000),
        };
        let mut other_actor = base.clone();
        other_actor.actor = Some("agent:bob".to_string());
        let mut other_time = base.clone();
        other_time.authored_time = Some(1_700_000_000_001);
        let id_base = store.write_commit(&base).unwrap();
        let id_actor = store.write_commit(&other_actor).unwrap();
        let id_time = store.write_commit(&other_time).unwrap();
        assert_ne!(id_base, id_actor, "actor must be in the hashed bytes");
        assert_ne!(
            id_base, id_time,
            "authored_time must be in the hashed bytes"
        );
    }

    /// `Hex64` rejects anything that is not exactly 64 lowercase-hex chars, so excerpt text
    /// can never reach `CommitObject::evidence_digest` (the secret-hygiene guard).
    #[test]
    fn hex64_rejects_non_digest() {
        assert!(Hex64::new("c".repeat(64)).is_ok());
        assert!(Hex64::new("not a real digest, this is excerpt text").is_err());
        assert!(Hex64::new("C".repeat(64)).is_err(), "uppercase rejected");
        assert!(Hex64::new("c".repeat(63)).is_err(), "wrong length rejected");
        // Error is path-free (S1): no separators leak from the validation message.
        let err = Hex64::new("zz").unwrap_err().to_string();
        assert!(!err.contains('/') && !err.contains('\\'));
    }

    #[test]
    fn read_commit_rejects_wrong_kind_and_bad_schema() {
        let id = ObjectId::parse(&format!("f1:commit:sha256:{}", "d".repeat(64))).unwrap();
        assert_eq!(id.kind().unwrap(), ObjectKind::Commit);
        let temp = tempfile::tempdir().unwrap();
        let store = NativeObjectStore::new(temp.path());
        // Wrong kind: a blob id is not a commit.
        let blob = store
            .write_object(ObjectKind::Blob, b"not a commit")
            .unwrap();
        assert!(store
            .read_commit(&blob)
            .unwrap_err()
            .to_string()
            .contains("not a commit"));
        // Bad schema: a commit whose schema_version is newer than this binary supports must
        // be refused, not silently ingested (forward-compat guard).
        let future = CommitObject {
            schema_version: SCHEMA_VERSION + 1,
            tree: format!("f1:tree:sha256:{}", "e".repeat(64)),
            parents: Vec::new(),
            intent_id: None,
            proposal_revision_id: None,
            decision_id: None,
            evidence_digest: None,
            actor: None,
            authored_time: None,
        };
        let payload = serde_json::to_vec(&future).unwrap();
        let future_id = store.write_object(ObjectKind::Commit, &payload).unwrap();
        assert!(store
            .read_commit(&future_id)
            .unwrap_err()
            .to_string()
            .contains("unsupported native commit schema version"));
    }

    #[test]
    fn read_head_rejects_non_commit_kind() {
        // HEAD must name a commit. A valid-but-wrong-kind id (a blob/tree id written
        // directly) is rejected — distinct from the unparseable-garbage case.
        let temp = tempfile::tempdir().unwrap();
        let refs = NativeRefStore::new(temp.path());
        fs::create_dir_all(temp.path().join(".forge/refs")).unwrap();
        let tree_id = ObjectId::new(ObjectKind::Tree, b"a tree, not a commit");
        fs::write(
            temp.path().join(".forge/refs/HEAD"),
            tree_id.to_string().as_bytes(),
        )
        .unwrap();
        assert!(refs
            .read_head()
            .unwrap_err()
            .to_string()
            .contains("does not name a commit"));
    }

    #[test]
    fn reachable_from_head_covers_genesis_commit_and_its_tree() {
        // gc reachability must include the live history tip: the genesis commit AND the
        // objects its tree reaches (the gc-flags-base-as-garbage fix).
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join("a.txt"), b"hello").unwrap();
        let base = NativeContentBackend.current_base(repo).unwrap(); // creates genesis + HEAD
        let store = NativeObjectStore::new(repo);
        let reachable = store.reachable_from_head().unwrap();
        let genesis = ObjectId::parse(&base).unwrap();
        assert!(
            reachable.contains(&genesis),
            "genesis commit must be reachable from HEAD"
        );
        let tree = ObjectId::parse(&store.read_commit(&genesis).unwrap().tree).unwrap();
        assert!(reachable.contains(&tree), "genesis tree must be reachable");
        // A repo with no HEAD (git backend / pre-anchoring) yields an empty set.
        let empty = tempfile::tempdir().unwrap();
        assert!(NativeObjectStore::new(empty.path())
            .reachable_from_head()
            .unwrap()
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn native_changed_paths_reports_executable_bit_change() {
        // A chmod with unchanged content must surface in changed_paths (the blob id is
        // unchanged but the mode is part of the diff key) — parity with git diff.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join("script.sh"), b"#!/bin/sh\n").unwrap();
        let backend = NativeContentBackend;
        let first = backend.snapshot_worktree(repo).unwrap(); // genesis @ mode 644
        assert!(first.changed_paths.is_empty());
        fs::set_permissions(repo.join("script.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let snap = backend.snapshot_worktree(repo).unwrap();
        assert!(
            snap.changed_paths.contains(&"script.sh".to_string()),
            "executable-bit-only change must be reported: {:?}",
            snap.changed_paths
        );
    }

    #[test]
    fn native_changed_paths_handles_nested_subdirectories() {
        // Exercises flatten_tree's recursive Dir branch in the changed_paths path: a
        // modified file and an added file in a subdirectory both surface with /-joined paths.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/main.rs"), b"v1").unwrap();
        let backend = NativeContentBackend;
        backend.snapshot_worktree(repo).unwrap(); // genesis
        fs::write(repo.join("src/main.rs"), b"v2").unwrap();
        fs::write(repo.join("src/util.rs"), b"new").unwrap();
        let mut changed = backend.snapshot_worktree(repo).unwrap().changed_paths;
        changed.sort();
        assert_eq!(
            changed,
            vec!["src/main.rs".to_string(), "src/util.rs".to_string()]
        );
    }

    #[test]
    fn ref_store_head_roundtrips_and_replaces_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let refs = NativeRefStore::new(temp.path());
        assert!(
            refs.read_head().unwrap().is_none(),
            "absent HEAD reads as None"
        );
        let first = ObjectId::new(ObjectKind::Commit, b"first");
        refs.set_head(&first).unwrap();
        assert_eq!(refs.read_head().unwrap(), Some(first));
        // Atomic replace: a second set is read back whole (never torn), into a freshly
        // created `.forge/refs/` (ancestor-fsync path).
        let second = ObjectId::new(ObjectKind::Commit, b"second");
        refs.set_head(&second).unwrap();
        assert_eq!(refs.read_head().unwrap(), Some(second));
    }

    #[test]
    fn ref_store_corrupt_head_error_is_path_free() {
        // S1: a corrupt/garbage HEAD must surface a path-free error.
        let temp = tempfile::tempdir().unwrap();
        let refs = NativeRefStore::new(temp.path());
        fs::create_dir_all(temp.path().join(".forge/refs")).unwrap();
        fs::write(
            temp.path().join(".forge/refs/HEAD"),
            b"garbage-not-an-object-id",
        )
        .unwrap();
        let error = refs.read_head().unwrap_err();
        let repo_str = temp.path().to_string_lossy();
        assert!(
            !error.to_string().contains(&*repo_str) && !format!("{error:#}").contains(&*repo_str),
            "S1: corrupt-HEAD error leaked a path: {error:#}"
        );
    }

    #[test]
    fn current_base_creates_stable_genesis_commit() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join("a.txt"), b"hello").unwrap();
        let backend = NativeContentBackend;
        let base = backend.current_base(repo).unwrap();
        assert!(
            base.starts_with("f1:commit:sha256:"),
            "native base is a commit id, not a git SHA: {base}"
        );
        // Idempotent: HEAD now exists, so a second call returns the same genesis id.
        assert_eq!(backend.current_base(repo).unwrap(), base);
        // Stability (the stale-base correctness property): editing/adding worktree files
        // must NOT move the base anchor.
        fs::write(repo.join("a.txt"), b"changed").unwrap();
        fs::write(repo.join("b.txt"), b"new").unwrap();
        assert_eq!(
            backend.current_base(repo).unwrap(),
            base,
            "base anchor must not move on worktree edits"
        );
    }

    #[test]
    fn base_content_ref_materializes_policy_excluded_tree() {
        // S2: base_content_ref names a forge-tree: that excludes is_ignored_by_policy paths.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join("keep.txt"), b"data").unwrap();
        fs::write(repo.join(".env"), b"SECRET=1").unwrap();
        fs::write(repo.join("server.pem"), b"key").unwrap();
        let backend = NativeContentBackend;
        let base = backend.current_base(repo).unwrap();
        let content_ref = backend.base_content_ref(repo, &base).unwrap();
        assert!(
            content_ref.starts_with(FORGE_TREE_PREFIX),
            "base content ref is a native forge-tree: ref: {content_ref}"
        );
        let dest = tempfile::tempdir().unwrap();
        materialize_content_ref(repo, dest.path(), &content_ref).unwrap();
        assert_eq!(fs::read(dest.path().join("keep.txt")).unwrap(), b"data");
        assert!(
            !dest.path().join(".env").exists(),
            "S2: secret must not materialize from the base tree"
        );
        assert!(!dest.path().join("server.pem").exists());
    }

    #[test]
    fn base_content_ref_missing_commit_is_path_free() {
        // S1: resolving a base that points at a missing commit object surfaces a path-free
        // error (no repo/temp-dir path in the envelope or its chain).
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let backend = NativeContentBackend;
        let missing = format!("f1:commit:sha256:{}", "0".repeat(64));
        let error = backend.base_content_ref(repo, &missing).unwrap_err();
        let repo_str = repo.to_string_lossy();
        assert!(
            !error.to_string().contains(&*repo_str) && !format!("{error:#}").contains(&*repo_str),
            "S1: base_content_ref leaked a path: {error:#}"
        );
    }

    #[test]
    fn native_changed_paths_reports_added_modified_removed() {
        // No git binary involved — pure native tree diff.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join("keep.txt"), b"v1").unwrap();
        fs::write(repo.join("gone.txt"), b"bye").unwrap();
        let backend = NativeContentBackend;
        // First snapshot establishes the genesis base over the current worktree.
        let first = backend.snapshot_worktree(repo).unwrap();
        assert!(
            first.changed_paths.is_empty(),
            "genesis-equals-worktree yields no changes: {:?}",
            first.changed_paths
        );
        // Modify, add, remove.
        fs::write(repo.join("keep.txt"), b"v2").unwrap();
        fs::write(repo.join("new.txt"), b"hi").unwrap();
        fs::remove_file(repo.join("gone.txt")).unwrap();
        let mut changed = backend.snapshot_worktree(repo).unwrap().changed_paths;
        changed.sort();
        assert_eq!(
            changed,
            vec![
                "gone.txt".to_string(),
                "keep.txt".to_string(),
                "new.txt".to_string()
            ]
        );
    }

    #[test]
    fn native_changed_paths_excludes_secret() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        fs::write(repo.join("app.txt"), b"v1").unwrap();
        let backend = NativeContentBackend;
        backend.snapshot_worktree(repo).unwrap(); // establish genesis
        fs::write(repo.join(".env"), b"SECRET=1").unwrap();
        fs::write(repo.join("app.txt"), b"v2").unwrap();
        let snap = backend.snapshot_worktree(repo).unwrap();
        assert!(snap.changed_paths.contains(&"app.txt".to_string()));
        assert!(
            !snap.changed_paths.iter().any(|p| p.contains(".env")),
            "secret must never surface in changed_paths: {:?}",
            snap.changed_paths
        );
    }

    #[test]
    fn native_production_paths_shell_no_git() {
        // NER-138 exit criterion: no git binary in the native snapshot/base/changed-paths
        // production paths. The differential harness in THIS test module still shells git
        // for the slice-1 parity proofs, so scope the scan to the production prefix —
        // everything before the test module. The invariant is strong and false-positive-
        // free: production native code spawns NO subprocess at all (`Command::new`). The
        // `ignore`-crate walker, the object/ref stores, and the tree diff are all
        // in-process, so any `Command::new` outside #[cfg(test)] is a git-dependency
        // regression. (Substring-matching `ls-files`/`rev-parse` would false-positive on
        // doc comments that reference the removed git calls, so we gate on the spawn.)
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
            .expect("read forge-content-native/src/lib.rs");
        let production = src
            .split("#[cfg(test)]\nmod tests")
            .next()
            .expect("source has a production prefix before the test module");
        assert!(
            !production.contains("Command::new"),
            "native production code must spawn no subprocess (found `Command::new` outside \
             #[cfg(test)]); slice 2 made base_head + changed_paths git-binary-free"
        );
    }
}
