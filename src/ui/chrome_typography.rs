//! Prepared semantic typography for application chrome.
//!
//! A caller chooses a role, never a point size. The catalog retains the resolved font family,
//! fallbacks, style, and authored OpenType features while owning size, weight, and line height.

use std::sync::Arc;

use gpui::{Font, FontFeatures, FontWeight, Pixels, Styled, TextRun, Window, px, rgba};

use crate::appearance::{ChromeDensity, ResolvedChromeTypography, ResolvedFontDescriptor};

use super::appearance::prepared_font;

const MINIMUM_SIZE: f32 = 9.0;
const MAXIMUM_SIZE: f32 = 32.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextRole {
    Title,
    Section,
    Body,
    BodyEmphasis,
    Navigation,
    Secondary,
    Caption,
    Shortcut,
    Badge,
}

impl TextRole {
    const COUNT: usize = 9;

    const fn index(self) -> usize {
        self as usize
    }

    const fn specification(self) -> RoleSpecification {
        match self {
            Self::Title => RoleSpecification::new(9.0, true, 1.18, true, false),
            Self::Section => RoleSpecification::new(0.0, true, 1.30, true, false),
            Self::Body => RoleSpecification::new(-1.0, true, 1.33, false, false),
            Self::BodyEmphasis => RoleSpecification::new(-1.0, true, 1.33, true, false),
            Self::Navigation => RoleSpecification::new(-1.0, true, 1.33, false, false),
            Self::Secondary => RoleSpecification::new(-2.0, true, 1.36, false, false),
            Self::Caption => RoleSpecification::new(-2.0, true, 1.27, false, false),
            Self::Shortcut => RoleSpecification::new(-2.0, false, 1.36, false, true),
            Self::Badge => RoleSpecification::new(-3.0, false, 1.20, true, true),
        }
    }
}

#[derive(Clone, Copy)]
struct RoleSpecification {
    offset: f32,
    comfortable_step: bool,
    line_height_ratio: f32,
    semibold: bool,
    tabular: bool,
}

impl RoleSpecification {
    const fn new(
        offset: f32,
        comfortable_step: bool,
        line_height_ratio: f32,
        semibold: bool,
        tabular: bool,
    ) -> Self {
        Self {
            offset,
            comfortable_step,
            line_height_ratio,
            semibold,
            tabular,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChromeTextStyle {
    pub(crate) font: Font,
    pub(crate) size: Pixels,
    pub(crate) line_height: Pixels,
}

impl ChromeTextStyle {
    /// Adds tabular figures without dropping the role's other font features.
    pub(crate) fn tabular(&self) -> Self {
        let mut style = self.clone();
        add_font_feature(&mut style.font, "tnum", 1);
        style
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChromeTypography {
    styles: [ChromeTextStyle; TextRole::COUNT],
}

impl ChromeTypography {
    pub(crate) fn prepare(resolved: &ResolvedChromeTypography, density: ChromeDensity) -> Self {
        let base_size = resolved.body.size;
        let styles = [
            TextRole::Title,
            TextRole::Section,
            TextRole::Body,
            TextRole::BodyEmphasis,
            TextRole::Navigation,
            TextRole::Secondary,
            TextRole::Caption,
            TextRole::Shortcut,
            TextRole::Badge,
        ]
        .map(|role| {
            Self::prepare_style(role, Self::descriptor(role, resolved), base_size, density)
        });
        Self { styles }
    }

    fn descriptor(role: TextRole, resolved: &ResolvedChromeTypography) -> &ResolvedFontDescriptor {
        match role {
            TextRole::Title | TextRole::Section => &resolved.heading,
            TextRole::Navigation => &resolved.navigation,
            TextRole::Caption | TextRole::Badge => &resolved.caption,
            TextRole::Body | TextRole::BodyEmphasis | TextRole::Secondary | TextRole::Shortcut => {
                &resolved.body
            }
        }
    }

    fn prepare_style(
        role: TextRole,
        descriptor: &ResolvedFontDescriptor,
        base_size: f32,
        density: ChromeDensity,
    ) -> ChromeTextStyle {
        let specification = role.specification();
        let density_step =
            if density == ChromeDensity::Comfortable && specification.comfortable_step {
                1.0
            } else {
                0.0
            };
        let size = (base_size + specification.offset + density_step)
            .round()
            .clamp(MINIMUM_SIZE, MAXIMUM_SIZE);
        let mut font = prepared_font(descriptor);
        font.weight = if specification.semibold {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        };
        if specification.tabular {
            add_font_feature(&mut font, "tnum", 1);
        }
        ChromeTextStyle {
            font,
            size: px(size),
            line_height: px((size * specification.line_height_ratio).round()),
        }
    }

    pub(crate) fn style(&self, role: TextRole) -> &ChromeTextStyle {
        &self.styles[role.index()]
    }

    pub(crate) fn measure(&self, role: TextRole, value: &str, window: &Window) -> Pixels {
        let style = self.style(role);
        window
            .text_system()
            .shape_line(
                value.to_owned().into(),
                style.size,
                &[TextRun {
                    len: value.len(),
                    font: style.font.clone(),
                    color: rgba(0x000000ff).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
            .width
    }
}

impl Default for ChromeTypography {
    fn default() -> Self {
        fn descriptor(size: f32) -> ResolvedFontDescriptor {
            ResolvedFontDescriptor {
                primary_family: ".SystemUIFont".to_owned(),
                fallback_families: Vec::new(),
                size,
                line_height: size,
                weight: 400,
                style: crate::appearance::FontStyle::Normal,
                features: Vec::new(),
                resolution_identity: "system-default".to_owned(),
            }
        }
        let body = descriptor(13.0);
        let resolved = ResolvedChromeTypography {
            body: body.clone(),
            small: body.clone(),
            control: body.clone(),
            navigation: body.clone(),
            caption: body.clone(),
            heading: body.clone(),
            shortcut: body,
        };
        Self::prepare(&resolved, ChromeDensity::Compact)
    }
}

fn add_font_feature(font: &mut Font, tag: &str, value: u32) {
    let mut features = font.features.tag_value_list().to_vec();
    if let Some((_, existing)) = features.iter_mut().find(|(candidate, _)| candidate == tag) {
        *existing = value;
    } else {
        features.push((tag.to_owned(), value));
    }
    font.features = FontFeatures(Arc::new(features));
}

pub(crate) trait ChromeTextStyleExt: Styled + Sized {
    fn chrome_text(self, style: &ChromeTextStyle) -> Self {
        self.font(style.font.clone())
            .text_size(style.size)
            .line_height(style.line_height)
    }
}

impl<T: Styled> ChromeTextStyleExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabular_readouts_preserve_features_and_override_disabled_tabular_figures() {
        let mut style = ChromeTypography::default().style(TextRole::Body).clone();
        style.font.features = FontFeatures(Arc::new(vec![
            ("calt".to_owned(), 0),
            ("tnum".to_owned(), 0),
        ]));
        let tabular = style.tabular();
        assert_eq!(tabular.size, style.size);
        assert_eq!(tabular.line_height, style.line_height);
        assert_eq!(
            tabular.font.features.tag_value_list(),
            &[("calt".to_owned(), 0), ("tnum".to_owned(), 1)]
        );
        assert_eq!(tabular.tabular(), tabular);
    }

    fn resolved_with_body_size(size: f32) -> ResolvedChromeTypography {
        fn descriptor(family: &str, size: f32, features: &[&str]) -> ResolvedFontDescriptor {
            ResolvedFontDescriptor {
                primary_family: family.to_owned(),
                fallback_families: vec!["Fallback One".to_owned(), "Fallback Two".to_owned()],
                size,
                line_height: size,
                weight: 300,
                style: crate::appearance::FontStyle::Italic,
                features: features.iter().map(ToString::to_string).collect(),
                resolution_identity: format!("{family}-{size}"),
            }
        }
        ResolvedChromeTypography {
            body: descriptor("Body Family", size, &["-calt", "+ss01"]),
            small: descriptor("Small Family", size - 2.0, &[]),
            control: descriptor("Control Family", size - 1.0, &[]),
            navigation: descriptor("Navigation Family", size - 1.0, &["+ss02"]),
            caption: descriptor("Caption Family", size - 2.0, &["+ss03"]),
            heading: descriptor("Heading Family", size + 9.0, &["+ss04"]),
            shortcut: descriptor("Shortcut Family", size - 2.0, &[]),
        }
    }

    #[test]
    fn comfortable_density_only_steps_roles_that_can_reflow() {
        let resolved = resolved_with_body_size(13.0);
        let compact = ChromeTypography::prepare(&resolved, ChromeDensity::Compact);
        let comfortable = ChromeTypography::prepare(&resolved, ChromeDensity::Comfortable);

        for role in [
            TextRole::Title,
            TextRole::Section,
            TextRole::Body,
            TextRole::BodyEmphasis,
            TextRole::Navigation,
            TextRole::Secondary,
            TextRole::Caption,
        ] {
            assert_eq!(
                f32::from(comfortable.style(role).size),
                f32::from(compact.style(role).size) + 1.0
            );
        }
        for role in [TextRole::Shortcut, TextRole::Badge] {
            assert_eq!(comfortable.style(role).size, compact.style(role).size);
        }
    }

    #[test]
    fn base_size_is_additive_and_catalog_bounds_are_enforced() {
        let large =
            ChromeTypography::prepare(&resolved_with_body_size(24.0), ChromeDensity::Compact);
        assert_eq!(large.style(TextRole::Body).size, px(23.0));
        assert_eq!(large.style(TextRole::Title).size, px(32.0));

        let small =
            ChromeTypography::prepare(&resolved_with_body_size(3.0), ChromeDensity::Compact);
        assert_eq!(small.style(TextRole::Badge).size, px(9.0));
    }

    #[test]
    fn roles_retain_descriptor_identity_and_features_but_own_weight() {
        let typography =
            ChromeTypography::prepare(&resolved_with_body_size(13.0), ChromeDensity::Compact);
        let shortcut = typography.style(TextRole::Shortcut);
        assert_eq!(shortcut.font.family.as_ref(), "Body Family");
        assert_eq!(
            shortcut
                .font
                .fallbacks
                .as_ref()
                .expect("fallbacks")
                .0
                .as_ref(),
            &["Fallback One".to_owned(), "Fallback Two".to_owned()]
        );
        assert_eq!(shortcut.font.style, gpui::FontStyle::Italic);
        assert_eq!(shortcut.font.weight, FontWeight::NORMAL);
        assert!(
            shortcut
                .font
                .features
                .tag_value_list()
                .contains(&("ss01".to_owned(), 1))
        );
        assert!(
            shortcut
                .font
                .features
                .tag_value_list()
                .contains(&("tnum".to_owned(), 1))
        );

        let title = typography.style(TextRole::Title);
        assert_eq!(title.font.family.as_ref(), "Heading Family");
        assert_eq!(title.font.weight, FontWeight::SEMIBOLD);
    }
}
