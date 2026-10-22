//! New Worktree Location for the server selected in the Servers list.
//!
//! This server-owned preference is deliberately outside Settings import, export and reset. The
//! field reads and writes only the selected server, and only while SpaceTerm presents it. Choosing
//! another server starts over, so a draft or an answer for one server never reaches another.
use super::server_list::ServerChoice;
use super::*;
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::server_worktrees::{Client, WriteError};
use crate::ui::sidebar_window::form::CaptionTone;
use gpui::Task;
use spaceterm_server::worktree::{Error, Operation, Read, ReadReply, Settings};
use spaceterm_ui::{TextInput, TextInputEvent};

pub(super) struct ServerLocation {
    field: super::git::WorktreeLocationField,
    saved: Option<Settings>,
    latest: Option<Settings>,
    connected: bool,
    offered: bool,
    saving: bool,
    notice: Option<String>,
    /// The server whose value the field shows.
    target: Option<ServerChoice>,
    /// Counts the times the field started over, so a read or write answered for an earlier server
    /// or connection lands nowhere.
    generation: u64,
    /// The writer that answered the latest read, which a change writes through.
    client: Option<Client>,
    poll: Option<Task<()>>,
    window: gpui::AnyWindowHandle,
}
impl ServerLocation {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        Self {
            field: super::git::WorktreeLocationField::new(window, cx),
            saved: None,
            latest: None,
            connected: false,
            offered: true,
            saving: false,
            notice: None,
            target: None,
            generation: 0,
            client: None,
            poll: None,
            window: window.window_handle(),
        }
    }
    /// Forgets everything read from or typed for the previous server and stops reading it.
    fn start_over(&mut self, target: Option<ServerChoice>, cx: &mut Context<SettingsWindow>) {
        self.target = target;
        self.generation = self.generation.wrapping_add(1);
        self.saved = None;
        self.latest = None;
        self.connected = false;
        self.offered = true;
        self.saving = false;
        self.notice = None;
        self.client = None;
        self.poll = None;
        self.field.problem = None;
        self.field
            .input
            .update(cx, |input, cx| input.set_value(String::new(), cx));
    }
    fn receive(&mut self, saved: Settings, cx: &mut Context<SettingsWindow>) {
        if self
            .latest
            .as_ref()
            .is_some_and(|latest| latest.version > saved.version)
        {
            return;
        }
        self.latest = Some(saved.clone());
        let draft = self
            .saved
            .as_ref()
            .is_some_and(|base| self.field.input.read(cx).value() != base.template);
        if self.saved.is_none()
            || (!self.saving
                && !self.field.input.read(cx).is_focused()
                && !draft
                && self.field.problem.is_none()
                && self.notice.is_none())
        {
            self.field
                .input
                .update(cx, |input, cx| input.set_value(saved.template.clone(), cx));
            self.saved = Some(saved);
        }
    }
    fn discard(&mut self, cx: &mut Context<SettingsWindow>) {
        let saved = self.latest.clone().unwrap_or_default();
        self.saved = Some(saved.clone());
        self.field.problem = None;
        self.notice = None;
        self.field
            .input
            .update(cx, |input, cx| input.set_value(saved.template, cx));
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
            ServerChoice::Ssh(destination) => self
                .ssh_servers
                .as_ref()
                .is_some_and(|rows| rows.connections.server(destination).is_some()),
            ServerChoice::Direct(route) => self
                .direct_routes
                .as_ref()
                .is_some_and(|rows| rows.connections.server(route).is_some()),
        }
    }

    /// Follows the selected server's New Worktree Location while the Servers section shows it.
    /// Selection, section and connection changes call this; rendering never does, so painting
    /// asks no server anything.
    pub(super) fn sync_server_location(&mut self, cx: &mut Context<Self>) {
        // End All Sessions and direct network access follow the same server through the same
        // changes.
        self.sync_end_all_sessions(cx);
        self.sync_direct_access(cx);
        let target = if self.active_section == SettingsSectionId::Servers {
            self.selected_server()
        } else {
            None
        };
        let reachable = target
            .as_ref()
            .is_some_and(|target| self.server_location_reachable(target));
        let state = &mut self.server_location;
        if state.target != target || (!reachable && (state.poll.is_some() || state.saved.is_some()))
        {
            state.start_over(target.clone(), cx);
        }
        let Some(target) = target.filter(|_| reachable) else {
            return;
        };
        if state.poll.is_some() {
            return;
        }
        let generation = state.generation;
        state.poll = Some(cx.spawn(async move |settings, cx| {
            loop {
                // The Workspace Window may register a newly presented server's writer after
                // this window hears of it, so each reading looks the writer up again.
                let client = cx.update(|cx| match &target {
                    ServerChoice::Local => Client::get(cx),
                    ServerChoice::Ssh(destination) => Client::presented_ssh(destination, cx),
                    ServerChoice::Direct(route) => Client::presented_direct(route, cx),
                });
                let read = match &client {
                    Some(client) => Some(cx.update(|cx| client.read(Read::Settings, cx)).await),
                    None => None,
                };
                if settings
                    .update(cx, |settings, cx| {
                        let state = &mut settings.server_location;
                        if state.generation != generation {
                            return;
                        }
                        state.connected = matches!(read, Some(Ok(_)));
                        if let Some(client) = &client
                            && client.availability().0
                        {
                            state.offered = client.supports_worktree_writes();
                        }
                        state.client = client;
                        if let Some(Ok(ReadReply::Settings(saved))) = read {
                            state.receive(saved, cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
            }
        }));
    }
    pub(super) fn offers_server_location(&self) -> bool {
        self.server_location.offered
    }
    pub(super) fn server_location_differs(&self) -> bool {
        self.server_location.connected
            && !self.server_location.saving
            && self
                .server_location
                .saved
                .as_ref()
                .is_some_and(|s| s.template != Settings::default().template)
    }
    pub(super) fn reset_server_location(&mut self, cx: &mut Context<Self>) {
        self.server_location.saved = self.server_location.latest.clone();
        self.server_location.field.problem = None;
        self.server_location.notice = None;
        self.server_location.field.input.update(cx, |input, cx| {
            input.set_value(Settings::default().template, cx)
        });
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
                self.server_location.field.problem =
                    crate::worktrees::path_template::validate(input.read(cx).value()).err();
                self.server_location.notice = None;
                cx.notify();
            }
            TextInputEvent::Submitted | TextInputEvent::FocusLost => {
                self.commit_server_location(cx)
            }
            TextInputEvent::Cancelled => {
                self.server_location.discard(cx);
                cx.notify();
            }
            _ => {}
        }
    }
    fn commit_server_location(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.server_location;
        let Some(saved) = state.saved.as_ref() else {
            return;
        };
        let template = state.field.input.read(cx).value().to_owned();
        if !state.connected
            || state.saving
            || state.field.problem.is_some()
            || template == saved.template
        {
            return;
        }
        let version = saved.version;
        // The change goes to the server whose value the person edited, through its own writer.
        let Some(client) = state.client.clone() else {
            return;
        };
        let generation = state.generation;
        let draft = template.clone();
        let write = client.write(Operation::SetLocation { version, template }, cx);
        let handle = self.server_location.window;
        self.server_location.saving = true;
        cx.spawn(async move |settings, cx| {
            let result = write.await.and_then(crate::ui::server_worktrees::result);
            let current = if result == Err(WriteError::Server(Error::Changed)) {
                Some(cx.update(|cx| client.read(Read::Settings, cx)).await)
            } else {
                None
            };
            let _ = handle.update(cx, |_, window, cx| {
                let _ = settings.update(cx, |settings, cx| {
                    // The person chose another server meanwhile. The change finished on the server
                    // it was written to, and the field now shows a different one.
                    if settings.server_location.generation != generation {
                        return;
                    }
                    settings.server_location.saving = false;
                    if result.is_ok() {
                        let saved = Settings {
                            template: draft.clone(),
                            version: version.saturating_add(1),
                        };
                        settings.server_location.saved = Some(saved.clone());
                        if settings.server_location.latest.as_ref().is_none_or(|latest| latest.version <= saved.version) {
                            settings.server_location.latest = Some(saved);
                        }
                    }
                    settings.server_location.notice = match result {
                        Ok(()) => None,
                        Err(WriteError::Server(Error::Changed)) => Some("Another client changed this setting. Your draft is kept for review.".into()),
                        Err(WriteError::Server(Error::Location)) => Some("The server refused this location template. Review the placeholders and path.".into()),
                        Err(WriteError::Refused(reason)) => Some(format!("SpaceTerm did not change New Worktree Location on the server. {}", reason.explanation())),
                        Err(_) => Some("The server could not complete this change. Review the server value before trying again.".into()),
                    };
                    if let Some(Ok(ReadReply::Settings(saved))) = current {
                        settings.present_server_location_conflict(saved, &draft, window, cx);
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }
    fn present_server_location_conflict(
        &mut self,
        saved: Settings,
        draft: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.server_location.latest = Some(saved.clone());
        let title = "Change Not Applied";
        let message = format!(
            "Another client changed New Worktree Location.\n\nOn the server:\n{}\n\nYour change:\n{draft}",
            saved.template,
        );
        let owner = cx.weak_entity();
        let generation = self.server_location.generation;
        let _ = spaceterm_ui::Alert::new(
            spaceterm_ui::ModalId::new("server-worktree-location-conflict"),
            title,
            title,
            message,
            vec![
                spaceterm_ui::ModalAction::new(
                    true,
                    "Try Again",
                    spaceterm_ui::ModalActionRole::Affirmative,
                    "server-worktree-location-retry",
                ),
                spaceterm_ui::ModalAction::new(
                    false,
                    "Cancel",
                    spaceterm_ui::ModalActionRole::Cancel,
                    "server-worktree-location-cancel",
                ),
            ],
        )
        .intent(spaceterm_ui::AlertIntent::Warning)
        .present(window, cx, move |outcome, cx| {
            let _ = owner.update(cx, |settings, cx| {
                if settings.server_location.generation != generation {
                    return;
                }
                if matches!(
                    outcome,
                    spaceterm_ui::AlertOutcome::Activated {
                        action_id: true,
                        ..
                    }
                ) {
                    // Retry against the value this confirmation showed, even if another poll arrived.
                    settings.server_location.saved = Some(saved.clone());
                    settings.commit_server_location(cx);
                } else {
                    settings.server_location.discard(cx);
                    cx.notify();
                }
            });
        });
    }
    pub(super) fn server_location_caption(&self, cx: &App) -> (SharedString, CaptionTone) {
        let state = &self.server_location;
        if let Some(notice) = &state.notice {
            return (notice.clone().into(), CaptionTone::Error);
        }
        if state.saving {
            return ("Saving…".into(), CaptionTone::Guidance);
        }
        // The card heading names the machine. A local server reads as the account's own.
        let server = match self.selected_server() {
            Some(ServerChoice::Ssh(destination)) => {
                format!("the server on {}", destination.as_str())
            }
            Some(ServerChoice::Direct(_)) => {
                format!("the server on {}", self.selected_server_name())
            }
            _ => "your account\u{2019}s server".to_owned(),
        };
        if !state.connected {
            return (
                format!("Connect to {server} to change it.").into(),
                CaptionTone::Guidance,
            );
        }
        if state.field.problem.is_some() {
            return super::git::worktree_location_caption(
                state.field.input.read(cx).value(),
                state.field.problem,
            );
        }
        (
            format!("Where {server} creates new Worktrees for its Workspaces. Use {{repository}} and {{branch}}. For example: ~/.worktrees/my-app/feature-login").into(),
            CaptionTone::Guidance,
        )
    }
    pub(super) fn render_server_location(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.server_location;
        let input = state.field.input.clone();
        input.update(cx, |input, cx| {
            input.set_editable(state.connected && !state.saving, cx)
        });
        let focus = input.read(cx).focus_handle();
        let click_focus = focus.clone();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        spaceterm_ui::field_frame(
            "settings-server-worktree-location-frame",
            &focus,
            spaceterm_ui::FieldState::default()
                .disabled(!state.connected || state.saving)
                .invalid(state.field.problem.is_some()),
            super::super::chrome_geometry::RadiusRole::Control.pixels(),
            cx,
        )
        .h(appearance.height(28.0, 13.0))
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .px(appearance.spacing(8.0))
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(colors.text))
        .on_click(move |_, window, cx| {
            click_focus.focus(window, cx);
            cx.stop_propagation();
        })
        .child(input)
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::settings_window::tests::open_settings;
    use gpui::TestAppContext;

    #[gpui::test]
    fn server_location_poll_keeps_the_drafts_original_version_until_review(
        cx: &mut TestAppContext,
    ) {
        use spaceterm_server::worktree::Settings;
        let (window, _, cx) = open_settings(cx);
        window.update(cx, |settings, cx| {
            settings.server_location.receive(
                Settings {
                    template: "~/.worktrees/{repository}/{branch}".into(),
                    version: 0,
                },
                cx,
            );
            settings
                .server_location
                .field
                .input
                .update(cx, |input, cx| input.set_value("~/draft/{branch}", cx));
            settings.server_location.receive(
                Settings {
                    template: "~/other-client/{branch}".into(),
                    version: 1,
                },
                cx,
            );
            assert_eq!(
                settings.server_location.field.input.read(cx).value(),
                "~/draft/{branch}"
            );
            assert_eq!(
                settings.server_location.saved.as_ref().unwrap().version,
                0,
                "the draft must conflict with the other client's version 1"
            );
            settings.server_location.discard(cx);
            assert_eq!(
                settings.server_location.field.input.read(cx).value(),
                "~/other-client/{branch}"
            );
            assert_eq!(settings.server_location.saved.as_ref().unwrap().version, 1);
            assert!(settings.server_location.notice.is_none());
        });
    }
}
