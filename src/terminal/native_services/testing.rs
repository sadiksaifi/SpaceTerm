use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{App, ClipboardItem};

use super::NativeServiceAdapters;
use super::clipboard::{
    ClipboardError, FileClipboard, SelectionClipboard, TextClipboard, TextClipboardTarget,
};
use super::file_preview::{FilePreviewError, FilePreviewFactory, FilePreviewPanel};
use crate::terminal::SelectionCopy;

struct TestTextClipboard;
impl TextClipboard for TestTextClipboard {
    fn resolve(&self, _: super::osc52::Osc52Target) -> TextClipboardTarget {
        TextClipboardTarget::Clipboard
    }
    fn read(&self, _: TextClipboardTarget, cx: &mut App) -> Result<Option<String>, ClipboardError> {
        Ok(cx.read_from_clipboard().and_then(|item| item.text()))
    }
    fn write(
        &self,
        _: TextClipboardTarget,
        text: &str,
        cx: &mut App,
    ) -> Result<(), ClipboardError> {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        Ok(())
    }
}

struct TestSelectionClipboard;
impl SelectionClipboard for TestSelectionClipboard {
    fn publish(&self, copy: &SelectionCopy, cx: &mut App) -> Result<(), ClipboardError> {
        cx.write_to_clipboard(ClipboardItem::new_string(copy.plain_text.clone()));
        Ok(())
    }
}

struct EmptyFileClipboard;
impl FileClipboard for EmptyFileClipboard {
    fn read_files(&self, _: &gpui::App) -> Result<Vec<PathBuf>, ClipboardError> {
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
        text_clipboard: Rc::new(TestTextClipboard),
        primary_selection: None,
        selection_clipboard: Rc::new(TestSelectionClipboard),
        file_insertion:
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
        file_clipboard: Rc::new(EmptyFileClipboard),
        file_preview: Rc::new(UnavailablePreview),
    }
}
