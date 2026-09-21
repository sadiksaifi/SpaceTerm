//! Semantic icon metrics paired with the chrome typography catalog.

use gpui::{Pixels, px};

use crate::appearance::ChromeDensity;

use super::chrome_typography::{ChromeTypography, TextRole};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IconRole {
    Caption,
    Status,
    Row,
    Control,
    Chrome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InteractiveIconRole {
    Row,
    Control,
    Chrome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkRole {
    Clear,
    Reset,
}

impl MarkRole {
    const COUNT: usize = 2;

    const fn index(self) -> usize {
        self as usize
    }
}

impl InteractiveIconRole {
    const COUNT: usize = 3;

    const fn index(self) -> usize {
        self as usize
    }
}

impl IconRole {
    const COUNT: usize = 5;

    const fn index(self) -> usize {
        self as usize
    }

    const fn paired_text(self) -> TextRole {
        match self {
            Self::Caption | Self::Status => TextRole::Caption,
            Self::Row | Self::Control => TextRole::Body,
            Self::Chrome => TextRole::Navigation,
        }
    }

    const fn glyph_offset(self) -> f32 {
        match self {
            Self::Caption => 1.0,
            Self::Status | Self::Row => 2.0,
            Self::Control => 3.0,
            Self::Chrome => 4.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChromeIconMetrics {
    pub(crate) glyph_size: Pixels,
    pub(crate) baseline_center: Pixels,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChromeMarkMetrics {
    pub(crate) disc_diameter: Pixels,
    pub(crate) glyph_size: Pixels,
    pub(crate) target_size: Pixels,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChromeIcons {
    metrics: [ChromeIconMetrics; IconRole::COUNT],
    interactive_target_sizes: [Pixels; InteractiveIconRole::COUNT],
    mark_metrics: [ChromeMarkMetrics; MarkRole::COUNT],
}

impl ChromeIcons {
    pub(crate) fn prepare(typography: &ChromeTypography, density: ChromeDensity) -> Self {
        let interactive_target_sizes = [
            InteractiveIconRole::Row,
            InteractiveIconRole::Control,
            InteractiveIconRole::Chrome,
        ]
        .map(|role| px(interactive_target_size(role, density)));
        let metrics = [
            IconRole::Caption,
            IconRole::Status,
            IconRole::Row,
            IconRole::Control,
            IconRole::Chrome,
        ]
        .map(|role| {
            let paired_size = f32::from(typography.style(role.paired_text()).size);
            ChromeIconMetrics {
                glyph_size: px((paired_size + role.glyph_offset()).round()),
                baseline_center: px((paired_size * 0.36).round()),
            }
        });
        let body_size = f32::from(typography.style(TextRole::Body).size);
        let clear_disc = body_size.round();
        let minimum_target = interactive_target_size(InteractiveIconRole::Control, density);
        let mark_metrics = [
            ChromeMarkMetrics {
                disc_diameter: px(clear_disc),
                glyph_size: px((clear_disc * 0.58).round()),
                target_size: px(minimum_target.max(clear_disc)),
            },
            ChromeMarkMetrics {
                disc_diameter: px(0.0),
                // An inline reset annotates the label instead of competing with it.
                glyph_size: px((body_size - 2.0).round().max(1.0)),
                target_size: px(minimum_target),
            },
        ];
        Self {
            metrics,
            interactive_target_sizes,
            mark_metrics,
        }
    }

    pub(crate) fn metrics(&self, role: IconRole) -> ChromeIconMetrics {
        self.metrics[role.index()]
    }

    pub(crate) fn interactive_target_size(&self, role: InteractiveIconRole) -> Pixels {
        self.interactive_target_sizes[role.index()]
    }

    pub(crate) fn mark_metrics(&self, role: MarkRole) -> ChromeMarkMetrics {
        self.mark_metrics[role.index()]
    }
}

impl Default for ChromeIcons {
    fn default() -> Self {
        Self::prepare(&ChromeTypography::default(), ChromeDensity::Compact)
    }
}

const fn interactive_target_size(role: InteractiveIconRole, density: ChromeDensity) -> f32 {
    match role {
        InteractiveIconRole::Row => match density {
            ChromeDensity::Compact => 24.0,
            ChromeDensity::Comfortable => 28.0,
        },
        InteractiveIconRole::Control => match density {
            ChromeDensity::Compact => 20.0,
            ChromeDensity::Comfortable => 28.0,
        },
        InteractiveIconRole::Chrome => 28.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typography(density: ChromeDensity) -> ChromeTypography {
        typography_with_size(13.0, density)
    }

    fn typography_with_size(size: f32, density: ChromeDensity) -> ChromeTypography {
        use crate::appearance::{FontStyle, ResolvedChromeTypography, ResolvedFontDescriptor};

        let descriptor = || ResolvedFontDescriptor {
            primary_family: ".SystemUIFont".to_owned(),
            fallback_families: Vec::new(),
            size,
            line_height: size,
            weight: 400,
            style: FontStyle::Normal,
            features: Vec::new(),
            resolution_identity: "system-default".to_owned(),
        };
        ChromeTypography::prepare(
            &ResolvedChromeTypography {
                body: descriptor(),
                small: descriptor(),
                control: descriptor(),
                navigation: descriptor(),
                caption: descriptor(),
                heading: descriptor(),
                shortcut: descriptor(),
            },
            density,
        )
    }

    #[test]
    fn comfortable_icon_metrics_follow_their_text_role_and_target_policy() {
        let compact_type = typography(ChromeDensity::Compact);
        let comfortable_type = typography(ChromeDensity::Comfortable);
        let compact = ChromeIcons::prepare(&compact_type, ChromeDensity::Compact);
        let comfortable = ChromeIcons::prepare(&comfortable_type, ChromeDensity::Comfortable);

        for role in [
            IconRole::Caption,
            IconRole::Status,
            IconRole::Row,
            IconRole::Control,
            IconRole::Chrome,
        ] {
            assert_eq!(
                f32::from(comfortable.metrics(role).glyph_size),
                f32::from(compact.metrics(role).glyph_size) + 1.0
            );
        }
        assert_eq!(
            compact.interactive_target_size(InteractiveIconRole::Control),
            px(20.0)
        );
        assert_eq!(
            comfortable.interactive_target_size(InteractiveIconRole::Control),
            px(28.0)
        );
        assert_eq!(
            comfortable.interactive_target_size(InteractiveIconRole::Chrome),
            px(28.0)
        );
    }

    #[test]
    fn baseline_center_tracks_the_paired_text_instead_of_the_glyph_box() {
        let typography = typography(ChromeDensity::Compact);
        let icons = ChromeIcons::prepare(&typography, ChromeDensity::Compact);

        assert_eq!(
            icons.metrics(IconRole::Control).baseline_center,
            icons.metrics(IconRole::Row).baseline_center
        );
        assert_ne!(
            icons.metrics(IconRole::Control).glyph_size,
            icons.metrics(IconRole::Row).glyph_size
        );
    }

    #[test]
    fn clear_mark_pairs_body_geometry_with_the_control_target() {
        let compact_type = typography(ChromeDensity::Compact);
        let comfortable_type = typography(ChromeDensity::Comfortable);
        let compact = ChromeIcons::prepare(&compact_type, ChromeDensity::Compact);
        let comfortable = ChromeIcons::prepare(&comfortable_type, ChromeDensity::Comfortable);

        assert_eq!(
            compact.mark_metrics(MarkRole::Clear),
            ChromeMarkMetrics {
                disc_diameter: px(12.0),
                glyph_size: px(7.0),
                target_size: px(20.0),
            }
        );
        assert_eq!(
            comfortable.mark_metrics(MarkRole::Clear),
            ChromeMarkMetrics {
                disc_diameter: px(13.0),
                glyph_size: px(8.0),
                target_size: px(28.0),
            }
        );

        let large_type = typography_with_size(33.0, ChromeDensity::Compact);
        let large = ChromeIcons::prepare(&large_type, ChromeDensity::Compact);
        let large = large.mark_metrics(MarkRole::Clear);
        assert_eq!(large.disc_diameter, px(32.0));
        assert_eq!(large.target_size, large.disc_diameter);
    }
}
