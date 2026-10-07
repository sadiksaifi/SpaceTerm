//! Dependency completion for the two built-in Chrome palettes.
use super::{Appearance, ChromeColors, Color};

pub(crate) fn compile_builtin_chrome(appearance: Appearance) -> ChromeColors {
    let authored = super::builtin::chrome_definition(appearance);
    let background = authored.background;
    // Foundation native windows have an opaque root backing of the root's own RGB.
    // Derivation and opaque_presentation use identical backing rules.
    let root_surface = background.with_alpha(255);
    let contrast = |foreground: Color, surface: Color, minimum| {
        contrast(foreground, surface.source_over(root_surface), minimum)
    };
    let text = authored.text;
    let panel_background = authored.panel_background;
    let elevated_surface_background = authored.elevated_surface_background;
    let title_bar_background = authored.title_bar_background;
    let title_bar_inactive_background = authored.title_bar_inactive_background;
    let text_accent = authored.text_accent;
    let text_secondary = authored.text_secondary;
    let text_muted = authored.text_muted;
    let text_placeholder = authored.text_placeholder;
    let text_disabled = authored.text_disabled;
    let link_text = contrast(text_accent, background, 4.5);
    let link_text_hover = authored.link_text_hover;
    let icon = text;
    let icon_muted = text_muted;
    let icon_disabled = text_disabled;
    let border = authored.border;
    let border_variant = authored.border_variant;
    let border_focused = contrast(text_accent, background, 3.0);
    let focus_ring = authored.focus_ring;
    // Selection remains independent of the keyboard-focus color.
    let border_selected = border.mix(text, 0.35);
    let border_disabled = authored.border_disabled;
    let border_transparent = Color::rgba(0);
    let element_background = authored.element_background;
    let segmented_track_background = authored
        .segmented_track_background
        .unwrap_or(element_background);
    let element_hover = authored.element_hover;
    let element_active = authored.element_active;
    // Persistent selection follows the definition's foreground/background contrast.
    let element_selected = authored.element_selected;
    let element_disabled = background;
    let element_foreground = contrast(text, element_background, 4.5);
    let element_hover_foreground = contrast(text, element_hover, 4.5);
    let element_active_foreground = contrast(text, element_active, 4.5);
    let element_disabled_foreground = text_disabled;
    let ghost_element_background = Color::rgba(0);
    let ghost_element_hover = authored.ghost_element_hover;
    let ghost_element_active = authored.ghost_element_active;
    let ghost_element_selected = element_selected;
    let ghost_element_disabled = Color::rgba(0);
    let ghost_element_foreground = contrast(text, ghost_element_background, 4.5);
    let ghost_element_hover_foreground = contrast(text, ghost_element_hover, 4.5);
    let ghost_element_active_foreground = contrast(text, ghost_element_active, 4.5);
    let ghost_element_selected_foreground = contrast(text, ghost_element_selected, 4.5);
    let ghost_element_disabled_foreground = text_disabled;
    let sidebar_focus = border_focused;
    let info = authored.info;
    let success = authored.success;
    let warning = authored.warning;
    let error = authored.error;
    let info_background = info.multiply_opacity(0x1a);
    let info_border = info;
    let success_background = success.multiply_opacity(0x1a);
    let success_border = success;
    let warning_background = warning.multiply_opacity(0x1a);
    let warning_border = warning;
    let error_background = error.multiply_opacity(0x1a);
    let error_border = error;
    // Repository marks reuse the status hues so an in-progress operation reads as a warning and
    // an open Pull Request as success; a draft is deliberately quiet.
    let repository_operation = warning;
    let repository_changes = info;
    let pull_request_open = success;
    let pull_request_draft = text_muted;
    let input_background = authored.input_background;
    let input_surface = input_background.source_over(panel_background.source_over(root_surface));
    let input_text = contrast(text, input_surface, 4.5);
    let input_placeholder = contrast(text_placeholder, input_surface, 4.5);
    let input_disabled_background = input_background;
    let input_disabled_text = text_disabled;
    let input_caret = input_text;
    let input_selection_background = text_accent.multiply_opacity(0x66);
    let input_selection_foreground = contrast(
        input_text,
        input_selection_background.source_over(input_surface),
        4.5,
    );
    let input_border = authored.input_border;
    let input_invalid_border = contrast(error, input_surface, 3.0);
    let input_disabled_border = border_disabled;
    let modal_scrim = background.multiply_opacity(0x99);
    let scrollbar_track = Color::rgba(0);
    let scrollbar_track_border = Color::rgba(0);
    let scrollbar_thumb_background = authored.scrollbar_thumb_background;
    let scrollbar_thumb_border = Color::rgba(0);
    let scrollbar_thumb_hover_background = contrast(
        scrollbar_thumb_background.mix(text, 0.15),
        panel_background,
        3.0,
    );
    let scrollbar_thumb_active_background = contrast(
        scrollbar_thumb_hover_background.mix(text, 0.2),
        panel_background,
        3.0,
    );
    let resize_idle = border;
    let resize_focused = border_focused;
    // Pointer hover strengthens the divider independently of its keyboard-focus indicator.
    let resize_hovered = resize_idle.mix(text, 0.35);
    let resize_dragged = border_selected;
    let resize_disabled = border_disabled;
    let shadow = authored.shadow;
    let primary_background = authored.primary_background;
    let primary_foreground = authored.primary_foreground;
    let primary_icon = primary_foreground;
    let primary_border = primary_background;
    let primary_hover_background = authored.primary_hover_background;
    let primary_hover_foreground = authored.primary_hover_foreground;
    let primary_hover_icon = primary_hover_foreground;
    let primary_hover_border = primary_hover_background;
    let primary_pressed_background = authored.primary_pressed_background;
    let primary_pressed_foreground = authored.primary_pressed_foreground;
    let primary_pressed_icon = primary_pressed_foreground;
    let primary_pressed_border = primary_pressed_background;
    let primary_disabled_background = primary_background.mix(background, 0.65);
    let primary_disabled_foreground = text_disabled;
    let primary_disabled_icon = primary_disabled_foreground;
    let primary_disabled_border = primary_disabled_background;
    let destructive_background = authored.destructive_background;
    let destructive_foreground = authored.destructive_foreground;
    let destructive_icon = destructive_foreground;
    let destructive_border = destructive_background;
    let destructive_hover_background = authored.destructive_hover_background;
    let destructive_hover_foreground = authored.destructive_hover_foreground;
    let destructive_hover_icon = destructive_hover_foreground;
    let destructive_hover_border = destructive_hover_background;
    let destructive_pressed_background = authored.destructive_pressed_background;
    let destructive_pressed_foreground = authored.destructive_pressed_foreground;
    let destructive_pressed_icon = destructive_pressed_foreground;
    let destructive_pressed_border = destructive_pressed_background;
    let destructive_disabled_background = destructive_background.mix(background, 0.65);
    let destructive_disabled_foreground = text_disabled;
    let destructive_disabled_icon = destructive_disabled_foreground;
    let destructive_disabled_border = destructive_disabled_background;
    let selection_background = authored.selection_background;
    let selection_foreground = contrast(text, selection_background.source_over(background), 4.5);
    let selection_icon = selection_foreground;
    let selection_border = selection_background;
    let selection_hover_background = selection_background.mix(text, 0.06);
    let selection_hover_foreground = contrast(
        text,
        selection_hover_background.source_over(background),
        4.5,
    );
    let selection_hover_icon = selection_hover_foreground;
    let selection_hover_border = selection_hover_background;
    let selection_pressed_background = selection_background.mix(text, 0.18);
    let selection_pressed_foreground = contrast(
        text,
        selection_pressed_background.source_over(background),
        4.5,
    );
    let selection_pressed_icon = selection_pressed_foreground;
    let selection_pressed_border = selection_pressed_background;
    let selection_disabled_background = selection_background.mix(background, 0.65);
    let selection_disabled_foreground = text_disabled;
    let selection_disabled_icon = selection_disabled_foreground;
    let selection_disabled_border = selection_disabled_background;
    let toggle_off_background = authored.toggle_off_background;
    let progress_track = toggle_off_background;
    let progress_indicator = text_accent;
    let toggle_off_mark = authored.toggle_off_mark;
    let toggle_off_border = authored.toggle_off_border;
    let toggle_off_label = text;
    let toggle_off_hover_background = toggle_off_background.mix(toggle_off_mark, 0.08);
    let toggle_off_hover_mark = authored.toggle_off_hover_mark;
    let toggle_off_hover_border = authored.toggle_off_hover_border;
    let toggle_off_hover_label = text;
    let toggle_off_pressed_background = toggle_off_background.mix(toggle_off_mark, 0.16);
    let toggle_off_pressed_mark = authored.toggle_off_pressed_mark;
    let toggle_off_pressed_border = authored.toggle_off_pressed_border;
    let toggle_off_pressed_label = text;
    let toggle_off_disabled_background = toggle_off_background.mix(background, 0.65);
    let toggle_off_disabled_mark = contrast(
        text,
        toggle_off_disabled_background.source_over(background),
        4.5,
    );
    let toggle_off_disabled_border = border_disabled;
    let toggle_off_disabled_label = text_disabled;
    // Enabled toggles share filled-action emphasis, not the link foreground.
    let toggle_on_background = authored.toggle_on_background;
    let toggle_on_mark = authored.toggle_on_mark;
    let toggle_on_border = authored.toggle_on_border;
    let toggle_on_label = text;
    let toggle_on_hover_background = authored.toggle_on_hover_background;
    let toggle_on_hover_mark = authored.toggle_on_hover_mark;
    let toggle_on_hover_border = authored.toggle_on_hover_border;
    let toggle_on_hover_label = text;
    let toggle_on_pressed_background = authored.toggle_on_pressed_background;
    let toggle_on_pressed_mark = authored.toggle_on_pressed_mark;
    let toggle_on_pressed_border = authored.toggle_on_pressed_border;
    let toggle_on_pressed_label = text;
    let toggle_on_disabled_background = toggle_on_background.mix(background, 0.65);
    let toggle_on_disabled_mark = authored.toggle_on_disabled_mark;
    let toggle_on_disabled_border = border_disabled;
    let toggle_on_disabled_label = text_disabled;
    let row_background = authored.row_background;
    let row_foreground = contrast(text, row_background.source_over(background), 4.5);
    let row_secondary = contrast(text_secondary, row_background.source_over(background), 4.5);
    let row_icon = row_foreground;
    let row_match = contrast(text_accent, row_background.source_over(background), 4.5);
    let row_border = border_transparent;
    let row_hover_background = authored.row_hover_background;
    let row_hover_foreground = contrast(text, row_hover_background.source_over(background), 4.5);
    let row_hover_secondary = contrast(
        text_secondary,
        row_hover_background.source_over(background),
        4.5,
    );
    let row_hover_icon = row_hover_foreground;
    let row_hover_match = contrast(
        text_accent,
        row_hover_background.source_over(background),
        4.5,
    );
    let row_hover_border = border_transparent;
    let row_selected_background = authored.row_selected_background;
    let row_selected_foreground = authored.row_selected_foreground;
    let row_selected_secondary = authored.row_selected_secondary;
    let row_selected_icon = row_selected_foreground;
    let navigation_selected_background = authored
        .navigation_selected_background
        .unwrap_or(row_selected_background);
    let navigation_selected_foreground = row_selected_foreground;
    let navigation_selected_secondary = row_selected_secondary;
    let navigation_selected_icon = row_selected_icon;
    let row_selected_match = contrast(
        text_accent,
        row_selected_background.source_over(background),
        4.5,
    );
    let row_selected_border = authored.row_selected_border;
    let row_selected_hover_background = authored
        .row_selected_hover_background
        .unwrap_or_else(|| row_selected_background.mix(text, 0.06));
    let row_selected_hover_foreground = authored.row_selected_hover_foreground;
    let row_selected_hover_secondary = authored.row_selected_hover_secondary;
    let row_selected_hover_icon = row_selected_hover_foreground;
    let row_selected_hover_match = contrast(
        text_accent,
        row_selected_hover_background.source_over(background),
        4.5,
    );
    let row_selected_hover_border = authored.row_selected_hover_border;
    let element_icon = element_foreground;
    let element_hover_icon = element_hover_foreground;
    let element_active_icon = element_active_foreground;
    let element_disabled_icon = element_disabled_foreground;
    let ghost_element_icon = ghost_element_foreground;
    let ghost_element_hover_icon = ghost_element_hover_foreground;
    let ghost_element_active_icon = ghost_element_active_foreground;
    let ghost_element_disabled_icon = ghost_element_disabled_foreground;
    let badge_background = panel_background;
    let badge_foreground = contrast(text_secondary, badge_background, 4.5);
    let preview_background = elevated_surface_background;
    let preview_foreground = contrast(text_secondary, preview_background, 4.5);
    let tab_active_background = authored.tab_active_background;
    let tab_inactive_background = title_bar_background;
    let tab_active_foreground = authored.tab_active_foreground;
    let tab_inactive_foreground = contrast(text_secondary, tab_inactive_background, 4.5);
    let inactive_selection_background = authored.inactive_selection_background;
    let element_border = border_transparent;
    let element_hover_border = border_transparent;
    let element_active_border = border_transparent;
    let element_disabled_border = border_transparent;
    let ghost_element_border = border_transparent;
    let ghost_element_hover_border = border_transparent;
    let ghost_element_active_border = border_transparent;
    let ghost_element_disabled_border = border_disabled;
    let outline_border = authored.outline_border;
    let outline_hover_border = authored.outline_hover_border;
    let outline_pressed_border = authored.outline_pressed_border;
    let outline_disabled_border = authored.outline_disabled_border;
    let tab_hover_background = row_hover_background;
    let tab_hover_foreground = contrast(text, tab_hover_background, 4.5);
    let tab_hover_icon = tab_hover_foreground;
    let tab_active_icon = tab_active_foreground;
    let tab_inactive_icon = tab_inactive_foreground;
    // The Active Tab carries the selected-row hierarchy in its own roles: a rim that stays put, and a
    // hover that gains weight from the Tab's material rather than from a list row's.
    let tab_active_border = authored.tab_active_border;
    let tab_active_hover_background = authored
        .tab_active_hover_background
        .unwrap_or_else(|| tab_active_background.mix(text, 0.06));
    let tab_active_hover_foreground = authored.tab_active_hover_foreground;
    // Close rests on the selected-hover fill whenever the pointer is anywhere over the Active Tab,
    // so its glyph follows the hovered title and keeps a non-text contrast against that fill.
    let tab_active_hover_icon = contrast(
        tab_active_hover_foreground,
        tab_active_hover_background.source_over(background),
        3.0,
    );
    let inactive_selection_border = authored.inactive_selection_border;
    // When not authored, the inactive Tab separator steps the inactive title back into the bar.
    let tab_separator = authored.tab_separator;
    let link_text_pressed = authored.link_text_pressed;
    let link_text_disabled = text_disabled;
    ChromeColors {
        background,
        panel_background,
        elevated_surface_background,
        title_bar_background,
        title_bar_inactive_background,
        tab_active_background,
        tab_inactive_background,
        text,
        text_secondary,
        text_muted,
        text_placeholder,
        text_disabled,
        text_accent,
        link_text,
        link_text_hover,
        link_text_pressed,
        link_text_disabled,
        icon,
        icon_muted,
        icon_disabled,
        border,
        border_variant,
        border_focused,
        focus_ring,
        border_selected,
        border_disabled,
        border_transparent,
        element_background,
        segmented_track_background,
        element_hover,
        element_active,
        element_selected,
        element_disabled,
        element_foreground,
        element_hover_foreground,
        element_active_foreground,
        element_disabled_foreground,
        ghost_element_background,
        ghost_element_hover,
        ghost_element_active,
        ghost_element_selected,
        ghost_element_disabled,
        ghost_element_foreground,
        ghost_element_hover_foreground,
        ghost_element_active_foreground,
        ghost_element_selected_foreground,
        ghost_element_disabled_foreground,
        sidebar_focus,
        info,
        info_background,
        success,
        warning,
        warning_background,
        warning_border,
        error,
        error_background,
        error_border,
        input_text,
        input_placeholder,
        input_disabled_text,
        input_caret,
        input_selection_background,
        input_selection_foreground,
        input_background,
        input_disabled_background,
        input_border,
        input_invalid_border,
        modal_scrim,
        scrollbar_track,
        scrollbar_track_border,
        scrollbar_thumb_background,
        scrollbar_thumb_border,
        scrollbar_thumb_hover_background,
        resize_idle,
        resize_focused,
        resize_hovered,
        resize_dragged,
        resize_disabled,
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
        progress_track,
        progress_indicator,
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
        navigation_selected_background,
        navigation_selected_foreground,
        navigation_selected_secondary,
        navigation_selected_icon,
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
        inactive_selection_background,
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
        outline_border,
        outline_hover_border,
        outline_pressed_border,
        outline_disabled_border,
        tab_hover_background,
        tab_hover_foreground,
        tab_hover_icon,
        tab_active_icon,
        tab_inactive_icon,
        tab_active_border,
        tab_active_hover_background,
        tab_active_hover_foreground,
        tab_active_hover_icon,
        inactive_selection_border,
        tab_separator,
        repository_operation,
        repository_changes,
        pull_request_open,
        pull_request_draft,
    }
}

/// Preserve the proposed color when readable, otherwise choose the better neutral.
fn contrast(foreground: Color, background: Color, minimum: f64) -> Color {
    if foreground
        .source_over(background)
        .contrast_ratio(background)
        >= minimum
    {
        return foreground;
    }
    let dark = Color::BLACK;
    let light = Color::WHITE;
    if dark.contrast_ratio(background) >= light.contrast_ratio(background) {
        dark
    } else {
        light
    }
}

/// The readable color nearest `foreground`, keeping its hue, so a muted tone that misses the
/// minimum on `background` darkens or lightens only as far as it must instead of becoming black
/// or white.
fn nearest_readable(foreground: Color, background: Color, minimum: f64) -> Color {
    foreground
        .readable_preserving_chroma(&[background], minimum)
        .unwrap_or_else(|| contrast(foreground, background, minimum))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SemanticPaint {
    pub(crate) background: Color,
    pub(crate) foreground: Color,
    pub(crate) icon: Color,
    pub(crate) border: Color,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CaptionPaint {
    pub(crate) background: Color,
    pub(crate) foreground: Color,
    pub(crate) secondary: Color,
    pub(crate) icon: Color,
    pub(crate) focus: Color,
    pub(crate) attention: Color,
    pub(crate) busy: Color,
    pub(crate) error: Color,
    pub(crate) repository_operation: Color,
    pub(crate) repository_changes: Color,
    pub(crate) control: SemanticPaint,
    pub(crate) control_hover: SemanticPaint,
    pub(crate) control_pressed: SemanticPaint,
    pub(crate) control_disabled: SemanticPaint,
}

impl ChromeColors {
    /// Pane Caption context is an opaque Terminal surface, including program-controlled colors.
    /// It does not mutate the theme. All caption controls share this contextual policy.
    pub(crate) fn caption(&self, surface: Color, focused: bool) -> CaptionPaint {
        let surface = surface.source_over(self.background.with_alpha(255));
        let foreground = nearest_readable(
            if focused {
                self.text_secondary
            } else {
                self.text_muted
            },
            surface,
            4.5,
        );
        let paint = |background: Color, proposed: Color| {
            let background = background.source_over(surface);
            let foreground = contrast(proposed, background, 4.5);
            SemanticPaint {
                background,
                foreground,
                icon: foreground,
                border: Color::rgba(0),
            }
        };
        CaptionPaint {
            background: surface,
            foreground,
            secondary: nearest_readable(self.text_muted, surface, 4.5),
            icon: foreground,
            focus: contrast(self.border_focused, surface, 3.0),
            attention: contrast(self.warning, surface, 4.5),
            busy: contrast(self.info, surface, 4.5),
            error: contrast(self.error, surface, 4.5),
            repository_operation: nearest_readable(self.repository_operation, surface, 4.5),
            repository_changes: nearest_readable(self.repository_changes, surface, 4.5),
            control: paint(surface, foreground),
            control_hover: paint(surface.mix(foreground, 0.10), foreground),
            control_pressed: paint(surface.mix(foreground, 0.18), foreground),
            control_disabled: paint(surface, self.text_disabled),
        }
    }
}

/// Status marks resolved for the surface one control currently rests on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StatusPaint {
    pub(crate) attention: Color,
    pub(crate) busy: Color,
    pub(crate) error: Color,
    pub(crate) paused: Color,
}

impl ChromeColors {
    /// Resolves semantic status colors against the control's current surface.
    ///
    /// The surface is composed over the opaque Chrome background. Resolving per surface guarantees
    /// contrast across active and inactive Tab surfaces.
    pub(crate) fn status(&self, surface: Color) -> StatusPaint {
        let base = self.background.with_alpha(255);
        let surface = surface.source_over(base);
        StatusPaint {
            attention: contrast(self.warning, surface, 3.0),
            busy: contrast(self.info, surface, 3.0),
            error: contrast(self.error, surface, 3.0),
            paused: contrast(self.text_muted, surface, 3.0),
        }
    }
}

/// Pull Request states resolved for the surface their text currently rests on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PullRequestPaint {
    /// The Pull Request number, a link to the Pull Request in either state.
    pub(crate) link: Color,
    /// The Open state label.
    pub(crate) open: Color,
    /// The Draft state label.
    pub(crate) draft: Color,
}

impl ChromeColors {
    /// Resolves Pull Request text colors against one surface composed over the opaque Chrome
    /// background, so the number stays readable on resting, hovered, and selected rows.
    pub(crate) fn pull_request(&self, surface: Color) -> PullRequestPaint {
        let surface = surface.source_over(self.background.with_alpha(255));
        PullRequestPaint {
            link: contrast(self.link_text, surface, 4.5),
            open: contrast(self.pull_request_open, surface, 4.5),
            draft: contrast(self.pull_request_draft, surface, 4.5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::builtin;
    use super::*;
    #[test]
    fn builtins_pass_supported_text_and_control_contrast_pairs() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let c = builtin::chrome_base(appearance);
            for (foreground, surface) in [
                (c.text, c.background),
                (c.text_secondary, c.panel_background),
                (c.text_muted, c.elevated_surface_background),
                (c.input_placeholder, c.input_background),
                (c.preview_foreground, c.preview_background),
                (c.badge_foreground, c.badge_background),
                (c.primary_foreground, c.primary_background),
                (c.primary_hover_foreground, c.primary_hover_background),
                (c.primary_pressed_foreground, c.primary_pressed_background),
                (c.destructive_foreground, c.destructive_background),
                (
                    c.destructive_hover_foreground,
                    c.destructive_hover_background,
                ),
                (
                    c.destructive_pressed_foreground,
                    c.destructive_pressed_background,
                ),
                (c.row_selected_foreground, c.row_selected_background),
                (c.row_selected_secondary, c.row_selected_background),
                (c.row_selected_match, c.row_selected_background),
                (
                    c.row_selected_hover_foreground,
                    c.row_selected_hover_background,
                ),
                (
                    c.row_selected_hover_secondary,
                    c.row_selected_hover_background,
                ),
                (c.row_selected_hover_match, c.row_selected_hover_background),
                (c.toggle_on_mark, c.toggle_on_background),
                (c.toggle_on_hover_mark, c.toggle_on_hover_background),
                (c.toggle_on_pressed_mark, c.toggle_on_pressed_background),
                (c.toggle_off_mark, c.toggle_off_background),
                (c.toggle_off_hover_mark, c.toggle_off_hover_background),
                (c.toggle_off_pressed_mark, c.toggle_off_pressed_background),
                (c.tab_active_hover_foreground, c.tab_active_hover_background),
            ] {
                assert!(
                    foreground.source_over(surface).contrast_ratio(surface) >= 4.5,
                    "{appearance:?} {foreground:?} {surface:?}"
                );
            }
            for (indicator, surface) in [
                (c.border_focused, c.background),
                (c.border_focused, c.elevated_surface_background),
                (c.scrollbar_thumb_background, c.panel_background),
                (c.scrollbar_thumb_hover_background, c.panel_background),
                (c.scrollbar_thumb_active_background, c.panel_background),
                (c.toggle_off_border, c.toggle_off_background),
                (c.toggle_off_hover_border, c.toggle_off_hover_background),
                (c.toggle_off_pressed_border, c.toggle_off_pressed_background),
            ] {
                assert!(
                    indicator.source_over(surface).contrast_ratio(surface) >= 3.0,
                    "{appearance:?} {indicator:?} {surface:?}"
                );
            }
        }
    }
    #[test]
    fn captions_resolve_text_marks_and_enabled_controls_on_program_surfaces() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = builtin::chrome_base(appearance);
            for surface in [
                Color::rgb(0x141415),
                Color::rgb(0xfbfbfc),
                Color::rgb(0xff00ff),
                Color::rgb(0x777777),
                Color::BLACK,
            ] {
                for focused in [false, true] {
                    let caption = colors.caption(surface, focused);
                    assert_eq!(caption.background, surface);
                    assert_eq!(caption.foreground, caption.control.icon);
                    for text in [
                        caption.foreground,
                        caption.secondary,
                        caption.icon,
                        caption.attention,
                        caption.busy,
                        caption.error,
                        caption.repository_operation,
                        caption.repository_changes,
                    ] {
                        assert!(text.contrast_ratio(surface) >= 4.5);
                    }
                    assert!(caption.focus.contrast_ratio(surface) >= 3.0);
                    for paint in [
                        caption.control,
                        caption.control_hover,
                        caption.control_pressed,
                    ] {
                        assert!(paint.foreground.contrast_ratio(paint.background) >= 4.5);
                        assert!(paint.icon.contrast_ratio(paint.background) >= 4.5);
                    }
                }
            }
        }
    }
    #[test]
    fn pull_request_text_reads_on_every_row_surface() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = builtin::chrome_base(appearance);
            let base = colors.background.with_alpha(255);
            for surface in [
                colors.title_bar_background,
                colors.row_hover_background,
                colors.row_selected_background,
                colors.row_selected_hover_background,
                colors.elevated_surface_background,
            ] {
                let paint = colors.pull_request(surface);
                let surface = surface.source_over(base);
                assert!(paint.link.contrast_ratio(surface) >= 4.5);
                assert!(paint.open.contrast_ratio(surface) >= 4.5);
                assert!(paint.draft.contrast_ratio(surface) >= 4.5);
            }
            // The resting sidebar keeps the authored hue; a selected row may fall back to a
            // neutral because the Open or Draft word, not color, carries the state.
            assert_eq!(
                colors.pull_request(colors.title_bar_background).open,
                colors.pull_request_open,
                "{appearance:?}"
            );
            assert_eq!(
                colors.pull_request(colors.title_bar_background).link,
                colors.link_text,
                "{appearance:?}"
            );
        }
    }

    #[test]
    fn status_marks_read_on_every_tab_surface_they_rest_on() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = builtin::chrome_base(appearance);
            let base = colors.background.with_alpha(255);
            for surfaces in [
                vec![
                    colors.tab_active_background,
                    colors.tab_active_hover_background,
                    colors.tab_inactive_background,
                    colors.tab_hover_background,
                ],
                // A surface painted in the warning color itself still gets a readable mark.
                vec![Color::rgb(0xfbfbfc), Color::rgb(0xe0a75c)],
            ] {
                for surface in surfaces {
                    let status = colors.status(surface);
                    for mark in [status.attention, status.busy, status.error, status.paused] {
                        let surface = surface.source_over(base);
                        assert!(
                            mark.contrast_ratio(surface) >= 3.0,
                            "{appearance:?} {mark:?} {surface:?}"
                        );
                    }
                }
            }
            let resting = [colors.tab_active_background, colors.tab_inactive_background];
            for surface in resting {
                if colors.warning.contrast_ratio(surface.source_over(base)) >= 3.0 {
                    assert_eq!(colors.status(surface).attention, colors.warning);
                }
            }
        }
    }
    #[test]
    fn status_fallback_meets_contrast_on_opposing_surfaces() {
        let surfaces = [Color::BLACK, Color::WHITE];
        let colors = ChromeColors {
            warning: Color::BLACK,
            ..builtin::chrome_base(Appearance::Light)
        };
        for surface in surfaces {
            let mark = colors.status(surface).attention;
            assert!(mark.contrast_ratio(surface) >= 3.0);
        }
    }
}
