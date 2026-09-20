use super::{
    ChromeAppearance, FloatingContrastFloors, FloatingControlFamily, readable_on_backgrounds,
    state_floating_material,
};
use crate::appearance::{ChromeColors, Color};

fn preserve_disabled_colors(target: &mut ChromeColors, active: &ChromeColors) {
    macro_rules! preserve { ($($field:ident),+ $(,)?) => { $(target.$field = active.$field;)+ }; }
    preserve!(
        text_disabled,
        icon_disabled,
        link_text_disabled,
        border_disabled,
        resize_disabled,
        element_disabled,
        element_disabled_foreground,
        element_disabled_icon,
        element_disabled_border,
        ghost_element_disabled,
        ghost_element_disabled_foreground,
        ghost_element_disabled_icon,
        ghost_element_disabled_border,
        primary_disabled_background,
        primary_disabled_foreground,
        primary_disabled_icon,
        primary_disabled_border,
        destructive_disabled_background,
        destructive_disabled_foreground,
        destructive_disabled_icon,
        destructive_disabled_border,
        outline_disabled_border,
        toggle_off_disabled_background,
        toggle_off_disabled_mark,
        toggle_off_disabled_label,
        toggle_off_disabled_border,
        toggle_on_disabled_background,
        toggle_on_disabled_mark,
        toggle_on_disabled_label,
        toggle_on_disabled_border,
        input_disabled_background,
        input_disabled_text,
        input_disabled_border,
        selection_disabled_background,
        selection_disabled_foreground,
        selection_disabled_icon,
        selection_disabled_border
    );
}

fn disabled_union_contrast<const N: usize>(color: Color, backgrounds: [Color; N]) -> f64 {
    backgrounds
        .into_iter()
        .map(|background| color.source_over(background).contrast_ratio(background))
        .fold(f64::INFINITY, f64::min)
}

fn disabled_union_endpoint_candidate<const N: usize>(
    proposed: Color,
    endpoint: Color,
    backgrounds: [Color; N],
    minimum: f64,
) -> Option<(Color, f64)> {
    let mut transitions = Vec::with_capacity(2 + 4 * 255);
    transitions.extend([0.0, 1.0]);
    for (start, end) in [
        (proposed.r, endpoint.r),
        (proposed.g, endpoint.g),
        (proposed.b, endpoint.b),
        (proposed.a, endpoint.a),
    ] {
        let distance = start.abs_diff(end);
        for step in 0..distance {
            transitions.push((f64::from(step) + 0.5) / f64::from(distance));
        }
    }
    transitions.sort_by(f64::total_cmp);
    transitions.dedup();
    for (index, bounds) in transitions.windows(2).enumerate() {
        if index != 0 {
            let boundary = proposed.mix(endpoint, bounds[0]);
            if disabled_union_contrast(boundary, backgrounds) >= minimum {
                return Some((boundary, bounds[0]));
            }
        }
        let amount = if index == 0 {
            0.0
        } else {
            (bounds[0] + bounds[1]) / 2.0
        };
        let candidate = proposed.mix(endpoint, amount);
        if disabled_union_contrast(candidate, backgrounds) >= minimum {
            return Some((candidate, bounds[0]));
        }
    }
    (disabled_union_contrast(endpoint, backgrounds) >= minimum).then_some((endpoint, 1.0))
}

/// Searches every quantized color on the existing black and white adjustment paths.
///
/// Minimum contrast across opposing hosts is not monotonic, so endpoint-directed bisection can
/// jump over a feasible middle interval. The first result is the smallest authored-color movement.
fn resolve_disabled_union_color<const N: usize>(
    proposed: Color,
    backgrounds: [Color; N],
    minimum: f64,
) -> Option<Color> {
    if disabled_union_contrast(proposed, backgrounds) >= minimum {
        return Some(proposed);
    }
    let dark = Color::rgb(0x000000);
    let light = Color::rgb(0xffffff);
    let (preferred, alternate) = if disabled_union_contrast(dark, backgrounds)
        >= disabled_union_contrast(light, backgrounds)
    {
        (dark, light)
    } else {
        (light, dark)
    };
    let preferred = disabled_union_endpoint_candidate(proposed, preferred, backgrounds, minimum);
    let alternate = disabled_union_endpoint_candidate(proposed, alternate, backgrounds, minimum);
    match (preferred, alternate) {
        (Some((preferred, preferred_amount)), Some((alternate, alternate_amount))) => {
            Some(if preferred_amount <= alternate_amount {
                preferred
            } else {
                alternate
            })
        }
        (Some((preferred, _)), None) => Some(preferred),
        (None, Some((alternate, _))) => Some(alternate),
        (None, None) => None,
    }
}

fn resolve_disabled_union_content<const N: usize, const B: usize>(
    proposed: [(Color, f64); N],
    backgrounds: [Color; B],
) -> Option<[Color; N]> {
    let mut resolved = proposed.map(|(color, _)| color);
    for (index, (color, minimum)) in proposed.into_iter().enumerate() {
        resolved[index] = resolve_disabled_union_color(color, backgrounds, minimum)?;
    }
    Some(resolved)
}

fn shared_optional_disabled_boundary(
    active: Color,
    inactive: Color,
    hosts: [Color; 2],
    minimum: Option<f64>,
) -> Option<Color> {
    let Some(minimum) = minimum else {
        return Some(active);
    };
    match (active.a == 0, inactive.a == 0) {
        (true, true) => Some(active),
        (true, false) | (false, true) => None,
        (false, false) => {
            resolve_disabled_union_content([(active, minimum)], hosts).map(|[boundary]| boundary)
        }
    }
}

#[derive(Clone, Copy)]
struct DisabledUnionPolicy {
    hosts: [Color; 2],
    minimum: f64,
    boundary_minimum: Option<f64>,
}

fn shared_disabled_frame<const N: usize>(
    active_fill: Color,
    inactive_fill: Color,
    active_content: [Color; N],
    active_border: Color,
    inactive_border: Color,
    policy: DisabledUnionPolicy,
) -> Option<(Color, [Color; N], Color)> {
    let border = shared_optional_disabled_boundary(
        active_border,
        inactive_border,
        policy.hosts,
        policy.boundary_minimum,
    )?;
    let opaque_active = active_fill.source_over(policy.hosts[0]);
    let opaque_inactive = inactive_fill.source_over(policy.hosts[1]);
    for (index, fill) in [active_fill, inactive_fill, opaque_active, opaque_inactive]
        .into_iter()
        .enumerate()
    {
        if index >= 2 && (active_fill.a == 0 || inactive_fill.a == 0) {
            continue;
        }
        let backgrounds = policy.hosts.map(|host| fill.source_over(host));
        let proposed = active_content.map(|color| (color, policy.minimum));
        if let Some(content) = resolve_disabled_union_content(proposed, backgrounds) {
            return Some((fill, content, border));
        }
    }
    None
}

/// Canonicalizes each disabled family only when one paint remains readable on both activity hosts.
fn share_disabled_control_colors(
    active: &mut ChromeColors,
    inactive: &mut ChromeColors,
    hosts: [Color; 2],
    floors: FloatingContrastFloors,
    accessible_boundaries: bool,
) -> Vec<FloatingControlFamily> {
    let boundary = accessible_boundaries.then_some(floors.boundary);
    let policy = DisabledUnionPolicy {
        hosts,
        minimum: floors.disabled,
        boundary_minimum: boundary,
    };
    let mut fallbacks = Vec::new();

    let mut element_feasible = true;
    macro_rules! host_content {
        ($target:ident, $minimum:expr) => {
            if let Some([$target]) =
                resolve_disabled_union_content([(active.$target, $minimum)], hosts)
            {
                active.$target = $target;
                inactive.$target = $target;
            } else {
                element_feasible = false;
            }
        };
    }
    host_content!(text_disabled, floors.disabled);
    host_content!(icon_disabled, floors.disabled);
    macro_rules! element_boundary {
        ($target:ident) => {
            if let Some(value) =
                shared_optional_disabled_boundary(active.$target, inactive.$target, hosts, boundary)
            {
                active.$target = value;
                inactive.$target = value;
            } else {
                element_feasible = false;
            }
        };
    }
    element_boundary!(border_disabled);
    element_boundary!(resize_disabled);
    element_boundary!(outline_disabled_border);
    macro_rules! element_frame {
        ($fill:ident, [$($content:ident),+ $(,)?], $border:ident) => {
            if let Some((fill, [$($content),+], border)) = shared_disabled_frame(
                active.$fill,
                inactive.$fill,
                [$(active.$content),+],
                active.$border,
                inactive.$border,
                policy,
            ) {
                active.$fill = fill;
                inactive.$fill = fill;
                $(
                    active.$content = $content;
                    inactive.$content = $content;
                )+
                active.$border = border;
                inactive.$border = border;
            } else {
                element_feasible = false;
            }
        };
    }
    element_frame!(
        element_disabled,
        [element_disabled_foreground, element_disabled_icon],
        element_disabled_border
    );
    element_frame!(
        primary_disabled_background,
        [primary_disabled_foreground, primary_disabled_icon],
        primary_disabled_border
    );
    element_frame!(
        destructive_disabled_background,
        [destructive_disabled_foreground, destructive_disabled_icon],
        destructive_disabled_border
    );
    element_frame!(
        selection_disabled_background,
        [selection_disabled_foreground, selection_disabled_icon],
        selection_disabled_border
    );
    if !element_feasible {
        fallbacks.push(FloatingControlFamily::Element);
    }

    let ghost = shared_disabled_frame(
        active.ghost_element_disabled,
        inactive.ghost_element_disabled,
        [
            active.ghost_element_disabled_foreground,
            active.ghost_element_disabled_icon,
        ],
        active.ghost_element_disabled_border,
        inactive.ghost_element_disabled_border,
        policy,
    );
    let link =
        resolve_disabled_union_content([(active.link_text_disabled, floors.disabled)], hosts);
    if let (Some((fill, [foreground, icon], border)), Some([link])) = (ghost, link) {
        for colors in [&mut *active, &mut *inactive] {
            colors.ghost_element_disabled = fill;
            colors.ghost_element_disabled_foreground = foreground;
            colors.ghost_element_disabled_icon = icon;
            colors.ghost_element_disabled_border = border;
            colors.link_text_disabled = link;
        }
    } else {
        fallbacks.push(FloatingControlFamily::GhostElement);
    }

    let input = shared_disabled_frame(
        active.input_disabled_background,
        inactive.input_disabled_background,
        [active.input_disabled_text],
        active.input_disabled_border,
        inactive.input_disabled_border,
        policy,
    );
    if let Some((fill, [text], border)) = input {
        for colors in [&mut *active, &mut *inactive] {
            colors.input_disabled_background = fill;
            colors.input_disabled_text = text;
            colors.input_disabled_border = border;
        }
    } else {
        fallbacks.push(FloatingControlFamily::Input);
    }

    let mut toggle = active.clone();
    let mut toggle_feasible = true;
    macro_rules! toggle_state {
        ($fill:ident, $mark:ident, $label:ident, $border:ident) => {
            let frame = shared_disabled_frame(
                active.$fill,
                inactive.$fill,
                [active.$mark],
                active.$border,
                inactive.$border,
                policy,
            );
            let label = resolve_disabled_union_content([(active.$label, floors.disabled)], hosts);
            if let (Some((fill, [mark], border)), Some([label])) = (frame, label) {
                toggle.$fill = fill;
                toggle.$mark = mark;
                toggle.$label = label;
                toggle.$border = border;
            } else {
                toggle_feasible = false;
            }
        };
    }
    toggle_state!(
        toggle_off_disabled_background,
        toggle_off_disabled_mark,
        toggle_off_disabled_label,
        toggle_off_disabled_border
    );
    toggle_state!(
        toggle_on_disabled_background,
        toggle_on_disabled_mark,
        toggle_on_disabled_label,
        toggle_on_disabled_border
    );
    if toggle_feasible {
        for colors in [&mut *active, &mut *inactive] {
            colors.toggle_off_disabled_background = toggle.toggle_off_disabled_background;
            colors.toggle_off_disabled_mark = toggle.toggle_off_disabled_mark;
            colors.toggle_off_disabled_label = toggle.toggle_off_disabled_label;
            colors.toggle_off_disabled_border = toggle.toggle_off_disabled_border;
            colors.toggle_on_disabled_background = toggle.toggle_on_disabled_background;
            colors.toggle_on_disabled_mark = toggle.toggle_on_disabled_mark;
            colors.toggle_on_disabled_label = toggle.toggle_on_disabled_label;
            colors.toggle_on_disabled_border = toggle.toggle_on_disabled_border;
        }
    } else {
        fallbacks.push(FloatingControlFamily::Toggle);
    }

    fallbacks
}

/// Resolves disabled segment content against both variants' actual parent tracks.
/// A shared paint is installed only when it meets every final-background constraint.
fn share_disabled_segmented_colors(
    active: &mut ChromeColors,
    inactive: &mut ChromeColors,
    host_backgrounds: [Color; 2],
    minimum: f64,
    boundary: bool,
) -> bool {
    let active_tracks = host_backgrounds.map(|host| active.element_background.source_over(host));
    let inactive_tracks =
        host_backgrounds.map(|host| inactive.element_background.source_over(host));
    let tracks = [
        active_tracks[0],
        active_tracks[1],
        inactive_tracks[0],
        inactive_tracks[1],
    ];
    let selected_fill = active.selection_disabled_background;
    let selected_backgrounds = tracks.map(|track| selected_fill.source_over(track));
    let label = resolve_disabled_union_content([(active.text_disabled, minimum)], tracks);
    let selected = resolve_disabled_union_content(
        [
            (active.selection_disabled_foreground, minimum),
            (active.selection_disabled_icon, minimum),
        ],
        selected_backgrounds,
    );
    let borders = if boundary {
        resolve_disabled_union_content(
            [
                (active.selection_disabled_border, 3.0),
                (active.ghost_element_disabled_border, 3.0),
            ],
            tracks,
        )
    } else {
        Some([
            active.selection_disabled_border,
            active.ghost_element_disabled_border,
        ])
    };
    if let (Some([label]), Some([foreground, icon]), Some([selected_border, unselected_border])) =
        (label, selected, borders)
    {
        for colors in [active, inactive] {
            colors.text_disabled = label;
            colors.selection_disabled_background = selected_fill;
            colors.selection_disabled_foreground = foreground;
            colors.selection_disabled_icon = icon;
            colors.selection_disabled_border = selected_border;
            colors.ghost_element_disabled_border = unselected_border;
        }
        return true;
    }

    // Distinct parent tracks can require incompatible ink polarities. Retain readable variant
    // content instead of copying a color known to fail on the other track; report this fallback.
    for (colors, tracks) in [(active, active_tracks), (inactive, inactive_tracks)] {
        colors.text_disabled = readable_on_backgrounds(colors.text_disabled, tracks, minimum);
        let backgrounds =
            tracks.map(|track| colors.selection_disabled_background.source_over(track));
        colors.selection_disabled_foreground =
            readable_on_backgrounds(colors.selection_disabled_foreground, backgrounds, minimum);
        colors.selection_disabled_icon =
            readable_on_backgrounds(colors.selection_disabled_icon, backgrounds, minimum);
        if boundary {
            colors.selection_disabled_border =
                readable_on_backgrounds(colors.selection_disabled_border, tracks, 3.0);
            colors.ghost_element_disabled_border =
                readable_on_backgrounds(colors.ghost_element_disabled_border, tracks, 3.0);
        }
    }
    false
}

pub(super) fn reconcile(active: &mut ChromeAppearance, inactive: &mut ChromeAppearance) {
    macro_rules! preserve { ($($field:ident),+ $(,)?) => { $(preserve_disabled_colors(&mut inactive.$field, &active.$field);)+ }; }
    preserve!(
        colors,
        control_colors,
        segmented_control_colors,
        floating_colors,
        floating_control_colors,
        floating_segmented_colors,
        floating_field_reference,
        floating_field_colors
    );
    for (inactive, active) in [
        (&mut inactive.panel_controls, &active.panel_controls),
        (&mut inactive.card_controls, &active.card_controls),
    ] {
        preserve_disabled_colors(&mut inactive.reference, &active.reference);
        preserve_disabled_colors(&mut inactive.colors, &active.colors);
        preserve_disabled_colors(&mut inactive.segmented, &active.segmented);
    }
    let minimum = if active.capabilities.increase_contrast {
        4.5
    } else {
        3.0
    };
    let boundary = active.capabilities.increase_contrast || active.capabilities.show_borders;
    let host = active.colors.background;
    let window_host = active.control_host_background(spaceterm_ui::ControlHost::Window);
    let title_bar_hosts = [
        active.control_host_background(spaceterm_ui::ControlHost::TitleBar),
        inactive.control_host_background(spaceterm_ui::ControlHost::TitleBar),
    ];
    let panel_host = active.control_host_background(spaceterm_ui::ControlHost::Panel);
    let card_host = active.control_host_background(spaceterm_ui::ControlHost::Card);
    let title_bar_fallbacks = share_disabled_control_colors(
        &mut active.title_bar_controls.colors,
        &mut inactive.title_bar_controls.colors,
        title_bar_hosts,
        FloatingContrastFloors {
            interactive: false,
            ..if active.capabilities.increase_contrast {
                FloatingContrastFloors::INCREASED
            } else {
                FloatingContrastFloors::STANDARD
            }
        },
        boundary,
    );
    for family in title_bar_fallbacks {
        for fallbacks in [
            &mut active.title_bar_controls.fallback_families,
            &mut inactive.title_bar_controls.fallback_families,
        ] {
            if !fallbacks.contains(&family) {
                fallbacks.push(family);
            }
        }
    }
    let (material, wash) = state_floating_material(
        active.appearance,
        active.floating_materials,
        host,
        active.colors.elevated_surface_background,
        active.capabilities.increase_contrast,
    );
    let floating_hosts = [Color::rgb(0), Color::rgb(0xffffff)]
        .map(|underlay| wash.source_over(material.source_over(underlay)));
    for (active_colors, inactive_colors, hosts, active_fallbacks, inactive_fallbacks) in [
        (
            &mut active.segmented_control_colors,
            &mut inactive.segmented_control_colors,
            [window_host; 2],
            &mut active.window_control_fallbacks,
            &mut inactive.window_control_fallbacks,
        ),
        (
            &mut active.title_bar_controls.segmented,
            &mut inactive.title_bar_controls.segmented,
            title_bar_hosts,
            &mut active.title_bar_controls.fallback_families,
            &mut inactive.title_bar_controls.fallback_families,
        ),
        (
            &mut active.panel_controls.segmented,
            &mut inactive.panel_controls.segmented,
            [panel_host; 2],
            &mut active.panel_controls.fallback_families,
            &mut inactive.panel_controls.fallback_families,
        ),
        (
            &mut active.card_controls.segmented,
            &mut inactive.card_controls.segmented,
            [card_host; 2],
            &mut active.card_controls.fallback_families,
            &mut inactive.card_controls.fallback_families,
        ),
        (
            &mut active.floating_segmented_colors,
            &mut inactive.floating_segmented_colors,
            floating_hosts,
            &mut active.floating_fallbacks,
            &mut inactive.floating_fallbacks,
        ),
    ] {
        if !share_disabled_segmented_colors(
            active_colors,
            inactive_colors,
            hosts,
            minimum,
            boundary,
        ) {
            for fallbacks in [active_fallbacks, inactive_fallbacks] {
                if !fallbacks.contains(&FloatingControlFamily::Segmented) {
                    fallbacks.push(FloatingControlFamily::Segmented);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_activity_tracks_keep_readable_disabled_ink_and_report_fallback() {
        let mut active = ChromeColors {
            element_background: Color::rgb(0x999999),
            selection_disabled_background: Color::rgb(0xffffff),
            ..ChromeColors::default()
        };
        let mut inactive = ChromeColors {
            element_background: Color::rgb(0x585858),
            ..active.clone()
        };
        assert!(!share_disabled_segmented_colors(
            &mut active,
            &mut inactive,
            [Color::rgb(0); 2],
            4.5,
            true,
        ));
        for colors in [active, inactive] {
            let track = colors.element_background;
            assert!(
                colors
                    .text_disabled
                    .source_over(track)
                    .contrast_ratio(track)
                    >= 4.5
            );
            assert!(
                colors
                    .selection_disabled_border
                    .source_over(track)
                    .contrast_ratio(track)
                    >= 3.0
            );
        }
    }

    #[test]
    fn incompatible_title_bar_disabled_family_keeps_readable_variants_and_reports_fallback() {
        let active_host = Color::rgb(0x595959);
        let inactive_host = Color::rgb(0x949494);
        let mut active = ChromeColors {
            ghost_element_disabled: Color::rgba(0),
            ghost_element_disabled_foreground: Color::rgb(0xffffff),
            ghost_element_disabled_icon: Color::rgb(0xffffff),
            ghost_element_disabled_border: Color::rgb(0xffffff),
            link_text_disabled: Color::rgb(0xffffff),
            ..ChromeColors::default()
        };
        let mut inactive = ChromeColors {
            ghost_element_disabled_foreground: Color::rgb(0x000000),
            ghost_element_disabled_icon: Color::rgb(0x000000),
            ghost_element_disabled_border: Color::rgb(0x000000),
            link_text_disabled: Color::rgb(0x000000),
            ..active.clone()
        };

        let fallbacks = share_disabled_control_colors(
            &mut active,
            &mut inactive,
            [active_host, inactive_host],
            FloatingContrastFloors::INCREASED,
            true,
        );

        assert!(fallbacks.contains(&FloatingControlFamily::GhostElement));
        assert_ne!(
            active.ghost_element_disabled_foreground,
            inactive.ghost_element_disabled_foreground
        );
        for (colors, host) in [(active, active_host), (inactive, inactive_host)] {
            let background = colors.ghost_element_disabled.source_over(host);
            for content in [
                colors.ghost_element_disabled_foreground,
                colors.ghost_element_disabled_icon,
                colors.link_text_disabled,
            ] {
                assert!(
                    content.source_over(background).contrast_ratio(background) >= 4.5,
                    "fallback content must remain readable on its own activity host"
                );
            }
            assert!(
                colors
                    .ghost_element_disabled_border
                    .source_over(host)
                    .contrast_ratio(host)
                    >= 3.0,
                "fallback boundary must remain visible on its own activity host"
            );
        }
    }

    #[test]
    fn disabled_union_finds_a_feasible_middle_tone_when_both_endpoints_fail() {
        let backgrounds = [Color::rgb(0x000000), Color::rgb(0xffffff)];

        let resolved = resolve_disabled_union_content([(Color::rgb(0xffffff), 4.5)], backgrounds);

        let [resolved] = resolved.expect("a middle gray satisfies both endpoint backgrounds");
        for background in backgrounds {
            assert!(resolved.source_over(background).contrast_ratio(background) >= 4.5);
        }
    }
}
