//! Typed unit-file model and builder.
//!
//! `parse::RawUnitFile` is the structural syntax tree; this module interprets
//! it into typed `UnitFile`/`*Config` structs, applies specifier expansion,
//! and merges drop-in directories (`foo.service.d/*.conf`) with
//! main-file-overrides-dropin semantics.

pub mod parse;

use std::path::PathBuf;

use crate::platform::signal::Signal;

use crate::calendar::CalendarSpec;
use crate::specifier::SpecifierContext;
use crate::timespan::TimeSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    Service,
    Timer,
    Target,
    #[cfg(feature = "socket")]
    Socket,
    #[cfg(all(target_os = "linux", feature = "udev"))]
    Device,
    #[cfg(target_os = "linux")]
    Mount,
}

impl UnitKind {
    pub fn suffix(&self) -> &'static str {
        match self {
            UnitKind::Service => "service",
            UnitKind::Timer => "timer",
            UnitKind::Target => "target",
            #[cfg(feature = "socket")]
            UnitKind::Socket => "socket",
            #[cfg(all(target_os = "linux", feature = "udev"))]
            UnitKind::Device => "device",
            #[cfg(target_os = "linux")]
            UnitKind::Mount => "mount",
        }
    }
    pub fn from_suffix(s: &str) -> Option<UnitKind> {
        match s {
            "service" => Some(UnitKind::Service),
            "timer" => Some(UnitKind::Timer),
            "target" => Some(UnitKind::Target),
            #[cfg(feature = "socket")]
            "socket" => Some(UnitKind::Socket),
            #[cfg(all(target_os = "linux", feature = "udev"))]
            "device" => Some(UnitKind::Device),
            #[cfg(target_os = "linux")]
            "mount" => Some(UnitKind::Mount),
            _ => None,
        }
    }
    pub fn from_unit_name(name: &str) -> Option<UnitKind> {
        let dot = name.rfind('.')?;
        Self::from_suffix(&name[dot + 1..])
    }
}

/// One parsed main executable command (from `ExecStart*`).
#[derive(Debug, Clone)]
pub struct ExecCommand {
    /// argv with specifiers already expanded; `$VAR` still to expand at exec.
    pub argv: Vec<String>,
    /// Leading `-` on `ExecStart=-...`: failure is not fatal.
    pub ignore_failure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServiceType {
    #[default]
    Simple,
    Exec,
    Oneshot,
    Forking,
    Notify,
    Dbus,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestartPolicy {
    #[default]
    No,
    OnSuccess,
    OnFailure,
    OnAbnormal,
    OnAbort,
    OnWatchdog,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KillMode {
    /// Kill the whole process group (our substitute for a cgroup).
    #[default]
    ControlGroup,
    /// Kill only the main PID.
    Process,
}

#[derive(Debug, Clone)]
pub struct ExitCodeSet {
    /// Inclusive code ranges.
    pub codes: Vec<(u32, u32)>,
    pub signals: Vec<i32>,
}

impl ExitCodeSet {
    pub fn default_success() -> Self {
        ExitCodeSet {
            codes: vec![(0, 0)],
            signals: vec![],
        }
    }
    /// Does the terminal status count as success?
    /// `code`: `Some(n)` = exited with code n; `None` = killed by `signal`.
    pub fn matches(&self, code: Option<i32>, signal: Option<i32>) -> bool {
        match (code, signal) {
            (Some(c), None) => {
                self.codes
                    .iter()
                    .any(|&(lo, hi)| (c as u32) >= lo && (c as u32) <= hi)
                    || (self.codes.is_empty() && c == 0)
            }
            (None, Some(sig)) => self.signals.contains(&sig),
            _ => false,
        }
    }
}

/// Parse a `SuccessExitStatus=` style value: `0`, `0..3`, `SIGTERM`, `SIGKILL`,
/// or a space-separated list of the above.
pub fn parse_exit_status(value: &str) -> Result<ExitCodeSet, String> {
    let mut out = ExitCodeSet {
        codes: vec![],
        signals: vec![],
    };
    for tok in value.split_whitespace() {
        if let Some((lo, hi)) = tok.split_once("..") {
            let lo: u32 = lo
                .trim()
                .parse()
                .map_err(|_| format!("bad range `{tok}`"))?;
            let hi: u32 = hi
                .trim()
                .parse()
                .map_err(|_| format!("bad range `{tok}`"))?;
            out.codes.push((lo, hi));
        } else if let Ok(n) = tok.parse::<u32>() {
            out.codes.push((n, n));
        } else if let Some(sig) = sig_from_name(tok) {
            out.signals.push(sig.as_raw());
        } else if let Some(code) = sysexit_from_name(tok) {
            out.codes.push((code, code));
        } else {
            return Err(format!("invalid success exit status `{tok}`"));
        }
    }
    if out.codes.is_empty() && out.signals.is_empty() {
        return Err("empty SuccessExitStatus".into());
    }
    Ok(out)
}

/// Parse a `sysexits(3)` exit-status name (`EX_DATAERR`, `DATAERR`, `TEMPFAIL`,
/// `SUCCESS`, ...) into its numeric code. Case-insensitive; the `EX_` prefix
/// is optional, matching systemd's `exit_status_from_string`.
pub fn sysexit_from_name(s: &str) -> Option<u32> {
    let s = s.to_ascii_uppercase();
    let s = s.strip_prefix("EX_").unwrap_or(&s);
    Some(match s {
        "OK" | "SUCCESS" => 0,
        "FAILURE" => 1,
        "USAGE" => 64,
        "DATAERR" => 65,
        "NOINPUT" => 66,
        "NOUSER" => 67,
        "NOHOST" => 68,
        "UNAVAILABLE" => 69,
        "SOFTWARE" => 70,
        "OSERR" => 71,
        "OSFILE" => 72,
        "CANTCREAT" => 73,
        "IOERR" => 74,
        "TEMPFAIL" => 75,
        "PROTOCOL" => 76,
        "NOPERM" => 77,
        "CONFIG" => 78,
        _ => return None,
    })
}

/// Parse a signal by name, accepting `SIGTERM`, `TERM`, or a bare number.
pub fn sig_from_name(s: &str) -> Option<Signal> {
    let s = s.strip_prefix("SIG").unwrap_or(s);
    if let Ok(number) = s.parse::<i32>() {
        return Signal::try_from(number).ok();
    }
    let s = s.to_ascii_uppercase();
    let n = match s.as_str() {
        "HUP" => 1,
        "INT" => 2,
        "QUIT" => 3,
        "ILL" => 4,
        "TRAP" => 5,
        "ABRT" => 6,
        "BUS" => 7,
        "FPE" => 8,
        "KILL" => 9,
        "USR1" => 10,
        "SEGV" => 11,
        "USR2" => 12,
        "PIPE" => 13,
        "ALRM" => 14,
        "TERM" => 15,
        "CHLD" => 17,
        "CONT" => 18,
        "STOP" => 19,
        "TSTP" => 20,
        "TTIN" => 21,
        "TTOU" => 22,
        "URG" => 23,
        "XCPU" => 24,
        "XFSZ" => 25,
        "VTALRM" => 26,
        "PROF" => 27,
        "WINCH" => 28,
        "IO" => 29,
        "PWR" => 30,
        "SYS" => 31,
        _ => return None,
    };
    Signal::try_from(n).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RlimitResource {
    NoFile,
    NProc,
    Core,
    AddressSpace,
}

#[derive(Debug, Clone)]
pub struct Rlimit {
    pub resource: RlimitResource,
    pub soft: u64,
    pub hard: u64,
}

/// cgroup v2 resource limits (Linux-only; no-ops elsewhere). Byte values are
/// raw bytes; `None` = unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CgroupLimits {
    /// `MemoryMax=` — hard ceiling on resident memory.
    pub memory_max: Option<u64>,
    /// `MemoryHigh=` — soft throttle threshold.
    pub memory_high: Option<u64>,
    /// `CPUWeight=` — relative CPU share (1..=10000).
    pub cpu_weight: Option<u32>,
    /// `TasksMax=` — maximum number of tasks (threads/processes).
    pub tasks_max: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StdioTarget {
    /// Capture to the in-memory per-unit log ring (shown by `status`).
    #[default]
    Journal,
    /// Inherit the manager's stdout/stderr.
    Inherit,
    Discard,
    File(PathBuf),
}

/// `[Unit]` dependencies/description.
#[derive(Debug, Clone, Default)]
pub struct UnitConfig {
    pub description: String,
    pub after: Vec<String>,
    pub before: Vec<String>,
    pub requires: Vec<String>,
    pub requisite: Vec<String>,
    pub wants: Vec<String>,
    pub conflicts: Vec<String>,
    pub on_failure: Vec<String>,
    pub part_of: Vec<String>,
    pub binds_to: Vec<String>,
    pub default_dependencies: bool,
    pub documentation: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ServiceConfig {
    pub service_type: ServiceType,
    pub remain_after_exit: bool,
    pub exec_start_pre: Vec<ExecCommand>,
    pub exec_start: Vec<ExecCommand>,
    pub exec_start_post: Vec<ExecCommand>,
    pub exec_stop: Vec<ExecCommand>,
    pub exec_reload: Vec<ExecCommand>,
    pub restart: RestartPolicy,
    pub restart_sec: TimeSpan,
    pub timeout_start_sec: TimeSpan,
    pub timeout_stop_sec: TimeSpan,
    pub environment: Vec<(String, String)>,
    /// (path, ignore_missing)
    pub environment_files: Vec<(String, bool)>,
    /// (path, ignore_failure)
    pub working_directory: Option<(String, bool)>,
    pub user: Option<String>,
    pub group: Option<String>,
    pub nice: Option<i32>,
    pub umask: Option<u32>,
    pub kill_signal: Option<Signal>,
    pub kill_mode: KillMode,
    pub send_sigkill: bool,
    pub success_exit_status: Option<ExitCodeSet>,
    pub pid_file: Option<String>,
    /// `BusName=` — for `Type=dbus`, the well-known bus name whose ownership
    /// gates the unit's transition to `active`.
    pub bus_name: Option<String>,
    pub rlimits: Vec<Rlimit>,
    pub cgroup_limits: CgroupLimits,
    pub std_output: StdioTarget,
    pub std_error: StdioTarget,
    pub std_input: bool, // false = /dev/null
}

impl ServiceConfig {
    pub fn effective_exit_success(&self) -> ExitCodeSet {
        // Exit 0 is always a success; `SuccessExitStatus=` adds to it (never
        // replaces it), matching systemd's `exit-status.c` semantics.
        let mut set = ExitCodeSet::default_success();
        if let Some(s) = &self.success_exit_status {
            set.codes.extend_from_slice(&s.codes);
            set.signals.extend_from_slice(&s.signals);
        }
        set
    }
}

#[derive(Debug, Clone, Default)]
pub struct TimerConfig {
    pub on_calendar: Vec<CalendarSpec>,
    pub on_boot_sec: Vec<TimeSpan>,
    pub on_startup_sec: Vec<TimeSpan>,
    pub on_active_sec: Vec<TimeSpan>,
    pub on_inactive_sec: Vec<TimeSpan>,
    pub persistent: bool,
    pub accuracy_sec: TimeSpan,
    pub randomized_delay_sec: TimeSpan,
    pub unit: Option<String>,
    pub remain_after_elapse: bool,
    pub wake_system: bool,
}

/// `[Socket]` section — socket-activation config. `listen_stream` entries are
/// unix socket paths (bare or `unix:/path`) or TCP `host:port`; interpretation
/// happens at bind time in the manager.
#[cfg(feature = "socket")]
#[derive(Debug, Clone, Default)]
pub struct SocketConfig {
    pub listen_stream: Vec<String>,
    /// `Accept=yes`: pass the *connected* socket per connection instead of the
    /// listening socket. Default (false) is the inetd/systemd `Accept=no` case.
    pub accept: bool,
    /// `Service=` override for the unit to activate (default: same prefix).
    pub service: Option<String>,
}

/// `[Mount]` section — a filesystem mount unit. `what` is the device (or the
/// filesystem name for pseudo-filesystems like `tmpfs`); `where_` is the mount
/// point, defaulting to the unit name with `-` mapped to `/`; `fs_type` is the
/// filesystem type; `options` are the comma-separated mount options.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Default)]
pub struct MountConfig {
    /// `What=` — the device or pseudo-filesystem to mount.
    pub what: Option<String>,
    /// `Where=` — the mount point (derived from the unit name if unset).
    pub where_: Option<String>,
    /// `Type=` — the filesystem type (`tmpfs`, `ext4`, ...).
    pub fs_type: Option<String>,
    /// `Options=` — comma-separated mount options.
    pub options: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct InstallConfig {
    pub wanted_by: Vec<String>,
    pub required_by: Vec<String>,
    pub also: Vec<String>,
    pub alias: Vec<String>,
}

/// A fully interpreted unit file (already merged with drop-ins).
#[derive(Debug, Clone)]
pub struct UnitFile {
    /// Path the unit was loaded from, if a real file backs it.
    pub path: Option<PathBuf>,
    pub unit: UnitConfig,
    pub service: Option<ServiceConfig>,
    pub timer: Option<TimerConfig>,
    #[cfg(feature = "socket")]
    pub socket: Option<SocketConfig>,
    #[cfg(target_os = "linux")]
    pub mount: Option<MountConfig>,
    pub install: InstallConfig,
}

impl UnitFile {
    pub fn kind(&self) -> UnitKind {
        if self.service.is_some() {
            UnitKind::Service
        } else if self.timer.is_some() {
            UnitKind::Timer
        } else {
            #[cfg(feature = "socket")]
            if self.socket.is_some() {
                return UnitKind::Socket;
            }
            #[cfg(target_os = "linux")]
            if self.mount.is_some() {
                return UnitKind::Mount;
            }
            UnitKind::Target
        }
    }
}

fn split_names(raw: &str) -> Vec<String> {
    raw.split_whitespace().map(str::to_string).collect()
}

/// Interpret a merged raw unit file into typed config, expanding specifiers.
pub fn build(raw: &parse::RawUnitFile, spec: &SpecifierContext) -> Result<UnitFile, String> {
    let exp = |s: &str| spec.expand(s);

    let unit = UnitConfig {
        description: match unit_scalar(raw, "Unit", "Description") {
            Some(v) => crate::unit::parse::unquote_scalar(&exp(v))?,
            None => String::new(),
        },
        after: list_of(raw, "Unit", "After", &exp),
        before: list_of(raw, "Unit", "Before", &exp),
        requires: list_of(raw, "Unit", "Requires", &exp),
        requisite: list_of(raw, "Unit", "Requisite", &exp),
        wants: list_of(raw, "Unit", "Wants", &exp),
        conflicts: list_of(raw, "Unit", "Conflicts", &exp),
        on_failure: list_of(raw, "Unit", "OnFailure", &exp),
        part_of: list_of(raw, "Unit", "PartOf", &exp),
        binds_to: list_of(raw, "Unit", "BindsTo", &exp),
        default_dependencies: match unit_scalar(raw, "Unit", "DefaultDependencies") {
            Some(v) => parse_bool(&exp(v))?,
            None => true,
        },
        documentation: raw
            .list("Unit", "Documentation")
            .into_iter()
            .map(&exp)
            .collect(),
    };

    let install = InstallConfig {
        wanted_by: list_of(raw, "Install", "WantedBy", &exp),
        required_by: list_of(raw, "Install", "RequiredBy", &exp),
        also: list_of(raw, "Install", "Also", &exp),
        alias: list_of(raw, "Install", "Alias", &exp),
    };

    let kind = UnitKind::from_unit_name(&spec.unit_name)
        .ok_or_else(|| format!("unsupported unit type in name `{}`", spec.unit_name))?;

    let service = if kind == UnitKind::Service {
        Some(build_service(raw, &exp)?)
    } else {
        None
    };
    let timer = if kind == UnitKind::Timer {
        Some(build_timer(raw, &exp)?)
    } else {
        None
    };
    #[cfg(feature = "socket")]
    let socket = if kind == UnitKind::Socket {
        Some(build_socket(raw, &exp)?)
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let mount = if kind == UnitKind::Mount {
        Some(build_mount(raw, &exp, spec)?)
    } else {
        None
    };

    Ok(UnitFile {
        path: None,
        unit,
        service,
        timer,
        #[cfg(feature = "socket")]
        socket,
        #[cfg(target_os = "linux")]
        mount,
        install,
    })
}

/// First (last-wins) scalar value for `key` in `section`, specifier-expanded.
fn unit_scalar<'a>(raw: &'a parse::RawUnitFile, section: &'a str, key: &str) -> Option<&'a str> {
    raw.scalar(section, key)
}

fn list_of(
    raw: &parse::RawUnitFile,
    section: &str,
    key: &str,
    exp: &impl Fn(&str) -> String,
) -> Vec<String> {
    raw.list(section, key)
        .into_iter()
        .flat_map(|v| split_names(&exp(v)))
        .collect()
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "yes" | "true" | "on" => Ok(true),
        "0" | "no" | "false" | "off" => Ok(false),
        other => Err(format!("invalid boolean `{other}`")),
    }
}

#[cfg(feature = "socket")]
fn build_socket(
    raw: &parse::RawUnitFile,
    exp: &impl Fn(&str) -> String,
) -> Result<SocketConfig, String> {
    let accept = match unit_scalar(raw, "Socket", "Accept") {
        Some(v) => parse_bool(&exp(v))?,
        None => false,
    };
    Ok(SocketConfig {
        listen_stream: list_of(raw, "Socket", "ListenStream", exp),
        accept,
        service: unit_scalar(raw, "Socket", "Service").map(exp),
    })
}

fn build_service(
    raw: &parse::RawUnitFile,
    exp: &impl Fn(&str) -> String,
) -> Result<ServiceConfig, String> {
    let mut cfg = ServiceConfig {
        kill_mode: KillMode::ControlGroup,
        send_sigkill: true,
        timeout_start_sec: TimeSpan::from_usec(90 * 1_000_000),
        timeout_stop_sec: TimeSpan::from_usec(90 * 1_000_000),
        restart_sec: TimeSpan::from_usec(100_000),
        ..Default::default()
    };

    if let Some(v) = unit_scalar(raw, "Service", "Type") {
        cfg.service_type = match exp(v).to_ascii_lowercase().as_str() {
            "simple" => ServiceType::Simple,
            "exec" => ServiceType::Exec,
            "oneshot" => ServiceType::Oneshot,
            "forking" => ServiceType::Forking,
            "notify" => ServiceType::Notify,
            "dbus" => ServiceType::Dbus,
            "idle" => ServiceType::Idle,
            other => return Err(format!("invalid service Type `{other}`")),
        };
    }
    if let Some(v) = unit_scalar(raw, "Service", "BusName") {
        cfg.bus_name = Some(crate::unit::parse::unquote_scalar(&exp(v))?);
    }
    if let Some(v) = unit_scalar(raw, "Service", "RemainAfterExit") {
        cfg.remain_after_exit = parse_bool(&exp(v))?;
    }
    cfg.exec_start_pre = exec_list(raw, "ExecStartPre", exp)?;
    cfg.exec_start = exec_list(raw, "ExecStart", exp)?;
    cfg.exec_start_post = exec_list(raw, "ExecStartPost", exp)?;
    cfg.exec_stop = exec_list(raw, "ExecStop", exp)?;
    cfg.exec_reload = exec_list(raw, "ExecReload", exp)?;
    if let Some(v) = unit_scalar(raw, "Service", "Restart") {
        cfg.restart = match exp(v).to_ascii_lowercase().as_str() {
            "no" => RestartPolicy::No,
            "on-success" => RestartPolicy::OnSuccess,
            "on-failure" => RestartPolicy::OnFailure,
            "on-abnormal" => RestartPolicy::OnAbnormal,
            "on-abort" => RestartPolicy::OnAbort,
            "on-watchdog" => RestartPolicy::OnWatchdog,
            "always" => RestartPolicy::Always,
            other => return Err(format!("invalid Restart policy `{other}`")),
        };
    }
    if let Some(v) = unit_scalar(raw, "Service", "RestartSec") {
        cfg.restart_sec = TimeSpan::parse(&exp(v))?;
    }
    if let Some(v) = unit_scalar(raw, "Service", "TimeoutStartSec") {
        cfg.timeout_start_sec = TimeSpan::parse(&exp(v))?;
    }
    if let Some(v) = unit_scalar(raw, "Service", "TimeoutStopSec") {
        cfg.timeout_stop_sec = TimeSpan::parse(&exp(v))?;
    }
    for v in raw.list("Service", "Environment") {
        for tok in crate::unit::parse::tokenize(&exp(v))? {
            let (k, val) = tok
                .split_once('=')
                .ok_or_else(|| format!("Environment assignment `{tok}` lacks `=`"))?;
            cfg.environment.push((k.to_string(), val.to_string()));
        }
    }
    for v in raw.list("Service", "EnvironmentFile") {
        for tok in crate::unit::parse::tokenize(&exp(v))? {
            let (ignore, path) = match tok.strip_prefix('-') {
                Some(p) => (true, p.to_string()),
                None => (false, tok),
            };
            cfg.environment_files.push((path, ignore));
        }
    }
    if let Some(v) = unit_scalar(raw, "Service", "WorkingDirectory") {
        let (ignore, path) = match exp(v).strip_prefix('-') {
            Some(p) => (true, p.to_string()),
            None => (false, exp(v)),
        };
        cfg.working_directory = Some((path, ignore));
    }
    if let Some(v) = unit_scalar(raw, "Service", "User") {
        cfg.user = Some(exp(v));
    }
    if let Some(v) = unit_scalar(raw, "Service", "Group") {
        cfg.group = Some(exp(v));
    }
    if let Some(v) = unit_scalar(raw, "Service", "Nice") {
        cfg.nice = Some(exp(v).parse().map_err(|_| format!("invalid Nice `{v}`"))?);
    }
    if let Some(v) = unit_scalar(raw, "Service", "UMask") {
        cfg.umask = Some(parse_octal(&exp(v))?);
    }
    if let Some(v) = unit_scalar(raw, "Service", "KillSignal") {
        cfg.kill_signal = exp(v)
            .split_whitespace()
            .next()
            .and_then(sig_from_name)
            .map(Some)
            .ok_or_else(|| format!("invalid KillSignal `{v}`"))?;
    }
    if let Some(v) = unit_scalar(raw, "Service", "KillMode") {
        cfg.kill_mode = match exp(v).to_ascii_lowercase().as_str() {
            "control-group" | "mixed" | "none" => KillMode::ControlGroup,
            "process" => KillMode::Process,
            other => return Err(format!("unsupported KillMode `{other}`")),
        };
    }
    if let Some(v) = unit_scalar(raw, "Service", "SendSIGKILL") {
        cfg.send_sigkill = parse_bool(&exp(v))?;
    }
    if let Some(v) = unit_scalar(raw, "Service", "SuccessExitStatus") {
        cfg.success_exit_status = Some(parse_exit_status(&exp(v))?);
    }
    if let Some(v) = unit_scalar(raw, "Service", "PIDFile") {
        cfg.pid_file = Some(crate::unit::parse::unquote_scalar(&exp(v))?);
    }
    for v in raw.list("Service", "LimitNOFILE") {
        cfg.rlimits
            .push(parse_rlimit(RlimitResource::NoFile, &exp(v))?);
    }
    for v in raw.list("Service", "LimitNPROC") {
        cfg.rlimits
            .push(parse_rlimit(RlimitResource::NProc, &exp(v))?);
    }
    for v in raw.list("Service", "LimitCORE") {
        cfg.rlimits
            .push(parse_rlimit(RlimitResource::Core, &exp(v))?);
    }
    for v in raw.list("Service", "LimitAS") {
        cfg.rlimits
            .push(parse_rlimit(RlimitResource::AddressSpace, &exp(v))?);
    }
    if let Some(v) = unit_scalar(raw, "Service", "MemoryMax") {
        cfg.cgroup_limits.memory_max = Some(parse_bytes(&exp(v))?);
    }
    if let Some(v) = unit_scalar(raw, "Service", "MemoryHigh") {
        cfg.cgroup_limits.memory_high = Some(parse_bytes(&exp(v))?);
    }
    if let Some(v) = unit_scalar(raw, "Service", "CPUWeight") {
        let w: u32 = exp(v)
            .trim()
            .parse()
            .map_err(|_| format!("invalid CPUWeight `{v}`"))?;
        if w == 0 || w > 10000 {
            return Err(format!("CPUWeight out of range 1..=10000: `{v}`"));
        }
        cfg.cgroup_limits.cpu_weight = Some(w);
    }
    if let Some(v) = unit_scalar(raw, "Service", "TasksMax") {
        let t = exp(v);
        cfg.cgroup_limits.tasks_max = Some(if t.trim().eq_ignore_ascii_case("infinity") {
            u64::MAX
        } else {
            t.trim()
                .parse()
                .map_err(|_| format!("invalid TasksMax `{v}`"))?
        });
    }
    if let Some(v) = unit_scalar(raw, "Service", "StandardOutput") {
        cfg.std_output = parse_stdio(&exp(v))?;
    }
    if let Some(v) = unit_scalar(raw, "Service", "StandardError") {
        cfg.std_error = parse_stdio(&exp(v))?;
    }
    if let Some(v) = unit_scalar(raw, "Service", "StandardInput") {
        cfg.std_input = match exp(v).to_ascii_lowercase().as_str() {
            "null" | "data" => false,
            "inherit" | "tty" => true,
            other => return Err(format!("unsupported StandardInput `{other}`")),
        };
    }
    Ok(cfg)
}

fn parse_octal(v: &str) -> Result<u32, String> {
    u32::from_str_radix(v, 8).map_err(|_| format!("invalid octal `{v}`"))
}

fn parse_rlimit(resource: RlimitResource, v: &str) -> Result<Rlimit, String> {
    let (soft, hard) = match v.split_once(':') {
        Some((s, h)) => (s.trim(), h.trim()),
        None => (v.trim(), v.trim()),
    };
    let s = parse_limit(soft)?;
    let h = parse_limit(hard)?;
    Ok(Rlimit {
        resource,
        soft: s,
        hard: h,
    })
}
fn parse_limit(v: &str) -> Result<u64, String> {
    if v.eq_ignore_ascii_case("infinity") {
        Ok(u64::MAX)
    } else {
        v.parse().map_err(|_| format!("invalid rlimit value `{v}`"))
    }
}

/// Parse a byte size with an optional binary suffix (`K`/`M`/`G`/`T`/`P`/`E`),
/// or the literal `infinity` (maps to `u64::MAX`, written as `max` to the
/// cgroupfs files). Used by `MemoryMax=`/`MemoryHigh=`.
fn parse_bytes(v: &str) -> Result<u64, String> {
    let s = v.trim();
    if s.eq_ignore_ascii_case("infinity") {
        return Ok(u64::MAX);
    }
    let s = s.replace(' ', "");
    let (num, mult): (&str, u64) = match s.chars().last() {
        Some('K') | Some('k') => (&s[..s.len() - 1], 1024),
        Some('M') | Some('m') => (&s[..s.len() - 1], 1024 * 1024),
        Some('G') | Some('g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        Some('T') | Some('t') => (&s[..s.len() - 1], 1024u64.pow(4)),
        Some('P') | Some('p') => (&s[..s.len() - 1], 1024u64.pow(5)),
        Some('E') | Some('e') => (&s[..s.len() - 1], 1024u64.pow(6)),
        _ => (&s[..], 1),
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| format!("invalid size `{v}`"))?;
    n.checked_mul(mult)
        .ok_or_else(|| format!("size overflow `{v}`"))
}

fn parse_stdio(v: &str) -> Result<StdioTarget, String> {
    let mut s = v;
    let mut path_err: Option<String> = None;
    let ignore = s.starts_with('-');
    if ignore {
        s = &s[1..];
    }
    let out = match s {
        "journal" | "kmsg" | "syslog" | "inherit" | "journal+console" => StdioTarget::Journal,
        "null" => StdioTarget::Discard,
        _ => {
            if let Some(f) = s.strip_prefix("file:") {
                StdioTarget::File(PathBuf::from(f))
            } else {
                path_err = Some(format!("unsupported StandardOutput/std error value `{v}`"));
                StdioTarget::Journal
            }
        }
    };
    if let Some(e) = path_err
        && !ignore
    {
        return Err(e);
    }
    Ok(out)
}

fn exec_list(
    raw: &parse::RawUnitFile,
    key: &str,
    exp: &impl Fn(&str) -> String,
) -> Result<Vec<ExecCommand>, String> {
    let mut out = vec![];
    for v in raw.list("Service", key) {
        let expanded = exp(v);
        let trimmed = expanded.trim_start();
        let (ignore_failure, rest) = match trimmed.strip_prefix('-') {
            Some(r) => (true, r.trim_start()),
            None => (false, trimmed),
        };
        // A fully-empty ExecStart value (or just "-") contributes nothing.
        if rest.trim().is_empty() {
            continue;
        }
        let argv = crate::unit::parse::tokenize(rest)?;
        if argv.is_empty() {
            continue;
        }
        out.push(ExecCommand {
            argv,
            ignore_failure,
        });
    }
    Ok(out)
}

fn build_timer(
    raw: &parse::RawUnitFile,
    exp: &impl Fn(&str) -> String,
) -> Result<TimerConfig, String> {
    let mut cfg = TimerConfig {
        accuracy_sec: TimeSpan::from_usec(60 * 1_000_000),
        ..Default::default()
    };
    for v in raw.list("Timer", "OnCalendar") {
        cfg.on_calendar.push(CalendarSpec::parse(&exp(v))?);
    }
    for v in raw.list("Timer", "OnBootSec") {
        cfg.on_boot_sec.push(TimeSpan::parse(&exp(v))?);
    }
    for v in raw.list("Timer", "OnStartupSec") {
        cfg.on_startup_sec.push(TimeSpan::parse(&exp(v))?);
    }
    for v in raw.list("Timer", "OnUnitActiveSec") {
        cfg.on_active_sec.push(TimeSpan::parse(&exp(v))?);
    }
    for v in raw.list("Timer", "OnUnitInactiveSec") {
        cfg.on_inactive_sec.push(TimeSpan::parse(&exp(v))?);
    }
    if let Some(v) = unit_scalar(raw, "Timer", "Persistent") {
        cfg.persistent = parse_bool(&exp(v))?;
    }
    if let Some(v) = unit_scalar(raw, "Timer", "AccuracySec") {
        cfg.accuracy_sec = TimeSpan::parse(&exp(v))?;
    }
    if let Some(v) = unit_scalar(raw, "Timer", "RandomizedDelaySec") {
        cfg.randomized_delay_sec = TimeSpan::parse(&exp(v))?;
    }
    if let Some(v) = unit_scalar(raw, "Timer", "Unit") {
        cfg.unit = Some(exp(v));
    }
    if let Some(v) = unit_scalar(raw, "Timer", "RemainAfterElapse") {
        cfg.remain_after_elapse = parse_bool(&exp(v))?;
    }
    Ok(cfg)
}

/// Derive a mount point from a `.mount` unit name: `tmp-demo.mount` →
/// `/tmp/demo`. The escaping mirrors systemd's path units — `-` maps to `/`,
/// and `\xHH` escapes decode to the literal byte (so a literal `-` in the path
/// is written `\x2d` in the unit name).
#[cfg(target_os = "linux")]
pub fn mount_path_from_unit_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".mount")?;
    let b = stem.as_bytes();
    // systemd's path escape drops the leading `/`; unescaping re-prepends it.
    let mut out: Vec<u8> = Vec::with_capacity(b.len() + 1);
    out.push(b'/');
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'-' {
            out.push(b'/');
            i += 1;
        } else if b[i] == b'\\' && i + 3 < b.len() && b[i + 1] == b'x' {
            let hex = std::str::from_utf8(&b[i + 2..i + 4]).unwrap_or("");
            match u8::from_str_radix(hex, 16) {
                Ok(v) => {
                    out.push(v);
                    i += 4;
                }
                Err(_) => {
                    out.push(b'\\');
                    i += 1;
                }
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(target_os = "linux")]
fn build_mount(
    raw: &parse::RawUnitFile,
    exp: &impl Fn(&str) -> String,
    spec: &SpecifierContext,
) -> Result<MountConfig, String> {
    let where_ = match unit_scalar(raw, "Mount", "Where") {
        Some(v) => Some(crate::unit::parse::unquote_scalar(&exp(v))?),
        None => mount_path_from_unit_name(&spec.unit_name),
    };
    Ok(MountConfig {
        what: unit_scalar(raw, "Mount", "What").map(exp),
        where_,
        fs_type: unit_scalar(raw, "Mount", "Type").map(exp),
        options: unit_scalar(raw, "Mount", "Options").map(exp),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> SpecifierContext {
        SpecifierContext {
            unit_name: name.into(),
            runtime_dir: "/run".into(),
            user_name: "alice".into(),
            uid: "1000".into(),
            home: "/home/alice".into(),
            hostname: "box".into(),
            machine_id: "m".into(),
        }
    }

    fn build_str(text: &str, name: &str) -> Result<UnitFile, String> {
        let raw = parse::parse(text).map_err(|e| e.to_string())?;
        build(&raw, &spec(name))
    }

    #[test]
    fn parses_service() {
        let f = build_str(
            "[Unit]\nDescription=My Service\n[Service]\nExecStart=/bin/sleep 100\nRestart=always\n",
            "my.service",
        )
        .unwrap();
        assert_eq!(f.unit.description, "My Service");
        assert_eq!(f.service.as_ref().unwrap().restart, RestartPolicy::Always);
        assert_eq!(
            f.service.as_ref().unwrap().exec_start[0].argv,
            vec!["/bin/sleep", "100"]
        );
    }

    #[test]
    fn oneshot_remain_after() {
        let f = build_str(
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
            "x.service",
        )
        .unwrap();
        let s = f.service.as_ref().unwrap();
        assert_eq!(s.service_type, ServiceType::Oneshot);
        assert!(s.remain_after_exit);
    }

    #[test]
    fn dbus_type_and_bus_name() {
        let f = build_str(
            "[Service]\nType=dbus\nBusName=com.example.Foo\nExecStart=/bin/foo\n",
            "dbus.service",
        )
        .unwrap();
        let s = f.service.as_ref().unwrap();
        assert_eq!(s.service_type, ServiceType::Dbus);
        assert_eq!(s.bus_name.as_deref(), Some("com.example.Foo"));
    }

    #[test]
    fn dbus_type_without_bus_name_still_parses() {
        // BusName= is optional at parse time; the manager enforces its
        // presence for Type=dbus at start time.
        let f = build_str("[Service]\nType=dbus\nExecStart=/bin/foo\n", "dbus.service").unwrap();
        let s = f.service.as_ref().unwrap();
        assert_eq!(s.service_type, ServiceType::Dbus);
        assert!(s.bus_name.is_none());
    }

    #[test]
    fn exit_status_parsing() {
        let set = parse_exit_status("0 2..4 SIGTERM").unwrap();
        assert!(set.matches(Some(0), None));
        assert!(set.matches(Some(3), None));
        assert!(!set.matches(Some(5), None));
        assert!(set.matches(None, Some(15)));
        assert!(!set.matches(None, Some(9)));
    }

    #[test]
    fn signal_parser_accepts_numeric_wire_values() {
        assert_eq!(sig_from_name("9"), Some(Signal::SIGKILL));
        assert_eq!(sig_from_name("15"), Some(Signal::SIGTERM));
    }

    #[test]
    fn sysexit_parsing() {
        let set = parse_exit_status("DATAERR").unwrap();
        assert!(set.matches(Some(65), None));
        let set = parse_exit_status("EX_TEMPFAIL ex_config SUCCESS").unwrap();
        assert!(set.matches(Some(75), None));
        assert!(set.matches(Some(78), None));
        assert!(set.matches(Some(0), None));
    }

    #[test]
    fn effective_success_keeps_exit_zero() {
        let cfg = ServiceConfig {
            success_exit_status: Some(parse_exit_status("DATAERR").unwrap()),
            ..Default::default()
        };
        let set = cfg.effective_exit_success();
        assert!(set.matches(Some(0), None)); // exit 0 is always success
        assert!(set.matches(Some(65), None)); // DATAERR
        assert!(!set.matches(Some(1), None)); // anything else fails
    }

    #[test]
    fn environment_and_files() {
        let f = build_str(
            "[Service]\nEnvironment=\"A=1\" \"B=two words\"\nEnvironmentFile=-/etc/env\n",
            "e.service",
        )
        .unwrap();
        let s = f.service.as_ref().unwrap();
        assert_eq!(
            s.environment,
            vec![("A".into(), "1".into()), ("B".into(), "two words".into())]
        );
        assert_eq!(s.environment_files, vec![("/etc/env".into(), true)]);
    }

    #[test]
    fn exec_quoting() {
        let f = build_str(
            "[Service]\nExecStart=/bin/sh -c 'echo \"hi world\"'\n",
            "q.service",
        )
        .unwrap();
        assert_eq!(
            f.service.as_ref().unwrap().exec_start[0].argv,
            vec!["/bin/sh", "-c", "echo \"hi world\""]
        );
    }

    #[test]
    fn specifier_expansion_in_values() {
        let f = build_str(
            "[Service]\nWorkingDirectory=%h\nExecStart=%p/bin/run\n",
            "app.service",
        )
        .unwrap();
        let s = f.service.as_ref().unwrap();
        assert_eq!(s.working_directory, Some(("/home/alice".into(), false)));
        assert_eq!(s.exec_start[0].argv, vec!["app/bin/run"]);
    }

    #[test]
    fn timer_parsing() {
        let f = build_str(
            "[Timer]\nOnCalendar=daily\nOnBootSec=5min\nPersistent=yes\nUnit=backup.service\n",
            "backup.timer",
        )
        .unwrap();
        let t = f.timer.as_ref().unwrap();
        assert_eq!(t.on_calendar.len(), 1);
        assert_eq!(t.on_boot_sec.len(), 1);
        assert!(t.persistent);
        assert_eq!(t.unit.as_deref(), Some("backup.service"));
    }

    #[test]
    fn cgroup_resource_directives() {
        let f = build_str(
            "[Service]\nExecStart=/bin/true\nMemoryMax=512M\nMemoryHigh=100M\nCPUWeight=256\nTasksMax=64\n",
            "r.service",
        )
        .unwrap();
        let l = f.service.as_ref().unwrap().cgroup_limits;
        assert_eq!(l.memory_max, Some(512 * 1024 * 1024));
        assert_eq!(l.memory_high, Some(100 * 1024 * 1024));
        assert_eq!(l.cpu_weight, Some(256));
        assert_eq!(l.tasks_max, Some(64));
    }

    #[test]
    fn cgroup_infinity_and_rejects() {
        let f = build_str(
            "[Service]\nMemoryMax=infinity\nTasksMax=infinity\n",
            "i.service",
        )
        .unwrap();
        let l = f.service.as_ref().unwrap().cgroup_limits;
        assert_eq!(l.memory_max, Some(u64::MAX));
        assert_eq!(l.tasks_max, Some(u64::MAX));

        assert!(build_str("[Service]\nCPUWeight=0\n", "b.service").is_err());
        assert!(build_str("[Service]\nCPUWeight=10001\n", "b.service").is_err());
        assert!(build_str("[Service]\nMemoryMax=bogus\n", "b.service").is_err());
    }

    #[test]
    fn dropin_override_semantics() {
        // Simulate a main file plus a drop-in appended: scalars last-wins,
        // list keys keep order.
        let combined = "[Unit]\nDescription=base\n[Service]\nExecStart=/bin/one\n".to_string()
            + "[Unit]\nDescription=override\n[Service]\nExecStart=/bin/two\n";
        let f = build_str(&combined, "o.service").unwrap();
        assert_eq!(f.unit.description, "override");
        assert_eq!(f.service.as_ref().unwrap().exec_start.len(), 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mount_path_derivation() {
        assert_eq!(
            mount_path_from_unit_name("tmp-demo.mount").as_deref(),
            Some("/tmp/demo")
        );
        assert_eq!(
            mount_path_from_unit_name("var-log.mount").as_deref(),
            Some("/var/log")
        );
        // A literal `-` in the path is `\x2d` in the unit name.
        assert_eq!(
            mount_path_from_unit_name("tmp-my\\x2ddir.mount").as_deref(),
            Some("/tmp/my-dir")
        );
        // A non-`.mount` suffix yields nothing.
        assert_eq!(mount_path_from_unit_name("tmp-demo.service"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mount_parsing_and_name_derivation() {
        // Explicit Where= wins over the name-derived default.
        let f = build_str(
            "[Mount]\nWhat=tmpfs\nWhere=/run/explicit\nType=tmpfs\nOptions=mode=1777,size=64m\n",
            "tmp-demo.mount",
        )
        .unwrap();
        let m = f.mount.as_ref().unwrap();
        assert_eq!(m.what.as_deref(), Some("tmpfs"));
        assert_eq!(m.where_.as_deref(), Some("/run/explicit"));
        assert_eq!(m.fs_type.as_deref(), Some("tmpfs"));
        assert_eq!(m.options.as_deref(), Some("mode=1777,size=64m"));
        assert_eq!(f.kind(), UnitKind::Mount);

        // No Where= → derived from the unit name (`tmp-demo.mount` → /tmp/demo).
        let f = build_str("[Mount]\nWhat=tmpfs\nType=tmpfs\n", "tmp-demo.mount").unwrap();
        assert_eq!(
            f.mount.as_ref().unwrap().where_.as_deref(),
            Some("/tmp/demo")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unit_kind_suffix_mount() {
        assert_eq!(UnitKind::Mount.suffix(), "mount");
        assert_eq!(UnitKind::from_suffix("mount"), Some(UnitKind::Mount));
        assert_eq!(
            UnitKind::from_unit_name("tmp-demo.mount"),
            Some(UnitKind::Mount)
        );
    }
}
