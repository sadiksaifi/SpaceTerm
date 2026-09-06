use std::path::PathBuf;
use std::rc::Rc;

use gpui::App;

use super::{NativeInsertion, NativeInsertionError};
use crate::terminal::{SelectionCopy, TerminalLocalFileCapabilities};

pub(crate) const PLAIN_TEXT_MIME: &str = "text/plain;charset=utf-8";
pub(crate) const HTML_MIME: &str = "text/html;charset=utf-8";

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct PasteboardRepresentation<'a> {
    pub(crate) mime: &'static str,
    pub(crate) text: &'a str,
}

impl std::fmt::Debug for PasteboardRepresentation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasteboardRepresentation")
            .field("mime", &self.mime)
            .finish_non_exhaustive()
    }
}

pub(crate) fn selection_representations<'a>(
    plain_text: &'a str,
    html: Option<&'a str>,
) -> Vec<PasteboardRepresentation<'a>> {
    let mut representations = vec![PasteboardRepresentation {
        mime: PLAIN_TEXT_MIME,
        text: plain_text,
    }];
    if let Some(html) = html.filter(|html| !html.is_empty()) {
        representations.push(PasteboardRepresentation {
            mime: HTML_MIME,
            text: html,
        });
    }
    representations
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardError {
    Unavailable,
    InvalidFiles,
}

/// Publishes all Selection representations synchronously before returning.
pub(crate) trait SelectionClipboard {
    fn publish(&self, copy: &SelectionCopy, cx: &mut App) -> Result<(), ClipboardError>;
}

/// Discovers ordered file paths without granting authority to insert them.
pub(crate) trait FileClipboard {
    fn read_files(&self) -> Result<Vec<PathBuf>, ClipboardError>;
}

pub(crate) struct SelectionPublication {
    clipboard: Rc<dyn SelectionClipboard>,
    fail_next_write: bool,
}

impl SelectionPublication {
    pub(crate) fn new(clipboard: Rc<dyn SelectionClipboard>) -> Self {
        Self {
            clipboard,
            fail_next_write: false,
        }
    }

    pub(crate) fn write(
        &mut self,
        copy: SelectionCopy,
        cx: &mut App,
    ) -> Result<(), ClipboardError> {
        if copy.plain_text.is_empty() {
            return Ok(());
        }
        if std::mem::take(&mut self.fail_next_write) {
            return Err(ClipboardError::Unavailable);
        }
        self.clipboard.publish(&copy, cx)
    }

    pub(crate) fn cancel_injected_failure(&mut self) {
        self.fail_next_write = false;
    }

    pub(crate) fn fail_next_write(&mut self) {
        self.fail_next_write = true;
    }
}

impl NativeInsertion {
    /// Focus and local authority are checked before consulting either clipboard source.
    pub(crate) fn clipboard(
        policy: super::file_insertion::FileInsertionPolicy,
        files: &dyn FileClipboard,
        text: impl FnOnce() -> Option<String>,
        focused: bool,
        local: TerminalLocalFileCapabilities,
    ) -> Result<Option<Self>, NativeInsertionError> {
        if !focused {
            return Err(NativeInsertionError::TerminalUnfocused);
        }
        if local.are_enabled() {
            let paths = files.read_files().map_err(|_| {
                NativeInsertionError::InvalidFiles("clipboard files are unavailable")
            })?;
            if !paths.is_empty() {
                return Self::dropped_files(policy, &paths, focused, local).map(Some);
            }
        }
        text()
            .map(|text| Self::service_text(text, focused))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ClipboardItem, TestAppContext};
    use std::cell::{Cell, RefCell};

    struct Files {
        reads: Cell<usize>,
        paths: Vec<PathBuf>,
        fail: bool,
    }
    impl FileClipboard for Files {
        fn read_files(&self) -> Result<Vec<PathBuf>, ClipboardError> {
            self.reads.set(self.reads.get() + 1);
            if self.fail {
                Err(ClipboardError::InvalidFiles)
            } else {
                Ok(self.paths.clone())
            }
        }
    }

    #[test]
    fn clipboard_intake_checks_authority_before_reading_and_preserves_file_precedence() {
        let files = Files {
            reads: Cell::new(0),
            paths: vec!["/a b".into(), "/c".into()],
            fail: false,
        };
        let text_reads = Cell::new(0);
        let text = || {
            text_reads.set(text_reads.get() + 1);
            Some("fixture".to_owned())
        };
        assert!(
            NativeInsertion::clipboard(
                crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
                &files,
                text,
                false,
                TerminalLocalFileCapabilities::Enabled
            )
            .is_err()
        );
        assert_eq!((files.reads.get(), text_reads.get()), (0, 0));
        let remote = NativeInsertion::clipboard(
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
            &files,
            text,
            true,
            TerminalLocalFileCapabilities::Disabled,
        )
        .unwrap()
        .unwrap();
        assert!(remote.text() == "fixture");
        assert_eq!((files.reads.get(), text_reads.get()), (0, 1));
        let local = NativeInsertion::clipboard(
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
            &files,
            text,
            true,
            TerminalLocalFileCapabilities::Enabled,
        )
        .unwrap()
        .unwrap();
        assert!(local.text() == "'/a b' '/c'");
        assert_eq!((files.reads.get(), text_reads.get()), (1, 1));
    }

    #[test]
    fn invalid_file_intake_cannot_fall_through_to_text() {
        let files = Files {
            reads: Cell::new(0),
            paths: Vec::new(),
            fail: true,
        };
        let result = NativeInsertion::clipboard(
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
            &files,
            || panic!("unexpected clipboard read"),
            true,
            TerminalLocalFileCapabilities::Enabled,
        );
        assert!(result.is_err());
    }

    struct RecordingClipboard(RefCell<Vec<SelectionCopy>>);
    impl SelectionClipboard for RecordingClipboard {
        fn publish(&self, copy: &SelectionCopy, cx: &mut App) -> Result<(), ClipboardError> {
            self.0.borrow_mut().push(copy.clone());
            cx.write_to_clipboard(ClipboardItem::new_string(copy.plain_text.clone()));
            Ok(())
        }
    }

    #[gpui::test]
    fn publication_preserves_both_representations_and_completes_before_paste(
        cx: &mut TestAppContext,
    ) {
        let clipboard = Rc::new(RecordingClipboard(RefCell::new(Vec::new())));
        let mut publication = SelectionPublication::new(clipboard.clone());
        let copy = SelectionCopy {
            plain_text: "fixture".into(),
            html: Some("<pre>fixture</pre>".into()),
        };
        cx.update(|cx| {
            publication.write(copy.clone(), cx).unwrap();
            assert!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref()
                    == Some(copy.plain_text.as_str())
            );
            publication
                .write(
                    SelectionCopy {
                        plain_text: String::new(),
                        html: None,
                    },
                    cx,
                )
                .unwrap();
        });
        assert_eq!(clipboard.0.borrow().as_slice(), &[copy]);
        let representations = selection_representations("fixture", Some("<pre>fixture</pre>"));
        assert_eq!(
            representations
                .iter()
                .map(|value| value.mime)
                .collect::<Vec<_>>(),
            vec![PLAIN_TEXT_MIME, HTML_MIME]
        );
        assert!(representations[1].text == "<pre>fixture</pre>");
        assert_eq!(selection_representations("fixture", Some("")).len(), 1);
    }
}
