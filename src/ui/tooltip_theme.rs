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
    let caption = typography.style(TextRole::Caption);
    let badge = typography.style(TextRole::Badge);
    TooltipMetrics::new(px(480.0))
        .spacing(px(8.0), px(5.0), px(4.0), px(12.0), px(6.0), px(8.0))
        .font_sizes(caption.size, badge.size, badge.size)
        .line_heights(caption.line_height, badge.line_height, badge.line_height)
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_metrics_project_caption_and_badge_line_heights() {
        let typography = ChromeTypography::default();
        let caption = typography.style(TextRole::Caption);
        let badge = typography.style(TextRole::Badge);

        assert_eq!(
            metrics(&typography),
            TooltipMetrics::new(px(480.0))
                .spacing(px(8.0), px(5.0), px(4.0), px(12.0), px(6.0), px(8.0))
                .font_sizes(caption.size, badge.size, badge.size)
                .line_heights(caption.line_height, badge.line_height, badge.line_height)
        );
    }
}
