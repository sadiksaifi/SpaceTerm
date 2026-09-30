//! The Linux CLIPBOARD selection through GPUI's Wayland and X11 clients.
use std::path::PathBuf;

use gpui::{App, ClipboardItem};

use crate::terminal::SelectionCopy;
use crate::terminal::native_services::clipboard::{ClipboardError, FileClipboard, SelectionClipboard};

/// Publishes the plain-text Selection representation, the one every Linux client reads.
pub(super) struct LinuxSelectionClipboard;

impl SelectionClipboard for LinuxSelectionClipboard {
    fn publish(&self, copy: &SelectionCopy, cx: &mut App) -> Result<(), ClipboardError> {
        cx.write_to_clipboard(ClipboardItem::new_string(copy.plain_text.clone()));
        Ok(())
    }
}

/// Reports no copied files until the CLIPBOARD file-list representation is read, so a paste
/// falls back to the clipboard text.
pub(super) struct LinuxFileClipboard;

impl FileClipboard for LinuxFileClipboard {
    fn read_files(&self) -> Result<Vec<PathBuf>, ClipboardError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn linux_selection_publishes_plain_text_to_the_clipboard(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            LinuxSelectionClipboard
                .publish(
                    &SelectionCopy {
                        plain_text: "copied".to_owned(),
                        html: Some("<b>copied</b>".to_owned()),
                    },
                    cx,
                )
                .unwrap();
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
                Some("copied")
            );
        });
        assert_eq!(LinuxFileClipboard.read_files(), Ok(Vec::new()));
    }
}
