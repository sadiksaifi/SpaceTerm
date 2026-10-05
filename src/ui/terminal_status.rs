//! The Terminal glyph a Pane Caption and a Tab item present beside a Terminal's title.
//!
//! Both surfaces describe the same Terminal Session, so the glyph's status treatment is decided
//! here once. The glyph slot carries the status: reported work with a known percentage keeps the
//! glyph and takes the busy color, work whose completion is unknown takes the frame spinner, other
//! work states use distinct shapes and semantic colors, and attention blinks the mark in the
//! warning color. Each state comes from sanitized Terminal Metadata. The metadata owner observes
//! reported title animation; loaders drawn inside terminal cells remain terminal content.
//!
//! Every mark inherits its color rather than taking the installed progress accent, because the
//! status color is resolved here against the exact Pane Caption or Tab surface the glyph rests on
//! and a Pane Caption's surface can be colored by the program running in it.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId,
    Pixels, Rgba, Task, Window, div, px,
};
use spaceterm_ui::{
    DeterminateProgress, FrameSpinner, Icon, IconName, ProgressRing, ProgressSize, ProgressState,
};

use crate::terminal::metadata::{MetadataFreshness, ProgressMetadata, TerminalMetadataSnapshot};
#[cfg(test)]
use crate::terminal::title::ReportedTitle;
use crate::terminal::title::is_glyph_mark;
pub(crate) use crate::terminal::title::reported_title;

/// How long an attention blink holds each of its two colors.
const BLINK_STEP: Duration = Duration::from_millis(500);
/// How many times the glyph blinks before it rests with an additive unread badge.
///
/// Blinking draws the eye when attention arrives. The settled badge keeps unread state present
/// without replacing the underlying work-state shape.
const BLINKS: u32 = 4;
/// Names the Session's reported work for the frame spinner.
///
/// The spinner never paints this, and it stays content-free: nothing a program reported reaches it.
const PROGRESS_NAME: &str = "terminal progress";

/// Whether the active Chrome typography and its selected fallbacks can draw every base in a
/// reported glyph.
///
/// Shape first so the text system chooses the same fallback run painting will use. Then ask the
/// selected fonts for each base character; an unassigned or unsupported scalar has no glyph and
/// leaves the Session's own icon in the slot.
pub(crate) fn reported_glyph_is_drawable(
    glyph: &str,
    font: &gpui::Font,
    font_size: Pixels,
    window: &Window,
) -> bool {
    let text = gpui::SharedString::from(glyph.to_owned());
    let shaped = window.text_system().shape_line(
        text,
        font_size,
        &[gpui::TextRun {
            len: glyph.len(),
            font: font.clone(),
            color: gpui::rgba(0).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    let fonts = shaped
        .runs
        .iter()
        .map(|run| run.font_id)
        .collect::<Vec<_>>();
    let rendered_glyphs = shaped
        .runs
        .iter()
        .map(|run| run.glyphs.len())
        .sum::<usize>();
    rendered_glyphs == 1
        && glyph_bases_are_drawable(glyph, |character| {
            fonts.iter().any(|font| {
                window
                    .text_system()
                    .typographic_bounds(*font, font_size, character)
                    .is_ok()
            })
        })
}

fn glyph_bases_are_drawable(glyph: &str, mut supports: impl FnMut(char) -> bool) -> bool {
    glyph
        .chars()
        .filter(|character| !is_glyph_mark(*character))
        .all(&mut supports)
}

/// Draws a glyph a program reported, in a square of `size`.
fn reported_glyph(glyph: &str, size: Pixels) -> AnyElement {
    div()
        .size(size)
        .flex()
        .items_center()
        .justify_center()
        .text_size(size)
        .child(gpui::SharedString::from(glyph.to_owned()))
        .into_any_element()
}

/// The OSC 9;4 status a Terminal Session last reported, as host chrome presents it.
///
/// A Session whose metadata has gone stale reports nothing, so an exited program never leaves a
/// loader behind in its Pane or Tab.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TerminalProgress {
    #[default]
    None,
    /// Work with a reported completion percentage, clamped to 0 through 100.
    Normal(u8),
    /// Work in progress whose completion is unknown.
    Indeterminate,
    /// Recently observed title animation whose short activity delay elapsed.
    TitleActivity,
    /// Work that reported a failure.
    Error(u8),
    /// Work that reported it is paused.
    Paused(u8),
}

impl TerminalProgress {
    pub(crate) fn from_metadata(
        metadata: &TerminalMetadataSnapshot,
        session_available: bool,
    ) -> Self {
        if !session_available || metadata.freshness != MetadataFreshness::Live {
            return Self::None;
        }
        match metadata.progress {
            ProgressMetadata::None if metadata.title_activity => Self::TitleActivity,
            ProgressMetadata::None => Self::None,
            ProgressMetadata::Normal(percent) => Self::Normal(percent.min(100)),
            ProgressMetadata::Indeterminate => Self::Indeterminate,
            ProgressMetadata::Error(percent) => Self::Error(percent.min(100)),
            ProgressMetadata::Paused(percent) => Self::Paused(percent.min(100)),
        }
    }

    /// The state's stable name, used to identify the rendered mark.
    const fn name(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Normal(_) => Some("normal"),
            Self::Indeterminate => Some("indeterminate"),
            Self::TitleActivity => Some("title-activity"),
            Self::Error(_) => Some("error"),
            Self::Paused(_) => Some("paused"),
        }
    }
}

/// Status colors a host resolves for the surfaces its glyph rests on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StatusColors {
    /// Immediate host tone used to separate the additive unread badge.
    pub(crate) host: Rgba,
    pub(crate) attention: Rgba,
    pub(crate) busy: Rgba,
    pub(crate) error: Rgba,
    pub(crate) paused: Rgba,
}

/// One Terminal Session's glyph and the status it presents.
pub(crate) struct StatusGlyph {
    pub(crate) icon: IconName,
    /// The glyph the program reported for itself, which takes the place of `icon`.
    pub(crate) reported: Option<gpui::SharedString>,
    pub(crate) size: Pixels,
    pub(crate) progress: TerminalProgress,
    pub(crate) attention: bool,
    /// Keys the attention blink's clock so it survives across frames.
    pub(crate) id: ElementId,
    /// Names the glyph as `{prefix}-{progress state}`, its attention state as
    /// `{prefix}-attention`, and the additive badge as `{prefix}-attention-badge`.
    pub(crate) selector_prefix: String,
    pub(crate) colors: StatusColors,
    /// Gives known-percentage work the determinate ring, so busy is not told by color alone.
    pub(crate) differentiate_without_color: bool,
}

impl StatusGlyph {
    /// Draws the glyph in a square of its size.
    ///
    /// A glyph with no status inherits the surrounding text color, so it keeps following the host's
    /// active, inactive, and hovered paints. Work states use distinct shapes and semantic colors:
    /// work with a known percentage keeps the glyph in the busy color, and work whose completion is
    /// unknown takes the frame spinner. Attention blinks the mark, then settles as an additive badge
    /// so the work shape remains. Under Differentiate Without Color, known-percentage work takes the
    /// determinate ring and the attention badge shows from the first blink.
    pub(crate) fn render(self) -> AnyElement {
        let Self {
            icon,
            reported,
            size,
            progress,
            attention,
            id,
            selector_prefix,
            colors,
            differentiate_without_color,
        } = self;
        let state = progress
            .name()
            .map(|name| format!("{selector_prefix}-{name}"));
        let glyph = div()
            .when_some(state, |glyph, state| glyph.debug_selector(move || state))
            .size(size)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center();
        let mark = Mark {
            icon,
            reported,
            size,
            progress,
            colors,
            differentiate_without_color,
            // The animation hangs off this Session's own glyph identity, so one spinner never
            // shares its frame state with another Session's.
            id: ElementId::NamedChild(std::sync::Arc::new(id.clone()), "progress".into()),
            selector: format!("{selector_prefix}-progress"),
        };
        if !attention {
            return glyph.child(mark.render(false)).into_any_element();
        }
        let selector = format!("{selector_prefix}-attention");
        glyph
            .child(Stepped::new(id, BLINK_STEP, BLINKS * 2, move |step| {
                let state_selector = selector.clone();
                let badge_selector = format!("{selector}-badge");
                // The blink is a color change, so under Differentiate Without Color the badge
                // shows from the first step rather than only once the blink settles.
                let badged = step.is_none() || differentiate_without_color;
                let blinked = step.is_some_and(|step| step % 2 == 0);
                let badge_size = attention_badge_size(size);
                div()
                    .debug_selector(move || state_selector)
                    .relative()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(mark.render(blinked))
                    .when(badged, |glyph| {
                        glyph.child(
                            div()
                                .debug_selector(move || badge_selector)
                                .absolute()
                                .top_0()
                                .right_0()
                                .size(badge_size)
                                .rounded(badge_size / 2.0)
                                .border(px(super::chrome_geometry::HAIRLINE))
                                .border_color(colors.host)
                                .bg(colors.attention),
                        )
                    })
                    .into_any_element()
            }))
            .into_any_element()
    }
}

fn attention_badge_size(glyph_size: Pixels) -> Pixels {
    (glyph_size - px(7.0)).max(px(0.0))
}

/// Which status color a glyph takes, before a host resolves it for its surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tint {
    /// The surrounding text color.
    Inherited,
    Attention,
    Busy,
    Error,
    Paused,
}

impl Tint {
    fn color(self, colors: StatusColors) -> Option<Rgba> {
        match self {
            Self::Inherited => None,
            Self::Attention => Some(colors.attention),
            Self::Busy => Some(colors.busy),
            Self::Error => Some(colors.error),
            Self::Paused => Some(colors.paused),
        }
    }
}

/// The tint and opacity `progress` gives a glyph, with attention's color taking precedence while
/// `blinked`.
fn treatment(progress: TerminalProgress, blinked: bool) -> (Tint, f32) {
    if blinked {
        return (Tint::Attention, 1.0);
    }
    match progress {
        TerminalProgress::None => (Tint::Inherited, 1.0),
        TerminalProgress::Normal(_) => (Tint::Busy, 1.0),
        TerminalProgress::Indeterminate | TerminalProgress::TitleActivity => (Tint::Inherited, 1.0),
        TerminalProgress::Error(_) => (Tint::Error, 1.0),
        TerminalProgress::Paused(_) => (Tint::Paused, 1.0),
    }
}

/// Everything the glyph's one slot draws, apart from the blink phase.
///
/// The blink rebuilds the mark twice a second, so what survives a blink is held here and only the
/// phase is passed in.
struct Mark {
    icon: IconName,
    /// The glyph the program reported for itself, which takes the place of `icon`.
    reported: Option<gpui::SharedString>,
    size: Pixels,
    progress: TerminalProgress,
    colors: StatusColors,
    differentiate_without_color: bool,
    /// Keys the progress mark's own animation, so a spinner survives across frames.
    id: ElementId,
    /// Names the frame spinner as `{selector}` and its frames as `{selector}-frame`.
    selector: String,
}

impl Mark {
    /// The mark for this status: the frame spinner, a distinct semantic shape, the program's
    /// reported glyph, or the Session's own glyph, all within the same slot.
    fn render(self, blinked: bool) -> AnyElement {
        let Self {
            icon,
            reported,
            size,
            progress,
            colors,
            differentiate_without_color,
            id,
            selector,
        } = self;
        let (tint, opacity) = treatment(progress, blinked);
        let mark = match (
            status_shape(progress, differentiate_without_color),
            reported,
        ) {
            (StatusShape::Spinner, _) => FrameSpinner::new(id, PROGRESS_NAME)
                .size(ProgressSize::Compact)
                .debug_selector(selector)
                .into_any_element(),
            (StatusShape::Ring(percent), _) => ProgressRing::new(
                id,
                PROGRESS_NAME,
                DeterminateProgress::new(f64::from(percent) / 100.0)
                    .map_or(ProgressState::Indeterminate, ProgressState::Determinate),
            )
            .size(ProgressSize::Compact)
            .inherited()
            .debug_selector(selector)
            .into_any_element(),
            (StatusShape::Error, _) => {
                Icon::inherited(IconName::TriangleAlert, size).into_any_element()
            }
            (StatusShape::Paused, _) => Icon::inherited(IconName::Pause, size).into_any_element(),
            (StatusShape::Glyph, Some(glyph)) => reported_glyph(&glyph, size),
            (StatusShape::Glyph, None) => Icon::inherited(icon, size).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .opacity(opacity)
            .when_some(tint.color(colors), |mark, color| mark.text_color(color))
            .child(mark)
            .into_any_element()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusShape {
    Glyph,
    /// Determinate progress in percent.
    Ring(u8),
    Spinner,
    Error,
    Paused,
}

fn status_shape(progress: TerminalProgress, differentiate_without_color: bool) -> StatusShape {
    match progress {
        // Under Differentiate Without Color the busy color cannot be the only cue, so the work
        // takes the determinate ring, the one shape that also says how far it has come.
        TerminalProgress::Normal(percent) if differentiate_without_color => {
            StatusShape::Ring(percent)
        }
        // A percentage says the work is bounded, which the busy color already carries. A ring in
        // this glyph-sized slot reads as a solid circle rather than as progress, so the glyph stays.
        TerminalProgress::None | TerminalProgress::Normal(_) => StatusShape::Glyph,
        TerminalProgress::Indeterminate | TerminalProgress::TitleActivity => StatusShape::Spinner,
        TerminalProgress::Error(_) => StatusShape::Error,
        TerminalProgress::Paused(_) => StatusShape::Paused,
    }
}

/// Rebuilds its child from a step that advances on a coarse clock while it stays on screen.
///
/// Only the owning view is notified on each step, at the step's pace rather than the display's.
/// The clock stops after `limit` steps and then renders `None`, so a settled animation costs
/// nothing more. The clock restarts when the element leaves the screen and returns.
struct Stepped {
    id: ElementId,
    interval: Duration,
    limit: u32,
    render: Option<Box<dyn FnOnce(Option<u32>) -> AnyElement>>,
}

impl Stepped {
    fn new(
        id: ElementId,
        interval: Duration,
        limit: u32,
        render: impl FnOnce(Option<u32>) -> AnyElement + 'static,
    ) -> Self {
        Self {
            id,
            interval,
            limit,
            render: Some(Box::new(render)),
        }
    }
}

/// The step a [`Stepped`] element renders, or `None` once a bounded clock has finished.
struct StepClock {
    step: Option<u32>,
    _tick: Task<()>,
}

impl StepClock {
    fn start(interval: Duration, limit: u32, cx: &mut gpui::Context<Self>) -> Self {
        Self {
            step: Some(0),
            _tick: cx.spawn(async move |clock, cx| {
                loop {
                    cx.background_executor().timer(interval).await;
                    let running = clock.update(cx, |clock: &mut StepClock, cx| {
                        let next = clock.step.map(|step| step.wrapping_add(1));
                        clock.step = next.filter(|next| *next < limit);
                        cx.notify();
                        clock.step.is_some()
                    });
                    if !matches!(running, Ok(true)) {
                        break;
                    }
                }
            }),
        }
    }
}

impl IntoElement for Stepped {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Stepped {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let (interval, limit) = (self.interval, self.limit);
        let step = window
            .use_keyed_state("clock", cx, move |_, cx| {
                StepClock::start(interval, limit, cx)
            })
            .read(cx)
            .step;
        let render = self.render.take().expect("a Stepped element lays out once");
        let mut child = render(step);
        (child.request_layout(window, cx), child)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        child: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        child: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        child.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::terminal::metadata::MetadataTracker;

    fn metadata(
        progress: ProgressMetadata,
        freshness: MetadataFreshness,
    ) -> TerminalMetadataSnapshot {
        let tracker = MetadataTracker::new(
            crate::local_path::LocalPathSemantics::Posix,
            "/",
            "zsh",
            Default::default(),
            std::time::Instant::now(),
        );
        let mut snapshot = Arc::unwrap_or_clone(tracker.snapshot());
        snapshot.progress = progress;
        snapshot.freshness = freshness;
        snapshot
    }

    /// A bounded animation settles and stops ticking, so a settled cue costs no further frames.
    #[gpui::test]
    fn bounded_step_clock_should_settle_and_stop(cx: &mut gpui::TestAppContext) {
        let interval = Duration::from_millis(10);
        let clock = cx.new(|cx| StepClock::start(interval, 3, cx));
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        let _observation = cx.update(|cx| {
            let notifications = std::rc::Rc::clone(&notifications);
            cx.observe(&clock, move |_, _| {
                notifications.set(notifications.get() + 1)
            })
        });
        let mut steps = vec![clock.read_with(cx, |clock, _| clock.step)];
        for _ in 0..6 {
            cx.executor().advance_clock(interval);
            cx.run_until_parked();
            steps.push(clock.read_with(cx, |clock, _| clock.step));
        }
        assert_eq!(steps, [Some(0), Some(1), Some(2), None, None, None, None]);
        assert_eq!(notifications.get(), 3);
    }

    #[gpui::test]
    fn attention_badges_from_its_first_blink_only_under_differentiate_without_color(
        cx: &mut gpui::TestAppContext,
    ) {
        struct Probe;
        impl gpui::Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                let glyph = |prefix: &str, differentiate_without_color| {
                    StatusGlyph {
                        icon: IconName::Terminal,
                        reported: None,
                        size: px(14.0),
                        progress: TerminalProgress::None,
                        attention: true,
                        id: ElementId::Name(prefix.to_owned().into()),
                        selector_prefix: prefix.to_owned(),
                        colors: StatusColors {
                            host: gpui::rgba(0x000000ff),
                            attention: gpui::rgba(0xffcc00ff),
                            busy: gpui::rgba(0x3399ffff),
                            error: gpui::rgba(0xff3333ff),
                            paused: gpui::rgba(0x888888ff),
                        },
                        differentiate_without_color,
                    }
                    .render()
                };
                div()
                    .child(glyph("color", false))
                    .child(glyph("shape", true))
            }
        }
        let (_, cx) = cx.add_window_view(|_, _| Probe);
        cx.run_until_parked();

        assert!(cx.debug_bounds("color-attention").is_some());
        assert!(cx.debug_bounds("color-attention-badge").is_none());
        assert!(cx.debug_bounds("shape-attention-badge").is_some());
    }

    /// A program's own glyph leaves the title and takes the Session's glyph slot instead.
    #[test]
    fn a_title_should_give_up_the_glyph_the_program_draws_at_its_front() {
        for (reported, glyph, words) in [
            ("\u{2733} Claude Code", Some("\u{2733}"), "Claude Code"),
            ("\u{25d0} Claude Code", Some("\u{25d0}"), "Claude Code"),
            ("\u{280b} building", Some("\u{280b}"), "building"),
            ("\u{1f680} deploying", Some("\u{1f680}"), "deploying"),
            // A glyph is a glyph whether or not it is a symbol, as long as it names nothing alone.
            ("\u{3c0} - SpaceTerm", Some("\u{3c0}"), "SpaceTerm"),
            (
                "\u{2058} diy-nucleus-clients",
                Some("\u{2058}"),
                "diy-nucleus-clients",
            ),
            ("\u{3c0} --help", Some("\u{3c0}"), "--help"),
            ("\u{2733} :memory", Some("\u{2733}"), ":memory"),
            // Marks belong to the glyph they follow.
            (
                "\u{2733}\u{fe0f} Claude Code",
                Some("\u{2733}\u{fe0f}"),
                "Claude Code",
            ),
            ("🇺🇸 build", Some("🇺🇸"), "build"),
            // A Private-Use glyph would paint as a box, so the Session keeps its own.
            ("\u{f0316} nvim", None, "nvim"),
            // Structural candidates are retained until the active font's shaper decides whether
            // they occupy one glyph slot.
            (
                "\u{2726}\u{2726} two frames",
                Some("\u{2726}\u{2726}"),
                "two frames",
            ),
            // Nothing a Session would be named after is a glyph.
            ("zsh", None, "zsh"),
            ("~ zsh", None, "~ zsh"),
            ("~/Projects/api", None, "~/Projects/api"),
            (
                "cargo test -- --nocapture",
                None,
                "cargo test -- --nocapture",
            ),
            (".config", None, ".config"),
            ("R interactive", None, "R interactive"),
            ("\u{6d4b}\u{8bd5} build", None, "\u{6d4b}\u{8bd5} build"),
            ("\u{2733}Claude", None, "\u{2733}Claude"),
            ("\u{2733}", None, "\u{2733}"),
            ("\u{2733} ", None, "\u{2733}"),
            ("", None, ""),
        ] {
            assert_eq!(
                reported_title(reported),
                ReportedTitle { glyph, words },
                "{reported:?}"
            );
        }
    }

    #[test]
    fn a_reported_glyph_requires_font_support_for_every_base() {
        assert!(glyph_bases_are_drawable("\u{3c0}", |character| character == '\u{3c0}'));
        assert!(!glyph_bases_are_drawable("\u{0378}", |_| false));
        assert!(!glyph_bases_are_drawable(
            "\u{1f469}\u{200d}\u{1f4bb}",
            |character| character == '\u{1f469}'
        ));
    }

    /// Each status recolors the glyph, and a blink shows attention over all.
    #[test]
    fn glyph_should_take_the_color_of_its_status() {
        for (progress, resting) in [
            (TerminalProgress::None, (Tint::Inherited, 1.0)),
            (TerminalProgress::Normal(30), (Tint::Busy, 1.0)),
            (TerminalProgress::Indeterminate, (Tint::Inherited, 1.0)),
            (TerminalProgress::TitleActivity, (Tint::Inherited, 1.0)),
            (TerminalProgress::Error(30), (Tint::Error, 1.0)),
            (TerminalProgress::Paused(70), (Tint::Paused, 1.0)),
        ] {
            assert_eq!(treatment(progress, false), resting, "{progress:?}");
            assert_eq!(
                treatment(progress, true),
                (Tint::Attention, 1.0),
                "{progress:?}"
            );
        }
    }

    #[test]
    fn unread_badge_tracks_the_status_icon_catalog_step() {
        assert_eq!(attention_badge_size(px(13.0)), px(6.0));
        assert_eq!(attention_badge_size(px(14.0)), px(7.0));
    }

    #[test]
    fn semantic_statuses_keep_distinct_shapes_when_their_colors_coincide() {
        let shapes = [
            status_shape(TerminalProgress::Indeterminate, false),
            status_shape(TerminalProgress::Error(30), false),
            status_shape(TerminalProgress::Paused(70), false),
        ];

        assert!(shapes[0] != shapes[1] && shapes[0] != shapes[2] && shapes[1] != shapes[2]);
    }

    #[test]
    fn busy_and_attention_colors_stay_readable_on_the_active_tab() {
        const MINIMUM_STATUS_CONTRAST: f64 = 4.5;
        let colors = crate::appearance::ChromeColors::default();
        let surface = colors.tab_active_background;
        let status = colors.status(surface);

        for foreground in [status.busy, status.attention] {
            assert!(foreground.contrast_ratio(surface) >= MINIMUM_STATUS_CONTRAST);
        }
    }

    #[test]
    fn differentiate_without_color_gives_every_tinted_state_a_shape_apart_from_idle() {
        let idle = status_shape(TerminalProgress::None, true);
        for progress in [
            TerminalProgress::Normal(0),
            TerminalProgress::Normal(42),
            TerminalProgress::Normal(100),
            TerminalProgress::Error(30),
            TerminalProgress::Paused(70),
        ] {
            assert_ne!(
                treatment(progress, false).0,
                Tint::Inherited,
                "{progress:?}"
            );
            assert_ne!(status_shape(progress, true), idle, "{progress:?}");
        }
    }

    #[test]
    fn known_percentages_keep_the_glyph_and_unknown_work_spins() {
        for percent in [0, 42, 100] {
            assert_eq!(
                status_shape(TerminalProgress::Normal(percent), false),
                StatusShape::Glyph,
                "{percent}%"
            );
        }
        for progress in [
            TerminalProgress::Indeterminate,
            TerminalProgress::TitleActivity,
        ] {
            assert_eq!(
                status_shape(progress, false),
                StatusShape::Spinner,
                "{progress:?}"
            );
        }
    }

    #[test]
    fn title_activity_is_distinct_from_explicit_indeterminate_progress() {
        let mut running = metadata(ProgressMetadata::None, MetadataFreshness::Live);
        running.title_activity = true;
        assert_eq!(
            TerminalProgress::from_metadata(&running, true),
            TerminalProgress::TitleActivity
        );
        running.progress = ProgressMetadata::Indeterminate;
        assert_eq!(
            TerminalProgress::from_metadata(&running, true),
            TerminalProgress::Indeterminate
        );
    }

    #[test]
    fn progress_should_present_each_reported_state_only_while_metadata_is_live() {
        for (reported, presented) in [
            (ProgressMetadata::None, TerminalProgress::None),
            (ProgressMetadata::Normal(42), TerminalProgress::Normal(42)),
            (
                ProgressMetadata::Indeterminate,
                TerminalProgress::Indeterminate,
            ),
            (ProgressMetadata::Error(30), TerminalProgress::Error(30)),
            (ProgressMetadata::Paused(70), TerminalProgress::Paused(70)),
        ] {
            assert_eq!(
                TerminalProgress::from_metadata(&metadata(reported, MetadataFreshness::Live), true,),
                presented
            );
            assert_eq!(
                TerminalProgress::from_metadata(
                    &metadata(reported, MetadataFreshness::Stale),
                    true,
                ),
                TerminalProgress::None
            );
            assert_eq!(
                TerminalProgress::from_metadata(
                    &metadata(reported, MetadataFreshness::Live),
                    false,
                ),
                TerminalProgress::None
            );
        }
    }
}
