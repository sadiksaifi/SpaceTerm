#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalFocusBlocker {
    Sidebar,
    SidebarResize,
    CommandPalette,
    RenameField,
    ContextMenu,
    PaneMenu,
    PaneResize,
    TabMenu,
    TopChrome,
    TabSelector,
    Modal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalFocusFacts {
    pub(crate) active_workspace: bool,
    pub(crate) active_tab: bool,
    pub(crate) focused_pane: bool,
    pub(crate) responder: bool,
    pub(crate) operating_system_window_key: bool,
    pub(crate) application_active: bool,
    pub(crate) blocker: Option<TerminalFocusBlocker>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalProductFocus {
    pub(crate) active_workspace: bool,
    pub(crate) active_tab: bool,
    pub(crate) pane_visible: bool,
    pub(crate) focused_pane: bool,
    pub(crate) blocker: Option<TerminalFocusBlocker>,
}

impl Default for TerminalProductFocus {
    fn default() -> Self {
        Self {
            active_workspace: true,
            active_tab: true,
            pane_visible: true,
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
            active_tab: true,
            focused_pane: true,
            responder: true,
            operating_system_window_key: true,
            application_active: true,
            blocker: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct TerminalFocusCoordinator {
    native_dialog_open: bool,
}

/// Raw temporary ownership facts. Priority belongs to the coordinator, not the managers.
#[derive(Default)]
pub(crate) struct WorkspaceFocusOwners {
    pub(crate) picker: bool,
    pub(crate) remote_flow: bool,
    pub(crate) switcher: bool,
    pub(crate) window_drag: bool,
    pub(crate) sidebar_resize: bool,
    pub(crate) rename: bool,
    pub(crate) context_menu: bool,
    pub(crate) sidebar: bool,
}

#[derive(Default)]
pub(crate) struct TabFocusOwners {
    pub(crate) parent: Option<TerminalFocusBlocker>,
    pub(crate) window_drag: bool,
    pub(crate) selector: bool,
    pub(crate) menu: bool,
    pub(crate) context_menu: bool,
}

impl TerminalFocusCoordinator {
    pub(crate) fn workspace_blocker(owners: WorkspaceFocusOwners) -> Option<TerminalFocusBlocker> {
        Self::first_owner(&[
            (owners.picker, TerminalFocusBlocker::Modal),
            (
                owners.remote_flow || owners.switcher,
                TerminalFocusBlocker::CommandPalette,
            ),
            (owners.window_drag, TerminalFocusBlocker::TopChrome),
            (owners.sidebar_resize, TerminalFocusBlocker::SidebarResize),
            (owners.rename, TerminalFocusBlocker::RenameField),
            (owners.context_menu, TerminalFocusBlocker::ContextMenu),
            (owners.sidebar, TerminalFocusBlocker::Sidebar),
        ])
    }

    pub(crate) fn tab_blocker(owners: TabFocusOwners) -> Option<TerminalFocusBlocker> {
        owners.parent.or_else(|| {
            Self::first_owner(&[
                (owners.window_drag, TerminalFocusBlocker::TopChrome),
                (owners.selector, TerminalFocusBlocker::TabSelector),
                (owners.menu, TerminalFocusBlocker::TabMenu),
                (owners.context_menu, TerminalFocusBlocker::ContextMenu),
            ])
        })
    }

    pub(crate) fn pane_layout_blocker(
        parent: Option<TerminalFocusBlocker>,
        menu: bool,
        resizing: bool,
    ) -> Option<TerminalFocusBlocker> {
        Self::first_owner(&[
            (menu, TerminalFocusBlocker::PaneMenu),
            (resizing, TerminalFocusBlocker::PaneResize),
        ])
        .or(parent)
    }

    pub(crate) fn modal_blocker(
        parent: Option<TerminalFocusBlocker>,
        modal: bool,
    ) -> Option<TerminalFocusBlocker> {
        modal.then_some(TerminalFocusBlocker::Modal).or(parent)
    }

    pub(crate) fn pane_blocker(
        &self,
        parent: Option<TerminalFocusBlocker>,
        modal: bool,
        context_menu: bool,
    ) -> Option<TerminalFocusBlocker> {
        Self::modal_blocker(
            Self::first_owner(&[(context_menu, TerminalFocusBlocker::ContextMenu)]).or(parent),
            modal || self.native_dialog_open,
        )
    }

    pub(crate) fn set_native_dialog_open(&mut self, open: bool) {
        self.native_dialog_open = open;
    }

    pub(crate) const fn native_dialog_open(&self) -> bool {
        self.native_dialog_open
    }

    fn first_owner(owners: &[(bool, TerminalFocusBlocker)]) -> Option<TerminalFocusBlocker> {
        owners
            .iter()
            .find_map(|(owns, blocker)| owns.then_some(*blocker))
    }

    pub(crate) const fn is_focused(facts: TerminalFocusFacts) -> bool {
        facts.active_workspace
            && facts.active_tab
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
    fn closing_a_workspace_menu_preserves_other_owners_and_does_not_invent_sidebar_focus() {
        let mut owners = WorkspaceFocusOwners {
            switcher: true,
            context_menu: true,
            ..Default::default()
        };
        assert_eq!(
            TerminalFocusCoordinator::workspace_blocker(owners),
            Some(TerminalFocusBlocker::CommandPalette)
        );
        owners = WorkspaceFocusOwners {
            switcher: true,
            context_menu: false,
            ..Default::default()
        };
        assert_eq!(
            TerminalFocusCoordinator::workspace_blocker(owners),
            Some(TerminalFocusBlocker::CommandPalette)
        );
        assert_eq!(
            TerminalFocusCoordinator::workspace_blocker(WorkspaceFocusOwners::default()),
            None
        );
    }

    #[test]
    fn nested_ownership_preserves_blocking_until_every_owner_releases() {
        let workspace = TerminalFocusCoordinator::workspace_blocker(WorkspaceFocusOwners {
            switcher: true,
            sidebar: true,
            ..Default::default()
        });
        let tab = TerminalFocusCoordinator::tab_blocker(TabFocusOwners {
            parent: workspace,
            menu: true,
            ..Default::default()
        });
        assert_eq!(tab, Some(TerminalFocusBlocker::CommandPalette));
        let pane = TerminalFocusCoordinator::pane_layout_blocker(tab, true, true);
        assert_eq!(pane, Some(TerminalFocusBlocker::PaneMenu));
        let mut coordinator = TerminalFocusCoordinator::default();
        coordinator.set_native_dialog_open(true);
        assert_eq!(
            coordinator.pane_blocker(pane, false, false),
            Some(TerminalFocusBlocker::Modal)
        );
        coordinator.set_native_dialog_open(false);
        assert_eq!(coordinator.pane_blocker(pane, false, false), pane);
        assert_eq!(
            TerminalFocusCoordinator::pane_layout_blocker(tab, false, true),
            Some(TerminalFocusBlocker::PaneResize)
        );
        assert_eq!(
            TerminalFocusCoordinator::pane_layout_blocker(tab, false, false),
            tab
        );
        assert_eq!(
            coordinator.pane_blocker(None, true, false),
            Some(TerminalFocusBlocker::Modal)
        );
        assert_eq!(coordinator.pane_blocker(None, false, false), None);
    }

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
                active_tab: false,
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
            TerminalFocusBlocker::SidebarResize,
            TerminalFocusBlocker::CommandPalette,
            TerminalFocusBlocker::RenameField,
            TerminalFocusBlocker::ContextMenu,
            TerminalFocusBlocker::PaneMenu,
            TerminalFocusBlocker::PaneResize,
            TerminalFocusBlocker::TabMenu,
            TerminalFocusBlocker::TopChrome,
            TerminalFocusBlocker::TabSelector,
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
