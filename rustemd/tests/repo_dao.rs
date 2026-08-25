#![cfg(unix)]

//! Integration test: prove the daemon LISTS and READS units through the
//! `rustemd-repo` DAO, and that the `repo` control op lets a client discover
//! and reopen the same repository.
//!
//! The unit file is written with `rustemd_repo::Repo` (the same way a client
//! would edit it); the daemon then discovers and loads it, and reports the
//! repository path over IPC.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use rustemd::control::{Control, SocketClient};

/// Serializes `RUSTEMD_*` env mutation across tests in this binary.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Scratch {
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Scratch {
        let lock = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let units = root.join("units");
        let config = root.join("config");
        let run = root.join("run");
        for d in [&units, &config, &run] {
            std::fs::create_dir_all(d).unwrap();
        }
        unsafe {
            std::env::set_var("RUSTEMD_UNIT_PATH", &units);
            std::env::set_var("RUSTEMD_CONFIG_DIR", &config);
            std::env::set_var("RUSTEMD_RUNTIME_DIR", &run);
            std::env::set_var("RUSTEMD_SOCKET", run.join("control.sock"));
        }
        Scratch { _lock: lock, dir }
    }

    fn units(&self) -> std::path::PathBuf {
        self.dir.path().join("units")
    }
}

fn wait_for<F: FnMut() -> bool>(timeout: Duration, mut f: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

struct Daemon {
    handle: Option<std::thread::JoinHandle<()>>,
    socket: std::path::PathBuf,
}

impl Daemon {
    fn start() -> Daemon {
        let socket: std::path::PathBuf = std::env::var_os("RUSTEMD_SOCKET").unwrap().into();
        let handle = std::thread::spawn(|| {
            let mut mgr = rustemd::manager::Manager::new(
                rustemd::manager::ManagerCfg::for_mode(false).unwrap(),
            )
            .unwrap();
            mgr.load_all();
            #[cfg(all(target_os = "linux", feature = "udev"))]
            mgr.udev_init();
            mgr.bind_ipc().unwrap();
            mgr.bind_notify().ok();
            mgr.setup_signals();
            mgr.run();
        });
        Daemon {
            handle: Some(handle),
            socket,
        }
    }

    fn client(&self) -> SocketClient {
        SocketClient::for_mode(false).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ =
            rustemd::client::request_json(&self.socket, &serde_json::json!({ "op": "shutdown" }));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[test]
fn daemon_lists_and_reads_units_through_repo_dao() {
    let scratch = Scratch::new();

    // Write the unit through the repository crate, exactly as a client would.
    let repo = rustemd_repo::Repo::open_roots(vec![scratch.units()]).unwrap();
    repo.write(
        "dao.service",
        "[Unit]\nDescription=DAO unit\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
    )
    .unwrap();

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));

    let ctl = daemon.client();

    // The daemon reports its repository over IPC.
    let info = ctl.repo().unwrap();
    assert_eq!(info.root, scratch.units().display().to_string());
    assert_eq!(info.backend, "dir");
    assert_eq!(info.roots, vec![scratch.units().display().to_string()]);
    assert_eq!(info.git_head, None);

    // The daemon LISTED the unit (discovery goes through the DAO).
    let units = ctl.list_units(&[], None).unwrap();
    assert!(
        units.iter().any(|u| u.unit == "dao.service"),
        "daemon should list the DAO-written unit"
    );

    // The daemon READ the unit (load/parse goes through the DAO).
    let status = ctl.status(&["dao.service"]).unwrap();
    assert_eq!(status[0].name, "dao.service");
    assert_eq!(status[0].description, "DAO unit");

    // A client can reopen the reported repository with the same crate.
    let client_repo = rustemd_repo::Repo::open(std::path::PathBuf::from(&info.root)).unwrap();
    assert_eq!(
        client_repo.read("dao.service").unwrap(),
        Some(
            "[Unit]\nDescription=DAO unit\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n"
                .to_string()
        )
    );
    assert!(
        client_repo
            .list()
            .unwrap()
            .iter()
            .any(|u| u.name == "dao.service")
    );
}
