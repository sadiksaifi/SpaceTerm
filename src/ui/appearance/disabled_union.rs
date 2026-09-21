use super::{
    ChromeAppearance, DisabledControlDiagnostic, FloatingContrastFloors, FloatingControlFamily,
    readable_on_backgrounds, relative_luminance, state_floating_material,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisabledPolarity {
    Dark,
    Light,
}

impl DisabledPolarity {
    fn from_pair(content: Color, background: Color) -> Self {
        if relative_luminance(content.source_over(background)) > relative_luminance(background) {
            Self::Light
        } else {
            Self::Dark
        }
    }

    fn holds(self, content: Color, background: Color) -> bool {
        match self {
            Self::Dark => relative_luminance(content) <= relative_luminance(background),
            Self::Light => relative_luminance(content) >= relative_luminance(background),
        }
    }

    const fn endpoint(self) -> Color {
        match self {
            Self::Dark => Color::rgb(0x000000),
            Self::Light => Color::rgb(0xffffff),
        }
    }
}

fn gray(channel: u8) -> Color {
    Color::from_rgb_components(channel, channel, channel)
}

fn nearest_gray(target_luminance: f64) -> Color {
    (0..=u8::MAX)
        .map(gray)
        .min_by(|left, right| {
            (relative_luminance(*left) - target_luminance)
                .abs()
                .total_cmp(&(relative_luminance(*right) - target_luminance).abs())
        })
        .unwrap_or(Color::rgb(0x000000))
}

#[derive(Clone, Copy)]
struct DisabledFrameResolution {
    fill: Color,
    content: Color,
    floor_unmet: bool,
    separation_unmet: bool,
}

#[derive(Clone, Copy)]
struct DisabledTextPolicy {
    fill_limits: DisabledFillLimits,
    host_side_content: Color,
    host_side_polarity: DisabledPolarity,
}

#[derive(Clone, Copy)]
struct DisabledFillLimits {
    minimum_contrast: f64,
    maximum_contrast: f64,
    direction: Option<DisabledPolarity>,
}

#[derive(Clone, Copy)]
struct EnabledFrame {
    fill: Color,
    content: Color,
}

fn fill_weight_cap<const N: usize>(ordinary_fill: Color, hosts: [Color; N]) -> f64 {
    hosts
        .into_iter()
        .map(|host| ordinary_fill.source_over(host).contrast_ratio(host))
        .fold(f64::INFINITY, f64::min)
}

#[cfg(test)]
fn paired_fill_weight_cap(active_fill: Color, inactive_fill: Color, hosts: [Color; 2]) -> f64 {
    [active_fill, inactive_fill]
        .into_iter()
        .zip(hosts)
        .map(|(fill, host)| fill.source_over(host).contrast_ratio(host))
        .fold(f64::INFINITY, f64::min)
}

fn fill_is_within_limits<const N: usize>(
    fill: Color,
    hosts: [Color; N],
    limits: DisabledFillLimits,
) -> bool {
    hosts.into_iter().all(|host| {
        let rendered = fill.source_over(host);
        let contrast = rendered.contrast_ratio(host);
        contrast >= limits.minimum_contrast
            && contrast <= limits.maximum_contrast + 1e-9
            && limits
                .direction
                .is_none_or(|direction| direction.holds(rendered, host))
    })
}

fn disabled_candidate_meets<const N: usize>(
    candidate: Color,
    backgrounds: [Color; N],
    enabled_contrast: [f64; N],
    polarity: DisabledPolarity,
    minimum: f64,
    separation_divisor: f64,
) -> bool {
    backgrounds
        .into_iter()
        .zip(enabled_contrast)
        .all(|(background, enabled)| {
            let contrast = candidate.contrast_ratio(background);
            polarity.holds(candidate, background)
                && contrast >= minimum
                && contrast <= enabled / separation_divisor + 1e-9
        })
}

fn closest_disabled_candidate<const N: usize>(
    seed: Color,
    backgrounds: [Color; N],
    enabled_contrast: [f64; N],
    polarity: DisabledPolarity,
    minimum: f64,
    separation_divisor: f64,
    range: std::ops::RangeInclusive<u8>,
) -> Option<Color> {
    let seed_luminance = relative_luminance(seed);
    range
        .map(gray)
        .filter(|candidate| {
            disabled_candidate_meets(
                *candidate,
                backgrounds,
                enabled_contrast,
                polarity,
                minimum,
                separation_divisor,
            )
        })
        .min_by(|left, right| {
            (relative_luminance(*left) - seed_luminance)
                .abs()
                .total_cmp(&(relative_luminance(*right) - seed_luminance).abs())
        })
}

/// Derives one achromatic disabled ink from the final active enabled presentation.
///
/// The authored ordinary pair contributes only its disabled depth. The active enabled pair owns
/// polarity, and the prepared disabled fill may move only after the neutral axis is exhausted.
fn resolve_disabled_frame<const N: usize>(
    authored: &ChromeColors,
    enabled_fill: Color,
    enabled_content: Color,
    disabled_fill: Color,
    hosts: [Color; N],
    minimum: f64,
    text_policy: Option<DisabledTextPolicy>,
) -> DisabledFrameResolution {
    let enabled_backgrounds = hosts.map(|host| enabled_fill.source_over(host));
    let disabled_backgrounds = hosts.map(|host| disabled_fill.source_over(host));
    let enabled_rendered = enabled_content.source_over(enabled_backgrounds[0]);
    let enabled_polarity = if enabled_rendered.contrast_ratio(enabled_backgrounds[0]) < 1.05 {
        let ordinary_fill = authored
            .element_disabled
            .source_over(authored.background.with_alpha(255));
        DisabledPolarity::from_pair(authored.element_disabled_foreground, ordinary_fill)
    } else {
        DisabledPolarity::from_pair(enabled_content, enabled_backgrounds[0])
    };
    let enabled_contrast = enabled_backgrounds.map(|background| {
        enabled_content
            .source_over(background)
            .contrast_ratio(background)
    });

    let definition_background = authored.background.with_alpha(255);
    let definition_text = authored.text.source_over(definition_background);
    let (dark_end, light_end) =
        if relative_luminance(definition_background) <= relative_luminance(definition_text) {
            (definition_background, definition_text)
        } else {
            (definition_text, definition_background)
        };
    let ordinary_fill = authored.element_disabled.source_over(definition_background);
    let ordinary_content = authored
        .element_disabled_foreground
        .source_over(ordinary_fill);
    let ordinary_polarity = DisabledPolarity::from_pair(ordinary_content, ordinary_fill);
    let ordinary_end = match ordinary_polarity {
        DisabledPolarity::Dark => dark_end,
        DisabledPolarity::Light => light_end,
    };
    let denominator = relative_luminance(ordinary_end) - relative_luminance(ordinary_fill);
    let depth = if denominator.abs() < 0.005 {
        0.5
    } else {
        ((relative_luminance(ordinary_end) - relative_luminance(ordinary_content)) / denominator)
            .clamp(0.0, 1.0)
    };
    let disabled_luminance = disabled_backgrounds
        .into_iter()
        .map(relative_luminance)
        .sum::<f64>()
        / N as f64;
    let polarity_end = match enabled_polarity {
        DisabledPolarity::Dark => dark_end,
        DisabledPolarity::Light => light_end,
    };
    let seed_luminance = relative_luminance(polarity_end)
        + (disabled_luminance - relative_luminance(polarity_end)) * depth;
    let seed = nearest_gray(seed_luminance);
    let scheme_range = {
        let dark = nearest_gray(relative_luminance(dark_end)).r;
        let light = nearest_gray(relative_luminance(light_end)).r;
        dark.min(light)..=dark.max(light)
    };

    let original_fill_allowed = text_policy
        .is_none_or(|policy| fill_is_within_limits(disabled_fill, hosts, policy.fill_limits));
    for range in [scheme_range.clone(), 0..=u8::MAX] {
        if let Some(content) = closest_disabled_candidate(
            seed,
            disabled_backgrounds,
            enabled_contrast,
            enabled_polarity,
            minimum,
            1.3,
            range,
        ) && original_fill_allowed
        {
            return DisabledFrameResolution {
                fill: disabled_fill,
                content,
                floor_unmet: false,
                separation_unmet: false,
            };
        }
    }
    for range in [scheme_range, 0..=u8::MAX] {
        if let Some(content) = closest_disabled_candidate(
            seed,
            disabled_backgrounds,
            enabled_contrast,
            enabled_polarity,
            minimum,
            1.0,
            range,
        ) && original_fill_allowed
        {
            return DisabledFrameResolution {
                fill: disabled_fill,
                content,
                floor_unmet: false,
                separation_unmet: true,
            };
        }
    }

    let content = enabled_polarity.endpoint();
    let current_fill_luminance = disabled_luminance;
    let adjusted_fill = |separation_divisor| {
        (0..=u8::MAX)
            .map(gray)
            .filter(|fill| {
                let within_cap = text_policy
                    .is_none_or(|policy| fill_is_within_limits(*fill, hosts, policy.fill_limits));
                let contrast = content.contrast_ratio(*fill);
                within_cap
                    && enabled_polarity.holds(content, *fill)
                    && contrast >= minimum
                    && enabled_contrast
                        .into_iter()
                        .all(|enabled| contrast <= enabled / separation_divisor + 1e-9)
            })
            .min_by(|left, right| {
                (relative_luminance(*left) - current_fill_luminance)
                    .abs()
                    .total_cmp(&(relative_luminance(*right) - current_fill_luminance).abs())
            })
    };
    if let Some(fill) = adjusted_fill(1.3) {
        return DisabledFrameResolution {
            fill,
            content,
            floor_unmet: false,
            separation_unmet: false,
        };
    }
    if let Some(fill) = adjusted_fill(1.0) {
        return DisabledFrameResolution {
            fill,
            content,
            floor_unmet: false,
            separation_unmet: true,
        };
    }
    if let Some(policy) = text_policy {
        let seed = nearest_gray(relative_luminance(policy.host_side_content));
        let resolve_for_fill = |fill: Color, separation_divisor| {
            if !fill_is_within_limits(fill, hosts, policy.fill_limits) {
                return None;
            }
            let backgrounds = hosts.map(|host| fill.source_over(host));
            closest_disabled_candidate(
                seed,
                backgrounds,
                enabled_contrast,
                policy.host_side_polarity,
                minimum,
                separation_divisor,
                0..=u8::MAX,
            )
            .map(|content| (fill, content))
        };
        for separation_divisor in [1.3, 1.0] {
            let candidate = std::iter::once(disabled_fill)
                .chain(std::iter::once(Color::rgba(0)))
                .chain((0..=u8::MAX).map(gray))
                .filter_map(|fill| resolve_for_fill(fill, separation_divisor))
                .min_by(|(left, _), (right, _)| {
                    (relative_luminance(*left) - current_fill_luminance)
                        .abs()
                        .total_cmp(&(relative_luminance(*right) - current_fill_luminance).abs())
                });
            if let Some((fill, content)) = candidate {
                return DisabledFrameResolution {
                    fill,
                    content,
                    floor_unmet: false,
                    separation_unmet: separation_divisor == 1.0,
                };
            }
        }
        let floor_only = std::iter::once(disabled_fill)
            .chain(std::iter::once(Color::rgba(0)))
            .chain((0..=u8::MAX).map(gray))
            .filter(|fill| fill_is_within_limits(*fill, hosts, policy.fill_limits))
            .filter_map(|fill| {
                let backgrounds = hosts.map(|host| fill.source_over(host));
                (0..=u8::MAX)
                    .map(gray)
                    .filter(|content| {
                        backgrounds.into_iter().all(|background| {
                            policy.host_side_polarity.holds(*content, background)
                                && content.contrast_ratio(background) >= minimum
                        })
                    })
                    .min_by(|left, right| {
                        (relative_luminance(*left) - relative_luminance(seed))
                            .abs()
                            .total_cmp(
                                &(relative_luminance(*right) - relative_luminance(seed)).abs(),
                            )
                    })
                    .map(|content| (fill, content))
            })
            .min_by(|(left, _), (right, _)| {
                (relative_luminance(*left) - current_fill_luminance)
                    .abs()
                    .total_cmp(&(relative_luminance(*right) - current_fill_luminance).abs())
            });
        if let Some((fill, content)) = floor_only {
            return DisabledFrameResolution {
                fill,
                content,
                floor_unmet: false,
                separation_unmet: true,
            };
        }
    }
    if let Some(fill) = (0..=u8::MAX)
        .map(gray)
        .filter(|fill| {
            text_policy.is_none_or(|policy| fill_is_within_limits(*fill, hosts, policy.fill_limits))
                && enabled_polarity.holds(content, *fill)
                && content.contrast_ratio(*fill) >= minimum
        })
        .min_by(|left, right| {
            (relative_luminance(*left) - current_fill_luminance)
                .abs()
                .total_cmp(&(relative_luminance(*right) - current_fill_luminance).abs())
        })
    {
        return DisabledFrameResolution {
            fill,
            content,
            floor_unmet: false,
            separation_unmet: true,
        };
    }
    DisabledFrameResolution {
        fill: disabled_fill,
        content,
        floor_unmet: true,
        separation_unmet: true,
    }
}

fn push_diagnostic(
    diagnostics: &mut Vec<DisabledControlDiagnostic>,
    diagnostic: DisabledControlDiagnostic,
) {
    if !diagnostics.contains(&diagnostic) {
        diagnostics.push(diagnostic);
    }
}

fn prepare_disabled_semantic_content(
    authored: &ChromeColors,
    colors: &mut ChromeColors,
    hosts: [Color; 2],
    minimum: f64,
) -> Vec<DisabledControlDiagnostic> {
    let mut diagnostics = Vec::new();
    macro_rules! frame {
        ($family:expr, $enabled_fill:ident, $enabled_content:ident, $disabled_fill:ident, [$($disabled_content:ident),+ $(,)?], $text_policy:expr) => {{
            let resolved = resolve_disabled_frame(
                authored,
                colors.$enabled_fill,
                colors.$enabled_content,
                colors.$disabled_fill,
                hosts,
                minimum,
                $text_policy,
            );
            colors.$disabled_fill = resolved.fill;
            $(colors.$disabled_content = resolved.content;)+
            if resolved.floor_unmet {
                push_diagnostic(
                    &mut diagnostics,
                    DisabledControlDiagnostic::ContrastFloor { family: $family },
                );
            }
            if resolved.separation_unmet {
                push_diagnostic(
                    &mut diagnostics,
                    DisabledControlDiagnostic::Separation { family: $family },
                );
            }
        }};
    }
    let ordinary_cap = fill_weight_cap(colors.element_background, hosts);
    let authored_ordinary_fill = authored
        .element_disabled
        .source_over(authored.background.with_alpha(255));
    let ordinary_policy = DisabledTextPolicy {
        fill_limits: DisabledFillLimits {
            minimum_contrast: 1.0,
            maximum_contrast: ordinary_cap,
            direction: None,
        },
        host_side_content: authored.element_disabled_foreground,
        host_side_polarity: DisabledPolarity::from_pair(
            authored.element_disabled_foreground,
            authored_ordinary_fill,
        ),
    };
    frame!(
        FloatingControlFamily::Element,
        element_background,
        element_foreground,
        element_disabled,
        [element_disabled_foreground, element_disabled_icon],
        Some(ordinary_policy)
    );
    colors.primary_disabled_background = colors.element_disabled;
    colors.destructive_disabled_background = colors.element_disabled;
    colors.selection_disabled_background = colors.element_disabled;
    let ordinary_disabled_background = colors.element_disabled.source_over(hosts[0]);
    let semantic_text_policy = DisabledTextPolicy {
        fill_limits: DisabledFillLimits {
            minimum_contrast: 1.0,
            maximum_contrast: ordinary_cap,
            direction: None,
        },
        host_side_content: colors.element_disabled_foreground,
        host_side_polarity: DisabledPolarity::from_pair(
            colors.element_disabled_foreground,
            ordinary_disabled_background,
        ),
    };
    frame!(
        FloatingControlFamily::Element,
        primary_background,
        primary_foreground,
        primary_disabled_background,
        [primary_disabled_foreground, primary_disabled_icon],
        Some(semantic_text_policy)
    );
    frame!(
        FloatingControlFamily::Element,
        destructive_background,
        destructive_foreground,
        destructive_disabled_background,
        [destructive_disabled_foreground, destructive_disabled_icon],
        Some(semantic_text_policy)
    );
    frame!(
        FloatingControlFamily::Element,
        selection_background,
        selection_foreground,
        selection_disabled_background,
        [selection_disabled_foreground, selection_disabled_icon],
        Some(semantic_text_policy)
    );
    frame!(
        FloatingControlFamily::Toggle,
        toggle_off_background,
        toggle_off_mark,
        toggle_off_disabled_background,
        [toggle_off_disabled_mark],
        None
    );
    frame!(
        FloatingControlFamily::Toggle,
        toggle_on_background,
        toggle_on_mark,
        toggle_on_disabled_background,
        [toggle_on_disabled_mark],
        None
    );
    diagnostics
}

#[derive(Clone, Copy)]
struct DisabledDiagnosticFrame<const C: usize> {
    fill: Color,
    content: [Color; C],
}

fn collect_disabled_frame_diagnostics<const N: usize, const C: usize>(
    family: FloatingControlFamily,
    enabled: DisabledDiagnosticFrame<C>,
    disabled: DisabledDiagnosticFrame<C>,
    enabled_hosts: [Color; N],
    disabled_hosts: [Color; N],
    minimum: f64,
    diagnostics: &mut Vec<DisabledControlDiagnostic>,
) {
    let mut floor_unmet = false;
    let mut separation_unmet = false;
    for (enabled_host, disabled_host) in enabled_hosts.into_iter().zip(disabled_hosts) {
        let enabled_background = enabled.fill.source_over(enabled_host);
        let disabled_background = disabled.fill.source_over(disabled_host);
        for (enabled_content, disabled_content) in enabled.content.into_iter().zip(disabled.content)
        {
            let enabled_contrast = enabled_content
                .source_over(enabled_background)
                .contrast_ratio(enabled_background);
            let disabled_contrast = disabled_content
                .source_over(disabled_background)
                .contrast_ratio(disabled_background);
            floor_unmet |= disabled_contrast < minimum;
            separation_unmet |= disabled_contrast > enabled_contrast / 1.3 + 1e-9;
        }
    }
    if floor_unmet {
        push_diagnostic(
            diagnostics,
            DisabledControlDiagnostic::ContrastFloor { family },
        );
    }
    if separation_unmet {
        push_diagnostic(
            diagnostics,
            DisabledControlDiagnostic::Separation { family },
        );
    }
}

/// Records diagnostics from the final paints after activity union and host composition.
fn collect_disabled_control_diagnostics(
    enabled: &ChromeColors,
    disabled: &ChromeColors,
    hosts: [Color; 2],
    minimum: f64,
    diagnostics: &mut Vec<DisabledControlDiagnostic>,
) {
    macro_rules! frame {
        ($family:expr, $enabled_fill:ident, [$($enabled_content:ident),+ $(,)?], $disabled_fill:ident, [$($disabled_content:ident),+ $(,)?]) => {
            collect_disabled_frame_diagnostics(
                $family,
                DisabledDiagnosticFrame {
                    fill: enabled.$enabled_fill,
                    content: [$(enabled.$enabled_content),+],
                },
                DisabledDiagnosticFrame {
                    fill: disabled.$disabled_fill,
                    content: [$(disabled.$disabled_content),+],
                },
                hosts,
                hosts,
                minimum,
                diagnostics,
            );
        };
    }
    frame!(
        FloatingControlFamily::Element,
        element_background,
        [element_foreground, element_icon],
        element_disabled,
        [element_disabled_foreground, element_disabled_icon]
    );
    frame!(
        FloatingControlFamily::Element,
        primary_background,
        [primary_foreground, primary_icon],
        primary_disabled_background,
        [primary_disabled_foreground, primary_disabled_icon]
    );
    frame!(
        FloatingControlFamily::Element,
        destructive_background,
        [destructive_foreground, destructive_icon],
        destructive_disabled_background,
        [destructive_disabled_foreground, destructive_disabled_icon]
    );
    frame!(
        FloatingControlFamily::Element,
        selection_background,
        [selection_foreground, selection_icon],
        selection_disabled_background,
        [selection_disabled_foreground, selection_disabled_icon]
    );
    frame!(
        FloatingControlFamily::GhostElement,
        ghost_element_background,
        [ghost_element_foreground, ghost_element_icon],
        ghost_element_disabled,
        [
            ghost_element_disabled_foreground,
            ghost_element_disabled_icon
        ]
    );
    frame!(
        FloatingControlFamily::Toggle,
        toggle_off_background,
        [toggle_off_mark],
        toggle_off_disabled_background,
        [toggle_off_disabled_mark]
    );
    frame!(
        FloatingControlFamily::Toggle,
        toggle_on_background,
        [toggle_on_mark],
        toggle_on_disabled_background,
        [toggle_on_disabled_mark]
    );
    frame!(
        FloatingControlFamily::Input,
        input_background,
        [input_text],
        input_disabled_background,
        [input_disabled_text]
    );
}

fn collect_disabled_segmented_diagnostics(
    enabled: &ChromeColors,
    disabled: &ChromeColors,
    hosts: [Color; 2],
    minimum: f64,
    diagnostics: &mut Vec<DisabledControlDiagnostic>,
) {
    for (enabled_track, disabled_track) in hosts
        .map(|host| enabled.element_background.source_over(host))
        .into_iter()
        .zip(hosts.map(|host| disabled.element_background.source_over(host)))
    {
        collect_disabled_frame_diagnostics(
            FloatingControlFamily::Segmented,
            DisabledDiagnosticFrame {
                fill: enabled.selection_background,
                content: [enabled.selection_foreground, enabled.selection_icon],
            },
            DisabledDiagnosticFrame {
                fill: disabled.selection_disabled_background,
                content: [
                    disabled.selection_disabled_foreground,
                    disabled.selection_disabled_icon,
                ],
            },
            [enabled_track],
            [disabled_track],
            minimum,
            diagnostics,
        );
    }
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

fn shared_optional_disabled_boundary<const N: usize>(
    active: Color,
    inactive: Color,
    hosts: [Color; N],
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
    hosts: [Color; 4],
    active_reference_host: Color,
    inactive_reference_host: Color,
    minimum: f64,
    boundary_minimum: Option<f64>,
}

#[derive(Clone, Copy)]
struct DisabledActivityHosts {
    active: [Color; 2],
    inactive: [Color; 2],
}

impl DisabledActivityHosts {
    const fn shared(hosts: [Color; 2]) -> Self {
        Self {
            active: hosts,
            inactive: hosts,
        }
    }

    const fn paired(active: Color, inactive: Color) -> Self {
        Self {
            active: [active; 2],
            inactive: [inactive; 2],
        }
    }

    const fn union(self) -> [Color; 4] {
        [
            self.active[0],
            self.active[1],
            self.inactive[0],
            self.inactive[1],
        ]
    }
}

fn shared_disabled_frame<const N: usize>(
    active_fill: Color,
    inactive_fill: Color,
    active_content: [Color; N],
    active_border: Color,
    inactive_border: Color,
    policy: DisabledUnionPolicy,
    fill_limits: Option<DisabledFillLimits>,
) -> Option<(Color, [Color; N], Color)> {
    let border = shared_optional_disabled_boundary(
        active_border,
        inactive_border,
        policy.hosts,
        policy.boundary_minimum,
    )?;
    let opaque_active = active_fill.source_over(policy.active_reference_host);
    let opaque_inactive = inactive_fill.source_over(policy.inactive_reference_host);
    for (index, fill) in [active_fill, inactive_fill, opaque_active, opaque_inactive]
        .into_iter()
        .enumerate()
    {
        if index >= 2 && (active_fill.a == 0 || inactive_fill.a == 0) {
            continue;
        }
        if fill_limits.is_some_and(|limits| !fill_is_within_limits(fill, policy.hosts, limits)) {
            continue;
        }
        let backgrounds = policy.hosts.map(|host| fill.source_over(host));
        let proposed = active_content.map(|color| (color, policy.minimum));
        if let Some(content) = resolve_disabled_union_content(proposed, backgrounds) {
            return Some((fill, content, border));
        }
    }
    if let Some(limits) = fill_limits {
        let active_luminance = relative_luminance(active_fill.source_over(policy.hosts[0]));
        return (0..=u8::MAX)
            .map(gray)
            .filter(|fill| fill_is_within_limits(*fill, policy.hosts, limits))
            .filter_map(|fill| {
                let backgrounds = policy.hosts.map(|host| fill.source_over(host));
                let proposed = active_content.map(|color| (color, policy.minimum));
                resolve_disabled_union_content(proposed, backgrounds)
                    .map(|content| (fill, content, border))
            })
            .min_by(|(left, _, _), (right, _, _)| {
                (relative_luminance(*left) - active_luminance)
                    .abs()
                    .total_cmp(&(relative_luminance(*right) - active_luminance).abs())
            });
    }
    None
}

/// Canonicalizes each disabled family only when one paint remains readable on both activity hosts.
fn share_disabled_control_colors(
    authored: &ChromeColors,
    active: &mut ChromeColors,
    inactive: &mut ChromeColors,
    activity_hosts: DisabledActivityHosts,
    floors: FloatingContrastFloors,
    accessible_boundaries: bool,
    diagnostics: &mut Vec<DisabledControlDiagnostic>,
) -> Vec<FloatingControlFamily> {
    let hosts = activity_hosts.union();
    let boundary = accessible_boundaries.then_some(floors.boundary);
    let policy = DisabledUnionPolicy {
        hosts,
        active_reference_host: activity_hosts.active[0],
        inactive_reference_host: activity_hosts.inactive[0],
        minimum: floors.disabled,
        boundary_minimum: boundary,
    };
    let mut fallbacks = Vec::new();
    let _active_diagnostics =
        prepare_disabled_semantic_content(authored, active, activity_hosts.active, floors.disabled);
    let _inactive_diagnostics = prepare_disabled_semantic_content(
        authored,
        inactive,
        activity_hosts.inactive,
        floors.disabled,
    );
    let shared_text_cap = fill_weight_cap(active.element_background, activity_hosts.active).min(
        fill_weight_cap(inactive.element_background, activity_hosts.inactive),
    );
    let shared_text_limits = DisabledFillLimits {
        minimum_contrast: 1.0,
        maximum_contrast: shared_text_cap,
        direction: None,
    };

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
                push_diagnostic(
                    diagnostics,
                    DisabledControlDiagnostic::SharedPaint {
                        family: FloatingControlFamily::Element,
                    },
                );
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
                Some(shared_text_limits),
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
        push_diagnostic(
            diagnostics,
            DisabledControlDiagnostic::SharedPaint {
                family: FloatingControlFamily::Element,
            },
        );
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
        Some(shared_text_limits),
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
        push_diagnostic(
            diagnostics,
            DisabledControlDiagnostic::SharedPaint {
                family: FloatingControlFamily::GhostElement,
            },
        );
    }

    let input = shared_disabled_frame(
        active.input_disabled_background,
        inactive.input_disabled_background,
        [active.input_disabled_text],
        active.input_disabled_border,
        inactive.input_disabled_border,
        policy,
        None,
    );
    if let Some((fill, [text], border)) = input {
        for colors in [&mut *active, &mut *inactive] {
            colors.input_disabled_background = fill;
            colors.input_disabled_text = text;
            colors.input_disabled_border = border;
        }
    } else {
        fallbacks.push(FloatingControlFamily::Input);
        for (colors, hosts) in [
            (&mut *active, activity_hosts.active),
            (&mut *inactive, activity_hosts.inactive),
        ] {
            let backgrounds = hosts.map(|host| colors.input_disabled_background.source_over(host));
            colors.input_disabled_text =
                readable_on_backgrounds(colors.input_disabled_text, backgrounds, floors.disabled);
            if let Some(boundary) = boundary {
                colors.input_disabled_border =
                    readable_on_backgrounds(colors.input_disabled_border, hosts, boundary);
            }
        }
        push_diagnostic(
            diagnostics,
            DisabledControlDiagnostic::SharedPaint {
                family: FloatingControlFamily::Input,
            },
        );
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
                None,
            );
            let label =
                resolve_disabled_union_content([(active.text_disabled, floors.disabled)], hosts);
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
        push_diagnostic(
            diagnostics,
            DisabledControlDiagnostic::SharedPaint {
                family: FloatingControlFamily::Toggle,
            },
        );
    }

    fallbacks
}

/// Resolves disabled segment content against both variants' actual parent tracks.
/// A shared paint is installed only when it meets every final-background constraint.
fn minimum_selected_fill<const N: usize>(
    tracks: [Color; N],
    directions: [DisabledPolarity; N],
    cap: f64,
) -> Option<Color> {
    (0..=u8::MAX)
        .map(gray)
        .filter(|fill| {
            tracks
                .into_iter()
                .zip(directions)
                .all(|(track, direction)| {
                    let contrast = fill.contrast_ratio(track);
                    direction.holds(*fill, track) && contrast >= 1.12 && contrast <= cap + 1e-9
                })
        })
        .min_by(|left, right| {
            let strength = |fill: Color| {
                tracks
                    .into_iter()
                    .map(|track| fill.contrast_ratio(track))
                    .fold(1.0_f64, f64::max)
            };
            strength(*left).total_cmp(&strength(*right))
        })
}

fn resolve_selected_content<const N: usize>(
    authored: &ChromeColors,
    enabled: EnabledFrame,
    fill: Color,
    tracks: [Color; N],
    minimum: f64,
    cap: f64,
    host_side_content: Color,
) -> Color {
    let fill_background = fill.source_over(tracks[0]);
    resolve_disabled_frame(
        authored,
        enabled.fill,
        enabled.content,
        fill,
        tracks,
        minimum,
        Some(DisabledTextPolicy {
            fill_limits: DisabledFillLimits {
                minimum_contrast: 1.12,
                maximum_contrast: cap,
                direction: None,
            },
            host_side_content,
            host_side_polarity: DisabledPolarity::from_pair(host_side_content, fill_background),
        }),
    )
    .content
}

fn prepare_segmented_variant_fallback(
    colors: &mut ChromeColors,
    tracks: [Color; 2],
    minimum: f64,
    boundary: bool,
) {
    colors.text_disabled = readable_on_backgrounds(colors.text_disabled, tracks, minimum);
    let backgrounds = tracks.map(|track| colors.selection_disabled_background.source_over(track));
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

fn share_disabled_segmented_colors(
    authored: &ChromeColors,
    active: &mut ChromeColors,
    inactive: &mut ChromeColors,
    activity_hosts: DisabledActivityHosts,
    minimum: f64,
    boundary: bool,
    diagnostics: &mut Vec<DisabledControlDiagnostic>,
) -> bool {
    let active_tracks = activity_hosts
        .active
        .map(|host| active.element_background.source_over(host));
    let inactive_tracks = activity_hosts
        .inactive
        .map(|host| inactive.element_background.source_over(host));
    let inactive_reference_selected =
        inactive_tracks.map(|track| active.selection_background.source_over(track));
    let inactive_reference_directions = std::array::from_fn(|index| {
        DisabledPolarity::from_pair(inactive_reference_selected[index], inactive_tracks[index])
    });
    if inactive_tracks.into_iter().any(|track| {
        inactive
            .selection_background
            .source_over(track)
            .contrast_ratio(track)
            < 1.12
    }) && let Some(fill) = minimum_selected_fill(
        inactive_tracks,
        inactive_reference_directions,
        f64::INFINITY,
    ) {
        inactive.selection_background = fill;
    }
    let tracks = [
        active_tracks[0],
        active_tracks[1],
        inactive_tracks[0],
        inactive_tracks[1],
    ];
    let enabled_selected =
        active_tracks.map(|track| active.selection_background.source_over(track));
    let inactive_enabled_selected =
        inactive_tracks.map(|track| inactive.selection_background.source_over(track));
    let active_directions = std::array::from_fn(|index| {
        DisabledPolarity::from_pair(enabled_selected[index], active_tracks[index])
    });
    let inactive_directions = std::array::from_fn(|index| {
        DisabledPolarity::from_pair(inactive_enabled_selected[index], inactive_tracks[index])
    });
    let active_cap = enabled_selected
        .into_iter()
        .zip(active_tracks)
        .map(|(fill, track)| fill.contrast_ratio(track))
        .fold(f64::INFINITY, f64::min);
    let inactive_cap = inactive_enabled_selected
        .into_iter()
        .zip(inactive_tracks)
        .map(|(fill, track)| fill.contrast_ratio(track))
        .fold(f64::INFINITY, f64::min);
    let active_capped_fill = minimum_selected_fill(active_tracks, active_directions, active_cap);
    let inactive_capped_fill =
        minimum_selected_fill(inactive_tracks, inactive_directions, inactive_cap);
    let active_content_cap = if active_capped_fill.is_some() {
        active_cap
    } else {
        f64::INFINITY
    };
    let inactive_content_cap = if inactive_capped_fill.is_some() {
        inactive_cap
    } else {
        f64::INFINITY
    };
    let active_fill = active_capped_fill
        .or_else(|| minimum_selected_fill(active_tracks, active_directions, f64::INFINITY));
    let inactive_fill = inactive_capped_fill
        .or_else(|| minimum_selected_fill(inactive_tracks, inactive_directions, f64::INFINITY));
    if active_fill.is_none() || inactive_fill.is_none() {
        push_diagnostic(
            diagnostics,
            DisabledControlDiagnostic::SelectedStep {
                family: FloatingControlFamily::Segmented,
            },
        );
    }
    let active_fill = active_fill.unwrap_or(active.selection_disabled_background);
    let inactive_fill = inactive_fill.unwrap_or(inactive.selection_disabled_background);
    let active_content = resolve_selected_content(
        authored,
        EnabledFrame {
            fill: active.selection_background,
            content: active.selection_foreground,
        },
        active_fill,
        active_tracks,
        minimum,
        active_content_cap,
        active.text_disabled,
    );
    let inactive_content = resolve_selected_content(
        authored,
        EnabledFrame {
            fill: active.selection_background,
            content: active.selection_foreground,
        },
        inactive_fill,
        inactive_tracks,
        minimum,
        inactive_content_cap,
        inactive.text_disabled,
    );
    active.selection_disabled_background = active_fill;
    active.selection_disabled_foreground = active_content;
    active.selection_disabled_icon = active_content;
    inactive.selection_disabled_background = inactive_fill;
    inactive.selection_disabled_foreground = inactive_content;
    inactive.selection_disabled_icon = inactive_content;

    // The unselected label and boundary do not depend on whether one selected chip can be
    // shared across both activity variants. Reconcile them first so a selected-chip split does
    // not also split otherwise-compatible unselected paints.
    if let Some([label]) = resolve_disabled_union_content([(active.text_disabled, minimum)], tracks)
    {
        active.text_disabled = label;
        inactive.text_disabled = label;
    }
    if boundary
        && let Some([border]) =
            resolve_disabled_union_content([(active.ghost_element_disabled_border, 3.0)], tracks)
    {
        active.ghost_element_disabled_border = border;
        inactive.ghost_element_disabled_border = border;
    }

    let shared_cap = active_cap.min(inactive_cap);
    let shared_directions = [
        active_directions[0],
        active_directions[1],
        inactive_directions[0],
        inactive_directions[1],
    ];
    let shared_fill = minimum_selected_fill(tracks, shared_directions, shared_cap);
    let Some(selected_fill) = shared_fill else {
        prepare_segmented_variant_fallback(active, active_tracks, minimum, boundary);
        prepare_segmented_variant_fallback(inactive, inactive_tracks, minimum, boundary);
        push_diagnostic(
            diagnostics,
            DisabledControlDiagnostic::SharedPaint {
                family: FloatingControlFamily::Segmented,
            },
        );
        return false;
    };
    let selected_content = resolve_selected_content(
        authored,
        EnabledFrame {
            fill: active.selection_background,
            content: active.selection_foreground,
        },
        selected_fill,
        tracks,
        minimum,
        shared_cap,
        active.text_disabled,
    );
    active.selection_disabled_background = selected_fill;
    active.selection_disabled_foreground = selected_content;
    active.selection_disabled_icon = selected_content;
    inactive.selection_disabled_background = selected_fill;
    inactive.selection_disabled_foreground = selected_content;
    inactive.selection_disabled_icon = selected_content;
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
    prepare_segmented_variant_fallback(active, active_tracks, minimum, boundary);
    prepare_segmented_variant_fallback(inactive, inactive_tracks, minimum, boundary);
    push_diagnostic(
        diagnostics,
        DisabledControlDiagnostic::SharedPaint {
            family: FloatingControlFamily::Segmented,
        },
    );
    false
}

pub(super) fn reconcile(
    active: &mut ChromeAppearance,
    inactive: &mut ChromeAppearance,
    authored: &ChromeColors,
) {
    macro_rules! preserve { ($($field:ident),+ $(,)?) => { $(preserve_disabled_colors(&mut inactive.$field, &active.$field);)+ }; }
    preserve!(
        colors,
        floating_colors,
        floating_field_reference,
        floating_field_colors
    );
    for (inactive, active) in [
        (&mut inactive.panel_controls, &active.panel_controls),
        (&mut inactive.card_controls, &active.card_controls),
    ] {
        preserve_disabled_colors(&mut inactive.reference, &active.reference);
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
    let (material, wash) = state_floating_material(
        active.appearance,
        active.floating_materials,
        host,
        active.floating_colors.elevated_surface_background,
        active.capabilities.increase_contrast,
    );
    let floating_hosts = [Color::rgb(0), Color::rgb(0xffffff)]
        .map(|underlay| wash.source_over(material.source_over(underlay)));
    let floors = FloatingContrastFloors {
        interactive: false,
        ..if active.capabilities.increase_contrast {
            FloatingContrastFloors::INCREASED
        } else {
            FloatingContrastFloors::STANDARD
        }
    };
    let mut diagnostics = Vec::new();
    for (active_colors, inactive_colors, hosts, active_fallbacks, inactive_fallbacks) in [
        (
            &mut active.control_colors,
            &mut inactive.control_colors,
            DisabledActivityHosts::shared([window_host; 2]),
            &mut active.window_control_fallbacks,
            &mut inactive.window_control_fallbacks,
        ),
        (
            &mut active.title_bar_controls.colors,
            &mut inactive.title_bar_controls.colors,
            DisabledActivityHosts::paired(title_bar_hosts[0], title_bar_hosts[1]),
            &mut active.title_bar_controls.fallback_families,
            &mut inactive.title_bar_controls.fallback_families,
        ),
        (
            &mut active.panel_controls.colors,
            &mut inactive.panel_controls.colors,
            DisabledActivityHosts::shared([panel_host; 2]),
            &mut active.panel_controls.fallback_families,
            &mut inactive.panel_controls.fallback_families,
        ),
        (
            &mut active.card_controls.colors,
            &mut inactive.card_controls.colors,
            DisabledActivityHosts::shared([card_host; 2]),
            &mut active.card_controls.fallback_families,
            &mut inactive.card_controls.fallback_families,
        ),
        (
            &mut active.floating_control_colors,
            &mut inactive.floating_control_colors,
            DisabledActivityHosts::shared(floating_hosts),
            &mut active.floating_fallbacks,
            &mut inactive.floating_fallbacks,
        ),
    ] {
        for family in share_disabled_control_colors(
            authored,
            active_colors,
            inactive_colors,
            hosts,
            floors,
            boundary,
            &mut diagnostics,
        ) {
            for fallbacks in [&mut *active_fallbacks, &mut *inactive_fallbacks] {
                if !fallbacks.contains(&family) {
                    fallbacks.push(family);
                }
            }
        }
    }
    for (active_colors, inactive_colors, hosts, active_fallbacks, inactive_fallbacks) in [
        (
            &mut active.segmented_control_colors,
            &mut inactive.segmented_control_colors,
            DisabledActivityHosts::shared([window_host; 2]),
            &mut active.window_control_fallbacks,
            &mut inactive.window_control_fallbacks,
        ),
        (
            &mut active.title_bar_controls.segmented,
            &mut inactive.title_bar_controls.segmented,
            DisabledActivityHosts::paired(title_bar_hosts[0], title_bar_hosts[1]),
            &mut active.title_bar_controls.fallback_families,
            &mut inactive.title_bar_controls.fallback_families,
        ),
        (
            &mut active.panel_controls.segmented,
            &mut inactive.panel_controls.segmented,
            DisabledActivityHosts::shared([panel_host; 2]),
            &mut active.panel_controls.fallback_families,
            &mut inactive.panel_controls.fallback_families,
        ),
        (
            &mut active.card_controls.segmented,
            &mut inactive.card_controls.segmented,
            DisabledActivityHosts::shared([card_host; 2]),
            &mut active.card_controls.fallback_families,
            &mut inactive.card_controls.fallback_families,
        ),
        (
            &mut active.floating_segmented_colors,
            &mut inactive.floating_segmented_colors,
            DisabledActivityHosts::shared(floating_hosts),
            &mut active.floating_fallbacks,
            &mut inactive.floating_fallbacks,
        ),
    ] {
        if !share_disabled_segmented_colors(
            authored,
            active_colors,
            inactive_colors,
            hosts,
            minimum,
            boundary,
            &mut diagnostics,
        ) {
            for fallbacks in [active_fallbacks, inactive_fallbacks] {
                if !fallbacks.contains(&FloatingControlFamily::Segmented) {
                    fallbacks.push(FloatingControlFamily::Segmented);
                }
            }
        }
    }

    for (enabled, active_disabled, inactive_disabled, active_hosts, inactive_hosts) in [
        (
            &active.control_colors,
            &active.control_colors,
            &inactive.control_colors,
            [window_host; 2],
            [window_host; 2],
        ),
        (
            &active.title_bar_controls.colors,
            &active.title_bar_controls.colors,
            &inactive.title_bar_controls.colors,
            [title_bar_hosts[0]; 2],
            [title_bar_hosts[1]; 2],
        ),
        (
            &active.panel_controls.colors,
            &active.panel_controls.colors,
            &inactive.panel_controls.colors,
            [panel_host; 2],
            [panel_host; 2],
        ),
        (
            &active.card_controls.colors,
            &active.card_controls.colors,
            &inactive.card_controls.colors,
            [card_host; 2],
            [card_host; 2],
        ),
        (
            &active.floating_control_colors,
            &active.floating_control_colors,
            &inactive.floating_control_colors,
            floating_hosts,
            floating_hosts,
        ),
    ] {
        collect_disabled_control_diagnostics(
            enabled,
            active_disabled,
            active_hosts,
            minimum,
            &mut diagnostics,
        );
        collect_disabled_control_diagnostics(
            enabled,
            inactive_disabled,
            inactive_hosts,
            minimum,
            &mut diagnostics,
        );
    }
    for (enabled, active_disabled, inactive_disabled, active_hosts, inactive_hosts) in [
        (
            &active.segmented_control_colors,
            &active.segmented_control_colors,
            &inactive.segmented_control_colors,
            [window_host; 2],
            [window_host; 2],
        ),
        (
            &active.title_bar_controls.segmented,
            &active.title_bar_controls.segmented,
            &inactive.title_bar_controls.segmented,
            [title_bar_hosts[0]; 2],
            [title_bar_hosts[1]; 2],
        ),
        (
            &active.panel_controls.segmented,
            &active.panel_controls.segmented,
            &inactive.panel_controls.segmented,
            [panel_host; 2],
            [panel_host; 2],
        ),
        (
            &active.card_controls.segmented,
            &active.card_controls.segmented,
            &inactive.card_controls.segmented,
            [card_host; 2],
            [card_host; 2],
        ),
        (
            &active.floating_segmented_colors,
            &active.floating_segmented_colors,
            &inactive.floating_segmented_colors,
            floating_hosts,
            floating_hosts,
        ),
    ] {
        collect_disabled_segmented_diagnostics(
            enabled,
            active_disabled,
            active_hosts,
            minimum,
            &mut diagnostics,
        );
        collect_disabled_segmented_diagnostics(
            enabled,
            inactive_disabled,
            inactive_hosts,
            minimum,
            &mut diagnostics,
        );
    }
    active.disabled_diagnostics = diagnostics.clone();
    inactive.disabled_diagnostics = diagnostics;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feasible_disabled_content_targets_lower_contrast_than_enabled_content() {
        let host = Color::rgb(0xffffff);
        let mut active = ChromeColors {
            background: host,
            element_background: host,
            element_foreground: Color::rgb(0x202020),
            element_disabled: host,
            element_disabled_foreground: Color::rgb(0x000000),
            element_disabled_icon: Color::rgb(0x000000),
            ..ChromeColors::default()
        };
        let mut inactive = active.clone();
        let authored = active.clone();
        let mut diagnostics = Vec::new();

        let _fallbacks = share_disabled_control_colors(
            &authored,
            &mut active,
            &mut inactive,
            DisabledActivityHosts::shared([host; 2]),
            FloatingContrastFloors::STANDARD,
            false,
            &mut diagnostics,
        );
        let enabled = active.element_foreground.contrast_ratio(host);
        let disabled = active.element_disabled_foreground.contrast_ratio(host);

        assert!(disabled >= 3.0);
        assert!(
            disabled <= enabled / 1.3,
            "disabled contrast {disabled:.3} must target at most enabled/1.3={:.3}",
            enabled / 1.3
        );
    }

    #[test]
    fn text_bearing_disabled_content_flips_coherently_when_the_fill_cap_binds() {
        let host = Color::rgb(0xffffff);
        let authored = ChromeColors {
            background: host,
            text: Color::rgb(0x111111),
            element_background: Color::rgb(0xf0f0f0),
            element_foreground: Color::rgb(0x111111),
            element_disabled: Color::rgb(0xdddddd),
            element_disabled_foreground: Color::rgb(0x777777),
            primary_disabled_foreground: Color::rgb(0x0040c0),
            primary_disabled_icon: Color::rgb(0x0040c0),
            ..ChromeColors::default()
        };
        let mut active = ChromeColors {
            background: host,
            primary_background: Color::rgb(0x333333),
            primary_foreground: Color::rgb(0xffffff),
            primary_icon: Color::rgb(0xffffff),
            primary_disabled_background: Color::rgb(0xdddddd),
            primary_disabled_foreground: Color::rgb(0x0040c0),
            primary_disabled_icon: Color::rgb(0x0040c0),
            ..authored.clone()
        };
        let mut inactive = ChromeColors {
            primary_background: Color::rgb(0xeeeeee),
            primary_foreground: Color::rgb(0x111111),
            primary_icon: Color::rgb(0x111111),
            ..active.clone()
        };
        let mut diagnostics = Vec::new();

        let _ = share_disabled_control_colors(
            &authored,
            &mut active,
            &mut inactive,
            DisabledActivityHosts::shared([host; 2]),
            FloatingContrastFloors::STANDARD,
            false,
            &mut diagnostics,
        );

        let fill = active.primary_disabled_background.source_over(host);
        let ink = active.primary_disabled_foreground;
        assert_eq!(ink.r, ink.g);
        assert_eq!(ink.g, ink.b);
        assert_eq!(active.primary_disabled_icon, ink);
        assert_eq!(inactive.primary_disabled_foreground, ink);
        assert!(relative_luminance(ink) < relative_luminance(fill));
        assert!(ink.contrast_ratio(fill) >= 3.0);
    }

    #[test]
    fn text_bearing_disabled_fill_does_not_outweigh_the_ordinary_enabled_control() {
        let host = Color::rgb(0xffffff);
        let authored = ChromeColors {
            background: host,
            text: Color::rgb(0x111111),
            element_background: Color::rgb(0xf0f0f0),
            element_foreground: Color::rgb(0x111111),
            element_disabled: Color::rgb(0xeeeeee),
            element_disabled_foreground: Color::rgb(0x767676),
            ..ChromeColors::default()
        };
        let mut active = ChromeColors {
            primary_background: Color::rgb(0x0066cc),
            primary_foreground: Color::rgb(0xffffff),
            primary_icon: Color::rgb(0xffffff),
            primary_disabled_background: authored.element_disabled,
            primary_disabled_foreground: Color::rgb(0xffffff),
            primary_disabled_icon: Color::rgb(0xffffff),
            ..authored.clone()
        };
        let mut inactive = active.clone();
        let mut diagnostics = Vec::new();

        let _ = share_disabled_control_colors(
            &authored,
            &mut active,
            &mut inactive,
            DisabledActivityHosts::shared([host; 2]),
            FloatingContrastFloors::STANDARD,
            false,
            &mut diagnostics,
        );

        let ordinary = active.element_background.source_over(host);
        let fill = active.primary_disabled_background.source_over(host);
        let content = active.primary_disabled_foreground.source_over(fill);
        assert!(
            fill.contrast_ratio(host) <= ordinary.contrast_ratio(host) + 1e-9,
            "disabled fill {fill:?} must not outweigh ordinary fill {ordinary:?}"
        );
        assert!(content.contrast_ratio(fill) >= 3.0);
        assert!(relative_luminance(content) < relative_luminance(fill));
        assert_eq!(
            active.primary_disabled_icon,
            active.primary_disabled_foreground
        );
        assert_eq!(
            inactive.primary_disabled_background,
            active.primary_disabled_background
        );
        assert_eq!(
            inactive.primary_disabled_foreground,
            active.primary_disabled_foreground
        );
        assert_eq!(inactive.primary_disabled_icon, active.primary_disabled_icon);
    }

    #[test]
    fn host_side_floor_wins_when_relative_separation_is_impossible() {
        let host = Color::rgb(0xffffff);
        let authored = ChromeColors {
            background: host,
            text: Color::rgb(0x111111),
            element_background: Color::rgb(0xf0f0f0),
            element_disabled: Color::rgb(0xeeeeee),
            element_disabled_foreground: Color::rgb(0x767676),
            ..ChromeColors::default()
        };
        let resolved = resolve_disabled_frame(
            &authored,
            Color::rgb(0xb0b0b0),
            Color::rgb(0xc0c0c0),
            authored.element_disabled,
            [host; 2],
            3.0,
            Some(DisabledTextPolicy {
                fill_limits: DisabledFillLimits {
                    minimum_contrast: 1.0,
                    maximum_contrast: fill_weight_cap(authored.element_background, [host; 2]),
                    direction: None,
                },
                host_side_content: authored.element_disabled_foreground,
                host_side_polarity: DisabledPolarity::Dark,
            }),
        );
        let fill = resolved.fill.source_over(host);

        assert!(!resolved.floor_unmet);
        assert!(resolved.separation_unmet);
        assert!(resolved.content.contrast_ratio(fill) >= 3.0);
        assert!(relative_luminance(resolved.content) < relative_luminance(fill));
    }

    #[test]
    fn shared_fill_cap_pairs_each_activity_fill_with_its_own_host() {
        let hosts = [Color::rgb(0x000000), Color::rgb(0xffffff)];
        let active_fill = Color::rgb(0xe0e0e0);
        let inactive_fill = Color::rgb(0x202020);

        let cap = paired_fill_weight_cap(active_fill, inactive_fill, hosts);
        let expected = active_fill
            .contrast_ratio(hosts[0])
            .min(inactive_fill.contrast_ratio(hosts[1]));

        assert!((cap - expected).abs() < 1e-9);
        assert!(cap > active_fill.contrast_ratio(hosts[1]));
        assert!(cap > inactive_fill.contrast_ratio(hosts[0]));
    }

    #[test]
    fn disabled_selected_chip_uses_the_first_quantized_step_in_enabled_direction() {
        let track = Color::rgb(0xededed);
        let enabled = Color::rgb(0xd6d6d6);
        let cap = enabled.contrast_ratio(track);
        let direction = DisabledPolarity::from_pair(enabled, track);

        let resolved = minimum_selected_fill([track; 2], [direction; 2], cap)
            .expect("the enabled selected step admits a disabled selected step");
        let closer = Color::from_rgb_components(
            resolved.r.saturating_add(1),
            resolved.g.saturating_add(1),
            resolved.b.saturating_add(1),
        );

        assert!(resolved.contrast_ratio(track) >= 1.12);
        assert!(resolved.contrast_ratio(track) <= cap);
        assert!(closer.contrast_ratio(track) < 1.12);
        assert!(resolved.contrast_ratio(track) < Color::rgb(0xc0c0c0).contrast_ratio(track));
    }

    #[test]
    fn impossible_strong_separation_reports_separation_without_misreporting_the_floor() {
        let host = Color::rgb(0xffffff);
        let authored = ChromeColors {
            background: host,
            text: Color::rgb(0x111111),
            element_disabled: host,
            element_disabled_foreground: Color::rgb(0x777777),
            ..ChromeColors::default()
        };
        let mut colors = ChromeColors {
            element_background: host,
            element_foreground: Color::rgb(0x747474),
            element_disabled: host,
            ..authored.clone()
        };

        let diagnostics = prepare_disabled_semantic_content(&authored, &mut colors, [host; 2], 4.5);

        assert!(
            diagnostics.contains(&DisabledControlDiagnostic::Separation {
                family: FloatingControlFamily::Element,
            })
        );
        assert!(
            !diagnostics.contains(&DisabledControlDiagnostic::ContrastFloor {
                family: FloatingControlFamily::Element,
            })
        );
        assert!(colors.element_disabled_foreground.contrast_ratio(host) >= 4.5);
    }

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
        let authored = active.clone();
        let mut diagnostics = Vec::new();
        assert!(!share_disabled_segmented_colors(
            &authored,
            &mut active,
            &mut inactive,
            DisabledActivityHosts::shared([Color::rgb(0); 2]),
            4.5,
            true,
            &mut diagnostics,
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
        let authored = active.clone();
        let mut diagnostics = Vec::new();

        let fallbacks = share_disabled_control_colors(
            &authored,
            &mut active,
            &mut inactive,
            DisabledActivityHosts::paired(active_host, inactive_host),
            FloatingContrastFloors::INCREASED,
            true,
            &mut diagnostics,
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
    fn reconcile_keeps_incompatible_title_bar_disabled_paints_per_variant() {
        let active_host = Color::rgb(0x595959);
        let inactive_host = Color::rgb(0x949494);
        let mut active = ChromeAppearance::default();
        let mut inactive = active.clone();
        active.active = true;
        inactive.active = false;
        for appearance in [&mut active, &mut inactive] {
            appearance.capabilities.increase_contrast = true;
            appearance.colors.title_bar_background = active_host;
            appearance.colors.title_bar_inactive_background = inactive_host;
        }
        active.title_bar_controls.colors.ghost_element_disabled = Color::rgba(0);
        active
            .title_bar_controls
            .colors
            .ghost_element_disabled_foreground = Color::rgb(0xffffff);
        active.title_bar_controls.colors.ghost_element_disabled_icon = Color::rgb(0xffffff);
        inactive.title_bar_controls.colors.ghost_element_disabled = Color::rgba(0);
        inactive
            .title_bar_controls
            .colors
            .ghost_element_disabled_foreground = Color::rgb(0x000000);
        inactive
            .title_bar_controls
            .colors
            .ghost_element_disabled_icon = Color::rgb(0x000000);
        let authored = ChromeColors::default();

        reconcile(&mut active, &mut inactive, &authored);

        assert_ne!(
            active
                .title_bar_controls
                .colors
                .ghost_element_disabled_foreground,
            inactive
                .title_bar_controls
                .colors
                .ghost_element_disabled_foreground
        );
        for (appearance, host) in [(&active, active_host), (&inactive, inactive_host)] {
            let colors = &appearance.title_bar_controls.colors;
            let background = colors.ghost_element_disabled.source_over(host);
            assert!(
                colors
                    .ghost_element_disabled_foreground
                    .source_over(background)
                    .contrast_ratio(background)
                    >= 4.5
            );
        }
        assert!(
            active
                .disabled_diagnostics
                .contains(&DisabledControlDiagnostic::SharedPaint {
                    family: FloatingControlFamily::GhostElement,
                })
        );
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
