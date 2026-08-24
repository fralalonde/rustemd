//! Unit-name helpers shared by the daemon, the IPC layer, and the CLI.

/// Append `.service` when a unit name has no type suffix.
pub fn normalize_unit(name: &str) -> String {
    if name.contains('.') {
        name.to_string()
    } else {
        format!("{name}.service")
    }
}
