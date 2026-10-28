//! The chrome's resolved style for the shared empty state.
//!
//! Every empty, unavailable, and no-results state outside a floating list presents through
//! [`spaceterm_ui::EmptyState`] with this style, so they share one type, icon paint, and width.

use spaceterm_ui::{EmptyState, EmptyStateStyle, EmptyStateText};

use super::appearance::{ChromeAppearance, gpui_color};
use super::chrome_typography::TextRole;

/// The section title, secondary detail, and muted icon of `appearance`.
pub(crate) fn style(appearance: &ChromeAppearance) -> EmptyStateStyle {
    let text = |role: TextRole, color| {
        let style = appearance.typography.style(role);
        EmptyStateText::new(
            style.font.clone(),
            style.size,
            style.line_height,
            gpui_color(color),
        )
    };
    EmptyStateStyle::new(
        text(TextRole::Section, appearance.colors.text),
        text(TextRole::Secondary, appearance.colors.text_secondary),
        appearance.typography.style(TextRole::Body).line_height,
        gpui_color(appearance.colors.icon_muted),
        f32::from(appearance.spacing(1.0)),
    )
    .intent_colors(
        gpui_color(appearance.colors.warning),
        gpui_color(appearance.colors.error),
    )
}

/// An empty state titled `title` in the current chrome.
pub(crate) fn empty_state(title: impl Into<gpui::SharedString>, cx: &gpui::App) -> EmptyState {
    EmptyState::new(style(super::appearance::chrome(cx)), title)
}
