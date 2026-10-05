use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;

use gpui::App;

use super::{PasteIntakeError, PastePayload};
use crate::terminal::{SelectionCopy, TerminalLocalFileCapabilities};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ClipboardPreferences {
    pub(crate) allow_write: bool,
    pub(crate) allow_read: bool,
}

impl Default for ClipboardPreferences {
    fn default() -> Self {
        Self {
            allow_write: true,
            allow_read: false,
        }
    }
}

/// The independently owned text selection chosen by the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextClipboardTarget {
    Clipboard,
    #[allow(
        dead_code,
        reason = "not every host exposes an independent PRIMARY selection"
    )]
    Primary,
}

/// A native clipboard read that completes when the selection owner answers or the host's read
/// deadline passes, without blocking the UI thread. Dropping it discards the result.
pub(crate) type ClipboardRead<T> = Pin<Box<dyn Future<Output = Result<T, ClipboardError>>>>;

/// Plain text only. Target resolution follows the Session's focus and Settings checks.
/// This Interface carries no local file authority and never falls back to another selection.
pub(crate) trait TextClipboard {
    fn resolve(&self, target: super::osc52::Osc52Target) -> TextClipboardTarget;
    fn read(&self, target: TextClipboardTarget, cx: &mut App) -> ClipboardRead<Option<String>>;
    fn write(
        &self,
        target: TextClipboardTarget,
        text: &str,
        cx: &mut App,
    ) -> Result<(), ClipboardError>;
}

pub(crate) const PLAIN_TEXT_MIME: &str = "text/plain;charset=utf-8";
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native pasteboard Adapter publishes typed representations"
    )
)]
pub(crate) const HTML_MIME: &str = "text/html;charset=utf-8";

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native pasteboard Adapter publishes typed representations"
    )
)]
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

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native pasteboard Adapter publishes typed representations"
    )
)]
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

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native pasteboard Adapter reports clipboard failures"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardError {
    Unavailable,
    InvalidFiles,
    InvalidText,
}

/// Publishes all Selection representations synchronously before returning.
pub(crate) trait SelectionClipboard {
    fn publish(&self, copy: &SelectionCopy, cx: &mut App) -> Result<(), ClipboardError>;
}

/// Discovers ordered file paths without granting authority to insert them.
pub(crate) trait FileClipboard {
    fn read_files(&self, cx: &mut App) -> ClipboardRead<Vec<PathBuf>>;
}

/// The host's optional PRIMARY selection, independent of explicit Copy.
pub(crate) trait PrimarySelection {
    fn publish(&self, copy: &SelectionCopy, cx: &mut App);
    fn read(&self, cx: &mut App) -> ClipboardRead<Option<String>>;
}

pub(crate) struct SelectionPublication {
    clipboard: Rc<dyn SelectionClipboard>,
    #[cfg(test)]
    fail_next_write: bool,
}

impl SelectionPublication {
    pub(crate) fn new(clipboard: Rc<dyn SelectionClipboard>) -> Self {
        Self {
            clipboard,
            #[cfg(test)]
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
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_write) {
            return Err(ClipboardError::Unavailable);
        }
        self.clipboard.publish(&copy, cx)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_write(&mut self) {
        self.fail_next_write = true;
    }
}

impl PastePayload {
    /// Focus and local authority are checked before consulting either clipboard source, and text
    /// is read only when no file was found.
    pub(crate) async fn clipboard<Files, Text>(
        policy: super::file_insertion::FileInsertionPolicy,
        files: impl FnOnce() -> Files,
        text: impl FnOnce() -> Text,
        focused: bool,
        local: TerminalLocalFileCapabilities,
    ) -> Result<Option<Self>, PasteIntakeError>
    where
        Files: Future<Output = Result<Vec<PathBuf>, ClipboardError>>,
        Text: Future<Output = Option<String>>,
    {
        if !focused {
            return Err(PasteIntakeError::TerminalUnfocused);
        }
        if let Some(access) = super::LocalFileAccess::authorize(local) {
            let paths = access
                .clipboard(files)
                .await
                .map_err(|_| PasteIntakeError::InvalidFiles("clipboard files are unavailable"))?;
            if !paths.is_empty() {
                return access
                    .insertion(policy, &paths)
                    .map(|text| Some(Self { text }))
                    .map_err(PasteIntakeError::InvalidFiles);
            }
        }
        text()
            .await
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
    impl Files {
        fn read_files(&self) -> std::future::Ready<Result<Vec<PathBuf>, ClipboardError>> {
            self.reads.set(self.reads.get() + 1);
            std::future::ready(if self.fail {
                Err(ClipboardError::InvalidFiles)
            } else {
                Ok(self.paths.clone())
            })
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
            std::future::ready(Some("fixture".to_owned()))
        };
        assert!(
            pollster::block_on(PastePayload::clipboard(
                crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
                || files.read_files(),
                text,
                false,
                TerminalLocalFileCapabilities::Enabled
            ))
            .is_err()
        );
        assert_eq!((files.reads.get(), text_reads.get()), (0, 0));
        let remote = pollster::block_on(PastePayload::clipboard(
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
            || files.read_files(),
            text,
            true,
            TerminalLocalFileCapabilities::Disabled,
        ))
        .unwrap()
        .unwrap();
        assert!(remote.text() == "fixture");
        assert_eq!((files.reads.get(), text_reads.get()), (0, 1));
        let local = pollster::block_on(PastePayload::clipboard(
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
            || files.read_files(),
            text,
            true,
            TerminalLocalFileCapabilities::Enabled,
        ))
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
        let result = pollster::block_on(PastePayload::clipboard(
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
            || files.read_files(),
            || -> std::future::Ready<Option<String>> { panic!("unexpected clipboard read") },
            true,
            TerminalLocalFileCapabilities::Enabled,
        ));
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
