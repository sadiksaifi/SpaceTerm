use gpui::prelude::*;
use gpui::{AnyElement, Context};
use spaceterm_ui::{SegmentedControl, SegmentedOption};

use super::{SettingsRowId, SettingsWindow};
use crate::settings::sidebar::{SidebarPreferences, SidebarState};

impl SettingsWindow {
    pub(super) fn render_sidebar_default_state(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.editor.document().sidebar.default_state;
        let owner = cx.weak_entity();
        SegmentedControl::new(
            "settings-sidebar-default-state",
            "Default sidebar state",
            &current,
            vec![
                SegmentedOption::new(SidebarState::Expanded, "Expanded")
                    .debug_selector("settings-sidebar-default-state-expanded"),
                SegmentedOption::new(SidebarState::Collapsed, "Collapsed")
                    .debug_selector("settings-sidebar-default-state-collapsed"),
            ],
        )
        .expect("two sidebar states are within the bounded option set")
        .disabled(!self.editor.editable())
        .debug_selector("settings-sidebar-default-state")
        .on_change(move |change, _, cx| {
            let state = *change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(move |draft| draft.sidebar.default_state = state, cx);
            });
        })
        .into_any_element()
    }

    pub(super) fn sidebar_preference_differs(&self, row: SettingsRowId) -> Option<bool> {
        (row == SettingsRowId::SidebarDefaultState)
            .then(|| self.editor.document().sidebar != SidebarPreferences::default())
    }

    pub(super) fn reset_sidebar_preference(&mut self, cx: &mut Context<Self>) {
        self.edit(|draft| draft.sidebar = SidebarPreferences::default(), cx);
    }
}
