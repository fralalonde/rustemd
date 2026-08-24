//! Programmatic control API — the library alternative to the `systemctl`
//! CLI and to D-Bus.
//!
//! The [`Control`] trait is the single entry point for driving a rustemd
//! manager. It has two implementations, so callers can hold a
//! `&mut dyn Control` and not care where the manager lives:
//!
//! - [`Manager`](crate::manager::Manager) — control an in-process manager
//!   directly (no IPC, no subprocess, no D-Bus).
//! - [`SocketClient`] — control a running manager daemon over the same
//!   JSON-over-unix-socket protocol the `systemctl` CLI uses.
//!
//! ```no_run
//! use rustemd::control::Control;
//! use rustemd::control::SocketClient;
//!
//! # fn demo() -> Result<(), rustemd::control::Error> {
//! let mut ctl: Box<dyn Control> = Box::new(SocketClient::for_mode(true)?);
//! ctl.start(&["my-service"])?;
//! let st = ctl.status(&["my-service"])?;
//! println!("{:?}", st);
//! ctl.stop(&["my-service"])?;
//! # Ok(()) }
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::manager::Manager;

/// Control-operation error. Wraps a human-readable message (typically sourced
/// from a unit error or an IPC transport failure).
#[derive(Debug, Clone)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error(s.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error(e.to_string())
    }
}

// ---- typed data -----------------------------------------------------------------

/// Status of one unit (`systemctl status`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitStatus {
    pub name: String,
    pub description: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub result: String,
    pub main_pid: Option<i32>,
    pub path: Option<String>,
    pub active_enter: Option<u64>,
    pub log: Vec<String>,
    pub enabled: String,
}

/// One row of `systemctl list-units`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitSummary {
    pub unit: String,
    #[serde(rename = "loaded")]
    pub loaded: String,
    pub active: String,
    pub sub: String,
    pub description: String,
}

/// One row of `systemctl list-timers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerInfo {
    pub unit: String,
    pub activates: String,
    pub next: Option<u64>,
    pub next_left: Option<i64>,
    pub last: Option<u64>,
    pub last_passed: Option<i64>,
    pub spec: Vec<String>,
}

/// One row of `systemctl list-unit-files`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitFileInfo {
    pub file: String,
    pub path: String,
    pub state: String,
}

/// One unit-file from `systemctl cat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatEntry {
    pub unit: String,
    pub path: String,
    pub text: String,
}

// ---- the trait ------------------------------------------------------------------

/// Core control surface of a manager. Implemented in-process by
/// [`Manager`] and remotely by [`SocketClient`].
///
/// All unit names are normalized the way `systemctl` does: a bare name with
/// no `.` suffix gets `.service` appended.
pub trait Control {
    // -- lifecycle (mutating) --
    fn start(&mut self, units: &[&str]) -> Result<(), Error>;
    fn stop(&mut self, units: &[&str]) -> Result<(), Error>;
    fn restart(&mut self, units: &[&str]) -> Result<(), Error>;
    fn reload(&mut self, units: &[&str]) -> Result<(), Error>;
    fn kill(&mut self, unit: &str, signal: &str) -> Result<(), Error>;
    fn reload_daemon(&mut self) -> Result<(), Error>;
    fn isolate(&mut self, unit: &str) -> Result<(), Error>;
    fn set_default(&mut self, unit: &str) -> Result<(), Error>;

    // -- enable/disable (mutating, filesystem) --
    fn enable(&mut self, units: &[&str]) -> Result<Vec<String>, Error>;
    fn disable(&mut self, units: &[&str]) -> Result<Vec<String>, Error>;

    // -- queries (read-only) --
    fn status(&self, units: &[&str]) -> Result<Vec<UnitStatus>, Error>;
    fn list_units(&self, types: &[&str], state: Option<&str>) -> Result<Vec<UnitSummary>, Error>;
    fn list_timers(&self) -> Result<Vec<TimerInfo>, Error>;
    fn list_unit_files(&self) -> Result<Vec<UnitFileInfo>, Error>;
    fn is_enabled(&self, units: &[&str]) -> Result<Vec<String>, Error>;
    fn is_active(&self, units: &[&str]) -> Result<Vec<String>, Error>;
    fn get_default(&self) -> Result<String, Error>;
}

// ---- in-process implementation (Manager) ----------------------------------------

impl Control for Manager {
    fn start(&mut self, units: &[&str]) -> Result<(), Error> {
        for u in normalize(units) {
            self.start(&u).map_err(Error)?;
        }
        Ok(())
    }
    fn stop(&mut self, units: &[&str]) -> Result<(), Error> {
        for u in normalize(units) {
            self.stop(&u).map_err(Error)?;
        }
        Ok(())
    }
    fn restart(&mut self, units: &[&str]) -> Result<(), Error> {
        for u in normalize(units) {
            self.restart(&u).map_err(Error)?;
        }
        Ok(())
    }
    fn reload(&mut self, units: &[&str]) -> Result<(), Error> {
        for u in normalize(units) {
            self.reload(&u).map_err(Error)?;
        }
        Ok(())
    }
    fn kill(&mut self, unit: &str, signal: &str) -> Result<(), Error> {
        let sig = crate::unit::sig_from_name(signal)
            .ok_or_else(|| Error(format!("unknown signal `{signal}`")))?;
        self.kill(&crate::names::normalize_unit(unit), sig)
            .map_err(Error)
    }
    fn reload_daemon(&mut self) -> Result<(), Error> {
        let errs = self.load_all();
        if errs.is_empty() {
            Ok(())
        } else {
            Err(Error(errs.join("; ")))
        }
    }
    fn isolate(&mut self, unit: &str) -> Result<(), Error> {
        self.isolate(unit).map_err(Error)
    }
    fn set_default(&mut self, unit: &str) -> Result<(), Error> {
        self.set_default(unit).map_err(Error)
    }

    fn enable(&mut self, units: &[&str]) -> Result<Vec<String>, Error> {
        let mut msgs = Vec::new();
        for u in normalize(units) {
            msgs.extend(crate::enable::enable(&self.cfg.paths, &u).map_err(Error)?);
        }
        Ok(msgs)
    }
    fn disable(&mut self, units: &[&str]) -> Result<Vec<String>, Error> {
        let mut msgs = Vec::new();
        for u in normalize(units) {
            msgs.extend(crate::enable::disable(&self.cfg.paths, &u).map_err(Error)?);
        }
        Ok(msgs)
    }

    fn status(&self, units: &[&str]) -> Result<Vec<UnitStatus>, Error> {
        let names = query_names(self, units);
        Ok(names.iter().filter_map(|n| self.status_of(n)).collect())
    }
    fn list_units(&self, types: &[&str], state: Option<&str>) -> Result<Vec<UnitSummary>, Error> {
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        Ok(self.list_unit_summaries(&types, state, None))
    }
    fn list_timers(&self) -> Result<Vec<TimerInfo>, Error> {
        Ok(self.list_timer_info())
    }
    fn list_unit_files(&self) -> Result<Vec<UnitFileInfo>, Error> {
        Ok(self.list_unit_file_info())
    }
    fn is_enabled(&self, units: &[&str]) -> Result<Vec<String>, Error> {
        Ok(normalize(units)
            .iter()
            .map(|u| crate::enable::enabled_state(&self.cfg.paths, u))
            .collect())
    }
    fn is_active(&self, units: &[&str]) -> Result<Vec<String>, Error> {
        Ok(normalize(units)
            .iter()
            .map(|u| {
                self.units
                    .get(u)
                    .map(|x| crate::manager::ops::active_str(x.active).to_string())
                    .unwrap_or_else(|| "unknown".into())
            })
            .collect())
    }
    fn get_default(&self) -> Result<String, Error> {
        Ok(self.get_default())
    }
}

// ---- remote implementation (SocketClient) ---------------------------------------

/// A [`Control`] handle that talks to a running manager daemon over the
/// JSON-line unix socket (the same channel the `systemctl` CLI uses).
///
/// Each call opens a short-lived connection, sends one request, and reads one
/// response, so no persistent state is held and the handle is cheap to create.
pub struct SocketClient {
    socket: std::path::PathBuf,
}

impl SocketClient {
    /// Client for the system manager (`false`) or the user manager (`true`).
    pub fn for_mode(user: bool) -> Result<Self, Error> {
        let paths = if user {
            crate::paths::Paths::user().map_err(Error)?
        } else {
            crate::paths::Paths::system()
        };
        Ok(SocketClient {
            socket: paths.control_socket(),
        })
    }

    fn call(&self, req: serde_json::Value) -> Result<serde_json::Value, Error> {
        crate::client::request_json(&self.socket, &req).map_err(Error)
    }

    fn op(&self, op: &str, units: &[String]) -> Result<serde_json::Value, Error> {
        self.call(serde_json::json!({ "op": op, "units": units }))
    }

    fn op_name(&self, op: &str, name: &str) -> Result<serde_json::Value, Error> {
        self.call(serde_json::json!({ "op": op, "name": name }))
    }
}

impl Control for SocketClient {
    fn start(&mut self, units: &[&str]) -> Result<(), Error> {
        self.op("start", &normalize(units))?;
        Ok(())
    }
    fn stop(&mut self, units: &[&str]) -> Result<(), Error> {
        self.op("stop", &normalize(units))?;
        Ok(())
    }
    fn restart(&mut self, units: &[&str]) -> Result<(), Error> {
        self.op("restart", &normalize(units))?;
        Ok(())
    }
    fn reload(&mut self, units: &[&str]) -> Result<(), Error> {
        self.op("reload", &normalize(units))?;
        Ok(())
    }
    fn kill(&mut self, unit: &str, signal: &str) -> Result<(), Error> {
        let unit = crate::names::normalize_unit(unit);
        self.call(serde_json::json!({ "op": "kill", "units": [unit], "signal": signal }))?;
        Ok(())
    }
    fn reload_daemon(&mut self) -> Result<(), Error> {
        self.call(serde_json::json!({ "op": "daemon_reload" }))?;
        Ok(())
    }
    fn isolate(&mut self, unit: &str) -> Result<(), Error> {
        self.op_name("isolate", &crate::names::normalize_unit(unit))?;
        Ok(())
    }
    fn set_default(&mut self, unit: &str) -> Result<(), Error> {
        self.op_name("set_default", &crate::names::normalize_unit(unit))?;
        Ok(())
    }

    fn enable(&mut self, units: &[&str]) -> Result<Vec<String>, Error> {
        let v = self.op("enable", &normalize(units))?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }
    fn disable(&mut self, units: &[&str]) -> Result<Vec<String>, Error> {
        let v = self.op("disable", &normalize(units))?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn status(&self, units: &[&str]) -> Result<Vec<UnitStatus>, Error> {
        let v = self.op("status", &normalize(units))?;
        serde_json::from_value(v).map_err(|e| Error(e.to_string()))
    }
    fn list_units(&self, types: &[&str], state: Option<&str>) -> Result<Vec<UnitSummary>, Error> {
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let v = self.call(serde_json::json!({
            "op": "list_units", "types": types, "state": state, "pattern": null
        }))?;
        serde_json::from_value(v).map_err(|e| Error(e.to_string()))
    }
    fn list_timers(&self) -> Result<Vec<TimerInfo>, Error> {
        let v = self.call(serde_json::json!({ "op": "list_timers" }))?;
        serde_json::from_value(v).map_err(|e| Error(e.to_string()))
    }
    fn list_unit_files(&self) -> Result<Vec<UnitFileInfo>, Error> {
        let v = self.call(serde_json::json!({ "op": "list_unit_files" }))?;
        serde_json::from_value(v).map_err(|e| Error(e.to_string()))
    }
    fn is_enabled(&self, units: &[&str]) -> Result<Vec<String>, Error> {
        let v = self.op("is_enabled", &normalize(units))?;
        serde_json::from_value(v).map_err(|e| Error(e.to_string()))
    }
    fn is_active(&self, units: &[&str]) -> Result<Vec<String>, Error> {
        let v = self.op("is_active", &normalize(units))?;
        let states: Vec<String> = v
            .get("states")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        x.as_array()
                            .and_then(|p| p.get(1))
                            .and_then(|s| s.as_str())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(states)
    }
    fn get_default(&self) -> Result<String, Error> {
        let v = self.call(serde_json::json!({ "op": "get_default" }))?;
        Ok(v.as_str().unwrap_or("default.target").to_string())
    }
}

// ---- helpers ---------------------------------------------------------------------

fn normalize(units: &[&str]) -> Vec<String> {
    units
        .iter()
        .map(|u| crate::names::normalize_unit(u))
        .collect()
}

fn query_names(mgr: &Manager, units: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = normalize(units);
    if names.is_empty() {
        let mut all: Vec<String> = mgr.units.keys().cloned().collect();
        all.sort();
        names = all;
    }
    names
}
