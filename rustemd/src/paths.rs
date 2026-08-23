//! Filesystem layout for system vs. user manager, mirroring systemd's
//! search-path precedence. Test hooks via `RUSTEMD_*` env vars let tests
//! run the real daemon against scratch directories.

use std::env;
use std::path::PathBuf;

/// Resolved path layout for one manager instance.
#[derive(Debug, Clone)]
pub struct Paths {
    pub user: bool,
    /// Unit search path, highest precedence first.
    pub unit_path: Vec<PathBuf>,
    /// Where enable/disable symlinks and `default.target` live.
    pub config_dir: PathBuf,
    /// Runtime dir holding the control socket branch.
    pub runtime_dir: PathBuf,
}

impl Paths {
    pub fn system() -> Self {
        let runtime = env::var_os("RUSTEMD_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/run"));
        let unit_path: Vec<PathBuf> = if let Some(p) = env::var_os("RUSTEMD_UNIT_PATH") {
            env::split_paths(&p).collect()
        } else {
            vec![
                PathBuf::from("/etc/systemd/system"),
                PathBuf::from("/run/systemd/system"),
                PathBuf::from("/usr/lib/systemd/system"),
            ]
        };
        let config_dir = env::var_os("RUSTEMD_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/etc/systemd/system"));
        Paths {
            user: false,
            unit_path,
            config_dir,
            runtime_dir: runtime,
        }
    }

    pub fn user() -> Result<Self, String> {
        let runtime = if let Some(p) = env::var_os("RUSTEMD_RUNTIME_DIR") {
            PathBuf::from(p)
        } else {
            let xdg = env::var_os("XDG_RUNTIME_DIR")
                .ok_or_else(|| "XDG_RUNTIME_DIR is not set (needed for user mode)".to_string())?;
            PathBuf::from(xdg)
        };
        let unit_path: Vec<PathBuf> = if let Some(p) = env::var_os("RUSTEMD_UNIT_PATH") {
            env::split_paths(&p).collect()
        } else {
            let config = user_config_dir();
            vec![
                config.clone(),
                PathBuf::from("/etc/systemd/user"),
                PathBuf::from("/usr/lib/systemd/user"),
            ]
        };
        let config_dir = if let Some(p) = env::var_os("RUSTEMD_CONFIG_DIR") {
            PathBuf::from(p)
        } else {
            user_config_dir()
        };
        Ok(Paths {
            user: true,
            unit_path,
            config_dir,
            runtime_dir: runtime,
        })
    }

    /// Override for tests: full path to the control socket.
    pub fn socket_override() -> Option<PathBuf> {
        env::var_os("RUSTEMD_SOCKET").map(PathBuf::from)
    }

    /// Path to the manager's control unix socket.
    pub fn control_socket(&self) -> PathBuf {
        if let Some(s) = Self::socket_override() {
            return s;
        }
        self.runtime_dir.join("rustemd").join("control")
    }

    /// Path to the sd_notify-compatible datagram socket.
    pub fn notify_socket(&self) -> PathBuf {
        self.runtime_dir.join("rustemd").join("notify")
    }

    /// `%t` specifier value.
    pub fn runtime_dir_spec(&self) -> &PathBuf {
        &self.runtime_dir
    }

    /// Where `default.target` lives (a symlink in the config dir).
    pub fn default_target(&self) -> PathBuf {
        self.config_dir.join("default.target")
    }

    /// Find the highest-precedence unit file for `name`, if any.
    pub fn find_unit(&self, name: &str) -> Option<PathBuf> {
        for dir in &self.unit_path {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
        #[cfg(feature = "boot")]
        {
            // Template instantiation: `getty@tty1.service` -> `getty@.service`.
            // The requested name is still used for specifier expansion, so
            // `%i` = "tty1", `%p` = "getty".
            if let Some(at) = name.find('@') {
                if let Some(dot) = name.rfind('.') {
                    if dot > at {
                        let tmpl = format!("{}.{}", &name[..=at], &name[dot + 1..]);
                        for dir in &self.unit_path {
                            let p = dir.join(&tmpl);
                            if p.is_file() {
                                return Some(p);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// All drop-in `.conf` files for `name` across all paths, sorted so the
    /// highest-precedence dirs are read last (overriding earlier ones).
    pub fn dropins(&self, name: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        // Reverse so iteration order (lowest precedence first) appends
        // highest-precedence drop-ins last.
        for dir in self.unit_path.iter().rev() {
            let d = dir.join(format!("{name}.d"));
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    if e.path().extension().map(|x| x == "conf").unwrap_or(false)
                        && e.path().is_file()
                    {
                        out.push(e.path());
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// Symlinked deps in `<name>.wants` and `<name>.requires` dirs.
    pub fn dir_deps(&self, name: &str, kind: &str) -> Vec<String> {
        let mut out = Vec::new();
        for dir in &self.unit_path {
            let d = dir.join(format!("{name}.{kind}"));
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    let fname = match p.file_name().and_then(|f| f.to_str()) {
                        Some(f) => f.to_string(),
                        None => continue,
                    };
                    if p.is_symlink() || p.is_file() {
                        out.push(fname);
                    }
                }
            }
        }
        out
    }

    /// Wants dir for `target` in the config dir (where `enable` writes).
    pub fn wants_dir(&self, target: &str) -> PathBuf {
        self.config_dir.join(format!("{target}.wants"))
    }
    pub fn requires_dir(&self, target: &str) -> PathBuf {
        self.config_dir.join(format!("{target}.requires"))
    }
}

fn user_config_dir() -> PathBuf {
    if let Some(c) = env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(c).join("systemd").join("user")
    } else if let Some(h) = env::var_os("HOME") {
        PathBuf::from(h)
            .join(".config")
            .join("systemd")
            .join("user")
    } else {
        PathBuf::from(".")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_defaults() {
        let p = Paths::system();
        assert!(!p.user);
        assert_eq!(p.runtime_dir, PathBuf::from("/run"));
        assert_eq!(p.control_socket(), PathBuf::from("/run/rustemd/control"));
    }

    #[test]
    fn unit_lookup_precedence() {
        let d = tempfile::tempdir().unwrap();
        let etc = d.path().join("etc");
        let usr = d.path().join("usr");
        std::fs::create_dir_all(etc.join("systemd/system")).unwrap();
        std::fs::create_dir_all(usr.join("lib/systemd/system")).unwrap();
        std::fs::write(etc.join("systemd/system/foo.service"), "x").unwrap();
        std::fs::write(usr.join("lib/systemd/system/foo.service"), "y").unwrap();

        let paths = Paths {
            user: false,
            unit_path: vec![etc.join("systemd/system"), usr.join("lib/systemd/system")],
            config_dir: etc.join("systemd/system"),
            runtime_dir: d.path().to_path_buf(),
        };
        // Highest precedence wins.
        assert_eq!(
            paths.find_unit("foo.service").unwrap(),
            etc.join("systemd/system/foo.service")
        );
        assert!(paths.find_unit("nope.service").is_none());
    }

    #[cfg(feature = "boot")]
    #[test]
    fn template_instantiation() {
        let d = tempfile::tempdir().unwrap();
        let etc = d.path().join("etc");
        std::fs::create_dir_all(etc.join("systemd/system")).unwrap();
        std::fs::write(etc.join("systemd/system/getty@.service"), "x").unwrap();

        let paths = Paths {
            user: false,
            unit_path: vec![etc.join("systemd/system")],
            config_dir: etc.join("systemd/system"),
            runtime_dir: d.path().to_path_buf(),
        };
        // `getty@tty1.service` resolves to the `getty@.service` template.
        assert_eq!(
            paths.find_unit("getty@tty1.service").unwrap(),
            etc.join("systemd/system/getty@.service")
        );
        // A templated name with no template file is still not found.
        assert!(paths.find_unit("other@x.service").is_none());
        // An exact file still wins over the template fallback.
        std::fs::write(etc.join("systemd/system/foo@bar.service"), "y").unwrap();
        assert!(
            paths
                .find_unit("foo@bar.service")
                .unwrap()
                .ends_with("foo@bar.service")
        );
    }

    #[test]
    fn dropin_ordering() {
        let d = tempfile::tempdir().unwrap();
        let etc = d.path().join("etc");
        let usr = d.path().join("usr");
        let e = etc.join("systemd/system");
        let u = usr.join("lib/systemd/system");
        std::fs::create_dir_all(e.join("foo.service.d")).unwrap();
        std::fs::create_dir_all(u.join("foo.service.d")).unwrap();
        std::fs::write(e.join("foo.service.d/10-override.conf"), "x").unwrap();
        std::fs::write(u.join("foo.service.d/20-override.conf"), "y").unwrap();
        std::fs::write(e.join("foo.service.d/05-first.conf"), "z").unwrap();

        let paths = Paths {
            user: false,
            unit_path: vec![e.clone(), u],
            config_dir: e,
            runtime_dir: d.path().to_path_buf(),
        };
        let drops = paths.dropins("foo.service");
        assert_eq!(drops.len(), 3);
        // Sorted lexicographically: 05-first, 10-override, 20-override.
        assert!(drops[0].ends_with("05-first.conf"));
        assert!(drops[1].ends_with("10-override.conf"));
        assert!(drops[2].ends_with("20-override.conf"));
    }
}
