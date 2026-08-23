//! Kernel device discovery and uevent monitoring (Linux).
//!
//! rustemd tracks devices as runtime-generated `.device` units, exactly as
//! systemd does: a device is **not** backed by a unit file, it is synthesized
//! from the kernel device tree and from hotplug uevents. This module is the
//! source of that data. It has two halves:
//!
//! 1. **Enumeration** ([`enumerate_devices`]) walks `/sys/devices` and reads
//!    each entry's `uevent` file (plus its `subsystem` symlink) to recover the
//!    same `SUBSYSTEM`/`DEVNAME`/`DEVTYPE` tuple udev publishes. No udev
//!    daemon or library is involved — this is plain sysfs, which is always
//!    present on Linux.
//! 2. **Monitoring** ([`UdevMonitor`]) opens a `NETLINK_KOBJECT_UEVENT`
//!    netlink socket (the same channel `systemd-udevd` listens on) and drains
//!    `add`/`remove`/`change`/`move` events so the manager can create and
//!    remove device units on hotplug.
//!
//! ## Why not the `libudev`/`udev` crates?
//!
//! Both the `libudev` crate and the Smithay `udev` crate (despite its "pure
//! Rust" reputation) bottom out in `libudev-sys`, which requires the C
//! `libudev` *development* headers at build time via `pkg-config`
//! (`libudev.h`, `libudev.pc`). Those headers are frequently absent on
//! headless containers and cross-compile images even when the runtime
//! `libudev.so.1` is present. The `NETLINK_KOBJECT_UEVENT` protocol is a
//! plain-text socket (no binary netlink header), and sysfs enumeration is a
//! directory walk, so both are trivially expressible with the `nix` and
//! `libc` crates already in this project's dependency tree. That keeps the
//! build dependency-free and the feature opt-out trivial.

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::Path;

use nix::sys::socket::{
    AddressFamily, MsgFlags, NetlinkAddr, SockFlag, SockProtocol, SockType, bind, recv, socket,
};

/// Root of the kernel device tree in sysfs.
const SYSFS_DEVICES: &str = "/sys/devices";

/// A kernel device: the union of its sysfs path and the identity keys udev
/// publishes for it (`SUBSYSTEM`, `DEVNAME`, `DEVTYPE`). One sysfs entry maps
/// to one `.device` unit (plus a subsystem-name alias).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Sysfs path relative to `/sys` (no leading slash), e.g.
    /// `devices/virtual/net/lo`.
    pub devpath: String,
    /// Subsystem name (`net`, `block`, `tty`, `pci`, ...).
    pub subsystem: String,
    /// Device node name from `DEVNAME` (`lo`, `sda`, `ttyS0`); empty when the
    /// device has no `/dev` node (network interfaces, CPUs, bridges, ...).
    pub devname: String,
    /// Device type (`disk`, `partition`, ...); often empty.
    pub devtype: String,
}

impl Device {
    /// Last path component (`lo`, `sda`, `cpu0`).
    pub fn sysname(&self) -> &str {
        self.devpath.rsplit('/').next().unwrap_or(&self.devpath)
    }

    /// systemd's sysfs-path unit name: `/sys/devices/virtual/net/lo` →
    /// `sys-devices-virtual-net-lo.device`.
    pub fn sysfs_unit_name(&self) -> String {
        format!("sys-{}.device", self.devpath.replace('/', "-"))
    }

    /// systemd's subsystem unit name: `sys-<subsystem>-<devname>.device`.
    /// Falls back to `<sysname>` when the device has no `/dev` node, matching
    /// systemd's treatment of deviceless devices.
    pub fn subsystem_unit_name(&self) -> String {
        let name = if self.devname.is_empty() {
            self.sysname()
        } else {
            &self.devname
        };
        format!("sys-{}-{}.device", self.subsystem, name)
    }

    /// Both unit names this device registers: the primary sysfs-path name and
    /// the subsystem alias. Ordering is stable (primary first).
    pub fn unit_names(&self) -> [String; 2] {
        [self.sysfs_unit_name(), self.subsystem_unit_name()]
    }
}

/// A uevent's action. Only add/remove/change/move matter for device tracking;
/// everything else (`online`, `offline`, `bind`, `unbind`, ...) collapses into
/// [`UEventAction::Other`] and is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UEventAction {
    Add,
    Remove,
    Change,
    Move,
    Other,
}

/// A live `NETLINK_KOBJECT_UEVENT` socket subscribed to kernel + udev uevents.
/// Non-blocking: the fd is polled by the manager's event loop and drained with
/// [`UdevMonitor::read_events`].
pub struct UdevMonitor {
    fd: OwnedFd,
}

impl UdevMonitor {
    /// Open and bind the uevent socket. Fails only if the kernel refuses the
    /// socket/bind (netlink unavailable); callers treat that as non-fatal and
    /// keep running with enumeration only.
    pub fn new() -> Result<UdevMonitor, String> {
        let fd = socket(
            AddressFamily::Netlink,
            SockType::Datagram,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK,
            SockProtocol::NetlinkKObjectUEvent,
        )
        .map_err(|e| format!("netlink socket: {e}"))?;
        // Multicast groups: 1 = kernel-originated uevents (hotplug add/remove),
        // 2 = udev-forwarded synthetic events (change/move). Subscribe to both
        // so nothing is missed; pid 0 lets the kernel assign one.
        let addr = NetlinkAddr::new(0, 1 | 2);
        bind(fd.as_raw_fd(), &addr).map_err(|e| format!("netlink bind: {e}"))?;
        Ok(UdevMonitor { fd })
    }

    /// The poll fd for the manager's event loop.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Drain all currently-pending uevents into `(action, device)` pairs.
    /// Returns an empty vector on `EAGAIN` (nothing pending); an individual
    /// malformed message is skipped rather than poisoning the whole drain.
    pub fn read_events(&mut self) -> Vec<(UEventAction, Device)> {
        let mut out = Vec::new();
        // Kernel uevents are capped at ~2 KiB; 8 KiB is ample headroom.
        let mut buf = [0u8; 8192];
        loop {
            match recv(self.fd.as_raw_fd(), &mut buf, MsgFlags::empty()) {
                Ok(0) => break,
                Ok(n) => {
                    if let Some(ev) = parse_uevent(&buf[..n]) {
                        out.push(ev);
                    }
                }
                Err(nix::errno::Errno::EAGAIN) => break,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(_) => break,
            }
        }
        out
    }
}

/// Enumerate every device currently present under `/sys/devices` by walking
/// the sysfs tree. The same data udev derives its database from.
pub fn enumerate_devices() -> Vec<Device> {
    let mut out = Vec::new();
    walk_sysfs(Path::new(SYSFS_DEVICES), &mut out);
    out
}

fn walk_sysfs(dir: &Path, out: &mut Vec<Device>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        // Use `file_type()` (not `path().is_dir()`): the latter follows
        // symlinks, and sysfs is full of `subsystem`/`device` symlink cycles
        // (`…/lo/subsystem -> /sys/class/net`, `/sys/class/net/lo -> …/lo`)
        // that would otherwise recurse forever. Only real directories descend.
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let p = e.path();
        if let Some(dev) = device_from_sysfs_dir(&p) {
            out.push(dev);
        }
        walk_sysfs(&p, out);
    }
}

/// Recover a [`Device`] from one sysfs device directory. Returns `None` when
/// the entry is a plain kobject with no subsystem (not a device), or when its
/// `uevent` file is unreadable.
fn device_from_sysfs_dir(dir: &Path) -> Option<Device> {
    let uevent = std::fs::read_to_string(dir.join("uevent")).ok()?;
    let kv = parse_key_values(&uevent);
    let subsystem = subsystem_of(dir).or_else(|| kv.get("SUBSYSTEM").cloned())?;
    let devpath = dir
        .strip_prefix("/sys")
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| dir.to_string_lossy().to_string());
    Some(Device {
        devpath,
        subsystem,
        devname: kv.get("DEVNAME").cloned().unwrap_or_default(),
        devtype: kv.get("DEVTYPE").cloned().unwrap_or_default(),
    })
}

/// The device's subsystem, from its `subsystem` symlink (which points at the
/// owning class/bus directory, e.g. `.../class/net`). Last component is the
/// subsystem name.
fn subsystem_of(dir: &Path) -> Option<String> {
    let target = std::fs::read_link(dir.join("subsystem")).ok()?;
    target
        .file_name()
        .and_then(|f| f.to_str())
        .map(str::to_string)
}

/// Parse a netlink uevent payload: `add@/devices/...\0KEY=value\0...`.
/// `NETLINK_KOBJECT_UEVENT` messages are raw text (no `nlmsghdr`), so the
/// buffer is split on NUL and the header carries `action@devpath`.
fn parse_uevent(payload: &[u8]) -> Option<(UEventAction, Device)> {
    let text = String::from_utf8_lossy(payload);
    let mut parts = text.split('\0');
    let header = parts.next()?;
    let (action, devpath) = header.split_once('@')?;
    let mut kv = HashMap::new();
    for p in parts {
        if let Some((k, v)) = p.split_once('=') {
            kv.insert(k.to_string(), v.to_string());
        }
    }
    let subsystem = kv.get("SUBSYSTEM").cloned().unwrap_or_default();
    if subsystem.is_empty() {
        return None;
    }
    let device = Device {
        devpath: devpath.trim_start_matches('/').to_string(),
        subsystem,
        devname: kv.get("DEVNAME").cloned().unwrap_or_default(),
        devtype: kv.get("DEVTYPE").cloned().unwrap_or_default(),
    };
    let action = match action {
        "add" => UEventAction::Add,
        "remove" => UEventAction::Remove,
        "change" => UEventAction::Change,
        "move" => UEventAction::Move,
        _ => UEventAction::Other,
    };
    Some((action, device))
}

/// Parse `KEY=value` lines (sysfs `uevent` files are newline-separated).
fn parse_key_values(text: &str) -> HashMap<String, String> {
    let mut kv = HashMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            kv.insert(k.to_string(), v.to_string());
        }
    }
    kv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(devpath: &str, subsystem: &str, devname: &str) -> Device {
        Device {
            devpath: devpath.to_string(),
            subsystem: subsystem.to_string(),
            devname: devname.to_string(),
            devtype: String::new(),
        }
    }

    #[test]
    fn sysfs_unit_name_maps_path_to_dashes() {
        let d = dev("devices/virtual/net/lo", "net", "lo");
        assert_eq!(d.sysfs_unit_name(), "sys-devices-virtual-net-lo.device");
    }

    #[test]
    fn subsystem_unit_name_uses_devname_when_present() {
        let d = dev("devices/pci0000:00/block/sda", "block", "sda");
        assert_eq!(d.subsystem_unit_name(), "sys-block-sda.device");
    }

    #[test]
    fn subsystem_unit_name_falls_back_to_sysname() {
        // Network interfaces have no /dev node: DEVNAME is empty.
        let d = dev("devices/virtual/net/lo", "net", "");
        assert_eq!(d.subsystem_unit_name(), "sys-net-lo.device");
    }

    #[test]
    fn parse_uevent_decodes_add_and_remove() {
        let add = b"add@/devices/virtual/net/lo\0ACTION=add\0DEVPATH=/devices/virtual/net/lo\0SUBSYSTEM=net\0INTERFACE=lo\0";
        let (action, device) = parse_uevent(add).unwrap();
        assert_eq!(action, UEventAction::Add);
        assert_eq!(device.devpath, "devices/virtual/net/lo");
        assert_eq!(device.subsystem, "net");

        let rm = b"remove@/devices/virtual/net/lo\0ACTION=remove\0SUBSYSTEM=net\0INTERFACE=lo\0";
        let (action, _) = parse_uevent(rm).unwrap();
        assert_eq!(action, UEventAction::Remove);
    }

    #[test]
    fn parse_uevent_rejects_subsystemless_message() {
        let m = b"add@/devices/system/cpu/cpu0\0ACTION=add\0";
        assert!(parse_uevent(m).is_none());
    }

    #[test]
    fn enumeration_finds_devices_when_sysfs_is_present() {
        // Skip quietly in sandboxes where /sys is not mounted.
        if !Path::new(SYSFS_DEVICES).is_dir() {
            eprintln!("skipping: {SYSFS_DEVICES} not present");
            return;
        }
        let devices = enumerate_devices();
        assert!(!devices.is_empty(), "sysfs device tree should not be empty");
        // Every enumerated device must carry a non-empty subsystem.
        assert!(devices.iter().all(|d| !d.subsystem.is_empty()));
    }
}
