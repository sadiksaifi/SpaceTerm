//! CLIPBOARD and optional PRIMARY through GPUI's Wayland and X11 clients.
use super::linux_appearance::LinuxAppearancePlatform;
use crate::terminal::SelectionCopy;
use crate::terminal::native_services::clipboard::{
    ClipboardError, FileClipboard, PrimarySelection, SelectionClipboard, TextClipboard,
};
use crate::terminal::native_services::file_insertion::{MAX_FILE_INSERTION_BYTES, MAX_FILE_ITEMS};
use gpui::{App, ClipboardEntry, ClipboardItem, ClipboardString};
use std::{path::PathBuf, rc::Rc};

/// OSC 52 uses the system text clipboard after the Session owner checks focus and policy.
pub(super) struct LinuxTextClipboard;

impl TextClipboard for LinuxTextClipboard {
    fn read(&self, cx: &mut App) -> Result<Option<String>, ClipboardError> {
        Ok(cx.read_from_clipboard().and_then(|item| item.text()))
    }

    fn write(&self, text: &str, cx: &mut App) -> Result<(), ClipboardError> {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        Ok(())
    }
}

fn selection_item(copy: &SelectionCopy) -> ClipboardItem {
    let mut string = ClipboardString::new(copy.plain_text.clone());
    if let Some(html) = copy.html.as_ref().filter(|html| !html.is_empty()) {
        string = string.with_html(html.clone());
    }
    ClipboardEntry::String(string).into()
}

pub(super) struct LinuxSelectionClipboard;
impl SelectionClipboard for LinuxSelectionClipboard {
    fn publish(&self, copy: &SelectionCopy, cx: &mut App) -> Result<(), ClipboardError> {
        cx.write_to_clipboard(selection_item(copy));
        Ok(())
    }
}
pub(super) struct LinuxFileClipboard;
impl FileClipboard for LinuxFileClipboard {
    fn read_files(&self, cx: &App) -> Result<Vec<PathBuf>, ClipboardError> {
        let Some(item) = cx.read_from_clipboard() else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        let mut bytes = 0usize;
        for entry in item.entries() {
            if let ClipboardEntry::ExternalPaths(paths) = entry {
                for path in paths.paths() {
                    bytes = bytes.saturating_add(path.as_os_str().len());
                    if result.len() >= MAX_FILE_ITEMS
                        || bytes > MAX_FILE_INSERTION_BYTES
                        || !path.is_absolute()
                    {
                        return Err(ClipboardError::InvalidFiles);
                    }
                    result.push(path.clone());
                }
            }
        }
        Ok(result)
    }
}
pub(super) struct LinuxPrimarySelection(pub(super) Rc<LinuxAppearancePlatform>);
impl PrimarySelection for LinuxPrimarySelection {
    fn publish(&self, copy: &SelectionCopy, cx: &mut App) {
        if self.0.primary_enabled() && !copy.plain_text.is_empty() {
            cx.write_to_primary(selection_item(copy));
        }
    }
    fn read(&self, cx: &App) -> Option<String> {
        self.0
            .primary_enabled()
            .then(|| cx.read_from_primary().and_then(|item| item.text()))
            .flatten()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn linux_text_clipboard_reads_and_replaces_system_text(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(LinuxTextClipboard.read(cx), Ok(None));
            LinuxTextClipboard.write("first", cx).unwrap();
            assert_eq!(LinuxTextClipboard.read(cx), Ok(Some("first".into())));
            LinuxTextClipboard.write("replacement", cx).unwrap();
            assert_eq!(LinuxTextClipboard.read(cx), Ok(Some("replacement".into())));
        });
    }

    #[gpui::test]
    fn selection_clipboards_keep_plain_text_and_html_alternates(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let copy = SelectionCopy {
                plain_text: "<selection>".into(),
                html: Some("<pre>&lt;selection&gt;</pre>".into()),
            };
            LinuxSelectionClipboard.publish(&copy, cx).unwrap();
            let item = cx.read_from_clipboard().unwrap();
            assert_eq!(item.text().as_deref(), Some("<selection>"));
            assert_eq!(item.html(), copy.html.as_deref());
            let primary = LinuxPrimarySelection(Rc::new(LinuxAppearancePlatform::new(None)));
            primary.publish(&copy, cx);
            assert_eq!(cx.read_from_primary().unwrap().html(), copy.html.as_deref());
            let plain = SelectionCopy {
                plain_text: "plain".into(),
                html: None,
            };
            LinuxSelectionClipboard.publish(&plain, cx).unwrap();
            assert_eq!(cx.read_from_clipboard().unwrap().html(), None);
        });
    }

    #[gpui::test]
    fn clipboard_files_remain_ordered_and_bounded(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.write_to_clipboard(ClipboardItem::from(ClipboardEntry::ExternalPaths(
                gpui::ExternalPaths(smallvec::smallvec!["/a b".into(), "/c".into()]),
            )));
            assert_eq!(
                LinuxFileClipboard.read_files(cx).unwrap(),
                [PathBuf::from("/a b"), PathBuf::from("/c")]
            );
            cx.write_to_clipboard(ClipboardItem::from(ClipboardEntry::ExternalPaths(
                gpui::ExternalPaths(vec!["/x".into(); MAX_FILE_ITEMS + 1].into()),
            )));
            assert_eq!(
                LinuxFileClipboard.read_files(cx),
                Err(ClipboardError::InvalidFiles)
            );
        });
    }
}
