//! macOS filesystem mechanics for read-only SSH host discovery.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::ssh::host_config::{HostConfigFilesystem, HostConfigFilesystemError};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MacosHostConfigFilesystem;

impl HostConfigFilesystem for MacosHostConfigFilesystem {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, HostConfigFilesystemError> {
        std::fs::canonicalize(path).map_err(classify)
    }

    fn read_file_limited(
        &self,
        path: &Path,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>, HostConfigFilesystemError> {
        let file = File::open(path).map_err(classify)?;
        let mut contents = Vec::with_capacity(maximum_bytes.min(16 * 1024));
        file.take(maximum_bytes.saturating_add(1) as u64)
            .read_to_end(&mut contents)
            .map_err(classify)?;
        Ok(contents)
    }

    fn read_directory_limited(
        &self,
        path: &Path,
        maximum_entries: usize,
    ) -> Result<Vec<PathBuf>, HostConfigFilesystemError> {
        let mut entries = Vec::with_capacity(maximum_entries.saturating_add(1));
        for entry in std::fs::read_dir(path).map_err(classify)? {
            entries.push(entry.map_err(classify)?.path());
            if entries.len() > maximum_entries {
                return Ok(entries);
            }
        }
        entries.sort();
        Ok(entries)
    }
}

fn classify(error: std::io::Error) -> HostConfigFilesystemError {
    if error.kind() == std::io::ErrorKind::NotFound {
        HostConfigFilesystemError::Missing
    } else {
        HostConfigFilesystemError::Unavailable
    }
}
