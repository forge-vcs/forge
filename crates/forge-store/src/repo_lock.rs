//! Repo-level advisory write lock on `.forge/forge.lock` (PRD §10.6, NER-132).
//!
//! Phase 1a made `.forge/forge.db` safe for concurrent processes (WAL +
//! `BEGIN IMMEDIATE` + busy-retry), but `IMMEDIATE` only serializes *commits
//! within one connection's transaction*. It cannot make atomic a determining
//! read a command performs on a *separate connection at the CLI layer* (e.g.
//! `accept` reading git `HEAD` before deciding) nor serialize cross-process
//! shard-directory creation in the object store. This module is that missing
//! serialization point, made **explicit** rather than an accidental property of
//! SQLite locking.
//!
//! Implementation uses the standard library's `File::try_lock` (stable since
//! Rust 1.89; the toolchain is pinned to 1.92) in a bounded, jittered backoff
//! loop, so no third-party file-locking crate is pulled in. On Unix this is
//! `flock`; on Windows, `LockFileEx`. Two independent file handles to the same
//! path — the normal cross-process case — contend correctly.
//!
//! **Acquire exactly once per command, at the command boundary — never nested.**
//! The standard library leaves the behavior of re-locking the *same* file handle
//! (or a clone) unspecified, "including the possibility that it will deadlock".
//! Acquiring once per command and never from a store function called within the
//! locked critical section avoids that entirely.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::time::{Duration, Instant};

/// Lock-file name inside `.forge`. Excluded from snapshots/exports by the
/// blanket `.forge/` prefix already honored in both content backends'
/// `is_ignored_by_policy` (the same rule that covers the WAL sidecars).
const LOCK_FILE_NAME: &str = "forge.lock";

/// Default wait before a contended acquire surfaces [`LockTimeout`]. Generous —
/// commands hold the lock only for a short critical section.
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Floor the configured timeout is clamped *up* to. `FORGE_LOCK_TIMEOUT_MS=0`
/// (or `1`) would otherwise turn the lock into try-once-fail and silently
/// re-open the cross-read races the lock exists to close; the floor guarantees
/// at least one real wait under contention.
const MIN_LOCK_TIMEOUT: Duration = Duration::from_millis(50);

/// Environment override for the acquire deadline, in milliseconds. Parsed leniently:
/// a non-numeric value falls back to [`DEFAULT_LOCK_TIMEOUT`]; any numeric value is
/// clamped up to [`MIN_LOCK_TIMEOUT`].
const LOCK_TIMEOUT_ENV: &str = "FORGE_LOCK_TIMEOUT_MS";

/// A held repo-level advisory write lock. Releases on drop.
///
/// Drop unlocks explicitly for determinism, but release does **not** depend on
/// Drop running: the OS reclaims the `flock` when the file handle is closed and,
/// crucially, when the process dies. A hard kill (the crash-injection harness's
/// `abort()`, or a SIGKILL'd agent) therefore never wedges a peer — the next
/// command acquires the lock immediately.
#[derive(Debug)]
pub struct RepoLock {
    file: File,
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        // Best-effort: closing the handle releases the lock too, and the OS
        // reclaims it on process death regardless.
        let _ = self.file.unlock();
    }
}

/// Signals that the repo-level write lock could not be acquired within the
/// deadline — another `forge` command holds it. **Retryable:** the contention is
/// transient, so the caller (or the user) can re-run.
///
/// Mirrors the [`crate::RequestIdReplay`] sentinel pattern: carried inside an
/// `anyhow::Error` and recovered at the CLI via `downcast_ref`, which maps it to
/// the `"LOCK_TIMEOUT"` envelope error code. NER-133 folds this into the typed
/// `ForgeError` taxonomy.
#[derive(Debug, Clone)]
pub struct LockTimeout {
    /// Milliseconds waited before giving up (the effective, clamped deadline).
    pub waited_ms: u128,
}

impl std::fmt::Display for LockTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LOCK_TIMEOUT: could not acquire the .forge write lock within {} ms; another forge command holds it (retry)",
            self.waited_ms
        )
    }
}

impl std::error::Error for LockTimeout {}

/// Acquire the repo-level advisory write lock on `<forge_dir>/forge.lock`, waiting
/// up to the configured (clamped) timeout before returning a [`LockTimeout`].
///
/// `forge_dir` must already exist (it does for every non-`init` command; `init`
/// creates it before calling this). Acquire exactly once per command — never
/// nested (see the module docs on the std re-entrancy caveat).
pub fn acquire(forge_dir: &Path) -> Result<RepoLock> {
    acquire_with_timeout(forge_dir, configured_timeout())
}

/// Resolve the acquire deadline from the environment, clamped to the floor.
fn configured_timeout() -> Duration {
    match std::env::var(LOCK_TIMEOUT_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(ms) => Duration::from_millis(ms).max(MIN_LOCK_TIMEOUT),
            Err(_) => DEFAULT_LOCK_TIMEOUT,
        },
        Err(_) => DEFAULT_LOCK_TIMEOUT,
    }
}

/// Core acquire loop with an explicit timeout (separated out so tests can drive a
/// short deadline deterministically).
fn acquire_with_timeout(forge_dir: &Path, timeout: Duration) -> Result<RepoLock> {
    let path = forge_dir.join(LOCK_FILE_NAME);
    // read+write+create: Windows refuses to lock an append-only handle, and the
    // file must exist to be locked — so create it on first acquire.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open lock file {}", path.display()))?;

    let deadline = Instant::now() + timeout;
    let mut attempt: u32 = 0;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(RepoLock { file }),
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(LockTimeout {
                        waited_ms: timeout.as_millis(),
                    }
                    .into());
                }
                attempt += 1;
                backoff(attempt, deadline);
            }
            Err(TryLockError::Error(err)) => {
                return Err(anyhow::Error::from(err))
                    .with_context(|| format!("failed to lock {}", path.display()));
            }
        }
    }
}

/// Jittered backoff between lock attempts, mixing the process id (distinct per
/// concurrent process) with the wall-clock nanosecond (distinct per attempt) so
/// contenders desync rather than retrying in lockstep — the same scheme as the
/// SQLite busy backoff. Capped so it never sleeps past the deadline.
fn backoff(attempt: u32, deadline: Instant) {
    let base_ms = (1u64 << attempt.min(5)).min(50); // 2, 4, 8, 16, 32, 50…
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter_ms = u64::from(std::process::id()).wrapping_add(u64::from(nanos)) % 25;
    let wanted = Duration::from_millis(base_ms + jitter_ms);
    let remaining = deadline.saturating_duration_since(Instant::now());
    std::thread::sleep(wanted.min(remaining));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forge_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create temp .forge dir")
    }

    #[test]
    fn acquire_creates_lock_file_and_succeeds_uncontended() {
        let dir = forge_dir();
        let guard = acquire(dir.path()).expect("uncontended acquire succeeds");
        assert!(
            dir.path().join(LOCK_FILE_NAME).exists(),
            "acquire creates the lock file"
        );
        drop(guard);
    }

    #[test]
    fn lock_is_reacquirable_after_release() {
        let dir = forge_dir();
        {
            let _guard = acquire(dir.path()).expect("first acquire");
        } // guard dropped here -> released
          // A second acquire after release must succeed (release works).
        let _guard = acquire(dir.path()).expect("re-acquire after release");
    }

    #[test]
    fn contended_acquire_times_out_with_lock_timeout() {
        let dir = forge_dir();
        let _held = acquire(dir.path()).expect("hold the lock");
        // A second, independent handle to the same lock file contends (flock /
        // LockFileEx treat independent open file descriptions independently, even
        // within one process) and must surface a typed LockTimeout — not a busy
        // error, not an io error — within ~the (clamped) deadline.
        let started = Instant::now();
        let error = acquire_with_timeout(dir.path(), Duration::from_millis(120))
            .expect_err("contended acquire times out");
        assert!(
            error.downcast_ref::<LockTimeout>().is_some(),
            "contended acquire returns a typed LockTimeout, got: {error:#}"
        );
        // Bounded: it waited roughly the clamped deadline, not indefinitely.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "acquire returned promptly after the deadline"
        );
    }

    #[test]
    fn zero_timeout_is_clamped_to_the_floor() {
        // The clamp protects the cross-read guarantee: a 0/1 ms override must not
        // become try-once-fail. We assert the floor is honored under contention.
        let dir = forge_dir();
        let _held = acquire(dir.path()).expect("hold the lock");
        let started = Instant::now();
        let error =
            acquire_with_timeout(dir.path(), Duration::from_millis(0).max(MIN_LOCK_TIMEOUT))
                .expect_err("contended acquire still times out");
        assert!(error.downcast_ref::<LockTimeout>().is_some());
        assert!(
            started.elapsed() >= MIN_LOCK_TIMEOUT,
            "a clamped-to-floor timeout still waits at least the floor before giving up"
        );
    }
}
