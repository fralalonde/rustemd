#![cfg(windows)]

use std::process::Command;
use std::time::{Duration, Instant};

fn wait_for(timeout: Duration, mut test: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if test() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    test()
}

#[test]
fn cli_drives_windows_user_manager_over_named_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let units = dir.path().join("units");
    let config = dir.path().join("config");
    let runtime = dir.path().join("runtime");
    for path in [&units, &config, &runtime] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(
        units.join("cli.service"),
        "[Unit]\nDescription=Windows CLI service\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=cmd.exe /D /C exit 0\n",
    ).unwrap();
    let pipe = format!(r"\\.\pipe\rustemd-cli-test-{}", std::process::id());
    unsafe {
        std::env::set_var("RUSTEMD_UNIT_PATH", &units);
        std::env::set_var("RUSTEMD_CONFIG_DIR", &config);
        std::env::set_var("RUSTEMD_RUNTIME_DIR", &runtime);
        std::env::set_var("RUSTEMD_SOCKET", &pipe);
    }

    let manager = std::thread::spawn(|| {
        let mut manager =
            rustemd::manager::Manager::new(rustemd::manager::ManagerCfg::for_mode(true).unwrap())
                .unwrap();
        manager.load_all();
        manager.bind_ipc().unwrap();
        manager.setup_signals();
        manager.run();
    });
    let client = rustemd::client::Client::for_mode(true).unwrap();
    assert!(wait_for(Duration::from_secs(5), || client
        .simple_op("list_units")
        .is_ok()));

    let binary = env!("CARGO_BIN_EXE_rustemctl");
    let start = Command::new(binary)
        .args(["--user", "start", "cli.service"])
        .env("RUSTEMD_SOCKET", &pipe)
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );

    let status = Command::new(binary)
        .args(["--user", "status", "cli.service"])
        .env("RUSTEMD_SOCKET", &pipe)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("Windows CLI service"));
    assert!(stdout.contains("active"));

    client.simple_op("shutdown").unwrap();
    manager.join().unwrap();
}
