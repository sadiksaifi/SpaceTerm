//! A [`Store`] over account-private directories. Each [`Mount`] puts one directory's files in
//! the root; the marker, `migration-backup/` and `reset/` live in the root's home directory.
use super::{Area, MARKER, Store, StoreError};
use crate::{CoreError, storage::Directory};
use std::ffi::CString;
use std::path::{Path, PathBuf};

const BACKUP: &str = "migration-backup";
const RESET: &str = "reset";

/// One directory whose files belong to a root.
pub struct Mount {
    prefix: &'static str,
    path: PathBuf,
    tree: bool,
    select: fn(&str) -> bool,
}

impl Mount {
    /// The files directly in `path`.
    pub fn files(path: PathBuf) -> Self {
        Self {
            prefix: "",
            path,
            tree: false,
            select: |_| true,
        }
    }
    /// Every file in `path` and its subdirectories.
    pub fn tree(path: PathBuf) -> Self {
        Self {
            tree: true,
            ..Self::files(path)
        }
    }
    /// Names this mount's files `prefix/...` in a root that mounts several directories.
    pub fn at(self, prefix: &'static str) -> Self {
        Self { prefix, ..self }
    }
    /// Keeps only the entries directly in the mounted directory whose name `select` accepts.
    pub fn only(self, select: fn(&str) -> bool) -> Self {
        Self { select, ..self }
    }
}

/// A root whose marker, backup and reset areas live in `home`.
pub struct DirectoryStore {
    home: PathBuf,
    mounts: Vec<Mount>,
}

impl DirectoryStore {
    pub fn new(home: PathBuf, mounts: Vec<Mount>) -> Self {
        Self { home, mounts }
    }

    /// The directory and file name of `path` in `area`.
    fn locate(&self, area: Area, path: &str) -> Result<(PathBuf, String), StoreError> {
        let components: Vec<&str> = path.split('/').collect();
        if components
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == ".." || part.contains('\0'))
        {
            return Err(StoreError::Unsafe);
        }
        let (name, parents) = components.split_last().ok_or(StoreError::Unsafe)?;
        let mut directory = match area {
            Area::Backup => self.home.join(BACKUP),
            Area::Reset => self.home.join(RESET),
            Area::Root if path == MARKER => self.home.clone(),
            Area::Root => {
                let (mount, rest) = self.mount(parents)?;
                let mut directory = mount.path.clone();
                directory.extend(rest);
                return Ok((directory, (*name).to_owned()));
            }
        };
        directory.extend(parents);
        Ok((directory, (*name).to_owned()))
    }

    fn mount<'a>(&self, parents: &'a [&'a str]) -> Result<(&Mount, &'a [&'a str]), StoreError> {
        for mount in &self.mounts {
            if mount.prefix.is_empty() {
                return Ok((mount, parents));
            }
            if parents.first() == Some(&mount.prefix) {
                return Ok((mount, &parents[1..]));
            }
        }
        Err(StoreError::Unsafe)
    }

    fn open(directory: &Path, create: bool) -> Result<Option<Directory>, StoreError> {
        match Directory::open(directory, create) {
            Ok(directory) => Ok(Some(directory)),
            Err(CoreError::NotSetUp) => Ok(None),
            Err(error) => Err(classify(error)),
        }
    }
}

impl Store for DirectoryStore {
    fn list(&self, area: Area) -> Result<Vec<String>, StoreError> {
        let mut paths = Vec::new();
        match area {
            Area::Backup => walk(&self.home.join(BACKUP), "", true, &|_| true, &mut paths)?,
            Area::Reset => walk(&self.home.join(RESET), "", true, &|_| true, &mut paths)?,
            Area::Root => {
                for mount in &self.mounts {
                    let home = mount.path == self.home;
                    let select = mount.select;
                    walk(
                        &mount.path,
                        mount.prefix,
                        mount.tree,
                        &|name| select(name) && !(home && (name == BACKUP || name == RESET)),
                        &mut paths,
                    )?;
                }
            }
        }
        Ok(paths)
    }

    fn read(&self, area: Area, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let (directory, name) = self.locate(area, path)?;
        let Some(directory) = Self::open(&directory, false)? else {
            return Ok(None);
        };
        directory.read_bytes(&name).map_err(classify)
    }

    fn write(&self, area: Area, path: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let (directory, name) = self.locate(area, path)?;
        let directory = Self::open(&directory, true)?.ok_or(StoreError::Unavailable)?;
        let name = CString::new(name).map_err(|_| StoreError::Unsafe)?;
        directory.write_bytes(&name, bytes).map_err(classify)
    }

    fn remove(&self, area: Area, path: &str) -> Result<(), StoreError> {
        let (directory, name) = self.locate(area, path)?;
        let Some(directory) = Self::open(&directory, false)? else {
            return Ok(());
        };
        let name = CString::new(name).map_err(|_| StoreError::Unsafe)?;
        directory.remove_record(&name).map_err(classify)
    }
}

/// Collects the regular files in `directory` as `prefix/...`. Symbolic links are never followed.
fn walk(
    directory: &Path,
    prefix: &str,
    tree: bool,
    select: &dyn Fn(&str) -> bool,
    paths: &mut Vec<String>,
) -> Result<(), StoreError> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(StoreError::Unavailable),
    };
    for entry in entries {
        let entry = entry.map_err(|_| StoreError::Unavailable)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !select(&name) {
            continue;
        }
        let kind = entry.file_type().map_err(|_| StoreError::Unavailable)?;
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if kind.is_file() {
            paths.push(path);
        } else if kind.is_dir() && tree {
            walk(&entry.path(), &path, true, &|_| true, paths)?;
        }
    }
    Ok(())
}

fn classify(error: CoreError) -> StoreError {
    match error {
        CoreError::UnsafeStorage => StoreError::Unsafe,
        _ => StoreError::Unavailable,
    }
}
