use super::*;

pub fn signing_key_fingerprint_for_public_key(public_key: &[u8]) -> String {
    signing::key_fingerprint_for_public_key(public_key)
}

#[derive(Debug, Clone, Serialize)]
pub struct InitRepository {
    pub repository_id: String,
    pub root_path: String,
    pub forge_dir: String,
    pub database_path: String,
    pub git_head: Option<String>,
    pub content_backend: String,
    pub current_operation_id: String,
    pub current_view_id: String,
    pub already_initialized: bool,
}

#[derive(Debug, Clone)]
pub struct RequestIdOperation {
    pub operation_id: String,
    pub command: String,
    pub status: String,
    pub error_json: Option<Value>,
    pub kind: Option<String>,
    pub view_id: Option<String>,
    pub state: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct RepositoryContext {
    pub repo_id: String,
    pub root_path: PathBuf,
    pub worktree_path: PathBuf,
    pub database_path: PathBuf,
    pub content_backend: String,
    pub current_operation_id: String,
    pub current_view_id: String,
    pub attached_attempt_id: Option<String>,
    pub workspace_attempt_id: Option<String>,
}

pub fn init_repository(
    cwd: &Path,
    request_id: Option<String>,
    content_backend: String,
) -> Result<InitRepository> {
    if !matches!(content_backend.as_str(), "git" | "native") {
        bail!("unsupported content backend");
    }
    // A NATIVE repo earns real git independence: it does not require the git binary at init —
    // its root is `cwd` (canonicalized). A GIT-backed repo still anchors on the git toplevel
    // (Forge layers on an existing git repo). This is what lets the full native lifecycle —
    // init included — run with git removed from PATH (NER-138 Phase 7 exit criterion).
    let root = if content_backend == "native" {
        cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
    } else {
        git_root(cwd)?
    };
    // NER-143 R9: refuse to initialize a forge repo nested inside an existing forge repo.
    // `forge_root`'s nearest-ancestor walk routes a subtree's commands to whichever `.forge`
    // is closer up-tree, and the nested repo's objects look unreachable to the outer repo's
    // gc (a Phase-8 deletion hazard). The check is BACKEND-AGNOSTIC because `forge_root` is:
    // a native inner repo anchors at `cwd` (so it can nest below anything), and a git inner
    // repo whose own toplevel sits below an outer repo can nest too — both are shadowing
    // hazards regardless of backend, so the guard must not be gated on `content_backend`
    // (the code-review adversarial pass flagged the native-only gating as an escape). Message
    // is path-free (S1). This checks ANCESTORS only (`root.parent()` upward), so re-init of
    // the same root never trips it and stays the already_initialized path below. (A
    // deliberately-independent nested repo is not a v0 use case; an --allow-nested opt-out is
    // future work. A narrow cross-repo-init TOCTOU window — two inits racing in
    // ancestor/descendant dirs before either's lock — is an accepted v0 limitation.)
    {
        let mut ancestor = root.parent();
        while let Some(dir) = ancestor {
            if dir.join(".forge/forge.db").exists() {
                bail!("refusing to initialize a forge repo nested inside an existing forge repo");
            }
            ancestor = dir.parent();
        }
    }
    let forge_dir = root.join(".forge");
    fs::create_dir_all(&forge_dir)
        .with_context(|| format!("failed to create {}", forge_dir.display()))?;

    // Serialize concurrent first-inits of the same repo (NER-132 U5): hold the repo
    // write lock across migration + the repository INSERT, so a racing init observes
    // the winner's committed row via read_init_repository below and returns
    // already_initialized rather than colliding on the repositories.root_path UNIQUE
    // constraint. The lock file lives in the .forge dir just created. init does not
    // route through the CLI command_result lock, so it acquires here, never nested.
    let _init_lock = repo_lock::acquire(&forge_dir)?;

    let database_path = forge_dir.join("forge.db");
    let already_had_db = database_path.exists();
    let mut connection = open_connection(&database_path)
        .with_context(|| format!("failed to open {}", database_path.display()))?;
    migrations::apply_pending_migrations(&mut connection)?;

    if let Some(existing) = read_init_repository(&connection, &root, &forge_dir, &database_path)? {
        return Ok(InitRepository {
            already_initialized: true,
            ..existing
        });
    }

    // A native repo records no git_head (it has its own ref store and never shells git);
    // recording it would reintroduce a git dependency at init.
    let git_head = if content_backend == "native" {
        None
    } else {
        git_head(&root)
    };
    let repo_id = RepositoryId::new().to_string();
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let state_json = json!({
        "repository_id": repo_id,
        "root_path": root,
        "git_head": git_head,
        "content_backend": content_backend,
        "lifecycle": "initialized"
    })
    .to_string();
    // No replay guard here: `init` has its own idempotency via the
    // `read_init_repository` short-circuit above; this only adds IMMEDIATE +
    // busy-retry for R3 consistency.
    with_immediate_retry(&mut connection, |tx| {
        tx.execute(
            "INSERT INTO repositories (id, root_path, git_head, content_backend, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![repo_id, root.to_string_lossy(), git_head, content_backend, now],
        )?;
        // The genesis link: parent is the documented genesis sentinel, no domain
        // digest. Stored so `doctor`'s re-walk starts from a verifiable anchor and a
        // fresh repo is never mis-flagged as a NULL-hash (tampered) op (NER-136).
        let genesis_hash = integrity::operation_link_hash(
            integrity::GENESIS_PARENT_HASH,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command: "init",
                kind: "repository_initialized",
                created_at_ms: now,
            },
            None,
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, 'init', ?4, 'repository_initialized', NULL, ?5, NULL, ?6, ?7)",
            params![
                operation_id,
                repo_id,
                request_id,
                format!("{:?}", OperationStatus::Succeeded).to_lowercase(),
                view_id,
                genesis_hash,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'initialized', ?4, ?5)",
            params![view_id, repo_id, operation_id, state_json, now],
        )?;
        tx.execute(
            "INSERT INTO current_state (
                singleton, repo_id, current_operation_id, current_view_id, attached_attempt_id, updated_at_ms
            ) VALUES (1, ?1, ?2, ?3, NULL, ?4)",
            params![repo_id, operation_id, view_id, now],
        )?;
        Ok(())
    })?;

    Ok(InitRepository {
        repository_id: repo_id,
        root_path: root.to_string_lossy().into_owned(),
        forge_dir: forge_dir.to_string_lossy().into_owned(),
        database_path: database_path.to_string_lossy().into_owned(),
        git_head,
        content_backend,
        current_operation_id: operation_id,
        current_view_id: view_id,
        already_initialized: already_had_db,
    })
}

pub fn open_repository(cwd: &Path) -> Result<RepositoryContext> {
    // Git-free root resolution (slice 3): walk up for `.forge/forge.db` rather than shelling
    // `git rev-parse`, so every post-init command works with git removed from PATH.
    let (root, worktree_path, workspace_attempt_id) = repository_location(cwd)?;
    let database_path = root.join(".forge/forge.db");
    if !database_path.exists() {
        return Err(ForgeError::NotInitialized.into());
    }
    // `open_repository` is a pure open+query: schema migrations are applied by the
    // transient `migrate()` entrypoint at the top of `command_result` (and by
    // `init_repository` under its own lock) — never here, where no lock is held.
    let connection = open_connection(&database_path)?;
    let (repo_id, content_backend, current_operation_id, current_view_id, attached_attempt_id): (
        String,
        String,
        String,
        String,
        Option<String>,
    ) = connection.query_row(
        "SELECT cs.repo_id, r.content_backend, cs.current_operation_id, cs.current_view_id, cs.attached_attempt_id
             FROM current_state cs
             JOIN repositories r ON r.id = cs.repo_id
             WHERE cs.singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    Ok(RepositoryContext {
        repo_id,
        root_path: root,
        worktree_path,
        database_path,
        content_backend,
        current_operation_id,
        current_view_id,
        attached_attempt_id,
        workspace_attempt_id,
    })
}

pub fn acquire_repository_lock(cwd: &Path) -> Result<RepoLock> {
    let context = open_repository(cwd)?;
    repo_lock::acquire(&context.root_path.join(".forge"))
}

pub fn effective_worktree_path(cwd: &Path) -> Result<PathBuf> {
    Ok(open_repository(cwd)?.worktree_path)
}

pub fn repository_root_path(cwd: &Path) -> Result<PathBuf> {
    Ok(open_repository(cwd)?.root_path)
}

pub fn repository_content_backend(cwd: &Path) -> Result<String> {
    Ok(open_repository(cwd)?.content_backend)
}

/// Acquire the repo-level advisory write lock for the repository containing `cwd`
/// (PRD §10.6, NER-132). The CLI holds the returned guard across a mutating
/// command's critical section so its determining reads and write are atomic
/// against other `forge` writers.
///
/// Returns `Ok(None)` when there is no repository to lock — `cwd` is not inside a
/// Git work tree, or `.forge` does not exist yet — so the caller's own logic
/// surfaces the canonical "not initialized" error instead of a lock-file error.
/// A genuine contention timeout surfaces as a [`LockTimeout`] (`Err`).
pub fn acquire_repo_lock(cwd: &Path) -> Result<Option<RepoLock>> {
    // Git-free root resolution (slice 3): a `.forge`-walk, so locking works without git.
    let root = match forge_root(cwd) {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    let forge_dir = root.join(".forge");
    if !forge_dir.exists() {
        return Ok(None);
    }
    repo_lock::acquire(&forge_dir).map(Some)
}

pub fn acquire_worktree_lock(cwd: &Path, attempt_id: &str) -> Result<RepoLock> {
    let context = open_repository(cwd)?;
    attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
        selector: attempt_id.to_string(),
    })?;
    let lock_path = context
        .root_path
        .join(".forge/worktree-locks")
        .join(format!("{attempt_id}.lock"));
    repo_lock::acquire_lock_file(&lock_path)
}

/// Bring the repository's schema up to this binary's head, acquiring the repo
/// write lock **only when a migration is actually pending** (NER-133 U4).
///
/// This is the transient, self-acquiring entrypoint the CLI runs at the top of
/// `command_result`, **before** any per-command lock — so it never nests inside an
/// already-held lock. It mirrors [`acquire_repo_lock`]'s resolution so it no-ops
/// when there is nothing to migrate, letting the command's own logic surface the
/// canonical error:
/// - `cwd` is not inside a Git work tree ⇒ `Ok(())` (the command surfaces
///   not-a-git-repo / `NOT_INITIALIZED`).
/// - `.forge/forge.db` does not exist ⇒ `Ok(())` (uninitialized — the command
///   surfaces `NOT_INITIALIZED`).
/// - DB version `== HEAD` ⇒ `Ok(())` on the cheap read, **no lock taken** (the
///   common path, including every read-only command and `run`).
/// - DB version `> HEAD` ⇒ `Err(ForgeError::UnknownSchemaVersion)`: the DB was
///   written by a newer Forge; refuse without acquiring the lock. The CLI maps
///   this to `SCHEMA_VERSION_UNSUPPORTED` and short-circuits before any write.
/// - DB version `< HEAD` ⇒ acquire the repo lock **transiently**, apply the
///   pending migrations (idempotent + version-gated, so a concurrent migrator that
///   won the race is handled), then release the lock (Drop) before returning.
pub fn migrate(cwd: &Path) -> Result<()> {
    // Git-free root resolution (slice 3): a `.forge`-walk, so migration works without git.
    let root = match forge_root(cwd) {
        Ok(root) => root,
        Err(_) => return Ok(()),
    };
    let forge_dir = root.join(".forge");
    let database_path = forge_dir.join("forge.db");
    if !database_path.exists() {
        return Ok(());
    }

    let mut connection = open_connection(&database_path)?;
    let db_version = migrations::current_schema_version(&connection)?;
    let head = migrations::schema_head();

    if db_version == head {
        // Common fast path: nothing to do, take no lock.
        return Ok(());
    }
    if db_version > head {
        // A forward-versioned DB: refuse to write without taking the lock.
        return Err(ForgeError::UnknownSchemaVersion {
            db_version,
            supported_head: head,
        }
        .into());
    }

    // Pending (`db_version < head`): acquire the repo lock transiently, apply, and
    // release on Drop before returning. `apply_pending_migrations` re-reads the
    // applied versions under the lock and is idempotent, so a concurrent migrator
    // that won the race is a no-op here. Acquired exactly once, before the
    // per-command lock — never nested.
    let _lock = repo_lock::acquire(&forge_dir)?;
    migrations::apply_pending_migrations(&mut connection)?;
    register_existing_local_key_after_migration(&root, &connection)?;
    Ok(())
}

fn register_existing_local_key_after_migration(root: &Path, connection: &Connection) -> Result<()> {
    let Some(key) = signing::existing_local_key_info(root)? else {
        return Ok(());
    };
    let repo_id = connection
        .query_row(
            "SELECT id FROM repositories ORDER BY rowid LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(repo_id) = repo_id {
        signing::register_local_signing_key(
            connection,
            &repo_id,
            &key.public_key,
            &key.key_fingerprint,
            now_ms(),
        )?;
    }
    Ok(())
}

pub fn operation_for_request(cwd: &Path, request_id: &str) -> Result<Option<RequestIdOperation>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT o.id, o.command, o.status, o.error_json, o.kind, o.resulting_view_id, v.state_json
             FROM operations o
             LEFT JOIN views v ON v.id = o.resulting_view_id
             WHERE o.repo_id = ?1 AND o.request_id = ?2
             ORDER BY o.created_at_ms DESC, o.rowid DESC LIMIT 1",
            params![context.repo_id, request_id],
            |row| {
                let error_json: Option<String> = row.get(3)?;
                let state_json: Option<String> = row.get(6)?;
                Ok(RequestIdOperation {
                    operation_id: row.get(0)?,
                    command: row.get(1)?,
                    status: row.get(2)?,
                    error_json: error_json.and_then(|json| serde_json::from_str(&json).ok()),
                    kind: row.get(4)?,
                    view_id: row.get(5)?,
                    state: state_json.and_then(|json| serde_json::from_str(&json).ok()),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn read_init_repository(
    connection: &Connection,
    root: &Path,
    forge_dir: &Path,
    database_path: &Path,
) -> Result<Option<InitRepository>> {
    let row = connection
        .query_row(
            "SELECT r.id, r.git_head, r.content_backend, cs.current_operation_id, cs.current_view_id
             FROM repositories r
             JOIN current_state cs ON cs.repo_id = r.id
             WHERE r.root_path = ?1",
            params![root.to_string_lossy()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;

    Ok(row.map(
        |(repository_id, git_head, content_backend, current_operation_id, current_view_id)| {
            InitRepository {
                repository_id,
                root_path: root.to_string_lossy().into_owned(),
                forge_dir: forge_dir.to_string_lossy().into_owned(),
                database_path: database_path.to_string_lossy().into_owned(),
                git_head,
                content_backend,
                current_operation_id,
                current_view_id,
                already_initialized: true,
            }
        },
    ))
}

/// Find the Forge repository root by walking up from `cwd` for the nearest ancestor that
/// contains the `.forge/forge.db` repo marker. GIT-FREE (NER-138 Phase 7 slice 3): post-`init`
/// commands resolve the root without the git binary, so the native lifecycle
/// (start→save→…→accept→restore→log→checkout→undo) runs with git removed from PATH. `init`
/// still anchors a *git-backed* repo on the git toplevel (Forge layers on an existing git
/// repo); a *native* repo's root is established at init without git. Returns
/// `NotInitialized` when no `.forge/forge.db` is found up the tree.
fn repository_location(cwd: &Path) -> Result<(PathBuf, PathBuf, Option<String>)> {
    let start = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut current: &Path = &start;
    loop {
        if current.join(".forge/forge.db").exists() {
            return Ok((current.to_path_buf(), current.to_path_buf(), None));
        }
        let marker_path = current.join(forge_content::WORKSPACE_MARKER_FILE);
        if marker_path.exists() {
            let marker: WorkspaceMarker = serde_json::from_slice(
                &fs::read(&marker_path)
                    .map_err(|error| anyhow!("read workspace marker: {}", error.kind()))?,
            )
            .map_err(|_| anyhow!("workspace marker is corrupt"))?;
            let root = PathBuf::from(marker.repo_root);
            if !root.join(".forge/forge.db").exists() {
                return Err(ForgeError::NotInitialized.into());
            }
            return Ok((root, current.to_path_buf(), Some(marker.attempt_id)));
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return Err(ForgeError::NotInitialized.into()),
        }
    }
}

fn forge_root(cwd: &Path) -> Result<PathBuf> {
    repository_location(cwd).map(|(root, _, _)| root)
}

fn git_root(cwd: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd)
        .output()
        .context("failed to run git")?;

    if !output.status.success() {
        return Err(anyhow!(
            "forge init must run inside an existing Git repository"
        ));
    }

    let root = String::from_utf8(output.stdout)?.trim().to_string();
    if root.is_empty() {
        return Err(anyhow!("git returned an empty repository root"));
    }

    Ok(PathBuf::from(root))
}

fn git_head(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
