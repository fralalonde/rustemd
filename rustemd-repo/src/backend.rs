//! The [`Backend`] seam and its two implementations: a plain-directory
//! backend (always available, zero external dependencies) and a git backend
//! (optional, auto-detected) that records every mutation as a commit.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::Error;
use crate::unit::{UnitFile, UnitType};

/// Identifies which backend a [`Repo`](crate::Repo) is running on. Exposed so
/// the daemon can report it to clients over IPC and so tests can assert it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Plain directory: unit files are just files. Always available.
    Dir,
    /// Git repository: mutations are additionally `git add`ed and committed.
    Git,
}

impl BackendKind {
    /// Stable wire/CLI string: `"dir"` or `"git"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Dir => "dir",
            BackendKind::Git => "git",
        }
    }
}

/// The storage implementation behind a [`Repo`](crate::Repo).
///
/// # Why a trait?
///
/// "Repo" is deliberately swappable. The daemon and its clients must agree on
/// *what* a unit-file repository is, but not on *how* it stores bytes: a plain
/// directory is always available and needs nothing external, while a git work
/// tree gives auditability (one commit per mutation) at the cost of shelling
/// out to the `git` binary. The trait keeps those two implementations
/// interchangeable behind one API, and it is the extension point for future
/// backends (e.g. a purely in-memory backend for tests, or a transactional
/// overlay).
///
/// `list`/`read` are identical for every backend (a unit file is a file on
/// disk regardless of how writes are made durable), so both backends share the
/// [`list_dir`]/[`read_unit`] helpers. Only `write`/`delete`/`head` differ.
pub trait Backend: Send + Sync {
    /// Which backend this is.
    fn kind(&self) -> BackendKind;

    /// List the unit files directly under `root` (recognized suffixes only).
    fn list(&self, root: &Path) -> Result<Vec<UnitFile>, Error>;

    /// Read the raw text of `name` under `root`, or `None` if absent.
    fn read(&self, root: &Path, name: &str) -> Result<Option<String>, Error>;

    /// Atomically create or replace `name` under `root`.
    fn write(&self, root: &Path, name: &str, content: &str) -> Result<(), Error>;

    /// Remove `name` under `root` (idempotent: absent is not an error).
    fn delete(&self, root: &Path, name: &str) -> Result<(), Error>;

    /// The git HEAD commit under `root`, when git-backed; `None` otherwise.
    fn head(&self, root: &Path) -> Option<String>;
}

/// The always-available plain-directory backend. No external dependencies.
///
/// Writes are already atomic on their own: content is written to a temp file
/// in the *same directory* and then `rename(2)`-ed over the target, which is
/// an atomic replace on POSIX filesystems and on NTFS alike. No crash can leave
/// a half-written unit file at the target path.
#[derive(Debug, Default)]
pub struct DirBackend;

impl Backend for DirBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Dir
    }

    fn list(&self, root: &Path) -> Result<Vec<UnitFile>, Error> {
        list_dir(root)
    }

    fn read(&self, root: &Path, name: &str) -> Result<Option<String>, Error> {
        read_unit(root, name)
    }

    fn write(&self, root: &Path, name: &str, content: &str) -> Result<(), Error> {
        atomic_write(root, name, content)
    }

    fn delete(&self, root: &Path, name: &str) -> Result<(), Error> {
        delete_file(root, name)
    }

    fn head(&self, _root: &Path) -> Option<String> {
        None
    }
}

/// The optional git backend: same file semantics as [`DirBackend`], but every
/// mutation is additionally staged and committed as a single commit.
///
/// # Dependency choice
///
/// This backend shells out to the `git` binary (`git -C <root> ...`) rather
/// than linking `git2`/libgit2. Linking libgit2 would pull a large C
/// dependency (and its CVEs) into an init supervisor where binary size and
/// attack surface are first-class concerns, and it would break the
/// "zero C dependencies" goal. Shelling out is opportunistic: it needs
/// `git` on `PATH`, but the *repository* is still fully usable (list/read/
/// atomic-write) if `git` is missing — only commit history is lost. That
/// graceful degradation is exactly the fallback [`Repo::open`](crate::Repo::open)
/// implements.
#[derive(Debug, Default)]
pub struct GitBackend;

impl Backend for GitBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Git
    }

    fn list(&self, root: &Path) -> Result<Vec<UnitFile>, Error> {
        list_dir(root)
    }

    fn read(&self, root: &Path, name: &str) -> Result<Option<String>, Error> {
        read_unit(root, name)
    }

    fn write(&self, root: &Path, name: &str, content: &str) -> Result<(), Error> {
        // 1. Atomic file replace (same as the directory backend).
        atomic_write(root, name, content)?;
        // 2. Stage and commit as a single commit.
        git(root, &["add", "--", name])?;
        git_commit(root, &format!("rustemd-repo: update {name}"))?;
        Ok(())
    }

    fn delete(&self, root: &Path, name: &str) -> Result<(), Error> {
        // 1. Remove the file.
        delete_file(root, name)?;
        // 2. Stage the removal and commit as a single commit.
        git(root, &["add", "-A", "--", name])?;
        git_commit(root, &format!("rustemd-repo: delete {name}"))?;
        Ok(())
    }

    fn head(&self, root: &Path) -> Option<String> {
        git_output(root, &["rev-parse", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

/// Is the `git` binary available on `PATH`?
pub(crate) fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git(root: &Path, args: &[&str]) -> Result<(), Error> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(Error::Io)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Git(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

fn git_output(root: &Path, args: &[&str]) -> Result<String, Error> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(Error::Io)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(Error::Git(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

/// Commit, tolerating "nothing to commit" (e.g. deleting an untracked file,
/// or writing byte-identical content) so idempotent mutations never error.
fn git_commit(root: &Path, message: &str) -> Result<(), Error> {
    match git(root, &["commit", "-m", message]) {
        Ok(()) => Ok(()),
        Err(Error::Git(e))
            if e.contains("nothing to commit") || e.contains("working tree clean") =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// List unit files directly under `root`. A missing directory is an empty
/// listing, not an error — matching how the daemon treats absent search
/// paths.
fn list_dir(root: &Path) -> Result<Vec<UnitFile>, Error> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(Error::Io(e)),
    };
    for entry in rd {
        let entry = entry.map_err(Error::Io)?;
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|f| f.to_str()).map(str::to_string) else {
            continue;
        };
        let Some(kind) = UnitType::from_unit_name(&name) else {
            continue;
        };
        out.push(UnitFile {
            name,
            kind,
            path: p,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn read_unit(root: &Path, name: &str) -> Result<Option<String>, Error> {
    match std::fs::read_to_string(root.join(name)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

fn delete_file(root: &Path, name: &str) -> Result<(), Error> {
    match std::fs::remove_file(root.join(name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Monotonic suffix for temp-file names, so two writers in one process never
/// collide on the same temp path.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_path(root: &Path, name: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    root.join(format!(".{name}.rustemd-tmp-{}-{seq}", std::process::id()))
}

/// Write `content` to `name` atomically: write to a temp file in the *same
/// directory*, fsync it, then rename it over the target. The rename is atomic
/// on POSIX and NTFS, so readers always see either the old or the new file,
/// never a torn write.
fn atomic_write(root: &Path, name: &str, content: &str) -> Result<(), Error> {
    std::fs::create_dir_all(root).map_err(Error::Io)?;
    let target = root.join(name);
    let tmp = temp_path(root, name);

    let result = (|| -> Result<(), Error> {
        let mut f = std::fs::File::create(&tmp).map_err(Error::Io)?;
        std::io::Write::write_all(&mut f, content.as_bytes()).map_err(Error::Io)?;
        f.sync_all().ok(); // best-effort durability before the rename
        Ok(())
    })();

    match result {
        Ok(()) => {
            std::fs::rename(&tmp, &target).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                Error::Io(e)
            })?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}
