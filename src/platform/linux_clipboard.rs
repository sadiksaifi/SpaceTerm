//! CLIPBOARD and optional PRIMARY through GPUI's Wayland and X11 clients.
use super::linux_appearance::LinuxAppearancePlatform;
use crate::terminal::SelectionCopy;
use crate::terminal::native_services::clipboard::{
    ClipboardError, ClipboardRead, FileClipboard, PrimarySelection, SelectionClipboard,
    TextClipboard, TextClipboardTarget,
};
use crate::terminal::native_services::file_insertion::{MAX_FILE_INSERTION_BYTES, MAX_FILE_ITEMS};
use crate::terminal::osc52::{MAX_OSC52_CONTENT_BYTES, Osc52Target};
use gpui::{
    App, ClipboardEntry, ClipboardItem, ClipboardReadError, ClipboardSelection, ClipboardString,
};
use std::{path::PathBuf, rc::Rc};

/// OSC 52 reaches the selected native buffer after the Terminal Session checks focus and policy.
pub(super) struct LinuxTextClipboard;

impl TextClipboard for LinuxTextClipboard {
    fn resolve(&self, target: Osc52Target) -> TextClipboardTarget {
        match target {
            Osc52Target::Default | Osc52Target::Standard => TextClipboardTarget::Clipboard,
            Osc52Target::Primary | Osc52Target::Selection => TextClipboardTarget::Primary,
        }
    }

    fn read(&self, target: TextClipboardTarget, cx: &mut App) -> ClipboardRead<Option<String>> {
        let read = cx.read_selection_text(native_target(target), MAX_OSC52_CONTENT_BYTES);
        Box::pin(async move { read.await.map_err(text_read_error) })
    }

    fn write(
        &self,
        target: TextClipboardTarget,
        text: &str,
        cx: &mut App,
    ) -> Result<(), ClipboardError> {
        cx.try_write_selection(
            native_target(target),
            ClipboardItem::new_string(text.to_owned()),
        )
        .map_err(|gpui::ClipboardWriteError::Unavailable| ClipboardError::Unavailable)
    }
}

/// Keeps a native text read failure's classification and none of its detail.
fn text_read_error(error: ClipboardReadError) -> ClipboardError {
    match error {
        ClipboardReadError::TooLarge | ClipboardReadError::UnsupportedContent => {
            ClipboardError::InvalidText
        }
        ClipboardReadError::Unavailable
        | ClipboardReadError::Denied(_)
        | ClipboardReadError::TimedOut => ClipboardError::Unavailable,
    }
}

fn native_target(target: TextClipboardTarget) -> ClipboardSelection {
    match target {
        TextClipboardTarget::Clipboard => ClipboardSelection::Clipboard,
        TextClipboardTarget::Primary => ClipboardSelection::Primary,
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
    fn read_files(&self, cx: &mut App) -> ClipboardRead<Vec<PathBuf>> {
        let read = cx.read_from_clipboard_async();
        Box::pin(async move {
            match read.await {
                Ok(Some(item)) => clipboard_files(&item),
                Ok(None) => Ok(Vec::new()),
                Err(_) => Err(ClipboardError::Unavailable),
            }
        })
    }
}

fn clipboard_files(item: &ClipboardItem) -> Result<Vec<PathBuf>, ClipboardError> {
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
pub(super) struct LinuxPrimarySelection(pub(super) Rc<LinuxAppearancePlatform>);
impl PrimarySelection for LinuxPrimarySelection {
    fn publish(&self, copy: &SelectionCopy, cx: &mut App) {
        if self.0.primary_enabled() && !copy.plain_text.is_empty() {
            cx.write_to_primary(selection_item(copy));
        }
    }
    fn read(&self, cx: &mut App) -> ClipboardRead<Option<String>> {
        if !self.0.primary_enabled() {
            return Box::pin(std::future::ready(Ok(None)));
        }
        let read = cx.read_from_primary_async();
        Box::pin(async move {
            read.await
                .map(|item| item.and_then(|item| item.text()))
                .map_err(text_read_error)
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The test platform answers at once, so each read completes on its first poll.
    fn read(target: TextClipboardTarget, cx: &mut App) -> Result<Option<String>, ClipboardError> {
        pollster::block_on(LinuxTextClipboard.read(target, cx))
    }

    #[gpui::test]
    fn linux_osc52_targets_preserve_independent_selections(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for selection in [TextClipboardTarget::Clipboard, TextClipboardTarget::Primary] {
                assert_eq!(read(selection, cx), Ok(None));
                LinuxTextClipboard.write(selection, "first", cx).unwrap();
                assert_eq!(read(selection, cx), Ok(Some("first".into())));
                LinuxTextClipboard
                    .write(selection, "replacement", cx)
                    .unwrap();
                assert_eq!(read(selection, cx), Ok(Some("replacement".into())));
            }
            for target in [
                Osc52Target::Default,
                Osc52Target::Standard,
                Osc52Target::Primary,
                Osc52Target::Selection,
            ] {
                cx.write_to_clipboard(ClipboardItem::new_string("clipboard".into()));
                cx.write_to_primary(ClipboardItem::new_string("primary".into()));
                let selection = LinuxTextClipboard.resolve(target);
                let (expected, other) = match target {
                    Osc52Target::Default | Osc52Target::Standard => {
                        ("clipboard", ClipboardSelection::Primary)
                    }
                    Osc52Target::Primary | Osc52Target::Selection => {
                        ("primary", ClipboardSelection::Clipboard)
                    }
                };
                assert_eq!(read(selection, cx), Ok(Some(expected.into())));
                LinuxTextClipboard
                    .write(selection, "replacement", cx)
                    .unwrap();
                assert_eq!(read(selection, cx), Ok(Some("replacement".into())));
                assert_eq!(
                    pollster::block_on(cx.read_selection_text(other, MAX_OSC52_CONTENT_BYTES)),
                    Ok(Some(
                        if expected == "primary" {
                            "clipboard"
                        } else {
                            "primary"
                        }
                        .to_owned()
                    ))
                );
            }
        });
    }

    #[gpui::test]
    fn linux_osc52_reads_only_bounded_explicit_text(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for target in [TextClipboardTarget::Clipboard, TextClipboardTarget::Primary] {
                cx.try_write_selection(
                    native_target(target),
                    ClipboardItem {
                        entries: vec![ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                            vec!["/tmp/fixture".into()].into(),
                        ))],
                    },
                )
                .unwrap();
                assert_eq!(read(target, cx), Ok(None));
                cx.try_write_selection(
                    native_target(target),
                    ClipboardItem::new_string("a\r\né".into()),
                )
                .unwrap();
                assert_eq!(read(target, cx), Ok(Some("a\r\né".into())));
                cx.try_write_selection(
                    native_target(target),
                    ClipboardItem::new_string("x".repeat(MAX_OSC52_CONTENT_BYTES)),
                )
                .unwrap();
                assert_eq!(
                    read(target, cx).unwrap().unwrap().len(),
                    MAX_OSC52_CONTENT_BYTES
                );
                cx.try_write_selection(
                    native_target(target),
                    ClipboardItem::new_string("x".repeat(MAX_OSC52_CONTENT_BYTES + 1)),
                )
                .unwrap();
                assert_eq!(read(target, cx), Err(ClipboardError::InvalidText));
            }
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
                pollster::block_on(LinuxFileClipboard.read_files(cx)).unwrap(),
                [PathBuf::from("/a b"), PathBuf::from("/c")]
            );
            cx.write_to_clipboard(ClipboardItem::from(ClipboardEntry::ExternalPaths(
                gpui::ExternalPaths(vec!["/x".into(); MAX_FILE_ITEMS + 1].into()),
            )));
            assert_eq!(
                pollster::block_on(LinuxFileClipboard.read_files(cx)),
                Err(ClipboardError::InvalidFiles)
            );
            let half_budget = format!("/{}", "x".repeat(MAX_FILE_INSERTION_BYTES / 2 - 1));
            let at_budget = vec![PathBuf::from(&half_budget), PathBuf::from(&half_budget)];
            cx.write_to_clipboard(ClipboardItem::from(ClipboardEntry::ExternalPaths(
                gpui::ExternalPaths(at_budget.clone().into()),
            )));
            assert_eq!(
                pollster::block_on(LinuxFileClipboard.read_files(cx)).unwrap(),
                at_budget
            );
            for paths in [
                vec![half_budget.clone().into(), half_budget.into(), "/x".into()],
                vec!["relative".into()],
            ] {
                cx.write_to_clipboard(ClipboardItem::from(ClipboardEntry::ExternalPaths(
                    gpui::ExternalPaths(paths.into()),
                )));
                assert_eq!(
                    pollster::block_on(LinuxFileClipboard.read_files(cx)),
                    Err(ClipboardError::InvalidFiles)
                );
            }
        });
    }

    #[test]
    fn native_text_read_failures_keep_only_their_classification() {
        for (error, expected) in [
            (ClipboardReadError::TooLarge, ClipboardError::InvalidText),
            (
                ClipboardReadError::UnsupportedContent,
                ClipboardError::InvalidText,
            ),
            (ClipboardReadError::TimedOut, ClipboardError::Unavailable),
            (ClipboardReadError::Unavailable, ClipboardError::Unavailable),
            (
                ClipboardReadError::Denied("detail".into()),
                ClipboardError::Unavailable,
            ),
        ] {
            assert_eq!(text_read_error(error), expected);
        }
    }
}
