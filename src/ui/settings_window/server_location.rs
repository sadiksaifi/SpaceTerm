//! This server-owned preference is deliberately outside Settings import, export and reset.
use super::*;
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::server_worktrees::Client;
use crate::ui::sidebar_window::form::CaptionTone;
use gpui::Task;
use spaceterm_server::worktree::{Error, Operation, Read, ReadReply, Settings};
use spaceterm_ui::{TextInput, TextInputEvent};

pub(super) struct ServerLocation {
    field: super::git::WorktreeLocationField,
    saved: Option<Settings>,
    connected: bool,
    offered: bool,
    saving: bool,
    notice: Option<&'static str>,
    poll: Option<Task<()>>,
    window: gpui::AnyWindowHandle,
}
impl ServerLocation {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        Self {
            field: super::git::WorktreeLocationField::with_scope("", true, window, cx),
            saved: None,
            connected: false,
            offered: true,
            saving: false,
            notice: None,
            poll: None,
            window: window.window_handle(),
        }
    }
}
impl SettingsWindow {
    fn follow_server_location(&mut self, cx: &mut Context<Self>) {
        if self
            .local_server
            .as_ref()
            .and_then(|r| r.worktree_server())
            .is_none()
        {
            return;
        }
        if self.server_location.poll.is_some() {
            return;
        }
        let Some(client) = Client::get(cx) else {
            return;
        };
        self.server_location.poll = Some(cx.spawn(async move |settings, cx| {
            loop {
                let request = cx.update(|cx| client.read(Read::Settings, cx));
                let read = request.await;
                if settings
                    .update(cx, |settings, cx| {
                        let state = &mut settings.server_location;
                        state.connected = read.is_ok();
                        if client.availability().0 {
                            state.offered = client.supports_worktree_writes();
                        }
                        if let Ok(ReadReply::Settings(saved)) = read {
                            if !state.saving
                                && !state.field.input.read(cx).is_focused()
                                && state.field.problem.is_none()
                                && state.notice.is_none()
                            {
                                state.field.input.update(cx, |input, cx| {
                                    input.set_value(saved.template.clone(), cx)
                                });
                            }
                            state.saved = Some(saved);
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
                let saved = self.server_location.saved.clone().unwrap_or_default();
                self.server_location.field.problem = None;
                self.server_location.notice = None;
                input.update(cx, |input, cx| input.set_value(saved.template, cx));
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
        let Some(client) = Client::get(cx) else {
            return;
        };
        let draft = template.clone();
        let write = client.write(Operation::SetLocation { version, template }, cx);
        let handle = self.server_location.window;
        self.server_location.saving = true;
        cx.spawn(async move |settings,cx| {
            let result=write.await.and_then(crate::ui::server_worktrees::result);
            let current=if result==Err(Error::Changed) {Some(cx.update(|cx|client.read(Read::Settings,cx)).await)} else {None};
            let _=handle.update(cx,|_,window,cx| {
                let _=settings.update(cx,|settings,cx| {
                    settings.server_location.saving=false;
                    settings.server_location.notice=match result {Ok(())=>None,Err(Error::Changed)=>Some("Another client changed this setting. Your draft is kept for review."),Err(Error::Location)=>Some("The server refused this location template. Review the placeholders and path."),Err(_)=>Some("The server could not confirm this change. Connect to the server to resolve the original request.")};
                    if let Some(Ok(ReadReply::Settings(saved)))=current {
                        settings.server_location.saved=Some(saved.clone());
                        let title="Change Not Applied";
                        let message=format!("Another client changed New Worktree Location. On the server: {}. Your change: {}.",saved.template,draft);
                        let owner=cx.weak_entity();
                        let _=spaceterm_ui::Alert::new(spaceterm_ui::ModalId::new("server-worktree-location-conflict"),title,title,message,vec![spaceterm_ui::ModalAction::new(true,"Try Again",spaceterm_ui::ModalActionRole::Affirmative,"server-worktree-location-retry"),spaceterm_ui::ModalAction::new(false,"Cancel",spaceterm_ui::ModalActionRole::Cancel,"server-worktree-location-cancel")]).intent(spaceterm_ui::AlertIntent::Warning).present(window,cx,move |outcome,cx| {
                            if matches!(outcome,spaceterm_ui::AlertOutcome::Activated {action_id:true,..}) {let _=owner.update(cx,|settings,cx|settings.commit_server_location(cx));}
                        });
                    }
                    cx.notify();
                });
            });
        }).detach();
        cx.notify();
    }
    pub(super) fn server_location_caption(&self, cx: &App) -> (SharedString, CaptionTone) {
        let state = &self.server_location;
        if let Some(notice) = state.notice {
            return (notice.into(), CaptionTone::Error);
        }
        if state.saving {
            return ("Saving…".into(), CaptionTone::Guidance);
        }
        if !state.connected {
            return (
                "Connect to the local server to change it.".into(),
                CaptionTone::Guidance,
            );
        }
        if state.field.problem.is_some() {
            return super::git::worktree_location_caption(
                state.field.input.read(cx).value(),
                state.field.problem,
            );
        }
        ("Where this server creates new Worktrees for shared Workspaces. Use {repository} and {branch}. For example: ~/.worktrees/my-app/feature-login".into(),CaptionTone::Guidance)
    }
    pub(super) fn render_server_location(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.follow_server_location(cx);
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
