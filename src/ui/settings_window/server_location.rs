//! "Git on <server>": the selected server's Git and its New Worktree Location.
//!
//! These Server Settings are outside Settings import, export and reset. The section reads and
//! writes only the selected server, over its existing connection, and only while Settings shows
//! the section. Choosing another server starts over, so a draft or an answer for one server never
//! reaches another. A server that is not connected shows the values last read from it, which the
//! server selection retains across launches with that server's other last-known facts.
use super::git::{WORKTREE_LOCATION_DESCRIPTION, worktree_location_problem};
use super::server_list::ServerChoice;
use super::server_selection;
use super::*;
use crate::repository_status::GitToolStatus;
use crate::server_context::LastKnownFacts;
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::server_worktrees::{Client, WriteError};
use crate::ui::shared_workspaces::SharedRefusalReason;
use crate::ui::sidebar_window::form::CaptionTone;
use crate::worktrees::path_template::{self, WorktreePathTemplateError};
use gpui::{Subscription, Task, Text};
use spaceterm_server::worktree::{Error, Operation, Read, ReadReply, Settings};
use spaceterm_ui::{TextInput, TextInputEvent, TextInputVariant};
use std::time::Duration;

/// How long typing pauses before the owning server checks the folder, so each keystroke does not
/// cost a round trip.
const SERVER_CHECK_DELAY: Duration = Duration::from_millis(300);
/// How often a connected server's New Worktree Location is read again, so another device's change
/// appears without reopening Settings.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// The field's width beside its label, wide enough for the default location.
const FIELD_WIDTH: f32 = 260.0;

/// What New Worktree Location shows and may write, apart from GPUI. The field's text mirrors
/// `draft`. A method that changes what the field should show returns that text.
#[derive(Debug, Default)]
struct LocationModel {
    /// The server's value as last read or written. It stays while the server is disconnected.
    known: Option<Settings>,
    /// The value the draft edits. A write names its version, so the server refuses it when
    /// another device changed the value first.
    base: Option<Settings>,
    draft: String,
    problem: Option<WorktreePathTemplateError>,
    notice: Option<LocationNotice>,
    connected: bool,
    saving: bool,
}

/// Something the field says about the last write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocationNotice {
    /// Another device changed the value first, and the field now shows that device's value.
    Changed,
    Refused(SharedRefusalReason),
    Failed,
}

impl LocationModel {
    /// The server's last-known value arrived, possibly after the server was chosen. It shows
    /// until the server answers with its own value and version, which an edit needs.
    fn remembered(&mut self, template: Option<String>) -> Option<String> {
        let template = template.filter(|_| self.known.is_none())?;
        (self.draft != template).then(|| {
            self.draft.clone_from(&template);
            template
        })
    }

    fn has_draft(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| base.template != self.draft)
    }

    /// The server answered a read. The field follows it unless the person is changing it.
    fn received(&mut self, settings: Settings) -> Option<String> {
        self.connected = true;
        if self
            .known
            .as_ref()
            .is_some_and(|known| known.version > settings.version)
        {
            return None;
        }
        self.known = Some(settings.clone());
        if self.saving || self.has_draft() || self.base.as_ref() == Some(&settings) {
            return None;
        }
        self.problem = None;
        self.draft = settings.template.clone();
        self.base = Some(settings);
        Some(self.draft.clone())
    }

    /// The server can no longer be read. A draft returns to the value last read, which cannot
    /// change until the server is connected again.
    fn disconnected(&mut self) -> Option<String> {
        self.connected = false;
        if self.saving {
            return None;
        }
        self.notice = None;
        self.revert()
    }

    /// The person changed the text. Returns the text for the owning server to check, when the
    /// check only it can make is still needed.
    fn edited(&mut self, text: &str) -> Option<String> {
        text.clone_into(&mut self.draft);
        self.notice = None;
        self.problem = path_template::validate(text).err();
        (self.connected && self.problem.is_none() && self.has_draft()).then(|| text.to_owned())
    }

    /// The owning server checked `template`. An answer about earlier text lands nowhere.
    fn validated(&mut self, template: &str, result: Result<(), Error>) {
        if template != self.draft || self.problem.is_some() {
            return;
        }
        if let Err(Error::Template(problem)) = result {
            self.problem = Some(problem);
        }
    }

    /// The write saving the draft, when it is a valid change. The draft's text and the version it
    /// edits identify the write.
    fn commit(&mut self) -> Option<Settings> {
        let base = self.base.as_ref()?;
        if !self.connected || self.saving || self.problem.is_some() || self.draft == base.template {
            return None;
        }
        self.saving = true;
        Some(Settings {
            template: self.draft.clone(),
            version: base.version,
        })
    }

    /// The server answered `write`. Returns whether another device changed the value first, so
    /// the caller reads it again for [`Self::changed_elsewhere`].
    fn committed(&mut self, write: Settings, result: Result<(), WriteError>) -> bool {
        self.saving = false;
        match result {
            Ok(()) => {
                let saved = Settings {
                    template: write.template,
                    version: write.version.saturating_add(1),
                };
                if self
                    .known
                    .as_ref()
                    .is_none_or(|known| known.version <= saved.version)
                {
                    self.known = Some(saved.clone());
                }
                self.base = Some(saved);
            }
            Err(WriteError::Server(Error::Changed)) => return true,
            Err(WriteError::Server(Error::Template(problem))) => {
                if self.draft == write.template {
                    self.problem = Some(problem);
                }
            }
            Err(WriteError::Refused(reason)) => self.notice = Some(LocationNotice::Refused(reason)),
            Err(WriteError::Server(_)) => self.notice = Some(LocationNotice::Failed),
        }
        false
    }

    /// Another device changed the value first. The field reloads the server's value, or the last
    /// one read when the server did not answer again, and says so. The person's draft is not
    /// written over the other device's change.
    fn changed_elsewhere(&mut self, current: Option<Settings>) -> Option<String> {
        if let Some(current) = current
            && self
                .known
                .as_ref()
                .is_none_or(|known| known.version <= current.version)
        {
            self.known = Some(current);
        }
        self.notice = Some(LocationNotice::Changed);
        self.revert()
    }

    /// Escape returns the field to the server's value.
    fn cancelled(&mut self) -> Option<String> {
        self.notice = None;
        self.revert()
    }

    fn revert(&mut self) -> Option<String> {
        let known = self.known.clone()?;
        self.problem = None;
        self.base = Some(known.clone());
        (self.draft != known.template).then(|| {
            self.draft = known.template;
            self.draft.clone()
        })
    }

    fn editable(&self) -> bool {
        self.connected && !self.saving
    }

    fn differs_from_default(&self) -> bool {
        self.editable()
            && self
                .base
                .as_ref()
                .is_some_and(|base| base.template != Settings::default().template)
    }

    /// What the field says under itself, if anything.
    fn message(&self) -> Option<(&'static str, CaptionTone)> {
        if let Some(problem) = self.problem {
            return Some((worktree_location_problem(problem), CaptionTone::Error));
        }
        Some(match self.notice? {
            LocationNotice::Changed => (
                "Another device changed this location.",
                CaptionTone::Warning,
            ),
            LocationNotice::Refused(reason) => (reason.explanation(), CaptionTone::Error),
            LocationNotice::Failed => ("Couldn\u{2019}t save this location.", CaptionTone::Error),
        })
    }
}

pub(super) struct ServerLocation {
    input: Entity<TextInput>,
    _events: Subscription,
    model: LocationModel,
    git: GitToolStatus,
    /// The server whose values the section shows.
    target: Option<ServerChoice>,
    /// Counts the times the section started over, so a read or write answered for an earlier
    /// server lands nowhere.
    generation: u64,
    /// The writer that answered the latest read, which a change writes through.
    client: Option<Client>,
    poll: Option<Task<()>>,
    /// Whether the server reported its Git since it was chosen, so last-known facts no longer
    /// replace it.
    git_answered: bool,
    /// The owning server's check of the text being typed. Replacing it cancels the earlier one.
    check: Option<Task<()>>,
}

impl ServerLocation {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        let selector = "settings-server-worktree-location";
        let input = cx.new(|cx| {
            TextInput::new(selector, "New Worktree Location", String::new(), window, cx)
                .variant(TextInputVariant::Bare)
                .input_length_limit(Some(1024))
                .debug_selector(selector)
        });
        let events = cx.subscribe_in(
            &input,
            window,
            |settings, input, event: &TextInputEvent, _, cx| {
                settings.server_location_event(input, event, cx);
            },
        );
        Self {
            input,
            _events: events,
            model: LocationModel::default(),
            git: GitToolStatus::Unknown,
            target: None,
            generation: 0,
            client: None,
            poll: None,
            git_answered: false,
            check: None,
        }
    }

    /// Shows `target` from its last-known facts and stops reading the previous server.
    fn choose(&mut self, target: Option<ServerChoice>, cx: &mut Context<SettingsWindow>) {
        self.target = target;
        self.generation = self.generation.wrapping_add(1);
        self.git = GitToolStatus::Unknown;
        self.git_answered = false;
        self.model = LocationModel::default();
        self.client = None;
        self.poll = None;
        self.check = None;
        self.show(Some(String::new()), cx);
        self.show_last_known(cx);
    }

    /// Shows the selected server's last-known facts wherever the server hasn't answered itself
    /// since it was chosen. Facts that load after the choice show the same way.
    fn show_last_known(&mut self, cx: &mut Context<SettingsWindow>) {
        let Some(facts) = self
            .target
            .as_ref()
            .and_then(|target| server_selection::facts(&target.key(), cx))
        else {
            return;
        };
        if !self.git_answered && self.git != facts.git {
            self.git = facts.git;
            cx.notify();
        }
        let text = self.model.remembered(facts.worktree_location);
        self.show(text, cx);
    }

    /// Records what the selected server answered among its last-known facts, so the section
    /// shows it while that server isn't connected, even after relaunch. The record's other facts
    /// and the time they were observed stay as their owner wrote them.
    fn retain(&self, cx: &mut App) {
        let Some(target) = &self.target else {
            return;
        };
        let template = self
            .model
            .known
            .as_ref()
            .map(|known| known.template.clone());
        let git = self
            .git_answered
            .then_some(self.git)
            .filter(|git| *git != GitToolStatus::Unknown);
        if git.is_none() && template.is_none() {
            return;
        }
        let key = target.key();
        let previous = server_selection::facts(&key, cx);
        let mut facts = previous.clone().unwrap_or_else(|| LastKnownFacts {
            observed_at: unix_seconds(),
            ..LastKnownFacts::default()
        });
        if let Some(git) = git {
            facts.git = git;
        }
        if template.is_some() {
            facts.worktree_location = template;
        }
        if previous.as_ref() != Some(&facts) {
            server_selection::remember(key, facts, cx);
        }
    }

    /// Stops reading the selected server and keeps what was read from it.
    fn disconnect(&mut self, cx: &mut Context<SettingsWindow>) {
        self.client = None;
        self.poll = None;
        self.check = None;
        let text = self.model.disconnected();
        self.show(text, cx);
    }

    fn show(&self, text: Option<String>, cx: &mut Context<SettingsWindow>) {
        if let Some(text) = text {
            self.input.update(cx, |input, cx| {
                input.set_value(text, cx);
            });
        }
    }
}

impl SettingsWindow {
    /// Whether SpaceTerm presents `target` now, so its Server Settings can be read.
    fn server_location_reachable(&self, target: &ServerChoice) -> bool {
        match target {
            ServerChoice::Local => self
                .local_server
                .as_ref()
                .and_then(|row| row.worktree_server())
                .is_some(),
            ServerChoice::Direct(route) => self
                .direct_routes
                .as_ref()
                .is_some_and(|rows| rows.connections.server(route).is_some()),
        }
    }

    /// Follows the selected server's Git settings while the section showing New Worktree Location
    /// is open. Selection, section, connection and last-known facts changes call this; rendering
    /// never does, so painting asks no server anything.
    pub(super) fn sync_server_location(&mut self, cx: &mut Context<Self>) {
        // End All Sessions and direct network access follow the same server through the same
        // changes.
        self.sync_end_all_sessions(cx);
        self.sync_direct_access(cx);
        let target = self.selected_server();
        let shown =
            self.active_section == SettingsRowId::ServerWorktreeLocation.descriptor().section;
        let live = shown
            && target
                .as_ref()
                .is_some_and(|target| self.server_location_reachable(target));
        let state = &mut self.server_location;
        if state.target != target {
            state.choose(target.clone(), cx);
        } else {
            if !live && state.poll.is_some() {
                state.disconnect(cx);
            }
            state.show_last_known(cx);
        }
        let Some(target) = target.filter(|_| live) else {
            return;
        };
        if state.poll.is_some() {
            return;
        }
        let generation = state.generation;
        state.poll = Some(cx.spawn(async move |settings, cx| {
            let mut git_read = false;
            loop {
                // The Workspace Window may register a newly presented server's writer after
                // this window hears of it, so each reading looks the writer up again.
                let client = cx.update(|cx| match &target {
                    ServerChoice::Local => Client::get(cx),
                    ServerChoice::Direct(route) => Client::presented_direct(route, cx),
                });
                let read = match &client {
                    Some(client) => Some(cx.update(|cx| client.read(Read::Settings, cx)).await),
                    None => None,
                };
                // Git changes only when someone installs it, so one answer per connection does.
                let git = match &client {
                    Some(client) if !git_read && matches!(read, Some(Ok(_))) => {
                        Some(cx.update(|cx| client.read(Read::Git, cx)).await)
                    }
                    _ => None,
                };
                if let Some(Ok(ReadReply::Git(_))) = git {
                    git_read = true;
                }
                if settings
                    .update(cx, |settings, cx| {
                        let state = &mut settings.server_location;
                        if state.generation != generation {
                            return;
                        }
                        state.client = client;
                        // A missed answer pauses editing and keeps the draft, which the next
                        // answer resumes.
                        match read {
                            Some(Ok(ReadReply::Settings(saved))) => {
                                let text = state.model.received(saved);
                                state.show(text, cx);
                            }
                            _ => state.model.connected = false,
                        }
                        if let Some(Ok(ReadReply::Git(git))) = git {
                            state.git = git;
                            state.git_answered = true;
                        }
                        state.retain(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor().timer(POLL_INTERVAL).await;
            }
        }));
    }

    /// The selected server's Git, as last read from it.
    pub(super) fn server_git(&self) -> GitToolStatus {
        self.server_location.git
    }

    /// The "Git on <server>" footnote while its rows cannot change, saying how to change them.
    pub(super) fn server_git_footnote(&self) -> Option<SharedString> {
        if self.server_location.model.connected {
            return None;
        }
        Some(
            match self.selected_server() {
                Some(ServerChoice::Local) | None => format!(
                    "Start the server on {} to change its Git settings.",
                    self.selected_server_name()
                ),
                Some(_) => format!(
                    "Connect to {} to change its Git settings.",
                    self.selected_server_name()
                ),
            }
            .into(),
        )
    }

    pub(super) fn server_location_differs(&self) -> bool {
        self.server_location.model.differs_from_default()
    }

    pub(super) fn reset_server_location(&mut self, cx: &mut Context<Self>) {
        let template = Settings::default().template;
        let state = &mut self.server_location;
        state.check = None;
        state.model.edited(&template);
        state.show(Some(template), cx);
        self.commit_server_location(cx);
    }

    pub(super) fn server_location_event(
        &mut self,
        input: &Entity<TextInput>,
        event: &TextInputEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            TextInputEvent::ValueChanged(_) => {
                let text = input.read(cx).value().to_owned();
                let state = &mut self.server_location;
                let check = state.model.edited(&text);
                let generation = state.generation;
                state.check = check.zip(state.client.clone()).map(|(template, client)| {
                    cx.spawn(async move |settings, cx| {
                        cx.background_executor().timer(SERVER_CHECK_DELAY).await;
                        let read = Read::ValidateLocationTemplate {
                            template: template.clone(),
                        };
                        let answer = cx.update(|cx| client.read(read, cx)).await;
                        let _ = settings.update(cx, |settings, cx| {
                            let state = &mut settings.server_location;
                            if state.generation == generation {
                                state.model.validated(&template, answer.map(drop));
                                cx.notify();
                            }
                        });
                    })
                });
                cx.notify();
            }
            TextInputEvent::Submitted | TextInputEvent::FocusLost => {
                self.commit_server_location(cx)
            }
            TextInputEvent::Cancelled => {
                let state = &mut self.server_location;
                state.check = None;
                let text = state.model.cancelled();
                state.show(text, cx);
                cx.notify();
            }
            _ => {}
        }
    }

    fn commit_server_location(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.server_location;
        // The change goes to the server whose value the person edited, through its own writer.
        let Some(client) = state.client.clone() else {
            return;
        };
        let Some(write) = state.model.commit() else {
            return;
        };
        let generation = state.generation;
        let operation = Operation::SetLocation {
            version: write.version,
            template: write.template.clone(),
        };
        let written = client.write(operation, cx);
        cx.spawn(async move |settings, cx| {
            let result = written.await.and_then(crate::ui::server_worktrees::result);
            let current = if result == Err(WriteError::Server(Error::Changed)) {
                match cx.update(|cx| client.read(Read::Settings, cx)).await {
                    Ok(ReadReply::Settings(current)) => Some(current),
                    _ => None,
                }
            } else {
                None
            };
            let _ = settings.update(cx, |settings, cx| {
                let state = &mut settings.server_location;
                // The person chose another server meanwhile. The change finished on the server
                // it was written to, and the section now shows a different one.
                if state.generation != generation {
                    return;
                }
                if state.model.committed(write, result) {
                    let text = state.model.changed_elsewhere(current);
                    state.show(text, cx);
                }
                state.retain(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// New Worktree Location's description. Validation and save results appear under the field.
    pub(super) fn server_location_caption(&self, _: &App) -> (SharedString, CaptionTone) {
        (
            SharedString::new_static(WORKTREE_LOCATION_DESCRIPTION),
            CaptionTone::Guidance,
        )
    }

    /// The field at a fixed width, with what it says about its text directly under it.
    pub(super) fn render_server_location(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let model = &self.server_location.model;
        let editable = model.editable();
        let invalid = model.problem.is_some();
        let message = model.message();
        let input = self.server_location.input.clone();
        input.update(cx, |input, cx| input.set_editable(editable, cx));
        let focus = input.read(cx).focus_handle();
        let click_focus = focus.clone();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let field = spaceterm_ui::field_frame(
            "settings-server-worktree-location-frame",
            &focus,
            spaceterm_ui::FieldState::default()
                .disabled(!editable)
                .invalid(invalid),
            super::super::chrome_geometry::RadiusRole::Control.pixels(),
            cx,
        )
        .debug_selector(|| "settings-server-worktree-location-frame".to_owned())
        .h(appearance.height(28.0, 13.0))
        .w_full()
        .flex()
        .items_center()
        .px(appearance.spacing(8.0))
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(colors.text))
        .on_click(move |_, window, cx| {
            click_focus.focus(window, cx);
            cx.stop_propagation();
        })
        .child(input);
        gpui::div()
            .flex()
            .flex_col()
            .items_end()
            .gap(appearance.spacing(3.0))
            .w(appearance.spacing(FIELD_WIDTH))
            .child(field)
            .children(message.map(|(message, tone)| {
                let color = match tone {
                    CaptionTone::Guidance => colors.text_muted,
                    CaptionTone::Warning => colors.warning,
                    CaptionTone::Error => colors.error,
                };
                gpui::div()
                    .debug_selector(|| "settings-server-worktree-location-message".to_owned())
                    .w_full()
                    .text_right()
                    .whitespace_normal()
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .text_color(gpui_color(color))
                    .child(Text::new(
                        SharedString::new_static("settings-server-worktree-location-message")
                            .into(),
                        SharedString::new_static(message),
                    ))
            }))
            .into_any_element()
    }
}

/// Now, as the Unix seconds a last-known fact carries.
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
#[path = "server_location_tests.rs"]
mod tests;
