//! Unit-file names and types as understood by the repository layer.

use std::path::PathBuf;

/// The systemd unit "type" — the suffix after the final `.` in a unit file
/// name (`foo.service` -> [`UnitType::Service`]).
///
/// Unlike the daemon's feature-gated `UnitKind`, this enum is exhaustive and
/// unconditional: the repository is a *generic* storage layer and does not
/// know which unit types a particular build of the daemon happens to parse.
/// The daemon filters [`Repo::list`](crate::Repo::list) results through its
/// own suffix rules, so the two cannot drift apart silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitType {
    Service,
    Timer,
    Target,
    Socket,
    Mount,
    Device,
}

impl UnitType {
    /// Every unit type the repository recognizes.
    pub const ALL: [UnitType; 6] = [
        UnitType::Service,
        UnitType::Timer,
        UnitType::Target,
        UnitType::Socket,
        UnitType::Mount,
        UnitType::Device,
    ];

    /// The file-name suffix for this type (without the leading `.`).
    pub fn suffix(&self) -> &'static str {
        match self {
            UnitType::Service => "service",
            UnitType::Timer => "timer",
            UnitType::Target => "target",
            UnitType::Socket => "socket",
            UnitType::Mount => "mount",
            UnitType::Device => "device",
        }
    }

    /// The suffix, as a string (alias for [`UnitType::suffix`]).
    pub fn as_str(&self) -> &'static str {
        self.suffix()
    }

    /// Resolve a bare suffix (no leading `.`): `"service"` -> [`UnitType::Service`].
    pub fn from_suffix(suffix: &str) -> Option<UnitType> {
        match suffix {
            "service" => Some(UnitType::Service),
            "timer" => Some(UnitType::Timer),
            "target" => Some(UnitType::Target),
            "socket" => Some(UnitType::Socket),
            "mount" => Some(UnitType::Mount),
            "device" => Some(UnitType::Device),
            _ => None,
        }
    }

    /// Resolve the type encoded in a full unit file name (`"foo.service"` ->
    /// [`UnitType::Service`]). Returns `None` when the name has no recognized
    /// unit suffix.
    pub fn from_unit_name(name: &str) -> Option<UnitType> {
        let dot = name.rfind('.')?;
        Self::from_suffix(&name[dot + 1..])
    }
}

/// One unit file discovered in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFile {
    /// The bare file name, e.g. `"foo.service"`.
    pub name: String,
    /// The unit type derived from [`name`](UnitFile::name)'s suffix.
    pub kind: UnitType,
    /// Absolute path to the file on disk.
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_round_trips() {
        for t in UnitType::ALL {
            assert_eq!(UnitType::from_suffix(t.suffix()), Some(t));
        }
        assert_eq!(UnitType::from_suffix("conf"), None);
        assert_eq!(UnitType::from_suffix(""), None);
    }

    #[test]
    fn name_resolution() {
        assert_eq!(
            UnitType::from_unit_name("foo.service"),
            Some(UnitType::Service)
        );
        assert_eq!(UnitType::from_unit_name("x.timer"), Some(UnitType::Timer));
        assert_eq!(
            UnitType::from_unit_name("a.b.target"),
            Some(UnitType::Target)
        );
        assert_eq!(
            UnitType::from_unit_name("getty@.service"),
            Some(UnitType::Service)
        );
        assert_eq!(UnitType::from_unit_name("nosuffix"), None);
        assert_eq!(UnitType::from_unit_name("foo."), None);
        assert_eq!(UnitType::from_unit_name(""), None);
    }
}
