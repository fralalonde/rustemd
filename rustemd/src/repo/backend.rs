//! Directory persistence for typed unit definitions. Other stores may implement
//! the same typed DAO contract later; Git is intentionally not a repository backend.
use crate::repo::{Error, UnitDefinition, UnitType};
use std::{
    io,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) fn list_names(root: &Path) -> Result<Vec<String>, Error> {
    let mut out = vec![];
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_file()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
            && UnitType::from_unit_name(name).is_some()
        {
            out.push(name.into())
        }
    }
    out.sort();
    Ok(out)
}

pub(crate) fn read(root: &Path, name: &str) -> Result<Option<UnitDefinition>, Error> {
    match std::fs::read_to_string(root.join(name)) {
        Ok(text) => UnitDefinition::parse(name, &text).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn read_path(path: &Path) -> Result<UnitDefinition, Error> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::InvalidName(format!("{} is not a unit file name", path.display())))?;
    UnitDefinition::parse(name, &std::fs::read_to_string(path)?)
}

pub(crate) fn delete(root: &Path, name: &str) -> Result<(), Error> {
    match std::fs::remove_file(root.join(name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) fn write(root: &Path, definition: &UnitDefinition) -> Result<(), Error> {
    std::fs::create_dir_all(root)?;
    let target = root.join(&definition.name);
    let temp = root.join(format!(
        ".{}.rustemd-tmp-{}-{}",
        definition.name,
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), Error> {
        let mut file = std::fs::File::create(&temp)?;
        std::io::Write::write_all(&mut file, definition.to_text().as_bytes())?;
        file.sync_all().ok();
        Ok(())
    })();
    match result {
        Ok(()) => {
            std::fs::rename(&temp, target).map_err(|e| {
                let _ = std::fs::remove_file(&temp);
                Error::Io(e)
            })?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(temp);
            Err(e)
        }
    }
}
