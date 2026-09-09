use std::{
    borrow::Cow,
    sync::{Arc, LazyLock},
};

use gpui::{
    App, AssetSource, FontFallbacks, Global, IntoElement, ParentElement as _, Pixels, RenderOnce,
    Rgba, SharedString, Styled as _, Window, div, font, svg,
};

/// The Lucide family embedded by `lucide-icons` 1.34.0.
const LUCIDE_FONT_FAMILY: &str = "lucide";
static EMPTY_FONT_FALLBACKS: LazyLock<FontFallbacks> =
    LazyLock::new(|| FontFallbacks(Arc::new(Vec::new())));

struct LucideFontRegistered;

impl Global for LucideFontRegistered {}

pub(crate) fn register_font(cx: &mut App) -> gpui::Result<()> {
    register_font_with(cx, |cx| {
        cx.text_system()
            .add_fonts(vec![Cow::Borrowed(lucide_icons::LUCIDE_FONT_BYTES)])
    })
}

fn register_font_with(
    cx: &mut App,
    add_font: impl FnOnce(&App) -> gpui::Result<()>,
) -> gpui::Result<()> {
    if cx.has_global::<LucideFontRegistered>() {
        return Ok(());
    }

    add_font(cx)?;
    cx.set_global(LucideFontRegistered);
    Ok(())
}

/// A bundled vector icon outside the Lucide family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustomIconName {
    /// Stacked rectangular surfaces, using the supplied rectangle.stack artwork.
    RectangleStack,
}

impl CustomIconName {
    fn path(self) -> &'static str {
        match self {
            Self::RectangleStack => "spaceterm-ui/icons/rectangle-stack.svg",
        }
    }
}

const EMBEDDED_ICONS: &[(&str, &[u8])] = &[(
    "spaceterm-ui/icons/rectangle-stack.svg",
    include_bytes!("../assets/icons/rectangle-stack.svg"),
)];

/// The reusable UI crate's bundled assets, registered with GPUI's
/// `Application::with_assets` before rendering custom icons.
///
/// Only paths in the `spaceterm-ui` namespace are provided. Applications with
/// additional assets can delegate these lookups from their own `AssetSource`.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmbeddedAssets;

impl AssetSource for EmbeddedAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(EMBEDDED_ICONS
            .iter()
            .find_map(|(name, bytes)| (*name == path).then_some(Cow::Borrowed(*bytes))))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let directory = path.trim_end_matches('/');
        Ok(EMBEDDED_ICONS
            .iter()
            .filter(|(name, _)| {
                directory.is_empty()
                    || name
                        .strip_prefix(directory)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

enum IconSource {
    Lucide(IconName),
    Custom(CustomIconName),
}

/// A typed icon rendered at an explicit logical size and tint.
///
/// The fixed square keeps bundled vectors and Lucide glyphs aligned with adjacent
/// typography without exposing raw asset paths or private-use glyphs to callers.
#[derive(IntoElement)]
pub struct Icon {
    source: IconSource,
    size: Pixels,
    tint: Rgba,
}

impl Icon {
    /// Creates an icon with no implicit size or color policy.
    pub fn new(name: IconName, size: Pixels, tint: Rgba) -> Self {
        Self {
            source: IconSource::Lucide(name),
            size,
            tint,
        }
    }

    /// Creates a bundled custom icon with the same size and tint contract as Lucide icons.
    /// Register [`EmbeddedAssets`] with the GPUI application before rendering it.
    pub fn custom(name: CustomIconName, size: Pixels, tint: Rgba) -> Self {
        Self {
            source: IconSource::Custom(name),
            size,
            tint,
        }
    }
}

impl RenderOnce for Icon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let content = match self.source {
            IconSource::Lucide(name) => div()
                .font({
                    let mut icon_font = font(LUCIDE_FONT_FAMILY);
                    icon_font.fallbacks = Some(EMPTY_FONT_FALLBACKS.clone());
                    icon_font
                })
                .text_size(self.size)
                .line_height(self.size)
                .text_color(self.tint)
                .child(name.unicode().to_string())
                .into_any_element(),
            IconSource::Custom(name) => svg()
                .path(name.path())
                .size(self.size)
                .text_color(self.tint)
                .into_any_element(),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(self.size)
            .child(content)
    }
}

pub use lucide_icons::Icon as IconName;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui::{
        Context, InteractiveElement as _, ParentElement as _, Render, TestAppContext, Window, size,
    };

    struct IconTestRoot;

    impl Render for IconTestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().flex().children(
                [
                    ("icon-10", IconName::Pin, 10.0),
                    ("icon-12", IconName::Search, 12.0),
                    ("icon-14", IconName::Folder, 14.0),
                ]
                .map(|(id, name, logical_size)| {
                    let logical_size = gpui::px(logical_size);
                    div()
                        .id(id)
                        .debug_selector(move || id.to_owned())
                        .size(logical_size)
                        .child(Icon::new(name, logical_size, gpui::rgba(0x8f9aafff)))
                }),
            )
        }
    }

    use super::*;

    #[gpui::test]
    fn lucide_font_registration_should_succeed_once(cx: &mut TestAppContext) {
        let mut registrations = 0;

        cx.update(|cx| {
            register_font_with(cx, |_| {
                registrations += 1;
                Ok(())
            })?;
            register_font_with(cx, |_| {
                registrations += 1;
                Ok(())
            })
        })
        .expect("font registration should succeed");

        assert_eq!(registrations, 1);
    }

    #[gpui::test]
    fn failed_font_registration_should_remain_retryable(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let failure =
                register_font_with(
                    cx,
                    |_| Err(std::io::Error::other("injected failure").into()),
                );
            assert!(failure.is_err());
            assert!(!cx.has_global::<LucideFontRegistered>());

            register_font_with(cx, |_| Ok(()))
                .expect("a failed registration must not poison a later attempt");
            assert!(cx.has_global::<LucideFontRegistered>());
        });
    }

    #[test]
    fn selected_icon_names_should_have_distinct_private_use_glyphs() {
        let names = [
            IconName::Folder,
            IconName::Globe,
            IconName::Terminal,
            IconName::Pin,
            IconName::PanelLeft,
            IconName::TriangleAlert,
            IconName::Search,
            IconName::Plus,
            IconName::SquarePlus,
            IconName::Pencil,
            IconName::RotateCw,
            IconName::X,
            IconName::Ellipsis,
            IconName::Columns2,
            IconName::Rows2,
            IconName::Maximize2,
            IconName::Minimize2,
            IconName::ChevronUp,
            IconName::ChevronDown,
            IconName::ChevronRight,
            IconName::Copy,
            IconName::ExternalLink,
            IconName::Eye,
            IconName::Check,
            IconName::CircleDot,
            IconName::Info,
            IconName::OctagonAlert,
            IconName::ImageOff,
            IconName::Square,
            IconName::SquareCheckBig,
        ];
        let glyphs = names
            .into_iter()
            .map(IconName::unicode)
            .collect::<HashSet<_>>();

        assert_eq!(glyphs.len(), names.len());
        assert!(glyphs.iter().all(|glyph| !glyph.is_whitespace()));
    }

    #[test]
    fn embedded_assets_should_only_resolve_owned_asset_paths() {
        let path = CustomIconName::RectangleStack.path();
        assert!(matches!(
            EmbeddedAssets.load(path).expect("asset lookup"),
            Some(Cow::Borrowed(_))
        ));
        assert!(
            EmbeddedAssets
                .load("icons/rectangle-stack.svg")
                .expect("unknown path")
                .is_none()
        );
        assert!(
            EmbeddedAssets
                .load("spaceterm-ui/icons/../rectangle-stack.svg")
                .expect("unknown path")
                .is_none()
        );
        assert_eq!(
            EmbeddedAssets
                .list("spaceterm-ui/icons")
                .expect("owned directory"),
            vec![SharedString::from(path)]
        );
        assert!(
            EmbeddedAssets
                .list("other/icons")
                .expect("unknown directory")
                .is_empty()
        );
    }

    #[gpui::test]
    fn custom_vector_should_rasterize_into_an_unclipped_square(cx: &mut TestAppContext) {
        let bytes = EmbeddedAssets
            .load(CustomIconName::RectangleStack.path())
            .expect("asset lookup")
            .expect("bundled rectangle.stack asset");
        cx.update(|cx| {
            let rendered = gpui::Image::from_bytes(gpui::ImageFormat::Svg, bytes.into_owned())
                .to_image_data(cx.svg_renderer())
                .expect("bundled vector should parse and rasterize through GPUI");
            assert_eq!(
                rendered.size(0),
                size(gpui::DevicePixels(24), gpui::DevicePixels(24))
            );
            let pixels = rendered.as_bytes(0).expect("rasterized SVG frame");
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 255));
            let width = 24;
            assert!(
                pixels.chunks_exact(4).enumerate().all(|(index, pixel)| {
                    let x = index % width;
                    let y = index / width;
                    (x != 0 && y != 0 && x != width - 1 && y != width - 1) || pixel[3] == 0
                }),
                "artwork should not reach the square canvas edge"
            );
        });
    }

    #[gpui::test]
    fn representative_icons_should_render_at_compact_logical_sizes(cx: &mut TestAppContext) {
        cx.update(register_font)
            .expect("bundled Lucide font registration should succeed");
        let (_, cx) = cx.add_window_view(|_, _| IconTestRoot);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        for (id, logical_size) in [("icon-10", 10.0), ("icon-12", 12.0), ("icon-14", 14.0)] {
            let bounds = cx
                .debug_bounds(id)
                .unwrap_or_else(|| panic!("{id} should be painted"));
            assert_eq!(
                bounds.size,
                size(gpui::px(logical_size), gpui::px(logical_size))
            );
        }
    }
}
