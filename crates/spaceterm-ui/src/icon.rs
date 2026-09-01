use std::{
    borrow::Cow,
    sync::{Arc, LazyLock},
};

use gpui::{
    App, FontFallbacks, Global, IntoElement, ParentElement as _, Pixels, RenderOnce, Rgba,
    Styled as _, Window, div, font,
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

/// A typed Lucide icon rendered at an explicit logical size and tint.
///
/// The fixed square and matched line height keep the 24-by-24 Lucide canvas aligned with adjacent
/// typography without exposing raw private-use glyphs to callers.
#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: Pixels,
    tint: Rgba,
}

impl Icon {
    /// Creates an icon with no implicit size or color policy.
    pub fn new(name: IconName, size: Pixels, tint: Rgba) -> Self {
        Self { name, size, tint }
    }
}

impl RenderOnce for Icon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(self.size)
            .font({
                let mut icon_font = font(LUCIDE_FONT_FAMILY);
                icon_font.fallbacks = Some(EMPTY_FONT_FALLBACKS.clone());
                icon_font
            })
            .text_size(self.size)
            .line_height(self.size)
            .text_color(self.tint)
            .child(self.name.unicode().to_string())
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
