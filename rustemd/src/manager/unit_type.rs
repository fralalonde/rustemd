//! Per-unit-type behavior — the internal analogue of systemd's per-type VTable.
//!
//! The manager core (job engine, dependency graph, process accounting, IPC,
//! timers) is type-agnostic; each unit type plugs its own "start" and "stop"
//! semantics in here. Adding a unit type (e.g. `.socket`) means implementing
//! this trait and registering it in [`Manager::unit_type`] — nothing in the
//! core dispatch needs to grow another `if kind == …` branch.
//!
//! The split mirrors s6-rc's longrun-vs-oneshot distinction: a *service* is
//! the only type that runs processes; *target*/*timer*/*socket* are marker or
//! trigger types with no process of their own.

use nix::sys::signal::Signal;

use crate::manager::Manager;
use crate::manager::state::{
    ActiveState, ControlCommand as UnitControlCommand, SubState, UnitResult,
};

pub trait UnitType {
    /// Begin the start sequence for a unit already cleared as operational.
    fn start(&self, mgr: &mut Manager, name: &str);
    /// Begin the stop sequence for a unit already cleared as operational.
    fn stop(&self, mgr: &mut Manager, name: &str);
}

/// A `.service` unit: spawn and supervise an `ExecStart` process (or its
/// control commands for `Type=oneshot`/`forking`).
pub struct ServiceUnit;

impl UnitType for ServiceUnit {
    fn start(&self, mgr: &mut Manager, name: &str) {
        let u = mgr.units.get_mut(name).unwrap();
        u.main_pid = None;
        u.group_pid = None;
        u.control_pid = None;
        u.control_command = None;
        u.cmd_index = 0;
        u.forked_main_pid = None;
        u.result = UnitResult::Success;
        u.set_active(
            ActiveState::Activating,
            SubState::StartPre,
            UnitResult::Success,
        );

        let has_pre = u
            .service_cfg()
            .map(|s| !s.exec_start_pre.is_empty())
            .unwrap_or(false);
        if has_pre {
            mgr.spawn_control(name, UnitControlCommand::StartPre, 0);
        } else {
            mgr.spawn_control(name, UnitControlCommand::Start, 0);
        }
    }

    fn stop(&self, mgr: &mut Manager, name: &str) {
        let has_main = mgr.unit_has_processes(name);
        let (sig, no_exec_stop) = {
            let u = mgr.units.get(name).unwrap();
            let sc = u.service_cfg().cloned();
            (
                sc.as_ref()
                    .and_then(|s| s.kill_signal)
                    .unwrap_or(Signal::SIGTERM),
                sc.as_ref().map(|s| s.exec_stop.is_empty()).unwrap_or(true),
            )
        };

        if has_main {
            mgr.kill_tree(name, sig);
            mgr.units.get_mut(name).unwrap().sub = SubState::StopSigterm;
            mgr.arm_stop_timeout(name);
        } else if no_exec_stop {
            mgr.finalize_stop(name);
        } else {
            mgr.units.get_mut(name).unwrap().sub = SubState::Stop;
            mgr.spawn_control(name, UnitControlCommand::Stop, 0);
        }
    }
}

/// A `.target`: pure grouping/synchronization marker — active the instant it
/// is "started".
pub struct TargetUnit;

impl UnitType for TargetUnit {
    fn start(&self, mgr: &mut Manager, name: &str) {
        mgr.units.get_mut(name).unwrap().set_active(
            ActiveState::Active,
            SubState::Dead,
            UnitResult::Success,
        );
        mgr.complete_start_job(name);
    }

    fn stop(&self, mgr: &mut Manager, name: &str) {
        mgr.finalize_stop(name);
    }
}

/// A `.timer`: an activation trigger for another unit, not a process. Starting
/// it marks it active (armed); the actual scheduling lives in the timer wheel.
pub struct TimerUnit;

impl UnitType for TimerUnit {
    fn start(&self, mgr: &mut Manager, name: &str) {
        mgr.units.get_mut(name).unwrap().set_active(
            ActiveState::Active,
            SubState::Dead,
            UnitResult::Success,
        );
        mgr.complete_start_job(name);
    }

    fn stop(&self, mgr: &mut Manager, name: &str) {
        mgr.finalize_stop(name);
    }
}

/// A `.socket`: binds listening sockets and activates a matching `.service` on
/// the first connection. Compiled only with the `socket` feature.
#[cfg(feature = "socket")]
pub struct SocketUnit;

#[cfg(feature = "socket")]
impl UnitType for SocketUnit {
    fn start(&self, mgr: &mut Manager, name: &str) {
        mgr.start_socket(name);
    }

    fn stop(&self, mgr: &mut Manager, name: &str) {
        mgr.stop_socket(name);
    }
}

/// A `.device`: runtime-generated from the sysfs device tree and uevents —
/// never parsed from a file. A device is active the instant it exists; there
/// is no process to spawn, so "start" simply confirms activation (a no-op for
/// an already-active unit) and "stop" finalizes immediately.
#[cfg(all(target_os = "linux", feature = "udev"))]
pub struct DeviceUnit;

#[cfg(all(target_os = "linux", feature = "udev"))]
impl UnitType for DeviceUnit {
    fn start(&self, mgr: &mut Manager, name: &str) {
        mgr.units.get_mut(name).unwrap().set_active(
            ActiveState::Active,
            SubState::Dead,
            UnitResult::Success,
        );
        mgr.complete_start_job(name);
    }

    fn stop(&self, mgr: &mut Manager, name: &str) {
        mgr.finalize_stop(name);
    }
}
