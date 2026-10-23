//! The title bar update pill and the Updates menu behind its chevron: the client's update above one
//! section per actual SpaceTerm Server. Each row reaches an existing journey. The client rows drive
//! the client update prompts, and a server row hands its exact server to that server's Settings
//! update journey.

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Context, Entity, Pixels, Rgba, SharedString, Subscription,
    WeakEntity, Window, div,
};
use spaceterm_ui::{
    ButtonPaint, ButtonShape, ButtonSize, ButtonVariant, ButtonVariantStyle, ComboButton,
    DeterminateProgress, Icon, IconName, Menu, MenuAlignment, MenuEntry, MenuLifecycleEvent,
    MenuPlacement, MenuPlacementConfig, MenuSize, ProgressRing, ProgressSize, ProgressState,
    Tooltip,
};

use super::server_rows::{ServerUpdateAction, ServerUpdateRow, ServerUpdates};
use super::{
    CHECK_FOR_UPDATES_TITLE, CURRENT_VERSION, ControlCommand, ControlGlyph, ControlPresentation,
    InstallRequest, PillCommand, PillTone, UpdatePill, check_for_updates, failure_message,
    present_stop_download, request_install, retry_install, service,
};
use crate::desktop_profile::{DesktopPresentation, DesktopWording};
use crate::ui::WorkspaceManager;
use crate::ui::appearance::{chrome, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::workspace_frame::WorkspaceFrame;
use crate::updates::{UpdateError, UpdateState};

/// What choosing a row does.
#[derive(Clone)]
enum UpdatesMenuAction {
    /// A status row, which reports and cannot be chosen.
    Status,
    Client(ControlCommand),
    CheckForUpdates,
    Server(ServerUpdateAction),
}

/// The update pill at the trailing end of one window's title bar. It appears while the client or a
/// server asks for action, and the Updates menu behind its chevron keeps every row.
pub(crate) struct UpdateControl {
    servers: Entity<ServerUpdates>,
    _subscriptions: Vec<Subscription>,
}

impl UpdateControl {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let servers = cx.new(ServerUpdates::new);
        let mut subscriptions = vec![
            cx.observe(&servers, |_, _, cx| cx.notify()),
            // The pill shows server needs while the menu is closed. A Settings journey changes
            // them elsewhere, so returning to the window reads them again.
            cx.observe_window_activation(window, |control, window, cx| {
                if window.is_window_active() {
                    control
                        .servers
                        .update(cx, |servers, cx| servers.refresh(cx));
                }
            }),
        ];
        subscriptions
            .extend(service(cx).map(|updates| cx.observe(&updates, |_, _, cx| cx.notify())));
        Self {
            servers,
            _subscriptions: subscriptions,
        }
    }

    /// Runs after the menu closes, because prompts present into the window the menu belongs to.
    fn activate(action: UpdatesMenuAction, window: AnyWindowHandle, cx: &mut App) {
        cx.defer(move |cx| match action {
            UpdatesMenuAction::Status => {}
            UpdatesMenuAction::CheckForUpdates => check_for_updates(cx),
            UpdatesMenuAction::Client(command) => activate_client(command, window, cx),
            UpdatesMenuAction::Server(action) => action.activate(cx),
        });
    }
}

/// Runs after the press, because prompts present into the window the pill belongs to.
fn activate_pill(command: PillCommand, window: AnyWindowHandle, cx: &mut App) {
    cx.defer(move |cx| match command {
        PillCommand::Client(Some(command)) => activate_client(command, window, cx),
        PillCommand::Server(action) => action.activate(cx),
        PillCommand::Client(None) | PillCommand::Menu => {}
    });
}

fn activate_client(command: ControlCommand, window: AnyWindowHandle, cx: &mut App) {
    let Some(updates) = service(cx) else { return };
    match command {
        ControlCommand::Download => updates.update(cx, |updates, cx| updates.download(cx)),
        ControlCommand::RetryDownload => {
            updates.update(cx, |updates, cx| updates.retry_download(cx))
        }
        ControlCommand::OfferStop => {
            let (state, cancelling) = {
                let updates = updates.read(cx);
                (updates.state().clone(), updates.is_cancelling())
            };
            let (UpdateState::Downloading { version, .. } | UpdateState::Verifying { version }) =
                state
            else {
                return;
            };
            if cancelling {
                return;
            }
            let Some(handle) = window.downcast::<WorkspaceManager>() else {
                return;
            };
            let _ = handle.update(cx, |_, window, cx| {
                present_stop_download(&version, window, cx)
            });
        }
        ControlCommand::RequestInstall => {
            request_install(&updates, InstallRequest::FrontWorkspace, cx)
        }
        ControlCommand::RetryInstall => retry_install(cx),
    }
}

fn status_row(status: impl Into<SharedString>) -> MenuEntry<UpdatesMenuAction> {
    MenuEntry::action(status, UpdatesMenuAction::Status).disabled(true)
}

/// The client's section. A pending update shows its status and next step. Otherwise the section
/// reports the last result and offers a check.
fn client_section(
    presentation: Option<&ControlPresentation>,
    state: &UpdateState,
    wording: DesktopWording,
) -> MenuEntry<UpdatesMenuAction> {
    let mut entries = Vec::with_capacity(2);
    if let Some(presentation) = presentation {
        if let Some(status) = presentation.status.clone() {
            entries.push(status_row(status).debug_selector("update-client-status"));
        }
        let command = match presentation.command {
            Some(command) => MenuEntry::action(
                presentation.label.clone(),
                UpdatesMenuAction::Client(command),
            ),
            None => status_row(presentation.label.clone()),
        };
        entries.push(command.debug_selector("update-client-command"));
    } else {
        let status = match state {
            UpdateState::UpToDate => Some("SpaceTerm Is Up to Date".to_owned()),
            UpdateState::Failed { error } => Some(failure_message(*error, wording)),
            UpdateState::Unavailable => Some(failure_message(UpdateError::Unavailable, wording)),
            _ => None,
        };
        if let Some(status) = status {
            entries.push(status_row(status).debug_selector("update-client-status"));
        }
        let check = match state {
            UpdateState::Unavailable => None,
            UpdateState::Checking => Some(status_row("Checking for Updates…")),
            _ => Some(MenuEntry::action(
                CHECK_FOR_UPDATES_TITLE,
                UpdatesMenuAction::CheckForUpdates,
            )),
        };
        entries.extend(check.map(|check| check.debug_selector("update-client-command")));
    }
    MenuEntry::section(format!("SpaceTerm {CURRENT_VERSION}"), entries)
}

/// One section per actual server, named by its machine and account. The status reads as a
/// disabled row, and the action that reaches the server's Settings journey follows it under its
/// own name.
fn server_sections(rows: Vec<ServerUpdateRow>) -> Vec<MenuEntry<UpdatesMenuAction>> {
    if rows.is_empty() {
        return vec![MenuEntry::section(
            "Servers",
            vec![status_row("No Servers").debug_selector("update-server-none")],
        )];
    }
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            let mut entries = vec![
                status_row(row.detail).debug_selector(format!("update-server-{index}-status")),
            ];
            entries.extend(row.action.map(|action| {
                MenuEntry::action(action.label(), UpdatesMenuAction::Server(action))
                    .debug_selector(format!("update-server-{index}-action"))
            }));
            MenuEntry::section(row.label, entries)
        })
        .collect()
}

/// The pill's leading glyph. Icons take the pill's foreground; the progress ring keeps the
/// installed accent because the disabled foreground of Checking and Stopping is too faint for it.
fn pill_glyph(glyph: ControlGlyph, glyph_size: Pixels) -> impl FnOnce(Rgba) -> AnyElement {
    move |foreground| match glyph {
        ControlGlyph::Download => {
            Icon::new(IconName::Download, glyph_size, foreground).into_any_element()
        }
        ControlGlyph::Restart => {
            Icon::new(IconName::RotateCw, glyph_size, foreground).into_any_element()
        }
        ControlGlyph::Warning => {
            Icon::new(IconName::CircleAlert, glyph_size, foreground).into_any_element()
        }
        ControlGlyph::Progress(fraction) => {
            let state = DeterminateProgress::new(f64::from(fraction))
                .map_or(ProgressState::Indeterminate, ProgressState::Determinate);
            ProgressRing::new("update-control-progress", "Update", state)
                .size(ProgressSize::Compact)
                .into_any_element()
        }
        ControlGlyph::Activity => ProgressRing::new(
            "update-control-progress",
            "Update",
            ProgressState::Indeterminate,
        )
        .size(ProgressSize::Compact)
        .into_any_element(),
    }
}

/// Reads the server inventory again whenever the Updates menu opens, so a local service change
/// is observed without polling.
fn refresh_on_open(
    servers: WeakEntity<ServerUpdates>,
) -> impl Fn(&MenuLifecycleEvent, &mut App) + 'static {
    move |event, cx| {
        if *event != MenuLifecycleEvent::Opened {
            return;
        }
        let servers = servers.clone();
        // Lifecycle delivery can occur while the window is borrowed.
        cx.defer(move |cx| {
            let _ = servers.update(cx, |servers, cx| servers.refresh(cx));
        });
    }
}

impl Render for UpdateControl {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wording = DesktopPresentation::get(cx).wording();
        let (state, presentation) = match service(cx) {
            Some(updates) => {
                let updates = updates.read(cx);
                let presentation = ControlPresentation::resolve(
                    updates.state(),
                    updates.pending_version(),
                    updates.is_cancelling(),
                    wording,
                )
                .map(|presentation| presentation.remind(updates.reminder()));
                (updates.state().clone(), presentation)
            }
            None => (UpdateState::Unavailable, None),
        };
        let rows = self.servers.read(cx).rows();
        // Nothing asks for action, so the pill stays hidden. Check for Updates stays in the
        // application menu.
        let Some(pill) = UpdatePill::resolve(presentation.clone(), &rows) else {
            return div().into_any_element();
        };
        let mut entries = vec![client_section(presentation.as_ref(), &state, wording)];
        entries.push(MenuEntry::separator());
        entries.extend(server_sections(rows));

        let appearance = chrome(cx);
        let glyph_size = appearance.icons.metrics(IconRole::Caption).glyph_size;
        let handle = window.window_handle();
        let servers = self.servers.downgrade();
        let placement = MenuPlacementConfig::new(MenuPlacement::Bottom, MenuAlignment::End);
        let variant = match pill.tone() {
            PillTone::Primary => ButtonVariant::Primary,
            PillTone::Neutral | PillTone::Warning => ButtonVariant::Secondary,
        };
        let label = pill.label();
        let description = pill.description();
        let control = match pill.command() {
            PillCommand::Menu => Menu::new("update-control-menu", label, entries)
                .button_trigger(variant, ButtonSize::Small, ButtonShape::Capsule)
                .size(MenuSize::Wide)
                .placement(placement)
                .debug_selector("update-control-menu")
                .on_lifecycle(refresh_on_open(servers))
                .on_activate(move |activation, _, cx| {
                    UpdateControl::activate(activation.action().clone(), handle, cx)
                })
                .into_any_element(),
            command => {
                let enabled = !matches!(command, PillCommand::Client(None));
                let dot = appearance.spacing(5.0);
                ComboButton::new("update-control", label, entries)
                    .variant(variant)
                    .size(ButtonSize::Small)
                    .shape(ButtonShape::Capsule)
                    .disabled(!enabled)
                    .when_some(pill.glyph(), |button, glyph| {
                        button.leading(pill_glyph(glyph, glyph_size))
                    })
                    .accessibility_description(description.clone())
                    .tooltip(
                        Tooltip::new("update-control-tooltip", description)
                            .debug_selector("update-control-tooltip"),
                    )
                    .when(pill.tone() == PillTone::Warning, |button| {
                        let pair = appearance.semantic_text_pairs.warning_status;
                        let paint = |amount| {
                            ButtonPaint::new(
                                gpui_color(pair.background.mix(pair.primary, amount)),
                                gpui_color(pair.primary),
                                gpui::rgba(0),
                            )
                        };
                        button.contextual_style(
                            ButtonVariantStyle::new(
                                paint(0.0),
                                paint(0.12),
                                paint(0.2),
                                paint(0.0),
                            ),
                            gpui_color(pair.primary),
                        )
                    })
                    .menu_label("Updates")
                    .menu_size(MenuSize::Wide)
                    .placement(placement)
                    .when(pill.marked(), |button| {
                        button.menu_indicator(move |color| {
                            div()
                                .debug_selector(|| "update-control-servers-dot".to_owned())
                                .size(dot)
                                .rounded_full()
                                .bg(color)
                                .into_any_element()
                        })
                    })
                    .debug_selector("update-control")
                    .on_activate(move |_, _, cx| activate_pill(command.clone(), handle, cx))
                    .on_menu_lifecycle(refresh_on_open(servers))
                    .on_menu_activate(move |activation, _, cx| {
                        UpdateControl::activate(activation.action().clone(), handle, cx)
                    })
                    .into_any_element()
            }
        };
        // The pill owns its clicks inside the title bar's drag region. Its trailing edge stops
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
                    .child(control),
            )
            .into_any_element()
    }
}
