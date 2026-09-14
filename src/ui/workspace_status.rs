//! Contrast-safe semantic paint shared by expanded and collapsed Workspace identities.

use crate::appearance::Color;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WorkspaceStatusPaint {
    pub(super) normal: Color,
    pub(super) hovered: Color,
}

pub(super) fn resolve(
    proposed: Color,
    normal_background: Color,
    hovered_background: Color,
    minimum_contrast: f64,
) -> WorkspaceStatusPaint {
    WorkspaceStatusPaint {
        normal: color_on(proposed, normal_background, minimum_contrast),
        hovered: color_on(proposed, hovered_background, minimum_contrast),
    }
}

fn color_on(proposed: Color, background: Color, minimum_contrast: f64) -> Color {
    let rendered = proposed.source_over(background);
    if rendered.contrast_ratio(background) >= minimum_contrast {
        return rendered;
    }

    let dark = Color::rgb(0x000000);
    let light = Color::rgb(0xffffff);
    let target = if dark.contrast_ratio(background) >= light.contrast_ratio(background) {
        dark
    } else {
        light
    };
    let mut lower = 0.0;
    let mut upper = 1.0;
    let mut readable = target;
    for _ in 0..16 {
        let amount = (lower + upper) / 2.0;
        let candidate = rendered.mix(target, amount);
        if candidate.contrast_ratio(background) >= minimum_contrast {
            readable = candidate;
            upper = amount;
        } else {
            lower = amount;
        }
    }
    readable
}
