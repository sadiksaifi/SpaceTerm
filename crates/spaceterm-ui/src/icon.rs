use std::{
    borrow::Cow,
    sync::{Arc, LazyLock},
};

use gpui::{
    App, AssetSource, FontFallbacks, Global, Hsla, IntoElement, ParentElement as _, Pixels,
    RenderOnce, Rgba, SharedString, Styled as _, Window, canvas, div, font,
    prelude::FluentBuilder as _,
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
    /// A panel with a left sidebar, rendered with the bundled chrome vectors.
    PanelLeft,
    /// A panel with a right sidebar, rendered with the bundled chrome vectors.
    PanelRight,
    /// An add action, rendered with the bundled chrome vectors.
    Plus,
    /// Stacked rectangular surfaces, using the supplied rectangle.stack artwork.
    RectangleStack,
    /// Globe with an add badge, using the supplied globe.plus artwork.
    GlobePlus,
    /// Stacked surfaces with an add badge, using rectangle.stack.badge.plus artwork.
    RectangleStackBadgePlus,
    /// Decreasing horizontal lines in a circle, using supplied filter artwork.
    FilterCircle,
}

impl CustomIconName {
    fn path(self) -> &'static str {
        match self {
            Self::PanelLeft => "spaceterm-ui/icons/panel-left.svg",
            Self::PanelRight => "spaceterm-ui/icons/panel-right.svg",
            Self::Plus => "spaceterm-ui/icons/plus.svg",
            Self::RectangleStack => "spaceterm-ui/icons/rectangle-stack.svg",
            Self::FilterCircle => "spaceterm-ui/icons/filter-circle.svg",
            Self::GlobePlus => "spaceterm-ui/icons/globe-plus.svg",
            Self::RectangleStackBadgePlus => "spaceterm-ui/icons/rectangle-stack-badge-plus.svg",
        }
    }
}

const EMBEDDED_ICONS: &[(&str, &[u8])] = &[
    (
        "spaceterm-ui/icons/panel-left.svg",
        include_bytes!("../assets/icons/panel-left.svg"),
    ),
    (
        "spaceterm-ui/icons/panel-right.svg",
        include_bytes!("../assets/icons/panel-right.svg"),
    ),
    (
        "spaceterm-ui/icons/plus.svg",
        include_bytes!("../assets/icons/plus.svg"),
    ),
    (
        "spaceterm-ui/icons/filter-circle.svg",
        include_bytes!("../assets/icons/filter-circle.svg"),
    ),
    (
        "spaceterm-ui/icons/rectangle-stack.svg",
        include_bytes!("../assets/icons/rectangle-stack.svg"),
    ),
    (
        "spaceterm-ui/icons/globe-plus.svg",
        include_bytes!("../assets/icons/globe-plus.svg"),
    ),
    (
        "spaceterm-ui/icons/rectangle-stack-badge-plus.svg",
        include_bytes!("../assets/icons/rectangle-stack-badge-plus.svg"),
    ),
];

const LUCIDE_ASSET_PREFIX: &str = "spaceterm-ui/lucide/";
const MIN_LUCIDE_ARTWORK_SIZE: u8 = 8;
const MAX_LUCIDE_ARTWORK_SIZE: u8 = 40;

macro_rules! lucide_sources {
    ($($variant:ident => $slug:literal),+ $(,)?) => {
        const LUCIDE_SOURCES: &[(IconName, &str, &[u8])] = &[
            $(
                (
                    IconName::$variant,
                    $slug,
                    include_bytes!(concat!("../assets/lucide/", $slug, ".svg")),
                ),
            )+
        ];
    };
}

lucide_sources! {
    AppWindow => "app-window",
    Check => "check",
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    ChevronUp => "chevron-up",
    CircleAlert => "circle-alert",
    CircleDot => "circle-dot",
    Cog => "cog",
    Columns2 => "columns-2",
    Copy => "copy",
    Ellipsis => "ellipsis",
    ExternalLink => "external-link",
    Eye => "eye",
    Folder => "folder",
    Globe => "globe",
    ImageOff => "image-off",
    Info => "info",
    Maximize2 => "maximize-2",
    Minimize2 => "minimize-2",
    Minus => "minus",
    Palette => "palette",
    Pause => "pause",
    Pencil => "pencil",
    Pin => "pin",
    PinOff => "pin-off",
    Plus => "plus",
    RotateCcw => "rotate-ccw",
    RotateCw => "rotate-cw",
    Rows2 => "rows-2",
    Search => "search",
    Shield => "shield",
    Square => "square",
    SquareCheckBig => "square-check-big",
    SquarePlus => "square-plus",
    SunMoon => "sun-moon",
    Terminal => "terminal",
    Trash2 => "trash-2",
    TriangleAlert => "triangle-alert",
    X => "x",
}

fn lucide_source(name: IconName) -> Option<(&'static str, &'static [u8])> {
    let discriminant = std::mem::discriminant(&name);
    LUCIDE_SOURCES
        .iter()
        .find(|(candidate, _, _)| std::mem::discriminant(candidate) == discriminant)
        .map(|(_, slug, source)| (*slug, *source))
}

fn lucide_asset_path(
    name: IconName,
    nominal_size: Pixels,
    artwork_size: Pixels,
) -> Option<SharedString> {
    let (slug, _) = lucide_source(name)?;
    let artwork_size = f32::from(artwork_size).round();
    if !(f32::from(MIN_LUCIDE_ARTWORK_SIZE)..=f32::from(MAX_LUCIDE_ARTWORK_SIZE))
        .contains(&artwork_size)
    {
        return None;
    }
    let artwork_size = artwork_size as u8;
    let stroke_width = normalized_stroke_width(nominal_size) as u8;
    Some(format!("{LUCIDE_ASSET_PREFIX}{slug}/{artwork_size}/{stroke_width}.svg").into())
}

fn normalized_stroke_width(nominal_size: Pixels) -> f32 {
    (f32::from(nominal_size) / 12.0).round().clamp(1.0, 2.0)
}

fn prepared_lucide_asset(path: &str) -> Option<Vec<u8>> {
    let relative = path.strip_prefix(LUCIDE_ASSET_PREFIX)?;
    let mut segments = relative.split('/');
    let slug = segments.next()?;
    let artwork_size = segments.next()?.parse::<u8>().ok()?;
    let stroke_width = segments.next()?.strip_suffix(".svg")?.parse::<u8>().ok()?;
    if segments.next().is_some()
        || !(MIN_LUCIDE_ARTWORK_SIZE..=MAX_LUCIDE_ARTWORK_SIZE).contains(&artwork_size)
        || !(1..=2).contains(&stroke_width)
    {
        return None;
    }
    let (_, _, source) = LUCIDE_SOURCES
        .iter()
        .find(|(_, candidate, _)| *candidate == slug)?;
    let source = std::str::from_utf8(source).ok()?;
    let source_width = f32::from(stroke_width) * 24.0 / f32::from(artwork_size);
    Some(
        source
            .replace(
                "stroke-width=\"2\"",
                &format!("stroke-width=\"{source_width:.6}\""),
            )
            .into_bytes(),
    )
}

/// The reusable UI crate's bundled assets, registered with GPUI's
/// `Application::with_assets` before rendering custom icons.
///
/// Only paths in the `spaceterm-ui` namespace are provided. Applications with
/// additional assets can delegate these lookups from their own `AssetSource`.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmbeddedAssets;

impl AssetSource for EmbeddedAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = prepared_lucide_asset(path) {
            return Ok(Some(Cow::Owned(bytes)));
        }
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

#[derive(Clone, Copy)]
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
    tint: Option<Rgba>,
    text_alignment: Option<IconTextAlignment>,
    #[cfg(test)]
    tint_observer: Option<Arc<std::sync::Mutex<Vec<Hsla>>>>,
}

#[derive(Clone)]
struct IconTextAlignment {
    font: gpui::Font,
    font_size: Pixels,
    line_height: Pixels,
    baseline_center: Pixels,
}

impl Icon {
    /// Creates an icon with no implicit size or color policy.
    pub fn new(name: IconName, size: Pixels, tint: Rgba) -> Self {
        Self {
            source: IconSource::Lucide(name),
            size,
            tint: Some(tint),
            text_alignment: None,
            #[cfg(test)]
            tint_observer: None,
        }
    }

    /// Creates a glyph whose tint follows the surrounding semantic foreground state.
    pub fn inherited(name: IconName, size: Pixels) -> Self {
        Self {
            source: IconSource::Lucide(name),
            size,
            tint: None,
            text_alignment: None,
            #[cfg(test)]
            tint_observer: None,
        }
    }

    /// Creates a bundled vector whose tint follows the surrounding semantic foreground state.
    pub fn custom_inherited(name: CustomIconName, size: Pixels) -> Self {
        Self {
            source: IconSource::Custom(name),
            size,
            tint: None,
            text_alignment: None,
            #[cfg(test)]
            tint_observer: None,
        }
    }

    /// Creates a bundled custom icon with the same size and tint contract as Lucide icons.
    /// Register [`EmbeddedAssets`] with the GPUI application before rendering it.
    pub fn custom(name: CustomIconName, size: Pixels, tint: Rgba) -> Self {
        Self {
            source: IconSource::Custom(name),
            size,
            tint: Some(tint),
            text_alignment: None,
            #[cfg(test)]
            tint_observer: None,
        }
    }

    /// Aligns the icon box's center to a prepared text role's cap-height band.
    ///
    /// The caller supplies the role's center-above-baseline metric. Font ascent remains a renderer
    /// fact and is resolved from the actual font and line height when the icon paints.
    pub fn align_to_text(
        mut self,
        font: gpui::Font,
        font_size: Pixels,
        line_height: Pixels,
        baseline_center: Pixels,
    ) -> Self {
        self.text_alignment = Some(IconTextAlignment {
            font,
            font_size,
            line_height,
            baseline_center,
        });
        self
    }

    #[cfg(test)]
    fn observe_tint(mut self, observer: Arc<std::sync::Mutex<Vec<Hsla>>>) -> Self {
        self.tint_observer = Some(observer);
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let artwork_size = optical_artwork_size(self.source, self.size);
        let vertical_offset = self
            .text_alignment
            .map(|alignment| icon_text_offset(&alignment, window))
            .unwrap_or_default();
        let content = match self.source {
            IconSource::Lucide(name) => match lucide_asset_path(name, self.size, artwork_size) {
                Some(path) => icon_svg(
                    path,
                    artwork_size,
                    IconTint {
                        explicit: self.tint,
                        #[cfg(test)]
                        observer: self.tint_observer.clone(),
                    },
                )
                .into_any_element(),
                None => div()
                    .font({
                        let mut icon_font = font(LUCIDE_FONT_FAMILY);
                        icon_font.fallbacks = Some(EMPTY_FONT_FALLBACKS.clone());
                        icon_font
                    })
                    .text_size(artwork_size)
                    .line_height(artwork_size)
                    .when_some(self.tint, |element, tint| element.text_color(tint))
                    .child(name.unicode().to_string())
                    .into_any_element(),
            },
            IconSource::Custom(name) => icon_svg(
                name.path().into(),
                artwork_size,
                IconTint {
                    explicit: self.tint,
                    #[cfg(test)]
                    observer: self.tint_observer.clone(),
                },
            )
            .into_any_element(),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(self.size)
            .relative()
            .top(vertical_offset)
            .child(content)
    }
}

struct IconTint {
    explicit: Option<Rgba>,
    #[cfg(test)]
    observer: Option<Arc<std::sync::Mutex<Vec<Hsla>>>>,
}

impl IconTint {
    fn resolve(&self, window: &Window) -> Hsla {
        let tint = self
            .explicit
            .map(Into::into)
            .unwrap_or_else(|| window.text_style().color);
        #[cfg(test)]
        if let Some(observer) = &self.observer {
            observer.lock().expect("tint observation lock").push(tint);
        }
        tint
    }
}

fn icon_svg(path: SharedString, size: Pixels, tint: IconTint) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| {
            let tint = tint.resolve(window);
            // Like GPUI's Svg element, an asset or raster failure omits the glyph. Canvas has no
            // error channel through which to return a recoverable paint failure.
            let _paint = window.paint_svg(bounds, path, Default::default(), tint, cx);
        },
    )
    .size(size)
}

fn icon_text_offset(alignment: &IconTextAlignment, window: &Window) -> Pixels {
    text_alignment_offset(
        &alignment.font,
        alignment.font_size,
        alignment.line_height,
        alignment.baseline_center,
        window,
    )
}

pub(crate) fn text_alignment_offset(
    font: &gpui::Font,
    font_size: Pixels,
    line_height: Pixels,
    baseline_center: Pixels,
    window: &Window,
) -> Pixels {
    let font_id = window.text_system().resolve_font(font);
    let baseline = window
        .text_system()
        .baseline_offset(font_id, font_size, line_height);
    baseline - baseline_center - line_height / 2.0
}

/// Keeps an icon's semantic layout box while correcting artwork that occupies an unusually large
/// share of the family's nominal square. Entries are admitted only by the chrome optical contract;
/// call sites never compensate individual glyphs.
fn optical_artwork_size(source: IconSource, nominal_size: Pixels) -> Pixels {
    let scale = match source {
        IconSource::Lucide(IconName::PinOff) => 0.85,
        IconSource::Lucide(_) | IconSource::Custom(_) => 1.0,
    };
    gpui::px((f32::from(nominal_size) * scale).round())
}

pub use lucide_icons::Icon as IconName;

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{Arc, Mutex},
    };

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

    struct InheritedIconTintRoot {
        inherited: Arc<Mutex<Vec<Hsla>>>,
        disabled: Arc<Mutex<Vec<Hsla>>>,
        explicit: Arc<Mutex<Vec<Hsla>>>,
        base: Hsla,
        hovered: Hsla,
        disabled_tint: Hsla,
        explicit_tint: Rgba,
    }

    impl Render for InheritedIconTintRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .child(
                    div()
                        .id("inherited-icon-hover-target")
                        .debug_selector(|| "inherited-icon-hover-target".to_owned())
                        .group("inherited-icon-hover-group")
                        .size(gpui::px(24.0))
                        .child(
                            div()
                                .text_color(self.base)
                                .group_hover("inherited-icon-hover-group", {
                                    let hovered = self.hovered;
                                    move |style| style.text_color(hovered)
                                })
                                .child(
                                    Icon::inherited(IconName::Terminal, gpui::px(14.0))
                                        .observe_tint(self.inherited.clone()),
                                ),
                        )
                        .child(
                            div()
                                .text_color(self.base)
                                .group_hover("inherited-icon-hover-group", |style| {
                                    style.text_color(gpui::rgba(0xffffffff))
                                })
                                .child(
                                    Icon::new(IconName::Cog, gpui::px(14.0), self.explicit_tint)
                                        .observe_tint(self.explicit.clone()),
                                ),
                        ),
                )
                .child(
                    div().text_color(self.disabled_tint).child(
                        Icon::inherited(IconName::Shield, gpui::px(14.0))
                            .observe_tint(self.disabled.clone()),
                    ),
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

    #[gpui::test]
    fn inherited_svg_icon_should_resolve_parent_tint_and_nonempty_artwork(cx: &mut TestAppContext) {
        let inherited = Arc::new(Mutex::new(Vec::new()));
        let disabled = Arc::new(Mutex::new(Vec::new()));
        let explicit = Arc::new(Mutex::new(Vec::new()));
        let base = gpui::rgba(0x4a90e2ff).into();
        let hovered = gpui::rgba(0xe24a90ff).into();
        let disabled_tint = gpui::rgba(0x727272ff).into();
        let explicit_tint = gpui::rgba(0x24aa68ff);
        let (_, cx) = cx.add_window_view({
            let inherited = inherited.clone();
            let disabled = disabled.clone();
            let explicit = explicit.clone();
            move |_, _| InheritedIconTintRoot {
                inherited,
                disabled,
                explicit,
                base,
                hovered,
                disabled_tint,
                explicit_tint,
            }
        });
        cx.run_until_parked();

        let bounds = cx
            .debug_bounds("inherited-icon-hover-target")
            .expect("hover target should be painted");
        cx.simulate_mouse_move(
            gpui::point(
                bounds.origin.x + bounds.size.width + gpui::px(50.0),
                bounds.origin.y + bounds.size.height + gpui::px(50.0),
            ),
            None,
            gpui::Modifiers::none(),
        );
        cx.run_until_parked();

        assert_eq!(
            inherited
                .lock()
                .expect("inherited tint observation lock")
                .last()
                .copied(),
            Some(base),
            "the SVG paint must receive the surrounding semantic foreground"
        );
        assert_eq!(
            disabled
                .lock()
                .expect("disabled tint observation lock")
                .last()
                .copied(),
            Some(disabled_tint),
            "disabled inherited SVGs must receive their semantic parent tint"
        );
        assert_eq!(
            explicit
                .lock()
                .expect("explicit tint observation lock")
                .last()
                .copied(),
            Some(explicit_tint.into()),
            "an explicit tint must override its parent state"
        );

        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            inherited
                .lock()
                .expect("inherited tint observation lock")
                .last()
                .copied(),
            Some(hovered),
            "an inherited SVG must follow its parent's live group-hover foreground"
        );
        assert_eq!(
            explicit
                .lock()
                .expect("explicit tint observation lock")
                .last()
                .copied(),
            Some(explicit_tint.into()),
            "an explicit tint must remain unchanged across parent hover"
        );

        let path = lucide_asset_path(IconName::Terminal, gpui::px(14.0), gpui::px(14.0))
            .expect("the product Terminal glyph is embedded");
        let bytes = EmbeddedAssets
            .load(&path)
            .expect("asset lookup")
            .expect("prepared Terminal glyph");
        cx.update(|_, cx| {
            let rendered = gpui::Image::from_bytes(gpui::ImageFormat::Svg, bytes.into_owned())
                .to_image_data(cx.svg_renderer())
                .expect("inherited glyph must rasterize through GPUI");
            assert!(
                rendered
                    .as_bytes(0)
                    .expect("rasterized SVG frame")
                    .chunks_exact(4)
                    .any(|pixel| pixel[3] != 0),
                "the inherited glyph raster must contain painted pixels"
            );
        });
    }

    #[test]
    fn embedded_product_lucide_sources_have_unique_round_trip_assets() {
        let mut slugs = HashSet::new();
        let glyphs = LUCIDE_SOURCES
            .iter()
            .map(|(name, slug, _)| {
                assert!(
                    slugs.insert(*slug),
                    "duplicate embedded Lucide slug: {slug}"
                );
                let path = lucide_asset_path(*name, gpui::px(14.0), gpui::px(14.0))
                    .expect("every embedded product glyph has an asset path");
                assert_eq!(path, format!("{LUCIDE_ASSET_PREFIX}{slug}/14/1.svg"));
                assert!(
                    EmbeddedAssets.load(&path).expect("asset lookup").is_some(),
                    "embedded product glyph must round-trip through the asset source: {slug}"
                );
                name.unicode()
            })
            .collect::<HashSet<_>>();

        assert_eq!(slugs.len(), LUCIDE_SOURCES.len());
        assert_eq!(glyphs.len(), LUCIDE_SOURCES.len());
        assert!(glyphs.iter().all(|glyph| !glyph.is_whitespace()));
    }

    #[test]
    fn pin_off_optical_correction_preserves_the_nominal_layout_box() {
        assert_eq!(
            optical_artwork_size(IconSource::Lucide(IconName::PinOff), gpui::px(15.0)),
            gpui::px(13.0)
        );
        assert_eq!(
            optical_artwork_size(IconSource::Lucide(IconName::Pin), gpui::px(15.0)),
            gpui::px(15.0)
        );
    }

    #[test]
    fn prepared_lucide_assets_normalize_stroke_after_view_box_scaling() {
        let path = lucide_asset_path(IconName::X, gpui::px(15.0), gpui::px(15.0))
            .expect("the product X glyph is embedded");
        let bytes = EmbeddedAssets
            .load(&path)
            .expect("asset lookup")
            .expect("prepared Lucide asset");
        let svg = std::str::from_utf8(&bytes).expect("official Lucide SVG is UTF-8");
        assert!(svg.contains("stroke-width=\"1.600000\""));

        let path = lucide_asset_path(IconName::PinOff, gpui::px(18.0), gpui::px(15.0))
            .expect("the product PinOff glyph is embedded");
        let bytes = EmbeddedAssets
            .load(&path)
            .expect("asset lookup")
            .expect("prepared optical Lucide asset");
        let svg = std::str::from_utf8(&bytes).expect("official Lucide SVG is UTF-8");
        assert!(svg.contains("stroke-width=\"3.200000\""));
        assert_eq!(normalized_stroke_width(gpui::px(17.0)), 1.0);
        assert_eq!(normalized_stroke_width(gpui::px(18.0)), 2.0);

        // Product roles can produce artwork from 9 through 36 points after the text-role clamp,
        // glyph offsets, and PinOff optical correction. The embedded sources' nearest geometry is
        // two view-box units from an edge. The normalized stroke keeps a positive geometric margin
        // at both extremes; an antialiased outer raster pixel is therefore not evidence of clipping.
        let edge_margin = |artwork_size: f32, stroke_width: f32| {
            let source_width = stroke_width * 24.0 / artwork_size;
            (2.0 - source_width / 2.0) * artwork_size / 24.0
        };
        assert!(edge_margin(9.0, 1.0) > 0.0);
        assert!(edge_margin(36.0, 2.0) > 0.0);
    }

    #[test]
    fn prepared_lucide_assets_reject_unbounded_or_unknown_requests() {
        assert!(prepared_lucide_asset("spaceterm-ui/lucide/x/41/1.svg").is_none());
        assert!(prepared_lucide_asset("spaceterm-ui/lucide/x/12/3.svg").is_none());
        assert!(prepared_lucide_asset("spaceterm-ui/lucide/not-an-icon/12/1.svg").is_none());
        assert!(lucide_asset_path(IconName::X, gpui::px(41.0), gpui::px(41.0)).is_none());
        assert!(lucide_asset_path(IconName::PinOff, gpui::px(8.0), gpui::px(7.0)).is_none());
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
            vec![
                CustomIconName::PanelLeft.path().into(),
                CustomIconName::PanelRight.path().into(),
                CustomIconName::Plus.path().into(),
                CustomIconName::FilterCircle.path().into(),
                SharedString::from(path),
                CustomIconName::GlobePlus.path().into(),
                CustomIconName::RectangleStackBadgePlus.path().into()
            ]
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
        for (icon, width) in [
            (CustomIconName::PanelLeft, 24),
            (CustomIconName::PanelRight, 24),
            (CustomIconName::Plus, 24),
            (CustomIconName::RectangleStack, 24),
            (CustomIconName::FilterCircle, 22),
            (CustomIconName::GlobePlus, 33),
            (CustomIconName::RectangleStackBadgePlus, 28),
        ] {
            let bytes = EmbeddedAssets
                .load(icon.path())
                .expect("asset lookup")
                .expect("bundled rectangle.stack asset");
            cx.update(|cx| {
                let rendered = gpui::Image::from_bytes(gpui::ImageFormat::Svg, bytes.into_owned())
                    .to_image_data(cx.svg_renderer())
                    .expect("bundled vector should parse and rasterize through GPUI");
                assert_eq!(
                    rendered.size(0),
                    size(gpui::DevicePixels(width), gpui::DevicePixels(width))
                );
                let pixels = rendered.as_bytes(0).expect("rasterized SVG frame");
                assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 255));
                let width = width as usize;
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
