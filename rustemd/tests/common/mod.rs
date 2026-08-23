//! Shared helpers for integration tests: run the real manager against a
//! scratch filesystem by pointing the `RUSTEMD_*` env hooks at a temp dir.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Serializes env-var mutation across tests within this binary (tests run in
/// parallel threads, and `RUSTEMD_*` are process-global).
static ENV_LOCK: Mutex<()> = Mutex::new(());

pub struct Scratch {
    _lock: std::sync::MutexGuard<'static, ()>,
    /// The temp dir (kept alive for the lifetime of this guard).
    pub dir: tempfile::TempDir,
}

impl Scratch {
    pub fn new() -> Scratch {
        let lock = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let units = root.join("units");
        let config = root.join("config");
        let run = root.join("run");
        for d in [&units, &config, &run] {
            std::fs::create_dir_all(d).unwrap();
        }
        // SAFETY: guarded by ENV_LOCK, so no other test in this process is
        // reading these vars while we set them.
        unsafe {
            std::env::set_var("RUSTEMD_UNIT_PATH", &units);
            std::env::set_var("RUSTEMD_CONFIG_DIR", &config);
            std::env::set_var("RUSTEMD_RUNTIME_DIR", &run);
            std::env::set_var("RUSTEMD_SOCKET", run.join("control.sock"));
        }
        Scratch { _lock: lock, dir }
    }

    /// Path to the unit-file directory.
    pub fn units(&self) -> std::path::PathBuf {
        self.dir.path().join("units")
    }

    /// Write a unit file into the scratch unit directory.
    pub fn write_unit(&self, name: &str, body: &str) {
        std::fs::write(self.units().join(name), body).unwrap();
    }
}

/// Poll `f` until it returns true or `timeout` elapses.
pub fn wait_for<F: FnMut() -> bool>(timeout: Duration, mut f: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}
