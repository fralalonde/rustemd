//! Integration tests for the repository crate: list/read/CRUD, atomic-rename
//! under contention, lock-ordered read-modify-write, multi-root precedence,
//! name validation, and the (skip-if-no-git) git backend.

use std::process::Command;

use rustemd_repo::{BackendKind, Error, Repo, UnitType};

fn temp_repo() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::open_dir(dir.path().to_path_buf()).unwrap();
    (dir, repo)
}

#[test]
fn list_read_crud_roundtrip() {
    let (dir, repo) = temp_repo();

    assert!(repo.list().unwrap().is_empty());
    assert_eq!(repo.read("nope.service").unwrap(), None);

    repo.create("hello.service", "[Service]\nExecStart=/bin/true\n")
        .unwrap();
    let list = repo.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "hello.service");
    assert_eq!(list[0].kind, UnitType::Service);
    assert_eq!(list[0].path, dir.path().join("hello.service"));

    assert_eq!(
        repo.read("hello.service").unwrap(),
        Some("[Service]\nExecStart=/bin/true\n".to_string())
    );

    // create refuses to overwrite.
    assert!(matches!(
        repo.create("hello.service", "x"),
        Err(Error::AlreadyExists(_))
    ));

    // update writes; update refuses to create.
    repo.update("hello.service", "[Service]\nExecStart=/bin/false\n")
        .unwrap();
    assert!(matches!(
        repo.update("missing.service", "x"),
        Err(Error::NotFound(_))
    ));

    // write is an upsert.
    repo.write("hello.service", "[Unit]\nDescription=x\n")
        .unwrap();
    assert_eq!(
        repo.read("hello.service").unwrap(),
        Some("[Unit]\nDescription=x\n".to_string())
    );

    // delete is idempotent.
    repo.delete("hello.service").unwrap();
    repo.delete("hello.service").unwrap();
    assert_eq!(repo.read("hello.service").unwrap(), None);
    assert!(repo.list().unwrap().is_empty());
}

#[test]
fn write_leaves_no_temp_files() {
    let (dir, repo) = temp_repo();
    repo.write("foo.service", "one").unwrap();
    repo.write("foo.service", "two").unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("rustemd-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp files must be cleaned up");
}

#[test]
fn atomic_rename_never_tears_writes() {
    let (_dir, repo) = temp_repo();
    // Every writer writes a distinct, large, well-formed payload to the SAME
    // unit. Atomic rename guarantees the final file is exactly one payload.
    let payload = |tag: char| format!("{tag}{}", "x".repeat(4096));

    std::thread::scope(|s| {
        for tag in ['a', 'b', 'c', 'd', 'e', 'f'] {
            let repo = &repo;
            s.spawn(move || {
                for _ in 0..50 {
                    repo.write("big.service", &payload(tag)).unwrap();
                }
            });
        }
    });

    let final_text = repo.read("big.service").unwrap().unwrap();
    let tag = final_text.as_bytes()[0] as char;
    assert_eq!(
        final_text,
        payload(tag),
        "content must be exactly one full payload"
    );
}

#[test]
fn mutate_orders_read_modify_write_without_lost_updates() {
    let (_dir, repo) = temp_repo();
    repo.write("counter.service", "0").unwrap();

    // 8 threads × 100 read-modify-write increments. Without the per-repo lock
    // these would lose updates (final < 800). `mutate` holds the lock across
    // the whole read->write, so the final value is exact.
    std::thread::scope(|s| {
        for _ in 0..8 {
            let repo = &repo;
            s.spawn(move || {
                for _ in 0..100 {
                    repo.mutate("counter.service", |cur| {
                        let n: u64 = cur.and_then(|c| c.trim().parse().ok()).unwrap_or(0);
                        Some((n + 1).to_string())
                    })
                    .unwrap();
                }
            });
        }
    });

    assert_eq!(repo.read("counter.service").unwrap().unwrap(), "800");
}

#[test]
fn multi_root_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let primary = dir.path().join("primary");
    let secondary = dir.path().join("secondary");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&secondary).unwrap();

    // Same name in both roots; primary wins.
    std::fs::write(primary.join("foo.service"), "from-primary").unwrap();
    std::fs::write(secondary.join("foo.service"), "from-secondary").unwrap();
    // Only in secondary.
    std::fs::write(secondary.join("bar.service"), "bar").unwrap();

    let repo = Repo::open_roots(vec![primary.clone(), secondary.clone()]).unwrap();
    let list = repo.list().unwrap();
    let names: Vec<_> = list.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(names, vec!["bar.service", "foo.service"]);
    // `foo.service` resolves to the primary copy.
    let foo = list.iter().find(|u| u.name == "foo.service").unwrap();
    assert_eq!(foo.path, primary.join("foo.service"));
    assert_eq!(repo.read("foo.service").unwrap().unwrap(), "from-primary");
    assert_eq!(repo.read("bar.service").unwrap().unwrap(), "bar");

    // Writes always target the primary root.
    repo.write("new.service", "new").unwrap();
    assert!(primary.join("new.service").exists());
    assert!(!secondary.join("new.service").exists());
}

#[test]
fn name_validation_rejects_traversal_and_unknown_suffix() {
    let (_dir, repo) = temp_repo();
    for bad in [
        "../evil.service",
        "a/b.service",
        "..\\evil.service",
        "no-suffix",
        "",
        "..",
    ] {
        assert!(
            matches!(repo.write(bad, "x"), Err(Error::InvalidName(_))),
            "should reject `{bad}`"
        );
    }
}

#[test]
fn missing_directory_lists_empty() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::open_dir(dir.path().join("does-not-exist")).unwrap();
    assert!(repo.list().unwrap().is_empty());
    assert_eq!(repo.read("foo.service").unwrap(), None);
    // Writing creates the directory on demand.
    repo.write("foo.service", "x").unwrap();
    assert_eq!(repo.read("foo.service").unwrap(), Some("x".to_string()));
}

#[test]
fn open_detects_backend_kind() {
    let dir = tempfile::tempdir().unwrap();
    // No .git -> dir backend.
    let repo = Repo::open(dir.path().to_path_buf()).unwrap();
    assert_eq!(repo.backend_kind(), BackendKind::Dir);
    assert!(!repo.is_git());
    assert_eq!(repo.git_head(), None);
}

fn git_on_path() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn git_backend_stages_and_commits() {
    if !git_on_path() {
        eprintln!("skipping git_backend_stages_and_commits: git not on PATH");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run = |args: &[&str]| {
        let out = Command::new("git").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    run(&["init", "-q", root.to_str().unwrap()]);
    run(&[
        "-C",
        root.to_str().unwrap(),
        "config",
        "user.email",
        "repo@example.com",
    ]);
    run(&[
        "-C",
        root.to_str().unwrap(),
        "config",
        "user.name",
        "rustemd-repo test",
    ]);
    run(&[
        "-C",
        root.to_str().unwrap(),
        "config",
        "commit.gpgsign",
        "false",
    ]);

    let repo = Repo::open(root.to_path_buf()).unwrap();
    assert_eq!(repo.backend_kind(), BackendKind::Git);
    assert!(repo.is_git());
    assert!(repo.git_head().is_none(), "no commits yet");

    repo.write("hello.service", "[Service]\nExecStart=/bin/true\n")
        .unwrap();
    let head = repo.git_head().expect("a commit must exist after write");
    assert!(!head.is_empty());

    let log = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout).to_string();
    assert!(log.contains("update hello.service"), "log was: {log}");

    repo.delete("hello.service").unwrap();
    let log = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout).to_string();
    assert!(log.contains("delete hello.service"), "log was: {log}");

    // The committed content is recoverable and correct.
    let shown = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &format!("{head}:hello.service")])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&shown.stdout),
        "[Service]\nExecStart=/bin/true\n"
    );
}

#[test]
fn open_git_errors_without_git_or_work_tree() {
    let dir = tempfile::tempdir().unwrap();
    // Not a git work tree.
    assert!(Repo::open_git(dir.path().join("plain")).is_err());
}

#[test]
fn read_file_reads_resolved_path() {
    let (dir, repo) = temp_repo();
    std::fs::write(dir.path().join("raw.service"), "raw-bytes").unwrap();
    assert_eq!(
        repo.read_file(&dir.path().join("raw.service")).unwrap(),
        "raw-bytes"
    );
}
