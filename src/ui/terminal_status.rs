//! The Terminal glyph a Pane Caption and a Tab item present beside a Terminal's title.
//!
//! Both surfaces describe the same Terminal Session, so the glyph's status treatment is decided
//! here once. The glyph itself carries the status: OSC 9;4 progress recolors it or, with a reported
//! percentage, takes its place as a ring, and attention blinks it in the warning color. Each state
//! is typed from sanitized Terminal Metadata and never from the title text, which stays opaque: a
//! loader a program draws in its own cells or title is not something host chrome can see or
//! restate.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    LayoutId, PathBuilder, Pixels, Point, Rgba, Task, Window, canvas, div, point, px,
};
use spaceterm_ui::{Icon, IconName};

use crate::terminal::metadata::{MetadataFreshness, ProgressMetadata, TerminalMetadataSnapshot};

/// How long an attention blink holds each of its two colors.
const BLINK_STEP: Duration = Duration::from_millis(500);
/// How many times the glyph blinks before it rests in the warning color.
///
/// Blinking draws the eye when attention arrives. Resting afterwards keeps an unread Tab in the
/// background from repainting its window for as long as it stays unread.
const BLINKS: u32 = 4;
/// How far the ring's stroke sits inside the glyph's square, as a share of its size.
const PROGRESS_STROKE_SHARE: f32 = 0.14;
/// The resting ring behind a reported percentage, as a share of the foreground's opacity.
const PROGRESS_TRACK_OPACITY: f32 = 0.28;

/// How many characters the first word of a title can hold and still be a glyph rather than a word.
const MAXIMUM_GLYPH_CHARS: usize = 2;

/// Characters a title keeps, because a Session can be named after them.
const TITLE_WORD_CHARS: &str = "~/\\._-:@$#([{'\"";

/// Characters that separate a program's glyph from the words after it.
const TITLE_SEPARATOR_CHARS: &str = "-\u{2013}\u{2014}\u{00b7}|:";

/// The share of the glyph's square a reported glyph is drawn at.
///
/// A character carries its own side bearings, so it is drawn a little smaller than a drawn icon
/// to settle on the same visual weight beside one.
const REPORTED_GLYPH_TEXT_SHARE: f32 = 0.86;

/// What a program put at the front of the title it reported, and the words that follow it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReportedTitle<'a> {
    /// The program's own glyph, when it reported one host chrome can draw.
    pub(crate) glyph: Option<&'a str>,
    /// The title without that glyph.
    pub(crate) words: &'a str,
}

/// Splits the glyph a program draws at the front of its own title from the words after it.
///
/// Programs often open the title they report with their own icon or a spinner frame. One Session
/// gets one glyph, so the program's takes the place of the glyph host chrome would draw rather than
/// sitting beside it, and never appears twice. Only a first word that names nothing on its own is
/// taken: one character that is not a letter or digit a Session could be named after, with the
/// marks that belong to it. No glyph is recognised by name, so this stays the same for every
/// program.
pub(crate) fn reported_title(title: &str) -> ReportedTitle<'_> {
    let title = title.trim();
    let plain = ReportedTitle {
        glyph: None,
        words: title,
    };
    let Some(split) = title.find(char::is_whitespace) else {
        return plain;
    };
    let (first, rest) = title.split_at(split);
    if !is_glyph_word(first) {
        return plain;
    }
    let words = rest
        .trim_start_matches(|character: char| {
            character.is_whitespace() || TITLE_SEPARATOR_CHARS.contains(character)
        })
        .trim_end();
    if words.is_empty() {
        return plain;
    }
    ReportedTitle {
        glyph: is_drawable_glyph(first).then_some(first),
        words,
    }
}

/// Whether the first word of a title decorates it rather than naming what the Session is doing.
fn is_glyph_word(word: &str) -> bool {
    let bases = word
        .chars()
        .filter(|character| !is_glyph_mark(*character))
        .count();
    // Several letters name something, in whatever script they are written. Several symbols
    // together are still decoration.
    let decorative = bases == 1 || word.chars().all(|character| !character.is_alphanumeric());
    (1..=MAXIMUM_GLYPH_CHARS).contains(&bases)
        && decorative
        && word.chars().all(|character| {
            !character.is_ascii_alphanumeric() && !TITLE_WORD_CHARS.contains(character)
        })
}

/// Whether a character belongs to the glyph before it rather than standing as one of its own.
fn is_glyph_mark(character: char) -> bool {
    matches!(
        u32::from(character),
        // Variation selectors, the zero-width joiner, and the skin tone modifiers.
        0xFE00..=0xFE0F | 0x200D | 0x1F3FB..=0x1F3FF | 0xE0100..=0xE01EF
    )
}

/// Whether host chrome can draw a reported glyph at the size a Session's own glyph takes.
///
/// A Private-Use character is drawn by the font a program expects rather than by the font the host
/// paints its chrome in, so it would paint as a missing-glyph box. An ASCII one says less than the
/// Session's own glyph does. Either way the Session keeps its own.
fn is_drawable_glyph(glyph: &str) -> bool {
    // Two glyphs would crowd the one square a Session's glyph gets, unless they are joined into one.
    let joined = glyph.contains('\u{200D}');
    let bases = glyph
        .chars()
        .filter(|character| !is_glyph_mark(*character))
        .count();
    (joined || bases == 1)
        && glyph.chars().all(|character| {
            !character.is_ascii()
                && !matches!(
                    u32::from(character),
                    0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
                )
        })
}

/// Draws a glyph a program reported, in a square of `size`.
fn reported_glyph(glyph: &str, size: Pixels) -> AnyElement {
    div()
        .size(size)
        .flex()
        .items_center()
        .justify_center()
        .text_size(size * REPORTED_GLYPH_TEXT_SHARE)
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
    /// Work that reported a failure.
    Error,
    /// Work that reported it is paused.
    Paused,
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
            ProgressMetadata::None => Self::None,
            ProgressMetadata::Normal(percent) => Self::Normal(percent.min(100)),
            ProgressMetadata::Indeterminate => Self::Indeterminate,
            ProgressMetadata::Error(_) => Self::Error,
            ProgressMetadata::Paused(_) => Self::Paused,
        }
    }

    /// The state's stable name, used to identify the rendered mark.
    const fn name(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Normal(_) => Some("normal"),
            Self::Indeterminate => Some("indeterminate"),
            Self::Error => Some("error"),
            Self::Paused => Some("paused"),
        }
    }
}

/// Status colors a host resolves for the surfaces its glyph rests on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StatusColors {
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
    /// Names the glyph as `{prefix}-{progress state}` and its blink as `{prefix}-attention`.
    pub(crate) selector_prefix: String,
    pub(crate) colors: StatusColors,
}

impl StatusGlyph {
    /// Draws the glyph in a square of its size.
    ///
    /// A glyph with no status inherits the surrounding text color, so it keeps following the host's
    /// active, inactive, and hovered paints. Work in progress takes the busy color, as a ring when
    /// it reports a percentage. Error takes the error color, and paused work dims the glyph.
    /// Attention blinks the glyph in the attention color and then leaves it in that color.
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
        if !attention {
            return glyph
                .child(status_mark(icon, reported, size, progress, false, colors))
                .into_any_element();
        }
        let selector = format!("{selector_prefix}-attention");
        glyph
            .child(Stepped::new(id, BLINK_STEP, BLINKS * 2, move |step| {
                // Even steps and the settled state show the attention color.
                let blinked = step.is_none_or(|step| step % 2 == 0);
                div()
                    .debug_selector(move || selector)
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(status_mark(
                        icon,
                        reported.clone(),
                        size,
                        progress,
                        blinked,
                        colors,
                    ))
                    .into_any_element()
            }))
            .into_any_element()
    }
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
        TerminalProgress::Normal(_) | TerminalProgress::Indeterminate => (Tint::Busy, 1.0),
        TerminalProgress::Error => (Tint::Error, 1.0),
        TerminalProgress::Paused => (Tint::Paused, 1.0),
    }
}

/// The glyph for `progress`: a ring in the glyph's place for a reported percentage, otherwise the
/// glyph the program reported, or the Session's own.
fn status_mark(
    icon: IconName,
    reported: Option<gpui::SharedString>,
    size: Pixels,
    progress: TerminalProgress,
    blinked: bool,
    colors: StatusColors,
) -> AnyElement {
    let (tint, opacity) = treatment(progress, blinked);
    let mark = match (progress, reported) {
        // A reported percentage says more than any glyph, so the ring takes the slot.
        (TerminalProgress::Normal(percent), _) => progress_ring(percent, size).into_any_element(),
        (_, Some(glyph)) => reported_glyph(&glyph, size),
        (_, None) => Icon::inherited(icon, size).into_any_element(),
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

fn progress_ring(percent: u8, size: Pixels) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let color = window.text_style().color;
            let track = Hsla {
                a: color.a * PROGRESS_TRACK_OPACITY,
                ..color
            };
            paint_arc(bounds, 0.0, 360.0, track, window);
            paint_arc(bounds, 0.0, f32::from(percent) * 3.6, color, window);
        },
    )
    .size(size)
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

/// Strokes the arc from `start` to `end`, in degrees clockwise from the top of `bounds`.
fn paint_arc(bounds: Bounds<Pixels>, start: f32, end: f32, color: Hsla, window: &mut Window) {
    let sweep = (end - start).clamp(0.0, 360.0);
    if sweep <= 0.0 || color.a <= 0.0 {
        return;
    }
    let side = f32::from(bounds.size.width.min(bounds.size.height));
    let stroke = side * PROGRESS_STROKE_SHARE;
    let radius = (side - stroke) / 2.0;
    let center = bounds.center();
    let at = |degrees: f32| -> Point<Pixels> {
        let radians = degrees.to_radians();
        point(
            center.x + px(radius * radians.sin()),
            center.y - px(radius * radians.cos()),
        )
    };
    let radii = point(px(radius), px(radius));
    let mut path = PathBuilder::stroke(px(stroke));
    path.move_to(at(start));
    // An arc cannot end where it starts, so a full ring is drawn as two halves.
    if sweep >= 360.0 {
        path.arc_to(radii, px(0.0), false, true, at(start + 180.0));
        path.arc_to(radii, px(0.0), false, true, at(start));
    } else {
        path.arc_to(radii, px(0.0), sweep > 180.0, true, at(start + sweep));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
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
            // Marks belong to the glyph they follow.
            (
                "\u{2733}\u{fe0f} Claude Code",
                Some("\u{2733}\u{fe0f}"),
                "Claude Code",
            ),
            // A Private-Use glyph would paint as a box, so the Session keeps its own.
            ("\u{f0316} nvim", None, "nvim"),
            // Two glyphs cannot share one slot, so both go and the Session keeps its own.
            ("\u{2726}\u{2726} two frames", None, "two frames"),
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

    /// Each status recolors the glyph, a paused one dims it, and a blink shows attention over all.
    #[test]
    fn glyph_should_take_the_color_of_its_status() {
        for (progress, resting) in [
            (TerminalProgress::None, (Tint::Inherited, 1.0)),
            (TerminalProgress::Normal(30), (Tint::Busy, 1.0)),
            (TerminalProgress::Indeterminate, (Tint::Busy, 1.0)),
            (TerminalProgress::Error, (Tint::Error, 1.0)),
            (TerminalProgress::Paused, (Tint::Paused, 1.0)),
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
    fn progress_should_present_each_reported_state_only_while_metadata_is_live() {
        for (reported, presented) in [
            (ProgressMetadata::None, TerminalProgress::None),
            (ProgressMetadata::Normal(42), TerminalProgress::Normal(42)),
            (
                ProgressMetadata::Indeterminate,
                TerminalProgress::Indeterminate,
            ),
            (ProgressMetadata::Error(30), TerminalProgress::Error),
            (ProgressMetadata::Paused(70), TerminalProgress::Paused),
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
