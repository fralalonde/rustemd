//! D-Bus integration (Linux only): `Type=dbus`/`BusName=` service activation
//! and a small manager control interface.
//!
//! # Threading model
//!
//! The manager's poll loop is single-threaded and synchronous, so **no**
//! D-Bus work happens there. All of it lives on dedicated threads, bridged to
//! the manager over `std::sync::mpsc` channels that the loop drains each
//! iteration (bounded to ≤ ~1s by the poll-timeout cap):
//!
//! - A *monitor thread* owns a blocking [`zbus::blocking::Connection`] (system
//!   bus in system mode, session bus in user mode). It serves the manager
//!   interface, owns the well-known name, and polls `org.freedesktop.DBus`
//!   `NameHasOwner` for the `BusName=` of every activating `Type=dbus` unit,
//!   forwarding acquisitions to the manager.
//! - The interface's method handlers run on zbus's internal executor threads;
//!   each forwards a request to the manager over a channel and blocks for the
//!   reply, so the manager stays the single writer of unit state.
//!
//! zbus is pulled in with `default-features = false` and just
//! `["blocking-api", "async-io"]`, keeping the dependency tree pure-Rust with
//! no C compiler and no tokio.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::time::Duration;

use zbus::interface;

use crate::log::mgr_log;

/// Well-known bus name rustemd owns on the (system or session) bus.
const BUS_NAME: &str = "org.rustemd.Manager1";
/// Object path the manager interface is exposed at.
const OBJECT_PATH: &str = "/org/rustemd/Manager1";

/// One unit's listing row, as returned over the bus.
#[derive(Debug, Clone)]
pub struct UnitEntry {
    pub name: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub description: String,
}

/// A unit's `(name, load, active, sub, description)` tuple on the wire.
pub type UnitRow = (String, String, String, String, String);

/// A control operation routed from a D-Bus method handler to the manager.
#[derive(Debug)]
pub enum DbOp {
    ListUnits,
    GetUnit(String),
    StartUnit(String),
    StopUnit(String),
}

/// A request from a D-Bus method handler to the manager. Carries its own
/// reply channel so concurrent method calls cannot race on a shared receiver.
pub struct DbRequest {
    pub reply: SyncSender<DbReply>,
    pub op: DbOp,
}

/// The manager's reply to a D-Bus method handler.
#[derive(Debug)]
pub enum DbReply {
    UnitList(Vec<UnitEntry>),
    Unit(Option<UnitEntry>),
    UnitStarted,
    UnitStopped,
    Error(String),
}

/// Name-ownership events forwarded from the monitor thread to the manager.
#[derive(Debug)]
pub enum DbEvent {
    NameAcquired(String),
    NameLost(String),
}

/// Manager → monitor thread: which bus names to watch.
#[derive(Debug)]
pub enum DbWatch {
    Add(String),
    Remove(String),
}

/// The manager-side handle to the D-Bus bridge: channels to drain plus the
/// sender used to register/unregister name watches.
pub struct DbusHandle {
    pub events: Receiver<DbEvent>,
    pub commands: Receiver<DbRequest>,
    pub watch_tx: Sender<DbWatch>,
}

/// Start the D-Bus bridge on a dedicated thread and return the manager-side
/// handle. The connection itself happens on the thread; if the bus is
/// unreachable the thread logs a warning and exits, leaving the manager fully
/// functional without D-Bus (a `Type=dbus` unit then times out, matching
/// systemd's behaviour when the bus is absent).
pub fn spawn(user: bool) -> DbusHandle {
    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (watch_tx, watch_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rustemd-dbus".into())
        .spawn(move || run_thread(user, event_tx, cmd_tx, watch_rx))
        .ok();
    DbusHandle {
        events: event_rx,
        commands: cmd_rx,
        watch_tx,
    }
}

/// The manager interface: methods are CamelCased on the wire (`ListUnits`,
/// `GetUnit`, `StartUnit`, `StopUnit`) and introspection is served
/// automatically by zbus. Blocking methods are called on zbus executor
/// threads; each one bridges to the manager and blocks for its reply.
struct ManagerIface {
    cmd_tx: Sender<DbRequest>,
}

#[interface(name = "org.rustemd.Manager1.Manager")]
impl ManagerIface {
    /// List loaded units: array of `(name, load, active, sub, description)`.
    fn list_units(&self) -> zbus::fdo::Result<Vec<UnitRow>> {
        match self.call(DbOp::ListUnits) {
            Ok(DbReply::UnitList(rows)) => Ok(rows
                .into_iter()
                .map(|r| (r.name, r.load, r.active, r.sub, r.description))
                .collect()),
            Ok(_) => Err(zbus::fdo::Error::Failed("unexpected reply".into())),
            Err(e) => Err(zbus::fdo::Error::Failed(e)),
        }
    }

    /// Look up one unit by name.
    fn get_unit(&self, name: String) -> zbus::fdo::Result<UnitRow> {
        match self.call(DbOp::GetUnit(name)) {
            Ok(DbReply::Unit(Some(r))) => Ok((r.name, r.load, r.active, r.sub, r.description)),
            Ok(DbReply::Unit(None)) => Err(zbus::fdo::Error::Failed("no such unit".into())),
            Ok(_) => Err(zbus::fdo::Error::Failed("unexpected reply".into())),
            Err(e) => Err(zbus::fdo::Error::Failed(e)),
        }
    }

    /// Start a unit.
    fn start_unit(&self, name: String) -> zbus::fdo::Result<()> {
        match self.call(DbOp::StartUnit(name)) {
            Ok(DbReply::UnitStarted) => Ok(()),
            Ok(DbReply::Error(e)) => Err(zbus::fdo::Error::Failed(e)),
            Ok(_) => Err(zbus::fdo::Error::Failed("unexpected reply".into())),
            Err(e) => Err(zbus::fdo::Error::Failed(e)),
        }
    }

    /// Stop a unit.
    fn stop_unit(&self, name: String) -> zbus::fdo::Result<()> {
        match self.call(DbOp::StopUnit(name)) {
            Ok(DbReply::UnitStopped) => Ok(()),
            Ok(DbReply::Error(e)) => Err(zbus::fdo::Error::Failed(e)),
            Ok(_) => Err(zbus::fdo::Error::Failed("unexpected reply".into())),
            Err(e) => Err(zbus::fdo::Error::Failed(e)),
        }
    }

    /// Manager version string.
    #[zbus(property)]
    fn version(&self) -> String {
        crate::VERSION.to_string()
    }
}

impl ManagerIface {
    /// Send a request to the manager and block for its reply (with a timeout,
    /// so a wedged manager can't hang a D-Bus worker thread forever).
    fn call(&self, op: DbOp) -> Result<DbReply, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.cmd_tx
            .send(DbRequest {
                reply: reply_tx,
                op,
            })
            .map_err(|_| "manager channel closed".to_string())?;
        reply_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "manager did not reply (timeout)".to_string())
    }
}

/// Does the bus currently have an owner for `name`? `None` on lookup error.
fn name_has_owner(proxy: &zbus::blocking::fdo::DBusProxy<'_>, name: &str) -> Option<bool> {
    let bus_name = zbus::names::BusName::try_from(name).ok()?;
    proxy.name_has_owner(bus_name).ok()
}

/// The monitor thread body: connect, serve the interface, request the name,
/// then poll name ownership and forward transitions to the manager.
fn run_thread(
    user: bool,
    event_tx: Sender<DbEvent>,
    cmd_tx: Sender<DbRequest>,
    watch_rx: Receiver<DbWatch>,
) {
    let bus = if user { "session" } else { "system" };
    let conn = match if user {
        zbus::blocking::Connection::session()
    } else {
        zbus::blocking::Connection::system()
    } {
        Ok(c) => c,
        Err(e) => {
            mgr_log(&format!("D-Bus: failed to connect to {bus} bus: {e}"));
            return;
        }
    };

    if let Err(e) = conn.object_server().at(
        OBJECT_PATH,
        ManagerIface {
            cmd_tx: cmd_tx.clone(),
        },
    ) {
        mgr_log(&format!("D-Bus: failed to register manager interface: {e}"));
        return;
    }
    if let Err(e) = conn.request_name(BUS_NAME) {
        mgr_log(&format!("D-Bus: failed to request name {BUS_NAME}: {e}"));
        return;
    }
    mgr_log(&format!(
        "D-Bus: manager interface live at {BUS_NAME} ({bus} bus)"
    ));

    let proxy = match zbus::blocking::fdo::DBusProxy::new(&conn) {
        Ok(p) => p,
        Err(e) => {
            mgr_log(&format!("D-Bus: failed to create DBus proxy: {e}"));
            return;
        }
    };

    // name -> whether it was owned on the previous poll tick.
    let mut watched: HashMap<String, bool> = HashMap::new();

    loop {
        // Drain name-watch add/remove requests from the manager.
        while let Ok(w) = watch_rx.try_recv() {
            match w {
                DbWatch::Add(name) => {
                    // Check immediately to close the race where the service
                    // acquires the name before we register the watch.
                    if let Some(now) = name_has_owner(&proxy, &name) {
                        if now {
                            let _ = event_tx.send(DbEvent::NameAcquired(name.clone()));
                        }
                        watched.insert(name, now);
                    }
                }
                DbWatch::Remove(name) => {
                    watched.remove(&name);
                }
            }
        }

        // Poll ownership transitions (false -> true).
        let mut acquired: Vec<String> = Vec::new();
        for (name, was_owned) in watched.iter_mut() {
            if let Some(now) = name_has_owner(&proxy, name) {
                if now && !*was_owned {
                    acquired.push(name.clone());
                }
                *was_owned = now;
            }
        }
        for name in acquired {
            let _ = event_tx.send(DbEvent::NameAcquired(name));
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}
