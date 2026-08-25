//! Per-repository advisory lock.
//!
//! Mutations (create/update/delete/write) and compound read-modify-write
//! operations are serialized by a single-writer lock scoped to one
//! repository, so concurrent editors — other threads *or* other processes —
//! cannot interleave their edits and produce a conflicting state.
//!
//! The lock has two layers, which together cover every contention case:
//!
//! 1. A process-local [`std::sync::Mutex`] serializes threads sharing one
//!    [`Repo`](crate::Repo) handle.
//! 2. An OS advisory lock ([`std::fs::File::lock`], which is `flock(2)` on
//!    Unix and `LockFileEx` on Windows) on a lock file inside the repository
//!    root serializes *independent* handles and *independent processes*.
//!
//! `File::lock` is std-only, so the lock adds **zero new dependencies** and
//! no C code — consistent with the project's minimal attack-surface goal.
//!
//! # Degradation on read-only repositories
//!
//! If the lock file cannot be opened (a read-only repository, for example),
//! cross-process serialization is silently unavailable and only the
//! in-process mutex remains. Reads are never locked, so a read-only
//! repository is fully usable for listing and reading.

use std::fs::File;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// File name of the per-repo lock file, relative to the repository root.
pub(crate) const LOCK_FILE_NAME: &str = ".rustemd-repo.lock";

pub(crate) struct RepoLock {
    /// Serializes threads that share a single `Repo` handle.
    inner: Mutex<()>,
    /// The OS-level lock file; `None` when the repository is read-only.
    lock_file: Option<File>,
}

impl RepoLock {
    /// Open (creating if necessary) the lock file in `root`. Never fails: a
    /// failure to create the lock file degrades to `lock_file: None`.
    pub(crate) fn open(root: &Path) -> RepoLock {
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(LOCK_FILE_NAME))
            .ok();
        RepoLock {
            inner: Mutex::new(()),
            lock_file,
        }
    }

    /// Acquire the write lock and return a guard that releases it on drop.
    pub(crate) fn acquire(&self) -> RepoLockGuard<'_> {
        // The mutex is the in-process (thread) half of the lock.
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // The OS lock is the cross-process half. Best-effort: if it fails the
        // mutation still proceeds, just without cross-process serialization.
        if let Some(f) = &self.lock_file {
            let _ = f.lock();
        }
        RepoLockGuard {
            _guard: guard,
            lock_file: self.lock_file.as_ref(),
        }
    }
}

pub(crate) struct RepoLockGuard<'a> {
    // Held for the guard's lifetime to keep the in-process mutex locked.
    _guard: MutexGuard<'a, ()>,
    lock_file: Option<&'a File>,
}

impl Drop for RepoLockGuard<'_> {
    fn drop(&mut self) {
        if let Some(f) = self.lock_file {
            let _ = f.unlock();
        }
        // `guard` drops here, releasing the in-process mutex.
    }
}
