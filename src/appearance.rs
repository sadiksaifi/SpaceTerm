//! Portable appearance policy, color-scheme interchange, and pure resolution.
//!
//! This Module deliberately contains no GPUI, terminal-engine, filesystem, or
//! native platform types. Callers supply catalogs and availability facts, then
//! receive immutable requested-versus-effective rendering specifications.

macro_rules! chrome_color_fields {
    ($macro:ident) => {
        $macro! {
            background, panel_background, elevated_surface_background,
            title_bar_background, title_bar_inactive_background,
            tab_active_background, tab_inactive_background,
            text, text_secondary, text_muted, text_placeholder, text_disabled,
            text_accent, link_text, link_text_hover, link_text_pressed, link_text_disabled,
            icon, icon_muted, icon_disabled, border, border_variant, border_focused, border_selected, border_disabled,
            border_transparent,
            element_background, element_hover, element_active, element_selected,
            element_disabled, element_foreground,
            element_hover_foreground, element_active_foreground,
            element_disabled_foreground,
            ghost_element_background, ghost_element_hover, ghost_element_active,
            ghost_element_selected, ghost_element_disabled,
            ghost_element_foreground, ghost_element_hover_foreground,
            ghost_element_active_foreground, ghost_element_selected_foreground,
            ghost_element_disabled_foreground,
            sidebar_focus,
            info, info_background, success,
            warning, warning_background, warning_border, error, error_background, error_border,
            input_text, input_placeholder, input_disabled_text, input_caret,
            input_selection_background, input_selection_foreground, input_background, input_disabled_background,
            input_border, input_focused_border, input_invalid_border,
            modal_scrim,
            scrollbar_track, scrollbar_track_border, scrollbar_thumb_background,
            scrollbar_thumb_border, scrollbar_thumb_hover_background,
            resize_idle, resize_focused, resize_hovered, resize_dragged, resize_disabled,
            shadow,
            primary_background,
            primary_foreground,
            primary_icon,
            primary_border,
            primary_hover_background,
            primary_hover_foreground,
            primary_hover_icon,
            primary_hover_border,
            primary_pressed_background,
            primary_pressed_foreground,
            primary_pressed_icon,
            primary_pressed_border,
            primary_disabled_background,
            primary_disabled_foreground,
            primary_disabled_icon,
            primary_disabled_border,
            destructive_background,
            destructive_foreground,
            destructive_icon,
            destructive_border,
            destructive_hover_background,
            destructive_hover_foreground,
            destructive_hover_icon,
            destructive_hover_border,
            destructive_pressed_background,
            destructive_pressed_foreground,
            destructive_pressed_icon,
            destructive_pressed_border,
            destructive_disabled_background,
            destructive_disabled_foreground,
            destructive_disabled_icon,
            destructive_disabled_border,
            selection_background,
            selection_foreground,
            selection_icon,
            selection_border,
            selection_hover_background,
            selection_hover_foreground,
            selection_hover_icon,
            selection_hover_border,
            selection_pressed_background,
            selection_pressed_foreground,
            selection_pressed_icon,
            selection_pressed_border,
            selection_disabled_background,
            selection_disabled_foreground,
            selection_disabled_icon,
            selection_disabled_border,
            toggle_off_background,
            toggle_off_mark,
            toggle_off_border,
            toggle_off_label,
            toggle_off_hover_background,
            toggle_off_hover_mark,
            toggle_off_hover_border,
            toggle_off_hover_label,
            toggle_off_pressed_background,
            toggle_off_pressed_mark,
            toggle_off_pressed_border,
            toggle_off_pressed_label,
            toggle_off_disabled_background,
            toggle_off_disabled_mark,
            toggle_off_disabled_border,
            toggle_off_disabled_label,
            toggle_on_background,
            toggle_on_mark,
            toggle_on_border,
            toggle_on_label,
            toggle_on_hover_background,
            toggle_on_hover_mark,
            toggle_on_hover_border,
            toggle_on_hover_label,
            toggle_on_pressed_background,
            toggle_on_pressed_mark,
            toggle_on_pressed_border,
            toggle_on_pressed_label,
            toggle_on_disabled_background,
            toggle_on_disabled_mark,
            toggle_on_disabled_border,
            toggle_on_disabled_label,
            row_background,
            row_foreground,
            row_secondary,
            row_icon,
            row_match,
            row_border,
            row_hover_background,
            row_hover_foreground,
            row_hover_secondary,
            row_hover_icon,
            row_hover_match,
            row_hover_border,
            row_selected_background,
            row_selected_foreground,
            row_selected_secondary,
            row_selected_icon,
            row_selected_match,
            row_selected_border,
            row_selected_hover_background,
            row_selected_hover_foreground,
            row_selected_hover_secondary,
            row_selected_hover_icon,
            row_selected_hover_match,
            row_selected_hover_border,
            element_icon,
            element_hover_icon,
            element_active_icon,
            element_disabled_icon,
            ghost_element_icon,
            ghost_element_hover_icon,
            ghost_element_active_icon,
            ghost_element_disabled_icon,
            input_disabled_border,
            badge_background,
            badge_foreground,
            preview_background,
            preview_foreground,
            tab_active_foreground,
            tab_inactive_foreground,
            tab_inactive_selected_background,
            tab_inactive_selected_foreground,
            scrollbar_thumb_active_background,
            success_background,
            success_border,
            info_border,
            element_border,
            element_hover_border,
            element_active_border,
            element_disabled_border,
            ghost_element_border,
            ghost_element_hover_border,
            ghost_element_active_border,
            ghost_element_disabled_border,
            outline_border, outline_hover_border, outline_pressed_border, outline_disabled_border,
            tab_hover_background, tab_hover_foreground, tab_hover_icon, tab_active_icon, tab_inactive_icon, tab_inactive_selected_icon,
            tab_active_border, tab_active_hover_background, tab_active_hover_foreground, tab_active_hover_icon,
            tab_inactive_selected_border, tab_separator
        }
    };
}

mod builtin;
#[cfg(test)]
mod catalog_tests;
mod compiler;
mod composition;
mod document;
#[cfg(test)]
mod interchange_tests;
mod preferences;
mod resolution;
#[cfg(test)]
mod schema_tests;
mod scheme;
#[cfg(test)]
mod tests;

pub(crate) use crate::theme::Color;
pub(crate) use compiler::{CaptionPaint, SemanticPaint};
pub(crate) use composition::{
    ResolvedWindowComposition, SurfaceMaterials, SurfaceRole, WindowBackgroundAppearance,
};
pub(crate) use document::{
    ImportCandidate, ImportError, SettingsDocument, SettingsDocumentError, ZedImportKind,
    export_resolved_schemes, export_schemes, export_settings, import_zed, list_zed_candidates,
    parse_color_document, parse_settings,
};
pub(crate) use preferences::{
    AppearanceMode, AppearancePreferences, ChromeDensity, ChromeFontFamily, ResetTarget,
    SchemeSlots, TerminalFontFamily,
};
pub(crate) use resolution::{
    AppearanceChangeSet, AppearanceGeneration, AvailableFont, AvailableFonts, FontClass, FontStyle,
    ResolvedAppearance, ResolvedChromeAppearance, ResolvedFontDescriptor,
    ResolvedTerminalAppearance, ResolvedTerminalTypography, SystemAppearance,
};
pub(crate) use scheme::{
    Appearance, CatalogError, ChromeColors, CustomScheme, SchemeCatalog, SchemeId, SchemeKind,
    SchemeSummary, TerminalColors,
};

/// The built-in chrome palettes, so a control theme can be asserted against what ships.
#[cfg(test)]
pub(crate) use builtin::chrome_base as builtin_chrome_base;
#[cfg(test)]
pub(crate) use builtin::fallback_id as builtin_fallback_scheme;
pub(crate) use resolution::AppearanceDiagnostic;

#[cfg(test)]
pub(crate) use resolution::ResolutionError;
#[cfg(test)]
pub(crate) use scheme::{
    ChromeColorOverrides, ChromeScheme, OptionalColorOverride, SchemeMetadata,
    TerminalColorOverrides, TerminalScheme,
};

#[cfg(test)]
pub(crate) use document::export_effective_schemes;
