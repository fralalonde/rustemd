//! D-Bus integration test: spin up a private session bus, run the manager with
//! its D-Bus bridge, and drive the `org.freedesktop.systemd1` surface through a
//! real zbus client. Self-skips when `dbus-daemon` is unavailable (e.g. a
//! dbus-broker-only host); CI installs the `dbus` package so it runs for real.
#![cfg(all(target_os = "linux", feature = "dbus"))]

mod common;

use std::time::Duration;

use common::{Daemon, Scratch, wait_for};

/// A private session bus, held as a foreground child process. The address is
/// read from the daemon's stdout; the child is killed on drop.
struct Bus {
    child: std::process::Child,
    address: String,
}

impl Bus {
    fn start() -> Option<Bus> {
        let mut child = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address", "--nopidfile"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(child.stdout.take()?);
        let mut address = String::new();
        reader.read_line(&mut address).ok()?;
        let address = address.trim().to_string();
        if address.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(Bus { child, address })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One `ListUnits` row, in systemd1 `UnitInfo` wire order:
/// `(name, description, load, active, sub, following, object_path, job_id,
/// job_type, job_object_path)` = `(ssssssouso)`.
type UnitInfo = (
    String,
    String,
    String,
    String,
    String,
    String,
    zbus::zvariant::OwnedObjectPath,
    u32,
    String,
    zbus::zvariant::OwnedObjectPath,
);

#[test]
fn systemd1_manager_surface_is_live_over_bus() {
    let Some(bus) = Bus::start() else {
        eprintln!("skipping: dbus-daemon not available");
        return;
    };

    // Point both the manager's monitor thread and our own client at the bus.
    // The manager runs in *system* mode (for_mode(false)), so its monitor
    // thread connects via Connection::system() (DBUS_SYSTEM_BUS_ADDRESS), while
    // the test client uses the session bus. Point both at the same private
    // daemon.
    // SAFETY: single test in this binary; the env vars are set before the
    // manager thread (which reads them via Connection) is spawned.
    unsafe {
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.address);
        std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &bus.address);
    }

    let scratch = Scratch::new();
    scratch.write_unit(
        "hello.service",
        "[Unit]\nDescription=hello\n[Service]\nType=oneshot\nExecStart=/bin/true\n",
    );

    let daemon = Daemon::start_with_dbus();

    // The manager thread runs udev_init (device enumeration) *before*
    // start_dbus, so the systemd1 name is only owned some seconds in. Wait for
    // the manager to be up, then give its monitor thread time to register.
    assert!(
        wait_for(Duration::from_secs(15), || std::path::Path::new(
            &daemon.socket
        )
        .exists()),
        "manager should come up"
    );

    // The test-side client on the same bus.
    let conn = zbus::blocking::Connection::session().unwrap();
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
    )
    .unwrap();

    // Version property — proves the systemd1 surface is live and reachable.
    let live = wait_for(Duration::from_secs(20), || {
        proxy
            .get_property::<zbus::zvariant::OwnedValue>("Version")
            .is_ok()
    });
    assert!(live, "systemd1 Manager Version should be readable");

    let ver = proxy
        .get_property::<zbus::zvariant::OwnedValue>("Version")
        .unwrap();
    match &*ver {
        zbus::zvariant::Value::Str(s) => assert_eq!(s.as_str(), rystemd::VERSION),
        other => panic!("Version should be a string, got {other:?}"),
    }

    // ListUnits — round-trips through the manager over the bus.
    let rows: Vec<UnitInfo> = proxy.call("ListUnits", &()).unwrap();
    assert!(!rows.is_empty(), "ListUnits should list the loaded units");
    assert!(
        rows.iter().any(|r| r.0 == "hello.service"),
        "hello.service should be listed by systemd1 ListUnits"
    );

    drop(daemon);
}
