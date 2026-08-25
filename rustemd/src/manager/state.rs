//! Unit active-state machine and per-unit runtime state.

use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use crate::log::LogRing;
#[cfg(target_os = "linux")]
use crate::unit::MountConfig;
#[cfg(feature = "socket")]
use crate::unit::SocketConfig;
use crate::unit::{ServiceConfig, TimerConfig, UnitFile, UnitKind};

pub type Pid = i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActiveState {
    Inactive,
    Activating,
    Active,
    Deactivating,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubState {
    Dead,
    StartPre,
    Start,
    Post,
    Running,
    /// A `.mount` unit that has completed `mount(2)` (active).
    Mounted,
    Exited,
    WaitingForBus,
    Stop,
    StopSigterm,
    StopSigkill,
    StopPost,
    FinalSigterm,
    FinalSigkill,
    AutoRestart,
    Failed,
}

impl SubState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubState::Dead => "dead",
            SubState::StartPre => "start-pre",
            SubState::Start => "start",
            SubState::Post => "post",
            SubState::Running => "running",
            SubState::Mounted => "mounted",
            SubState::Exited => "exited",
            SubState::WaitingForBus => "waiting-for-bus",
            SubState::Stop => "stop",
            SubState::StopSigterm => "stop-sigterm",
            SubState::StopSigkill => "stop-sigkill",
            SubState::StopPost => "stop-post",
            SubState::FinalSigterm => "final-sigterm",
            SubState::FinalSigkill => "final-sigkill",
            SubState::AutoRestart => "auto-restart",
            SubState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitResult {
    Success,
    ExitCode,
    Signal,
    Timeout,
    StartLimitHit,
    Dependency,
    Resources,
    Protocol,
    Watchdog,
    Exec,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoadState {
    Loaded,
    NotFound,
    Error,
}

/// Which Exec* command the control process is currently running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCommand {
    StartPre,
    Start,
    StartPost,
    Stop,
    Reload,
    Kill,
}

/// Per-unit runtime state.
pub struct Unit {
    pub name: String,
    pub kind: UnitKind,
    pub file: Option<UnitFile>,
    pub load: LoadState,
    pub path: Option<PathBuf>,
    pub load_error: Option<String>,
    pub active: ActiveState,
    pub sub: SubState,
    pub result: UnitResult,
    /// Main PID of a running service (or forking pidfile).
    pub main_pid: Option<Pid>,
    /// Process-group leader (= the pid we forked for this service); this is
    /// the id we `kill(-group_pid, ...)` to reach the whole tree.
    pub group_pid: Option<Pid>,
    /// cgroup v2 directory for this unit's processes (Linux). When `Some`,
    /// kill/limits operate on the cgroup; when `None`, process groups.
    pub cgroup: Option<PathBuf>,
    /// Index into the current Exec* command list (for sequential oneshot).
    pub cmd_index: usize,
    /// PID of the current control process (ExecStartPre/Start/Stop/...).
    pub control_pid: Option<Pid>,
    /// Which command the control process runs.
    pub control_command: Option<ControlCommand>,
    /// For Type=forking: PID read from PIDFile.
    pub forked_main_pid: Option<Pid>,
    pub active_enter: Option<SystemTime>,
    pub inactive_enter: Option<SystemTime>,
    /// When the current control command was spawned (for timeouts).
    pub control_start: Option<Instant>,
    pub stop_started: Option<Instant>,
    pub last_exit_code: Option<i32>,
    pub last_exit_signal: Option<i32>,
    /// True when a start was interrupted by a stop/restart of a running unit.
    pub stop_sent_kill: bool,
    pub log: LogRing,
    /// Next auto-restart time (wall-clock Instant).
    pub restart_at: Option<Instant>,
    /// Sliding window of start attempt times for the start limit.
    pub start_window: Vec<Instant>,
    /// True once this unit has newly-loaded config after daemon-reload.
    pub notify_fd: Option<i32>,
    /// Notify/so-callback socket credentials target (sd_notify).
    pub notify_ready: bool,
    /// Timer scheduling state when this is a timer unit.
    pub timer: Option<TimerState>,
}

impl Unit {
    pub fn new(name: &str, kind: UnitKind) -> Self {
        Unit {
            name: name.to_string(),
            kind,
            file: None,
            load: LoadState::NotFound,
            path: None,
            load_error: None,
            active: ActiveState::Inactive,
            sub: SubState::Dead,
            result: UnitResult::Success,
            main_pid: None,
            group_pid: None,
            cgroup: None,
            cmd_index: 0,
            control_pid: None,
            control_command: None,
            forked_main_pid: None,
            active_enter: None,
            inactive_enter: Some(SystemTime::now()),
            control_start: None,
            stop_started: None,
            last_exit_code: None,
            last_exit_signal: None,
            stop_sent_kill: false,
            log: LogRing::new(200),
            restart_at: None,
            start_window: Vec::new(),
            notify_fd: None,
            notify_ready: false,
            timer: None,
        }
    }

    pub fn service_cfg(&self) -> Option<&ServiceConfig> {
        self.file.as_ref().and_then(|f| f.service.as_ref())
    }
    pub fn timer_cfg(&self) -> Option<&TimerConfig> {
        self.file.as_ref().and_then(|f| f.timer.as_ref())
    }

    #[cfg(feature = "socket")]
    pub fn socket_cfg(&self) -> Option<&SocketConfig> {
        self.file.as_ref().and_then(|f| f.socket.as_ref())
    }

    #[cfg(target_os = "linux")]
    pub fn mount_cfg(&self) -> Option<&MountConfig> {
        self.file.as_ref().and_then(|f| f.mount.as_ref())
    }

    /// The unit that a timer activates (default: same prefix, `.service`).
    pub fn activated_unit(&self) -> String {
        if let Some(t) = self.timer_cfg()
            && let Some(u) = &t.unit
        {
            return u.clone();
        }
        let dot = self.name.rfind('.').unwrap_or(self.name.len());
        format!("{}.service", &self.name[..dot])
    }

    /// The service a socket unit activates (default: same prefix, `.service`).
    #[cfg(feature = "socket")]
    pub fn activated_service(&self) -> String {
        if let Some(s) = self.socket_cfg()
            && let Some(u) = &s.service
        {
            return u.clone();
        }
        let dot = self.name.rfind('.').unwrap_or(self.name.len());
        format!("{}.service", &self.name[..dot])
    }

    pub fn set_active(&mut self, state: ActiveState, sub: SubState, result: UnitResult) {
        let now = SystemTime::now();
        self.active = state;
        self.sub = sub;
        if result != UnitResult::Success {
            self.result = result;
        }
        match state {
            ActiveState::Active => {
                self.active_enter = Some(now);
                self.inactive_enter = None;
                self.restart_at = None;
            }
            ActiveState::Inactive | ActiveState::Failed | ActiveState::Deactivating
                if self.active_enter.is_some() =>
            {
                self.inactive_enter = Some(now);
            }
            _ => {}
        }
    }
}

/// Bookkeeping for a timer unit's next scheduled elapse.
#[derive(Debug, Clone)]
pub struct TimerState {
    /// Next wall-clock (civil, epoch-seconds) trigger from OnCalendar.
    pub next_calendar: Option<(u64, usize)>, // (epoch sec, calendar index)
    /// Next monotonic trigger from OnBootSec/OnUnitActiveSec/... in Instant.
    pub next_monotonic: Option<Instant>,
    /// Last trigger times.
    pub last_trigger: Option<SystemTime>,
    pub last_trigger_calendar: Option<(u64, usize)>,
    pub last_trigger_monotonic: Option<Instant>,
    /// Last result of the triggered job.
    pub last_result: Option<UnitResult>,
    /// Fired-at for the current cycle (epoch secs) — for list-timers NEXT.
    pub next_display: Option<SystemTime>,
    /// The source spec string for display.
    pub spec_strings: Vec<String>,
}

impl TimerState {
    pub fn new(spec_strings: Vec<String>) -> Self {
        TimerState {
            next_calendar: None,
            next_monotonic: None,
            last_trigger: None,
            last_trigger_calendar: None,
            last_trigger_monotonic: None,
            last_result: None,
            next_display: None,
            spec_strings,
        }
    }
}
