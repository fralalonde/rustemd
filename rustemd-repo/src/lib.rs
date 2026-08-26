//! Typed unit-definition repository. `rustemd-repo` owns the systemd document
//! schema and its parser/writer; callers exchange [`UnitDefinition`] values,
//! never unit-file text. Directory persistence is deliberately the only backend.
mod backend;
mod domain;
mod error;
mod lock;
pub use domain::{UnitDefinition, UnitDocument, UnitEntry, UnitSection, UnitType};
pub use error::Error;
use lock::RepoLock;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
pub struct Repo {
    roots: Vec<PathBuf>,
    lock: RepoLock,
}
impl Repo {
    pub fn open(root: PathBuf) -> Result<Self, Error> {
        Self::open_roots(vec![root])
    }
    pub fn open_roots(roots: Vec<PathBuf>) -> Result<Self, Error> {
        let Some(primary) = roots.first() else {
            return Err(Error::InvalidName(
                "repository needs at least one root".into(),
            ));
        };
        let lock = RepoLock::open(primary);
        Ok(Self { roots, lock })
    }
    pub fn root(&self) -> &Path {
        &self.roots[0]
    }
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
    pub fn list(&self) -> Result<Vec<UnitDefinition>, Error> {
        let mut seen = HashSet::new();
        let mut out = vec![];
        for root in &self.roots {
            for name in backend::list_names(root)? {
                if seen.insert(name.clone())
                    && let Some(unit) = backend::read(root, &name)?
                {
                    out.push(unit);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
    pub fn read(&self, name: &str) -> Result<Option<UnitDefinition>, Error> {
        validate_name(name)?;
        for root in &self.roots {
            if let Some(unit) = backend::read(root, name)? {
                return Ok(Some(unit));
            }
        }
        Ok(None)
    }
    pub fn read_path(&self, path: &Path) -> Result<UnitDefinition, Error> {
        backend::read_path(path)
    }
    pub fn write(&self, unit: &UnitDefinition) -> Result<(), Error> {
        validate_name(&unit.name)?;
        let _guard = self.lock.acquire();
        backend::write(self.root(), unit)
    }
    pub fn create(&self, unit: &UnitDefinition) -> Result<(), Error> {
        validate_name(&unit.name)?;
        let _guard = self.lock.acquire();
        if backend::read(self.root(), &unit.name)?.is_some() {
            return Err(Error::AlreadyExists(unit.name.clone()));
        }
        backend::write(self.root(), unit)
    }
    pub fn update(&self, unit: &UnitDefinition) -> Result<(), Error> {
        validate_name(&unit.name)?;
        let _guard = self.lock.acquire();
        if backend::read(self.root(), &unit.name)?.is_none() {
            return Err(Error::NotFound(unit.name.clone()));
        }
        backend::write(self.root(), unit)
    }
    pub fn delete(&self, name: &str) -> Result<(), Error> {
        validate_name(name)?;
        let _guard = self.lock.acquire();
        backend::delete(self.root(), name)
    }
    pub fn mutate<F>(&self, name: &str, f: F) -> Result<(), Error>
    where
        F: FnOnce(Option<UnitDefinition>) -> Option<UnitDefinition>,
    {
        validate_name(name)?;
        let _guard = self.lock.acquire();
        match f(backend::read(self.root(), name)?) {
            Some(unit) => {
                if unit.name != name {
                    return Err(Error::InvalidName(
                        "mutate closure changed the unit name".into(),
                    ));
                }
                backend::write(self.root(), &unit)
            }
            None => backend::delete(self.root(), name),
        }
    }
}
fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(Error::InvalidName(format!(
            "unit name `{name}` is not a plain unit file name"
        )));
    }
    if UnitType::from_unit_name(name).is_none() {
        return Err(Error::InvalidName(format!(
            "unit name `{name}` has no recognized unit suffix"
        )));
    }
    Ok(())
}
