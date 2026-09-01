use std::path::PathBuf;

use crate::domain::{PaneId, WindowId, WorkspaceId};

use super::file_insertion::prepare_file_insertion;
use super::hyperlink::{HyperlinkKind, HyperlinkTarget};
use super::metadata::TerminalLocalFileCapabilities;
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
    pub(crate) fn from_presence(
        local_file_capabilities: TerminalLocalFileCapabilities,
        selection_present: bool,
        link: Option<&HyperlinkTarget>,
    ) -> Self {
        let link = link.filter(|link| {
            link.kind == HyperlinkKind::Url || local_file_capabilities.are_enabled()
        });
        Self {
            copy: selection_present,
            open_link: link.is_some(),
            quick_look: local_file_capabilities.are_enabled()
                && link.is_some_and(|link| link.kind == HyperlinkKind::LocalPath),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_state(
        local_file_capabilities: TerminalLocalFileCapabilities,
        selection: Option<&SelectionCopy>,
        link: Option<&HyperlinkTarget>,
    ) -> Self {
        Self::from_presence(
            local_file_capabilities,
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
        local_file_capabilities: TerminalLocalFileCapabilities,
    ) -> Result<Self, NativeInsertionError> {
        if !terminal_input_focused {
            return Err(NativeInsertionError::TerminalUnfocused);
        }
        Self::prepare_dropped_files(paths, local_file_capabilities)
            .map_err(NativeInsertionError::InvalidFiles)
    }

    pub(crate) fn prepare_dropped_files(
        paths: &[PathBuf],
        local_file_capabilities: TerminalLocalFileCapabilities,
    ) -> Result<Self, &'static str> {
        if !local_file_capabilities.are_enabled() {
            return Err("local file insertion is disabled for this Terminal Session");
        }
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
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the next stacked context-action layer owns Quick Look presentation"
    )
)]
pub(crate) struct QuickLookTarget {
    link: HyperlinkTarget,
    local_file_capabilities: TerminalLocalFileCapabilities,
}

impl QuickLookTarget {
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the next stacked context-action layer resolves the current hyperlink"
        )
    )]
    pub(crate) fn from_link(
        link: &HyperlinkTarget,
        local_file_capabilities: TerminalLocalFileCapabilities,
    ) -> Option<Self> {
        link.revalidated_local_path(local_file_capabilities)?;
        Some(Self {
            link: link.clone(),
            local_file_capabilities,
        })
    }

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the next stacked context-action layer revalidates before native presentation"
        )
    )]
    pub(crate) fn revalidated_path(&self) -> Option<PathBuf> {
        self.link
            .revalidated_local_path(self.local_file_capabilities)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    use super::*;
    use crate::terminal::{HyperlinkTarget, SelectionCopy};

    const LOCAL_FILES: TerminalLocalFileCapabilities = TerminalLocalFileCapabilities::Enabled;
    const REMOTE_FILES: TerminalLocalFileCapabilities = TerminalLocalFileCapabilities::Disabled;

    #[test]
    fn context_actions_follow_selection_and_validated_link_state() {
        let selection = SelectionCopy {
            plain_text: "selected".to_owned(),
            html: None,
        };
        let url = HyperlinkTarget::url("https://example.test").unwrap();

        assert_eq!(
            NativeContextActions::from_state(LOCAL_FILES, Some(&selection), Some(&url)),
            NativeContextActions {
                copy: true,
                open_link: true,
                quick_look: false,
            }
        );
        assert_eq!(
            NativeContextActions::from_state(LOCAL_FILES, None, None),
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
            NativeInsertion::dropped_files(&[PathBuf::from("/tmp/a b")], true, LOCAL_FILES)
                .unwrap()
                .text(),
            "'/tmp/a b'"
        );
        assert_eq!(
            NativeInsertion::service_text("ignored", false),
            Err(NativeInsertionError::TerminalUnfocused)
        );
        assert!(
            NativeInsertion::dropped_files(&[PathBuf::from("relative")], true, LOCAL_FILES)
                .is_err()
        );
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
            NativeContextActions::from_presence(LOCAL_FILES, true, None),
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

        let local = HyperlinkTarget::osc8(
            &format!("file://{}", file.to_str().unwrap()),
            &directory,
            None,
            LOCAL_FILES,
        )
        .unwrap();
        let url = HyperlinkTarget::url("https://example.test").unwrap();
        assert_eq!(
            QuickLookTarget::from_link(&local, LOCAL_FILES)
                .and_then(|target| target.revalidated_path()),
            Some(file.canonicalize().unwrap())
        );
        assert!(QuickLookTarget::from_link(&url, LOCAL_FILES).is_none());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_native_actions_are_inert_after_the_file_is_removed() {
        let directory =
            std::env::temp_dir().join(format!("spaceterm-local-removed-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();
        let local =
            HyperlinkTarget::osc8("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();

        fs::remove_file(file).unwrap();

        assert_eq!(local.activation_url(LOCAL_FILES), None);
        assert_eq!(QuickLookTarget::from_link(&local, LOCAL_FILES), None);
        assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).open_link);
        assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).quick_look);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_native_actions_are_inert_after_the_file_is_replaced() {
        let directory =
            std::env::temp_dir().join(format!("spaceterm-local-replaced-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        let replacement = directory.join("replacement.txt");
        fs::write(&file, b"first").unwrap();
        let local =
            HyperlinkTarget::osc8("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();

        fs::write(&replacement, b"replacement").unwrap();
        fs::rename(&replacement, &file).unwrap();

        assert_eq!(local.activation_url(LOCAL_FILES), None);
        assert_eq!(QuickLookTarget::from_link(&local, LOCAL_FILES), None);
        assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).open_link);
        assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).quick_look);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_native_actions_are_inert_after_the_path_becomes_a_different_symlink() {
        let directory =
            std::env::temp_dir().join(format!("spaceterm-local-symlink-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        let other = directory.join("other.txt");
        fs::write(&file, b"first").unwrap();
        fs::write(&other, b"other").unwrap();
        let local =
            HyperlinkTarget::osc8("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();

        fs::remove_file(&file).unwrap();
        symlink(&other, &file).unwrap();

        assert_eq!(local.activation_url(LOCAL_FILES), None);
        assert_eq!(QuickLookTarget::from_link(&local, LOCAL_FILES), None);
        assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).open_link);
        assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).quick_look);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn remote_capabilities_disable_every_file_action_but_preserve_text_and_web_actions() {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-remote-capability-boundary-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();
        let local =
            HyperlinkTarget::osc8("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();
        let web = HyperlinkTarget::url("https://example.test").unwrap();

        assert_eq!(local.activation_url(REMOTE_FILES), None);
        assert_eq!(QuickLookTarget::from_link(&local, REMOTE_FILES), None);
        assert_eq!(
            NativeContextActions::from_presence(REMOTE_FILES, true, Some(&local)),
            NativeContextActions {
                copy: true,
                open_link: false,
                quick_look: false,
            }
        );
        assert!(NativeContextActions::from_presence(REMOTE_FILES, false, Some(&web)).open_link);
        assert!(NativeInsertion::service_text("ordinary text", true).is_ok());
        assert!(NativeInsertion::dropped_files(&[file], true, REMOTE_FILES).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
