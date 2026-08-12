use std::path::{Path, PathBuf};

use crate::domain::{PaneId, WindowId, WorkspaceId};

use super::file_insertion::prepare_file_insertion;
use super::hyperlink::{HyperlinkKind, HyperlinkTarget};
#[cfg(test)]
use super::selection::SelectionCopy;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeContextActions {
    pub(crate) copy: bool,
    pub(crate) open_link: bool,
    pub(crate) quick_look: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeServiceCapabilities {
    pub(crate) send_text: bool,
    pub(crate) return_text: bool,
}

impl NativeServiceCapabilities {
    pub(crate) const fn new(send_text: bool, return_text: bool) -> Self {
        Self {
            send_text,
            return_text,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeServiceOrigin {
    workspace_id: WorkspaceId,
    window_id: WindowId,
    pane_id: PaneId,
    session_identity: u64,
    focus_epoch: u64,
    hierarchy_generation: u64,
}

impl NativeServiceOrigin {
    pub(crate) const fn new(
        workspace_id: WorkspaceId,
        window_id: WindowId,
        pane_id: PaneId,
        session_identity: u64,
        focus_epoch: u64,
        hierarchy_generation: u64,
    ) -> Self {
        Self {
            workspace_id,
            window_id,
            pane_id,
            session_identity,
            focus_epoch,
            hierarchy_generation,
        }
    }

    pub(crate) const fn workspace_id(self) -> WorkspaceId {
        self.workspace_id
    }

    pub(crate) const fn window_id(self) -> WindowId {
        self.window_id
    }

    pub(crate) const fn pane_id(self) -> PaneId {
        self.pane_id
    }

    pub(crate) const fn session_identity(self) -> u64 {
        self.session_identity
    }

    pub(crate) const fn focus_epoch(self) -> u64 {
        self.focus_epoch
    }

    pub(crate) const fn hierarchy_generation(self) -> u64 {
        self.hierarchy_generation
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeServiceStatus {
    pub(crate) capabilities: NativeServiceCapabilities,
    pub(crate) origin: Option<NativeServiceOrigin>,
}

impl NativeServiceStatus {
    pub(crate) const fn new(
        capabilities: NativeServiceCapabilities,
        origin: Option<NativeServiceOrigin>,
    ) -> Self {
        Self {
            capabilities,
            origin,
        }
    }
}

impl NativeContextActions {
    pub(crate) fn from_presence(selection_present: bool, link: Option<&HyperlinkTarget>) -> Self {
        Self {
            copy: selection_present,
            open_link: link.is_some(),
            quick_look: link.and_then(QuickLookTarget::from_link).is_some(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_state(
        selection: Option<&SelectionCopy>,
        link: Option<&HyperlinkTarget>,
    ) -> Self {
        Self::from_presence(
            selection.is_some_and(|selection| !selection.plain_text.is_empty()),
            link,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeInsertion {
    text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeInsertionError {
    TerminalUnfocused,
    InvalidFiles(&'static str),
}

impl NativeInsertion {
    pub(crate) fn service_text(
        text: impl Into<String>,
        terminal_input_focused: bool,
    ) -> Result<Self, NativeInsertionError> {
        if !terminal_input_focused {
            return Err(NativeInsertionError::TerminalUnfocused);
        }
        Ok(Self { text: text.into() })
    }

    pub(crate) fn dropped_files(
        paths: &[PathBuf],
        terminal_input_focused: bool,
    ) -> Result<Self, NativeInsertionError> {
        if !terminal_input_focused {
            return Err(NativeInsertionError::TerminalUnfocused);
        }
        Self::prepare_dropped_files(paths).map_err(NativeInsertionError::InvalidFiles)
    }

    pub(crate) fn prepare_dropped_files(paths: &[PathBuf]) -> Result<Self, &'static str> {
        prepare_file_insertion(paths).map(|insertion| Self {
            text: insertion.text,
        })
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn into_text(self) -> String {
        self.text
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QuickLookTarget {
    path: PathBuf,
}

impl QuickLookTarget {
    pub(crate) fn from_link(link: &HyperlinkTarget) -> Option<Self> {
        if link.kind != HyperlinkKind::LocalPath {
            return None;
        }
        let path = Path::new(&link.value).canonicalize().ok()?;
        path.is_file().then_some(Self { path })
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::terminal::{HyperlinkTarget, SelectionCopy};

    #[test]
    fn context_actions_follow_selection_and_validated_link_state() {
        let selection = SelectionCopy {
            plain_text: "selected".to_owned(),
            html: None,
        };
        let url = HyperlinkTarget::url("https://example.test").unwrap();

        assert_eq!(
            NativeContextActions::from_state(Some(&selection), Some(&url)),
            NativeContextActions {
                copy: true,
                open_link: true,
                quick_look: false,
            }
        );
        assert_eq!(
            NativeContextActions::from_state(None, None),
            NativeContextActions::default()
        );
    }

    #[test]
    fn service_text_and_file_drops_produce_only_sanitized_paste_candidates() {
        assert_eq!(
            NativeInsertion::service_text("printf 'ok'\n", true)
                .unwrap()
                .text(),
            "printf 'ok'\n"
        );
        assert_eq!(
            NativeInsertion::dropped_files(&[PathBuf::from("/tmp/a b")], true)
                .unwrap()
                .text(),
            "'/tmp/a b'"
        );
        assert_eq!(
            NativeInsertion::service_text("ignored", false),
            Err(NativeInsertionError::TerminalUnfocused)
        );
        assert!(NativeInsertion::dropped_files(&[PathBuf::from("relative")], true).is_err());
    }

    #[test]
    fn service_capabilities_keep_selection_export_distinct_from_terminal_input_focus() {
        assert_eq!(
            NativeServiceCapabilities::new(true, false),
            NativeServiceCapabilities {
                send_text: true,
                return_text: false,
            }
        );
    }

    #[test]
    fn context_enablement_can_be_derived_without_copying_selection_text() {
        assert_eq!(
            NativeContextActions::from_presence(true, None),
            NativeContextActions {
                copy: true,
                open_link: false,
                quick_look: false,
            }
        );
    }

    #[test]
    fn quick_look_accepts_only_existing_validated_local_files() {
        let directory =
            std::env::temp_dir().join(format!("spaceterm-quick-look-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();

        let local = HyperlinkTarget::local(file.to_str().unwrap(), &directory).unwrap();
        let url = HyperlinkTarget::url("https://example.test").unwrap();
        assert_eq!(
            QuickLookTarget::from_link(&local).map(|target| target.path().to_path_buf()),
            Some(file.canonicalize().unwrap())
        );
        assert!(QuickLookTarget::from_link(&url).is_none());

        fs::remove_dir_all(directory).unwrap();
    }
}
