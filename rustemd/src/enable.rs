//! `enable`/`disable`/`is-enabled` — pure filesystem logic, no daemon needed
//! (matching real `systemctl`, which does the same).
//!
//! Enabling a unit reads its `[Install]` section and creates symlinks:
//! - `WantedBy=/RequiredBy=` -> `<config-dir>/<target>.wants|.requires/<name>`
//! - `Alias=` -> aliases in the config dir
//! - `Also=` units get enabled too.

use std::fs;
use std::path::PathBuf;

use crate::paths::Paths;
use crate::specifier::SpecifierContext;
use crate::unit::InstallConfig;

const SUFFIXES: [&str; 3] = ["service", "timer", "target"];

/// Parse the `[Install]` section of a unit file directly from disk.
pub fn install_section(path: &PathBuf, unit_name: &str) -> Option<InstallConfig> {
    let text = fs::read_to_string(path).ok()?;
    let raw = crate::unit::parse::parse(&text).ok()?;
    let spec = SpecifierContext {
        unit_name: unit_name.to_string(),
        runtime_dir: String::new(),
        user_name: String::new(),
        uid: String::new(),
        home: String::new(),
        hostname: String::new(),
        machine_id: String::new(),
    };
    let exp = |s: &str| spec.expand(s);
    Some(InstallConfig {
        wanted_by: raw
            .list("Install", "WantedBy")
            .into_iter()
            .flat_map(|v| {
                exp(v)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect(),
        required_by: raw
            .list("Install", "RequiredBy")
            .into_iter()
            .flat_map(|v| {
                exp(v)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect(),
        also: raw.list("Install", "Also").into_iter().map(&exp).collect(),
        alias: raw.list("Install", "Alias").into_iter().map(exp).collect(),
    })
}

fn normalize_file_name(name: &str) -> String {
    if !SUFFIXES.iter().any(|s| name.ends_with(&format!(".{s}"))) {
        format!("{name}.service")
    } else {
        name.to_string()
    }
}

/// Enable-state of a unit: enabled / disabled / static / not-found / masked.
pub fn enabled_state(paths: &Paths, name: &str) -> String {
    let name = normalize_file_name(name);
    let Some(path) = paths.find_unit(&name) else {
        return "not-found".to_string();
    };
    let Some(install) = install_section(&path, &name) else {
        return "static".to_string();
    };
    if install.wanted_by.is_empty() && install.required_by.is_empty() && install.alias.is_empty() {
        return "static".to_string();
    }
    if install_links_exist(paths, &name, &install) {
        "enabled".to_string()
    } else {
        "disabled".to_string()
    }
}

/// Create the enable symlinks. Returns human-readable confirmation lines.
pub fn enable(paths: &Paths, name: &str) -> Result<Vec<String>, String> {
    let name = normalize_file_name(name);
    let path = paths
        .find_unit(&name)
        .ok_or_else(|| format!("Failed to enable unit: Unit file {name} not found."))?;
    let install = install_section(&path, &name)
        .ok_or_else(|| format!("Unit {name} has no installation config (no [Install] section)."))?;

    let mut messages = Vec::new();
    for target in install.wanted_by.iter().chain(install.required_by.iter()) {
        let suffix = if install.wanted_by.contains(target) {
            "wants"
        } else {
            "requires"
        };
        let dir = paths.config_dir.join(format!("{target}.{suffix}"));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let link = dir.join(&name);
        if link.exists() {
            let _ = fs::remove_file(&link);
        }
        // Create symlink relative or absolute to the unit file.
        let target_path = absolute_of(&path);
        fs::remove_file(&link).ok();
        crate::platform::fs::link_file(&target_path, &link).map_err(|e| e.to_string())?;
        messages.push(format!(
            "Created symlink {} -> {}",
            link.display(),
            target_path.display()
        ));
    }
    for alias in &install.alias {
        let link = paths.config_dir.join(normalize_file_name(alias));
        let target_path = absolute_of(&path);
        fs::create_dir_all(&paths.config_dir).map_err(|e| e.to_string())?;
        fs::remove_file(&link).ok();
        crate::platform::fs::link_file(&target_path, &link).map_err(|e| e.to_string())?;
        messages.push(format!(
            "Created alias {} -> {}",
            link.display(),
            target_path.display()
        ));
    }
    for also in &install.also {
        messages.extend(enable(paths, &normalize_file_name(also))?);
    }
    Ok(messages)
}

fn absolute_of(p: &PathBuf) -> PathBuf {
    if p.is_absolute() {
        p.clone()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(p)
    }
}

/// Remove the enable symlinks (and `Also=` units').
pub fn disable(paths: &Paths, name: &str) -> Result<Vec<String>, String> {
    let name = normalize_file_name(name);
    link_existing(paths, &name);
    let mut removed = Vec::new();
    let unit_path = paths.find_unit(&name);
    let install = unit_path.as_ref().and_then(|p| install_section(p, &name));

    if let Some(install) = &install {
        for target in install.wanted_by.iter().chain(install.required_by.iter()) {
            for dirname in [format!("{target}.wants"), format!("{target}.requires")] {
                let dir = paths.config_dir.join(dirname);
                let link = dir.join(&name);
                if fs::symlink_metadata(&link).is_ok() {
                    fs::remove_file(&link).ok();
                    removed.push(format!("Removed {}", link.display()));
                }
            }
        }
        for alias in &install.alias {
            let link = paths.config_dir.join(normalize_file_name(alias));
            if fs::symlink_metadata(&link).is_ok() {
                fs::remove_file(&link).ok();
                removed.push(format!("Removed {}", link.display()));
            }
        }
        for also in &install.also {
            removed.extend(disable(paths, &normalize_file_name(also))?);
        }
    }
    if removed.is_empty() {
        removed.push(format!("Unit {name} was not enabled."));
    }
    Ok(removed)
}

/// Remove any enable symlink pointing at `name`, regardless of which target
/// it lives under (robust clean-up for stray/renamed install sections).
fn link_existing(paths: &Paths, name: &str) {
    for suffix in ["wants", "requires"] {
        let Ok(rd) = fs::read_dir(&paths.config_dir) else {
            continue;
        };
        for e in rd.flatten() {
            let f = e.file_name().to_string_lossy().to_string();
            let Some(base) = f.strip_suffix(suffix) else {
                continue;
            };
            let d = paths.config_dir.join(format!("{base}.{suffix}"));
            // Only descend into dirs whose `base` is a target-style name.
            if base.contains(".") {
                if let Ok(rd2) = fs::read_dir(&d) {
                    for l in rd2.flatten() {
                        let link_name = l.file_name().to_string_lossy().to_string();
                        if link_name == name {
                            fs::remove_file(l.path()).ok();
                        }
                    }
                }
            }
        }
    }
}

fn install_links_exist(paths: &Paths, name: &str, install: &InstallConfig) -> bool {
    for target in install.wanted_by.iter().chain(install.required_by.iter()) {
        if paths
            .config_dir
            .join(format!("{target}.wants"))
            .join(name)
            .exists()
        {
            return true;
        }
        if paths
            .config_dir
            .join(format!("{target}.requires"))
            .join(name)
            .exists()
        {
            return true;
        }
    }
    for alias in &install.alias {
        if paths.config_dir.join(normalize_file_name(alias)).exists() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("conf");
        fs::create_dir_all(&cfg).unwrap();
        let paths = Paths {
            user: false,
            unit_path: vec![d.path().join("units")],
            config_dir: cfg.clone(),
            runtime_dir: d.path().join("run"),
        };
        fs::create_dir_all(&paths.unit_path[0]).unwrap();
        (d, paths)
    }

    #[test]
    fn enable_then_disable() {
        let (_d, paths) = setup();
        let unit = paths.unit_path[0].join("hello.service");
        fs::write(&unit, "[Install]\nWantedBy=multi-user.target\n").unwrap();

        assert_eq!(enabled_state(&paths, "hello"), "disabled");
        let msgs = enable(&paths, "hello").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(enabled_state(&paths, "hello"), "enabled");
        assert!(
            paths
                .wants_dir("multi-user.target")
                .join("hello.service")
                .is_symlink()
        );

        let removed = disable(&paths, "hello").unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(enabled_state(&paths, "hello"), "disabled");
    }

    #[test]
    fn static_and_not_found() {
        let (_d, paths) = setup();
        let unit = paths.unit_path[0].join("oneshot.service");
        fs::write(&unit, "[Service]\nType=oneshot\nExecStart=/bin/true\n").unwrap();
        assert_eq!(enabled_state(&paths, "oneshot"), "static");
        assert_eq!(enabled_state(&paths, "missing.service"), "not-found");
    }

    #[test]
    fn aliases_created() {
        let (_d, paths) = setup();
        let unit = paths.unit_path[0].join("web.service");
        fs::write(
            &unit,
            "[Install]\nWantedBy=multi-user.target\nAlias=httpd.service\n",
        )
        .unwrap();
        enable(&paths, "web.service").unwrap();
        assert!(
            paths
                .wants_dir("multi-user.target")
                .join("web.service")
                .is_symlink()
        );
        assert!(paths.config_dir.join("httpd.service").is_symlink());
    }
}
