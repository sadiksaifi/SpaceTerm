//! Functional rules are prepared independently of decorative control and surface borders.

use crate::appearance::Color;

use super::{content_is_lighter_than_background, host_relative_fill, preferred_readable_endpoint};

/// A role's standard contrast limits. Increase Contrast retains its independent floor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SeparatorBand {
    pub(super) floor: f64,
    pub(super) ceiling: f64,
}

pub(super) fn prepare_in_band<const N: usize>(
    seed: Color,
    root: Color,
    text: Color,
    backgrounds: [Color; N],
    increase_contrast: bool,
    band: SeparatorBand,
) -> Color {
    let host = backgrounds[0];
    let floor = if increase_contrast { 3.0 } else { band.floor };
    let lighter = content_is_lighter_than_background(text, host);
    let mut endpoint = Color::rgb(if lighter { 0xffffff } else { 0 });
    let meets_floor = |color: Color| {
        backgrounds
            .iter()
            .all(|host| color.source_over(*host).contrast_ratio(*host) >= floor)
    };
    if !meets_floor(endpoint) {
        endpoint = preferred_readable_endpoint(backgrounds);
    }
    let lighter = content_is_lighter_than_background(endpoint, host);
    let on_text_side = |color: Color| {
        backgrounds
            .iter()
            .all(|host| content_is_lighter_than_background(color, *host) == lighter)
    };
    let target = host_relative_fill(seed, root, host).unwrap_or(seed);
    let proposed = target.relative_overlay(host);
    let within_ceiling = |color: Color| {
        increase_contrast
            || backgrounds
                .iter()
                .all(|host| color.source_over(*host).contrast_ratio(*host) <= band.ceiling)
    };
    if on_text_side(proposed) && meets_floor(proposed) && within_ceiling(proposed) {
        return proposed;
    }

    // Retain the authored ink when its polarity works on every endpoint. Otherwise use the
    // definition's text side. A bounded alpha search keeps the rule transmitting its material.
    let ink = proposed.with_alpha(255);
    let ink = if on_text_side(ink) && meets_floor(ink) {
        ink
    } else {
        endpoint
    };
    let minimum_alpha = |ink: Color| {
        let mut lower = 0_u16;
        let mut upper = 255_u16;
        while lower + 1 < upper {
            let middle = (lower + upper) / 2;
            if meets_floor(ink.with_alpha(middle as u8)) {
                upper = middle;
            } else {
                lower = middle;
            }
        }
        ink.with_alpha(upper as u8)
    };
    let prepared = minimum_alpha(ink);
    if within_ceiling(prepared) {
        prepared
    } else {
        minimum_alpha(endpoint)
    }
}
