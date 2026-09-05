//! Unit-name helpers shared by the daemon, the IPC layer, and the CLI.

/// Append `.service` when a unit name has no type suffix.
pub fn normalize_unit(name: &str) -> String {
    if name.contains('.') {
        name.to_string()
    } else {
        format!("{name}.service")
    }
}

/// Return whether `name` is safe to use as one file name below a unit directory.
pub fn is_plain_unit_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::is_plain_unit_name;

    #[test]
    fn plain_unit_names_cannot_escape_their_directory() {
        assert!(is_plain_unit_name("demo.service"));
        assert!(is_plain_unit_name("demo@instance.service"));
        for name in [
            "",
            ".",
            "..",
            "../demo.service",
            "dir/demo.service",
            "dir\\demo.service",
        ] {
            assert!(!is_plain_unit_name(name), "accepted {name:?}");
        }
    }
}
