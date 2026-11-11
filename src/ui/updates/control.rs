//! The title-bar split pill. It shows while an update step is owed and hides otherwise. Whenever
//! it shows it has two segments: the main area, which performs the step or opens the list while
//! work the person started is in progress, and the chevron, which always opens the same list of
//! SpaceTerm and every Machine. While the list is open the pill keeps its shape: the rows keep
//! their number and order and only their text changes, and the pill stays until the list closes.
//! The pill is one split capsule in every state, progress included, so starting or finishing work
//! never changes its shape. [`crate::updates::state`] decides every label, row and command.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Pixels, Rgba, SharedString, WeakEntity, Window, div};
use spaceterm_ui::{
    ButtonPaint, ButtonShape, ButtonSize, ButtonVariant, ButtonVariantStyle, ComboButton,
    DeterminateProgress, Icon, IconName, MenuAlignment, MenuEntry, MenuLifecycleEvent,
    MenuPlacement, MenuPlacementConfig, MenuSize, ProgressRing, ProgressSize, ProgressState,
    Tooltip,
};

use super::commands::{self, Presenter};
use super::service;
use crate::ui::appearance::{chrome, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::workspace_frame::WorkspaceFrame;
use crate::updates::copy::rows::{CORE_HEADING, UPDATE_SETTINGS};
use crate::updates::ladder::Emphasis;
use crate::updates::machines::MachineId;
use crate::updates::machines::store::MachineUpdates;
use crate::updates::state::{self, Command, Glyph, PillView, RowView, UpdateView};

/// What the control reads each frame. The title bar reads the shared update state; a test
/// scripts it.
pub(crate) type ViewSource = Rc<dyn Fn(&App) -> Shown>;

/// What the pill and its list show at one moment.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    pub(crate) pill: Option<PillView>,
    pub(crate) app: RowView,
    /// Each Machine's row, local first, then store order.
    pub(crate) machines: Vec<(MachineId, RowView)>,
}

impl Shown {
    pub(crate) fn of(view: &UpdateView) -> Self {
        Self {
            pill: view.pill(),
            app: view.app_row(),
            // `machine_rows()[i]` describes `machines[i]`.
            machines: view
                .machines
                .iter()
                .map(|machine| machine.id)
                .zip(view.machine_rows())
                .collect(),
        }
    }

    /// What an open list shows when `next` arrives: the same Machines in the same order, each
    /// with its own row from `next`. A Machine that left keeps its last row, and one that arrived
    /// waits for the list to close. A pill whose step resolved stays until the list closes.
    fn keep_shape(&self, next: Self) -> Self {
        let machines = self
            .machines
            .iter()
            .map(|(id, open)| {
                let row = next
                    .machines
                    .iter()
                    .find(|(next, _)| next == id)
                    .map_or_else(|| open.clone(), |(_, row)| row.clone());
                (*id, row)
            })
            .collect();
        Self {
            pill: next.pill.or_else(|| self.pill.clone()),
            app: next.app,
            machines,
        }
    }
}

/// Progress on work the person started: the main area then opens the list.
fn in_progress(pill: &PillView) -> bool {
    pill.main == Command::ShowList
}

/// The split pill at the trailing end of one window's title bar.
pub(crate) struct UpdateControl {
    source: ViewSource,
    /// What the open list showed last. None while the list is closed.
    open: Option<Shown>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl UpdateControl {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_source(Rc::new(|cx| Shown::of(&state::current(cx))), window, cx)
    }

    pub(crate) fn with_source(
        source: ViewSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![cx.observe_window_activation(window, |_, window, cx| {
            // A due desktop reminder waits for an active window.
            if window.is_window_active()
                && let Some(updates) = service(cx)
            {
                updates.read(cx).window_activated();
            }
        })];
        subscriptions
            .extend(service(cx).map(|updates| cx.observe(&updates, |_, _, cx| cx.notify())));
        subscriptions.extend(
            MachineUpdates::global(cx)
                .map(|machines| cx.observe(&machines, |_, _, cx| cx.notify())),
        );
        Self {
            source,
            open: None,
            _subscriptions: subscriptions,
        }
    }

    /// What shows now: the current state, or while the list is open, the open list's shape.
    fn shown(&self, cx: &App) -> Shown {
        let next = (self.source)(cx);
        match &self.open {
            Some(open) => open.keep_shape(next),
            None => next,
        }
    }

    /// Whether the pill shows. The title bar that holds it is then occupied.
    pub(crate) fn shows_pill(&self, cx: &App) -> bool {
        self.shown(cx).pill.is_some()
    }

    fn list_changed(&mut self, event: MenuLifecycleEvent, cx: &mut Context<Self>) {
        self.open = match event {
            MenuLifecycleEvent::Opened => Some(self.shown(cx)),
            MenuLifecycleEvent::Closed(_) => None,
        };
        cx.notify();
    }
}

/// What choosing an entry of the list does.
#[derive(Clone)]
enum Choice {
    Perform(Command),
    UpdateSettings,
    /// A row that reports and cannot be chosen.
    Status,
}

/// The glyph leading the main area or a row. Icons take `foreground`. A ring on the accent fill
/// takes the foreground too, because the installed accent disappears against it.
fn glyph(
    glyph: Glyph,
    fraction: Option<f32>,
    size: Pixels,
    inherited: bool,
    foreground: Rgba,
) -> AnyElement {
    let icon = |name| Icon::new(name, size, foreground).into_any_element();
    match glyph {
        Glyph::Working => {
            let state = fraction
                .and_then(|fraction| DeterminateProgress::new(f64::from(fraction)).ok())
                .map_or(ProgressState::Indeterminate, ProgressState::Determinate);
            ProgressRing::new("update-pill-progress", "Update", state)
                .size(ProgressSize::Compact)
                .when(inherited, ProgressRing::inherited)
                .into_any_element()
        }
        Glyph::Download => icon(IconName::Download),
        Glyph::Restart => icon(IconName::RotateCw),
        Glyph::UpdateCore => icon(IconName::Package),
        Glyph::WhatsNew => icon(IconName::Sparkles),
        Glyph::UpdateFailed => icon(IconName::CircleAlert),
        Glyph::CantConfirm => icon(IconName::CircleHelp),
    }
}

/// The warning tint paints over the neutral fill, from the installed warning pair.
fn warning_style(cx: &App) -> (ButtonVariantStyle, Rgba) {
    let pair = chrome(cx).semantic_text_pairs.warning_status;
    let paint = |amount| {
        ButtonPaint::new(
            gpui_color(pair.background.mix(pair.primary, amount)),
            gpui_color(pair.primary),
            gpui::rgba(0),
        )
    };
    (
        ButtonVariantStyle::new(paint(0.0), paint(0.12), paint(0.2), paint(0.0)),
        gpui_color(pair.primary),
    )
}

fn variant(emphasis: Emphasis) -> ButtonVariant {
    match emphasis {
        Emphasis::Accent => ButtonVariant::Primary,
        Emphasis::Plain | Emphasis::Warning => ButtonVariant::Secondary,
    }
}

/// A row's trailing text: `{status}` or `{status} · {action}`.
fn detail(row: &RowView) -> Option<SharedString> {
    let status = row.status.clone().or_else(|| row.version.clone());
    match (status, &row.action) {
        (Some(status), Some(action)) => Some(format!("{status} · {}", action.label).into()),
        (Some(status), None) => Some(status),
        (None, Some(action)) => Some(action.label.clone()),
        (None, None) => None,
    }
}

fn row_entry(row: &RowView, selector: String) -> MenuEntry<Choice> {
    let choice = match &row.action {
        Some(action) => Choice::Perform(action.command.clone()),
        None => Choice::Status,
    };
    let informational = matches!(choice, Choice::Status);
    let mut entry = MenuEntry::action(row.name.clone(), choice)
        .description(row.accessibility.clone())
        .debug_selector(selector);
    if let Some(detail) = detail(row) {
        entry = entry.detail(detail);
    }
    if informational {
        entry = entry.informational();
    }
    entry
}

/// The list: SpaceTerm's row, the heading and one row per Machine, then Update Settings.
fn entries(shown: &Shown) -> Vec<MenuEntry<Choice>> {
    let mut entries = vec![row_entry(&shown.app, "update-row-app".to_owned())];
    if !shown.machines.is_empty() {
        entries.push(MenuEntry::section(
            CORE_HEADING,
            shown
                .machines
                .iter()
                .enumerate()
                .map(|(index, (_, row))| row_entry(row, format!("update-row-{index}")))
                .collect(),
        ));
    }
    entries.push(MenuEntry::separator());
    entries.push(
        MenuEntry::action(UPDATE_SETTINGS, Choice::UpdateSettings)
            .debug_selector("update-settings"),
    );
    entries
}

fn choose(choice: &Choice, presenter: &Presenter, cx: &mut App) {
    match choice {
        Choice::Perform(command) => commands::perform(command.clone(), presenter.clone(), cx),
        Choice::UpdateSettings => {
            crate::ui::settings_window::open_or_activate(
                Some(crate::ui::settings_window::SettingsSectionId::Updates),
                cx,
            );
        }
        Choice::Status => {}
    }
}

/// Reports the list's openings and closings to the control, after the current update.
fn lifecycle(control: WeakEntity<UpdateControl>) -> impl Fn(&MenuLifecycleEvent, &mut App) {
    move |event, cx| {
        let (control, event) = (control.clone(), *event);
        cx.defer(move |cx| {
            let _ = control.update(cx, |control, cx| control.list_changed(event, cx));
        });
    }
}

fn placement() -> MenuPlacementConfig {
    MenuPlacementConfig::new(MenuPlacement::Bottom, MenuAlignment::End)
}

/// The capsule: main area and chevron. While work is in progress the main area opens the list
/// too, and the capsule keeps both segments.
fn pill(
    pill: PillView,
    shown: &Shown,
    size: Pixels,
    presenter: Presenter,
    control: WeakEntity<UpdateControl>,
    cx: &App,
) -> AnyElement {
    let inherited = pill.emphasis == Emphasis::Accent;
    let (mark, fraction) = (pill.glyph, pill.progress);
    let warning = (pill.emphasis == Emphasis::Warning).then(|| warning_style(cx));
    let progress = in_progress(&pill);
    let chosen = presenter.clone();
    let main = pill.main;
    let button = ComboButton::new("update-pill", pill.label, entries(shown))
        .variant(variant(pill.emphasis))
        .size(ButtonSize::Small)
        .shape(ButtonShape::Capsule)
        .leading(move |foreground| glyph(mark, fraction, size, inherited, foreground))
        .accessibility_description(pill.tooltip.clone())
        .tooltip(
            Tooltip::new("update-pill-tooltip", pill.tooltip).debug_selector("update-pill-tooltip"),
        )
        .menu_label("Updates")
        .menu_size(MenuSize::Wide)
        .placement(placement())
        .debug_selector("update-pill")
        .on_menu_lifecycle(lifecycle(control))
        .on_activate(move |_, _, cx| commands::perform(main.clone(), presenter.clone(), cx))
        .primary_opens_menu(progress)
        .on_menu_activate(move |activation, _, cx| choose(activation.action(), &chosen, cx));
    match warning {
        Some((style, focus)) => button.contextual_style(style, focus),
        None => button,
    }
    .into_any_element()
}

impl Render for UpdateControl {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.shown(cx);
        if self.open.is_some() {
            self.open = Some(shown.clone());
        }
        let Some(view) = shown.pill.clone() else {
            return div().into_any_element();
        };
        let appearance = chrome(cx);
        let size = appearance.icons.metrics(IconRole::Caption).glyph_size;
        let presenter = Presenter::Control(window.window_handle(), cx.weak_entity());
        let element = pill(view, &shown, size, presenter, cx.weak_entity(), cx);
        // The control owns its clicks inside the title bar's drag region. Its trailing edge stops
        // where the Pane beneath it stops, and it keeps one frame gap from the Tabs.
        let frame = WorkspaceFrame::for_appearance(appearance, cx);
        div()
            .debug_selector(|| "update-control-area".to_owned())
            .flex_none()
            .pl(frame.space())
            .pr(frame.window_edge_inset())
            .child(
                div()
                    .block_mouse_except_scroll()
                    .flex()
                    .items_center()
                    .child(element),
            )
            .into_any_element()
    }
}
