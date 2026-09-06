use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{App, ClipboardItem};

use super::NativeServiceAdapters;
use super::clipboard::{ClipboardError, FileClipboard, SelectionClipboard};
use super::quick_look::{QuickLookError, QuickLookFactory, QuickLookPanel};
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
impl QuickLookPanel for UnavailablePreview {
    fn preview_file(&mut self, _: &Path) -> Result<(), QuickLookError> {
        Err(QuickLookError::PlatformUnavailable)
    }
    fn dismiss(&mut self) {}
}
impl QuickLookFactory for UnavailablePreview {
    fn create(&self) -> Box<dyn QuickLookPanel> {
        Box::new(UnavailablePreview)
    }
}

pub(crate) fn adapters() -> NativeServiceAdapters {
    NativeServiceAdapters {
        selection_clipboard: Rc::new(TestSelectionClipboard),
        file_clipboard: Rc::new(EmptyFileClipboard),
        quick_look: Rc::new(UnavailablePreview),
    }
}
