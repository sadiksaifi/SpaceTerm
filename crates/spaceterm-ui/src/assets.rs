use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub(crate) const ALERT_INFORMATION_ICON: &str = "spaceterm-ui/icons/alert-information.svg";
pub(crate) const ALERT_WARNING_ICON: &str = "spaceterm-ui/icons/alert-warning.svg";
pub(crate) const ALERT_CRITICAL_ICON: &str = "spaceterm-ui/icons/alert-critical.svg";
pub(crate) const IMAGE_PLACEHOLDER_ICON: &str = "spaceterm-ui/icons/image-placeholder.svg";
pub(crate) const CHECKBOX_UNCHECKED_ICON: &str = "spaceterm-ui/icons/checkbox-unchecked.svg";
pub(crate) const CHECKBOX_CHECKED_ICON: &str = "spaceterm-ui/icons/checkbox-checked.svg";

const ASSET_PATHS: [&str; 6] = [
    ALERT_INFORMATION_ICON,
    ALERT_WARNING_ICON,
    ALERT_CRITICAL_ICON,
    IMAGE_PLACEHOLDER_ICON,
    CHECKBOX_UNCHECKED_ICON,
    CHECKBOX_CHECKED_ICON,
];

/// Embedded SVG assets required by reusable SpaceTerm controls.
///
/// Install this source on the GPUI application with `Application::with_assets` before rendering
/// controls from this crate.
#[derive(Clone, Copy, Debug, Default)]
pub struct ControlAssets;

impl AssetSource for ControlAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            ALERT_INFORMATION_ICON => Some(include_bytes!("../assets/icons/alert-information.svg")),
            ALERT_WARNING_ICON => Some(include_bytes!("../assets/icons/alert-warning.svg")),
            ALERT_CRITICAL_ICON => Some(include_bytes!("../assets/icons/alert-critical.svg")),
            IMAGE_PLACEHOLDER_ICON => Some(include_bytes!("../assets/icons/image-placeholder.svg")),
            CHECKBOX_UNCHECKED_ICON => {
                Some(include_bytes!("../assets/icons/checkbox-unchecked.svg"))
            }
            CHECKBOX_CHECKED_ICON => Some(include_bytes!("../assets/icons/checkbox-checked.svg")),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSET_PATHS
            .into_iter()
            .filter(|asset| asset.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_control_asset_loads_and_renders_as_svg() {
        let assets = ControlAssets;

        for path in ASSET_PATHS {
            let bytes = assets
                .load(path)
                .expect("embedded control asset should load")
                .expect("declared control asset should exist");
            let tree = resvg::usvg::Tree::from_data(&bytes, &resvg::usvg::Options::default())
                .unwrap_or_else(|error| panic!("{path} should parse as SVG: {error}"));
            let mut pixmap = resvg::tiny_skia::Pixmap::new(16, 16)
                .expect("control icon raster target should be valid");
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(
                    16.0 / tree.size().width(),
                    16.0 / tree.size().height(),
                ),
                &mut pixmap.as_mut(),
            );

            assert!(
                pixmap.pixels().iter().any(|pixel| pixel.alpha() > 0),
                "{path} should rasterize visible pixels"
            );
        }
    }
}
