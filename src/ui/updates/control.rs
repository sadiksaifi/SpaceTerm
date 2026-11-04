//! The title-bar update control. It appears only while something needs doing or reading and shows
//! one glyph: blue for action and progress, orange for problems and overdue states. One pending
//! item is a plain button that performs its action, or a status while it works. Two or more open
//! a compact menu with one line per Machine, "Restart All…" and Recent.

use gpui::prelude::*;
use gpui::{AnyElement, AnyWindowHandle, App, Context, Pixels, Rgba, SharedString, Window, div};
use spaceterm_ui::{
    Button, ButtonPaint, ButtonShape, ButtonSize, ButtonVariant, ButtonVariantStyle,
    DeterminateProgress, Icon, IconName, Menu, MenuAlignment, MenuEntry, MenuPlacement,
    MenuPlacementConfig, MenuSize, ProgressRing, ProgressSize, ProgressState, Tooltip,
};

use super::commands;
use super::service;
use crate::ui::appearance::{chrome, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::workspace_frame::WorkspaceFrame;
use crate::updates::machines::store::MachineUpdates;
use crate::updates::presentation::{
    self, Command, Control, Glyph, Item, MenuRow, Presentation, Tone,
};

/// The control at the trailing end of one window's title bar.
pub(crate) struct UpdateControl {
    _subscriptions: Vec<gpui::Subscription>,
}

impl UpdateControl {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            _subscriptions: subscriptions,
        }
    }

    /// Whether the control shows. The title bar that holds it is then occupied.
    pub(crate) fn shows_pill(&self, cx: &App) -> bool {
        presentation::current(cx).control() != Control::Hidden
    }
}

/// What a menu row does when chosen.
#[derive(Clone)]
enum Choice {
    Perform(Command),
    /// A row that reports and cannot be chosen.
    Status,
}

/// The glyph leading the control or a menu row. Icons take `foreground`. A ring on the blue fill
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
            ProgressRing::new("update-control-progress", "Update", state)
                .size(ProgressSize::Compact)
                .when(inherited, ProgressRing::inherited)
                .into_any_element()
        }
        Glyph::Download => icon(IconName::Download),
        Glyph::Restart => icon(IconName::RotateCw),
        Glyph::Overdue => icon(IconName::Clock),
        Glyph::UpdateDesktop => icon(IconName::CircleArrowUp),
        Glyph::UpdateCore => icon(IconName::Package),
        Glyph::WhatsNew => icon(IconName::Sparkles),
        Glyph::UpdateFailed => icon(IconName::CircleAlert),
        Glyph::InstallFailed => icon(IconName::CircleX),
        Glyph::CantConfirm => icon(IconName::CircleHelp),
        Glyph::CantCheck => icon(IconName::CloudOff),
    }
}

/// Orange paints over the neutral fill, from the installed warning pair.
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

fn warning_color(cx: &App) -> Rgba {
    gpui_color(chrome(cx).semantic_text_pairs.warning_status.primary)
}

fn variant(tone: Tone) -> ButtonVariant {
    match tone {
        Tone::Action => ButtonVariant::Primary,
        Tone::Problem => ButtonVariant::Secondary,
    }
}

/// One item: a plain button that performs its command, or a status while the item works.
fn single(item: Item, size: Pixels, window: AnyWindowHandle, cx: &App) -> AnyElement {
    let inherited = item.tone == Tone::Action;
    let (mark, fraction) = (item.glyph, item.fraction);
    let button = Button::new("update-control", item.label)
        .variant(variant(item.tone))
        .size(ButtonSize::Small)
        .shape(ButtonShape::Capsule)
        .leading(move |foreground| glyph(mark, fraction, size, inherited, foreground))
        .accessibility_description(item.sentence.clone())
        .tooltip(
            Tooltip::new("update-control-tooltip", item.sentence)
                .debug_selector("update-control-tooltip"),
        )
        .debug_selector("update-control")
        .when(item.tone == Tone::Problem, |button| {
            let (style, focus) = warning_style(cx);
            button.contextual_style(style, focus)
        });
    match item.command {
        Some(command) => button
            .on_activate(move |_, _, cx| commands::perform(command.clone(), Some(window), cx))
            .into_any_element(),
        None => button.into_any_element(),
    }
}

/// The trailing text of a menu row: its status, then the action choosing it performs.
fn trailing(row: &MenuRow) -> SharedString {
    match &row.button {
        Some(button) => format!("{} · {}", row.status, button.label).into(),
        None => row.status.clone(),
    }
}

fn row_entry(row: MenuRow, index: usize, warning: Rgba) -> MenuEntry<Choice> {
    let choice = match &row.button {
        Some(button) => Choice::Perform(button.command.clone()),
        None => Choice::Status,
    };
    let disabled = matches!(choice, Choice::Status);
    let (mark, fraction, tone) = (row.glyph, row.fraction, row.tone);
    let mut entry = MenuEntry::action(row.name.clone(), choice)
        .shortcut(trailing(&row))
        .disabled(disabled)
        .debug_selector(format!("update-row-{index}"));
    if let Some(mark) = mark {
        entry = entry.icon(move |foreground, size| {
            let tint = if tone == Some(Tone::Problem) {
                warning
            } else {
                foreground
            };
            glyph(mark, fraction, size, false, tint)
        });
    }
    entry
}

/// The compact menu: the desktop's row, the "SpaceTerm Core" rows, then Restart All and Recent.
fn menu_entries(presentation: &Presentation, warning: Rgba, cx: &App) -> Vec<MenuEntry<Choice>> {
    let mut entries = Vec::new();
    let desktop = presentation.desktop_row();
    let machines = presentation.machine_rows();
    let has_desktop = desktop.is_some();
    entries.extend(desktop.map(|row| row_entry(row, 0, warning)));
    if !machines.is_empty() {
        if has_desktop {
            entries.push(MenuEntry::separator());
        }
        entries.push(MenuEntry::section(
            "SpaceTerm Core",
            machines
                .into_iter()
                .enumerate()
                .map(|(index, row)| row_entry(row, index + 1, warning))
                .collect(),
        ));
    }
    let restart_all = presentation.restart_all();
    let recent: Vec<MenuEntry<Choice>> = crate::updates::recent_lines(cx)
        .into_iter()
        .map(|line| MenuEntry::action(line, Choice::Status).disabled(true))
        .collect();
    if restart_all.is_some() || !recent.is_empty() {
        entries.push(MenuEntry::separator());
    }
    if let Some(all) = restart_all {
        entries.push(
            MenuEntry::action(all.label, Choice::Perform(all.command))
                .shortcut(all.trailing)
                .debug_selector("update-restart-all"),
        );
    }
    if !recent.is_empty() {
        entries.push(MenuEntry::submenu("Recent", recent).debug_selector("update-recent"));
    }
    entries
}

impl Render for UpdateControl {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let presentation = presentation::current(cx);
        let control = presentation.control();
        if control == Control::Hidden {
            return div().into_any_element();
        }
        let appearance = chrome(cx);
        let size = appearance.icons.metrics(IconRole::Caption).glyph_size;
        let handle = window.window_handle();
        let warning = warning_color(cx);
        let element = match control {
            Control::Hidden => unreachable!("handled above"),
            Control::Single(item) => single(item, size, handle, cx),
            Control::Menu {
                label,
                tone,
                glyph: mark,
                sentence: _,
            } => {
                let inherited = tone == Tone::Action;
                Menu::new(
                    "update-control-menu",
                    label,
                    menu_entries(&presentation, warning, cx),
                )
                .button_trigger(variant(tone), ButtonSize::Small, ButtonShape::Capsule)
                .leading_icon(move |foreground| glyph(mark, None, size, inherited, foreground))
                .when(tone == Tone::Problem, |menu| {
                    let (style, focus) = warning_style(cx);
                    menu.contextual_style(style, focus)
                })
                .size(MenuSize::Wide)
                .placement(MenuPlacementConfig::new(
                    MenuPlacement::Bottom,
                    MenuAlignment::End,
                ))
                .debug_selector("update-control-menu")
                .on_activate(move |activation, _, cx| {
                    if let Choice::Perform(command) = activation.action() {
                        commands::perform(command.clone(), Some(handle), cx);
                    }
                })
                .into_any_element()
            }
        };
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
