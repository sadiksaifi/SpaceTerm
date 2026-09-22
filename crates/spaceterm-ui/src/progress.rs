//! Shared progress values, presentation state, and the reusable progress indicators.
//!
//! The indicators paint one operation's extent and nothing else. They carry no task name, no
//! percentage, no elapsed or remaining time, and no success, warning, paused, or error intent:
//! every visible word and every outcome color belongs to the surface that owns the operation and
//! sits outside the primitive. Reaching the maximum value neither completes nor hides an
//! indicator, so the owner decides when the work is over and the indicator goes away.
//!
//! The linear bar and the circular ring are separate types with separate treatments, so one
//! operation keeps one shape from start to finish. Determinate work fills a restrained track:
//! leading to trailing on the bar, clockwise from twelve o'clock on the ring. Indeterminate work
//! is activity rather than extent: the bar fills end to end and animates its shade along its
//! length, while [`FrameSpinner`] advances a compact sequence of monochrome dot frames.
//!
//! Both indicators paint in the application's installed colors. A ring may instead inherit the
//! semantic foreground of the surface it is embedded in, for a slot whose contrast the embedder
//! has already resolved against the one background under it.

use std::{error::Error, fmt, time::Duration};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, ElementId, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, PathBuilder, Pixels, RenderOnce,
    Rgba, SharedString, Styled as _, Window, canvas, div, point, prelude::FluentBuilder as _, px,
    relative,
};

/// Error constructing a normalized determinate progress value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressValueError {
    /// NaN and positive or negative infinity cannot represent determinate progress.
    NotFinite,
}

impl fmt::Display for ProgressValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "determinate progress must be finite")
    }
}

impl Error for ProgressValueError {}

/// Finite normalized progress in the inclusive range `0.0..=1.0`.
///
/// ```
/// use spaceterm_ui::DeterminateProgress;
///
/// let progress = DeterminateProgress::new(1.25)?;
/// assert_eq!(progress.value(), 1.0);
/// assert!(progress.is_maximum());
/// # Ok::<(), spaceterm_ui::ProgressValueError>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeterminateProgress(f32);

impl DeterminateProgress {
    /// Normalizes a finite value by clamping it to the inclusive unit range.
    pub fn new(value: f64) -> Result<Self, ProgressValueError> {
        if !value.is_finite() {
            return Err(ProgressValueError::NotFinite);
        }
        Ok(Self(value.clamp(0.0, 1.0) as f32))
    }

    /// Returns the normalized finite value.
    pub const fn value(self) -> f32 {
        self.0
    }

    /// Returns whether the presentation reached the maximum value.
    ///
    /// Reaching one does not complete or hide the operation. The owner controls lifecycle.
    pub const fn is_maximum(self) -> bool {
        self.0 >= 1.0
    }
}

/// Progress with explicit determinate and indeterminate states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProgressState {
    /// Work has no knowable normalized completion value.
    Indeterminate,
    /// Work has a finite normalized completion value.
    Determinate(DeterminateProgress),
}

/// Standard sizes for progress indicators.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProgressSize {
    /// Dense indicators embedded in status rows, captions, and list rows.
    Compact,
    /// Ordinary indicators in panels, sheets, and modal content.
    #[default]
    Regular,
}

/// Whether indeterminate activity animates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProgressMotion {
    /// Indeterminate activity travels across the bar and advances through spinner frames.
    #[default]
    Standard,
    /// Indeterminate activity holds one static mark, so nothing on screen moves.
    Reduced,
}

/// Application-owned colors shared by every progress indicator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressPaint {
    track: Rgba,
    indicator: Rgba,
}

impl ProgressPaint {
    /// Creates resolved track and indicator paint.
    ///
    /// The track carries the extent still to come: a bar's unfilled remainder and a determinate
    /// ring's complete circle. The indicator carries determinate fill and the indeterminate bar's
    /// shades. Neither color changes with an outcome, because the primitive holds no outcome.
    pub const fn new(track: Rgba, indicator: Rgba) -> Self {
        Self { track, indicator }
    }
}

/// Bounded geometry for one progress size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressMetrics {
    bar_thickness: Pixels,
    bar_corner_radius: Pixels,
    ring_diameter: Pixels,
    ring_thickness: Pixels,
}

impl ProgressMetrics {
    /// Creates the complete geometry for one size: the bar's track thickness and corner radius,
    /// then the ring's outer diameter and stroke thickness.
    ///
    /// A bar takes its length from the container it fills and keeps only its thickness here.
    pub const fn new(
        bar_thickness: Pixels,
        bar_corner_radius: Pixels,
        ring_diameter: Pixels,
        ring_thickness: Pixels,
    ) -> Self {
        Self {
            bar_thickness,
            bar_corner_radius,
            ring_diameter,
            ring_thickness,
        }
    }

    fn scaled(self, spacing_scale: f32) -> Self {
        let spacing_scale = crate::appearance::normalized_scale(spacing_scale);
        Self {
            bar_thickness: self.bar_thickness * spacing_scale,
            // A progress bar is a capsule rather than a semantic rounded surface, so its radius
            // follows the scaled bar thickness.
            bar_corner_radius: self.bar_corner_radius * spacing_scale,
            ring_diameter: self.ring_diameter * spacing_scale,
            ring_thickness: self.ring_thickness * spacing_scale,
        }
    }
}

/// The bounded set of installed progress geometries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressSizes {
    compact: ProgressMetrics,
    regular: ProgressMetrics,
}

impl ProgressSizes {
    /// Creates the complete catalog of named progress geometries.
    pub const fn new(compact: ProgressMetrics, regular: ProgressMetrics) -> Self {
        Self { compact, regular }
    }

    const fn metrics(self, size: ProgressSize) -> ProgressMetrics {
        match size {
            ProgressSize::Compact => self.compact,
            ProgressSize::Regular => self.regular,
        }
    }

    fn scaled(self, spacing_scale: f32) -> Self {
        Self {
            compact: self.compact.scaled(spacing_scale),
            regular: self.regular.scaled(spacing_scale),
        }
    }
}

/// Application-installed presentation for every progress indicator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressTheme {
    paint: ProgressPaint,
    sizes: ProgressSizes,
    motion: ProgressMotion,
}

impl ProgressTheme {
    /// Creates a complete progress theme from resolved paint, the named geometries, and the
    /// application's resolved motion preference.
    pub const fn new(paint: ProgressPaint, sizes: ProgressSizes, motion: ProgressMotion) -> Self {
        Self {
            paint,
            sizes,
            motion,
        }
    }

    pub(crate) fn scaled_metrics(self, _text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            sizes: self.sizes.scaled(spacing_scale),
            ..self
        }
    }
}

impl Global for ProgressTheme {}

/// One complete pass of the shade crest along an indeterminate bar.
const BAR_CREST_PASS: Duration = Duration::from_millis(1_100);

/// The opacity an indeterminate bar's accent fill rests at between crests.
///
/// The bar is filled end to end, so the resting fill has to read as accent rather than as a track
/// while staying clearly under the solid fill a determinate bar reaches at its maximum.
const BAR_RESTING_OPACITY: f32 = 0.55;

/// The crest's stacked shade layers: each layer's share of the layer around it, then its opacity
/// over that layer.
///
/// The outermost share is the crest's own share of the bar. Nesting the layers instead of stepping
/// one band's color keeps the crest's ends soft without a gradient, so the shade rises and falls
/// along the fill rather than sliding across it as a segment.
const BAR_CREST_LAYERS: [(f32, f32); 3] = [(0.46, 0.18), (0.56, 0.22), (0.3, 0.3)];

/// Monochrome frames from the `Dots` visual reference credited in packaged notices.
///
/// The sequence lights the six-dot ring and walks the unlit gap around it, so what travels is the
/// gap and the ring itself stays whole. That is what reads as rotation. A sequence that instead
/// lights one column solid and walks a single dot beside it reads as a bar with a mark sliding
/// past it, which is not the same motion.
pub(crate) const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPINNER_FRAME_INTERVAL: Duration = Duration::from_millis(80);

/// Unicode Braille bit, column, and row for each of the eight possible dots.
const BRAILLE_DOTS: [(u8, u8, u8); 8] = [
    (0, 0, 0),
    (1, 0, 1),
    (2, 0, 2),
    (6, 0, 3),
    (3, 1, 0),
    (4, 1, 1),
    (5, 1, 2),
    (7, 1, 3),
];

/// The columns and rows the installed frame sequence lights.
///
/// `Dots` never lights the fourth row, so laying its artwork over a full eight-dot cell would
/// leave the bottom quarter of the square empty and hang the ring above the center of whatever
/// text sits beside it. A test holds the sequence to this extent.
const SPINNER_COLUMNS: f32 = 2.0;
const SPINNER_ROWS: f32 = 3.0;

/// A dot's diameter as a share of the distance between neighboring dot centers.
///
/// Physical Braille sets a 1.5 mm dot on a 2.5 mm pitch. Holding near that ratio leaves a gap
/// between every pair of neighbors in both axes, so lit dots stay countable instead of fusing
/// into a bar. The ratio sits slightly under the physical one because these dots are painted a
/// few pixels wide, where antialiasing spreads each edge and closes a gap the geometry still has.
const SPINNER_DOT_PITCH_RATIO: f32 = 0.55;

/// The cell's height as a share of the square the spinner occupies.
///
/// The spinner takes the same slot as a drawn icon or a reported glyph, and neither of those fills
/// its box: a glyph set at the slot's size inks roughly two thirds of that height and leaves the
/// rest to ascent and descent. Reaching the square's edges instead would put the spinner on a
/// heavier optical weight than everything it sits beside, which is the whole complaint against a
/// spinner that looks too big. Insetting to a glyph's ink proportion settles it onto the same
/// weight, and the dots shrink with the cell because one pitch derives them both.
const SPINNER_CELL_HEIGHT_RATIO: f32 = 0.7;

/// An inherited ring's determinate track, as a share of the inherited color's own opacity.
///
/// An inherited ring has one color to work from, so the extent still to come is that same color
/// held well back rather than a second color introduced here. Holding it back this far keeps the
/// circle behind the accent arc at a small diameter instead of competing with it, while leaving
/// enough of the circle visible that a low extent still reads as progress along a track.
const INHERITED_TRACK_OPACITY: f32 = 0.28;

/// A horizontal progress bar for determinate and indeterminate work.
///
/// The bar fills the width it is given and keeps the thin thickness its installed size supplies,
/// so callers control its length through their own layout. Determinate progress fills the track
/// from the leading edge of the reading order. Indeterminate progress fills the complete track and
/// runs one soft shade crest along it, or holds that crest still at the center under reduced
/// motion.
///
/// ```ignore
/// ProgressBar::new("restore", "Restoring session", state).size(ProgressSize::Compact)
/// ```
#[derive(IntoElement)]
pub struct ProgressBar {
    id: ElementId,
    name: SharedString,
    state: ProgressState,
    size: ProgressSize,
    right_to_left: bool,
    debug_selector: Option<SharedString>,
}

impl ProgressBar {
    /// Creates a bar for one named operation.
    ///
    /// The name identifies the operation for the owner's own visible text and diagnostics. The
    /// bar never paints it.
    pub fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        state: ProgressState,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            state,
            size: ProgressSize::default(),
            right_to_left: false,
            debug_selector: None,
        }
    }

    /// Selects one installed geometry.
    pub fn size(mut self, size: ProgressSize) -> Self {
        self.size = size;
        self
    }

    /// Fills and travels from the trailing physical edge for right-to-left reading order.
    pub fn right_to_left(mut self, right_to_left: bool) -> Self {
        self.right_to_left = right_to_left;
        self
    }

    /// Overrides the stable selector prefix used by GPUI interaction tests, which otherwise
    /// derives from the bar's own identifier.
    pub fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }
}

impl RenderOnce for ProgressBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *crate::floating_surface::hosted_progress_theme(cx);
        let metrics = theme.sizes.metrics(self.size);
        let selector = self
            .debug_selector
            .unwrap_or_else(|| SharedString::from(self.id.to_string()));
        debug_assert!(
            !self.name.is_empty(),
            "a progress indicator needs a name for its owner's visible text"
        );
        let track_selector = selector.clone();
        div()
            .debug_selector(move || format!("{track_selector}-track"))
            .flex()
            .flex_row()
            .items_center()
            .when(self.right_to_left, |track| track.justify_end())
            .w_full()
            .flex_none()
            .h(metrics.bar_thickness)
            // The track clips its own corners, so a fill or a crest stays inside the capsule
            // rather than squaring it off at either end.
            .overflow_hidden()
            .rounded(metrics.bar_corner_radius)
            .bg(theme.paint.track)
            .child(bar_fill(
                &self.id,
                &selector,
                self.state,
                self.right_to_left,
                metrics,
                theme,
            ))
    }
}

/// A small GPUI-native frame spinner for indeterminate activity.
///
/// The spinner inherits the surrounding semantic foreground color, holds a fixed square extent,
/// and advances only while its element remains in the tree. Reduced Motion keeps the first frame
/// visible without installing an animation.
#[derive(IntoElement)]
pub struct FrameSpinner {
    id: ElementId,
    name: SharedString,
    size: ProgressSize,
    debug_selector: Option<SharedString>,
}

impl FrameSpinner {
    /// Creates a spinner for one named operation.
    pub fn new(id: impl Into<ElementId>, name: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            size: ProgressSize::default(),
            debug_selector: None,
        }
    }

    /// Selects one installed compact or regular geometry.
    pub fn size(mut self, size: ProgressSize) -> Self {
        self.size = size;
        self
    }

    /// Overrides the stable selector prefix used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }
}

impl RenderOnce for FrameSpinner {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *crate::floating_surface::hosted_progress_theme(cx);
        let extent = theme.sizes.metrics(self.size).ring_diameter;
        let selector = self
            .debug_selector
            .unwrap_or_else(|| SharedString::from(self.id.to_string()));
        debug_assert!(
            !self.name.is_empty(),
            "a frame spinner needs a name for its owner's visible text"
        );
        let root_selector = selector.clone();
        let root = div()
            .debug_selector(move || root_selector.to_string())
            .flex_none()
            .size(extent);
        match theme.motion {
            ProgressMotion::Reduced => {
                let reduced_selector = selector.clone();
                root.debug_selector(move || format!("{reduced_selector}-reduced-motion"))
                    .child(spinner_frame(selector, extent, 0))
                    .into_any_element()
            }
            ProgressMotion::Standard => {
                let frame_selector = selector.clone();
                root.with_animation(
                    ElementId::NamedChild(Box::new(self.id), "frames".into()),
                    Animation::new(SPINNER_FRAME_INTERVAL * SPINNER_FRAMES.len() as u32).repeat(),
                    move |spinner, delta| {
                        spinner.child(spinner_frame(
                            frame_selector.clone(),
                            extent,
                            spinner_frame_index(delta),
                        ))
                    },
                )
                .into_any_element()
            }
        }
    }
}

fn spinner_frame(selector: SharedString, extent: Pixels, index: usize) -> gpui::Div {
    div()
        .debug_selector(move || format!("{selector}-frame"))
        .size_full()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    let color = window.text_style().color;
                    for dot in spinner_dot_bounds(bounds, index) {
                        window
                            .paint_quad(gpui::fill(dot, color).corner_radii(dot.size.width / 2.0));
                    }
                },
            )
            .size(extent),
        )
}

/// Active dots for one `Dots` frame, laid out on a centered Braille lattice.
///
/// One pitch governs both axes, so the two columns sit exactly as far apart as consecutive rows
/// and the cell keeps the tall, narrow proportion a Braille glyph has. The cell is centered in the
/// square at a glyph's ink height rather than stretched to the square's edges. Stretching it is
/// what turns each column into a solid bar and the ring into an exclamation mark.
pub(crate) fn spinner_dot_bounds(
    bounds: Bounds<Pixels>,
    index: usize,
) -> impl Iterator<Item = Bounds<Pixels>> {
    let extent = bounds.size.width.min(bounds.size.height).max(px(0.0));
    let cell_height = extent * SPINNER_CELL_HEIGHT_RATIO;
    let pitch = cell_height / (SPINNER_ROWS - 1.0 + SPINNER_DOT_PITCH_RATIO);
    let diameter = pitch * SPINNER_DOT_PITCH_RATIO;
    let cell_width = pitch * (SPINNER_COLUMNS - 1.0) + diameter;
    let left = bounds.origin.x + (bounds.size.width - cell_width) / 2.0;
    let top = bounds.origin.y + (bounds.size.height - cell_height) / 2.0;
    let mask = (u32::from(SPINNER_FRAMES[index]) - 0x2800) as u8;

    BRAILLE_DOTS
        .into_iter()
        .filter(move |(bit, _, _)| mask & (1 << bit) != 0)
        .map(move |(_, column, row)| {
            Bounds::new(
                point(
                    left + pitch * f32::from(column),
                    top + pitch * f32::from(row),
                ),
                gpui::size(diameter, diameter),
            )
        })
}

pub(crate) fn spinner_frame_index(delta: f32) -> usize {
    ((delta.clamp(0.0, 1.0 - f32::EPSILON) * SPINNER_FRAMES.len() as f32) as usize)
        .min(SPINNER_FRAMES.len() - 1)
}

/// Paints what a bar's track contains for one progress state.
fn bar_fill(
    id: &ElementId,
    selector: &SharedString,
    state: ProgressState,
    right_to_left: bool,
    metrics: ProgressMetrics,
    theme: ProgressTheme,
) -> AnyElement {
    match state {
        ProgressState::Determinate(progress) => {
            let indicator_selector = selector.clone();
            div()
                .debug_selector(move || format!("{indicator_selector}-indicator"))
                .flex_none()
                .h(metrics.bar_thickness)
                // The fill measures the track it sits in, so one normalized value is the whole
                // geometry and no call site converts progress into pixels.
                .w(relative(progress.value()))
                .rounded(metrics.bar_corner_radius)
                .bg(theme.paint.indicator)
                .into_any_element()
        }
        ProgressState::Indeterminate => {
            let activity_selector = selector.clone();
            // Indeterminate work has no extent to show, so the accent covers the complete track
            // and the shade does the talking. One short segment crossing an empty track would
            // report a position the operation does not have.
            let activity = div()
                .debug_selector(move || format!("{activity_selector}-activity"))
                .relative()
                .flex_none()
                .w_full()
                .h(metrics.bar_thickness)
                .rounded(metrics.bar_corner_radius)
                .bg(shaded(theme.paint.indicator, BAR_RESTING_OPACITY));
            match theme.motion {
                ProgressMotion::Reduced => {
                    let reduced_selector = selector.clone();
                    activity
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .child(
                            bar_crest(metrics, theme.paint.indicator).debug_selector(move || {
                                format!("{reduced_selector}-reduced-motion")
                            }),
                        )
                        .into_any_element()
                }
                ProgressMotion::Standard => activity
                    .child(
                        bar_crest(metrics, theme.paint.indicator)
                            .absolute()
                            .top_0()
                            .with_animation(
                                ElementId::NamedChild(Box::new(id.clone()), "activity".into()),
                                Animation::new(BAR_CREST_PASS).repeat(),
                                move |crest, delta| {
                                    crest.left(relative(crest_offset(delta, right_to_left)))
                                },
                            ),
                    )
                    .into_any_element(),
            }
        }
    }
}

/// Builds the crest that lifts the filled bar's shade, softest layer outermost.
fn bar_crest(metrics: ProgressMetrics, indicator: Rgba) -> gpui::Div {
    let mut crest: Option<gpui::Div> = None;
    for (share, opacity) in BAR_CREST_LAYERS.iter().rev().copied() {
        let mut layer = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .h(metrics.bar_thickness)
            .w(relative(share))
            .rounded(metrics.bar_corner_radius)
            .bg(shaded(indicator, opacity));
        if let Some(inner) = crest.take() {
            layer = layer.child(inner);
        }
        crest = Some(layer);
    }
    crest.expect("a crest has at least one shade layer")
}

/// Returns the crest's leading offset at one point of its pass.
///
/// The crest enters from the edge the reading order starts at and leaves past the opposite edge, so
/// one pass covers the bar plus the crest's own width and the shade never stalls at either end.
fn crest_offset(delta: f32, right_to_left: bool) -> f32 {
    let (crest, _) = BAR_CREST_LAYERS[0];
    let travel = 1.0 + crest;
    if right_to_left {
        1.0 - delta * travel
    } else {
        delta * travel - crest
    }
}

/// Returns paint at a share of its own opacity, so every shade comes from the installed color.
fn shaded(paint: Rgba, opacity: f32) -> Rgba {
    Rgba {
        a: paint.a * opacity.clamp(0.0, 1.0),
        ..paint
    }
}

/// A circular progress indicator for determinate work.
///
/// The ring keeps the small square extent its installed size supplies and never stretches, so it
/// sits inside rows and beside text without changing their height. Determinate progress sweeps one
/// continuous accent arc clockwise from twelve o'clock over a complete neutral circle. Prefer the
/// ring for bounded progress in rows too constrained for a bar, and use [`FrameSpinner`] when the
/// completion value is unknown.
///
/// The ring paints in the installed progress colors unless the embedding surface claims that
/// decision with [`ProgressRing::inherited`].
#[derive(IntoElement)]
pub struct ProgressRing {
    id: ElementId,
    name: SharedString,
    progress: DeterminateProgress,
    size: ProgressSize,
    inherited: bool,
    debug_selector: Option<SharedString>,
}

impl ProgressRing {
    /// Creates a ring for one named operation.
    ///
    /// The name identifies the operation for the owner's own visible text and diagnostics. The
    /// ring never paints it.
    pub fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        progress: DeterminateProgress,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            progress,
            size: ProgressSize::default(),
            inherited: false,
            debug_selector: None,
        }
    }

    /// Selects one installed geometry.
    pub fn size(mut self, size: ProgressSize) -> Self {
        self.size = size;
        self
    }

    /// Paints the ring in the semantic foreground of the surface around it, and derives its
    /// determinate track from that same color.
    ///
    /// Use this where the embedding surface rather than the application catalog owns the contrast
    /// decision: a status slot whose foreground is already resolved against the one background it
    /// rests on, which an installed accent cannot know. The caller still hands the ring no color.
    /// The ring reads the text color in effect where it paints, so it follows that surface's
    /// active, inactive, and hovered paints on its own, and it carries no more outcome meaning
    /// than a themed ring does.
    pub fn inherited(mut self) -> Self {
        self.inherited = true;
        self
    }

    /// Overrides the stable selector prefix used by GPUI interaction tests, which otherwise
    /// derives from the ring's own identifier.
    pub fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }
}

impl RenderOnce for ProgressRing {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *crate::floating_surface::hosted_progress_theme(cx);
        let metrics = theme.sizes.metrics(self.size);
        let paint = if self.inherited {
            RingPaint::Inherited
        } else {
            RingPaint::Installed(theme.paint)
        };
        let selector = self
            .debug_selector
            .unwrap_or_else(|| SharedString::from(self.id.to_string()));
        debug_assert!(
            !self.name.is_empty(),
            "a progress indicator needs a name for its owner's visible text"
        );
        let track_selector = selector.clone();
        div()
            .debug_selector(move || format!("{track_selector}-track"))
            .relative()
            .flex_none()
            .w(metrics.ring_diameter)
            .h(metrics.ring_diameter)
            // Determinate work runs against the complete circle, so the extent still to come stays
            // visible and only the accent arc changes.
            .child(ring_arc(
                metrics.ring_thickness,
                paint,
                RingRole::Track,
                0.0,
                1.0,
            ))
            .child(ring_figure(&selector, self.progress, metrics, paint))
    }
}

/// Where a ring's two colors come from.
#[derive(Clone, Copy)]
enum RingPaint {
    /// The application's installed progress paint.
    Installed(ProgressPaint),
    /// The semantic foreground of the surface the ring paints on.
    Inherited,
}

impl RingPaint {
    /// Resolves both colors inside the paint pass.
    ///
    /// An inherited ring can resolve nothing while it is being built: the surrounding text style
    /// only covers it once its ancestors are painting, so the color is read here and nowhere
    /// earlier.
    fn resolve(self, window: &Window) -> ProgressPaint {
        match self {
            Self::Installed(paint) => paint,
            Self::Inherited => {
                let indicator = Rgba::from(window.text_style().color);
                ProgressPaint::new(shaded(indicator, INHERITED_TRACK_OPACITY), indicator)
            }
        }
    }
}

/// Which of a ring's two colors one arc is stroked in.
#[derive(Clone, Copy)]
enum RingRole {
    Track,
    Indicator,
}

impl RingRole {
    const fn color(self, paint: ProgressPaint) -> Rgba {
        match self {
            Self::Track => paint.track,
            Self::Indicator => paint.indicator,
        }
    }
}

/// Paints the accent figure a ring shows for one progress state.
fn ring_figure(
    selector: &SharedString,
    progress: DeterminateProgress,
    metrics: ProgressMetrics,
    paint: RingPaint,
) -> AnyElement {
    let thickness = metrics.ring_thickness;
    let indicator_selector = selector.clone();
    ring_overlay(metrics)
        .debug_selector(move || format!("{indicator_selector}-indicator"))
        .child(ring_arc(
            thickness,
            paint,
            RingRole::Indicator,
            0.0,
            progress.value(),
        ))
        .into_any_element()
}

/// Returns an overlay covering the ring exactly, so an arc shares the track's coordinates.
///
/// The arcs paint through a canvas, which carries no identifier of its own, so each one sits
/// inside an overlay that holds the stable selector for it.
fn ring_overlay(metrics: ProgressMetrics) -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .w(metrics.ring_diameter)
        .h(metrics.ring_diameter)
}

/// Draws one continuous arc of the ring's stroke.
///
/// Turns run clockwise from twelve o'clock: zero begins at the top of the ring and one is a
/// complete revolution.
fn ring_arc(
    thickness: Pixels,
    paint: RingPaint,
    role: RingRole,
    start: f32,
    sweep: f32,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let color = role.color(paint.resolve(window));
            paint_ring_arc(bounds, thickness, color, start, sweep, window);
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w_full()
    .h_full()
}

/// Paints one arc as a single stroked path.
fn paint_ring_arc(
    bounds: Bounds<Pixels>,
    thickness: Pixels,
    paint: Rgba,
    start: f32,
    sweep: f32,
    window: &mut Window,
) {
    // The ring stays circular inside whatever extent it is given, so the shorter side rules.
    let extent = f32::from(bounds.size.width).min(f32::from(bounds.size.height));
    let stroke = f32::from(thickness).min(extent / 2.0);
    if extent <= 0.0 || stroke <= 0.0 || sweep <= 0.0 {
        return;
    }
    let center = bounds.center();
    let center_x = f32::from(center.x);
    let center_y = f32::from(center.y);
    // The stroke straddles its path, so the arc runs down the middle of the ring's width and the
    // painted band stays inside the extent.
    let radius = (extent - stroke) / 2.0;
    let at = |degrees: f32| {
        let radians = degrees.to_radians();
        point(
            px(center_x + radius * radians.sin()),
            px(center_y - radius * radians.cos()),
        )
    };
    let start_degrees = start * 360.0;
    let sweep_degrees = (sweep * 360.0).min(360.0);
    let radii = point(px(radius), px(radius));
    let mut arc = PathBuilder::stroke(px(stroke));
    arc.move_to(at(start_degrees));
    if sweep_degrees >= 360.0 {
        // A complete revolution ends where it begins, which no single arc can describe, so the
        // track is two clockwise halves.
        arc.arc_to(radii, px(0.0), false, true, at(start_degrees + 180.0));
        arc.arc_to(radii, px(0.0), false, true, at(start_degrees + 360.0));
    } else {
        arc.arc_to(
            radii,
            px(0.0),
            sweep_degrees > 180.0,
            true,
            at(start_degrees + sweep_degrees),
        );
    }
    if let Ok(path) = arc.build() {
        window.paint_path(path, paint);
    }
}
