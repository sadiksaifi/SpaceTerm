//! The Settings Document section: the whole document as editable JSON, applied only on request.

use gpui::prelude::*;
use gpui::{AnyElement, Entity, Window, div};
use spaceterm_ui::{FieldState, TextArea};

use super::DeveloperWorkbench;
use crate::ui::appearance::settings::SettingsAppearance;
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::{FormGroup, FormRow, FormRowLayout, action_button};

/// How many lines show before the editor scrolls.
const EDITOR_ROWS: usize = 18;
/// The largest text the editor accepts, well above any Settings Document or theme family.
const EDITOR_LIMIT: usize = 1024 * 1024;

pub(super) struct DocumentEditor {
    pub(super) area: Entity<TextArea>,
}

impl DocumentEditor {
    pub(super) fn new(
        initial: String,
        window: &mut Window,
        cx: &mut Context<DeveloperWorkbench>,
    ) -> Self {
        Self {
            area: cx.new(|cx| {
                TextArea::new(
                    "workbench-document",
                    "Settings Document JSON",
                    initial,
                    window,
                    cx,
                )
                .line_numbers(true)
                .rows(EDITOR_ROWS)
                .input_length_limit(Some(EDITOR_LIMIT))
                .debug_selector("workbench-document-text")
            }),
        }
    }

    pub(super) fn text(&self, cx: &gpui::App) -> String {
        self.area.read(cx).value().to_owned()
    }

    pub(super) fn replace(&self, text: String, cx: &mut Context<DeveloperWorkbench>) {
        self.area.update(cx, |area, cx| {
            area.set_value(text, cx);
        });
    }

    pub(super) fn render(
        &self,
        surface: &SettingsAppearance,
        window: &Window,
        cx: &mut Context<DeveloperWorkbench>,
    ) -> Vec<AnyElement> {
        let appearance = &surface.chrome;
        let focus = self.area.read(cx).focus_handle();
        let editor = spaceterm_ui::field_frame(
            "workbench-document-frame",
            &focus,
            FieldState::default(),
            RadiusRole::Control.pixels(),
            cx,
        )
        .w_full()
        .min_w_0()
        .px(appearance.spacing(8.0))
        .py(appearance.spacing(6.0))
        .chrome_text(appearance.typography.style(TextRole::Secondary))
        .font_family(crate::bundled_font::FAMILY)
        .child(self.area.clone());
        let button =
            |selector: &'static str,
             label: &'static str,
             operation: fn(&mut DeveloperWorkbench, &mut Context<DeveloperWorkbench>)| {
                let owner = cx.weak_entity();
                action_button(selector, label, true, move |_, cx| {
                    let _ = owner.update(cx, operation);
                })
            };
        let actions = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(appearance.spacing(6.0))
            .child(button(
                "workbench-document-apply",
                "Preview Document",
                DeveloperWorkbench::preview_document,
            ))
            .child(button(
                "workbench-document-install",
                "Install Theme Family",
                DeveloperWorkbench::install_theme_family,
            ))
            .child(button(
                "workbench-document-show",
                "Show Current",
                DeveloperWorkbench::show_current_document,
            ))
            .child(button(
                "workbench-document-reload",
                "Reload File",
                DeveloperWorkbench::reload_settings,
            ));
        vec![
            FormGroup::new(
                "workbench-group-document".to_owned(),
                "",
                vec![
                    FormRow::new(
                        "workbench-row-document-editor",
                        "Settings Document",
                        div()
                            .flex()
                            .flex_col()
                            .w_full()
                            .gap(appearance.spacing(8.0))
                            .child(editor)
                            .child(actions),
                    )
                    .layout(FormRowLayout::Full)
                    .render(appearance, window, cx)
                    .into_any_element(),
                ],
            )
            .render(surface)
            .into_any_element(),
        ]
    }
}
