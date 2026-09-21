use gpui::{Rgba, px, rgba};
use spaceterm_ui::{TooltipMetrics, TooltipPaint, TooltipTheme};

use crate::appearance::{ChromeColors, Color};
use crate::ui::chrome_typography::{ChromeTypography, TextRole};

pub(super) fn prepared(colors: &ChromeColors, typography: &ChromeTypography) -> TooltipTheme {
    TooltipTheme::new(
        TooltipPaint::new(
            gpui_color(colors.text),
            gpui_color(colors.text_muted),
            gpui_color(colors.text_muted),
        ),
        metrics(typography),
    )
}

fn metrics(typography: &ChromeTypography) -> TooltipMetrics {
    let body = typography.style(TextRole::Body);
    let secondary = typography.style(TextRole::Secondary);
    let shortcut = typography.style(TextRole::Shortcut);
    TooltipMetrics::new(px(480.0))
        .spacing(px(8.0), px(5.0), px(4.0), px(12.0), px(6.0), px(8.0))
        .font_sizes(body.size, secondary.size, shortcut.size)
        .line_heights(
            body.line_height,
            secondary.line_height,
            shortcut.line_height,
        )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_metrics_project_body_secondary_and_shortcut_roles() {
        let typography = ChromeTypography::default();
        let body = typography.style(TextRole::Body);
        let secondary = typography.style(TextRole::Secondary);
        let shortcut = typography.style(TextRole::Shortcut);

        assert_eq!(
            metrics(&typography),
            TooltipMetrics::new(px(480.0))
                .spacing(px(8.0), px(5.0), px(4.0), px(12.0), px(6.0), px(8.0))
                .font_sizes(body.size, secondary.size, shortcut.size)
                .line_heights(
                    body.line_height,
                    secondary.line_height,
                    shortcut.line_height,
                )
        );
    }
}
