use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{App, ClipboardItem};

use super::NativeServiceAdapters;
use super::clipboard::{ClipboardError, FileClipboard, SelectionClipboard};
use super::file_preview::{FilePreviewError, FilePreviewFactory, FilePreviewPanel};
use crate::terminal::SelectionCopy;

struct TestSelectionClipboard;
impl SelectionClipboard for TestSelectionClipboard {
    fn publish(&self, copy: &SelectionCopy, cx: &mut App) -> Result<(), ClipboardError> {
        cx.write_to_clipboard(ClipboardItem::new_string(copy.plain_text.clone()));
        Ok(())
    }
}

struct EmptyFileClipboard;
impl FileClipboard for EmptyFileClipboard {
    fn read_files(&self) -> Result<Vec<PathBuf>, ClipboardError> {
        Ok(Vec::new())
    }
}

struct UnavailablePreview;
impl FilePreviewPanel for UnavailablePreview {
    fn preview_file(&mut self, _: &Path) -> Result<(), FilePreviewError> {
        Err(FilePreviewError::PlatformUnavailable)
    }
    fn dismiss(&mut self) {}
}
impl FilePreviewFactory for UnavailablePreview {
    fn create(&self) -> Box<dyn FilePreviewPanel> {
        Box::new(UnavailablePreview)
    }
}

pub(crate) fn adapters() -> NativeServiceAdapters {
    NativeServiceAdapters {
        selection_clipboard: Rc::new(TestSelectionClipboard),
        file_insertion:
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
        file_clipboard: Rc::new(EmptyFileClipboard),
        file_preview: Rc::new(UnavailablePreview),
    }
}
