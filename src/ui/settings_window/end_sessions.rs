//! End All Sessions for this computer's server, when Servers presents it. End All Sessions uses
//! account-local work admission, so a paired server offers none here.
//!
//! The action ends every live Terminal Session on that server for every client. The server keeps
//! running, and its Workspaces, Tabs, Panes, and control grants remain. The row reads the
//! server's writer, the one the Workspace Window shares, so the request uses the same operation
//! sequence and recovery as every other shared edit. Nothing here ever asks twice: a lost answer
//! is resolved by that writer's recovery, never by another request.
use super::server_list::ServerChoice;
use super::*;
use crate::ui::chrome_typography::ChromeTextStyleExt as _;
use crate::ui::server_worktrees::Client;
use crate::ui::shared_workspaces::SharedRefusalReason;
use crate::ui::sidebar_window::form::CaptionTone;
use gpui::Task;
use spaceterm_server::hierarchy::{CloseInventory, Id, Mutation, Outcome, RetainedOutcome};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, ModalAction,
    ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ProgressRing, ProgressSize,
    ProgressState,
};

pub(super) const END_SELECTOR: &str = "settings-server-end-all-sessions";
pub(super) const END_PROGRESS_SELECTOR: &str = "settings-server-end-all-sessions-progress";
pub(super) const END_CONFIRM_SELECTOR: &str = "settings-server-end-all-sessions-confirm";
pub(super) const END_CANCEL_SELECTOR: &str = "settings-server-end-all-sessions-cancel";
const LABEL: &str = "End All\u{2026}";
const CONFIRM: &str = "End All Sessions";

/// What the selected server's writer allows now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Availability {
    /// No writer has answered for this server yet.
    Unknown,
    /// The server is not connected, so nothing can be asked of it.
    Offline,
    /// The server is connected but doesn't offer End All Sessions.
    Unsupported,
    /// Another shared edit awaits its answer, and one edit is asked at a time.
    Pending,
    /// The server's current run and its Panes' Sessions, as the server last published them.
    Ready(CloseInventory),
}

impl Availability {
    fn of(snapshot: &crate::terminal::shared_workspace::SharedWorkspaceSnapshot) -> Self {
        if !snapshot.connected {
            return Self::Offline;
        }
        if !snapshot.end_all_sessions {
            return Self::Unsupported;
        }
        if snapshot.pending {
            return Self::Pending;
        }
        match snapshot
            .inventory
            .as_ref()
            .and_then(|inventory| inventory.close.as_ref())
        {
            Some(close) => Self::Ready(close.clone()),
            None => Self::Unknown,
        }
    }
}

/// Each running Session, by its Pane.
fn live_sessions(close: &CloseInventory) -> std::collections::BTreeSet<(Id, Id)> {
    close
        .sessions
        .iter()
        .filter_map(|pane| Some((pane.id, pane.session?)))
        .collect()
}

/// Why a confirmation no longer describes the server, if it doesn't. A confirmation names the
/// exact Sessions it showed, so a restart, an ended Session, and a newly started one each make it
/// stale.
pub(super) fn recheck(confirmed: &CloseInventory, current: &CloseInventory) -> Option<Ended> {
    if current.boot != confirmed.boot {
        Some(Ended::Restarted)
    } else if live_sessions(current) != live_sessions(confirmed) {
        Some(Ended::Changed)
    } else {
        None
    }
}

/// The request for the Sessions the person confirmed, which recovery resends unchanged.
fn request(confirmed: &CloseInventory) -> Mutation {
    Mutation::EndAllSessions {
        confirmation: confirmed.clone(),
    }
}

/// The answer to the last request, kept until the person selects another server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Ended {
    Accepted,
    /// The server restarted after the person confirmed, so the confirmation named other Sessions.
    Restarted,
    /// The server's Sessions changed after the person confirmed. The server answers a stale
    /// confirmation this way too, restart or not.
    Changed,
    Refused(SharedRefusalReason),
    /// The server answered with a refusal that has no specific explanation.
    Failed,
    /// The answer never arrived. The writer's recovery resolves the original request.
    Unconfirmed,
}

pub(super) struct EndAllSessions {
    /// The server the row presents.
    target: Option<ServerChoice>,
    /// Counts the times the row started over, so an answer for an earlier server lands nowhere.
    generation: u64,
    availability: Availability,
    working: bool,
    ended: Option<Ended>,
    client: Option<Client>,
    poll: Option<Task<()>>,
}

impl EndAllSessions {
    pub(super) fn new() -> Self {
        Self {
            target: None,
            generation: 0,
            availability: Availability::Unknown,
            working: false,
            ended: None,
            client: None,
            poll: None,
        }
    }

    fn start_over(&mut self, target: Option<ServerChoice>) {
        self.target = target;
        self.generation = self.generation.wrapping_add(1);
        self.availability = Availability::Unknown;
        self.working = false;
        self.ended = None;
        self.client = None;
        self.poll = None;
    }
}

/// The row's caption, derived from its state alone so every state reads one way. A connected
/// server ready for End All Sessions needs none: the row's count and its confirmation say it.
pub(super) fn caption(
    server: &str,
    availability: &Availability,
    working: bool,
    ended: Option<Ended>,
) -> Option<(SharedString, CaptionTone)> {
    let guidance = |text: String| Some((SharedString::from(text), CaptionTone::Guidance));
    if working {
        return guidance(format!("Ending every Terminal Session on {server}\u{2026}"));
    }
    match ended {
        Some(Ended::Accepted) => {
            return guidance(format!(
                "Every Terminal Session on {server} ended. Its Panes show Session Ended until \
                 someone chooses Start Session."
            ));
        }
        Some(Ended::Restarted) => {
            return Some((
                format!("{server} restarted, so nothing ended. Try again.").into(),
                CaptionTone::Warning,
            ));
        }
        Some(Ended::Changed) => {
            return Some((
                format!("The Terminal Sessions on {server} changed, so nothing ended. Try again.")
                    .into(),
                CaptionTone::Warning,
            ));
        }
        Some(Ended::Refused(reason)) => {
            return Some((reason.explanation().into(), CaptionTone::Error));
        }
        Some(Ended::Failed) => {
            return Some((
                format!("{server} couldn\u{2019}t end its Terminal Sessions.").into(),
                CaptionTone::Error,
            ));
        }
        Some(Ended::Unconfirmed) => {
            return Some((
                format!(
                    "SpaceTerm couldn\u{2019}t confirm whether {server}\u{2019}s Terminal Sessions ended. It will check when the connection returns."
                )
                .into(),
                CaptionTone::Warning,
            ));
        }
        None => {}
    }
    match availability {
        Availability::Unknown | Availability::Offline => {
            guidance(format!("Connect to {server} to end its Terminal Sessions."))
        }
        Availability::Unsupported => guidance(format!(
            "{server} doesn\u{2019}t support ending all its Terminal Sessions. Update the server \
             to use End All Sessions."
        )),
        Availability::Pending => guidance(format!(
            "End All Sessions is available once {server} answers an earlier change."
        )),
        // The row's count says how many run, and End All waits until one does.
        Availability::Ready(_) => None,
    }
}

/// The confirmation's title, message, and detail. Each names the selected server.
pub(super) fn confirmation(server: &str, live: usize) -> (String, String, String) {
    let message = match live {
        1 => "This ends 1 Terminal Session for every client and clears its output.".to_owned(),
        count => {
            format!("This ends {count} Terminal Sessions for every client and clears their output.")
        }
    };
    (
        format!("End All Sessions on {server}?"),
        message,
        "The server keeps running, and Workspaces, Tabs and Panes stay.".to_owned(),
    )
}

impl SettingsWindow {
    /// Follows the selected server's writer while the Servers section shows it. Selection,
    /// section, and connection changes call this; rendering never does.
    pub(super) fn sync_end_all_sessions(&mut self, cx: &mut Context<Self>) {
        // The presented server's facts follow the same selection, section and connection changes.
        self.sync_server_facts(cx);
        // Ending every Terminal Session administers the server, which only this computer's
        // server offers.
        let target = if self.active_section == SettingsSectionId::Servers {
            self.selected_server()
                .filter(|target| matches!(target, ServerChoice::Local))
        } else {
            None
        };
        let state = &mut self.end_all_sessions;
        if state.target != target {
            state.start_over(target.clone());
        }
        let Some(target) = target else {
            return;
        };
        if state.poll.is_some() {
            return;
        }
        let generation = state.generation;
        state.poll = Some(cx.spawn(async move |settings, cx| {
            loop {
                // The Workspace Window may register a newly presented server's writer after this
                // window hears of it, so each reading looks the writer up again.
                let client = cx.update(|cx| match &target {
                    ServerChoice::Local => Client::get(cx),
                    ServerChoice::Direct(_) => None,
                });
                let availability = client.as_ref().map_or(Availability::Unknown, |client| {
                    Availability::of(&client.source.snapshot())
                });
                if settings
                    .update(cx, |settings, cx| {
                        let state = &mut settings.end_all_sessions;
                        if state.generation != generation {
                            return;
                        }
                        if state.availability != availability {
                            state.availability = availability;
                            cx.notify();
                        }
                        state.client = client;
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
            }
        }));
    }

    /// How many Terminal Sessions run on the selected server, as it last published them, or
    /// `None` while it isn't connected.
    pub(super) fn running_sessions(&self) -> Option<usize> {
        match &self.end_all_sessions.availability {
            Availability::Ready(close) => Some(live_sessions(close).len()),
            _ => None,
        }
    }

    /// The caption under the button, in every state that needs one.
    pub(super) fn end_all_sessions_caption(&self) -> Option<(SharedString, CaptionTone)> {
        let state = &self.end_all_sessions;
        caption(
            &self.selected_server_name(),
            &state.availability,
            state.working,
            state.ended,
        )
    }

    /// Whether the person can ask for End All Sessions now.
    fn end_all_sessions_offered(&self) -> bool {
        let state = &self.end_all_sessions;
        !state.working
            && state.client.is_some()
            && matches!(&state.availability, Availability::Ready(close) if !live_sessions(close).is_empty())
    }

    /// Asks the person to confirm ending the selected server's Sessions. The alert names the
    /// server, and its confirmation applies only to that server in the same run.
    fn confirm_end_all_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.end_all_sessions_offered() {
            return;
        }
        let state = &self.end_all_sessions;
        // The confirmation names these exact Sessions, and only they are asked to end.
        let Availability::Ready(confirmed) = state.availability.clone() else {
            return;
        };
        let generation = state.generation;
        let server = self.selected_server_name();
        let (title, message, detail) = confirmation(&server, live_sessions(&confirmed).len());
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-server-end-all-sessions"),
            title.clone(),
            title,
            message,
            vec![
                ModalAction::new(
                    true,
                    CONFIRM,
                    ModalActionRole::Affirmative,
                    END_CONFIRM_SELECTOR,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    END_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .detail(detail)
        .present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = owner.update(cx, |settings, cx| {
                settings.end_all_sessions(generation, &confirmed, cx);
            });
        });
        if result.is_err() {
            eprintln!("failed to present the End All Sessions confirmation");
        }
    }

    /// Asks the server once to end the Sessions the person confirmed. A confirmation for another
    /// server, for the server's earlier run, or for Sessions that changed since, submits nothing.
    fn end_all_sessions(
        &mut self,
        generation: u64,
        confirmed: &CloseInventory,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.end_all_sessions;
        if state.generation != generation || state.working {
            return;
        }
        let Some(client) = state.client.clone() else {
            return;
        };
        // Read the server afresh: the confirmation named the Sessions it showed.
        match Availability::of(&client.source.snapshot()) {
            Availability::Ready(current) => {
                if let Some(stale) = recheck(confirmed, &current) {
                    state.ended = Some(stale);
                    cx.notify();
                    return;
                }
            }
            Availability::Offline | Availability::Unknown => {
                state.ended = Some(Ended::Refused(SharedRefusalReason::Offline));
                cx.notify();
                return;
            }
            Availability::Pending => {
                state.ended = Some(Ended::Refused(SharedRefusalReason::Pending));
                cx.notify();
                return;
            }
            Availability::Unsupported => {
                state.ended = Some(Ended::Failed);
                cx.notify();
                return;
            }
        }
        let completion = match client.source.submit_waiting(request(confirmed)) {
            Ok(completion) => completion,
            Err(reason) => {
                state.ended = Some(Ended::Refused(reason));
                cx.notify();
                return;
            }
        };
        state.working = true;
        state.ended = None;
        let source = client.source.clone();
        cx.spawn(async move |settings, cx| {
            let result = loop {
                // Reading the snapshot delivers this request's answer once the server gives it.
                source.snapshot();
                match completion.try_recv() {
                    Ok(result) => break Some(result),
                    Err(async_channel::TryRecvError::Closed) => break None,
                    Err(async_channel::TryRecvError::Empty) => (),
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
            };
            let _ = settings.update(cx, |settings, cx| {
                let state = &mut settings.end_all_sessions;
                if state.generation != generation {
                    return;
                }
                state.working = false;
                state.ended = Some(match result {
                    Some(Ok(RetainedOutcome {
                        outcome: Outcome::Accepted { .. },
                        ..
                    })) => Ended::Accepted,
                    Some(Ok(RetainedOutcome {
                        outcome: Outcome::StaleConfirmation,
                        ..
                    })) => Ended::Changed,
                    Some(Ok(_)) => Ended::Failed,
                    Some(Err(reason)) => Ended::Refused(reason),
                    None => Ended::Unconfirmed,
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// How many Terminal Sessions run, and End All. The count is the server's own while it is
    /// connected and the last one observed otherwise. End All stays focusable while working, so
    /// the keyboard keeps its place, and ignores activation.
    pub(super) fn render_end_all_sessions(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.end_all_sessions;
        let working = state.working;
        let offered = self.end_all_sessions_offered();
        let server = self.selected_server_name();
        let count = self.running_sessions().or_else(|| {
            let key = self.selected_server()?.key();
            super::server_selection::facts(&key, cx)?
                .running_sessions
                .and_then(|count| usize::try_from(count).ok())
        });
        let value = match count {
            Some(0) => SharedString::new_static("None"),
            Some(count) => SharedString::from(format!("{count} running")),
            None => SharedString::new_static("Unknown"),
        };
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let owner = cx.weak_entity();
        let button = Button::new(END_SELECTOR, LABEL)
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Regular)
            .disabled(!offered && !working)
            .tab_stop(true)
            .debug_selector(END_SELECTOR)
            .accessibility_description(SharedString::from(format!(
                "Ends every Terminal Session on {server} for every client."
            )))
            .on_activate(move |_, window, cx| {
                let _ = owner.update(cx, |settings, cx| {
                    settings.confirm_end_all_sessions(window, cx);
                });
            });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(
                div()
                    .id("settings-server-sessions-count")
                    .debug_selector(|| "settings-server-sessions-count".to_owned())
                    .role(gpui::accesskit::Role::Status)
                    .aria_label(SharedString::from(format!("Terminal Sessions: {value}")))
                    .chrome_text(
                        appearance
                            .typography
                            .style(crate::ui::chrome_typography::TextRole::Body),
                    )
                    .text_color(crate::ui::appearance::gpui_color(colors.text_secondary))
                    .child(value.clone()),
            )
            .when(working, |row| {
                row.child(
                    ProgressRing::new(
                        END_PROGRESS_SELECTOR,
                        "Ending Sessions",
                        ProgressState::Indeterminate,
                    )
                    .size(ProgressSize::Compact)
                    .debug_selector(END_PROGRESS_SELECTOR),
                )
            })
            .child(button)
            .into_any_element()
    }
}
