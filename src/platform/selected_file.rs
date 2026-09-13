//! Opening an explicitly selected local file while retaining the selected object.

use std::{fs::File, path::Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SelectedFileOpenError;

/// Supplies an opened object without reading contents or waiting on a special file.
/// Callers own file-type validation, size limits, and content reads on the returned handle.
pub(crate) trait SelectedFileOpener: Send + Sync {
    fn open(&self, path: &Path) -> Result<File, SelectedFileOpenError>;
}
