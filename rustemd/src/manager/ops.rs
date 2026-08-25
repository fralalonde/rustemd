//! Typed operations over the manager — the single implementation shared by the
//! JSON IPC surface ([`crate::ipc`]) and the programmatic
//! [`Control`](crate::control::Control) API.
//!
//! Keeping the read queries here (rather than inline in the IPC match) means
//! the CLI, the in-process library API, and the socket client all see the same
//! data and the same semantics.

use serde_json::{Value, json};

use crate::control::{CatEntry, TimerInfo, UnitFileInfo, UnitStatus, UnitSummary};
use crate::enable;
use crate::manager::Manager;
use crate::manager::state::{ActiveState, LoadState};
use crate::unit::UnitKind;

/// `LoadState` as the stable wire/CLI string.
pub(crate) fn load_str(l: LoadState) -> &'static str {
    match l {
        LoadState::Loaded => "loaded",
        LoadState::NotFound => "not-found",
        LoadState::Error => "error",
    }
}

/// `ActiveState` as the stable wire/CLI string.
pub(crate) fn active_str(a: ActiveState) -> &'static str {
    match a {
        ActiveState::Inactive => "inactive",
        ActiveState::Activating => "activating",
        ActiveState::Active => "active",
        ActiveState::Deactivating => "deactivating",
        ActiveState::Failed => "failed",
    }
}

fn epoch(t: std::time::SystemTime) -> Option<u64> {
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

impl Manager {
    /// Typed status snapshot of a single unit.
    pub fn status_of(&self, name: &str) -> Option<UnitStatus> {
        let u = self.units.get(name)?;
        Some(UnitStatus {
            name: u.name.clone(),
            description: u
                .file
                .as_ref()
                .map(|f| f.unit.description.clone())
                .unwrap_or_default(),
            load: load_str(u.load).to_string(),
            active: active_str(u.active).to_string(),
            sub: u.sub.as_str().to_string(),
            result: serde_json::to_value(u.result)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".into()),
            main_pid: u.main_pid,
            path: u.path.as_ref().map(|p| p.display().to_string()),
            active_enter: u.active_enter.and_then(epoch),
            log: u.log.snapshot(),
            enabled: enable::enabled_state(&self.cfg.paths, &u.name),
        })
    }

    /// `list-units`: loaded units, optionally filtered by type/state/pattern.
    pub fn list_unit_summaries(
        &self,
        types: &[String],
        state: Option<&str>,
        pattern: Option<&str>,
    ) -> Vec<UnitSummary> {
        let mut names: Vec<String> = self
            .units
            .iter()
            .filter(|(_, u)| u.load == LoadState::Loaded)
            .map(|(n, _)| n.clone())
            .collect();
        names.sort();

        let mut rows = Vec::new();
        for n in names {
            let u = &self.units[&n];
            if let Some(p) = pattern {
                let in_name = n.contains(p);
                let in_desc = u
                    .file
                    .as_ref()
                    .map(|f| f.unit.description.contains(p))
                    .unwrap_or(false);
                if !in_name && !in_desc {
                    continue;
                }
            }
            if !types.is_empty() && !types.iter().any(|t| n.ends_with(&format!(".{t}"))) {
                continue;
            }
            if let Some(st) = state {
                if active_str(u.active) != st {
                    continue;
                }
            }
            rows.push(UnitSummary {
                unit: n,
                loaded: load_str(u.load).to_string(),
                active: active_str(u.active).to_string(),
                sub: u.sub.as_str().to_string(),
                description: u
                    .file
                    .as_ref()
                    .map(|f| f.unit.description.clone())
                    .unwrap_or_default(),
            });
        }
        rows
    }

    /// `list-timers`: timer units with next/last trigger bookkeeping.
    pub fn list_timer_info(&self) -> Vec<TimerInfo> {
        let mut names: Vec<String> = self
            .units
            .iter()
            .filter(|(_, u)| u.kind == UnitKind::Timer && u.load == LoadState::Loaded)
            .map(|(n, _)| n.clone())
            .collect();
        names.sort();

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        names
            .into_iter()
            .filter_map(|n| {
                let u = &self.units[&n];
                let t = u.timer.as_ref()?;
                let next = t.next_display.and_then(epoch);
                let last = t.last_trigger.and_then(epoch);
                Some(TimerInfo {
                    unit: n,
                    activates: u.activated_unit(),
                    next,
                    next_left: next.map(|e| e.saturating_sub(now_secs) as i64),
                    last,
                    last_passed: last.map(|e| now_secs.saturating_sub(e) as i64),
                    spec: u
                        .timer_cfg()
                        .map(|t| t.on_calendar.iter().map(|c| c.to_string()).collect())
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    /// `list-unit-files`: unit files on disk and their enable state.
    pub fn list_unit_file_info(&self) -> Vec<UnitFileInfo> {
        let mut rows = Vec::new();
        for d in &self.cfg.paths.unit_path {
            let Ok(rd) = std::fs::read_dir(d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let Some(f) = p.file_name().and_then(|f| f.to_str()) else {
                    continue;
                };
                let f = f.to_string();
                if p.is_file()
                    && (f.ends_with(".service")
                        || f.ends_with(".timer")
                        || f.ends_with(".target")
                        || cfg!(feature = "socket") && f.ends_with(".socket"))
                {
                    rows.push(UnitFileInfo {
                        state: enable::enabled_state(&self.cfg.paths, &f),
                        file: f,
                        path: p.display().to_string(),
                    });
                }
            }
        }
        rows.sort_by(|a, b| a.file.cmp(&b.file));
        rows
    }

    /// `cat`: raw unit-file text for the given units.
    pub fn cat(&self, units: &[String]) -> Result<Vec<CatEntry>, String> {
        let mut out = Vec::new();
        for u in units {
            let unit = self
                .units
                .get(u)
                .ok_or_else(|| format!("Unit {u} not found."))?;
            match &unit.path {
                Some(p) => {
                    let text = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
                    out.push(CatEntry {
                        unit: u.clone(),
                        path: p.display().to_string(),
                        text,
                    });
                }
                None => out.push(CatEntry {
                    unit: u.clone(),
                    path: "(synthesized)".to_string(),
                    text: String::new(),
                }),
            }
        }
        Ok(out)
    }

    /// `show`: free-form property map per unit (subset of systemd's `show`).
    pub fn show(&self, units: &[String], props: &[String]) -> Result<Vec<Value>, String> {
        let names: Vec<String> = if units.is_empty() {
            let mut n: Vec<String> = self.units.keys().cloned().collect();
            n.sort();
            n
        } else {
            units.to_vec()
        };
        let mut out = Vec::new();
        for n in names {
            let Some(u) = self.units.get(&n) else {
                continue;
            };
            let mut map = serde_json::Map::new();
            map.insert("Id".into(), json!(n));
            map.insert("LoadState".into(), json!(load_str(u.load)));
            map.insert("ActiveState".into(), json!(active_str(u.active)));
            map.insert("SubState".into(), json!(u.sub.as_str()));
            map.insert("MainPID".into(), json!(u.main_pid.unwrap_or(0)));
            map.insert(
                "Description".into(),
                json!(
                    u.file
                        .as_ref()
                        .map(|f| f.unit.description.clone())
                        .unwrap_or_default()
                ),
            );
            map.insert(
                "UnitFileState".into(),
                json!(enable::enabled_state(&self.cfg.paths, &n)),
            );
            map.insert(
                "ActiveEnterTimestamp".into(),
                json!(
                    u.active_enter
                        .and_then(epoch)
                        .map(|e| e.to_string())
                        .unwrap_or_default()
                ),
            );
            let map: serde_json::Map<String, Value> = if props.is_empty() {
                map
            } else {
                map.into_iter()
                    .filter(|(k, _)| props.iter().any(|p| p == k))
                    .collect()
            };
            out.push(Value::Object(map));
        }
        Ok(out)
    }

    /// The default target (what `default.target` resolves to).
    pub fn get_default(&self) -> String {
        let target = self.cfg.paths.default_target();
        std::fs::read_link(&target)
            .ok()
            .and_then(|p| p.file_name().and_then(|f| f.to_str()).map(str::to_string))
            .unwrap_or_else(|| "default.target".to_string())
    }

    /// Point `default.target` at `name` and start it.
    pub fn set_default(&mut self, name: &str) -> Result<(), String> {
        let target = self.cfg.paths.default_target();
        let link = self
            .cfg
            .paths
            .find_unit(name)
            .ok_or_else(|| format!("unit {name} not found"))?;
        if let Some(p) = target.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let _ = std::fs::remove_file(&target);
        crate::platform::fs::link_file(&link, &target).map_err(|e| e.to_string())?;
        self.start(name).ok();
        Ok(())
    }

    /// `isolate`: stop everything not required by `name`, then start it.
    pub fn isolate(&mut self, name: &str) -> Result<(), String> {
        let name = crate::names::normalize_unit(name);
        let names: Vec<String> = self
            .units
            .iter()
            .filter(|(n, u)| {
                *n != &name && u.active != ActiveState::Inactive && u.kind == UnitKind::Service
            })
            .map(|(n, _)| n.clone())
            .collect();
        for n in names {
            self.stop(&n)?;
        }
        self.start(&name)
    }
}
