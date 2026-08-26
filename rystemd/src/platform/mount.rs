//! Filesystem mount/unmount — thin, safe wrappers over `mount(2)`/`umount2(2)`
//! via `nix::mount` (Linux only).

use std::path::Path;

pub use nix::mount::{MntFlags, MsFlags};

/// Mount `source` at `target` with the given filesystem type, flags, and
/// filesystem-specific `data` (options) string.
pub fn mount(
    source: Option<&str>,
    target: &Path,
    fstype: &str,
    flags: MsFlags,
    data: Option<&str>,
) -> Result<(), String> {
    nix::mount::mount(source, target, Some(fstype), flags, data).map_err(|e| e.to_string())
}

/// Unmount the filesystem mounted at `target`. When `lazy`, detach immediately
/// (`MNT_DETACH`) even if the mount is still busy.
pub fn unmount(target: &Path, lazy: bool) -> Result<(), String> {
    let flags = if lazy {
        MntFlags::MNT_DETACH
    } else {
        MntFlags::empty()
    };
    nix::mount::umount2(target, flags).map_err(|e| e.to_string())
}

/// Split a systemd `Options=` string into `mount(2)` flags (the generic subset
/// the kernel understands) and the filesystem-specific options string passed as
/// `data`. Flag tokens are removed from `data`; everything else is passed
/// through comma-joined and unchanged.
pub fn split_options(options: Option<&str>) -> (MsFlags, Option<String>) {
    let mut flags = MsFlags::empty();
    let mut data: Vec<&str> = Vec::new();
    if let Some(o) = options {
        for tok in o.split(',') {
            let t = tok.trim();
            if t.is_empty() {
                continue;
            }
            match t {
                "ro" => flags |= MsFlags::MS_RDONLY,
                "nosuid" => flags |= MsFlags::MS_NOSUID,
                "nodev" => flags |= MsFlags::MS_NODEV,
                "noexec" => flags |= MsFlags::MS_NOEXEC,
                "sync" => flags |= MsFlags::MS_SYNCHRONOUS,
                "noatime" => flags |= MsFlags::MS_NOATIME,
                "nodiratime" => flags |= MsFlags::MS_NODIRATIME,
                // Flag-negating / non-flag tokens that the kernel knows; they
                // contribute no flag bit and are not filesystem data.
                "rw" | "suid" | "dev" | "exec" | "async" | "atime" | "diratime" | "relatime"
                | "strictatime" => {}
                _ => data.push(t),
            }
        }
    }
    let data = if data.is_empty() {
        None
    } else {
        Some(data.join(","))
    };
    (flags, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_flags_from_data() {
        let (flags, data) = split_options(Some("ro,nosuid,size=64m,mode=1777"));
        assert!(flags.contains(MsFlags::MS_RDONLY));
        assert!(flags.contains(MsFlags::MS_NOSUID));
        assert!(!flags.contains(MsFlags::MS_NOEXEC));
        assert_eq!(data.as_deref(), Some("size=64m,mode=1777"));
    }

    #[test]
    fn empty_and_plain_options() {
        let (flags, data) = split_options(None);
        assert!(flags.is_empty());
        assert!(data.is_none());

        // A non-flag token passes through untouched.
        let (flags, data) = split_options(Some("size=1m"));
        assert!(flags.is_empty());
        assert_eq!(data.as_deref(), Some("size=1m"));
    }
}
