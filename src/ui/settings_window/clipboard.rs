use gpui::prelude::*;
use gpui::{AnyElement, Context};
use spaceterm_ui::{Switch, ToggleSize};

use super::{SettingsRowId, SettingsWindow};
use crate::terminal::native_services::clipboard::ClipboardPreferences;

impl SettingsWindow {
    pub(super) fn render_clipboard_preference(
        &mut self,
        row: SettingsRowId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.editor.document().clipboard;
        let value = if row == SettingsRowId::ClipboardReads {
            current.allow_read
        } else {
            current.allow_write
        };
        let selector = if row == SettingsRowId::ClipboardReads {
            "settings-clipboard-reads"
        } else {
            "settings-clipboard-writes"
        };
        let owner = cx.weak_entity();
        Switch::new(
            selector,
            row.descriptor().label(self.computer_use_access.naming()),
            value,
        )
        .size(ToggleSize::Regular)
        .label_hidden(true)
        .disabled(!self.editor.editable())
        .debug_selector(selector)
        .on_change(move |change, _, cx| {
            let value = change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(move |draft| set(row, &mut draft.clipboard, value), cx);
            });
        })
        .into_any_element()
    }

    pub(super) fn clipboard_preference_differs(&self, row: SettingsRowId) -> Option<bool> {
        let current = self.editor.document().clipboard;
        let defaults = ClipboardPreferences::default();
        match row {
            SettingsRowId::ClipboardReads => Some(current.allow_read != defaults.allow_read),
            SettingsRowId::ClipboardWrites => Some(current.allow_write != defaults.allow_write),
            _ => None,
        }
    }

    pub(super) fn reset_clipboard_preference(
        &mut self,
        row: SettingsRowId,
        cx: &mut Context<Self>,
    ) {
        let defaults = ClipboardPreferences::default();
        let value = if row == SettingsRowId::ClipboardReads {
            defaults.allow_read
        } else {
            defaults.allow_write
        };
        self.edit(move |draft| set(row, &mut draft.clipboard, value), cx);
    }
}

fn set(row: SettingsRowId, preferences: &mut ClipboardPreferences, value: bool) {
    match row {
        SettingsRowId::ClipboardReads => preferences.allow_read = value,
        SettingsRowId::ClipboardWrites => preferences.allow_write = value,
        _ => {}
    }
}
