//! JSON-line control protocol between the `systemctl`-style CLI
//! (and [`crate::control::SocketClient`]) and the manager.
//!
//! Each request is one JSON object per line; each response is one JSON object
//! per line:
//!
//! - request: `{"op": "...", ...args}`
//! - response: `{"ok": true, "data": {...}}` or `{"ok": false, "error": "..."}`
//!
//! This module is intentionally thin: it translates JSON into the typed
//! operations on [`Manager`] (see [`crate::manager::ops`]) and back. All the
//! real logic lives in the manager, so the wire format and the programmatic
//! [`Control`](crate::control::Control) API can never drift apart.

use serde_json::{Value, json};

use crate::manager::Manager;
use crate::manager::ops::active_str;
use crate::manager::state::ActiveState;

/// Dispatch one request line against the manager. Returns the response JSON.
pub fn dispatch(mgr: &mut Manager, line: &str) -> Value {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "error": format!("bad request: {e}")}),
    };
    let op = req.get("op").and_then(Value::as_str);
    match run_op(mgr, op, &req) {
        Ok(d) => json!({"ok": true, "data": d}),
        Err(e) => json!({"ok": false, "error": e}),
    }
}

fn req_units(req: &Value) -> Vec<String> {
    req.get("units")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(crate::names::normalize_unit))
                .collect()
        })
        .unwrap_or_default()
}

fn req_str<'a>(req: &'a Value, key: &str) -> Option<&'a str> {
    req.get(key).and_then(Value::as_str)
}

fn run_op(mgr: &mut Manager, op: Option<&str>, req: &Value) -> Result<Value, String> {
    let op = op.ok_or("missing op")?;
    match op {
        "start" => {
            for u in req_units(req) {
                if crate::enable::is_masked(&mgr.cfg.paths, &u) {
                    return Err(format!("Unit {u} is masked."));
                }
                mgr.start(&u)?;
            }
            Ok(Value::Null)
        }
        "stop" => {
            for u in req_units(req) {
                mgr.stop(&u)?;
            }
            Ok(Value::Null)
        }
        "restart" => {
            for u in req_units(req) {
                mgr.restart(&u)?;
            }
            Ok(Value::Null)
        }
        "try_restart" => {
            mgr.try_restart_units(&req_units(req))?;
            Ok(Value::Null)
        }
        "reset_failed" => {
            mgr.reset_failed_units(&req_units(req))?;
            Ok(Value::Null)
        }
        "clean" => Ok(json!(mgr.clean_units(&req_units(req)))),
        "list_dependencies" => {
            let name = req_str(req, "name").ok_or("list_dependencies: missing name")?;
            let reverse = req.get("reverse").and_then(Value::as_bool).unwrap_or(false);
            Ok(json!(mgr.list_dependencies(name, reverse)))
        }
        "reload" => {
            for u in req_units(req) {
                mgr.reload(&u)?;
            }
            Ok(Value::Null)
        }
        "kill" => {
            let u = req_units(req);
            let u = u.first().ok_or("kill: no unit")?.clone();
            let sig = req_str(req, "signal")
                .and_then(crate::unit::sig_from_name)
                .unwrap_or(crate::platform::signal::Signal::SIGTERM);
            mgr.kill(&u, sig)?;
            Ok(Value::Null)
        }
        "status" => {
            let units = req_units(req);
            let names: Vec<String> = if units.is_empty() {
                let mut n: Vec<String> = mgr.units.keys().cloned().collect();
                n.sort();
                n
            } else {
                units
            };
            let out: Vec<_> = names.iter().filter_map(|n| mgr.status_of(n)).collect();
            Ok(json!(out))
        }
        "is_active" => {
            let mut states = Vec::new();
            for u in req_units(req) {
                if let Some(x) = mgr.units.get(&u) {
                    states.push((u, active_str(x.active).to_string()));
                } else {
                    return Err(format!("Unit {u} not found."));
                }
            }
            let worst: i32 = states
                .iter()
                .map(|(_, s)| match s.as_str() {
                    "failed" => 4,
                    "inactive" => 3,
                    _ => 0,
                })
                .max()
                .unwrap_or(0);
            Ok(json!({"states": states, "exit": worst}))
        }
        "is_failed" => {
            for u in req_units(req) {
                let x = mgr
                    .units
                    .get(&u)
                    .ok_or_else(|| format!("Unit {u} not found."))?;
                if x.active != ActiveState::Failed {
                    return Ok(json!([false]));
                }
            }
            Ok(json!([true]))
        }
        "list_units" => {
            let types: Vec<String> = req
                .get("types")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let state = req_str(req, "state");
            let pattern = req_str(req, "pattern");
            Ok(json!(mgr.list_unit_summaries(&types, state, pattern)))
        }
        "list_unit_files" => Ok(json!(mgr.list_unit_file_info())),
        "list_timers" => Ok(json!(mgr.list_timer_info())),
        "cat" => {
            let units = req_units(req);
            Ok(json!(mgr.cat(&units)?))
        }
        "show" => {
            let units = req_units(req);
            let props: Vec<String> = req
                .get("properties")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Ok(json!(mgr.show(&units, &props)?))
        }
        "daemon_reload" => {
            let errs = mgr.load_all();
            if errs.is_empty() {
                Ok(Value::Null)
            } else {
                Err(errs.join("; "))
            }
        }
        "get_default" => Ok(json!(mgr.get_default())),
        "repo" => Ok(json!(mgr.repo_info())),
        "journal" => {
            let unit = req_str(req, "unit");
            let since = req.get("since").and_then(Value::as_u64);
            let tail = req.get("tail").and_then(Value::as_u64).map(|v| v as usize);
            let mut records = Vec::new();
            match &unit {
                Some(u) => records.extend(mgr.journal.read(u, since)),
                None => {
                    for u in mgr.journal.units() {
                        records.extend(mgr.journal.read(&u, since));
                    }
                }
            }
            records.sort_by_key(|r| r.secs);
            if let Some(n) = tail {
                records = records.into_iter().rev().take(n).collect();
                records.reverse();
            }
            Ok(json!({
                "records": records,
                "dir": mgr.journal.dir().display().to_string(),
            }))
        }
        "set_default" => {
            let name = req_str(req, "name").ok_or("set_default: missing name")?;
            mgr.set_default(name)?;
            Ok(Value::Null)
        }
        "isolate" => {
            let name = req_str(req, "name").ok_or("isolate: missing name")?;
            mgr.isolate(name)?;
            Ok(Value::Null)
        }
        "enable" => {
            let mut msgs = Vec::new();
            for u in req_units(req) {
                msgs.extend(crate::enable::enable(&mgr.cfg.paths, &u)?);
            }
            Ok(json!(msgs))
        }
        "disable" => {
            let mut msgs = Vec::new();
            for u in req_units(req) {
                msgs.extend(crate::enable::disable(&mgr.cfg.paths, &u)?);
            }
            Ok(json!(msgs))
        }
        "mask" => {
            mgr.mask_units(&req_units(req))?;
            Ok(Value::Null)
        }
        "unmask" => {
            mgr.unmask_units(&req_units(req))?;
            Ok(Value::Null)
        }
        "reenable" => Ok(json!(mgr.reenable_units(&req_units(req))?)),
        "is_enabled" => {
            let states: Vec<String> = req_units(req)
                .iter()
                .map(|u| crate::enable::enabled_state(&mgr.cfg.paths, u))
                .collect();
            Ok(json!(states))
        }
        "is_system_running" => {
            let degraded = mgr.units.values().any(|u| u.active == ActiveState::Failed);
            Ok(json!({"running": !degraded, "degraded": degraded}))
        }
        "shutdown" => {
            mgr.shutdown();
            Ok(Value::Null)
        }
        _ => Err(format!("unknown op `{op}`")),
    }
}
