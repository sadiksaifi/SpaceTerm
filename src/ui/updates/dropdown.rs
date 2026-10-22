//! The title bar Updates menu: the client's update above one section per actual SpaceTerm Server.
//! Each row reaches an existing journey. The client rows drive the client update prompts, and a
//! server row hands its exact server to that server's Settings update journey.

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Context, Entity, Pixels, Rgba, SharedString, Subscription,
    Window, div,
};
use spaceterm_ui::{
    DeterminateProgress, Icon, IconName, Menu, MenuAlignment, MenuEntry, MenuPlacement,
    MenuPlacementConfig, MenuSize, ProgressRing, ProgressSize, ProgressState,
};

use super::server_rows::{ServerUpdateAction, ServerUpdateRow, ServerUpdates};
use super::{
    CHECK_FOR_UPDATES_TITLE, CURRENT_VERSION, ControlCommand, ControlGlyph, ControlPresentation,
    ControlReminder, InstallRequest, check_for_updates, failure_message, present_stop_download,
    request_install, retry_install, service,
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

/// The Updates menu at the trailing end of one window's title bar. It stays present so each
/// server's update remains reachable while the client has none.
pub(crate) struct UpdateControl {
    servers: Entity<ServerUpdates>,
    _subscriptions: Vec<Subscription>,
}

impl UpdateControl {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let servers = cx.new(ServerUpdates::new);
        let mut subscriptions = vec![cx.observe(&servers, |_, _, cx| cx.notify())];
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

/// One section per actual server, named by its machine and account. A row without an action
/// reports the server's status truthfully and cannot be chosen.
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
            let entry = match row.action {
                Some(action) => MenuEntry::action(row.detail, UpdatesMenuAction::Server(action)),
                None => status_row(row.detail),
            };
            MenuEntry::section(
                row.label,
                vec![entry.debug_selector(format!("update-server-{index}"))],
            )
        })
        .collect()
}

/// The trigger's leading glyph while the client has a step in progress. The user's own step takes
/// the accent paint, and an overdue update takes the warning paint.
fn trigger_glyph(
    presentation: &ControlPresentation,
    accent: Rgba,
    warning: Rgba,
    glyph_size: Pixels,
) -> impl Fn(Rgba) -> AnyElement + 'static {
    let glyph = presentation.glyph;
    let paint = match presentation.reminder {
        Some(ControlReminder::Overdue) => Some(warning),
        _ if presentation.prominent => Some(accent),
        _ => None,
    };
    move |foreground| {
        let color = paint.unwrap_or(foreground);
        match glyph {
            ControlGlyph::Download => {
                Icon::new(IconName::Download, glyph_size, color).into_any_element()
            }
            ControlGlyph::Restart => {
                Icon::new(IconName::RotateCw, glyph_size, color).into_any_element()
            }
            ControlGlyph::Warning => {
                Icon::new(IconName::CircleAlert, glyph_size, color).into_any_element()
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
        let mut entries = vec![client_section(presentation.as_ref(), &state, wording)];
        entries.push(MenuEntry::separator());
        entries.extend(server_sections(self.servers.read(cx).rows()));

        let appearance = chrome(cx);
        let glyph_size = appearance.icons.metrics(IconRole::Caption).glyph_size;
        let handle = window.window_handle();
        let menu = Menu::new("update-control", "Updates", entries)
            .size(MenuSize::Wide)
            .placement(MenuPlacementConfig::new(
                MenuPlacement::Bottom,
                MenuAlignment::End,
            ))
            .when_some(presentation.as_ref(), |menu, presentation| {
                menu.leading_icon(trigger_glyph(
                    presentation,
                    gpui_color(appearance.colors.text_accent),
                    gpui_color(appearance.semantic_text_pairs.warning_status.primary),
                    glyph_size,
                ))
            })
            .debug_selector("update-control")
            .on_activate(move |activation, _, cx| {
                UpdateControl::activate(activation.action().clone(), handle, cx)
            });
        // The menu owns its clicks inside the title bar's drag region. Its trailing edge stops
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
                    .child(menu),
            )
    }
}
