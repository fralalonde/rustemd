//! `rustemd-repo` — the DAO/CRUD layer for rustemd unit files, backed by a
//! *repository*.
//!
//! # What this crate is
//!
//! The rustemd daemon reads and lists unit files from disk; its clients
//! (`rustemctl`, `rustemd-tui`) need to discover *which* repository the daemon
//! uses and open it themselves. This crate is the single shared
//! implementation of that disk access: the daemon routes its unit LOAD/READ
//! path through it, and clients open the same repository with the same crate,
//! so the two can never disagree about what "the unit files" are.
//!
//! # Core API
//!
//! - [`Repo::open`] / [`Repo::open_roots`] — open a repository (auto-detecting
//!   a git backend); [`Repo::open_dir`] / [`Repo::open_git`] force a backend.
//! - [`Repo::list`] — list unit files (name + [`UnitType`]), merged across all
//!   roots with highest-precedence-first semantics.
//! - [`Repo::read`] / [`Repo::read_file`] — read a unit's raw content.
//! - [`Repo::write`] / [`Repo::create`] / [`Repo::update`] / [`Repo::delete`] —
//!   CRUD, all atomic and lock-serialized.
//! - [`Repo::mutate`] — atomic read-modify-write under the write lock.
//!
//! # The repository model (backends)
//!
//! A [`Repo`] is one or more *roots* (directories, highest precedence first).
//! Reads search the roots in order; writes target the first root (the
//! *primary* repository). Behind it sits a [`Backend`]:
//!
//! - [`DirBackend`] — plain directory, **always available**, zero external
//!   dependencies. Writes are atomic (temp file + `rename(2)`).
//! - [`GitBackend`] — a git work tree (auto-detected when `<root>/.git`
//!   exists *and* `git` is on `PATH`). Every mutation is additionally staged
//!   and committed as a single commit.
//!
//! See [`Backend`] for why the seam exists.
//!
//! # Atomicity
//!
//! Every write goes to a temp file in the *same directory* as the target and
//! is then renamed over it. `rename(2)` is an atomic replace on POSIX
//! filesystems and on NTFS, so a reader (or a crash) can only ever observe the
//! old file or the new file — never a torn, half-written unit. For the git
//! backend, the stage+commit happens *after* the atomic rename, so the commit
//! records a complete, already-visible file.
//!
//! # Ordering / conflict avoidance
//!
//! Atomic rename makes individual writes safe, but it does not order
//! *sequences* of writes (e.g. a read-modify-write, or two git commits racing).
//! A single-writer, per-repo advisory lock ([`RepoLock`]) serializes all
//! mutations: an exclusive `flock(2)` (Unix) / `LockFileEx` (Windows) lock
//! file in the repo root, plus a process-local mutex, so concurrent editors —
//! threads or processes — cannot interleave their edits. Compound edits that
//! need read-then-write semantics should go through [`Repo::mutate`], which
//! holds the lock across the whole read-modify-write and thereby makes the
//! classic lost-update conflict *impossible*.
//!
//! # Cross-platform
//!
//! Windows is a first-class target. The crate uses only `std` file primitives
//! (`std::fs::rename`, `std::fs::File::lock`), which the standard library maps
//! to the correct atomic-rename and `LockFileEx` semantics on Windows, so the
//! same code compiles and behaves identically for `x86_64-pc-windows-msvc`.
//!
//! # Design decisions (and where the wiggle room went)
//!
//! 1. **Multi-root `Repo`** — requirement: preserve reading from *all* the
//!    daemon's `unit_path` directories. A `Repo` therefore holds a list of
//!    roots; `list`/`read` merge across them, and writes go to the primary
//!    (first) root, which is also the directory the daemon reports over IPC.
//! 2. **Template instantiation stays in the daemon** — `getty@tty1.service`
//!    → `getty@.service` is a systemd unit-name rule, not a storage rule, so
//!    it lives in the daemon's path resolution. The daemon locates the file,
//!    then reads its *content* through [`Repo::read_file`], keeping the DAO
//!    generic and the read path unified.
//! 3. **No new dependencies** — `Backend::list`/`read` and the lock are
//!    pure `std`; `git` is shelled out to (never linked) and is optional, so
//!    the directory backend works with zero external deps.
//! 4. **Lock degradation** — a read-only repository loses cross-process
//!    locking but remains fully usable for reads (see [`RepoLock`]).

mod backend;
mod error;
mod lock;
mod unit;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub use backend::{Backend, BackendKind, DirBackend, GitBackend};
pub use error::Error;
pub use unit::{UnitFile, UnitType};

use backend::git_available;
use lock::RepoLock;

/// A unit-file repository: one or more root directories (highest precedence
/// first) plus the [`Backend`] that makes mutations durable.
pub struct Repo {
    /// Roots in precedence order; `roots[0]` is the primary (writable) repo.
    roots: Vec<PathBuf>,
    backend: Box<dyn Backend>,
    lock: RepoLock,
}

impl std::fmt::Debug for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repo")
            .field("roots", &self.roots)
            .field("backend", &self.backend.kind())
            .finish()
    }
}

impl Repo {
    /// Open a single-root repository, auto-detecting a git backend when
    /// `<root>/.git` exists and `git` is on `PATH` (falling back to the plain
    /// directory backend otherwise).
    pub fn open(root: PathBuf) -> Result<Repo, Error> {
        Self::open_roots(vec![root])
    }

    /// Open a multi-root repository. `roots[0]` is the primary (writable)
    /// root; later roots are read-only search paths consulted in precedence
    /// order. Git detection applies to the primary root only.
    ///
    /// Fails only when `roots` is empty.
    pub fn open_roots(roots: Vec<PathBuf>) -> Result<Repo, Error> {
        if roots.is_empty() {
            return Err(Error::InvalidName(
                "repository needs at least one root".into(),
            ));
        }
        let backend = detect_backend(&roots[0]);
        let lock = RepoLock::open(&roots[0]);
        Ok(Repo {
            roots,
            backend,
            lock,
        })
    }

    /// Open a single-root repository on the plain-directory backend, even if
    /// the directory happens to be a git work tree.
    pub fn open_dir(root: PathBuf) -> Result<Repo, Error> {
        Self::open_dir_roots(vec![root])
    }

    /// [`Repo::open_dir`] for a multi-root repository.
    pub fn open_dir_roots(roots: Vec<PathBuf>) -> Result<Repo, Error> {
        if roots.is_empty() {
            return Err(Error::InvalidName(
                "repository needs at least one root".into(),
            ));
        }
        let lock = RepoLock::open(&roots[0]);
        Ok(Repo {
            roots,
            backend: Box::new(DirBackend),
            lock,
        })
    }

    /// Open a single-root repository forcing the git backend. Fails if `git`
    /// is not on `PATH` or `<root>/.git` does not exist.
    pub fn open_git(root: PathBuf) -> Result<Repo, Error> {
        if !root.join(".git").exists() {
            return Err(Error::Git(format!(
                "{} is not a git work tree (no .git)",
                root.display()
            )));
        }
        if !git_available() {
            return Err(Error::Git("git binary not found on PATH".into()));
        }
        let lock = RepoLock::open(&root);
        Ok(Repo {
            roots: vec![root],
            backend: Box::new(GitBackend),
            lock,
        })
    }

    /// The primary (writable) root.
    pub fn root(&self) -> &Path {
        &self.roots[0]
    }

    /// All roots, highest precedence first.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Which backend this repository is running on.
    pub fn backend_kind(&self) -> BackendKind {
        self.backend.kind()
    }

    /// `true` when git-backed.
    pub fn is_git(&self) -> bool {
        self.backend.kind() == BackendKind::Git
    }

    /// The git HEAD commit (trimmed), when git-backed; `None` otherwise.
    pub fn git_head(&self) -> Option<String> {
        self.backend
            .head(self.root())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// List unit files, merged across all roots (a name that exists in
    /// several roots is reported once, from the highest-precedence root).
    /// Sorted by name.
    pub fn list(&self) -> Result<Vec<UnitFile>, Error> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for root in &self.roots {
            for uf in self.backend.list(root)? {
                if seen.insert(uf.name.clone()) {
                    out.push(uf);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Read the raw content of `name`, searching the roots in precedence
    /// order. `Ok(None)` when no root has the file.
    pub fn read(&self, name: &str) -> Result<Option<String>, Error> {
        validate_unit_name(name)?;
        for root in &self.roots {
            if let Some(text) = self.backend.read(root, name)? {
                return Ok(Some(text));
            }
        }
        Ok(None)
    }

    /// Read the raw content of an already-resolved unit file path.
    ///
    /// This is the escape hatch the daemon uses: the daemon owns systemd
    /// path semantics (search precedence and `getty@tty1` → `getty@.service`
    /// template instantiation), so it resolves the path itself and then reads
    /// the bytes through the repository. Keeping template instantiation in the
    /// daemon keeps this storage layer generic.
    pub fn read_file(&self, path: &Path) -> Result<String, Error> {
        std::fs::read_to_string(path).map_err(Error::Io)
    }

    /// Atomically create or replace `name` in the primary root (upsert).
    pub fn write(&self, name: &str, content: &str) -> Result<(), Error> {
        validate_unit_name(name)?;
        let _guard = self.lock.acquire();
        self.backend.write(self.root(), name, content)
    }

    /// Create `name` in the primary root, failing with
    /// [`Error::AlreadyExists`] if it already exists.
    pub fn create(&self, name: &str, content: &str) -> Result<(), Error> {
        validate_unit_name(name)?;
        let _guard = self.lock.acquire();
        if self.backend.read(self.root(), name)?.is_some() {
            return Err(Error::AlreadyExists(name.into()));
        }
        self.backend.write(self.root(), name, content)
    }

    /// Replace `name` in the primary root, failing with [`Error::NotFound`]
    /// if it does not exist.
    pub fn update(&self, name: &str, content: &str) -> Result<(), Error> {
        validate_unit_name(name)?;
        let _guard = self.lock.acquire();
        if self.backend.read(self.root(), name)?.is_none() {
            return Err(Error::NotFound(name.into()));
        }
        self.backend.write(self.root(), name, content)
    }

    /// Delete `name` from the primary root (idempotent: absent is not an
    /// error).
    pub fn delete(&self, name: &str) -> Result<(), Error> {
        validate_unit_name(name)?;
        let _guard = self.lock.acquire();
        self.backend.delete(self.root(), name)
    }

    /// Atomically transform a unit file under the write lock.
    ///
    /// Reads `name`, calls `f` with its current content (`None` if absent),
    /// then writes back whatever `f` returns — or deletes the file if `f`
    /// returns `None`. The whole read-modify-write runs under the per-repo
    /// lock, so two concurrent `mutate` calls cannot lose each other's
    /// updates. This is the primitive for edits that must be *ordered*, not
    /// merely atomic.
    pub fn mutate<F>(&self, name: &str, f: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&str>) -> Option<String>,
    {
        validate_unit_name(name)?;
        let _guard = self.lock.acquire();
        let current = self.backend.read(self.root(), name)?;
        match f(current.as_deref()) {
            Some(new) => self.backend.write(self.root(), name, &new),
            None => self.backend.delete(self.root(), name),
        }
    }
}

fn detect_backend(primary: &Path) -> Box<dyn Backend> {
    if primary.join(".git").exists() && git_available() {
        Box::new(GitBackend)
    } else {
        Box::new(DirBackend)
    }
}

fn validate_unit_name(name: &str) -> Result<(), Error> {
    if name.is_empty() {
        return Err(Error::InvalidName("unit name is empty".into()));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(Error::InvalidName(format!(
            "unit name `{name}` must not contain path separators"
        )));
    }
    if name == "." || name == ".." || name.starts_with("../") || name.starts_with("..\\") {
        return Err(Error::InvalidName(format!(
            "unit name `{name}` is not a plain file name"
        )));
    }
    if UnitType::from_unit_name(name).is_none() {
        return Err(Error::InvalidName(format!(
            "unit name `{name}` has no recognized unit suffix"
        )));
    }
    Ok(())
}
