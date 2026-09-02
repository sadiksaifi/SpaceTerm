use crate::terminal::metadata::{CommandState, MetadataFreshness, PromptZone};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseScope {
    Pane,
    Tab,
    Workspace,
    Window,
    Application,
}

impl CloseScope {
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Pane => "Close Pane?",
            Self::Tab => "Close Tab?",
            Self::Workspace => "Close Workspace?",
            Self::Window => "Close Window?",
            Self::Application => "Quit SpaceTerm?",
        }
    }

    pub(crate) const fn destructive_label(self) -> &'static str {
        match self {
            Self::Pane => "Close Pane",
            Self::Tab => "Close Tab",
            Self::Workspace => "Close Workspace",
            Self::Window => "Close Window",
            Self::Application => "Quit SpaceTerm",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaneLifecycle {
    NoLiveTerminalSession,
    Live,
    Exited,
    FatallyFailed,
    DisconnectedRemote,
}

pub(crate) fn pane_requires_close_confirmation(
    lifecycle: PaneLifecycle,
    freshness: MetadataFreshness,
    prompt_zone: PromptZone,
    command: Option<&CommandState>,
) -> bool {
    if !matches!(lifecycle, PaneLifecycle::Live) {
        return false;
    }
    if freshness == MetadataFreshness::Stale {
        return true;
    }
    if matches!(command, Some(CommandState::Finished { .. })) {
        return false;
    }
    match prompt_zone {
        PromptZone::Prompt | PromptZone::CommandInput => false,
        PromptZone::Unknown | PromptZone::CommandOutput => true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn running_command_should_require_confirmation() {
        assert!(pane_requires_close_confirmation(
            PaneLifecycle::Live,
            MetadataFreshness::Live,
            PromptZone::CommandOutput,
            Some(&CommandState::Running),
        ));
    }

    #[test]
    fn prompt_should_be_safe_to_close() {
        assert!(!pane_requires_close_confirmation(
            PaneLifecycle::Live,
            MetadataFreshness::Live,
            PromptZone::Prompt,
            None,
        ));
    }

    #[test]
    fn command_input_should_be_safe_to_close() {
        assert!(!pane_requires_close_confirmation(
            PaneLifecycle::Live,
            MetadataFreshness::Live,
            PromptZone::CommandInput,
            None,
        ));
    }

    #[test]
    fn finished_command_should_be_safe_to_close() {
        assert!(!pane_requires_close_confirmation(
            PaneLifecycle::Live,
            MetadataFreshness::Live,
            PromptZone::CommandOutput,
            Some(&CommandState::Finished {
                exit_status: Some(0),
                duration: Duration::from_secs(1),
            }),
        ));
    }

    #[test]
    fn command_output_without_completion_should_require_confirmation() {
        assert!(pane_requires_close_confirmation(
            PaneLifecycle::Live,
            MetadataFreshness::Live,
            PromptZone::CommandOutput,
            None,
        ));
    }

    #[test]
    fn unknown_startup_state_should_require_confirmation() {
        assert!(pane_requires_close_confirmation(
            PaneLifecycle::Live,
            MetadataFreshness::Live,
            PromptZone::Unknown,
            None,
        ));
    }

    #[test]
    fn stale_metadata_should_require_confirmation() {
        assert!(pane_requires_close_confirmation(
            PaneLifecycle::Live,
            MetadataFreshness::Stale,
            PromptZone::Prompt,
            None,
        ));
    }

    #[test]
    fn terminal_lifecycle_without_live_work_should_be_safe_to_close() {
        for lifecycle in [
            PaneLifecycle::NoLiveTerminalSession,
            PaneLifecycle::Exited,
            PaneLifecycle::FatallyFailed,
            PaneLifecycle::DisconnectedRemote,
        ] {
            assert!(!pane_requires_close_confirmation(
                lifecycle,
                MetadataFreshness::Stale,
                PromptZone::Unknown,
                Some(&CommandState::Running),
            ));
        }
    }

    #[test]
    fn close_scope_copy_should_match_the_requested_hierarchy_level() {
        let copy = [
            (CloseScope::Pane, "Close Pane?", "Close Pane"),
            (CloseScope::Tab, "Close Tab?", "Close Tab"),
            (CloseScope::Workspace, "Close Workspace?", "Close Workspace"),
            (CloseScope::Window, "Close Window?", "Close Window"),
            (CloseScope::Application, "Quit SpaceTerm?", "Quit SpaceTerm"),
        ];

        assert!(copy.into_iter().all(|(scope, title, label)| {
            scope.title() == title && scope.destructive_label() == label
        }));
    }
}
