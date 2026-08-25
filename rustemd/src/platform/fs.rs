//! Platform-native filesystem links used for unit enablement and aliases.

use std::path::Path;

pub fn link_file(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => Ok(()),
            Err(symlink_error) => std::fs::hard_link(target, link).map_err(|hardlink_error| {
                std::io::Error::new(
                    hardlink_error.kind(),
                    format!("symlink failed ({symlink_error}); hard-link fallback failed ({hardlink_error})"),
                )
            }),
        }
    }
}
