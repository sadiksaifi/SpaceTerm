//! Reading one user-chosen interchange document.
//!
//! Importing a color package or a theme is the only Settings operation that reads a file the user
//! picked rather than a file SpaceTerm owns. The read is bounded before anything is allocated and
//! reports only a typed classification, so a failure carries no path and no native error text.

use std::path::Path;

/// The greatest interchange document SpaceTerm will read.
///
/// The same bound the retained Settings document uses. A color package larger than this is not a
/// color package.
const MAXIMUM_IMPORT_BYTES: u64 = 4 * 1024 * 1024;

/// Why an interchange document could not be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ImportError {
    /// The chosen path is not a readable regular file.
    Unreadable,
    /// The file is larger than SpaceTerm will read.
    TooLarge,
}

impl ImportError {
    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::Unreadable => "That file could not be read.",
            Self::TooLarge => "That file is too large to be a color package.",
        }
    }
}

/// Reads one interchange document, refusing anything that is not a bounded regular file.
///
/// The size is checked before the read so an enormous or endless file cannot be pulled into memory.
pub(super) fn read_interchange_document(path: &Path) -> Result<Vec<u8>, ImportError> {
    let metadata = std::fs::metadata(path).map_err(|_| ImportError::Unreadable)?;
    if !metadata.is_file() {
        return Err(ImportError::Unreadable);
    }
    if metadata.len() > MAXIMUM_IMPORT_BYTES {
        return Err(ImportError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|_| ImportError::Unreadable)?;
    // The file may have grown between the check and the read.
    if bytes.len() as u64 > MAXIMUM_IMPORT_BYTES {
        return Err(ImportError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join("spaceterm-settings-import-tests");
        std::fs::create_dir_all(&directory).expect("scratch directory");
        directory.join(name)
    }

    #[test]
    fn a_bounded_document_is_read() {
        let path = scratch("bounded.json");
        std::fs::write(&path, b"{\"schema_version\":1}").expect("fixture");

        let bytes = read_interchange_document(&path).expect("a bounded file should be read");

        assert_eq!(bytes, b"{\"schema_version\":1}");
    }

    #[test]
    fn a_document_beyond_the_bound_is_refused_before_it_is_read() {
        let path = scratch("oversized.json");
        let oversized = vec![b'x'; (MAXIMUM_IMPORT_BYTES + 1) as usize];
        std::fs::write(&path, &oversized).expect("fixture");

        assert_eq!(read_interchange_document(&path), Err(ImportError::TooLarge));
    }

    #[test]
    fn a_missing_path_is_refused() {
        let path = scratch("missing.json");
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            read_interchange_document(&path),
            Err(ImportError::Unreadable)
        );
    }

    #[test]
    fn a_directory_is_refused() {
        let path = scratch("directory-fixture");
        std::fs::create_dir_all(&path).expect("fixture");

        assert_eq!(
            read_interchange_document(&path),
            Err(ImportError::Unreadable)
        );
    }

    #[test]
    fn every_failure_reports_content_free_wording() {
        for error in [ImportError::Unreadable, ImportError::TooLarge] {
            let message = error.message();
            assert!(!message.is_empty());
            assert!(!message.contains('/'), "{message} should carry no path");
        }
    }
}
