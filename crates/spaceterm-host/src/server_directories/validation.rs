//! Fresh native directory and create-target validation. Query results are only observations.
use super::native::{NativeDirectoryProvider, classify, io_failure};
use crate::git::filesystem::RetainedDirectory;
use spaceterm_core::organization::ServerDirectory;
use spaceterm_core::server_directories::*;
use std::path::PathBuf;
use std::sync::Arc;

/// Retained absent or empty destination, rechecked before an external write.
pub struct CreateTargetLease {
    path: PathBuf,
    parent: Arc<dyn RetainedDirectory>,
    directory: Option<Arc<dyn RetainedDirectory>>,
    absent_components: Vec<PathBuf>,
}
impl CreateTargetLease {
    /// Return the destination category observed when this lease was retained.
    pub fn class(&self) -> DirectoryValidationClass {
        if self.directory.is_some() {
            DirectoryValidationClass::EmptyCreateTarget
        } else {
            DirectoryValidationClass::AbsentCreateTarget
        }
    }
    /// Reject replacement, newly occupied destinations and changed ancestors.
    pub fn revalidate(&self) -> Result<(), DirectoryFailure> {
        self.parent.revalidate().map_err(classify)?;
        for absent in &self.absent_components {
            match std::fs::symlink_metadata(absent) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => return Err(DirectoryFailure::IdentityChanged),
                Err(error) => return Err(io_failure(error)),
            }
        }
        if let Some(directory) = &self.directory {
            if std::fs::symlink_metadata(&self.path)
                .map_err(io_failure)?
                .file_type()
                .is_symlink()
            {
                return Err(DirectoryFailure::IdentityChanged);
            }
            directory.revalidate().map_err(classify)?;
            if std::fs::read_dir(&self.path)
                .map_err(io_failure)?
                .next()
                .is_some()
            {
                return Err(DirectoryFailure::Unchecked);
            }
            directory.revalidate().map_err(classify)?;
        }
        Ok(())
    }
}
impl NativeDirectoryProvider {
    /// Retain exact empty-target identity or the nearest existing parent of an absent target.
    pub fn retain_create_target(
        &self,
        directory: &ServerDirectory,
    ) -> Result<CreateTargetLease, DirectoryFailure> {
        let path = self.path(directory)?;
        let mut absent_components = Vec::new();
        let retained = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(DirectoryFailure::NotDirectory);
                }
                Some(
                    self.filesystem
                        .validate_directory(&path)
                        .map_err(classify)?,
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                absent_components.push(path.clone());
                None
            }
            Err(error) => return Err(io_failure(error)),
        };
        let mut parent = path
            .parent()
            .ok_or(DirectoryFailure::InvalidDirectory)?
            .to_path_buf();
        let lease = loop {
            match std::fs::symlink_metadata(&parent) {
                Ok(metadata) if metadata.is_dir() => {
                    break self
                        .filesystem
                        .validate_directory(&parent)
                        .map_err(classify)?;
                }
                Ok(_) => return Err(DirectoryFailure::NotDirectory),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    absent_components.push(parent.clone());
                    parent = parent
                        .parent()
                        .ok_or(DirectoryFailure::InvalidDirectory)?
                        .to_path_buf();
                }
                Err(error) => return Err(io_failure(error)),
            }
        };
        absent_components.reverse();
        let lease = CreateTargetLease {
            path,
            parent: lease,
            directory: retained,
            absent_components,
        };
        lease.revalidate()?;
        Ok(lease)
    }
    /// Observe one directory without granting launch or mutation authority.
    pub fn validate(
        &self,
        directory: &ServerDirectory,
        purpose: DirectoryPurpose,
    ) -> Result<DirectoryValidation, DirectoryFailure> {
        self.path(directory)?;
        let observed = if purpose == DirectoryPurpose::WorktreeCreateTarget {
            self.retain_create_target(directory)
                .map(|lease| lease.class())
        } else {
            self.retain(directory)
                .and_then(|lease| lease.revalidate().map_err(classify))
                .map(|()| DirectoryValidationClass::AvailableDirectory)
        };
        let class = match observed {
            Ok(class) => class,
            Err(DirectoryFailure::Missing) => DirectoryValidationClass::Missing,
            Err(DirectoryFailure::NotDirectory) => DirectoryValidationClass::NotDirectory,
            Err(DirectoryFailure::IdentityChanged) => DirectoryValidationClass::IdentityChanged,
            Err(DirectoryFailure::Unavailable) => DirectoryValidationClass::Unavailable,
            Err(DirectoryFailure::Unchecked) => DirectoryValidationClass::Unchecked,
            Err(error) => return Err(error),
        };
        Ok(DirectoryValidation {
            directory: directory.clone(),
            purpose,
            class,
        })
    }
}
