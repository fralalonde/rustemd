//! Linux PID-1 boot: mount the virtual/API filesystems and run early-boot
//! configuration.
//!
//! Only compiled with the `boot` feature, and only meaningful when rystemd is
//! **PID 1** on a real Linux host, VM, or container — a container runtime
//! normally does all of this for us, and the `--user` manager never calls it.
//!
//! Everything here is deliberately best-effort and idempotent: on a system
//! where the mounts already exist (container, or a re-exec), each step is a
//! no-op. This is the *host-init* surface (Tier 1 + 2); the supervisor /
//! container path never touches it.

use std::ffi::CString;
use std::fs;
use std::path::Path;

type MountFlags = libc::c_ulong;

const MS_NOSUID: MountFlags = libc::MS_NOSUID as MountFlags;
const MS_NODEV: MountFlags = libc::MS_NODEV as MountFlags;
const MS_NOEXEC: MountFlags = libc::MS_NOEXEC as MountFlags;
/// The "secure" flag set systemd applies to most API filesystems.
const SECURE: MountFlags = MS_NOSUID | MS_NODEV | MS_NOEXEC;

/// Mount `src` (or `none`) at `target` as `fstype`. "Already mounted"
/// (`EBUSY`) is treated as success so the sequence is idempotent.
fn mount(
    src: Option<&str>,
    target: &str,
    fstype: &str,
    flags: MountFlags,
    data: Option<&str>,
) -> Result<(), String> {
    let target_c = CString::new(target).map_err(|e| e.to_string())?;
    let fstype_c = CString::new(fstype).map_err(|e| e.to_string())?;
    let src_c = src
        .map(|s| CString::new(s).map_err(|e| e.to_string()))
        .transpose()?;
    let data_c = data
        .map(|s| CString::new(s).map_err(|e| e.to_string()))
        .transpose()?;

    let rc = unsafe {
        libc::mount(
            src_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            target_c.as_ptr(),
            fstype_c.as_ptr(),
            flags,
            data_c
                .as_ref()
                .map_or(std::ptr::null(), |c| c.as_ptr() as *const libc::c_void),
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EBUSY) {
        return Ok(()); // already mounted — idempotent
    }
    Err(format!("mount {fstype} at {target}: {err}"))
}

fn mkdir_p(path: &str) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("mkdir {path}: {e}"))
}

/// Tier 1 — mount the API/virtual filesystems a Linux system expects.
///
/// Order matters: `/proc` and `/sys` first; `/dev` before its children;
/// `/sys` before `/sys/fs/cgroup` (which rystemd's cgroup code depends on).
///
/// Best-effort: every mount is attempted, and the *first* failure is returned
/// after the rest have been tried. This matters in unprivileged namespaces,
/// where `devtmpfs`/`cgroup2` may be refused while `proc`/`sys`/`run` still
/// succeed — a partial boot is better than an aborted one.
pub fn mount_api_filesystems() -> Result<(), String> {
    let mut first_err: Option<String> = None;
    let mut attempt = |r: Result<(), String>| {
        if let Err(e) = r {
            first_err.get_or_insert(e);
        }
    };

    attempt(mount(Some("proc"), "/proc", "proc", SECURE, None));
    attempt(mount(Some("sysfs"), "/sys", "sysfs", SECURE, None));
    attempt(mount(
        Some("devtmpfs"),
        "/dev",
        "devtmpfs",
        MS_NOSUID,
        Some("mode=0755"),
    ));
    attempt(mkdir_p("/dev/pts"));
    attempt(mount(
        Some("devpts"),
        "/dev/pts",
        "devpts",
        MS_NOSUID | MS_NOEXEC,
        Some("mode=0620,gid=5"),
    ));
    attempt(mkdir_p("/dev/shm"));
    attempt(mount(
        Some("tmpfs"),
        "/dev/shm",
        "tmpfs",
        MS_NOSUID | MS_NODEV,
        Some("mode=1777"),
    ));
    attempt(mkdir_p("/run"));
    attempt(mount(
        Some("tmpfs"),
        "/run",
        "tmpfs",
        MS_NOSUID | MS_NODEV,
        Some("mode=0755"),
    ));
    attempt(mkdir_p("/tmp"));
    attempt(mount(Some("tmpfs"), "/tmp", "tmpfs", 0, Some("mode=1777")));
    attempt(mkdir_p("/sys/fs/cgroup"));
    attempt(mount(
        Some("cgroup2"),
        "/sys/fs/cgroup",
        "cgroup2",
        SECURE,
        None,
    ));

    first_err.map_or(Ok(()), Err)
}

/// Tier 2 — early-boot configuration. Each step is best-effort and continues
/// past failure, so a minimal system missing `/etc/hostname`, `/etc/fstab`,
/// etc. still boots.
pub fn early_boot() {
    if let Err(e) = set_hostname() {
        eprintln!("rystemd boot[hostname]: {e}");
    }
    if let Err(e) = ensure_machine_id() {
        eprintln!("rystemd boot[machine-id]: {e}");
    }
    if let Err(e) = apply_sysctl() {
        eprintln!("rystemd boot[sysctl]: {e}");
    }
    if let Err(e) = load_modules() {
        eprintln!("rystemd boot[modules]: {e}");
    }
    if let Err(e) = seed_random() {
        eprintln!("rystemd boot[random-seed]: {e}");
    }
    if let Err(e) = ensure_runtime_dirs() {
        eprintln!("rystemd boot[tmpfiles]: {e}");
    }
    if let Err(e) = mount_fstab() {
        eprintln!("rystemd boot[fstab]: {e}");
    }
}

fn set_hostname() -> Result<(), String> {
    let name = match fs::read_to_string("/etc/hostname") {
        Ok(s) => s.trim().to_string(),
        Err(_) => return Ok(()), // no hostname file; keep the kernel default
    };
    if name.is_empty() {
        return Ok(());
    }
    nix::unistd::sethostname(&name).map_err(|e| e.to_string())
}

/// Generate and persist a machine-id if none exists (a real boot needs a
/// stable id; rystemd's manager reads it for `%m` and identity).
fn ensure_machine_id() -> Result<(), String> {
    if Path::new("/etc/machine-id").exists()
        && fs::read_to_string("/etc/machine-id")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    {
        return Ok(());
    }
    let mut id = [0u8; 16];
    {
        use std::io::Read;
        let mut f = match fs::File::open("/dev/urandom") {
            Ok(f) => f,
            Err(_) => return Ok(()), // no entropy source; leave it
        };
        f.read_exact(&mut id).map_err(|e| e.to_string())?;
    }
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    fs::write("/etc/machine-id", hex.as_bytes()).map_err(|e| e.to_string())
}

/// Apply `/etc/sysctl.conf` and `/etc/sysctl.d/*.conf` (`key = value` lines)
/// by writing to `/proc/sys/...`. `-key` lines (never set) are skipped.
fn apply_sysctl() -> Result<(), String> {
    let mut files = vec!["/etc/sysctl.conf".to_string()];
    if let Ok(rd) = fs::read_dir("/etc/sysctl.d") {
        let mut v: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "conf").unwrap_or(false))
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();
        v.sort();
        files.extend(v);
    }
    for f in files {
        let text = match fs::read_to_string(&f) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let rest = line.strip_prefix('-').unwrap_or(line); // `-key` = never set
            let Some((key, val)) = rest.split_once('=') else {
                continue;
            };
            let path = format!("/proc/sys/{}", key.trim().replace('.', "/"));
            let _ = fs::write(&path, val.trim());
        }
    }
    Ok(())
}

/// Load kernel modules listed in `/etc/modules-load.d/*.conf` via `modprobe`.
fn load_modules() -> Result<(), String> {
    let mut mods = Vec::new();
    if let Ok(rd) = fs::read_dir("/etc/modules-load.d") {
        for e in rd.flatten() {
            if let Ok(t) = fs::read_to_string(e.path()) {
                for line in t.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                        continue;
                    }
                    mods.extend(line.split_whitespace().map(str::to_string));
                }
            }
        }
    }
    for m in mods {
        // Best-effort: `modprobe` may be absent on minimal images.
        let _ = std::process::Command::new("modprobe").arg(&m).status();
    }
    Ok(())
}

/// Feed the saved random seed into the kernel pool, if present.
fn seed_random() -> Result<(), String> {
    let seed = match fs::read("/var/lib/rystemd/random-seed") {
        Ok(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };
    let _ = fs::write("/dev/urandom", &seed);
    Ok(())
}

/// Minimal tmpfiles: the directories rystemd itself needs before it binds its
/// control/notify sockets.
fn ensure_runtime_dirs() -> Result<(), String> {
    for d in ["/run/rystemd", "/var/lib/rystemd", "/var/log"] {
        fs::create_dir_all(d).map_err(|e| format!("mkdir {d}: {e}"))?;
    }
    Ok(())
}

/// Mount `/etc/fstab` entries we don't already handle (skips the API
/// filesystems, `/`, and swap). Options are passed through verbatim.
fn mount_fstab() -> Result<(), String> {
    let text = match fs::read_to_string("/etc/fstab") {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    const SKIP: [&str; 10] = [
        "proc", "sysfs", "devtmpfs", "devpts", "tmpfs", "cgroup", "cgroup2", "swap", "none", "",
    ];
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let (dev, mnt, fstype) = (fields[0], fields[1], fields[2]);
        if SKIP.contains(&fstype) || mnt == "/" {
            continue;
        }
        let options = fields.get(3).copied().unwrap_or("defaults");
        let _ = mount(Some(dev), mnt, fstype, 0, Some(options));
    }
    Ok(())
}

/// Power the machine off via `reboot(2)`. Only meaningful as PID 1 (real
/// root); elsewhere the syscall fails and we fall back to exiting. Never
/// returns on success.
pub fn poweroff() -> ! {
    reboot_cmd(libc::LINUX_REBOOT_CMD_POWER_OFF);
}

/// Reboot via `reboot(2)`. Same PID-1-only caveat as [`poweroff`].
pub fn reboot() -> ! {
    reboot_cmd(libc::LINUX_REBOOT_CMD_RESTART);
}

fn reboot_cmd(cmd: libc::c_int) -> ! {
    // glibc's reboot(2) wrapper takes only the command; it supplies the
    // LINUX_REBOOT_MAGIC values internally.
    unsafe {
        libc::reboot(cmd);
    }
    // reboot(2) returned (not PID 1, or no CAP_SYS_BOOT in the init userns).
    // As PID 1, exiting makes the kernel panic "init died"; everywhere else
    // it's just a clean exit.
    std::process::exit(0);
}
