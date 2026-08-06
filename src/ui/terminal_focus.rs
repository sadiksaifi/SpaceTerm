#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the blocker vocabulary precedes every planned native surface"
    )
)]
pub(crate) enum TerminalFocusBlocker {
    Sidebar,
    RenameField,
    ContextMenu,
    PaneMenu,
    TopChrome,
    WindowSelector,
    Modal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalFocusFacts {
    pub(crate) active_workspace: bool,
    pub(crate) active_window: bool,
    pub(crate) focused_pane: bool,
    pub(crate) responder: bool,
    pub(crate) operating_system_window_key: bool,
    pub(crate) application_active: bool,
    pub(crate) blocker: Option<TerminalFocusBlocker>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalProductFocus {
    pub(crate) active_workspace: bool,
    pub(crate) active_window: bool,
    pub(crate) focused_pane: bool,
    pub(crate) blocker: Option<TerminalFocusBlocker>,
}

impl Default for TerminalProductFocus {
    fn default() -> Self {
        Self {
            active_workspace: true,
            active_window: true,
            focused_pane: true,
            blocker: None,
        }
    }
}

impl TerminalFocusFacts {
    #[cfg(test)]
    const fn focused() -> Self {
        Self {
            active_workspace: true,
            active_window: true,
            focused_pane: true,
            responder: true,
            operating_system_window_key: true,
            application_active: true,
            blocker: None,
        }
    }
}

pub(crate) struct TerminalFocusCoordinator;

impl TerminalFocusCoordinator {
    pub(crate) const fn is_focused(facts: TerminalFocusFacts) -> bool {
        facts.active_workspace
            && facts.active_window
            && facts.focused_pane
            && facts.responder
            && facts.operating_system_window_key
            && facts.application_active
            && facts.blocker.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_input_focus_requires_every_positive_fact_and_no_blocker() {
        let focused = TerminalFocusFacts::focused();
        assert!(TerminalFocusCoordinator::is_focused(focused));

        let cases = [
            TerminalFocusFacts {
                active_workspace: false,
                ..focused
            },
            TerminalFocusFacts {
                active_window: false,
                ..focused
            },
            TerminalFocusFacts {
                focused_pane: false,
                ..focused
            },
            TerminalFocusFacts {
                responder: false,
                ..focused
            },
            TerminalFocusFacts {
                operating_system_window_key: false,
                ..focused
            },
            TerminalFocusFacts {
                application_active: false,
                ..focused
            },
            TerminalFocusFacts {
                blocker: Some(TerminalFocusBlocker::PaneMenu),
                ..focused
            },
        ];

        for facts in cases {
            assert!(!TerminalFocusCoordinator::is_focused(facts));
        }
    }

    #[test]
    fn every_temporary_ui_owner_blocks_terminal_input_focus() {
        let blockers = [
            TerminalFocusBlocker::Sidebar,
            TerminalFocusBlocker::RenameField,
            TerminalFocusBlocker::ContextMenu,
            TerminalFocusBlocker::PaneMenu,
            TerminalFocusBlocker::TopChrome,
            TerminalFocusBlocker::WindowSelector,
            TerminalFocusBlocker::Modal,
        ];

        for blocker in blockers {
            assert!(!TerminalFocusCoordinator::is_focused(TerminalFocusFacts {
                blocker: Some(blocker),
                ..TerminalFocusFacts::focused()
            }));
        }
    }
}
