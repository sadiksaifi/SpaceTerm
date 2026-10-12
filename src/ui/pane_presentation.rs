//! What a Pane shows over, or in place of, its Terminal while this Client cannot present the
//! Terminal Session live.
//!
//! The Pane keeps measuring its cell grid in every presentation. Only a live presentation accepts
//! input. Only a live or frozen screen may be painted, exposed to accessibility, copied, or given
//! to an input method as context; every other presentation is content-free.
//!
//! The Pane hosts a [`PanePresentationLayer`] in every presentation, live included, with one
//! [`PresentationFocus`] it keeps for its lifetime, and calls
//! [`PanePresentation::traverse_actions`] in its key handler before its input gate. Presses on a
//! presentation reach the Pane, so they focus and select it as they would a live Terminal; the
//! Pane refuses pointer reports, selection, and link hover and activation while the presentation
//! refuses input or content. Scrolling stays with the presentation.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the Terminal Pane adopts these presentations when it hosts server sessions"
    )
)]

#[path = "pane_presentation/content_unavailable.rs"]
mod content_unavailable;
#[cfg(test)]
#[path = "pane_presentation/tests.rs"]
pub(in crate::ui) mod tests;

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ElementId, FocusHandle, KeyDownEvent, ScrollWheelEvent, SharedString, Task,
    Window, accesskit, div, relative,
};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, FloatingRole, FrameSpinner, Icon, IconName, ProgressSize,
};

use crate::appearance::{ChromeColors, Color};
use crate::repository_status::presentation::sanitize_for_display;
use crate::ui::appearance::{ChromeAppearance, gpui_color, shared_chrome};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

pub(in crate::ui) use content_unavailable::{ContentUnavailable, ContentUnavailablePaint};

/// How long a starting Pane shows nothing before it shows progress, so a quick start never
/// flashes an indicator.
pub(crate) const STARTUP_PROGRESS_DELAY: Duration = Duration::from_millis(300);
/// How long a Takeover Screen keeps Take Over unavailable after it appears, so a click meant for
/// the Terminal cannot take control back by landing just as the screen replaces it.
pub(crate) const TAKEOVER_ARMING_DELAY: Duration = Duration::from_millis(500);
/// The longest device or server name a surface shows, in characters.
pub(crate) const MAXIMUM_DISPLAY_NAME_CHARS: usize = 64;
/// How much of the Pane surface a frozen screen lays over its last content, out of 255.
const FROZEN_SCRIM_ALPHA: u8 = 0x59;

/// A name another device or server reported for itself. It is bounded and its control and
/// bidirectional formatting characters are replaced before any surface shows it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DisplayName(Option<SharedString>);

impl DisplayName {
    pub(crate) fn reported(name: &str) -> Self {
        let name = name.trim();
        if name.is_empty() {
            return Self(None);
        }
        Self(Some(
            sanitize_for_display(name, MAXIMUM_DISPLAY_NAME_CHARS).into(),
        ))
    }

    /// A device or server that reported no name.
    pub(crate) const fn unnamed() -> Self {
        Self(None)
    }

    pub(crate) fn get(&self) -> Option<&SharedString> {
        self.0.as_ref()
    }

    fn or(&self, fallback: &'static str) -> SharedString {
        self.0.clone().unwrap_or(SharedString::new_static(fallback))
    }
}

/// Why a Pane's Terminal Session ended while the Pane stayed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionEndReason {
    /// Someone chose End All Sessions on this Pane's server.
    EndAllSessions,
    /// The server restarted and restored this Pane without its session.
    Restored,
}

/// The bounded class of a session failure a Pane presents. It carries no native error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaneFailureClass {
    /// The server could not start a shell for the Pane.
    Launch,
    /// The session stopped without its program exiting.
    Interrupted,
    /// The server is at its session limit.
    ResourceLimit,
    /// Any other failure, presented without detail.
    Unavailable,
}

/// What an offline or reconciling Pane keeps showing from before its server became unreachable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OfflinePrior {
    /// The last screen, frozen.
    Frozen,
    /// The Takeover Screen that was showing. An older screen is never revealed.
    Takeover(DisplayName),
}

/// What a Pane presents in place of a live Terminal. The Pane owns one, and only
/// [`PanePresentation::Live`] gives the Terminal its input.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum PanePresentation {
    #[default]
    Live,
    /// The Pane has not measured its cell grid yet, so no session can start at its size.
    AwaitingMeasurement,
    /// The server is starting the Pane's session at its measured size.
    Starting,
    /// Another device controls the Pane's session.
    Takeover(DisplayName),
    Ended {
        reason: SessionEndReason,
    },
    Failed {
        class: PaneFailureClass,
    },
    /// The Pane's directory is missing or the server cannot reach it.
    DirectoryUnavailable,
    /// The Pane's server cannot be reached.
    Offline {
        prior: OfflinePrior,
    },
    /// The server is reachable again and the Pane is catching up with it.
    Reconciling {
        prior: OfflinePrior,
    },
}

/// What a person asked a presentation to do. The Pane's owner decides what each one means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PanePresentationAction {
    TakeOver,
    StartSession,
    Retry,
    ChooseDirectory,
}

impl PanePresentation {
    /// Whether keys, text, pastes, and pointer reports may reach the Terminal Session.
    pub(crate) fn accepts_input(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// Whether the Pane may paint its Terminal, expose it to accessibility, copy from it, offer it
    /// to Services, or give an input method its text as context. A frozen screen stays readable;
    /// every other presentation that is not live is content-free.
    pub(crate) fn content_eligible(&self) -> bool {
        matches!(
            self,
            Self::Live
                | Self::Offline {
                    prior: OfflinePrior::Frozen
                }
                | Self::Reconciling {
                    prior: OfflinePrior::Frozen
                }
        )
    }

    /// Whether the Terminal shows its last screen, unchanged, under the presentation.
    pub(crate) fn freezes_content(&self) -> bool {
        self.content_eligible() && !self.accepts_input()
    }

    /// Whether the Pane's Take Over command may act now. Plain Tab never reaches the Take Over
    /// button, so a keyboard user takes over through that command, which a deliberate shortcut
    /// operates, or through assistive technology.
    pub(crate) fn offers_take_over(&self) -> bool {
        matches!(self, Self::Takeover(_))
    }

    /// Whether the presentation shows a Takeover Screen, whose Take Over is available or not.
    fn guards_take_over(&self) -> bool {
        matches!(
            self,
            Self::Takeover(_)
                | Self::Offline {
                    prior: OfflinePrior::Takeover(_)
                }
                | Self::Reconciling {
                    prior: OfflinePrior::Takeover(_)
                }
        )
    }

    /// The actions the presentation lets a person operate, in presentation order. `armed` says
    /// whether a Takeover Screen has been showing long enough to offer Take Over.
    fn operable_actions(&self, armed: bool) -> Vec<PanePresentationAction> {
        if let Self::Offline {
            prior: OfflinePrior::Frozen,
        } = self
        {
            return vec![PanePresentationAction::Retry];
        }
        self.screen()
            .map(|screen| {
                screen
                    .actions
                    .iter()
                    .filter(|action| action.available && armed)
                    .map(|action| action.action)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The actions plain Tab and Shift-Tab visit from the Pane. A Takeover Screen has none, so Tab
    /// completion followed by Return or Space, typed for the shell, cannot take control from the
    /// other device.
    fn keyboard_actions(&self) -> Vec<PanePresentationAction> {
        if self.guards_take_over() {
            Vec::new()
        } else {
            self.operable_actions(true)
        }
    }

    /// Moves keyboard focus on plain Tab and Shift-Tab, which a live Terminal would send to its
    /// program, through a cycle of the Pane and then each of the presentation's keyboard actions.
    /// Focus never leaves the Pane. Returns whether the key was used; with no keyboard action, the
    /// Pane keeps the key and its input gate drops it unless the Terminal is live. Every other key
    /// stays with the input gate, so typing never operates an action.
    pub(crate) fn traverse_actions(
        &self,
        event: &KeyDownEvent,
        focus: &PresentationFocus,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if keystroke.key != "tab"
            || modifiers.control
            || modifiers.alt
            || modifiers.platform
            || modifiers.function
        {
            return false;
        }
        let actions = self.keyboard_actions();
        let current = if focus.pane.is_focused(window) {
            if actions.is_empty() {
                return false;
            }
            0
        } else if let Some(action) = focus.focused_action(window) {
            match actions.iter().position(|offered| *offered == action) {
                Some(index) => index + 1,
                // An action the keyboard no longer visits, such as Take Over reached some other
                // way, hands Tab back to the Pane.
                None => {
                    focus.pane.focus(window, cx);
                    return true;
                }
            }
        } else {
            return false;
        };
        let stops = actions.len() + 1;
        let next = if modifiers.shift {
            (current + stops - 1) % stops
        } else {
            (current + 1) % stops
        };
        match next.checked_sub(1) {
            None => focus.pane.focus(window, cx),
            Some(index) => focus.action(actions[index]).focus(window, cx),
        }
        true
    }

    /// The content-free screen that replaces the Terminal, if this presentation has one.
    fn screen(&self) -> Option<Screen> {
        let takeover = |device: &DisplayName| {
            (
                IconName::MonitorSmartphone,
                SharedString::from(format!("In use on {}", device.or("another device"))),
            )
        };
        let screen = match self {
            Self::Live
            | Self::AwaitingMeasurement
            | Self::Starting
            | Self::Offline {
                prior: OfflinePrior::Frozen,
            }
            | Self::Reconciling {
                prior: OfflinePrior::Frozen,
            } => return None,
            Self::Takeover(device) => {
                let (icon, title) = takeover(device);
                let detail = match device.get() {
                    Some(name) => format!(
                        "Take Over to use this Pane here. {name} will show this screen instead."
                    ),
                    None => "Take Over to use this Pane here. The other device will show this \
                             screen instead."
                        .to_owned(),
                };
                Screen::new(icon, Tone::Neutral, title, detail).action(
                    PanePresentationAction::TakeOver,
                    Prominence::Primary,
                    true,
                )
            }
            Self::Offline {
                prior: OfflinePrior::Takeover(device),
            } => {
                let (icon, title) = takeover(device);
                Screen::new(
                    icon,
                    Tone::Neutral,
                    title,
                    "The server can't be reached. Take Over is available again after it \
                     reconnects.",
                )
                .action(PanePresentationAction::Retry, Prominence::Secondary, true)
                .action(
                    PanePresentationAction::TakeOver,
                    Prominence::Primary,
                    false,
                )
            }
            Self::Reconciling {
                prior: OfflinePrior::Takeover(device),
            } => {
                let (icon, title) = takeover(device);
                Screen::new(icon, Tone::Neutral, title, "Reconnecting to the server…").action(
                    PanePresentationAction::TakeOver,
                    Prominence::Primary,
                    false,
                )
            }
            Self::Ended { reason } => Screen::new(
                IconName::SquareTerminal,
                Tone::Neutral,
                "Session ended",
                match reason {
                    SessionEndReason::EndAllSessions => {
                        "All sessions on this server were ended. Start a new session to use this \
                         Pane."
                    }
                    SessionEndReason::Restored => {
                        "The server restarted. Start a new session in this Pane's last directory."
                    }
                },
            )
            .action(
                PanePresentationAction::StartSession,
                Prominence::Primary,
                true,
            ),
            Self::Failed { class } => {
                let (title, detail) = match class {
                    PaneFailureClass::Launch => (
                        "The shell couldn't start",
                        "The server couldn't start a shell for this Pane.",
                    ),
                    PaneFailureClass::Interrupted => (
                        "Session stopped unexpectedly",
                        "The session stopped before its program exited.",
                    ),
                    PaneFailureClass::ResourceLimit => (
                        "Server limit reached",
                        "The server can't start another session. Close Panes you don't need, \
                         then try again.",
                    ),
                    PaneFailureClass::Unavailable => (
                        "Session unavailable",
                        "The server couldn't provide this Pane's session.",
                    ),
                };
                Screen::new(IconName::TriangleAlert, Tone::Error, title, detail).action(
                    PanePresentationAction::Retry,
                    Prominence::Primary,
                    true,
                )
            }
            Self::DirectoryUnavailable => Screen::new(
                IconName::FolderX,
                Tone::Warning,
                "Directory unavailable",
                "This Pane's directory was moved or deleted, or the server can't open it.",
            )
            .action(PanePresentationAction::Retry, Prominence::Secondary, true)
            .action(
                PanePresentationAction::ChooseDirectory,
                Prominence::Primary,
                true,
            ),
        };
        Some(screen)
    }
}

impl PanePresentationAction {
    const ALL: [Self; 4] = [
        Self::TakeOver,
        Self::StartSession,
        Self::Retry,
        Self::ChooseDirectory,
    ];

    const fn index(self) -> usize {
        match self {
            Self::TakeOver => 0,
            Self::StartSession => 1,
            Self::Retry => 2,
            Self::ChooseDirectory => 3,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::TakeOver => "Take Over",
            Self::StartSession => "Start Session",
            Self::Retry => "Retry",
            Self::ChooseDirectory => "Choose Directory…",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::TakeOver => "pane-presentation-take-over",
            Self::StartSession => "pane-presentation-start-session",
            Self::Retry => "pane-presentation-retry",
            Self::ChooseDirectory => "pane-presentation-choose-directory",
        }
    }
}

/// The semantic color a screen's symbol takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tone {
    Neutral,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Prominence {
    Primary,
    Secondary,
}

/// One action a screen offers, and whether it is available apart from arming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScreenAction {
    action: PanePresentationAction,
    prominence: Prominence,
    available: bool,
}

/// A content-free screen's symbol, copy, and actions, in presentation order.
#[derive(Clone, Debug)]
struct Screen {
    icon: IconName,
    tone: Tone,
    title: SharedString,
    detail: SharedString,
    actions: Vec<ScreenAction>,
}

impl Screen {
    fn new(
        icon: IconName,
        tone: Tone,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
    ) -> Self {
        Self {
            icon,
            tone,
            title: title.into(),
            detail: detail.into(),
            actions: Vec::new(),
        }
    }

    fn action(
        mut self,
        action: PanePresentationAction,
        prominence: Prominence,
        available: bool,
    ) -> Self {
        self.actions.push(ScreenAction {
            action,
            prominence,
            available,
        });
        self
    }
}

/// The colors a presentation paints on the Pane surface it rests on. Text and symbols are resolved
/// against that surface, because a program can choose the Terminal background.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PresentationPaint {
    /// The opaque surface a content-free presentation paints over the whole Pane.
    pub(crate) surface: Color,
    pub(crate) title: Color,
    pub(crate) detail: Color,
    pub(crate) neutral: Color,
    pub(crate) warning: Color,
    pub(crate) error: Color,
}

impl PresentationPaint {
    /// Resolves the presentation colors for a Pane whose Terminal background is
    /// `terminal_background`.
    pub(crate) fn resolve(appearance: &ChromeAppearance, terminal_background: Color) -> Self {
        // The Pane's surface as the window shows it, so a replacement reads as the Pane itself.
        let surface = appearance
            .pane_surface(terminal_background)
            .source_over(appearance.control_host_background(spaceterm_ui::ControlHost::Window));
        Self::on_surface(&appearance.colors, surface)
    }

    fn on_surface(colors: &ChromeColors, surface: Color) -> Self {
        let caption = colors.caption(surface, true);
        let readable = |proposed: Color, fallback: Color, minimum: f64| {
            proposed
                .readable_preserving_chroma(&[caption.background], minimum)
                .unwrap_or(fallback)
        };
        Self {
            surface: caption.background,
            title: readable(colors.text, caption.foreground, 4.5),
            detail: caption.foreground,
            neutral: readable(colors.icon, caption.icon, 3.0),
            warning: caption.attention,
            error: caption.error,
        }
    }

    fn tone(&self, tone: Tone) -> Color {
        match tone {
            Tone::Neutral => self.neutral,
            Tone::Warning => self.warning,
            Tone::Error => self.error,
        }
    }

    fn content(&self) -> ContentUnavailablePaint {
        ContentUnavailablePaint {
            title: self.title,
            detail: self.detail,
        }
    }
}

/// The focus a Pane shares with its presentation: the Pane's own focus, which Escape and every
/// action return to, and one focus for each action. The Pane keeps it for its lifetime, so an
/// action that the next presentation also offers keeps keyboard focus, and the Pane takes focus
/// back when a presentation takes the focused action away.
#[derive(Clone)]
pub(crate) struct PresentationFocus {
    pane: FocusHandle,
    actions: [FocusHandle; PanePresentationAction::ALL.len()],
}

impl PresentationFocus {
    pub(crate) fn new(pane: FocusHandle, cx: &App) -> Self {
        Self {
            pane,
            actions: std::array::from_fn(|_| cx.focus_handle()),
        }
    }

    fn action(&self, action: PanePresentationAction) -> &FocusHandle {
        &self.actions[action.index()]
    }

    /// The action whose control has keyboard focus, if one has.
    fn focused_action(&self, window: &Window) -> Option<PanePresentationAction> {
        PanePresentationAction::ALL
            .into_iter()
            .find(|action| self.action(*action).is_focused(window))
    }

    /// Gives the Pane keyboard focus back when the focused action is not among `operable`, because
    /// the presentation no longer shows it or shows it unavailable. Without this, keys would reach
    /// no Pane once the Terminal is live again.
    fn reclaim(&self, operable: &[PanePresentationAction], window: &mut Window, cx: &mut App) {
        let Some(action) = self.focused_action(window) else {
            return;
        };
        if operable.contains(&action) {
            return;
        }
        let stale = self.action(action).clone();
        let pane = self.pane.clone();
        // A focused control that renders unavailable blurs the window as it renders, so the Pane
        // takes focus once this frame is done.
        window.defer(cx, move |window, cx| {
            if stale.is_focused(window) || window.focused(cx).is_none() {
                pane.focus(window, cx);
            }
        });
    }
}

type ActionHandler = Rc<dyn Fn(PanePresentationAction, &mut Window, &mut App)>;

/// Draws a [`PanePresentation`] over the Pane's Terminal. A live presentation draws nothing. A
/// frozen screen keeps the Terminal visible under a scrim and a connection notice. Every other
/// presentation covers the whole Pane with an opaque, content-free surface.
///
/// The layer never takes focus by itself and has no default action, so Return typed for the
/// Terminal operates nothing. The Pane reaches its actions through
/// [`PanePresentation::traverse_actions`]. A press on a covering surface passes to the Pane, and
/// scrolling over one stays with it.
#[derive(IntoElement)]
pub(crate) struct PanePresentationLayer {
    id: ElementId,
    presentation: PanePresentation,
    terminal_background: Color,
    focus: Option<PresentationFocus>,
    on_action: Option<ActionHandler>,
}

impl PanePresentationLayer {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        presentation: PanePresentation,
        terminal_background: Color,
    ) -> Self {
        Self {
            id: id.into(),
            presentation,
            terminal_background,
            focus: None,
            on_action: None,
        }
    }

    /// The focus the Pane shares with the layer. Escape from an action returns to the Pane, and so
    /// does every action, so keys reach the Terminal again once it is live.
    pub(crate) fn focus(mut self, focus: PresentationFocus) -> Self {
        self.focus = Some(focus);
        self
    }

    pub(crate) fn on_action(
        mut self,
        handler: impl Fn(PanePresentationAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for PanePresentationLayer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let appearance = shared_chrome(cx);
        let paint = PresentationPaint::resolve(&appearance, self.terminal_background);
        let Self {
            id,
            presentation,
            focus,
            on_action,
            ..
        } = self;
        let child = |name: &'static str| ElementId::NamedChild(Arc::new(id.clone()), name.into());
        let armed = match &presentation {
            PanePresentation::Takeover(_) => {
                elapsed(child("takeover-arming"), TAKEOVER_ARMING_DELAY, window, cx)
            }
            _ => true,
        };
        if let Some(focus) = &focus {
            focus.reclaim(&presentation.operable_actions(armed), window, cx);
        }
        let actions = Actions { focus, on_action };
        match &presentation {
            PanePresentation::Live => gpui::Empty.into_any_element(),
            PanePresentation::AwaitingMeasurement | PanePresentation::Starting => {
                // Both states share one clock, so measuring and then starting reveal progress once.
                let revealed = elapsed(child("pending"), STARTUP_PROGRESS_DELAY, window, cx);
                render_pending(id.clone(), revealed, &paint, &appearance)
            }
            PanePresentation::Offline {
                prior: OfflinePrior::Frozen,
            }
            | PanePresentation::Reconciling {
                prior: OfflinePrior::Frozen,
            } => render_frozen(
                id.clone(),
                matches!(presentation, PanePresentation::Offline { .. }),
                &paint,
                &appearance,
                actions,
                cx,
            ),
            _ => {
                let Some(screen) = presentation.screen() else {
                    return gpui::Empty.into_any_element();
                };
                render_screen(id.clone(), screen, armed, &paint, &appearance, actions)
            }
        }
    }
}

/// Where a presentation's actions report, and the focus they share with the Pane.
#[derive(Clone)]
struct Actions {
    focus: Option<PresentationFocus>,
    on_action: Option<ActionHandler>,
}

impl Actions {
    fn button(
        &self,
        action: PanePresentationAction,
        variant: ButtonVariant,
        size: ButtonSize,
        enabled: bool,
    ) -> Button {
        let actions = self.clone();
        let button = Button::new(action.selector(), action.label());
        let button = match &self.focus {
            Some(focus) => button.focus_handle(focus.action(action).clone()),
            None => button,
        };
        button
            .variant(variant)
            .size(size)
            .disabled(!enabled)
            // Take Over is never a tab stop, so no Tab, typed in the Pane or anywhere else in the
            // window, can put it under a following Return or Space.
            .tab_stop(action != PanePresentationAction::TakeOver)
            .debug_selector(action.selector())
            .on_activate(move |_, window, cx| {
                if let Some(focus) = &actions.focus {
                    focus.pane.focus(window, cx);
                }
                if let Some(on_action) = &actions.on_action {
                    on_action(action, window, cx);
                }
            })
    }

    /// Returns focus to the Pane when Escape reaches the layer from one of its actions.
    fn escape_returns_focus<E: InteractiveElement>(&self, element: E) -> E {
        let Some(focus) = self.focus.as_ref().map(|focus| focus.pane.clone()) else {
            return element;
        };
        element.on_key_down(move |event: &KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" && !focus.is_focused(window) {
                focus.focus(window, cx);
                cx.stop_propagation();
            }
        })
    }
}

/// Startup shows nothing until `revealed`, then a quiet spinner, so a quick start never flashes.
fn render_pending(
    id: ElementId,
    revealed: bool,
    paint: &PresentationPaint,
    appearance: &ChromeAppearance,
) -> AnyElement {
    let child = |name: &'static str| ElementId::NamedChild(Arc::new(id.clone()), name.into());
    let progress = revealed.then(|| {
        div()
            .id(child("progress"))
            .role(accesskit::Role::Status)
            .aria_label("Session status")
            .aria_value("Starting")
            .debug_selector(|| "pane-presentation-progress".to_owned())
            .flex()
            .items_center()
            .gap(appearance.spacing(8.0))
            .text_color(gpui_color(paint.detail))
            .chrome_text(appearance.typography.style(TextRole::Secondary))
            .child(
                FrameSpinner::new(child("spinner"), "Starting session").size(ProgressSize::Compact),
            )
            .child("Starting session…")
    });
    div()
        .id(id)
        .debug_selector(|| "pane-presentation-pending".to_owned())
        .absolute()
        .inset_0()
        .bg(gpui_color(paint.surface))
        .cursor_default()
        .on_scroll_wheel(keep_scroll)
        .flex()
        .items_center()
        .justify_center()
        .children(progress)
        .into_any_element()
}

/// A frozen screen keeps the last Terminal content readable under a light scrim, with a docked,
/// non-modal connection notice. The scrim takes no pointer input, so the frozen text stays
/// selectable.
fn render_frozen(
    id: ElementId,
    offline: bool,
    paint: &PresentationPaint,
    appearance: &ChromeAppearance,
    actions: Actions,
    cx: &App,
) -> AnyElement {
    let child = |name: &'static str| ElementId::NamedChild(Arc::new(id.clone()), name.into());
    let shell = spaceterm_ui::floating_surface_theme(cx).shell(FloatingRole::Notice);
    let floating_colors = &appearance.floating_colors;
    let message = if offline {
        "Server offline. Reconnecting…"
    } else {
        "Reconnecting…"
    };
    let glyph_size = appearance.icons.metrics(IconRole::Status).glyph_size;
    let glyph = if offline {
        div()
            .flex_none()
            .text_color(gpui_color(floating_colors.icon))
            .child(Icon::inherited(IconName::ServerOff, glyph_size))
            .into_any_element()
    } else {
        FrameSpinner::new(child("spinner"), "Reconnecting")
            .size(ProgressSize::Compact)
            .into_any_element()
    };
    let notice = shell.mount(
        actions.escape_returns_focus(
            div()
                .id(child("connection"))
                .role(accesskit::Role::Status)
                .aria_label("Connection status")
                .aria_value(if offline { "Offline" } else { "Reconnecting" })
                .debug_selector(move || {
                    format!(
                        "pane-presentation-{}",
                        if offline { "offline" } else { "reconciling" }
                    )
                })
                .absolute()
                .right(appearance.spacing(12.0))
                .bottom(appearance.spacing(12.0))
                .max_w(relative(0.94))
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(8.0))
                .px(appearance.spacing(10.0))
                .py(appearance.spacing(6.0))
                .text_color(gpui_color(floating_colors.text))
                .chrome_text(appearance.typography.style(TextRole::Body))
                .block_mouse_except_scroll()
                .child(glyph)
                .child(
                    div()
                        .debug_selector(|| "pane-presentation-connection-message".to_owned())
                        .min_w_0()
                        .child(message),
                )
                .when(offline, |notice| {
                    notice.child(actions.button(
                        PanePresentationAction::Retry,
                        ButtonVariant::Link,
                        ButtonSize::Compact,
                        true,
                    ))
                }),
        ),
    );
    div()
        .id(id)
        .debug_selector(|| "pane-presentation-frozen".to_owned())
        .absolute()
        .inset_0()
        .child(
            div()
                .debug_selector(|| "pane-presentation-scrim".to_owned())
                .absolute()
                .inset_0()
                .bg(gpui_color(paint.surface.with_alpha(FROZEN_SCRIM_ALPHA))),
        )
        .child(notice)
        .into_any_element()
}

/// A content-free screen covering the Pane. Its actions stay off until `armed`.
fn render_screen(
    id: ElementId,
    screen: Screen,
    armed: bool,
    paint: &PresentationPaint,
    appearance: &ChromeAppearance,
    actions: Actions,
) -> AnyElement {
    let buttons = screen
        .actions
        .iter()
        .map(|action| {
            actions
                .button(
                    action.action,
                    match action.prominence {
                        Prominence::Primary => ButtonVariant::Primary,
                        Prominence::Secondary => ButtonVariant::Secondary,
                    },
                    ButtonSize::Regular,
                    action.available && armed,
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();
    let content = ContentUnavailable::new(
        ElementId::NamedChild(Arc::new(id.clone()), "content".into()),
        "pane-presentation-screen",
        screen.icon,
        screen.title,
        screen.detail,
    )
    .symbol(paint.tone(screen.tone))
    .paint(paint.content())
    .actions(buttons);
    actions
        .escape_returns_focus(
            div()
                .id(id)
                .debug_selector(|| "pane-presentation-replacement".to_owned())
                .absolute()
                .inset_0()
                .bg(gpui_color(paint.surface))
                .cursor_default()
                // A Pane too short for the screen scrolls it from its top rather than clipping
                // its actions, and a taller one centers it.
                .overflow_y_scroll()
                .on_scroll_wheel(keep_scroll)
                .flex()
                .flex_col()
                .p(appearance.spacing(16.0)),
        )
        .child(div().m_auto().child(content))
        .into_any_element()
}

/// Keeps a scroll over a covering surface from reaching the Pane, whose Terminal it hides. The
/// surface's own scrolling has already happened by the time this runs.
fn keep_scroll(_: &ScrollWheelEvent, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

/// Whether `delay` has passed since the element keyed by `key` first rendered. The clock restarts
/// when the element leaves the screen and returns.
fn elapsed(key: ElementId, delay: Duration, window: &mut Window, cx: &mut App) -> bool {
    window
        .use_keyed_state(key, cx, move |_, cx| Elapsed::start(delay, cx))
        .read(cx)
        .elapsed
}

struct Elapsed {
    elapsed: bool,
    _timer: Task<()>,
}

impl Elapsed {
    fn start(delay: Duration, cx: &mut gpui::Context<Self>) -> Self {
        Self {
            elapsed: false,
            _timer: cx.spawn(async move |elapsed, cx| {
                cx.background_executor().timer(delay).await;
                let _ = elapsed.update(cx, |elapsed: &mut Elapsed, cx| {
                    elapsed.elapsed = true;
                    cx.notify();
                });
            }),
        }
    }
}
