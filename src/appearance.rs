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
            text_accent, link_text, link_text_hover,
            icon, icon_muted, icon_disabled, icon_accent,
            border, border_variant, border_focused, border_selected, border_disabled,
            border_transparent,
            element_background, element_hover, element_active, element_selected,
            element_selected_hover, element_disabled, element_foreground,
            element_hover_foreground, element_active_foreground,
            element_selected_foreground, element_selected_hover_foreground,
            element_disabled_foreground,
            ghost_element_background, ghost_element_hover, ghost_element_active,
            ghost_element_selected, ghost_element_disabled,
            ghost_element_foreground, ghost_element_hover_foreground,
            ghost_element_active_foreground, ghost_element_selected_foreground,
            ghost_element_disabled_foreground,
            navigation_selection, sidebar_focus,
            info, info_background, success,
            warning, warning_background, warning_border, error, error_background, error_border,
            input_text, input_placeholder, input_disabled_text, input_caret,
            input_selection_background, input_background, input_disabled_background,
            input_border, input_focused_border, input_invalid_border,
            modal_scrim, modal_checkbox, modal_checkbox_selected,
            modal_checkbox_focused, modal_checkbox_disabled,
            scrollbar_track, scrollbar_track_border, scrollbar_thumb_background,
            scrollbar_thumb_border, scrollbar_thumb_hover_background,
            resize_idle, resize_focused, resize_hovered, resize_dragged, resize_disabled,
            shadow
        }
    };
}

mod builtin;
#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod consumer_ledger;
mod document;
mod preferences;
mod resolution;
mod scheme;
#[cfg(test)]
mod tests;

pub(crate) use crate::theme::Color;
pub(crate) use document::{
    AppearanceDocument, AppearanceDocumentError, ImportCandidate, ImportError, ZedImportKind,
    export_schemes, export_settings, import_zed, list_zed_candidates, parse_color_document,
    parse_settings,
};
pub(crate) use preferences::{
    AppearancePreferences, ChromeDensity, ChromeFontFamily, ResetTarget, SchemeSelection,
    TerminalFontFamily,
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

pub(crate) use builtin::fallback_id as builtin_fallback_scheme;
pub(crate) use resolution::AppearanceDiagnostic;

#[cfg(test)]
pub(crate) use resolution::ResolutionError;
#[cfg(test)]
pub(crate) use scheme::{
    ChromeColorOverrides, ChromeScheme, OptionalColorOverride, SchemeMetadata,
    TerminalColorOverrides, TerminalScheme,
};
