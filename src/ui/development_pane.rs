//! The Development Pane: a window presenting one Terminal Session that the local SpaceTerm Server
//! owns.
//!
//! The window stays outside every Workspace, so production Panes, quit confirmation, and Workspace
//! state never see it. Closing the window or quitting SpaceTerm detaches and leaves the shell
//! running. Only End Session, or Close Pane inside the window, ends the shell.

pub(crate) use window::{DevelopmentPaneComposition, is_open, open_or_activate};

mod window {
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    use gpui::prelude::*;
    use gpui::{
        AnyElement, App, Bounds, Edges, Entity, FocusHandle, Global, SharedString, Task,
        TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, px,
        size,
    };
    use spaceterm_ui::{
        Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, ClientWindowControls,
        ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ModalLayer,
        ModalPresentationHandle, ProgressRing, ProgressSize, ProgressState, WindowControlSide,
    };

    use super::super::appearance::{ChromeAppearance, gpui_color};
    use super::super::appearance_runtime::{self, WindowAppearanceOwner, WindowTrafficLightOwner};
    use super::super::chrome_typography::{ChromeTextStyleExt as _, TextRole};
    use super::super::pane_lifecycle::{PaneConstruction, PaneLifecycleDependencies};
    use super::super::server_pane::{
        self, Dormant, RESTORED_DETAIL, RESTORED_TITLE, SESSION_ENDED_DETAIL, SESSION_ENDED_TITLE,
        START_SHELL_UNCONFIRMED, STARTING_SHELL, TAKE_OVER_UNCONFIRMED, TAKEOVER_DETAIL,
        TAKEOVER_FAILED_DETAIL, TAKEOVER_TITLE, TAKING_OVER, UnavailableTerminalAccessibility,
        with_tab_traversal,
    };
    use super::super::sidebar_window::WindowMovement;
    use super::super::terminal_pane::StatusIntent;
    use super::super::{ClosePane, TerminalPane, TerminalPaneEvent};
    use crate::application_identity::ApplicationIdentity;
    use crate::local_server::{LocalServer, ServerError};
    use crate::platform::local_filesystem::LocalFilesystemAuthority;
    use crate::platform::window_movement::{
        OperatingSystemWindowDragPlatform, WindowMovementFactory,
    };
    use crate::terminal::native_services::NativeServiceAdapters;
    use crate::terminal::server_session::{
        DevelopmentPaneState, DevelopmentPaneStatus, DevelopmentServerSessionFactory,
    };
    use crate::terminal::{TerminalKeyInputAdapterFactory, WorkspaceTerminalSessionFactory};

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
    pub(super) const RESTORED_SELECTOR: &str = "development-pane-restored";
    pub(super) const START_SHELL_SELECTOR: &str = "development-pane-start-shell";
    pub(super) const START_SHELL_STATUS_SELECTOR: &str = "development-pane-start-shell-status";
    const CLOSE_WINDOW_SELECTOR: &str = "development-pane-close-window";
    const CONFIRM_SELECTOR: &str = "development-pane-end-session-confirm";
    const CANCEL_SELECTOR: &str = "development-pane-end-session-cancel";

    /// What the window needs from the server connection behind its Pane.
    pub(crate) trait DevelopmentPaneConnection {
        fn status(&self) -> DevelopmentPaneStatus;
        /// Identifies the connection that last supplied an authoritative screen. It changes when a
        /// new connection reaches the Session and when another client takes control, and it
        /// outlives any status the window misses.
        fn connection_generation(&self) -> u64;
        /// Asks for a reconnection attempt now instead of at the next automatic one.
        fn retry(&self);
        /// Asks the server to start a fresh shell in a Pane retained across a server restart. Only
        /// a person's explicit request calls this; rendering, focus, and reconnection never do.
        fn start_restored(&self);
        fn directory_server(&self) -> Option<LocalServer> {
            None
        }
        fn recovery_target(&self) -> Option<spaceterm_server::pane::directories::Target> {
            None
        }
        fn directory_progress(&self) -> (bool, Option<&'static str>) {
            (false, None)
        }
        fn repair_directory(&self, _: spaceterm_server::pane::directories::Target, _: PathBuf) {}

        /// Asks the server to make this client the Controlling Client. From Takeover, the status
        /// reads TakingOver once this returns.
        fn take_over(&self);
        /// Asks the server to end the Terminal Session on behalf of the connection generation the
        /// person approved it on. A request for any other generation ends nothing.
        fn end_session(&self, generation: u64);
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

        fn directory_server(&self) -> Option<LocalServer> {
            Self::directory_server(self)
        }
        fn recovery_target(&self) -> Option<spaceterm_server::pane::directories::Target> {
            Self::recovery_target(self)
        }
        fn directory_progress(&self) -> (bool, Option<&'static str>) {
            Self::directory_progress(self)
        }
        fn repair_directory(
            &self,
            target: spaceterm_server::pane::directories::Target,
            replacement: PathBuf,
        ) {
            Self::repair_directory(self, target, replacement);
        }
        fn start_restored(&self) {
            DevelopmentPaneState::start_restored(self);
        }

        fn take_over(&self) {
            DevelopmentPaneState::take_over(self);
        }

        fn end_session(&self, generation: u64) {
            DevelopmentPaneState::close(self, generation);
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

    /// Whether a Development Pane window is presenting a server-owned Pane right now.
    ///
    /// One device holds one authenticated connection per Pane, so the main Workspace Window's
    /// shared Workspaces and this window must never subscribe to the same server Pane at once:
    /// each subscription would rebind the Pane's lease and fence the other into reconnecting.
    pub(crate) fn is_open(cx: &App) -> bool {
        open_window(cx).is_some()
    }

    /// Opens the Development Pane, or activates it when it is already open. Opening asks the
    /// server to resume its Development Pane's Terminal Session, or to start one once the Pane has
    /// measured its size. It never sets up or starts the server.
    pub(crate) fn open_or_activate(cx: &mut App) {
        // A Workspace Window already presents this server's Panes, and one device holds one
        // connection per Pane. Opening a second presentation would fence the first.
        if super::super::shared_workspaces::adopted(cx) {
            eprintln!(
                "the Development Pane is unavailable because a Workspace Window presents this \
                 server's Workspaces"
            );
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
        let (factory, state) =
            DevelopmentServerSessionFactory::new(server, composition.local_filesystem.clone());
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
        /// The screen replacing a Pane retained across a server restart. It stays through
        /// interruptions until a fresh shell's screen arrives or another client controls the Pane.
        dormant: Option<Dormant>,
        /// Start New Shell was requested, and no answer has reached this window yet.
        start_pending: bool,
        /// The last Start New Shell ended without a shell reaching this client, so the screen says
        /// so. The server may still have started it before the reply was lost.
        start_unconfirmed: bool,
        directory_progress: (bool, Option<&'static str>),
        directory_recovery: Option<Entity<server_pane::DirectoryRecovery>>,
        announcements: server_pane::ScreenAnnouncements,
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
                dormant: None,
                start_pending: false,
                start_unconfirmed: false,
                directory_progress: (false, None),
                announcements: server_pane::ScreenAnnouncements::default(),
                directory_recovery: None,
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
            let progress = self.connection.directory_progress();
            if status == self.status
                && !superseded
                && !generation_changed
                && progress == self.directory_progress
            {
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
            // A new connection that still finds the Pane dormant never received the start, or
            // lost its answer. The window claims neither and offers Start New Shell again.
            if self.start_pending && generation_changed && status == DevelopmentPaneStatus::Restored
            {
                self.start_pending = false;
                self.start_unconfirmed = true;
            }
            self.directory_progress = progress;
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
                    self.announcements = server_pane::ScreenAnnouncements::default();
                    self.takeover_shown = false;
                    self.take_over_unconfirmed = false;
                    self.dormant = None;
                    self.start_pending = false;
                    self.start_unconfirmed = false;
                }
                _ => {}
            }
            self.apply_dormancy(window, cx);
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
                // Another client started the shell, so nothing here waits to start it.
                self.dormant = None;
                self.start_pending = false;
                self.start_unconfirmed = false;
                // Focus rests on the window, so typing meant for the shell reaches nothing and
                // never takes control back by accident. Tab reaches Take Over.
                self.focus_content(window, cx);
            }
            // Close Window replaces Take Over or Start New Shell, which may hold focus. The window
            // keeps focus, so typing never closes it and Tab reaches Close Window.
            if self.session_ended()
                || self.status == DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
            {
                self.focus_handle.focus(window, cx);
            }
            // Retry held focus during the interruption and leaves with it, so typing returns to
            // the Pane once input resumes.
            if !interrupted && !spaceterm_ui::window_modal_is_open(window, cx) {
                self.focus_content(window, cx);
            }
            cx.notify();
        }

        /// Presents a Pane retained across a server restart. Its former screen and title belong
        /// to programs that ended with the server, so they are forgotten before anything else
        /// paints, and only a person's Start New Shell starts a shell.
        fn apply_dormancy(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
            let dormant = match self.status {
                DevelopmentPaneStatus::Restored => Dormant::Restored,
                DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable) => {
                    Dormant::DirectoryUnavailable
                }
                // An interruption while starting leaves the outcome unknown. Reconnecting
                // answers it.
                DevelopmentPaneStatus::Reconnecting | DevelopmentPaneStatus::Failed(_)
                    if self.start_pending =>
                {
                    self.start_pending = false;
                    self.start_unconfirmed = true;
                    return;
                }
                _ => return,
            };
            let entering = self.dormant.is_none();
            if dormant == Dormant::DirectoryUnavailable {
                self.start_pending = false;
                self.start_unconfirmed = false;
            }
            self.dormant = Some(dormant);
            self.takeover_shown = false;
            self.take_over_unconfirmed = false;
            self.terminal
                .update(cx, |terminal, cx| terminal.clear_server_presentation(cx));
            if entering {
                // Focus rests on the window, so typing meant for the former shell reaches
                // nothing and never starts a new one by accident. Tab reaches Start New Shell.
                self.focus_handle.focus(window, cx);
            }
        }

        fn directory_unavailable(&self) -> bool {
            self.dormant == Some(Dormant::DirectoryUnavailable)
        }

        /// Starts a fresh shell in a Pane retained across a server restart, once, on a person's
        /// request.
        fn start_shell(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
            if !self.can_start_shell() {
                return;
            }
            self.start_unconfirmed = false;
            self.start_pending = true;
            // Start New Shell waits disabled while the server answers, and a disabled control
            // gives up focus. The window keeps it, so the keyboard stays on the screen.
            self.focus_handle.focus(window, cx);
            self.connection.start_restored();
            self.sync_status(window, cx);
            cx.notify();
        }

        fn choose_directory(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
            if !self.can_start_shell()
                || self
                    .directory_recovery
                    .as_ref()
                    .is_some_and(|picker| picker.read(cx).is_open())
            {
                return;
            }
            let (Some(server), Some(target)) = (
                self.connection.directory_server(),
                self.connection.recovery_target(),
            ) else {
                return;
            };
            self.focus_handle.focus(window, cx);
            let current = Arc::clone(&self.connection);
            let confirm = Arc::clone(&self.connection);
            let online = Arc::clone(&self.connection);
            let expected = target.clone();
            let restore_owner = cx.weak_entity();
            let expected_target = target.clone();
            let actions = server_pane::RecoveryActions {
                current: Rc::new(move |_| {
                    current.recovery_target().as_ref() == Some(&expected)
                        && matches!(
                            current.status(),
                            DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                                | DevelopmentPaneStatus::Reconnecting
                        )
                }),
                online: Rc::new(move || {
                    online.status()
                        == DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                }),
                confirm: Rc::new(move |replacement| {
                    confirm.repair_directory(target.clone(), replacement)
                }),
                restore_focus: Rc::new(move |window, cx| {
                    let _ = restore_owner.update(cx, |view, cx| view.focus_content(window, cx));
                }),
            };
            self.directory_recovery = Some(cx.new(|cx| {
                server_pane::DirectoryRecovery::new(server, expected_target, actions, window, cx)
            }));

            cx.notify();
        }

        fn can_start_shell(&self) -> bool {
            matches!(
                self.status,
                DevelopmentPaneStatus::Restored
                    | DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
            ) && !self.start_pending
                && !self.directory_progress.0
        }

        /// Moves focus to what the window presents: the Pane, or the window itself while a
        /// screen replaces the Pane.
        fn focus_content(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
            if self.takeover_shown || self.dormant.is_some() {
                self.focus_handle.focus(window, cx);
            } else {
                self.terminal
                    .update(cx, |terminal, cx| terminal.focus(window, cx));
            }
        }

        /// Retries the connection only. It never starts a shell in a dormant Pane, and an
        /// unavailable directory waits for directory repair rather than a retry.
        fn retry(&mut self, cx: &mut gpui::Context<Self>) {
            if self.session_ended() || self.directory_unavailable() {
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
        /// Once the Session ended elsewhere, or while no shell runs since the server restarted,
        /// Close Pane closes the window. The server keeps a dormant Pane.
        fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut gpui::Context<Self>) {
            if self.session_ended() || self.dormant.is_some() {
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
                // rather than leaving it awaiting an answer that never comes. The request carries
                // the approved generation, so a connection that arrives after this check ends
                // nothing.
                self.end_request = Some(generation);
                self.connection.end_session(generation);
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
            // Starting a shell reports on the screen that replaces the Pane.
            (self.status == DevelopmentPaneStatus::Connecting && self.dormant.is_none())
                .then_some("Connecting…")
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
            // A dormant Pane shows no title either, because the shell that set it ended.
            let title = Some(self.terminal.read(cx).title()).filter(|title| {
                !self.takeover_shown
                    && self.dormant.is_none()
                    && !title.trim().is_empty()
                    && title.as_ref() != WINDOW_TITLE
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
                    } else if self.dormant == Some(Dormant::Restored) {
                        "Start New Shell is available once SpaceTerm reconnects."
                    } else if self.dormant.is_some() {
                        "No shell is running in this Pane."
                    } else if self.end_request.is_some() {
                        "End Session was interrupted and is not sent again. Typing is paused and \
                         is not sent."
                    } else {
                        "Typing is paused and is not sent. The shell keeps running on the server."
                    }),
                    StatusIntent::Warning,
                ),
                DevelopmentPaneStatus::TakeoverFailed(_) if self.session_ended() => return None,
                // The screen replacing the Pane explains it, and nothing here can repair it.
                DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                    if self.directory_unavailable() =>
                {
                    return None;
                }
                DevelopmentPaneStatus::Failed(error)
                | DevelopmentPaneStatus::TakeoverFailed(error) => (
                    SharedString::new_static("Not Connected"),
                    SharedString::from(error.to_string()),
                    StatusIntent::Error,
                ),
                _ => return None,
            };
            let owner = cx.weak_entity();
            let retry = server_pane::retry_button(RETRY_SELECTOR.into(), move |_, cx| {
                let _ = owner.update(cx, |pane_window, cx| pane_window.retry(cx));
            });
            Some(server_pane::render_notice(
                NOTICE_SELECTOR.into(),
                title,
                detail,
                intent,
                Some(retry),
                true,
                cx,
            ))
        }

        fn render_surface(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> AnyElement {
            let appearance = super::super::appearance::shared_chrome(cx);
            let header = self.render_header(window, cx);
            let notice = self.render_notice(cx);
            // The dormant and Takeover Screens replace the Pane outright, so nothing of the former
            // screen paints or reaches assistive technology.
            let content = if let Some(dormant) = self.dormant {
                self.render_dormant_screen(dormant, cx)
            } else if self.takeover_shown {
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
                .transient(div().children(self.directory_recovery.clone()))
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
            let session_ended = self.session_ended();
            let (title, detail) = match self.status {
                _ if session_ended => (SESSION_ENDED_TITLE, SESSION_ENDED_DETAIL),
                // SpaceTerm stopped reconnecting, so the screen claims nothing about the shell.
                DevelopmentPaneStatus::TakeoverFailed(_) => {
                    (TAKEOVER_TITLE, TAKEOVER_FAILED_DETAIL)
                }
                _ => (TAKEOVER_TITLE, TAKEOVER_DETAIL),
            };
            let status = if self.status == DevelopmentPaneStatus::TakingOver {
                Some(server_pane::progress_status(
                    TAKE_OVER_STATUS_SELECTOR.into(),
                    "development-pane-take-over-progress".into(),
                    TAKING_OVER.into(),
                    cx,
                ))
            } else if self.take_over_unconfirmed && !session_ended {
                Some(server_pane::note_status(
                    TAKE_OVER_STATUS_SELECTOR.into(),
                    TAKE_OVER_UNCONFIRMED.into(),
                    cx,
                ))
            } else {
                None
            };
            let owner = cx.weak_entity();
            let action = if session_ended {
                close_window_button("Closes this window. The Session already ended.")
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
            // Assistive technology announces the end, which happens without any action here.
            server_pane::render_screen(
                TAKEOVER_SELECTOR.into(),
                server_pane::ScreenCopy::new(title.into(), detail.into()),
                session_ended,
                Some(action),
                status,
                &self.announcements,
                cx,
            )
        }

        /// The screen replacing a Pane retained across a server restart. Its programs ended, so
        /// it shows nothing of them. Start New Shell is the only way to start a shell here.
        fn render_dormant_screen(
            &self,
            dormant: Dormant,
            cx: &mut gpui::Context<Self>,
        ) -> AnyElement {
            if dormant == Dormant::DirectoryUnavailable {
                let picker_open = self
                    .directory_recovery
                    .as_ref()
                    .is_some_and(|picker| picker.read(cx).is_open());
                let starting = self.start_pending || self.directory_progress.0;
                let enabled = self.can_start_shell() && !picker_open;
                let retry = Button::new("development-pane-directory-retry", "Retry")
                    .variant(ButtonVariant::Secondary).size(ButtonSize::Regular).disabled(!enabled).tab_stop(true)
                    .accessibility_description("Checks this directory on the server again and starts a shell if it's available.")
                    .debug_selector("development-pane-directory-retry")
                    .on_activate({ let owner = cx.weak_entity(); move |_, window, cx| { let _ = owner.update(cx, |view, cx| view.start_shell(window, cx)); } });
                let target = self.connection.recovery_target();
                let choose = Button::new("development-pane-choose-directory", "Choose Directory…")
                    .variant(ButtonVariant::Primary).size(ButtonSize::Regular).disabled(!enabled || target.is_none()).tab_stop(true)
                    .accessibility_description(if target.is_some() { "Chooses another directory on the server for this Pane. The Workspace's pinned directory and Worktree don't change." } else { "This server can't change a Pane's directory. Update the server to choose another directory." })
                    .debug_selector("development-pane-choose-directory")
                    .on_activate({ let owner = cx.weak_entity(); move |_, window, cx| { let _ = owner.update(cx, |view, cx| view.choose_directory(window, cx)); } });
                let status = if starting {
                    Some(server_pane::progress_status(
                        "development-pane-directory-status".into(),
                        "development-pane-directory-progress".into(),
                        STARTING_SHELL.into(),
                        cx,
                    ))
                } else {
                    self.directory_progress.1.map(|note| {
                        server_pane::note_status(
                            "development-pane-directory-status".into(),
                            note.into(),
                            cx,
                        )
                    })
                };
                return server_pane::render_directory_screen(
                    RESTORED_SELECTOR.into(),
                    target.and_then(|target| {
                        target
                            .original
                            .to_str()
                            .map(|s| SharedString::from(s.to_owned()))
                    }),
                    vec![retry, choose],
                    status,
                    false,
                    &self.announcements,
                    cx,
                );
            }

            let status = if self.start_pending {
                Some(server_pane::progress_status(
                    START_SHELL_STATUS_SELECTOR.into(),
                    "development-pane-start-shell-progress".into(),
                    STARTING_SHELL.into(),
                    cx,
                ))
            } else if self.start_unconfirmed {
                Some(server_pane::note_status(
                    START_SHELL_STATUS_SELECTOR.into(),
                    START_SHELL_UNCONFIRMED.into(),
                    cx,
                ))
            } else {
                None
            };
            let owner = cx.weak_entity();
            let action = Button::new(START_SHELL_SELECTOR, "Start New Shell")
                .variant(ButtonVariant::Primary)
                .size(ButtonSize::Regular)
                .disabled(!self.can_start_shell())
                .tab_stop(true)
                .accessibility_description(
                    "Starts a new shell on the server in this Pane's last directory.",
                )
                .debug_selector(START_SHELL_SELECTOR)
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |pane_window, cx| {
                        pane_window.start_shell(window, cx);
                    });
                });
            // The server restart happens without any action here, so assistive technology
            // announces it.
            server_pane::render_screen(
                RESTORED_SELECTOR.into(),
                server_pane::ScreenCopy::new(RESTORED_TITLE.into(), RESTORED_DETAIL.into()),
                true,
                Some(action),
                status,
                &self.announcements,
                cx,
            )
        }
    }

    /// Close Window, offered once nothing in the window can act on the Pane.
    fn close_window_button(description: &'static str) -> Button {
        Button::new(CLOSE_WINDOW_SELECTOR, "Close Window")
            .variant(ButtonVariant::Primary)
            .size(ButtonSize::Regular)
            .tab_stop(true)
            .accessibility_description(description)
            .debug_selector(CLOSE_WINDOW_SELECTOR)
            .on_activate(|_, window, _| window.remove_window())
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

    #[cfg(test)]
    pub(super) mod testing {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

        use super::{DevelopmentPaneConnection, DevelopmentPaneStatus};

        /// A connection whose status and reconnections the test sets and whose requests it
        /// counts.
        pub(in crate::ui::development_pane) struct ScriptedConnection {
            status: Mutex<DevelopmentPaneStatus>,
            generation: AtomicU64,
            reconnect_after_next_read: AtomicBool,
            retries: AtomicUsize,
            restored_starts: AtomicUsize,
            take_overs: AtomicUsize,
            end_session_generations: Mutex<Vec<u64>>,
        }

        impl ScriptedConnection {
            pub(in crate::ui::development_pane) fn new(status: DevelopmentPaneStatus) -> Self {
                Self {
                    status: Mutex::new(status),
                    generation: AtomicU64::new(0),
                    reconnect_after_next_read: AtomicBool::new(false),
                    retries: AtomicUsize::new(0),
                    restored_starts: AtomicUsize::new(0),
                    take_overs: AtomicUsize::new(0),
                    end_session_generations: Mutex::new(Vec::new()),
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

            /// A new connection reaches the Session right after the window next reads the
            /// generation, before the window acts on what it read.
            pub(in crate::ui::development_pane) fn reconnect_after_next_read(&self) {
                self.reconnect_after_next_read.store(true, Ordering::SeqCst);
            }

            pub(in crate::ui::development_pane) fn generation(&self) -> u64 {
                self.generation.load(Ordering::SeqCst)
            }

            pub(in crate::ui::development_pane) fn take_overs(&self) -> usize {
                self.take_overs.load(Ordering::SeqCst)
            }

            pub(in crate::ui::development_pane) fn retries(&self) -> usize {
                self.retries.load(Ordering::SeqCst)
            }

            /// How many times the window asked to start a fresh shell in a dormant Pane.
            pub(in crate::ui::development_pane) fn restored_starts(&self) -> usize {
                self.restored_starts.load(Ordering::SeqCst)
            }

            pub(in crate::ui::development_pane) fn end_sessions(&self) -> usize {
                self.end_session_generations().len()
            }

            /// The connection generation each End Session request carried, in order.
            pub(in crate::ui::development_pane) fn end_session_generations(&self) -> Vec<u64> {
                self.end_session_generations
                    .lock()
                    .expect("end sessions")
                    .clone()
            }
        }

        impl DevelopmentPaneConnection for ScriptedConnection {
            fn status(&self) -> DevelopmentPaneStatus {
                *self.status.lock().expect("status")
            }

            fn connection_generation(&self) -> u64 {
                let generation = self.generation.load(Ordering::SeqCst);
                if self.reconnect_after_next_read.swap(false, Ordering::SeqCst) {
                    self.reconnect();
                }
                generation
            }

            fn retry(&self) {
                self.retries.fetch_add(1, Ordering::SeqCst);
            }

            /// Counts the request and leaves the status for the test to answer.
            fn start_restored(&self) {
                self.restored_starts.fetch_add(1, Ordering::SeqCst);
            }

            fn take_over(&self) {
                self.take_overs.fetch_add(1, Ordering::SeqCst);
                let mut status = self.status.lock().expect("status");
                if *status == DevelopmentPaneStatus::Takeover {
                    *status = DevelopmentPaneStatus::TakingOver;
                }
            }

            fn end_session(&self, generation: u64) {
                self.end_session_generations
                    .lock()
                    .expect("end sessions")
                    .push(generation);
            }
        }
    }
}

#[cfg(test)]
#[path = "development_pane/tests.rs"]
mod tests;
