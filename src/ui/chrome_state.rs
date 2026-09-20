//! Product-owned window activity and accessibility presentation policy.

use crate::appearance::{ChromeColors, Color, CompositionCapabilities};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChromeStatePolicy {
    pub(crate) active: bool,
    pub(crate) capabilities: CompositionCapabilities,
}

impl ChromeStatePolicy {
    /// Preserve authored surface colors unless no foreground can meet the requested floor.
    pub(crate) fn surfaces(self, source: &ChromeColors) -> ChromeColors {
        let mut colors = source.clone();
        if self.capabilities.increase_contrast {
            colors.background = contrast_host(colors.background, 7.0);
            colors.panel_background = contrast_host(colors.panel_background, 7.0);
            colors.elevated_surface_background =
                contrast_host(colors.elevated_surface_background, 7.0);
            colors.preview_background = contrast_host(colors.preview_background, 7.0);
        }
        colors
    }

    pub(crate) fn colors(self, source: &ChromeColors, host: Color) -> ChromeColors {
        let mut colors = source.clone();
        if !self.active {
            inactive(&mut colors, host);
        }
        disabled_content(&mut colors, host, 3.0);
        if self.capabilities.increase_contrast {
            increased_contrast(&mut colors, host);
        }
        if self.capabilities.show_borders {
            control_edges(&mut colors, host);
        }
        if !self.active {
            suppress_hover(&mut colors);
            colors.focus_ring = Color::rgba(0);
            colors.sidebar_focus = Color::rgba(0);
            colors.border_focused = colors.border;
            colors.input_focused_border = colors.input_border;
            colors.resize_focused = colors.resize_idle;
        }
        colors
    }
}

/// Resolves disabled content against the surface it actually paints on.
///
/// Disabled fills stay authored and remain identical across window activity. Only their content is
/// moved when the authored paint falls below the disabled readability floor.
fn disabled_content(c: &mut ChromeColors, host: Color, minimum: f64) {
    macro_rules! on_host {
        ($($role:ident),+ $(,)?) => { $(
            c.$role = readable(c.$role, host, minimum);
        )+ };
    }
    on_host!(text_disabled, icon_disabled, link_text_disabled);

    macro_rules! on_fill {
        ($($fill:ident => [$($role:ident),+]),+ $(,)?) => { $(
            let background = c.$fill.source_over(host);
            $(c.$role = readable(c.$role, background, minimum);)+
        )+ };
    }
    on_fill!(
        element_disabled => [element_disabled_foreground, element_disabled_icon],
        ghost_element_disabled => [ghost_element_disabled_foreground, ghost_element_disabled_icon],
        primary_disabled_background => [primary_disabled_foreground, primary_disabled_icon],
        destructive_disabled_background => [destructive_disabled_foreground, destructive_disabled_icon],
        input_disabled_background => [input_disabled_text],
        selection_disabled_background => [selection_disabled_foreground, selection_disabled_icon],
        toggle_off_disabled_background => [toggle_off_disabled_mark],
        toggle_on_disabled_background => [toggle_on_disabled_mark]
    );
    on_host!(toggle_off_disabled_label, toggle_on_disabled_label);
}

fn suppress_hover(c: &mut ChromeColors) {
    macro_rules! rest {
        ($($hover:ident => $idle:ident),+ $(,)?) => { $(c.$hover = c.$idle;)+ };
    }
    rest!(
        element_hover => element_background,
        element_hover_foreground => element_foreground,
        element_hover_icon => element_icon,
        element_hover_border => element_border,
        ghost_element_hover => ghost_element_background,
        ghost_element_hover_foreground => ghost_element_foreground,
        ghost_element_hover_icon => ghost_element_icon,
        ghost_element_hover_border => ghost_element_border,
        primary_hover_background => primary_background,
        primary_hover_foreground => primary_foreground,
        primary_hover_icon => primary_icon,
        primary_hover_border => primary_border,
        outline_hover_border => outline_border,
        destructive_hover_background => destructive_background,
        destructive_hover_foreground => destructive_foreground,
        destructive_hover_icon => destructive_icon,
        destructive_hover_border => destructive_border,
        toggle_off_hover_background => toggle_off_background,
        toggle_off_hover_mark => toggle_off_mark,
        toggle_off_hover_label => toggle_off_label,
        toggle_off_hover_border => toggle_off_border,
        toggle_on_hover_background => toggle_on_background,
        toggle_on_hover_mark => toggle_on_mark,
        toggle_on_hover_label => toggle_on_label,
        toggle_on_hover_border => toggle_on_border,
        selection_hover_background => selection_background,
        selection_hover_foreground => selection_foreground,
        selection_hover_icon => selection_icon,
        selection_hover_border => selection_border,
        row_hover_background => row_background,
        row_hover_foreground => row_foreground,
        row_hover_secondary => row_secondary,
        row_hover_icon => row_icon,
        row_hover_match => row_match,
        row_hover_border => row_border,
        row_selected_hover_background => row_selected_background,
        row_selected_hover_foreground => row_selected_foreground,
        row_selected_hover_secondary => row_selected_secondary,
        row_selected_hover_icon => row_selected_icon,
        row_selected_hover_match => row_selected_match,
        row_selected_hover_border => row_selected_border,
        tab_active_hover_background => tab_active_background,
        tab_active_hover_foreground => tab_active_foreground,
        tab_active_hover_icon => tab_active_icon,
        tab_hover_background => tab_inactive_background,
        tab_hover_foreground => tab_inactive_foreground,
        tab_hover_icon => tab_inactive_icon,
        link_text_hover => link_text,
        resize_hovered => resize_idle,
        scrollbar_thumb_hover_background => scrollbar_thumb_background
    );
}

fn readable(color: Color, host: Color, minimum: f64) -> Color {
    super::appearance::readable_on_backgrounds(color, [host.with_alpha(255)], minimum)
}

/// A middle-luminance host can have less than 7:1 contrast with both black and white. Move
/// only such a host toward the nearest feasible endpoint before resolving its content.
pub(super) fn contrast_host(host: Color, minimum: f64) -> Color {
    let dark = Color::rgb(0x000000);
    let light = Color::rgb(0xffffff);
    let ink = if dark.contrast_ratio(host) >= light.contrast_ratio(host) {
        dark
    } else {
        light
    };
    if ink.contrast_ratio(host) >= minimum {
        return host;
    }
    let endpoint = if ink == dark { light } else { dark };
    let (mut low, mut high) = (0.0, 1.0);
    let mut result = endpoint;
    for _ in 0..16 {
        let amount = (low + high) / 2.0;
        let candidate = host.mix(endpoint, amount);
        if ink.contrast_ratio(candidate) >= minimum {
            result = candidate;
            high = amount;
        } else {
            low = amount;
        }
    }
    result
}

fn inactive(c: &mut ChromeColors, host: Color) {
    macro_rules! soften {
        ($amount:expr; $($role:ident),+ $(,)?) => { $(
            c.$role = c.$role.mix(host, $amount);
        )+ };
    }
    soften!(0.45;
        element_background, element_hover, element_active,
        ghost_element_hover, ghost_element_active,
        input_background, toggle_off_background, toggle_off_hover_background,
        toggle_off_pressed_background
    );
    macro_rules! selected {
        ($($role:ident),+ $(,)?) => { $(
            c.$role = readable(c.$role.mix(host, 0.55), host, 1.12);
        )+ };
    }
    selected!(
        element_selected,
        ghost_element_selected,
        selection_background,
        selection_hover_background,
        selection_pressed_background,
        row_selected_background,
        row_selected_hover_background,
        navigation_selected_background,
        tab_active_background,
        tab_active_hover_background
    );
    macro_rules! text {
        ($minimum:expr; $($role:ident),+ $(,)?) => { $(
            c.$role = readable(c.$role.mix(host, 0.20), host, $minimum);
        )+ };
    }
    text!(4.5;
        text, icon, element_foreground, element_hover_foreground, element_active_foreground,
        element_icon, element_hover_icon, element_active_icon,
        ghost_element_foreground, ghost_element_hover_foreground,
        ghost_element_active_foreground, ghost_element_selected_foreground,
        ghost_element_icon, ghost_element_hover_icon, ghost_element_active_icon,
        row_foreground, row_hover_foreground, row_selected_foreground,
        row_selected_hover_foreground, navigation_selected_foreground,
        selection_foreground, selection_hover_foreground, selection_pressed_foreground,
        selection_icon, selection_hover_icon, selection_pressed_icon,
        tab_active_foreground, tab_active_hover_foreground, tab_inactive_foreground,
        tab_hover_foreground, input_text, toggle_off_label, toggle_on_label
    );
    text!(3.0;
        text_secondary, text_muted, text_placeholder, icon_muted,
        row_secondary, row_hover_secondary, row_selected_secondary,
        row_selected_hover_secondary, navigation_selected_secondary,
        tab_active_icon, tab_active_hover_icon, tab_inactive_icon, tab_hover_icon
    );
    c.navigation_selected_icon = c.text_secondary;
    c.primary_background = c.element_background;
    c.primary_hover_background = c.element_background;
    c.primary_pressed_background = c.element_background;
    c.primary_foreground = readable(c.text, c.element_background.source_over(host), 4.5);
    c.primary_hover_foreground = c.primary_foreground;
    c.primary_pressed_foreground = c.primary_foreground;
    c.primary_icon = c.primary_foreground;
    c.primary_hover_icon = c.primary_foreground;
    c.primary_pressed_icon = c.primary_foreground;
    c.primary_border = c.element_border;
    c.primary_hover_border = c.element_border;
    c.primary_pressed_border = c.element_border;
    let checked = host.mix(c.text, 0.45);
    c.toggle_on_background = checked;
    c.toggle_on_hover_background = checked;
    c.toggle_on_pressed_background = checked;
    c.toggle_on_mark = readable(c.toggle_on_mark, checked, 4.5);
    c.toggle_on_hover_mark = c.toggle_on_mark;
    c.toggle_on_pressed_mark = c.toggle_on_mark;
    // The window variant now owns the inactive selection. Legacy Tab consumers remain equivalent
    // until they switch to the common active-state roles.
    c.tab_inactive_selected_background = c.tab_active_background;
    c.tab_inactive_selected_foreground = c.tab_active_foreground;
    c.tab_inactive_selected_icon = c.tab_active_icon;
    c.tab_inactive_selected_border = c.tab_active_border;
}

fn control_edges(c: &mut ChromeColors, host: Color) {
    let edge = readable(host, host, 3.0);
    macro_rules! edges {
        ($($role:ident),+ $(,)?) => { $( c.$role = edge; )+ };
    }
    edges!(
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
        primary_border,
        primary_hover_border,
        primary_pressed_border,
        primary_disabled_border,
        destructive_border,
        destructive_hover_border,
        destructive_pressed_border,
        destructive_disabled_border,
        toggle_off_border,
        toggle_off_hover_border,
        toggle_off_pressed_border,
        toggle_off_disabled_border,
        toggle_on_border,
        toggle_on_hover_border,
        toggle_on_pressed_border,
        toggle_on_disabled_border,
        selection_border,
        selection_hover_border,
        selection_pressed_border,
        selection_disabled_border,
        input_border,
        input_disabled_border,
        tab_active_border,
        tab_inactive_selected_border
    );
}

fn increased_contrast(c: &mut ChromeColors, host: Color) {
    macro_rules! content {
        ($minimum:expr; $($role:ident),+ $(,)?) => { $( c.$role = readable(c.$role, host, $minimum); )+ };
    }
    content!(7.0; text, icon, row_foreground, row_hover_foreground,
        row_selected_foreground, row_selected_hover_foreground, navigation_selected_foreground,
        tab_active_foreground, tab_active_hover_foreground, tab_inactive_foreground,
        tab_inactive_selected_foreground, tab_hover_foreground);
    content!(4.5; text_secondary, text_muted, text_placeholder, text_disabled,
        icon_muted, icon_disabled, row_secondary, row_hover_secondary,
        row_selected_secondary, row_selected_hover_secondary, navigation_selected_secondary,
        row_icon, row_hover_icon, row_selected_icon, row_selected_hover_icon,
        navigation_selected_icon, tab_active_icon, tab_active_hover_icon,
        tab_inactive_icon, tab_inactive_selected_icon, tab_hover_icon,
        link_text_disabled, ghost_element_disabled_foreground, ghost_element_disabled_icon,
        toggle_off_disabled_label, toggle_on_disabled_label);
    content!(3.0; border, border_variant, border_selected, border_disabled,
        tab_separator, resize_idle, resize_hovered, resize_dragged, resize_disabled,
        scrollbar_track_border, scrollbar_thumb_border);
    for focus in [&mut c.focus_ring, &mut c.sidebar_focus] {
        if focus.a != 0 {
            *focus = readable(*focus, host, 4.5);
        }
    }
    macro_rules! selection {
        ($($fill:ident => $edge:ident),+ $(,)?) => { $(
            c.$fill = readable(c.$fill, host, 1.4);
            c.$edge = readable(c.$edge, host, 3.0);
        )+ };
    }
    selection!(
        selection_background => selection_border,
        selection_hover_background => selection_hover_border,
        selection_pressed_background => selection_pressed_border,
        selection_disabled_background => selection_disabled_border,
        row_selected_background => row_selected_border,
        row_selected_hover_background => row_selected_hover_border,
        tab_active_background => tab_active_border,
        tab_inactive_selected_background => tab_inactive_selected_border
    );
    c.navigation_selected_background = readable(c.navigation_selected_background, host, 1.4);
    macro_rules! control {
        ($minimum:expr; $($fill:ident => [$($ink:ident),+] => $edge:ident),+ $(,)?) => { $(
            let original = c.$fill.source_over(host);
            let background = contrast_host(original, $minimum);
            if background != original { c.$fill = background; }
            $(c.$ink = readable(c.$ink, background, $minimum);)+
            c.$edge = readable(c.$edge, host, 3.0);
        )+ };
    }
    control!(7.0;
        element_background => [element_foreground, element_icon] => element_border,
        element_hover => [element_hover_foreground, element_hover_icon] => element_hover_border,
        element_active => [element_active_foreground, element_active_icon] => element_active_border,
        ghost_element_background => [ghost_element_foreground, ghost_element_icon] => ghost_element_border,
        ghost_element_hover => [ghost_element_hover_foreground, ghost_element_hover_icon] => ghost_element_hover_border,
        ghost_element_active => [ghost_element_active_foreground, ghost_element_active_icon] => ghost_element_active_border,
        primary_background => [primary_foreground, primary_icon] => primary_border,
        primary_hover_background => [primary_hover_foreground, primary_hover_icon] => primary_hover_border,
        primary_pressed_background => [primary_pressed_foreground, primary_pressed_icon] => primary_pressed_border,
        destructive_background => [destructive_foreground, destructive_icon] => destructive_border,
        destructive_hover_background => [destructive_hover_foreground, destructive_hover_icon] => destructive_hover_border,
        destructive_pressed_background => [destructive_pressed_foreground, destructive_pressed_icon] => destructive_pressed_border,
        input_background => [input_text] => input_border
    );
    control!(7.0;
        selection_background => [selection_foreground, selection_icon] => selection_border,
        selection_hover_background => [selection_hover_foreground, selection_hover_icon] => selection_hover_border,
        selection_pressed_background => [selection_pressed_foreground, selection_pressed_icon] => selection_pressed_border,
        row_selected_background => [row_selected_foreground] => row_selected_border,
        row_selected_hover_background => [row_selected_hover_foreground] => row_selected_hover_border,
        tab_active_background => [tab_active_foreground] => tab_active_border,
        tab_inactive_selected_background => [tab_inactive_selected_foreground] => tab_inactive_selected_border,
        toggle_off_background => [toggle_off_mark] => toggle_off_border,
        toggle_off_hover_background => [toggle_off_hover_mark] => toggle_off_hover_border,
        toggle_off_pressed_background => [toggle_off_pressed_mark] => toggle_off_pressed_border,
        toggle_on_background => [toggle_on_mark] => toggle_on_border,
        toggle_on_hover_background => [toggle_on_hover_mark] => toggle_on_hover_border,
        toggle_on_pressed_background => [toggle_on_pressed_mark] => toggle_on_pressed_border
    );
    control!(4.5;
        element_disabled => [element_disabled_foreground, element_disabled_icon] => element_disabled_border,
        ghost_element_disabled => [ghost_element_disabled_foreground, ghost_element_disabled_icon] => ghost_element_disabled_border,
        primary_disabled_background => [primary_disabled_foreground, primary_disabled_icon] => primary_disabled_border,
        destructive_disabled_background => [destructive_disabled_foreground, destructive_disabled_icon] => destructive_disabled_border,
        input_disabled_background => [input_disabled_text] => input_disabled_border,
        selection_disabled_background => [selection_disabled_foreground, selection_disabled_icon] => selection_disabled_border,
        toggle_off_disabled_background => [toggle_off_disabled_mark] => toggle_off_disabled_border,
        toggle_on_disabled_background => [toggle_on_disabled_mark] => toggle_on_disabled_border
    );
    c.input_placeholder = readable(
        c.input_placeholder,
        c.input_background.source_over(host),
        4.5,
    );
    c.input_focused_border = readable(c.input_focused_border, host, 3.0);
    c.input_invalid_border = readable(c.input_invalid_border, host, 3.0);
    for label in [
        &mut c.toggle_off_label,
        &mut c.toggle_off_hover_label,
        &mut c.toggle_off_pressed_label,
        &mut c.toggle_on_label,
        &mut c.toggle_on_hover_label,
        &mut c.toggle_on_pressed_label,
    ] {
        *label = readable(*label, host, 7.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(active: bool) -> ChromeStatePolicy {
        ChromeStatePolicy {
            active,
            capabilities: CompositionCapabilities::default(),
        }
    }

    #[test]
    fn inactive_controls_keep_status_and_selection_without_default_action_emphasis() {
        let source = ChromeColors::default();
        let active = policy(true).colors(&source, source.background);
        let prepared = policy(false).colors(&source, source.background);
        assert_eq!(
            prepared.destructive_background,
            source.destructive_background
        );
        assert_eq!(prepared.error, source.error);
        assert_eq!(prepared.primary_background, prepared.element_background);
        assert_eq!(prepared.element_disabled, active.element_disabled);
        assert_eq!(
            prepared.element_disabled_foreground,
            active.element_disabled_foreground
        );
        assert_ne!(
            prepared.navigation_selected_background,
            source.navigation_selected_background
        );
        assert!(
            prepared
                .navigation_selected_background
                .contrast_ratio(source.background)
                >= 1.12
        );
        assert_eq!(prepared.focus_ring.a, 0);
        assert_eq!(prepared.input_focused_border, prepared.input_border);
        assert_eq!(prepared.input_invalid_border, source.input_invalid_border);
    }

    #[test]
    fn disabled_content_meets_its_normal_floor_on_each_final_control_fill() {
        let host = Color::rgb(0xebebeb);
        let faint = Color::rgb(0xa5a5a5);
        let mut source = ChromeColors {
            background: host,
            text_disabled: faint,
            icon_disabled: faint,
            link_text_disabled: faint,
            element_disabled_foreground: faint,
            element_disabled_icon: faint,
            ghost_element_disabled_foreground: faint,
            ghost_element_disabled_icon: faint,
            primary_disabled_foreground: faint,
            primary_disabled_icon: faint,
            destructive_disabled_foreground: faint,
            destructive_disabled_icon: faint,
            input_disabled_text: faint,
            selection_disabled_foreground: faint,
            selection_disabled_icon: faint,
            toggle_off_disabled_mark: faint,
            toggle_off_disabled_label: faint,
            toggle_on_disabled_mark: faint,
            toggle_on_disabled_label: faint,
            ..ChromeColors::default()
        };
        for fill in [
            &mut source.element_disabled,
            &mut source.ghost_element_disabled,
            &mut source.primary_disabled_background,
            &mut source.destructive_disabled_background,
            &mut source.input_disabled_background,
            &mut source.selection_disabled_background,
            &mut source.toggle_off_disabled_background,
            &mut source.toggle_on_disabled_background,
        ] {
            *fill = host;
        }

        let prepared = policy(true).colors(&source, host);
        for ink in [
            prepared.text_disabled,
            prepared.icon_disabled,
            prepared.link_text_disabled,
            prepared.toggle_off_disabled_label,
            prepared.toggle_on_disabled_label,
        ] {
            assert!(ink.contrast_ratio(host) >= 3.0, "{ink:?} on {host:?}");
        }
        for (fill, inks) in [
            (
                prepared.element_disabled,
                [
                    prepared.element_disabled_foreground,
                    prepared.element_disabled_icon,
                ],
            ),
            (
                prepared.ghost_element_disabled,
                [
                    prepared.ghost_element_disabled_foreground,
                    prepared.ghost_element_disabled_icon,
                ],
            ),
            (
                prepared.primary_disabled_background,
                [
                    prepared.primary_disabled_foreground,
                    prepared.primary_disabled_icon,
                ],
            ),
            (
                prepared.destructive_disabled_background,
                [
                    prepared.destructive_disabled_foreground,
                    prepared.destructive_disabled_icon,
                ],
            ),
            (
                prepared.selection_disabled_background,
                [
                    prepared.selection_disabled_foreground,
                    prepared.selection_disabled_icon,
                ],
            ),
            (
                prepared.toggle_off_disabled_background,
                [
                    prepared.toggle_off_disabled_mark,
                    prepared.toggle_off_disabled_mark,
                ],
            ),
            (
                prepared.toggle_on_disabled_background,
                [
                    prepared.toggle_on_disabled_mark,
                    prepared.toggle_on_disabled_mark,
                ],
            ),
        ] {
            let background = fill.source_over(host);
            for ink in inks {
                assert!(
                    ink.contrast_ratio(background) >= 3.0,
                    "{ink:?} on {background:?}"
                );
            }
        }
        assert!(
            prepared
                .input_disabled_text
                .contrast_ratio(prepared.input_disabled_background.source_over(host))
                >= 3.0
        );
    }

    #[test]
    fn show_borders_adds_edges_without_changing_fill_text_or_authored_colors() {
        let source = ChromeColors::default();
        let mut state = policy(true);
        state.capabilities.show_borders = true;
        let prepared = state.colors(&source, source.background);
        assert_eq!(prepared.element_background, source.element_background);
        assert_eq!(prepared.text, source.text);
        assert!(
            prepared
                .ghost_element_border
                .source_over(source.background)
                .contrast_ratio(source.background)
                >= 3.0
        );
        assert!(
            prepared
                .ghost_element_disabled_border
                .source_over(source.background)
                .contrast_ratio(source.background)
                >= 3.0
        );
        assert_eq!(prepared.row_border, source.row_border);
        assert_eq!(source, ChromeColors::default());
    }

    #[test]
    fn increased_contrast_strengthens_content_and_selection_at_the_prepared_seam() {
        let source = ChromeColors {
            text: Color::rgb(0x777777),
            text_secondary: Color::rgb(0x666666),
            text_disabled: Color::rgb(0x444444),
            ..ChromeColors::default()
        };
        let original = source.clone();
        let mut state = policy(true);
        state.capabilities.increase_contrast = true;
        let prepared = state.colors(&source, source.background);
        assert!(prepared.text.contrast_ratio(source.background) >= 7.0);
        assert!(prepared.text_secondary.contrast_ratio(source.background) >= 4.5);
        assert!(prepared.text_disabled.contrast_ratio(source.background) >= 4.5);
        assert!(
            prepared
                .row_selected_background
                .contrast_ratio(source.background)
                >= 1.4
        );
        assert!(
            prepared
                .row_selected_border
                .contrast_ratio(source.background)
                >= 3.0
        );
        assert_eq!(source, original);
    }

    #[test]
    fn increased_contrast_resolves_infeasible_midtone_hosts_without_changing_authored_values() {
        let source = ChromeColors {
            background: Color::rgb(0x777777),
            element_background: Color::rgb(0x777777),
            primary_background: Color::rgb(0x007aff),
            ..ChromeColors::default()
        };
        let mut state = policy(true);
        state.capabilities.increase_contrast = true;
        let surfaces = state.surfaces(&source);
        let colors = state.colors(&surfaces, surfaces.background);
        for (ink, fill) in [
            (colors.text, colors.background),
            (colors.element_foreground, colors.element_background),
            (colors.primary_foreground, colors.primary_background),
        ] {
            assert!(ink.contrast_ratio(fill) >= 7.0, "{ink:?} on {fill:?}");
        }
        assert_eq!(source.background, Color::rgb(0x777777));
    }

    #[test]
    fn increased_contrast_preserves_explicitly_absent_focus_paints() {
        let absent_focus = Color::rgba(0x12345600);
        let source = ChromeColors {
            focus_ring: absent_focus,
            sidebar_focus: absent_focus,
            ..ChromeColors::default()
        };
        let mut state = policy(true);
        state.capabilities.increase_contrast = true;

        let prepared = state.colors(&source, source.background);

        assert_eq!(prepared.focus_ring, absent_focus);
        assert_eq!(prepared.sidebar_focus, absent_focus);
    }
}
