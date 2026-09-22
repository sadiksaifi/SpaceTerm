use crate::appearance::{ChromeColors, Color, SurfaceRole};

use super::ChromeAppearance;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedCollectionSelection {
    window: ChromeColors,
    title_bar: ChromeColors,
    panel: ChromeColors,
    card: ChromeColors,
    floating: ChromeColors,
}

impl PreparedCollectionSelection {
    pub(super) fn identity(
        window: &ChromeColors,
        title_bar: &ChromeColors,
        panel: &ChromeColors,
        card: &ChromeColors,
        floating: &ChromeColors,
    ) -> Self {
        Self {
            window: window.clone(),
            title_bar: title_bar.clone(),
            panel: panel.clone(),
            card: card.clone(),
            floating: floating.clone(),
        }
    }

    pub(crate) fn colors(&self, host: spaceterm_ui::ControlHost) -> &ChromeColors {
        match host {
            spaceterm_ui::ControlHost::Window => &self.window,
            spaceterm_ui::ControlHost::TitleBar => &self.title_bar,
            spaceterm_ui::ControlHost::Panel => &self.panel,
            spaceterm_ui::ControlHost::Card => &self.card,
            spaceterm_ui::ControlHost::Floating => &self.floating,
        }
    }
}

pub(super) fn prepare(
    active: &ChromeAppearance,
    inactive: &ChromeAppearance,
) -> PreparedCollectionSelection {
    let mut result = PreparedCollectionSelection::identity(
        &active.colors,
        &active.title_bar_controls.reference,
        &active.panel_controls.reference,
        &active.card_controls.reference,
        &active.floating_colors,
    );
    for host in [
        spaceterm_ui::ControlHost::Window,
        spaceterm_ui::ControlHost::TitleBar,
        spaceterm_ui::ControlHost::Panel,
        spaceterm_ui::ControlHost::Card,
    ] {
        let active_colors = active.host_colors(host);
        let inactive_colors = inactive.host_colors(host);
        let final_host = active.control_host_background(host);
        let semantic_host = match host {
            spaceterm_ui::ControlHost::Window => active_colors.background,
            spaceterm_ui::ControlHost::TitleBar => active_colors.title_bar_background,
            spaceterm_ui::ControlHost::Panel => active_colors.panel_background,
            spaceterm_ui::ControlHost::Card => active_colors.elevated_surface_background,
            spaceterm_ui::ControlHost::Floating => unreachable!(),
        };
        let materials = active.materials;
        let row_host = materials
            .paint(
                SurfaceRole::Surface,
                semantic_host,
                active_colors.row_background,
            )
            .source_over(final_host);
        prepare_colors(
            result.colors_mut(host),
            inactive_colors,
            active.capabilities.increase_contrast,
            |fill| {
                let surface = active
                    .selection_surface(semantic_host, fill)
                    .source_over(final_host);
                let row = active
                    .selection_surface(active_colors.row_background, fill)
                    .source_over(row_host);
                [surface, row]
            },
        );
    }

    let shell = active
        .floating_surfaces()
        .shell(spaceterm_ui::FloatingRole::Popover);
    let tone = Color::rgba(u32::from(shell.backdrop_tone()));
    let wash = Color::rgba(u32::from(shell.material()));
    let hosts = [Color::rgb(0), Color::rgb(0xffffff)]
        .map(|underlay| wash.source_over(tone.source_over(underlay)));
    prepare_colors(
        &mut result.floating,
        &inactive.floating_colors,
        active.capabilities.increase_contrast,
        |fill| hosts.map(|host| fill.source_over(host)),
    );
    result
}

impl PreparedCollectionSelection {
    fn colors_mut(&mut self, host: spaceterm_ui::ControlHost) -> &mut ChromeColors {
        match host {
            spaceterm_ui::ControlHost::Window => &mut self.window,
            spaceterm_ui::ControlHost::TitleBar => &mut self.title_bar,
            spaceterm_ui::ControlHost::Panel => &mut self.panel,
            spaceterm_ui::ControlHost::Card => &mut self.card,
            spaceterm_ui::ControlHost::Floating => &mut self.floating,
        }
    }
}

fn prepare_colors(
    active: &mut ChromeColors,
    inactive: &ChromeColors,
    increase_contrast: bool,
    backgrounds: impl Fn(Color) -> [Color; 2] + Copy,
) {
    let primary = if increase_contrast { 7.0 } else { 4.5 };
    let secondary = 4.5;
    let icon = if increase_contrast { 4.5 } else { 3.0 };
    prepare_state(
        &mut active.row_selected_background,
        inactive.row_selected_background,
        [
            (&mut active.row_selected_foreground, primary),
            (&mut active.row_selected_secondary, secondary),
            (&mut active.row_selected_icon, icon),
            (&mut active.row_selected_match, primary),
        ],
        increase_contrast,
        backgrounds,
    );
    prepare_state(
        &mut active.row_selected_hover_background,
        inactive.row_selected_hover_background,
        [
            (&mut active.row_selected_hover_foreground, primary),
            (&mut active.row_selected_hover_secondary, secondary),
            (&mut active.row_selected_hover_icon, icon),
            (&mut active.row_selected_hover_match, primary),
        ],
        increase_contrast,
        backgrounds,
    );
    active.row_selected_border = inactive.row_selected_border;
    active.row_selected_hover_border = inactive.row_selected_hover_border;
}

fn prepare_state<const N: usize>(
    target_fill: &mut Color,
    inactive_fill: Color,
    mut content: [(&mut Color, f64); N],
    increase_contrast: bool,
    backgrounds: impl Fn(Color) -> [Color; 2],
) {
    let active_fill = *target_fill;
    let proposals = content.each_ref().map(|(color, floor)| (**color, *floor));
    let hosts = backgrounds(Color::rgba(0));
    let selection_floor = if increase_contrast {
        super::SUBDUED_SELECTION_CONTRAST
    } else {
        // A translucent selected fill cannot promise its opaque reference contrast. Retain the
        // visible active step instead of flipping a raised selection into a dark recess, and ask
        // an unfocused selection for no more separation than the focused one actually has: a
        // fixed floor above that step is only reachable by leaving the window's material behind.
        backgrounds(active_fill)
            .into_iter()
            .zip(hosts)
            .map(|(background, host)| background.contrast_ratio(host))
            .fold(super::SUBDUED_SELECTION_CONTRAST, f64::min)
    };
    let inactive_keeps_direction = backgrounds(active_fill)
        .into_iter()
        .zip(backgrounds(inactive_fill))
        .zip(hosts)
        .all(|((active, inactive), host)| {
            let host = super::relative_luminance(host);
            let active_delta = super::relative_luminance(active) - host;
            let inactive_delta = super::relative_luminance(inactive) - host;
            active_delta * inactive_delta >= 0.0
        });
    let resolve = |fill: Color| {
        let backgrounds = backgrounds(fill);
        if backgrounds
            .into_iter()
            .zip(hosts)
            .any(|(background, host)| background.contrast_ratio(host) < selection_floor)
        {
            return None;
        }
        let resolved =
            proposals.map(|(color, floor)| color.readable_preserving_chroma(&backgrounds, floor));
        let resolved: Option<[Color; N]> = resolved
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .and_then(|resolved| resolved.try_into().ok());
        resolved.map(|resolved| (fill, resolved))
    };

    let dimmed = if inactive_keeps_direction {
        resolve(inactive_fill)
    } else {
        None
    };
    let prepared = dimmed.or_else(|| resolve(active_fill)).or_else(|| {
        [Color::rgb(0), Color::rgb(0xffffff)]
            .into_iter()
            .filter_map(|endpoint| {
                let mut previous = 0.0;
                for step in 1..=16 {
                    let amount = f64::from(step) / 16.0;
                    let Some(mut prepared) = resolve(inactive_fill.mix(endpoint, amount)) else {
                        previous = amount;
                        continue;
                    };
                    let mut lower = previous;
                    let mut upper = amount;
                    for _ in 0..12 {
                        let middle = (lower + upper) / 2.0;
                        if let Some(candidate) = resolve(inactive_fill.mix(endpoint, middle)) {
                            prepared = candidate;
                            upper = middle;
                        } else {
                            lower = middle;
                        }
                    }
                    return Some((prepared, upper));
                }
                None
            })
            .min_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(prepared, _)| prepared)
    });
    let (fill, resolved) = prepared.unwrap_or_else(|| {
        // Opaque opposite endpoints are the strongest possible presentation. This branch is a
        // deterministic best effort for a genuinely contradictory set of multiple backgrounds;
        // it never leaves the original unreadable active ink in the prepared pair.
        let score = |fill: Color, ink: Color| {
            backgrounds(fill)
                .into_iter()
                .map(|background| ink.contrast_ratio(background))
                .fold(f64::INFINITY, f64::min)
        };
        [Color::rgb(0), Color::rgb(0xffffff)]
            .map(|fill| {
                let ink = [Color::rgb(0), Color::rgb(0xffffff)]
                    .into_iter()
                    .max_by(|left, right| score(fill, *left).total_cmp(&score(fill, *right)))
                    .expect("two achromatic endpoints");
                (fill, [ink; N], score(fill, ink))
            })
            .into_iter()
            .max_by(|(_, _, left), (_, _, right)| left.total_cmp(right))
            .map(|(fill, resolved, _)| (fill, resolved))
            .expect("two achromatic fills")
    });
    *target_fill = fill;
    for ((target, _), resolved) in content.iter_mut().zip(resolved) {
        **target = resolved;
    }
}
