//! The Development Pane: a SpaceTerm Development window presenting one Terminal Session that the
//! local SpaceTerm Server owns.
//!
//! The window stays outside every Workspace, so production Panes, quit confirmation, and Workspace
//! state never see it. Closing the window or quitting SpaceTerm detaches and leaves the shell
//! running. Only End Session, or Close Pane inside the window, ends the shell.

use crate::application_identity::ApplicationIdentity;

/// Whether this build offers the Development Pane. Only SpaceTerm Development with its developer
/// tools does.
pub(crate) fn offered() -> bool {
    cfg!(feature = "developer-tools") && ApplicationIdentity::current().is_development()
}

#[cfg(not(feature = "developer-tools"))]
pub(crate) fn open_or_activate(_: &mut gpui::App) {}

#[cfg(feature = "developer-tools")]
pub(crate) use window::{DevelopmentPaneComposition, open_or_activate};

#[cfg(feature = "developer-tools")]
mod window {
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    use gpui::prelude::*;
    use gpui::{
        AnyElement, App, Bounds, Edges, Entity, FocusHandle, Global, Pixels, SharedString,
        Stateful, Task, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind,
        WindowOptions, div, px, size,
    };
    use spaceterm_ui::{
        Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, ClientWindowControls,
        FloatingRole, Icon, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole,
        ModalId, ModalLayer, ModalPresentationHandle, ProgressRing, ProgressSize, ProgressState,
        WindowControlSide,
    };

    use super::super::appearance::{ChromeAppearance, gpui_color};
    use super::super::appearance_runtime::{self, WindowAppearanceOwner, WindowTrafficLightOwner};
    use super::super::chrome_icons::IconRole;
    use super::super::chrome_typography::{ChromeTextStyleExt as _, TextRole};
    use super::super::pane_lifecycle::{PaneConstruction, PaneLifecycleDependencies};
    use super::super::sidebar_window::WindowMovement;
    use super::super::terminal_pane::StatusIntent;
    use super::super::{ClosePane, TerminalPane, TerminalPaneEvent};
    use crate::application_identity::ApplicationIdentity;
    use crate::local_server::{LocalServer, ServerError};
    use crate::platform::local_filesystem::LocalFilesystemAuthority;
    use crate::platform::terminal_accessibility::{
        TerminalAccessibilityAdapter, TerminalAccessibilityAdapterFactory,
        TerminalAccessibilityUpdate,
    };
    use crate::platform::window_movement::{
        OperatingSystemWindowDragPlatform, WindowMovementFactory,
    };
    use crate::terminal::native_services::NativeServiceAdapters;
    use crate::terminal::server_session::{
        DevelopmentPaneState, DevelopmentPaneStatus, DevelopmentServerSessionFactory,
    };
    use crate::terminal::{
        AccessibilityNotifications, TerminalAccessibilityModel, TerminalKeyInputAdapterFactory,
        WorkspaceTerminalSessionFactory,
    };

    pub(super) const WINDOW_TITLE: &str = "Development Pane";
    /// The key context the window publishes around its Pane.
    const KEY_CONTEXT: &str = "DevelopmentPane";
    const WINDOW_WIDTH: f32 = 760.0;
    const WINDOW_HEIGHT: f32 = 480.0;
    /// How often the window reads the connection's status. Reading takes one uncontended lock.
    pub(super) const STATUS_INTERVAL: Duration = Duration::from_millis(200);

    pub(super) const END_SESSION_SELECTOR: &str = "development-pane-end-session";
    pub(super) const RETRY_SELECTOR: &str = "development-pane-retry";
    pub(super) const NOTICE_SELECTOR: &str = "development-pane-notice";
    pub(super) const TERMINAL_TITLE_SELECTOR: &str = "development-pane-terminal-title";
    pub(super) const TERMINAL_SELECTOR: &str = "development-pane-terminal";
    pub(super) const TAKEOVER_SELECTOR: &str = "development-pane-takeover";
    pub(super) const TAKE_OVER_SELECTOR: &str = "development-pane-take-over";
    pub(super) const TAKE_OVER_STATUS_SELECTOR: &str = "development-pane-take-over-status";
    const CLOSE_WINDOW_SELECTOR: &str = "development-pane-close-window";
    const CONFIRM_SELECTOR: &str = "development-pane-end-session-confirm";
    const CANCEL_SELECTOR: &str = "development-pane-end-session-cancel";

    const TAKEOVER_TITLE: &str = "Controlled by Another Client";
    const TAKEOVER_DETAIL: &str = "Take Over to type and resize here. The shell keeps running.";
    const TAKEOVER_FAILED_DETAIL: &str = "Take Over is available once SpaceTerm connects again.";
    const SESSION_ENDED_TITLE: &str = "Session Ended";
    const SESSION_ENDED_DETAIL: &str =
        "This Terminal Session is no longer available on the server.";
    const TAKING_OVER: &str = "Taking Over…";
    const TAKE_OVER_UNCONFIRMED: &str = "Take Over was not confirmed.";

    /// What the window needs from the server connection behind its Pane.
    pub(crate) trait DevelopmentPaneConnection {
        fn status(&self) -> DevelopmentPaneStatus;
        /// Identifies the connection that last supplied an authoritative screen. It changes when a
        /// new connection reaches the Session and when another client takes control, and it
        /// outlives any status the window misses.
        fn connection_generation(&self) -> u64;
        /// Asks for a reconnection attempt now instead of at the next automatic one.
        fn retry(&self);
        /// Asks the server to make this client the Controlling Client. From Takeover, the status
        /// reads TakingOver once this returns.
        fn take_over(&self);
        /// Asks the server to end the Terminal Session.
        fn end_session(&self);
    }

    impl DevelopmentPaneConnection for DevelopmentPaneState {
        fn status(&self) -> DevelopmentPaneStatus {
            DevelopmentPaneState::status(self)
        }

        fn connection_generation(&self) -> u64 {
            DevelopmentPaneState::connection_generation(self)
        }

        fn retry(&self) {
            DevelopmentPaneState::retry(self);
        }

        fn take_over(&self) {
            DevelopmentPaneState::take_over(self);
        }

        fn end_session(&self) {
            DevelopmentPaneState::close(self);
        }
    }

    /// Application capabilities the window composes its Pane from.
    pub(crate) struct DevelopmentPaneComposition {
        panes: PaneConstruction,
        home_directory: PathBuf,
        local_filesystem: LocalFilesystemAuthority,
        window_movement: Rc<dyn WindowMovementFactory>,
    }

    impl DevelopmentPaneComposition {
        /// The Pane takes the application's input and native lifecycle adapters. Its terminal
        /// text publishes no accessibility content until server-backed accessibility exists.
        pub(crate) fn new(
            key_input: Rc<dyn TerminalKeyInputAdapterFactory>,
            native_services: NativeServiceAdapters,
            lifecycle: PaneLifecycleDependencies,
            home_directory: PathBuf,
            local_filesystem: LocalFilesystemAuthority,
            window_movement: Rc<dyn WindowMovementFactory>,
        ) -> Self {
            Self {
                panes: PaneConstruction::new(
                    key_input,
                    Rc::new(UnavailableTerminalAccessibility),
                    native_services,
                    lifecycle,
                ),
                home_directory,
                local_filesystem,
                window_movement,
            }
        }
    }

    impl Global for DevelopmentPaneComposition {}

    /// The one open Development Pane window, so a second request activates it.
    struct OpenDevelopmentPane(WindowHandle<DevelopmentPaneWindow>);
    impl Global for OpenDevelopmentPane {}

    /// The open window. Membership, not a root-view read, decides this: the root view is leased
    /// while an action dispatches inside the window.
    fn open_window(cx: &App) -> Option<WindowHandle<DevelopmentPaneWindow>> {
        let handle = cx.try_global::<OpenDevelopmentPane>()?.0;
        cx.windows()
            .iter()
            .any(|window| window.window_id() == handle.window_id())
            .then_some(handle)
    }

    /// Opens the Development Pane, or activates it when it is already open. Opening asks the
    /// server to resume its Development Pane's Terminal Session, or to start one once the Pane has
    /// measured its size. It never sets up or starts the server.
    pub(crate) fn open_or_activate(cx: &mut App) {
        if !super::offered() {
            return;
        }
        if let Some(existing) = open_window(cx) {
            cx.defer(move |cx| {
                let _ = existing.update(cx, |_, window, _| window.activate_window());
            });
            return;
        }
        if !cx.has_global::<appearance_runtime::AppearanceRuntime>() {
            eprintln!("The Development Pane is unavailable because appearance is not installed");
            return;
        }
        let Some(composition) = cx.try_global::<DevelopmentPaneComposition>() else {
            eprintln!("The Development Pane is unavailable because SpaceTerm did not compose it");
            return;
        };
        let Ok(server) = LocalServer::for_identity(ApplicationIdentity::current().directory_name())
        else {
            eprintln!("The Development Pane is unavailable because the local server is unresolved");
            return;
        };
        let Ok(home) = composition
            .local_filesystem
            .validate_directory(&composition.home_directory)
        else {
            eprintln!("The Development Pane is unavailable because the home directory is invalid");
            return;
        };
        let (factory, state) = DevelopmentServerSessionFactory::new(server);
        let session_factory = WorkspaceTerminalSessionFactory::new_server(Rc::new(factory), home);
        let panes = composition.panes.clone();
        let window_drag = composition.window_movement.create();
        let opened = cx.open_window(window_options(cx), |window, cx| {
            cx.new(|cx| {
                DevelopmentPaneWindow::new(session_factory, state, &panes, window_drag, window, cx)
            })
        });
        match opened {
            Ok(handle) => {
                cx.set_global(OpenDevelopmentPane(handle));
                cx.activate(true);
            }
            Err(_) => eprintln!("failed to open the Development Pane window"),
        }
    }

    fn window_options(cx: &App) -> WindowOptions {
        let titlebar_height = super::super::appearance::chrome(cx).top_height();
        let traffic_light_position = cx
            .try_global::<crate::platform::window_frame::WindowFrameGeometry>()
            .and_then(|geometry| geometry.sidebar_window_traffic_light_position(titlebar_height));
        cx.global::<crate::platform::window_chrome::WindowChrome>()
            .options(
                crate::platform::window_chrome::WindowRole::SidebarWindow,
                WindowOptions {
                    window_background: appearance_runtime::window_background(cx),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
                        cx,
                    ))),
                    window_min_size: Some(size(px(480.0), px(260.0))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(WINDOW_TITLE.into()),
                        // The native title names the window for the Window menu and assistive
                        // technology; the client surface draws its visible title.
                        appears_transparent: true,
                        traffic_light_position,
                    }),
                    kind: WindowKind::Normal,
                    is_movable: true,
                    is_resizable: true,
                    is_minimizable: true,
                    tabbing_identifier: None,
                    ..WindowOptions::default()
                },
                cx,
            )
    }

    /// The window's single Pane, its server connection, and the End Session request.
    pub(crate) struct DevelopmentPaneWindow {
        terminal: Entity<TerminalPane>,
        connection: Arc<dyn DevelopmentPaneConnection>,
        status: DevelopmentPaneStatus,
        /// The connection generation the window last read.
        generation: u64,
        /// The connection generation that carried a confirmed End Session request still awaiting
        /// the server's answer. A connection drops an unanswered request and never resends it.
        end_request: Option<u64>,
        /// The open End Session confirmation. It answers only for the connection it opened on.
        confirmation: Option<ModalPresentationHandle>,
        /// The Takeover Screen replaces the Pane. It stays through interruptions until this client
        /// controls the Pane again and the server sent a fresh screen.
        takeover_shown: bool,
        /// The last Take Over ended without a grant reaching this client, so the Takeover Screen
        /// says so. The server may still have committed it before the reply was lost.
        take_over_unconfirmed: bool,
        focus_handle: FocusHandle,
        window_appearance: WindowAppearanceOwner,
        window_traffic_lights: WindowTrafficLightOwner,
        window_movement: WindowMovement,
        _status_task: Task<()>,
    }

    impl DevelopmentPaneWindow {
        pub(super) fn new(
            session_factory: WorkspaceTerminalSessionFactory,
            connection: Arc<dyn DevelopmentPaneConnection>,
            panes: &PaneConstruction,
            window_drag: Rc<dyn OperatingSystemWindowDragPlatform>,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> Self {
            window.set_window_title(WINDOW_TITLE);
            let mut window_appearance = WindowAppearanceOwner::default();
            window_appearance.apply(window, cx);
            let mut window_traffic_lights = WindowTrafficLightOwner::sidebar_window();
            window_traffic_lights.apply(window, cx);
            cx.observe_global_in::<appearance_runtime::InstalledAppearance>(
                window,
                |pane_window, window, cx| {
                    pane_window.window_appearance.apply(window, cx);
                    pane_window.window_traffic_lights.apply(window, cx);
                    cx.notify();
                },
            )
            .detach();
            cx.observe_window_activation(window, |_, _, cx| cx.notify())
                .detach();
            let prepared_launch = session_factory
                .prepare_child_launch()
                .expect("a server-backed launch reserves no channel");
            let terminal = cx.new(|cx| panes.create(session_factory, prepared_launch, window, cx));
            cx.subscribe_in(
                &terminal,
                window,
                |_, _, event: &TerminalPaneEvent, window, cx| match event {
                    // The server ended the Terminal Session, so the Pane closes as a Workspace
                    // Pane does when its shell exits.
                    TerminalPaneEvent::Exited => window.remove_window(),
                    TerminalPaneEvent::TitleChanged(_) => cx.notify(),
                    _ => {}
                },
            )
            .detach();
            // Closing the window releases this entity. Closing the Pane then drops its Terminal
            // Session handle, which detaches from the server without ending the shell.
            cx.on_release(|pane_window, cx| {
                pane_window
                    .terminal
                    .update(cx, |terminal, _| terminal.close());
            })
            .detach();
            let focus_handle = cx.focus_handle();
            let window_movement =
                WindowMovement::new(window_drag, focus_handle.clone(), WINDOW_TITLE);
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
            let status_task = cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(STATUS_INTERVAL).await;
                    if this
                        .update_in(cx, |pane_window, window, cx| {
                            pane_window.sync_status(window, cx);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let mut pane_window = Self {
                terminal,
                status: connection.status(),
                generation: connection.connection_generation(),
                connection,
                end_request: None,
                confirmation: None,
                takeover_shown: false,
                take_over_unconfirmed: false,
                focus_handle,
                window_appearance,
                window_traffic_lights,
                window_movement,
                _status_task: status_task,
            };
            pane_window.apply_status(window, cx);
            pane_window
        }

        /// Reads the connection's status and presents any change.
        fn sync_status(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
            // Status first, because a connection publishes its generation before it reports
            // Connected.
            let status = self.connection.status();
            let generation = self.connection.connection_generation();
            // A later connection reached the Session, so it still runs, even when the window
            // missed the interruption between two reads.
            let superseded = self.end_request.is_some_and(|sent| sent != generation);
            let generation_changed = generation != self.generation;
            if status == self.status && !superseded && !generation_changed {
                return;
            }
            // Take Over that returns to the Takeover Screen on the same connection generation is
            // unconfirmed. The server may have refused it, or committed it and lost the reply, and
            // another client may have taken control since. Reconnecting may still find the grant.
            // The window claims none of these. A new generation means this client held control
            // and another client took it since.
            if self.status == DevelopmentPaneStatus::TakingOver
                && matches!(
                    status,
                    DevelopmentPaneStatus::Takeover
                        | DevelopmentPaneStatus::TakeoverReconnecting
                        | DevelopmentPaneStatus::TakeoverFailed(_)
                )
            {
                self.take_over_unconfirmed = !generation_changed;
            }
            self.status = status;
            self.generation = generation;
            if superseded {
                self.end_request = None;
            }
            if generation_changed {
                // An End Session approval answers only for the connection it was given on.
                self.dismiss_confirmation(window, cx);
            }
            self.apply_status(window, cx);
        }

        fn apply_status(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
            match self.status {
                DevelopmentPaneStatus::Ended => {
                    window.remove_window();
                    return;
                }
                // A refused End Session leaves the shell running, so the action is offered again.
                DevelopmentPaneStatus::Failed(_) => self.end_request = None,
                DevelopmentPaneStatus::Connected => {
                    self.takeover_shown = false;
                    self.take_over_unconfirmed = false;
                }
                _ => {}
            }
            let interrupted = self.status != DevelopmentPaneStatus::Connected;
            if interrupted {
                self.dismiss_confirmation(window, cx);
            }
            self.terminal.update(cx, |terminal, cx| {
                terminal.set_server_connection_interrupted(interrupted, window, cx);
            });
            // Another client controls the Pane, so this window forgets what it showed. Neither an
            // interruption nor a Take Over in progress reveals that screen again.
            let entering_takeover = !self.takeover_shown
                && matches!(
                    self.status,
                    DevelopmentPaneStatus::Takeover
                        | DevelopmentPaneStatus::TakeoverReconnecting
                        | DevelopmentPaneStatus::TakeoverFailed(_)
                        | DevelopmentPaneStatus::TakingOver
                );
            if entering_takeover
                || matches!(
                    self.status,
                    DevelopmentPaneStatus::Takeover
                        | DevelopmentPaneStatus::TakeoverReconnecting
                        | DevelopmentPaneStatus::TakeoverFailed(_)
                )
            {
                self.terminal
                    .update(cx, |terminal, cx| terminal.clear_server_presentation(cx));
            }
            if entering_takeover {
                self.takeover_shown = true;
                // Focus rests on the window, so typing meant for the shell reaches nothing and
                // never takes control back by accident. Tab reaches Take Over.
                self.focus_content(window, cx);
            }
            // Close Window replaces Take Over, which may hold focus. The window keeps focus, so
            // typing never closes it and Tab reaches Close Window.
            if self.session_ended() {
                self.focus_handle.focus(window, cx);
            }
            // Retry held focus during the interruption and leaves with it, so typing returns to
            // the Pane once input resumes.
            if !interrupted && !spaceterm_ui::window_modal_is_open(window, cx) {
                self.focus_content(window, cx);
            }
            cx.notify();
        }

        /// Moves focus to what the window presents: the Pane, or the window itself while the
        /// Takeover Screen replaces the Pane.
        fn focus_content(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
            if self.takeover_shown {
                self.focus_handle.focus(window, cx);
            } else {
                self.terminal
                    .update(cx, |terminal, cx| terminal.focus(window, cx));
            }
        }

        fn retry(&mut self, cx: &mut gpui::Context<Self>) {
            if self.session_ended() {
                return;
            }
            if matches!(
                self.status,
                DevelopmentPaneStatus::Reconnecting
                    | DevelopmentPaneStatus::TakeoverReconnecting
                    | DevelopmentPaneStatus::TakeoverFailed(_)
                    | DevelopmentPaneStatus::Failed(_)
            ) {
                self.connection.retry();
                cx.notify();
            }
        }

        fn take_over(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
            if self.status != DevelopmentPaneStatus::Takeover {
                return;
            }
            self.take_over_unconfirmed = false;
            // Take Over waits disabled while the server answers, and a disabled control gives up
            // focus. The window keeps it instead, so the keyboard stays on the Takeover Screen.
            self.focus_handle.focus(window, cx);
            self.connection.take_over();
            self.sync_status(window, cx);
            cx.notify();
        }

        /// The Session ended while another client controlled it. Nothing remains to take over,
        /// retry, or end, so the window offers only to close.
        fn session_ended(&self) -> bool {
            self.status == DevelopmentPaneStatus::TakeoverFailed(ServerError::SessionUnavailable)
        }

        fn can_end_session(&self) -> bool {
            self.status == DevelopmentPaneStatus::Connected
                && self.end_request.is_none()
                && self.confirmation.is_none()
        }

        /// Close Pane in this window ends the shell, as it does for a Workspace Pane. Closing the
        /// window leaves it running.
        /// Once the Session ended elsewhere, Close Pane closes the window.
        fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut gpui::Context<Self>) {
            if self.session_ended() {
                window.remove_window();
                return;
            }
            self.confirm_end_session(window, cx);
        }

        fn confirm_end_session(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
            if !self.can_end_session() {
                return;
            }
            let Some(handle) = window.window_handle().downcast::<Self>() else {
                return;
            };
            let generation = self.generation;
            let result = Alert::new(
                ModalId::new("development-pane-end-session"),
                "End Session?",
                "End Session?",
                "The shell and its running commands will stop. Close the window instead to keep \
                 them running on the server.",
                vec![
                    ModalAction::new(
                        true,
                        "End Session",
                        ModalActionRole::Affirmative,
                        CONFIRM_SELECTOR,
                    )
                    .with_intent(ModalActionIntent::Destructive)
                    .with_emphasis(ModalActionEmphasis::Prominent),
                    ModalAction::new(false, "Cancel", ModalActionRole::Cancel, CANCEL_SELECTOR),
                ],
            )
            .intent(AlertIntent::Critical)
            .present(window, cx, move |outcome, cx| {
                let confirmed = matches!(
                    outcome,
                    AlertOutcome::Activated {
                        action_id: true,
                        ..
                    }
                );
                let _ = handle.update(cx, |pane_window, window, cx| {
                    pane_window.settle_end_session(confirmed, generation, window, cx);
                });
            });
            match result {
                Ok(confirmation) => {
                    self.confirmation = Some(confirmation);
                    cx.notify();
                }
                Err(error) => eprintln!("failed to present the End Session confirmation: {error}"),
            }
        }

        /// Closes an open End Session confirmation without ending the Session.
        fn dismiss_confirmation(&mut self, window: &Window, cx: &mut gpui::Context<Self>) {
            if let Some(confirmation) = self.confirmation.take() {
                let _ = confirmation.dismiss(window, cx);
            }
        }

        fn settle_end_session(
            &mut self,
            confirmed: bool,
            generation: u64,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) {
            self.confirmation = None;
            // The connection may have changed while the confirmation was open. Approval given to
            // an earlier connection, including one before a takeover, never applies.
            if confirmed
                && self.status == DevelopmentPaneStatus::Connected
                && self.connection.connection_generation() == generation
            {
                // Recorded before sending, so a reconnection that races the request settles it
                // rather than leaving it awaiting an answer that never comes.
                self.end_request = Some(generation);
                self.connection.end_session();
            }
            self.focus_content(window, cx);
            cx.notify();
        }

        /// The header's progress, read by assistive technology as one status. An interruption
        /// reports through its notice instead.
        fn progress_label(&self) -> Option<&'static str> {
            if self.end_request.is_some() && self.status == DevelopmentPaneStatus::Connected {
                return Some("Ending Session…");
            }
            (self.status == DevelopmentPaneStatus::Connecting).then_some("Connecting…")
        }

        fn render_header(&self, window: &Window, cx: &mut gpui::Context<Self>) -> AnyElement {
            let appearance = super::super::appearance::chrome(cx);
            let height = appearance.top_height();
            let leading_clearance = cx
                .try_global::<crate::platform::window_frame::WindowFrameGeometry>()
                .and_then(|geometry| geometry.sidebar_window_titlebar_clearance())
                .filter(|_| !window.is_fullscreen())
                .unwrap_or(px(0.0));
            // The Pane falls back to the window's own name until the shell reports a title, and
            // the window already shows that name. The Takeover Screen shows no title, because the
            // shell's title belongs to the Controlling Client's view.
            let title = Some(self.terminal.read(cx).title()).filter(|title| {
                !self.takeover_shown && !title.trim().is_empty() && title.as_ref() != WINDOW_TITLE
            });
            let status = self.progress_label().map(|label| {
                let indicator = ProgressRing::new(
                    "development-pane-progress",
                    label,
                    ProgressState::Indeterminate,
                )
                .size(ProgressSize::Compact);
                div()
                    .id("development-pane-status")
                    .role(gpui::accesskit::Role::Status)
                    .aria_label(label)
                    .debug_selector(|| "development-pane-status".to_owned())
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(appearance.spacing(6.0))
                    .child(indicator)
                    .child(
                        div()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(appearance.colors.text_secondary))
                            .child(label),
                    )
            });
            let heading = div()
                .h(height)
                .w_full()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(10.0))
                .pl(leading_clearance + appearance.spacing(12.0))
                .child(
                    div()
                        .flex_none()
                        .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                        .text_color(gpui_color(appearance.colors.text))
                        .child(WINDOW_TITLE),
                )
                .children(title.map(|title| {
                    div()
                        .debug_selector(|| TERMINAL_TITLE_SELECTOR.to_owned())
                        .min_w_0()
                        .truncate()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(appearance.colors.text_secondary))
                        .child(title)
                }))
                .children(status);
            let region = self.window_movement.region(
                "development-pane-drag-region".to_owned(),
                heading,
                Edges::default(),
                window,
            );
            let owner = cx.weak_entity();
            let end_session = Button::new(END_SESSION_SELECTOR, "End Session")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Compact)
                .disabled(!self.can_end_session())
                .tab_stop(true)
                .accessibility_description(
                    "Stops the shell and its running commands in the server.",
                )
                .debug_selector(END_SESSION_SELECTOR)
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |pane_window, cx| {
                        pane_window.confirm_end_session(window, cx);
                    });
                });
            let client_controls =
                ClientWindowControls::width(WindowControlSide::Right, window, cx) > px(0.0);
            // Closing the window detaches; the server keeps the shell running.
            let close: spaceterm_ui::WindowCloseHandler =
                Rc::new(|window, _| window.remove_window());
            div()
                .debug_selector(|| "development-pane-header".to_owned())
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .w_full()
                .h(height)
                .bg(gpui_color(appearance.colors.title_bar_background))
                .child(div().flex_1().min_w_0().child(region))
                .child(
                    div()
                        .flex_none()
                        .pl(appearance.spacing(8.0))
                        .pr(appearance.spacing(if client_controls { 8.0 } else { 12.0 }))
                        .child(end_session),
                )
                .when(client_controls, |header| {
                    header.child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .pr(px(spaceterm_ui::DesktopWindowStyle::current(cx)
                                .control_metrics()
                                .edge_margin))
                            .child(
                                ClientWindowControls::new(close).surface_color(gpui_color(
                                    appearance.colors.title_bar_background,
                                )),
                            ),
                    )
                })
                .into_any_element()
        }

        /// The interruption notice over the frozen screen, with its Retry action.
        fn render_notice(&self, cx: &mut gpui::Context<Self>) -> Option<AnyElement> {
            let (title, detail, intent) = match self.status {
                DevelopmentPaneStatus::Reconnecting
                | DevelopmentPaneStatus::TakeoverReconnecting => (
                    SharedString::new_static("Reconnecting…"),
                    SharedString::new_static(if self.takeover_shown {
                        "Take Over is available once SpaceTerm reconnects. The shell keeps \
                         running on the server."
                    } else if self.end_request.is_some() {
                        "End Session was interrupted and is not sent again. Typing is paused and \
                         is not sent."
                    } else {
                        "Typing is paused and is not sent. The shell keeps running on the server."
                    }),
                    StatusIntent::Warning,
                ),
                DevelopmentPaneStatus::TakeoverFailed(_) if self.session_ended() => return None,
                DevelopmentPaneStatus::Failed(error)
                | DevelopmentPaneStatus::TakeoverFailed(error) => (
                    SharedString::new_static("Not Connected"),
                    SharedString::from(error.to_string()),
                    StatusIntent::Error,
                ),
                _ => return None,
            };
            let appearance = super::super::appearance::shared_chrome(cx);
            let shell = spaceterm_ui::floating_surface_theme(cx).shell(FloatingRole::Notice);
            let floating_colors = &appearance.floating_colors;
            let intent_color = intent.color(&appearance.floating_control_colors);
            let icon = intent.glyph(appearance.capabilities.differentiate_without_color);
            let icon_size = appearance.icons.metrics(IconRole::Status).glyph_size;
            let owner = cx.weak_entity();
            let description = format!("{title} {detail}");
            Some(
                shell
                    .mount(
                        div()
                            .id(NOTICE_SELECTOR)
                            .role(gpui::accesskit::Role::Status)
                            .aria_label(title.clone())
                            .aria_description(description)
                            .debug_selector(|| NOTICE_SELECTOR.to_owned())
                            .absolute()
                            .right(appearance.spacing(12.0))
                            .bottom(appearance.spacing(12.0))
                            .w(appearance.spacing(300.0))
                            .max_w(gpui::relative(0.94))
                            .text_color(gpui_color(floating_colors.text))
                            .chrome_text(appearance.typography.style(TextRole::Body))
                            .flex()
                            .flex_row()
                            .child(
                                div()
                                    .w(shell.hairline() * 2.0)
                                    .flex_shrink_0()
                                    .bg(gpui_color(intent_color)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .px(appearance.spacing(10.0))
                                    .py(appearance.spacing(8.0))
                                    .flex()
                                    .flex_col()
                                    .gap(appearance.spacing(4.0))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(appearance.spacing(8.0))
                                            .child(Icon::new(
                                                icon,
                                                icon_size,
                                                gpui_color(intent_color),
                                            ))
                                            .child(
                                                div()
                                                    .chrome_text(
                                                        appearance
                                                            .typography
                                                            .style(TextRole::BodyEmphasis),
                                                    )
                                                    .child(title),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .whitespace_normal()
                                            .chrome_text(
                                                appearance.typography.style(TextRole::Secondary),
                                            )
                                            .text_color(gpui_color(floating_colors.text_muted))
                                            .child(detail),
                                    )
                                    .child(
                                        div().child(
                                            Button::new(RETRY_SELECTOR, "Retry")
                                                .variant(ButtonVariant::Link)
                                                .size(ButtonSize::Compact)
                                                .tab_stop(true)
                                                .debug_selector(RETRY_SELECTOR)
                                                .on_activate(move |_, _, cx| {
                                                    let _ = owner.update(cx, |pane_window, cx| {
                                                        pane_window.retry(cx);
                                                    });
                                                }),
                                        ),
                                    ),
                            ),
                    )
                    .into_any_element(),
            )
        }

        fn render_surface(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> AnyElement {
            let appearance = super::super::appearance::shared_chrome(cx);
            let header = self.render_header(window, cx);
            let notice = self.render_notice(cx);
            // The Takeover Screen replaces the Pane outright, so nothing of the former screen
            // paints or reaches assistive technology.
            let content = if self.takeover_shown {
                self.render_takeover_screen(cx)
            } else {
                self.render_terminal(&appearance)
            };
            let pane = div()
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_hidden()
                .child(content)
                .children(notice);
            let content = with_tab_traversal(
                div()
                    .id("development-pane-window")
                    .role(gpui::accesskit::Role::Group)
                    .aria_label(WINDOW_TITLE)
                    .debug_selector(|| "development-pane-window".to_owned())
                    .key_context(KEY_CONTEXT)
                    .track_focus(&self.focus_handle)
                    .on_action(cx.listener(Self::close_pane))
                    .size_full()
                    .flex()
                    .flex_col()
                    .bg(gpui_color(appearance.colors.title_bar_background))
                    .child(header)
                    .child(pane),
            );
            ModalLayer::new(super::super::window_shell::render(content, window, cx))
                .into_any_element()
        }
    }

    impl DevelopmentPaneWindow {
        fn render_terminal(&self, appearance: &Arc<ChromeAppearance>) -> AnyElement {
            let surface_terminal = self.terminal.clone();
            let surface_appearance = Arc::clone(appearance);
            div()
                .id(TERMINAL_SELECTOR)
                .debug_selector(|| TERMINAL_SELECTOR.to_owned())
                .absolute()
                .inset_0()
                .child(
                    gpui::canvas(
                        |_, _, _| (),
                        move |bounds, (), window, cx| {
                            // Read the surface at paint time because the terminal chooses its
                            // presentation during prepaint.
                            let color = surface_appearance
                                .pane_surface(surface_terminal.read(cx).surface_background());
                            window.paint_quad(gpui::fill(bounds, gpui_color(color)));
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
                .child(self.terminal.clone())
                .into_any_element()
        }

        /// The Takeover Screen: another client controls the Pane, and Take Over moves control
        /// here. If the Session ended meanwhile, the screen says so and offers only to close.
        fn render_takeover_screen(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
            let appearance = super::super::appearance::shared_chrome(cx);
            // The configured terminal background, never one the shell chose.
            let surface =
                appearance.pane_surface(appearance_runtime::current(cx).terminal.colors.background);
            let session_ended = self.session_ended();
            let (title, detail) = match self.status {
                _ if session_ended => (SESSION_ENDED_TITLE, SESSION_ENDED_DETAIL),
                // SpaceTerm stopped reconnecting, so the screen claims nothing about the shell.
                DevelopmentPaneStatus::TakeoverFailed(_) => {
                    (TAKEOVER_TITLE, TAKEOVER_FAILED_DETAIL)
                }
                _ => (TAKEOVER_TITLE, TAKEOVER_DETAIL),
            };
            let taking_over = self.status == DevelopmentPaneStatus::TakingOver;
            let status_text_style = appearance.typography.style(TextRole::Secondary);
            let status = if taking_over {
                Some(
                    div()
                        .id(TAKE_OVER_STATUS_SELECTOR)
                        .role(gpui::accesskit::Role::Status)
                        .aria_label(TAKING_OVER)
                        .debug_selector(|| TAKE_OVER_STATUS_SELECTOR.to_owned())
                        .flex()
                        .items_center()
                        .gap(appearance.spacing(6.0))
                        .child(
                            ProgressRing::new(
                                "development-pane-take-over-progress",
                                TAKING_OVER,
                                ProgressState::Indeterminate,
                            )
                            .size(ProgressSize::Compact),
                        )
                        .child(
                            div()
                                .chrome_text(status_text_style)
                                .text_color(gpui_color(appearance.colors.text_secondary))
                                .child(TAKING_OVER),
                        )
                        .into_any_element(),
                )
            } else if self.take_over_unconfirmed && !session_ended {
                Some(
                    div()
                        .id(TAKE_OVER_STATUS_SELECTOR)
                        .role(gpui::accesskit::Role::Status)
                        .aria_label(TAKE_OVER_UNCONFIRMED)
                        .debug_selector(|| TAKE_OVER_STATUS_SELECTOR.to_owned())
                        .chrome_text(status_text_style)
                        .text_color(gpui_color(appearance.colors.text_secondary))
                        .child(TAKE_OVER_UNCONFIRMED)
                        .into_any_element(),
                )
            } else {
                None
            };
            let owner = cx.weak_entity();
            let action = if session_ended {
                Button::new(CLOSE_WINDOW_SELECTOR, "Close Window")
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Regular)
                    .tab_stop(true)
                    .accessibility_description("Closes this window. The Session already ended.")
                    .debug_selector(CLOSE_WINDOW_SELECTOR)
                    .on_activate(|_, window, _| window.remove_window())
            } else {
                Button::new(TAKE_OVER_SELECTOR, "Take Over")
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Regular)
                    .disabled(self.status != DevelopmentPaneStatus::Takeover)
                    .tab_stop(true)
                    .accessibility_description("Moves typing and the Pane's size to this window.")
                    .debug_selector(TAKE_OVER_SELECTOR)
                    .on_activate(move |_, window, cx| {
                        let _ = owner.update(cx, |pane_window, cx| {
                            pane_window.take_over(window, cx);
                        });
                    })
            };
            div()
                .id(TAKEOVER_SELECTOR)
                .role(gpui::accesskit::Role::Group)
                .aria_label(title)
                .aria_description(detail)
                .debug_selector(|| TAKEOVER_SELECTOR.to_owned())
                .absolute()
                .inset_0()
                .bg(gpui_color(surface))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(appearance.spacing(12.0))
                .px(appearance.spacing(24.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(appearance.spacing(4.0))
                        .max_w(appearance.spacing(360.0))
                        .text_center()
                        .child(
                            div()
                                .chrome_text(appearance.typography.style(TextRole::Section))
                                .text_color(gpui_color(appearance.colors.text))
                                .child(title),
                        )
                        .child(
                            div()
                                .id("development-pane-takeover-detail")
                                // Assistive technology announces the end, which happens without
                                // any action here.
                                .when(session_ended, |detail_text| {
                                    detail_text
                                        .role(gpui::accesskit::Role::Status)
                                        .aria_label(detail)
                                })
                                .whitespace_normal()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(appearance.colors.text_secondary))
                                .child(detail),
                        ),
                )
                .child(action)
                // The slot keeps its height, so Take Over stays put while its status changes.
                .child(
                    div()
                        .h(appearance.spacing(20.0))
                        .flex()
                        .items_center()
                        .children(status),
                )
                .into_any_element()
        }
    }

    /// Plain Tab the Pane did not take moves focus, so the window's actions stay reachable from
    /// the keyboard while terminal input is paused.
    fn with_tab_traversal(surface: Stateful<gpui::Div>) -> Stateful<gpui::Div> {
        surface.on_key_down(|event: &gpui::KeyDownEvent, window, cx| {
            let modifiers = event.keystroke.modifiers;
            if event.keystroke.key != "tab"
                || modifiers.control
                || modifiers.alt
                || modifiers.platform
                || modifiers.function
            {
                return;
            }
            if modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            window.prevent_default();
            cx.stop_propagation();
        })
    }

    impl Render for DevelopmentPaneWindow {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let activity = super::super::appearance::window_activity(window);
            activity.mount(activity.with_scope(|| self.render_surface(window, cx)))
        }
    }

    /// Publishes the Pane as one labeled terminal without text until server-backed Terminal
    /// Sessions supply accessible content.
    struct UnavailableTerminalAccessibility;

    impl TerminalAccessibilityAdapterFactory for UnavailableTerminalAccessibility {
        fn create(
            &self,
            _: &Window,
            _: TerminalAccessibilityModel,
            _: &crate::appearance::ResolvedFontDescriptor,
            _: Pixels,
        ) -> Box<dyn TerminalAccessibilityAdapter> {
            Box::new(UnavailableTerminalAccessibility)
        }
    }

    impl TerminalAccessibilityAdapter for UnavailableTerminalAccessibility {
        fn set_hierarchy(&mut self, _: bool) {}

        fn update(&mut self, _: TerminalAccessibilityUpdate<'_>) -> AccessibilityNotifications {
            AccessibilityNotifications::default()
        }

        fn decorate(&self, pane: Stateful<gpui::Div>) -> Stateful<gpui::Div> {
            pane.role(gpui::accesskit::Role::Group)
                .aria_label("Terminal")
                .aria_description(
                    "Terminal text is not yet available to assistive technologies in the \
                     Development Pane.",
                )
        }
    }

    #[cfg(test)]
    pub(super) mod testing {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

        use super::{DevelopmentPaneConnection, DevelopmentPaneStatus};

        /// A connection whose status and reconnections the test sets and whose requests it
        /// counts.
        pub(in crate::ui::development_pane) struct ScriptedConnection {
            status: Mutex<DevelopmentPaneStatus>,
            generation: AtomicU64,
            retries: AtomicUsize,
            take_overs: AtomicUsize,
            end_sessions: AtomicUsize,
        }

        impl ScriptedConnection {
            pub(in crate::ui::development_pane) fn new(status: DevelopmentPaneStatus) -> Self {
                Self {
                    status: Mutex::new(status),
                    generation: AtomicU64::new(0),
                    retries: AtomicUsize::new(0),
                    take_overs: AtomicUsize::new(0),
                    end_sessions: AtomicUsize::new(0),
                }
            }

            pub(in crate::ui::development_pane) fn set(&self, status: DevelopmentPaneStatus) {
                *self.status.lock().expect("status") = status;
            }

            /// A new connection reaches the Session and supplies its screen.
            pub(in crate::ui::development_pane) fn reconnect(&self) {
                self.generation.fetch_add(1, Ordering::SeqCst);
                self.set(DevelopmentPaneStatus::Connected);
            }

            /// Another client takes control. The connection invalidates its grant before it reports
            /// the Takeover Screen.
            pub(in crate::ui::development_pane) fn lose_control(&self) {
                self.generation.fetch_add(1, Ordering::SeqCst);
                self.set(DevelopmentPaneStatus::Takeover);
            }

            pub(in crate::ui::development_pane) fn take_overs(&self) -> usize {
                self.take_overs.load(Ordering::SeqCst)
            }

            pub(in crate::ui::development_pane) fn retries(&self) -> usize {
                self.retries.load(Ordering::SeqCst)
            }

            pub(in crate::ui::development_pane) fn end_sessions(&self) -> usize {
                self.end_sessions.load(Ordering::SeqCst)
            }
        }

        impl DevelopmentPaneConnection for ScriptedConnection {
            fn status(&self) -> DevelopmentPaneStatus {
                *self.status.lock().expect("status")
            }

            fn connection_generation(&self) -> u64 {
                self.generation.load(Ordering::SeqCst)
            }

            fn retry(&self) {
                self.retries.fetch_add(1, Ordering::SeqCst);
            }

            fn take_over(&self) {
                self.take_overs.fetch_add(1, Ordering::SeqCst);
                let mut status = self.status.lock().expect("status");
                if *status == DevelopmentPaneStatus::Takeover {
                    *status = DevelopmentPaneStatus::TakingOver;
                }
            }

            fn end_session(&self) {
                self.end_sessions.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

#[cfg(all(test, feature = "developer-tools"))]
#[path = "development_pane/tests.rs"]
mod tests;
