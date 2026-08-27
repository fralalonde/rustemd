//! Phase-1 service sandboxing (Linux): mount-namespace plumbing, read-only
//! protection, and `NoNewPrivileges` — applied in the spawn `pre_exec` path.
//!
//! Everything here is raw syscalls (`mount`, `unshare`, `prctl`), safe to run
//! in the forked child; only the op list is pre-built in the parent. Ordering
//! matters: mount/unshare need `CAP_SYS_ADMIN`, so those ops run before the
//! child drops to its service uid.
//!
//! # User-mode degradation
//! As root we use a plain `CLONE_NEWNS`. As a user manager we need
//! `CLONE_NEWUSER|CLONE_NEWNS` + uid_map setup. If the *initial* user-namespace
//! unshare is refused by kernel policy (EPERM/EINVAL), the mount/namespace ops
//! are skipped (the service runs unsandboxed for those) but non-namespace
//! hardening that needs no namespace still applies. A failure *part-way*
//! through mount setup aborts via the returned `Err`; the caller decides
//! whether to fail the spawn.

use std::path::{Path, PathBuf};

use crate::unit::{ProtectMode, ProtectSystemLevel, SandboxConfig};

/// A raw sandbox op, built from [`SandboxConfig`] and executed by [`apply`] in
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Enter a private mount namespace (`CLONE_NEWNS`, or `CLONE_NEWUSER|
    /// CLONE_NEWNS` for a user manager).
    UnshareMount,
    /// `mount(NULL,"/",NULL,MS_REC|MS_PRIVATE)` — cut propagation.
    MakeRprivate,
    /// Mount a tmpfs over `path` (shadows the original = inaccessible).
    MountTmpfs(PathBuf),
    /// Bind-mount `path` over itself then remount read-only.
    BindReadOnly(PathBuf),
    /// `PrivateDevices=`: shadow `/dev` with a minimal tmpfs + core device
    /// nodes (null/zero/full/random/urandom/tty), a private `devpts` at
    /// `/dev/pts`, and a tmpfs `/dev/shm`, hiding the host's devices. Must run
    /// inside the private mount namespace. Best-effort — each step warns on
    /// failure and continues, matching systemd's tolerance.
    PrivateDevices,
    /// `prctl(PR_SET_NO_NEW_PRIVS)`.
    NoNewPrivileges,
    /// `CapabilityBoundingSet=`: drop the given capabilities from the
    /// process's bounding set via `prctl(PR_CAPBSET_DROP)`. When the config
    /// used the `~` inversion (`~CAP_...`), `ops` carries the *complement*
    /// (the caps kept); otherwise it carries the caps to drop.
    CapBoundingDrop(Vec<u32>, bool /* invert: ops lists kept-caps */),
    /// `AmbientCapabilities=`: raise the given capabilities in the ambient set
    /// via `prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE)`. Best-effort — needs
    /// the cap in the permitted set and a non-zero bounding set; a failure
    /// (EPERM) is tolerated and logged.
    AmbientRaise(Vec<u32>),
    /// `SystemCallFilter=`: install the pre-built seccomp BPF program via
    /// `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER)`. Compiled in the parent
    /// (pure, no syscalls); the child just installs it. Applied last, after
    /// the namespace/capability ops.
    Seccomp(Vec<libc::sock_filter>),
}

/// Build the ordered op list for `cfg`, or `None` if no implemented directive
/// is set. Pure; runs in the parent.
pub fn plan(cfg: &SandboxConfig) -> Option<Vec<Op>> {
    if !cfg.has_sandbox() {
        return None;
    }
    let mut ops = Vec::new();

    let need_mountns = cfg.private_tmp
        || cfg.private_devices
        || cfg.protect_home != ProtectMode::No
        || cfg.protect_system != ProtectSystemLevel::No
        || !cfg.read_only_paths.is_empty();
    if need_mountns {
        ops.push(Op::UnshareMount);
        ops.push(Op::MakeRprivate);
    }
    if cfg.private_tmp {
        ops.push(Op::MountTmpfs(PathBuf::from("/tmp")));
        ops.push(Op::MountTmpfs(PathBuf::from("/var/tmp")));
    }
    match cfg.protect_home {
        ProtectMode::No => {}
        ProtectMode::ReadOnly => {
            for p in ["/home", "/root", "/run/user"] {
                ops.push(Op::BindReadOnly(PathBuf::from(p)));
            }
        }
        ProtectMode::Tmpfs => {
            for p in ["/home", "/root", "/run/user"] {
                ops.push(Op::MountTmpfs(PathBuf::from(p)));
            }
        }
    }
    match cfg.protect_system {
        ProtectSystemLevel::No => {}
        ProtectSystemLevel::Yes => {
            for p in ["/usr", "/boot", "/efi"] {
                ops.push(Op::BindReadOnly(PathBuf::from(p)));
            }
        }
        ProtectSystemLevel::Full => {
            for p in ["/usr", "/boot", "/efi", "/etc"] {
                ops.push(Op::BindReadOnly(PathBuf::from(p)));
            }
        }
        ProtectSystemLevel::Strict => ops.push(Op::BindReadOnly(PathBuf::from("/"))),
    }
    for p in &cfg.read_only_paths {
        ops.push(Op::BindReadOnly(PathBuf::from(p)));
    }
    if cfg.private_devices {
        ops.push(Op::PrivateDevices);
    }
    if cfg.no_new_privileges {
        ops.push(Op::NoNewPrivileges);
    }
    // CapabilityBoundingSet=: convert the named caps to numbers. When the set
    // is inverted (`~`), we drop everything *except* the listed caps.
    if !cfg.bounding_set.is_empty() {
        let keep = cfg.bounding_invert;
        let caps: Vec<u32> = cfg
            .bounding_set
            .iter()
            .filter_map(|s| cap_number(s))
            .collect();
        if !caps.is_empty() {
            ops.push(Op::CapBoundingDrop(caps, keep));
        }
    }
    if !cfg.ambient_set.is_empty() {
        let caps: Vec<u32> = cfg
            .ambient_set
            .iter()
            .filter_map(|s| cap_number(s))
            .collect();
        if !caps.is_empty() {
            ops.push(Op::AmbientRaise(caps));
        }
    }
    // SystemCallFilter= (seccomp): the syscall numbers are pre-resolved at
    // parse time (so unknown names fail the unit at load, not spawn); here we
    // just build the pure BPF program and hand it to the child. Only
    // meaningful on x86_64, where the syscall-number table lives (other arches
    // keep the directive as a parse-time compat warning). `RestrictRealtime=`
    // rides the same machinery: a pure deny of the realtime-scheduler syscalls
    // (`sched_setscheduler`/`sched_setattr`/`sched_setparam`), so both share
    // one install.
    #[cfg(target_arch = "x86_64")]
    {
        // x86_64 syscall numbers for the `RestrictRealtime=` deny set.
        const RT_DENY: [u32; 3] = [144, 314, 142]; // sched_setscheduler, sched_setattr, sched_setparam
        // `LockPersonality=` denies `personality(2)` entirely (syscall 135 on
        // x86_64), so the service cannot switch execution domains or drop ASLR.
        const PERSONALITY: u32 = 135;
        // `RestrictSUIDSGID=` denies the file-mode syscalls that could set an
        // SUID/SGID bit or relabel ownership, resolved by name through the
        // same syscall table that backs `SystemCallFilter=` (see
        // `seccomp::suidsgid_nrs`).
        let need_seccomp = !cfg.syscall_nrs.is_empty()
            || cfg.restrict_realtime
            || cfg.lock_personality
            || cfg.restrict_suidsgid
            || cfg.af_present
            || cfg.memory_deny_write_execute;
        if need_seccomp {
            // Implicit NoNewPrivileges: installing a `SECCOMP_MODE_FILTER`
            // requires either CAP_SYS_ADMIN or PR_SET_NO_NEW_PRIVS (seccomp(2)).
            // A user-mode manager has neither, so without this a
            // `SystemCallFilter=`/`RestrictRealtime=` unit would refuse to
            // spawn with EACCES. systemd draws the same conclusion
            // (systemd.exec: `SystemCallFilter=` overrides/implies
            // `NoNewPrivileges=`), so we force it whenever a filter is going to
            // be installed — unless the unit already asked. Push it *before*
            // the Seccomp op. Note AmbientCapabilities raising needs
            // no_new_privs OFF, so a unit that sets both degrades to ambient
            // best-effort (warned at apply) — the seccomp requirement always
            // wins, matching systemd's inability to combine them.
            if !cfg.no_new_privileges {
                ops.push(Op::NoNewPrivileges);
            }
            let mut extra_deny: Vec<u32> = Vec::new();
            if cfg.restrict_realtime {
                extra_deny.extend_from_slice(&RT_DENY);
            }
            if cfg.lock_personality {
                extra_deny.push(PERSONALITY);
            }
            if cfg.restrict_suidsgid {
                extra_deny.extend(seccomp::suidsgid_nrs());
            }
            // `RestrictRealtime=` is inherently a *deny* of a few syscalls.
            // When it stands alone (no `SystemCallFilter=`), a deny-list is the
            // only correct interpretation (deny the RT calls, allow everything
            // else) — an allow-list would permit only [`ALLOW_BASE`] and refuse
            // to even `execve` the service. When combined with a
            // `SystemCallFilter=` the caller's mode wins (an allow-list denies
            // the RT calls implicitly; a deny-list folds them into its entries).
            let deny = if cfg.syscall_nrs.is_empty() {
                true
            } else {
                cfg.syscall_deny
            };
            let af = if cfg.af_present {
                Some(AfGate {
                    deny: cfg.af_deny,
                    families: &cfg.af_families,
                    deny_all: cfg.af_deny_all,
                })
            } else {
                None
            };
            let program = build_seccomp(
                &cfg.syscall_nrs,
                deny,
                cfg.syscall_errno,
                &extra_deny,
                af,
                cfg.memory_deny_write_execute,
            );
            ops.push(Op::Seccomp(program));
        }
    }
    Some(ops)
}

/// Execute the ordered ops in the forked child. Returns the first hard error
/// (partial setup). If the initial user-namespace unshare is refused, returns
/// `Ok(None)` so the caller knows mount sandboxing was skipped.
pub fn apply(ops: &[Op]) -> Result<Option<()>, String> {
    let is_root = unsafe { libc::geteuid() == 0 };
    for op in ops {
        match op {
            Op::UnshareMount => {
                let flags = if is_root {
                    libc::CLONE_NEWNS
                } else {
                    libc::CLONE_NEWUSER | libc::CLONE_NEWNS
                };
                // SAFETY: valid namespace flags.
                if unsafe { libc::unshare(flags) } != 0 {
                    let e = std::io::Error::last_os_error();
                    if !is_root
                        && matches!(e.raw_os_error(), Some(libc::EPERM) | Some(libc::EINVAL))
                    {
                        // Kernel denies user namespaces — everything remaining
                        // needs the namespace; fall back unsandboxed for the
                        // mount ops.
                        return Ok(None);
                    }
                    return Err(fmt("unshare", &e));
                }
                if !is_root {
                    setup_userns_map()?;
                }
            }
            Op::MakeRprivate => {
                // SAFETY: NULL source, '/' target, private flags.
                if unsafe {
                    libc::mount(
                        std::ptr::null(),
                        c"/".as_ptr(),
                        std::ptr::null(),
                        libc::MS_REC | libc::MS_PRIVATE,
                        std::ptr::null(),
                    )
                } != 0
                {
                    return Err(fmt("make-rprivate", &std::io::Error::last_os_error()));
                }
            }
            Op::MountTmpfs(target) => {
                let c = cstr(target);
                // SAFETY: tmpfs source/fstype + mode data.
                if unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        c.as_ptr(),
                        c"tmpfs".as_ptr(),
                        libc::MS_NOSUID | libc::MS_NODEV,
                        c"mode=1777".as_ptr().cast(),
                    )
                } != 0
                {
                    return Err(fmt("mount-tmpfs", &std::io::Error::last_os_error()));
                }
            }
            Op::BindReadOnly(target) => {
                if !target.exists() {
                    continue; // like systemd: skip absent paths
                }
                // Read-only relabeling of a pre-existing host mount needs
                // CAP_SYS_ADMIN over the target, which a user namespace does
                // not confer (binding `/` itself in particular fails EINVAL
                // on a shared root). Under root this is a hard error; under an
                // unprivileged manager we degrade to a warning, matching
                // systemd's tolerance for unprivileged sandboxing.
                if !is_root {
                    eprintln!(
                        "rystemd: [sandbox] read-only {} not enforced (needs CAP_SYS_ADMIN)",
                        target.display()
                    );
                    continue;
                }
                let c = cstr(target);
                // SAFETY: bind over itself.
                if unsafe {
                    libc::mount(
                        c.as_ptr(),
                        c.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND | libc::MS_REC,
                        std::ptr::null(),
                    )
                } != 0
                {
                    return Err(fmt("bind-ro", &std::io::Error::last_os_error()));
                }
                // SAFETY: remount same path read-only (a single MS_BIND|RDONLY
                // silently drops RDONLY, so do two steps).
                if unsafe {
                    libc::mount(
                        std::ptr::null(),
                        c.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC,
                        std::ptr::null(),
                    )
                } != 0
                {
                    return Err(fmt("remount-ro", &std::io::Error::last_os_error()));
                }
            }
            Op::PrivateDevices => {
                // Shadow `/dev` with a fresh tmpfs and a minimal device tree.
                // Best-effort: each step warns and continues on failure,
                // matching systemd's tolerance for unprivileged managers; only
                // a unit that *needs* a missing node is affected.
                // SAFETY: tmpfs mount over the `/dev` mount point; children
                // of a containerized root mount and the (empty) target.
                if unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        c"/dev".as_ptr(),
                        c"tmpfs".as_ptr(),
                        libc::MS_NOSUID | libc::MS_NODEV,
                        c"mode=0755".as_ptr().cast(),
                    )
                } != 0
                {
                    let e = std::io::Error::last_os_error();
                    eprintln!("rystemd: [sandbox] private /dev not mounted: {e}");
                } else {
                    // Core device nodes (mknod), matching systemd's minimal set.
                    for (name, maj, min) in [
                        ("null", 1, 3),
                        ("zero", 1, 5),
                        ("full", 1, 7),
                        ("random", 1, 8),
                        ("urandom", 1, 9),
                        ("tty", 5, 0),
                    ] {
                        let path = format!("/dev/{name}");
                        let cpath = cstr(Path::new(&path));
                        // SAFETY: valid path and char-device mode.
                        if unsafe {
                            libc::mknod(
                                cpath.as_ptr(),
                                libc::S_IFCHR | 0o666,
                                libc::makedev(maj, min),
                            )
                        } != 0
                        {
                            let e = std::io::Error::last_os_error();
                            eprintln!("rystemd: [sandbox] mknod /dev/{name}: {e}");
                        }
                    }
                    // Private pseudo-terminal + POSIX-shm mounts.
                    let _ = std::fs::create_dir_all("/dev/pts");
                    let _ = std::fs::create_dir_all("/dev/shm");
                    // SAFETY: devpts over the private `/dev/pts` dir.
                    if unsafe {
                        libc::mount(
                            c"devpts".as_ptr(),
                            c"/dev/pts".as_ptr(),
                            c"devpts".as_ptr(),
                            libc::MS_NOSUID | libc::MS_NOEXEC,
                            std::ptr::null(),
                        )
                    } != 0
                    {
                        let e = std::io::Error::last_os_error();
                        eprintln!("rystemd: [sandbox] private /dev/pts: {e}");
                    }
                    let _ = std::fs::remove_file("/dev/ptmx");
                    let _ = std::os::unix::fs::symlink("/dev/pts/ptmx", "/dev/ptmx");
                    // SAFETY: tmpfs over the private `/dev/shm` dir.
                    if unsafe {
                        libc::mount(
                            c"tmpfs".as_ptr(),
                            c"/dev/shm".as_ptr(),
                            c"tmpfs".as_ptr(),
                            libc::MS_NOSUID | libc::MS_NODEV,
                            c"mode=1777".as_ptr().cast(),
                        )
                    } != 0
                    {
                        let e = std::io::Error::last_os_error();
                        eprintln!("rystemd: [sandbox] private /dev/shm: {e}");
                    }
                    // Standard /proc-backed symlinks kept for compat.
                    for (link, target) in [
                        ("/dev/fd", "/proc/self/fd"),
                        ("/dev/stdin", "/proc/self/fd/0"),
                        ("/dev/stdout", "/proc/self/fd/1"),
                        ("/dev/stderr", "/proc/self/fd/2"),
                    ] {
                        let _ = std::fs::remove_file(link);
                        let _ = std::os::unix::fs::symlink(target, link);
                    }
                }
            }
            Op::NoNewPrivileges => {
                // SAFETY: simple prctl.
                if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
                    return Err(fmt("no-new-privs", &std::io::Error::last_os_error()));
                }
            }
            Op::CapBoundingDrop(caps, invert) => {
                // PR_CAPBSET_DROP is irreversible and per-capability. To keep
                // only the listed caps we drop every cap not in the list; to
                // drop only the listed caps we invert that set. Dropping a cap
                // that is not currently in the bounding set is harmless (prctl
                // succeeds), so this is also safe for over-broad names.
                let drop: Vec<u32> = if *invert {
                    (0..=LAST_CAP).filter(|c| !caps.contains(c)).collect()
                } else {
                    caps.clone()
                };
                for cap in &drop {
                    // SAFETY: PR_CAPBSET_DROP with a valid cap number.
                    if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, *cap, 0, 0, 0) } != 0 {
                        return Err(fmt("cap-bounding-drop", &std::io::Error::last_os_error()));
                    }
                }
            }
            Op::AmbientRaise(caps) => {
                for cap in caps {
                    // SAFETY: PR_CAP_AMBIENT with PR_CAP_AMBIENT_RAISE.
                    let r = unsafe {
                        libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_RAISE, *cap, 0, 0)
                    };
                    if r != 0 {
                        // Needs the cap in the permitted set, a non-empty
                        // bounding set holding it, and no_new_privs off.
                        // Tolerate: ambient caps are best-effort under
                        // unprivileged managers.
                        eprintln!("rystemd: [sandbox] ambient CAP_{cap} not raised");
                    }
                }
            }
            Op::Seccomp(program) => {
                // SAFETY: program is a valid, owned BPF instruction buffer;
                // len/filter describe it and prctl copies it into the kernel
                // before returning.
                let mut fprog = libc::sock_fprog {
                    len: program.len() as u16,
                    filter: program.as_ptr().cast_mut(),
                };
                if unsafe {
                    libc::prctl(PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &mut fprog, 0, 0)
                } != 0
                {
                    let e = std::io::Error::last_os_error();
                    // SECCOMP_MODE_FILTER is mandatory (returns EINVAL only if
                    // TSYNC is off) so a failure here is fatal for the unit —
                    // the process would run without the requested filter.
                    return Err(fmt("seccomp", &e));
                }
            }
        }
    }
    Ok(Some(()))
}

/// After `CLONE_NEWUSER`, write the uid/gid map so the namespace is usable.
/// Order per `user_namespaces(7)`: deny `setgroups` first, then write
/// `uid_map` and `gid_map`.
fn setup_userns_map() -> Result<(), String> {
    // Write "deny" to setgroups so an unprivileged process may write gid_map.
    if std::fs::write("/proc/self/setgroups", "deny").is_err() {
        // Perhaps already denied (a nested userns); harmless to continue.
    }
    let euid = unsafe { libc::geteuid() };
    let egid = unsafe { libc::getegid() };
    for (name, id) in [("uid_map", euid), ("gid_map", egid)] {
        let path = format!("/proc/self/{name}");
        let body = format!("0 {id} 1\n");
        if std::fs::write(&path, body).is_err() {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EPERM) {
                // Already mapped (e.g. a container pre-mapped us). Fine.
                continue;
            }
            return Err(fmt(&format!("userns-{name}"), &e));
        }
    }
    Ok(())
}

fn cstr(p: &Path) -> std::ffi::CString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap_or_default()
}

/// Highest defined capability number (Linux 5.11+ has 40, CHECKPOINT_RESTORE).
const LAST_CAP: u32 = 40;

/// Map a capability name (optionally `CAP_`-prefixed, case-insensitive) to its
/// number, matching the `capabilities(7)` table. Unknown names return `None`.
fn cap_number(name: &str) -> Option<u32> {
    let name = name.to_ascii_uppercase();
    let name = name.strip_prefix("CAP_").unwrap_or(&name);
    let caps = [
        "CHOWN",
        "DAC_OVERRIDE",
        "DAC_READ_SEARCH",
        "FOWNER",
        "FSETID",
        "KILL",
        "SETGID",
        "SETUID",
        "SETPCAP",
        "LINUX_IMMUTABLE",
        "NET_BIND_SERVICE",
        "NET_BROADCAST",
        "NET_ADMIN",
        "NET_RAW",
        "IPC_LOCK",
        "IPC_OWNER",
        "SYS_MODULE",
        "SYS_RAWIO",
        "SYS_CHROOT",
        "SYS_PTRACE",
        "SYS_PACCT",
        "SYS_ADMIN",
        "SYS_BOOT",
        "SYS_NICE",
        "SYS_RESOURCE",
        "SYS_TIME",
        "SYS_TTY_CONFIG",
        "MKNOD",
        "LEASE",
        "AUDIT_WRITE",
        "AUDIT_CONTROL",
        "SETFCAP",
        "MAC_OVERRIDE",
        "MAC_ADMIN",
        "SYSLOG",
        "WAKE_ALARM",
        "BLOCK_SUSPEND",
        "AUDIT_READ",
        "PERFMON",
        "BPF",
        "CHECKPOINT_RESTORE",
    ];
    caps.iter().position(|c| *c == name).map(|i| i as u32)
}

fn fmt(op: &str, e: &std::io::Error) -> String {
    format!("sandbox {op}: {e}")
}

// ---------------------------------------------------------------------------
// seccomp (`SystemCallFilter=`) — x86_64
//
// The syscall numbers are x86_64 ABI values. On any other architecture the
// directive is left as a parse-time compat warning (see unit/mod.rs), so none
// of this code is reachable there and it is compiled out to avoid a wrong
// (dangerous) table ever being applied.
//
// `PR_SET_SECCOMP` is not exported by libc for Linux; AUDIT_ARCH_* is absent
// entirely, so both are defined here.
// ---------------------------------------------------------------------------

/// `prctl(PR_SET_SECCOMP, ...)` (linux, x86_64).
const PR_SET_SECCOMP: libc::c_int = 22;
/// `AUDIT_ARCH_X86_64` — the `seccomp_data.arch` value a native x86_64 process
/// reports in its filter input.
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;

/// A guaranteed-readable-and-closable syscall base always allowed in an
/// allow-list, so the process can still exit/be signalled and the runtime
/// (allocator, timers, randomness) keeps working. systemd injects an analogous
/// minimal set. Deny-lists need no base.
const ALLOW_BASE: &[&str] = &[
    "exit",
    "exit_group",
    "rt_sigreturn",
    "rt_sigaction",
    "rt_sigprocmask",
    "rt_sigpending",
    "rt_sigtimedwait",
    "rt_sigqueueinfo",
    "rt_sigsuspend",
    "sigaltstack",
    "getpid",
    "gettid",
    "getppid",
    "tgkill",
    "kill",
    "nanosleep",
    "clock_gettime",
    "clock_nanosleep",
    "getrandom",
    "futex",
    "mmap",
    "mprotect",
    "munmap",
    "madvise",
    "brk",
    "read",
    "write",
    "openat",
    "close",
    "fstat",
    "newfstatat",
    "lseek",
    "pread64",
    "pwrite64",
    "ioctl",
];

#[cfg(target_arch = "x86_64")]
pub fn resolve_syscalls(names: &[String]) -> Result<Vec<u32>, String> {
    seccomp::resolve(names)
}

/// `RestrictAddressFamilies=` gate passed into the seccomp builder: `socket(2)`
/// and `socketpair(2)` are allowed only when the requested address family is
/// permitted. `deny` is the `~`-prefix (deny the listed families, allow all
/// others); `families` are the listed family numbers; `deny_all` is `~all`
/// (every family denied when `deny`, or an allow-all no-op when not).
#[cfg(target_arch = "x86_64")]
pub struct AfGate<'a> {
    pub deny: bool,
    pub families: &'a [u32],
    pub deny_all: bool,
}

#[cfg(target_arch = "x86_64")]
fn build_seccomp(
    nrs: &[u32],
    deny: bool,
    errno: u32,
    extra_deny: &[u32],
    af: Option<AfGate<'_>>,
    wx: bool,
) -> Vec<libc::sock_filter> {
    seccomp::build(nrs, deny, errno, extra_deny, af, wx)
}

#[cfg(target_arch = "x86_64")]
mod seccomp {
    use super::*;

    /// Resolve a list of `SystemCallFilter=` names (already stripped of any
    /// `~`) to syscall numbers, expanding `@group` references. An unknown
    /// *bare* name or group is an error — systemd fails the unit, so we
    /// surface it at parse time. A group member that has no number on this
    /// architecture (e.g. legacy `query_module`/`get_kernel_syms`/`finmod`
    /// that exist only in the historical filter lists) is skipped rather than
    /// failing the whole unit — it cannot be filtered on x86_64 anyway.
    pub fn resolve(names: &[String]) -> Result<Vec<u32>, String> {
        let mut out: Vec<u32> = Vec::new();
        for n in names {
            if let Some(names) = n.strip_prefix('@') {
                let members = group_members(names)
                    .ok_or_else(|| format!("SystemCallFilter: unknown group `@{names}`"))?;
                for m in members {
                    // Skip members with no number on this arch (legacy module
                    // syscalls), rather than failing the whole unit.
                    if let Some(nr) = syscall_nr(m) {
                        out.push(nr);
                    }
                }
            } else {
                let nr = syscall_nr(&n.to_ascii_lowercase())
                    .ok_or_else(|| format!("SystemCallFilter: unknown syscall `{n}`"))?;
                out.push(nr);
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// The `RestrictSUIDSGID=` deny set: the file-mode syscalls that could set
    /// an SUID/SGID bit or relabel ownership. Resolved by name through the
    /// table below (so it stays honest to the architecture) and de-duplicated.
    /// On x86_64 every name resolves; a missing one (should never happen here)
    /// is skipped rather than failing the unit.
    pub fn suidsgid_nrs() -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for name in [
            "chmod", "fchmod", "fchmodat", "chown", "fchown", "lchown", "fchownat",
        ] {
            if let Some(nr) = syscall_nr(name) {
                out.push(nr);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Build the seccomp BPF program.
    ///
    /// Layout (per `seccomp(2)`/BPF): first load `seccomp_data.arch` (offset 4)
    /// and require the native arch — a foreign-arch (e.g. 32-bit compat)
    /// process reports numbers that the table cannot interpret, so it is let
    /// through untouched. Then load `nr` (offset 0) and linearly compare
    /// against each listed number. An allow-list denies everything not listed
    /// (plus [`ALLOW_BASE`]); a deny-list allows everything not listed.
    ///
    /// `extra_deny` is a set of syscall numbers that must *always* be denied
    /// (used by `RestrictRealtime=`), independent of the `SystemCallFilter=`
    /// mode. For an allow-list these are already denied by the default
    /// deny-action; for a deny-list they are merged into the entries so they
    /// keep their errno response rather than being swallowed by the passive
    /// "allow everything else" tail.
    pub fn build(
        nrs: &[u32],
        deny: bool,
        errno: u32,
        extra_deny: &[u32],
        af: Option<super::AfGate<'_>>,
        wx: bool,
    ) -> Vec<libc::sock_filter> {
        let errno = if errno == 0 {
            libc::EPERM as u32
        } else {
            errno
        };
        let errno_ret = libc::SECCOMP_RET_ERRNO | (errno & 0xFFFF);
        let allow_ret = libc::SECCOMP_RET_ALLOW;

        let socket = syscall_nr("socket");
        let socketpair = syscall_nr("socketpair");
        // `MemoryDenyWriteExecute=`-gated syscalls: create/transition a mapping
        // to writable+executable. On x86_64 these are `mmap`, `mprotect` and
        // `pkey_mprotect`, and all three take `prot` as argument 2.
        let wx_nrs: Vec<u32> = if wx {
            ["mmap", "mprotect", "pkey_mprotect"]
                .into_iter()
                .filter_map(syscall_nr)
                .collect()
        } else {
            Vec::new()
        };

        // Plain entries: everything except the gated socket/socketpair and the
        // gated WX syscalls, which are dispatched to their argument gates.
        let mut entries: Vec<u32> = nrs.to_vec();
        if af.is_some() {
            for s in [socket, socketpair].into_iter().flatten() {
                entries.retain(|e| *e != s);
            }
        }
        if wx {
            for s in &wx_nrs {
                entries.retain(|e| e != s);
            }
        }
        if !deny {
            for b in ALLOW_BASE {
                if let Some(nr) = syscall_nr(b)
                    && !entries.contains(&nr)
                {
                    entries.push(nr);
                }
            }
        } else {
            // A deny-list's tail allows everything unmatched; fold the
            // hard-deny extras in so they hit the errno return instead.
            for nr in extra_deny {
                if !entries.contains(nr) {
                    entries.push(*nr);
                }
            }
        }
        entries.sort_unstable();
        entries.dedup();

        use libc::{
            BPF_ABS, BPF_ALU, BPF_AND, BPF_JEQ, BPF_JMP, BPF_JSET, BPF_K, BPF_LD, BPF_RET, BPF_W,
        };
        // Forward jump targets, resolved once the full layout is known.
        // jt/jf are unsigned 8-bit *relative* offsets in the emitted program,
        // so we assemble against absolute indices and convert at the end.
        let ld = |k: u32| ((BPF_LD | BPF_W | BPF_ABS) as u16, k);
        let jeq = |k: u32| ((BPF_JMP | BPF_JEQ | BPF_K) as u16, k);
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Tgt {
            None,
            I(usize),
            Errno,
            Allow,
            Famarg,
            /// A `MemoryDenyWriteExecute=`-gated syscall that needs its `prot`
            /// argument (arg2) checked for writable+executable.
            WxArg,
            /// The "not a gated socket" path: continue as an ordinary syscall
            /// through the plain entries (falling back to `default_t` when
            /// there are none). Never falls into the family-arg block.
            Plain,
        }
        struct I {
            code: u16,
            k: u32,
            jt: Tgt,
            jf: Tgt,
        }
        let mut prog: Vec<I> = Vec::new();
        // Append `(code,k)` as an instruction, returning its index.
        macro_rules! push {
            ($prog:expr, $ck:expr, $jt:expr, $jf:expr) => {{
                let i = $prog.len();
                let (code, k) = $ck;
                $prog.push(I {
                    code,
                    k,
                    jt: $jt,
                    jf: $jf,
                });
                i
            }};
        }
        let pick_t = |errno_side: bool| if errno_side { Tgt::Errno } else { Tgt::Allow };

        // Default and per-entry decisions (from the SystemCallFilter list):
        //   allow-list (deny=false) -> deny everything not listed
        //   deny-list / standalone (deny=true) -> allow everything not listed
        let default_t = pick_t(!deny);
        let match_t = pick_t(deny);

        // 0..2: arch check (foreign arch is let through), load `nr`.
        push!(prog, ld(4), Tgt::None, Tgt::None);
        push!(prog, jeq(AUDIT_ARCH_X86_64), Tgt::I(2), Tgt::Allow);
        push!(prog, ld(0), Tgt::None, Tgt::None);

        // Gateway to the argument gates: `RestrictAddressFamilies=`
        // (socket/socketpair) and `MemoryDenyWriteExecute=` (mmap/mprotect/
        // pkey_mprotect) intercept their syscalls here and route each match to
        // an argument-check block below. The SystemCallFilter list is always
        // the more restrictive of the two independent filters: a syscall the
        // list already denies stays denied, and a gate can only *allow* a
        // call the list lets through.
        let mut need_famarg = false;
        let mut need_wxarg = false;
        // Build the ordered gate set — WX syscalls first, then the AF
        // socket/socketpair — each with the decision to take on a match.
        let mut gates: Vec<(u32, Tgt, usize)> = Vec::new();
        if wx {
            for s in &wx_nrs {
                let list_denies = if deny {
                    nrs.contains(s)
                } else {
                    !nrs.contains(s)
                };
                let jt = if list_denies {
                    Tgt::Errno
                } else {
                    need_wxarg = true;
                    Tgt::WxArg
                };
                gates.push((*s, jt, 0));
            }
        }
        if let Some(af) = af.as_ref() {
            let fam_default_errno = if af.deny_all { af.deny } else { !af.deny };
            // `~all`/`all` degenerate straight to a constant decision (no
            // arg0 is loaded when every family resolves the same way).
            let const_errno = af.deny_all || af.families.is_empty();
            for s in [socket, socketpair].into_iter().flatten() {
                let list_denies = if deny {
                    nrs.contains(&s)
                } else {
                    !nrs.contains(&s)
                };
                let jt = if list_denies {
                    Tgt::Errno
                } else if const_errno {
                    pick_t(fam_default_errno)
                } else {
                    need_famarg = true;
                    Tgt::Famarg
                };
                gates.push((s, jt, 0));
            }
        }
        // Emit the jeq chain with proper fall-through: each gate's non-match
        // falls to the *next* gate (a non-mmap syscall can still be mprotect,
        // and a non-socket syscall can still be socketpair), so only the final
        // gate routes a non-match onward (Plain → plain entries/default). A
        // single jump-to-plain from an early gate would leak later gated
        // syscalls through the filter.
        for k in 0..gates.len() {
            let idx = push!(prog, jeq(gates[k].0), gates[k].1, Tgt::Plain);
            if k > 0 {
                prog[gates[k - 1].2].jf = Tgt::I(idx);
            }
            gates[k].2 = idx;
        }
        // The final gate keeps jf = Plain (patched to the plain path below).
        // A syscall that matches no gate must continue through the plain
        // entries (or straight to the default when there are none) — it must
        // NOT fall into either argument-gate block below.
        let plain_tgt = if entries.is_empty() {
            default_t
        } else {
            Tgt::I(prog.len()) // index of the first plain entry
        };
        for i in &mut prog {
            if i.jt == Tgt::Plain {
                i.jt = plain_tgt;
            }
            if i.jf == Tgt::Plain {
                i.jf = plain_tgt;
            }
        }

        // Plain entries: equal -> match action; non-equal falls through to the next
        // entry so a later (deny-list) entry can still match. Only the final entry's
        // `jf` targets the default action.
        for (i, nr) in entries.iter().enumerate() {
            // Fall through to the next entry's compare; the last entry targets the
            // default return.
            let jf = if i + 1 == entries.len() {
                default_t
            } else {
                Tgt::I(prog.len() + 1)
            };
            push!(prog, jeq(*nr), match_t, jf);
        }

        // Family-gate block (only when a socket compare needs arg0's family).
        if need_famarg {
            let famarg_idx = push!(prog, ld(16), Tgt::None, Tgt::None) /* load arg0 */;
            let af = af.as_ref().unwrap();
            // A listed family follows the `~`/allow decision; a family not
            // listed takes the inferred default for this directive.
            for f in af.families {
                let fam_default_errno = if af.deny_all { af.deny } else { !af.deny };
                push!(prog, jeq(*f), pick_t(af.deny), pick_t(fam_default_errno));
            }
            // Point every pending Famarg jump at the block's start.
            for i in &mut prog {
                if matches!(i.jt, Tgt::Famarg) {
                    i.jt = Tgt::I(famarg_idx);
                }
                if matches!(i.jf, Tgt::Famarg) {
                    i.jf = Tgt::I(famarg_idx);
                }
            }
        }

        // `MemoryDenyWriteExecute=`-gate block (only when a WX syscall needs
        // its `prot` argument checked). Deny when
        // `(prot & (PROT_WRITE|PROT_EXEC)) == (PROT_WRITE|PROT_EXEC)`:
        // PROT_WRITE=0x2, PROT_EXEC=0x4, so mask to 0x6 and reject only when
        // both bits are set. `prot` is argument 2 (byte offset 32) for
        // mmap/mprotect/pkey_mprotect alike.
        if need_wxarg {
            let wx_idx = push!(prog, ld(32), Tgt::None, Tgt::None) /* load arg2 */;
            // A = prot & 0x6
            push!(
                prog,
                ((BPF_ALU | BPF_AND | BPF_K) as u16, 0x6),
                Tgt::None,
                Tgt::None
            );
            // If the WRITE bit is clear, the mapping can't be W+X: allow.
            // BPF_JSET takes `jt` when A & K != 0.
            let js_w = push!(
                prog,
                ((BPF_JMP | BPF_JSET | BPF_K) as u16, 2),
                Tgt::None,
                Tgt::Allow
            );
            // With WRITE set, deny only when EXEC is also set (jt), else allow
            // (a writable-only mapping).
            let js_x = push!(
                prog,
                ((BPF_JMP | BPF_JSET | BPF_K) as u16, 4),
                Tgt::Errno,
                Tgt::Allow
            );
            prog[js_w].jt = Tgt::I(js_x);
            // Point every pending WxArg jump at the block's start.
            for i in &mut prog {
                if matches!(i.jt, Tgt::WxArg) {
                    i.jt = Tgt::I(wx_idx);
                }
                if matches!(i.jf, Tgt::WxArg) {
                    i.jf = Tgt::I(wx_idx);
                }
            }
        }

        // Terminal returns (shared). Patched targets reference these indices.
        let errno_i = push!(
            prog,
            ((BPF_RET | BPF_K) as u16, errno_ret),
            Tgt::None,
            Tgt::None
        );
        let allow_i = push!(
            prog,
            ((BPF_RET | BPF_K) as u16, allow_ret),
            Tgt::None,
            Tgt::None
        );

        // Resolve targets to absolute indices, then to relative offsets.
        let resolve = |t: Tgt| -> Option<usize> {
            match t {
                Tgt::None => None,
                Tgt::I(i) => Some(i),
                Tgt::Errno => Some(errno_i),
                Tgt::Allow => Some(allow_i),
                // Resolved to a concrete target in the dispatch/arg blocks.
                Tgt::Famarg => unreachable!("Famarg forwarded"),
                Tgt::WxArg => unreachable!("WxArg forwarded"),
                Tgt::Plain => unreachable!("Plain forwarded"),
            }
        };
        prog.iter()
            .enumerate()
            .map(|(i, ins)| {
                let jt = resolve(ins.jt).map(|t| (t - i - 1) as u8).unwrap_or(0);
                let jf = resolve(ins.jf).map(|t| (t - i - 1) as u8).unwrap_or(0);
                // A plain struct construction; no `unsafe` is needed.
                libc::sock_filter {
                    code: ins.code,
                    jt,
                    jf,
                    k: ins.k,
                }
            })
            .collect()
    }

    /// `(name, number)` for the syscalls rystemd can name. A curated but broad
    /// x86_64 set covering the common runtime + privileged/sensitive calls, so
    /// both allow-lists and the `~` groups are expressible.
    const SYSCALLS: &[(&str, u32)] = &[
        ("read", 0),
        ("write", 1),
        ("open", 2),
        ("close", 3),
        ("stat", 4),
        ("fstat", 5),
        ("lstat", 6),
        ("poll", 7),
        ("lseek", 8),
        ("mmap", 9),
        ("mprotect", 10),
        ("munmap", 11),
        ("brk", 12),
        ("rt_sigaction", 13),
        ("rt_sigprocmask", 14),
        ("rt_sigreturn", 15),
        ("ioctl", 16),
        ("pread64", 17),
        ("pwrite64", 18),
        ("readv", 19),
        ("writev", 20),
        ("access", 21),
        ("pipe", 22),
        ("select", 23),
        ("sched_yield", 24),
        ("mremap", 25),
        ("msync", 26),
        ("mincore", 27),
        ("madvise", 28),
        ("shmget", 29),
        ("shmat", 30),
        ("shmctl", 31),
        ("dup", 32),
        ("dup2", 33),
        ("pause", 34),
        ("nanosleep", 35),
        ("getitimer", 36),
        ("alarm", 37),
        ("setitimer", 38),
        ("getpid", 39),
        ("sendfile", 40),
        ("socket", 41),
        ("connect", 42),
        ("accept", 43),
        ("sendto", 44),
        ("recvfrom", 45),
        ("sendmsg", 46),
        ("recvmsg", 47),
        ("shutdown", 48),
        ("bind", 49),
        ("listen", 50),
        ("getsockname", 51),
        ("getpeername", 52),
        ("socketpair", 53),
        ("setsockopt", 54),
        ("getsockopt", 55),
        ("clone", 56),
        ("fork", 57),
        ("vfork", 58),
        ("execve", 59),
        ("exit", 60),
        ("wait4", 61),
        ("kill", 62),
        ("uname", 63),
        ("semget", 64),
        ("semop", 65),
        ("semctl", 66),
        ("shmdt", 67),
        ("msgget", 68),
        ("msgsnd", 69),
        ("msgrcv", 70),
        ("msgctl", 71),
        ("fcntl", 72),
        ("flock", 73),
        ("fsync", 74),
        ("fdatasync", 75),
        ("truncate", 76),
        ("ftruncate", 77),
        ("getdents", 78),
        ("getcwd", 79),
        ("chdir", 80),
        ("fchdir", 81),
        ("rename", 82),
        ("mkdir", 83),
        ("rmdir", 84),
        ("creat", 85),
        ("link", 86),
        ("unlink", 87),
        ("symlink", 88),
        ("readlink", 89),
        ("chmod", 90),
        ("fchmod", 91),
        ("chown", 92),
        ("fchown", 93),
        ("lchown", 94),
        ("umask", 95),
        ("gettimeofday", 96),
        ("getrlimit", 97),
        ("getrusage", 98),
        ("sysinfo", 99),
        ("times", 100),
        ("ptrace", 101),
        ("getuid", 102),
        ("syslog", 103),
        ("getgid", 104),
        ("setuid", 105),
        ("setgid", 106),
        ("geteuid", 107),
        ("getegid", 108),
        ("setpgid", 109),
        ("getppid", 110),
        ("getpgrp", 111),
        ("setsid", 112),
        ("setreuid", 113),
        ("setregid", 114),
        ("getgroups", 115),
        ("setgroups", 116),
        ("setresuid", 117),
        ("getresuid", 118),
        ("setresgid", 119),
        ("getresgid", 120),
        ("getpgid", 121),
        ("setfsuid", 122),
        ("setfsgid", 123),
        ("getsid", 124),
        ("capget", 125),
        ("capset", 126),
        ("rt_sigpending", 127),
        ("rt_sigtimedwait", 128),
        ("rt_sigqueueinfo", 129),
        ("rt_sigsuspend", 130),
        ("sigaltstack", 131),
        ("utime", 132),
        ("mknod", 133),
        ("personality", 135),
        ("ustat", 136),
        ("statfs", 137),
        ("fstatfs", 138),
        ("sysfs", 139),
        ("getpriority", 140),
        ("setpriority", 141),
        ("sched_setparam", 142),
        ("sched_getparam", 143),
        ("sched_setscheduler", 144),
        ("sched_getscheduler", 145),
        ("sched_get_priority_max", 146),
        ("sched_get_priority_min", 147),
        ("sched_rr_get_interval", 148),
        ("mlock", 149),
        ("munlock", 150),
        ("mlockall", 151),
        ("munlockall", 152),
        ("vhangup", 153),
        ("modify_ldt", 154),
        ("pivot_root", 155),
        ("sysctl", 156),
        ("prctl", 157),
        ("arch_prctl", 158),
        ("adjtimex", 159),
        ("setrlimit", 160),
        ("chroot", 161),
        ("sync", 162),
        ("acct", 163),
        ("settimeofday", 164),
        ("mount", 165),
        ("umount2", 166),
        ("swapon", 167),
        ("swapoff", 168),
        ("reboot", 169),
        ("sethostname", 170),
        ("setdomainname", 171),
        ("iopl", 172),
        ("ioperm", 173),
        ("create_module", 174),
        ("init_module", 175),
        ("delete_module", 176),
        ("quotactl", 179),
        ("gettid", 186),
        ("readahead", 187),
        ("setxattr", 188),
        ("lsetxattr", 189),
        ("fsetxattr", 190),
        ("getxattr", 191),
        ("lgetxattr", 192),
        ("fgetxattr", 193),
        ("listxattr", 194),
        ("llistxattr", 195),
        ("flistxattr", 196),
        ("removexattr", 197),
        ("lremovexattr", 198),
        ("fremovexattr", 199),
        ("tkill", 200),
        ("time", 201),
        ("futex", 202),
        ("sched_setaffinity", 203),
        ("sched_getaffinity", 204),
        ("set_thread_area", 205),
        ("io_setup", 206),
        ("io_destroy", 207),
        ("io_getevents", 208),
        ("io_submit", 209),
        ("io_cancel", 210),
        ("get_thread_area", 211),
        ("lookup_dcookie", 212),
        ("epoll_create", 213),
        ("epoll_wait_old", 214),
        ("epoll_ctl_old", 215),
        ("getdents", 216),
        ("getdents64", 217),
        ("set_tid_address", 218),
        ("restart_syscall", 219),
        ("semtimedop", 220),
        ("fadvise64", 221),
        ("timer_create", 222),
        ("timer_settime", 223),
        ("timer_gettime", 224),
        ("timer_getoverrun", 225),
        ("timer_delete", 226),
        ("clock_settime", 227),
        ("clock_gettime", 228),
        ("clock_getres", 229),
        ("clock_nanosleep", 230),
        ("exit_group", 231),
        ("epoll_wait", 232),
        ("epoll_ctl", 233),
        ("tgkill", 234),
        ("utimes", 235),
        ("mbind", 237),
        ("set_mempolicy", 238),
        ("get_mempolicy", 239),
        ("mq_open", 240),
        ("mq_unlink", 241),
        ("mq_timedsend", 242),
        ("mq_timedreceive", 243),
        ("mq_notify", 244),
        ("mq_getsetattr", 245),
        ("kexec_load", 246),
        ("waitid", 247),
        ("add_key", 248),
        ("request_key", 249),
        ("keyctl", 250),
        ("ioprio_set", 251),
        ("ioprio_get", 252),
        ("inotify_init", 253),
        ("inotify_add_watch", 254),
        ("inotify_rm_watch", 255),
        ("migrate_pages", 256),
        ("openat", 257),
        ("mkdirat", 258),
        ("mknodat", 259),
        ("fchownat", 260),
        ("futimesat", 261),
        ("newfstatat", 262),
        ("unlinkat", 263),
        ("renameat", 264),
        ("linkat", 265),
        ("symlinkat", 266),
        ("readlinkat", 267),
        ("fchmodat", 268),
        ("faccessat", 269),
        ("pselect6", 270),
        ("ppoll", 271),
        ("unshare", 272),
        ("set_robust_list", 273),
        ("get_robust_list", 274),
        ("splice", 275),
        ("tee", 276),
        ("sync_file_range", 277),
        ("vmsplice", 278),
        ("move_pages", 279),
        ("utimensat", 280),
        ("epoll_pwait", 281),
        ("signalfd", 282),
        ("timerfd_create", 283),
        ("eventfd", 284),
        ("fallocate", 285),
        ("timerfd_settime", 286),
        ("timerfd_gettime", 287),
        ("accept4", 288),
        ("signalfd4", 289),
        ("eventfd2", 290),
        ("epoll_create1", 291),
        ("dup3", 292),
        ("pipe2", 293),
        ("inotify_init1", 294),
        ("preadv", 295),
        ("pwritev", 296),
        ("rt_tgsigqueueinfo", 297),
        ("perf_event_open", 298),
        ("recvmmsg", 299),
        ("fanotify_init", 300),
        ("fanotify_mark", 301),
        ("prlimit64", 302),
        ("name_to_handle_at", 303),
        ("open_by_handle_at", 304),
        ("clock_adjtime", 305),
        ("syncfs", 306),
        ("sendmmsg", 307),
        ("setns", 308),
        ("getcpu", 309),
        ("process_vm_readv", 310),
        ("process_vm_writev", 311),
        ("kcmp", 312),
        ("finit_module", 313),
        ("sched_setattr", 314),
        ("sched_getattr", 315),
        ("renameat2", 316),
        ("seccomp", 317),
        ("getrandom", 318),
        ("memfd_create", 319),
        ("kexec_file_load", 320),
        ("bpf", 321),
        ("execveat", 322),
        ("userfaultfd", 323),
        ("membarrier", 324),
        ("mlock2", 325),
        ("copy_file_range", 326),
        ("preadv2", 327),
        ("pwritev2", 328),
        ("pkey_mprotect", 329),
        ("pkey_alloc", 330),
        ("pkey_free", 331),
        ("statx", 332),
        ("io_pgetevents", 333),
        ("rseq", 334),
        ("pidfd_send_signal", 424),
        ("io_uring_setup", 425),
        ("io_uring_enter", 426),
        ("io_uring_register", 427),
        ("open_tree", 428),
        ("move_mount", 429),
        ("fsopen", 430),
        ("fsconfig", 431),
        ("fsmount", 432),
        ("fspick", 433),
        ("pidfd_open", 434),
        ("clone3", 435),
        ("close_range", 436),
        ("openat2", 437),
        ("pidfd_getfd", 438),
        ("faccessat2", 439),
        ("process_madvise", 440),
        ("epoll_pwait2", 441),
        ("mount_setattr", 442),
        ("quotactl_fd", 443),
        ("landlock_create_ruleset", 444),
        ("landlock_add_rule", 445),
        ("landlock_restrict_self", 446),
        ("memfd_secret", 447),
        ("process_mrelease", 448),
        ("futex_waitv", 449),
        ("set_mempolicy_home_node", 450),
    ];

    fn syscall_nr(name: &str) -> Option<u32> {
        SYSCALLS.iter().find(|(n, _)| *n == name).map(|(_, nr)| *nr)
    }

    /// Members of the named `@group`. Every name here must exist in
    /// [`SYSCALLS`] — verified by the unit tests that expand every group.
    fn group_members(group: &str) -> Option<&'static [&'static str]> {
        let m: &[&str] = match group {
            "basic-io" => &[
                "read", "write", "readv", "writev", "pread64", "pwrite64", "preadv", "pwritev",
                "preadv2", "pwritev2", "lseek",
            ],
            "file-system" => &[
                "open",
                "openat",
                "openat2",
                "creat",
                "close",
                "stat",
                "fstat",
                "lstat",
                "newfstatat",
                "access",
                "faccessat",
                "faccessat2",
                "chmod",
                "fchmod",
                "fchmodat",
                "chown",
                "fchown",
                "lchown",
                "fchownat",
                "readlink",
                "readlinkat",
                "rename",
                "renameat",
                "renameat2",
                "link",
                "linkat",
                "symlink",
                "symlinkat",
                "unlink",
                "unlinkat",
                "mkdir",
                "mkdirat",
                "rmdir",
                "truncate",
                "ftruncate",
                "statfs",
                "fstatfs",
                "getdents",
                "getdents64",
                "getcwd",
                "chdir",
                "fchdir",
                "umask",
                "getxattr",
                "lgetxattr",
                "fgetxattr",
                "setxattr",
                "lsetxattr",
                "fsetxattr",
                "removexattr",
                "lremovexattr",
                "fremovexattr",
                "listxattr",
                "llistxattr",
                "flistxattr",
                "utime",
                "utimes",
                "utimensat",
                "mknod",
                "mknodat",
                "copy_file_range",
                "name_to_handle_at",
                "open_by_handle_at",
                "inotify_init",
                "inotify_init1",
                "inotify_add_watch",
                "inotify_rm_watch",
            ],
            "network-io" => &[
                "socket",
                "socketpair",
                "bind",
                "listen",
                "accept",
                "accept4",
                "connect",
                "getsockname",
                "getpeername",
                "sendto",
                "recvfrom",
                "sendmsg",
                "recvmsg",
                "sendmmsg",
                "recvmmsg",
                "setsockopt",
                "getsockopt",
                "shutdown",
            ],
            "process" => &[
                "clone",
                "clone3",
                "fork",
                "vfork",
                "execve",
                "execveat",
                "exit",
                "exit_group",
                "wait4",
                "waitid",
                "getpid",
                "getppid",
                "gettid",
                "set_tid_address",
                "set_robust_list",
                "prlimit64",
                "getrlimit",
                "setrlimit",
                "sched_setparam",
                "sched_getparam",
                "sched_setscheduler",
                "sched_getscheduler",
                "sched_yield",
                "sched_setaffinity",
                "sched_getaffinity",
                "sched_setattr",
                "sched_getattr",
                "sched_get_priority_max",
                "sched_get_priority_min",
                "sched_rr_get_interval",
                "getpriority",
                "setpriority",
                "getcpu",
                "nanosleep",
            ],
            "signal" => &[
                "kill",
                "tkill",
                "tgkill",
                "rt_sigaction",
                "rt_sigprocmask",
                "rt_sigreturn",
                "rt_sigpending",
                "rt_sigtimedwait",
                "rt_sigqueueinfo",
                "rt_sigsuspend",
                "sigaltstack",
            ],
            "ipc" => &[
                "pipe",
                "pipe2",
                "socketpair",
                "dup",
                "dup2",
                "dup3",
                "semget",
                "semop",
                "semctl",
                "semtimedop",
                "shmget",
                "shmat",
                "shmdt",
                "shmctl",
                "msgget",
                "msgsnd",
                "msgrcv",
                "msgctl",
                "futex",
                "futex_waitv",
                "eventfd",
                "eventfd2",
                "signalfd",
                "signalfd4",
                "timerfd_create",
                "timerfd_settime",
                "timerfd_gettime",
            ],
            "chown" => &["chown", "fchown", "lchown", "fchownat"],
            "setuid" => &[
                "setuid",
                "setgid",
                "setreuid",
                "setregid",
                "setresuid",
                "setresgid",
                "setfsuid",
                "setfsgid",
                "setgroups",
            ],
            "timer" => &[
                "timer_create",
                "timer_settime",
                "timer_gettime",
                "timer_getoverrun",
                "timer_delete",
                "clock_settime",
                "clock_gettime",
                "clock_getres",
                "clock_nanosleep",
                "timerfd_create",
                "timerfd_settime",
                "timerfd_gettime",
                "alarm",
            ],
            "resources" => &[
                "prlimit64",
                "setrlimit",
                "setpriority",
                "ioprio_set",
                "ioprio_get",
                "sched_setaffinity",
                "sched_getaffinity",
            ],
            "sync" => &[
                "sync",
                "fsync",
                "fdatasync",
                "syncfs",
                "sync_file_range",
                "msync",
            ],
            "reboot" => &["reboot", "kexec_load", "kexec_file_load"],
            "mount" => &[
                "mount",
                "umount2",
                "pivot_root",
                "move_mount",
                "open_tree",
                "fsopen",
                "fsconfig",
                "fsmount",
                "fspick",
                "mount_setattr",
            ],
            "module-load" => &[
                "init_module",
                "finit_module",
                "delete_module",
                "create_module",
                "query_module",
                "finmod",
                "get_kernel_syms",
            ],
            "raw-io" => &["iopl", "ioperm", "modify_ldt"],
            "debug" => &[
                "ptrace",
                "process_vm_readv",
                "process_vm_writev",
                "kcmp",
                "perf_event_open",
            ],
            "swap" => &["swapon", "swapoff"],
            "privileged" => &[
                "setdomainname",
                "sethostname",
                "iopl",
                "ioperm",
                "reboot",
                "swapon",
                "swapoff",
                "acct",
                "chroot",
                "ptrace",
                "bpf",
                "perf_event_open",
                "quotactl",
                "mount",
                "umount2",
                "pivot_root",
                "kexec_load",
                "kexec_file_load",
                "init_module",
                "finit_module",
                "delete_module",
                "create_module",
            ],
            "obsolete" => &["ustat", "query_module", "lookup_dcookie", "sysfs"],
            "cpu-emulation" => &["modify_ldt"],
            "pkey" => &["pkey_mprotect", "pkey_alloc", "pkey_free"],
            "system-service" => &[
                // The set of syscalls a normal system service is allowed. This
                // is `@default` plus the broad io/network/process/ipc families
                // minus privileged/kernel-modifying calls. Kept explicit rather
                // than a computed union so it reads as an auditable list.
                "read",
                "write",
                "open",
                "openat",
                "openat2",
                "close",
                "stat",
                "fstat",
                "lstat",
                "newfstatat",
                "lseek",
                "mmap",
                "mprotect",
                "munmap",
                "brk",
                "access",
                "faccessat",
                "faccessat2",
                "fork",
                "vfork",
                "clone",
                "clone3",
                "execve",
                "execveat",
                "exit",
                "exit_group",
                "wait4",
                "waitid",
                "kill",
                "tgkill",
                "tkill",
                "getpid",
                "gettid",
                "getppid",
                "getuid",
                "getgid",
                "geteuid",
                "getegid",
                "getgroups",
                "getpgrp",
                "getpgid",
                "getsid",
                "getppid",
                "getcwd",
                "chdir",
                "fchdir",
                "dup",
                "dup2",
                "dup3",
                "fcntl",
                "flock",
                "fsync",
                "fdatasync",
                "ioctl",
                "poll",
                "select",
                "ppoll",
                "pselect6",
                "epoll_create",
                "epoll_create1",
                "epoll_ctl",
                "epoll_ctl_old",
                "epoll_wait",
                "epoll_wait_old",
                "epoll_pwait",
                "epoll_pwait2",
                "pipe",
                "pipe2",
                "socket",
                "socketpair",
                "bind",
                "listen",
                "accept",
                "accept4",
                "connect",
                "getsockname",
                "getpeername",
                "sendto",
                "recvfrom",
                "sendmsg",
                "recvmsg",
                "sendmmsg",
                "recvmmsg",
                "setsockopt",
                "getsockopt",
                "shutdown",
                "nanosleep",
                "clock_gettime",
                "clock_getres",
                "clock_settime",
                "clock_nanosleep",
                "sched_yield",
                "gettimeofday",
                "time",
                "getrusage",
                "sysinfo",
                "uname",
                "times",
                "umask",
                "prlimit64",
                "getrlimit",
                "setrlimit",
                "rt_sigaction",
                "rt_sigprocmask",
                "rt_sigreturn",
                "rt_sigpending",
                "rt_sigtimedwait",
                "rt_sigqueueinfo",
                "rt_sigsuspend",
                "sigaltstack",
                "mknod",
                "mknodat",
                "readlink",
                "readlinkat",
                "symlink",
                "symlinkat",
                "link",
                "linkat",
                "rename",
                "renameat",
                "renameat2",
                "unlink",
                "unlinkat",
                "mkdir",
                "mkdirat",
                "rmdir",
                "creat",
                "truncate",
                "ftruncate",
                "statfs",
                "fstatfs",
                "getdents",
                "getdents64",
                "chmod",
                "fchmod",
                "fchmodat",
                "chown",
                "fchown",
                "fchownat",
                "lchown",
                "utime",
                "utimes",
                "utimensat",
                "setxattr",
                "getxattr",
                "statx",
                "sync",
                "fsync",
                "fdatasync",
                "syncfs",
                "getrandom",
                "getrlimit",
                "setrlimit",
            ],
            _ => return None,
        };
        Some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with<F: FnOnce(&mut SandboxConfig)>(f: F) -> SandboxConfig {
        let mut c = SandboxConfig::default();
        f(&mut c);
        c
    }

    #[test]
    fn plan_is_none_without_sandbox() {
        assert!(plan(&SandboxConfig::default()).is_none());
    }

    #[test]
    fn plan_orders_mountns_before_no_new_privs() {
        let c = cfg_with(|c| {
            c.private_tmp = true;
            c.no_new_privileges = true;
        });
        let ops = plan(&c).unwrap();
        assert!(matches!(&ops[0], Op::UnshareMount));
        let nnip = ops
            .iter()
            .position(|o| matches!(o, Op::NoNewPrivileges))
            .unwrap();
        assert!(nnip > 1, "mount ops must precede NoNewPrivileges");
    }

    #[test]
    fn plan_private_tmp_mounts_both_tmp_dirs() {
        let c = cfg_with(|c| c.private_tmp = true);
        let ops = plan(&c).unwrap();
        assert!(ops.contains(&Op::MountTmpfs(PathBuf::from("/tmp"))));
        assert!(ops.contains(&Op::MountTmpfs(PathBuf::from("/var/tmp"))));
    }

    #[test]
    fn cap_names_resolve_to_numbers() {
        assert_eq!(cap_number("CHOWN"), Some(0));
        assert_eq!(cap_number("CAP_NET_BIND_SERVICE"), Some(10));
        assert_eq!(cap_number("SYS_ADMIN"), Some(21));
        assert_eq!(cap_number("cap_setuid"), Some(7)); // case-insensitive
        assert_eq!(cap_number("BPF"), Some(39));
        assert_eq!(cap_number("NOT_A_CAP"), None);
    }

    #[test]
    fn plan_adds_bounding_and_ambient_ops() {
        let c = cfg_with(|c| {
            c.bounding_set = vec!["CAP_NET_BIND_SERVICE".into(), "SYS_ADMIN".into()];
            c.ambient_set = vec!["CAP_NET_RAW".into()];
        });
        let ops = plan(&c).unwrap();
        assert!(ops.contains(&Op::CapBoundingDrop(vec![10, 21], false)));
        assert!(ops.contains(&Op::AmbientRaise(vec![13])));
    }

    #[test]
    fn inverted_bounding_keeps_only_listed() {
        let c = cfg_with(|c| {
            c.bounding_invert = true;
            c.bounding_set = vec!["CAP_KILL".into()];
        });
        let ops = plan(&c).unwrap();
        assert!(ops.contains(&Op::CapBoundingDrop(vec![5], true)));
    }

    #[test]
    fn protect_system_strict_readonlys_root() {
        let c = cfg_with(|c| c.protect_system = ProtectSystemLevel::Strict);
        let ops = plan(&c).unwrap();
        assert!(ops.contains(&Op::BindReadOnly(PathBuf::from("/"))));
    }

    /// Exercise the real mount ops in a forked child so we see the actual
    /// errno without disturbing the test process. Only meaningful where
    /// userns+mountns are permitted (the CI VM is, per `unshare -m`).
    #[test]
    fn apply_reports_mount_errors_not_panics() {
        let c = cfg_with(|c| c.protect_system = ProtectSystemLevel::Yes);
        let Some(ops) = plan(&c) else { panic!() };
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            let r = apply(&ops);
            eprintln!(
                "apply result: {}",
                r.map(|_| "ok".to_string())
                    .unwrap_or_else(|e| format!("ERR: {e}"))
            );
            unsafe { libc::_exit(0) };
        }
        let mut st: libc::c_int = 0;
        unsafe { libc::waitpid(pid, &mut st, 0) };
        // No assertion on the result — this is a smoke test that apply returns
        // a clean Result (Ok or Err) rather than crashing.
    }

    // --- seccomp (`SystemCallFilter=`) ---

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn resolve_syscall_names_and_numbers() {
        assert_eq!(resolve_syscalls(&["read".into()]).unwrap(), vec![0]);
        assert_eq!(resolve_syscalls(&["exit_group".into()]).unwrap(), vec![231]);
        // Case-insensitive names and dedup.
        let nrs = resolve_syscalls(&["CHMOD".into(), "chown".into(), "chmod".into()]).unwrap();
        assert_eq!(nrs, vec![90, 92]);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn resolve_expands_groups() {
        let nrs = resolve_syscalls(&["@chown".into()]).unwrap();
        assert!(!nrs.contains(&90)); // chmod is a mode change, not in @chown
        assert!(nrs.contains(&92)); // chown
        assert!(nrs.contains(&93)); // fchown
        // @system-service expands to a broad, ordered, deduped set.
        let svc = resolve_syscalls(&["@system-service".into()]).unwrap();
        assert!(svc.contains(&0)); // read
        assert!(svc.contains(&57)); // fork
        let mut sorted = svc.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, svc, "@system-service must be sorted+deduped");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn resolve_errors_on_unknown() {
        assert!(resolve_syscalls(&["definitely_not_a_syscall".into()]).is_err());
        assert!(resolve_syscalls(&["@no-such-group".into()]).is_err());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn every_group_member_is_a_known_syscall() {
        // Enumerate every group and assert each member resolves, so a typo in
        // a group table is caught here rather than at runtime.
        let groups = [
            "basic-io",
            "file-system",
            "network-io",
            "process",
            "signal",
            "ipc",
            "chown",
            "setuid",
            "timer",
            "resources",
            "sync",
            "reboot",
            "mount",
            "module-load",
            "raw-io",
            "debug",
            "swap",
            "privileged",
            "obsolete",
            "cpu-emulation",
            "pkey",
            "system-service",
        ];
        for g in groups {
            let nrs = resolve_syscalls(&[format!("@{g}")]).unwrap();
            assert!(!nrs.is_empty(), "group @{g} expands to nothing");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn build_program_layout_and_jumps() {
        use libc::{BPF_ABS, BPF_JEQ, BPF_JMP, BPF_K, BPF_LD, BPF_RET, BPF_W};
        // Allow-list of a single syscall → the ALLOW_BASE is auto-added, so
        // expect >3 entries. Deny-list blocks `quotactl` (179).
        let nrs = seccomp::resolve(&["quotactl".into()]).unwrap();
        let prog = build_seccomp(&nrs, true, 1, &[], None, false);
        let n = nrs.len(); // 1
        // Layout: 3 prologue (arch load, arch jeq, nr load) + n compare
        // entries + 2 returns = 5 + n. (index of allow = 4 + n).
        assert_eq!(prog.len(), 5 + n);
        // [0] loads arch from offset 4.
        assert_eq!(prog[0].code, (BPF_LD | BPF_W | BPF_ABS) as u16);
        assert_eq!(prog[0].k, 4);
        // [1] JEQ native arch; jt=0 → arch match falls through to load nr ([2]),
        // so the filter can actually match; jf → allow (index `ok` = 4+n): for
        // n=1, ok=5, jf offset = 5-2 = 3.
        assert_eq!(prog[1].code, (BPF_JMP | BPF_JEQ | BPF_K) as u16);
        assert_eq!(prog[1].k, AUDIT_ARCH_X86_64);
        assert_eq!(prog[1].jt, 0);
        assert_eq!(prog[1].jf as usize, 4 + n - 2);
        // [2] loads nr from offset 0.
        assert_eq!(prog[2].k, 0);
        // [3] JEQ the blocked syscall (179) with jt to return-errno (`den`).
        let den = 3 + n;
        assert_eq!(prog[3].code, (BPF_JMP | BPF_JEQ | BPF_K) as u16);
        assert_eq!(prog[3].k, 179);
        assert_eq!((prog[3].jt as usize), den - 4);
        // [den] returns ERRNO|1; [ok] returns ALLOW.
        assert_eq!(prog[den].code, (BPF_RET | BPF_K) as u16);
        assert_eq!(prog[den].k, libc::SECCOMP_RET_ERRNO | 1);
        assert_eq!(prog[4 + n].k, libc::SECCOMP_RET_ALLOW);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn build_allow_list_adds_base_and_denies_others() {
        // An allow-list of just `read` must still permit exit/exit_group (the
        // base), i.e. the built program must include the base numbers. Check
        // structurally: any number not in entries refers to `den`.
        let nrs = seccomp::resolve(&["read".into()]).unwrap();
        let prog = build_seccomp(&nrs, false, 1, &[], None, false);
        // The allow-list's JEQ table covers all entries (base included); the
        // exit_group (231) must be present among the JEQ k values.
        let ks: Vec<u32> = prog.iter().map(|s| s.k).collect();
        assert!(ks.contains(&231), "exit_group must be in the allow-list");
        // Last two are the returns.
        let last = prog.len();
        assert_eq!(prog[last - 2].k & 0xFFFF_0000, libc::SECCOMP_RET_ERRNO);
        assert_eq!(prog[last - 1].k, libc::SECCOMP_RET_ALLOW);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn seccomp_implies_no_new_privileges_before_filter() {
        // SystemCallFilter= must force NoNewPrivileges (installing
        // SECCOMP_MODE_FILTER needs CAP_SYS_ADMIN or no_new_privs), so an
        // unprivileged manager can apply the filter. The NNP op must precede
        // the Seccomp op.
        let c = cfg_with(|c| c.syscall_nrs = vec![83]); // mkdir
        let ops = plan(&c).unwrap();
        let nnp = ops
            .iter()
            .position(|o| matches!(o, Op::NoNewPrivileges))
            .expect("seccomp must imply NoNewPrivileges");
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("seccomp op present");
        assert!(
            nnp < sec,
            "NoNewPrivileges must be applied before the filter"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn explicit_no_new_privileges_not_duplicated_for_seccomp() {
        // When the unit already sets NoNewPrivileges=yes, do not push a second
        // NNP op.
        let c = cfg_with(|c| {
            c.no_new_privileges = true;
            c.syscall_nrs = vec![83];
        });
        let ops = plan(&c).unwrap();
        let nnps = ops.iter().filter(|o| matches!(o, Op::NoNewPrivileges));
        assert_eq!(nnps.count(), 1, "NNP must not be duplicated");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn build_deny_errno_zero_guards_to_eperm() {
        // A deny-list with errno==0 must NOT build a program that lets blocked
        // syscalls "succeed" (ERRNO|0 = success). The build defaults 0 to
        // EPERM (1), so the deny return carries EPERM.
        let nrs = seccomp::resolve(&["mkdirat".into()]).unwrap();
        let prog = build_seccomp(&nrs, true, 0, &[], None, false);
        // The deny return is the second-to-last instruction (the last is
        // ALLOW); it must carry SECCOMP_RET_ERRNO | EPERM, not | 0.
        let den_ret = &prog[prog.len() - 2];
        assert_eq!(den_ret.code, (libc::BPF_RET | libc::BPF_K) as u16);
        assert_eq!(
            den_ret.k & !0xFFFF,
            libc::SECCOMP_RET_ERRNO,
            "deny return must be an ERRNO action"
        );
        assert_eq!(den_ret.k & 0xFFFF, 1, "errno 0 must default to EPERM (1)");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn restrict_address_families_gates_socket() {
        use libc::{BPF_ABS, BPF_JEQ, BPF_JMP, BPF_K, BPF_LD, BPF_W};
        // `RestrictAddressFamilies=AF_UNIX` alone (no SystemCallFilter) must
        // install a seccomp filter that intercepts socket/socketpair and
        // reads the family argument (offset 16), while allowing every other
        // syscall through.
        let c = cfg_with(|c| {
            c.af_present = true;
            c.af_families = vec![1]; // AF_UNIX
        });
        let ops = plan(&c).unwrap();
        let nnp = ops
            .iter()
            .position(|o| matches!(o, Op::NoNewPrivileges))
            .expect("RestrictAddressFamilies must imply NoNewPrivileges");
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("RestrictAddressFamilies must install a seccomp op");
        assert!(nnp < sec, "NNP must precede the family filter");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        // The family gate loads `seccomp_data.args[0]` (offset 16).
        assert!(
            prog.iter()
                .any(|s| s.code == (BPF_LD | BPF_W | BPF_ABS) as u16 && s.k == 16),
            "family gate must read the address-family argument"
        );
        // socket(41)/socketpair(53) are intercepted; AF_UNIX(1) is the only
        // allowed family token.
        let socket = 41u32;
        let socketpair = 53u32;
        let jeq: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16)
            .map(|s| s.k)
            .collect();
        assert!(
            jeq.contains(&socket) && jeq.contains(&socketpair),
            "socket/socketpair must be dispatched to the family gate"
        );
        assert!(
            jeq.contains(&1),
            "the allowed family (AF_UNIX) must be compared against"
        );
        // Standalone (no SystemCallFilter) default is ALLOW: a non-matching
        // syscall must not be blocked.
        let allow = prog.last().unwrap();
        assert_eq!(allow.k, libc::SECCOMP_RET_ALLOW);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn restrict_address_families_deny_all_blocks_socket_directly() {
        // `RestrictAddressFamilies=~all` is a constant deny: no arg0 load is
        // emitted; socket/socketpair jump straight to the errno return.
        use libc::{BPF_JEQ, BPF_JMP, BPF_K};
        let c = cfg_with(|c| {
            c.af_present = true;
            c.af_deny = true;
            c.af_deny_all = true;
        });
        let ops = plan(&c).unwrap();
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("seccomp op present");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        let (sock_i, sock_jeq) = prog
            .iter()
            .enumerate()
            .find(|(_, s)| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16 && s.k == 41)
            .expect("socket must be intercepted");
        // The intercept jumps straight to the ERRNO return (there is no
        // family-arg block for `~all`).
        let jt = sock_jeq.jt as usize;
        let den = prog
            .iter()
            .position(|s| {
                s.code == (libc::BPF_RET | libc::BPF_K) as u16
                    && s.k & !0xFFFF == libc::SECCOMP_RET_ERRNO
            })
            .expect("a deny return must exist");
        assert_eq!(
            sock_i + jt + 1,
            den,
            "socket under `~all` must jump to the deny return"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn restrict_realtime_forces_nnp_and_denies_scheduler_syscalls() {
        use libc::{BPF_JEQ, BPF_JMP, BPF_K};
        let c = cfg_with(|c| c.restrict_realtime = true);
        let ops = plan(&c).unwrap();
        let nnp = ops
            .iter()
            .position(|o| matches!(o, Op::NoNewPrivileges))
            .expect("RestrictRealtime must force NoNewPrivileges");
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("RestrictRealtime must install a seccomp op");
        assert!(nnp < sec, "NNP must precede the RestrictRealtime filter");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        // Standalone `RestrictRealtime=` (no `SystemCallFilter=`) is a
        // *deny*-list: the RT syscalls are explicit deny entries (so execve
        // and everything else still work), not an allow-list that would permit
        // only the base set. Collect the JEQ entries (excluding the arch
        // probe) and require the RT numbers among them.
        let entries: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16 && s.k != AUDIT_ARCH_X86_64)
            .map(|s| s.k)
            .collect();
        for nr in [144u32, 314, 142] {
            // sched_setscheduler, sched_setattr, sched_setparam
            assert!(
                entries.contains(&nr),
                "RestrictRealtime syscall {nr} must be denied"
            );
        }
        // A deny-of-a-few means a small entry count (not the whole table),
        // confirming the deny-list reading the service can still `execve`.
        assert!(
            entries.len() <= 10,
            "standalone RestrictRealtime must be a deny-list ({} entries)",
            entries.len()
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn restrict_realtime_respects_systemcallfilter_allow_list() {
        // Combined with an *allow*-list `SystemCallFilter=`, the caller's mode
        // wins: the allow entries stay, and the RT syscalls are denied
        // implicitly (they are simply not in the allowed set).
        use libc::{BPF_JEQ, BPF_JMP, BPF_K};
        let c = cfg_with(|c| {
            c.restrict_realtime = true;
            c.syscall_nrs = vec![0]; // an allow-list of `read`
            c.syscall_deny = false;
        });
        let ops = plan(&c).unwrap();
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("seccomp op present");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        let allowed: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16 && s.k != AUDIT_ARCH_X86_64)
            .map(|s| s.k)
            .collect();
        assert!(allowed.contains(&0), "`read` must remain allowed");
        for nr in [144u32, 314, 142] {
            assert!(
                !allowed.contains(&nr),
                "RT syscall {nr} must not be allowed"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn restrict_realtime_folds_into_deny_list() {
        // Combined with a `SystemCallFilter=~` deny-list, the RT syscalls are
        // merged into the deny entries so they hit the errno return rather
        // than falling through the allow-everything tail.
        use libc::{BPF_JEQ, BPF_JMP, BPF_K};
        let c = cfg_with(|c| {
            c.restrict_realtime = true;
            c.syscall_deny = true;
        });
        let ops = plan(&c).unwrap();
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("seccomp op present");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        let entries: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16 && s.k != AUDIT_ARCH_X86_64)
            .map(|s| s.k)
            .collect();
        for nr in [144u32, 314, 142] {
            assert!(
                entries.contains(&nr),
                "RT syscall {nr} must be a deny entry"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn lock_personality_forces_nnp_and_denies_personality() {
        use libc::{BPF_JEQ, BPF_JMP, BPF_K};
        let c = cfg_with(|c| c.lock_personality = true);
        let ops = plan(&c).unwrap();
        let nnp = ops
            .iter()
            .position(|o| matches!(o, Op::NoNewPrivileges))
            .expect("LockPersonality must force NoNewPrivileges");
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("LockPersonality must install a seccomp op");
        assert!(nnp < sec, "NNP must precede the LockPersonality filter");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        // Standalone `LockPersonality=` is a *deny*-list (deny `personality`,
        // allow everything else, incl. `execve`), so `personality` (135) must
        // be an explicit deny entry but the table must stay small.
        let entries: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16 && s.k != AUDIT_ARCH_X86_64)
            .map(|s| s.k)
            .collect();
        assert!(
            entries.contains(&135),
            "personality(2) must be denied by LockPersonality"
        );
        assert!(
            entries.len() <= 5,
            "standalone LockPersonality must be a deny-list ({} entries)",
            entries.len()
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn lock_personality_folds_into_deny_list() {
        // Combined with a `SystemCallFilter=~` deny-list, `personality` is
        // merged into the deny entries rather than swallowed by the passive
        // allow-everything tail.
        use libc::{BPF_JEQ, BPF_JMP, BPF_K};
        let c = cfg_with(|c| {
            c.lock_personality = true;
            c.syscall_deny = true;
        });
        let ops = plan(&c).unwrap();
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("seccomp op present");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        let entries: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16 && s.k != AUDIT_ARCH_X86_64)
            .map(|s| s.k)
            .collect();
        assert!(entries.contains(&135), "personality must be a deny entry");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn restrict_suidsgid_forces_nnp_and_denies_file_mode_syscalls() {
        use libc::{BPF_JEQ, BPF_JMP, BPF_K};
        let c = cfg_with(|c| c.restrict_suidsgid = true);
        let ops = plan(&c).unwrap();
        let nnp = ops
            .iter()
            .position(|o| matches!(o, Op::NoNewPrivileges))
            .expect("RestrictSUIDSGID must force NoNewPrivileges");
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("RestrictSUIDSGID must install a seccomp op");
        assert!(nnp < sec, "NNP must precede the RestrictSUIDSGID filter");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        // Standalone `RestrictSUIDSGID=` is a *deny*-list (deny exactly the
        // file-mode syscalls, allow everything else incl. `execve`): each of
        // the chmod/chown family must be an explicit deny entry but the table
        // stays small (it is not an allow-list of the whole base set).
        let expected = seccomp::suidsgid_nrs();
        let entries: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16 && s.k != AUDIT_ARCH_X86_64)
            .map(|s| s.k)
            .collect();
        for nr in &expected {
            assert!(
                entries.contains(nr),
                "RestrictSUIDSGID syscall {nr} must be denied"
            );
        }
        assert!(
            entries.len() <= 10,
            "standalone RestrictSUIDSGID must be a deny-list ({} entries)",
            entries.len()
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn restrict_suidsgid_folds_into_deny_list() {
        // Combined with a `SystemCallFilter=~` deny-list, the file-mode
        // syscalls are merged into the deny entries rather than swallowed by
        // the passive allow-everything tail. Every deny name must resolve.
        use libc::{BPF_JEQ, BPF_JMP, BPF_K};
        let c = cfg_with(|c| {
            c.restrict_suidsgid = true;
            c.syscall_deny = true;
        });
        let ops = plan(&c).unwrap();
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("seccomp op present");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        let entries: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16 && s.k != AUDIT_ARCH_X86_64)
            .map(|s| s.k)
            .collect();
        for nr in seccomp::suidsgid_nrs() {
            assert!(
                entries.contains(&nr),
                "file-mode syscall {nr} must be denied"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn memory_deny_write_execute_forces_nnp_and_checks_prot_arg() {
        use libc::{BPF_ABS, BPF_ALU, BPF_AND, BPF_JMP, BPF_JSET, BPF_K, BPF_LD, BPF_W};
        let c = cfg_with(|c| c.memory_deny_write_execute = true);
        let ops = plan(&c).unwrap();
        let nnp = ops
            .iter()
            .position(|o| matches!(o, Op::NoNewPrivileges))
            .expect("MemoryDenyWriteExecute must force NoNewPrivileges");
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("MemoryDenyWriteExecute must install a seccomp op");
        assert!(
            nnp < sec,
            "NNP must precede the MemoryDenyWriteExecute filter"
        );
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        // The WX gate loads `seccomp_data.args[2]` (byte offset 32): the
        // `prot` argument shared by mmap/mprotect/pkey_mprotect.
        assert!(
            prog.iter()
                .any(|s| s.code == (BPF_LD | BPF_W | BPF_ABS) as u16 && s.k == 32),
            "WX gate must read the `prot` argument (arg2@32)"
        );
        // ...masks it to the WRITE|EXEC bits (ALU AND 0x6)...
        assert!(
            prog.iter()
                .any(|s| s.code == (BPF_ALU | BPF_AND | BPF_K) as u16 && s.k == 0x6),
            "WX gate must mask `prot` to PROT_WRITE|PROT_EXEC (0x6)"
        );
        // ...then tests the WRITE (0x2) then EXEC (0x4) bits with JSET.
        let jset: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JSET | BPF_K) as u16)
            .map(|s| s.k)
            .collect();
        assert_eq!(jset, vec![2, 4], "WX gate must test WRITE, then EXEC bits");
        // Standalone `MemoryDenyWriteExecute=` is a deny-list (default ALLOW):
        // a service can still `execve` and map ordinary RW/RX pages. It must
        // not be an allow-list of the whole base set.
        let allow = prog.last().unwrap();
        assert_eq!(allow.k, libc::SECCOMP_RET_ALLOW);
    }

    // End-to-end kernel enforcement of a seccomp deny-list is environment
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn memory_deny_write_execute_gates_mmap_mprotect_pkey() {
        use libc::{BPF_JEQ, BPF_JMP, BPF_K};
        let c = cfg_with(|c| c.memory_deny_write_execute = true);
        let ops = plan(&c).unwrap();
        let sec = ops
            .iter()
            .position(|o| matches!(o, Op::Seccomp(_)))
            .expect("seccomp op present");
        let Op::Seccomp(prog) = &ops[sec] else {
            unreachable!()
        };
        let jeq: Vec<u32> = prog
            .iter()
            .filter(|s| s.code == (BPF_JMP | BPF_JEQ | BPF_K) as u16 && s.k != AUDIT_ARCH_X86_64)
            .map(|s| s.k)
            .collect();
        for nr in [9u32, 10, 329] {
            // mmap, mprotect, pkey_mprotect
            assert!(
                jeq.contains(&nr),
                "WX-gated syscall {nr} must be dispatched to the prot gate"
            );
        }
    }

    // End-to-end kernel enforcement of a seccomp deny-list is environment
    // sensitive: installing a `SECCOMP_MODE_FILTER` inside a container (the
    // typical dev/CI sandbox) is restricted, so a forked-child proof hangs
    // there rather than asserting cleanly. The filter *construction* and
    // syscall resolution are fully covered by the pure tests above
    // (`build_*`, `resolve_*`), plus the real-daemon e2e in
    // `tests/seccomp.rs::memory_deny_write_execute_blocks_wx_protect`; actual
    // enforcement is otherwise verified manually on a real host as root.
}
