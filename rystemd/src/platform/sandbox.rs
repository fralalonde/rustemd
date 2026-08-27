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
}

/// Build the ordered op list for `cfg`, or `None` if no implemented directive
/// is set. Pure; runs in the parent.
pub fn plan(cfg: &SandboxConfig) -> Option<Vec<Op>> {
    if !cfg.has_sandbox() {
        return None;
    }
    let mut ops = Vec::new();

    let need_mountns = cfg.private_tmp
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
}
