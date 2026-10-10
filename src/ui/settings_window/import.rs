//! Reads one user-chosen file, a Zed theme, an exported Settings Document, or a Background Image,
//! with a byte bound.
//! Failures carry only a typed classification, never a path or native error text.

use std::{fs::File, io::Read, path::Path};

use crate::platform::selected_file::SelectedFileOpener;

/// The greatest file SpaceTerm will read, shared by theme translation and the Settings Document.
const MAXIMUM_IMPORT_BYTES: u64 = crate::appearance::MAX_FAMILY_BYTES as u64;

/// Why a chosen file could not be read.
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
            Self::TooLarge => "That file is too large to be a theme file.",
        }
    }
}

/// Reads one chosen file, refusing anything that is not a bounded regular file. Checks use the
/// opened file, and the read stays bounded if that file grows.
pub(super) fn read_selected_document(
    path: &Path,
    opener: &dyn SelectedFileOpener,
) -> Result<Vec<u8>, ImportError> {
    read_selected_file(path, opener, MAXIMUM_IMPORT_BYTES)
}

/// Reads one chosen file of at most `limit` bytes, as [`read_selected_document`] does.
pub(super) fn read_selected_file(
    path: &Path,
    opener: &dyn SelectedFileOpener,
    limit: u64,
) -> Result<Vec<u8>, ImportError> {
    read_open_document(
        opener.open(path).map_err(|_| ImportError::Unreadable)?,
        limit,
    )
}

fn read_open_document(file: File, limit: u64) -> Result<Vec<u8>, ImportError> {
    let metadata = file.metadata().map_err(|_| ImportError::Unreadable)?;
    if !metadata.is_file() {
        return Err(ImportError::Unreadable);
    }
    if metadata.len() > limit {
        return Err(ImportError::TooLarge);
    }
    read_bounded(file, limit)
}

fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>, ImportError> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ImportError::Unreadable)?;
    if bytes.len() as u64 > limit {
        return Err(ImportError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureFileOpener;

    impl SelectedFileOpener for FixtureFileOpener {
        fn open(
            &self,
            path: &Path,
        ) -> Result<File, crate::platform::selected_file::SelectedFileOpenError> {
            File::open(path).map_err(|_| crate::platform::selected_file::SelectedFileOpenError)
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join("spaceterm-settings-import-tests");
        std::fs::create_dir_all(&directory).expect("scratch directory");
        directory.join(name)
    }

    #[test]
    fn a_bounded_document_is_read() {
        let path = scratch("bounded.json");
        std::fs::write(&path, b"{\"schema_version\":1}").expect("fixture");

        let bytes = read_selected_document(&path, &FixtureFileOpener)
            .expect("a bounded file should be read");

        assert_eq!(bytes, b"{\"schema_version\":1}");
    }

    #[test]
    fn a_document_at_the_bound_is_read() {
        let bytes = vec![b'x'; MAXIMUM_IMPORT_BYTES as usize];

        assert_eq!(
            read_bounded(bytes.as_slice(), MAXIMUM_IMPORT_BYTES),
            Ok(bytes)
        );
    }

    #[test]
    fn a_growing_document_cannot_read_past_the_bound_and_one_sentinel_byte() {
        struct GrowingDocument(u64);

        impl Read for GrowingDocument {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                buffer.fill(b'x');
                self.0 += buffer.len() as u64;
                assert!(self.0 <= MAXIMUM_IMPORT_BYTES + 1);
                Ok(buffer.len())
            }
        }

        let mut document = GrowingDocument(0);
        assert_eq!(
            read_bounded(&mut document, MAXIMUM_IMPORT_BYTES),
            Err(ImportError::TooLarge)
        );
        assert_eq!(document.0, MAXIMUM_IMPORT_BYTES + 1);
    }

    #[test]
    fn replacing_the_selected_path_does_not_replace_the_open_document() {
        let path = scratch("replaced.json");
        std::fs::write(&path, b"original").expect("fixture");
        let file = FixtureFileOpener.open(&path).expect("open fixture");
        std::fs::remove_file(&path).expect("remove fixture");
        std::fs::write(&path, b"replacement").expect("replacement fixture");

        assert_eq!(
            read_open_document(file, MAXIMUM_IMPORT_BYTES),
            Ok(b"original".to_vec())
        );
    }

    #[test]
    fn a_document_beyond_the_bound_is_refused_before_it_is_read() {
        let path = scratch("oversized.json");
        let oversized = vec![b'x'; (MAXIMUM_IMPORT_BYTES + 1) as usize];
        std::fs::write(&path, &oversized).expect("fixture");

        assert_eq!(
            read_selected_document(&path, &FixtureFileOpener),
            Err(ImportError::TooLarge)
        );
    }

    #[test]
    fn a_missing_path_is_refused() {
        let path = scratch("missing.json");
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            read_selected_document(&path, &FixtureFileOpener),
            Err(ImportError::Unreadable)
        );
    }

    #[test]
    fn a_directory_is_refused() {
        let path = scratch("directory-fixture");
        std::fs::create_dir_all(&path).expect("fixture");

        assert_eq!(
            read_selected_document(&path, &FixtureFileOpener),
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
