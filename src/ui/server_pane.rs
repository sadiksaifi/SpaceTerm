//! Presentation shared by every Pane whose Terminal Session a SpaceTerm Server owns.
//!
//! The Development Pane window and the Panes of a shared server Workspace show the same screens,
//! interruption notices, and action statuses, so their copy and composition live here once. The
//! surfaces are deliberately plain: a screen replaces the Pane outright, so nothing of a former
//! shell's output paints or reaches assistive technology after another client takes control or the
//! server restarts.

use gpui::prelude::*;
use gpui::{AnyElement, App, Pixels, SharedString, Stateful, div};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, FloatingRole, Icon, ProgressRing, ProgressSize,
    ProgressState,
};

mod directory_recovery;
pub(crate) use directory_recovery::{DirectoryRecovery, RecoveryActions};

use super::appearance::gpui_color;
use super::appearance_runtime;
use super::chrome_icons::IconRole;
use super::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use super::terminal_pane::StatusIntent;
use crate::platform::terminal_accessibility::{
    TerminalAccessibilityAdapter, TerminalAccessibilityAdapterFactory, TerminalAccessibilityUpdate,
};
use crate::terminal::{AccessibilityNotifications, TerminalAccessibilityModel};

pub(crate) const TAKEOVER_TITLE: &str = "Controlled by Another Client";
pub(crate) const TAKEOVER_DETAIL: &str =
    "Take Over to type and resize here. The shell keeps running.";
pub(crate) const TAKEOVER_FAILED_DETAIL: &str =
    "Take Over is available once SpaceTerm connects again.";
pub(crate) const SESSION_ENDED_TITLE: &str = "Session Ended";
pub(crate) const SESSION_ENDED_DETAIL: &str =
    "This Terminal Session is no longer available on the server.";
pub(crate) const TAKING_OVER: &str = "Taking Over…";
pub(crate) const TAKE_OVER_UNCONFIRMED: &str = "Take Over was not confirmed.";
pub(crate) const RESTORED_TITLE: &str = "Server Restarted";
pub(crate) const RESTORED_DETAIL: &str = "Programs that were running in this Pane ended when the \
                                          server stopped. Start a new shell to keep working here.";
pub(crate) const DIRECTORY_UNAVAILABLE_TITLE: &str = "Directory Unavailable";
pub(crate) const DIRECTORY_UNAVAILABLE_DETAIL: &str = "SpaceTerm couldn't start a shell because this directory is missing or inaccessible on the server. Retry once it's available, or choose another directory for this Pane.";
pub(crate) const STARTING_SHELL: &str = "Starting Shell…";
pub(crate) const START_SHELL_UNCONFIRMED: &str = "Starting the shell was not confirmed.";

/// Why a Pane has no shell after the server restarted. The screen naming it replaces the Pane,
/// because the programs it showed ended with the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dormant {
    /// The Pane waits for a person to start a fresh shell.
    Restored,
    /// The server refused to start a shell, because the Pane's directory is unavailable.
    DirectoryUnavailable,
}

/// A centered screen replacing a Pane: a title, its detail, one action, and a status slot under
/// the action.
pub(crate) fn render_screen(
    selector: SharedString,
    title: SharedString,
    detail: SharedString,
    announce: bool,
    action: Option<Button>,
    status: Option<AnyElement>,
    cx: &App,
) -> AnyElement {
    render_screen_content(
        selector,
        ScreenCopy {
            title,
            detail,
            path: None,
        },
        announce,
        action.into_iter().collect(),
        status,
        cx,
    )
}

pub(crate) fn render_directory_screen(
    selector: SharedString,
    path: Option<SharedString>,
    actions: Vec<Button>,
    status: Option<AnyElement>,
    missing_worktree: bool,
    cx: &App,
) -> AnyElement {
    let detail = if missing_worktree {
        "This Tab's Worktree is missing, and SpaceTerm doesn't recreate Worktrees. Retry after restoring it with git, or choose another directory for this Pane."
    } else {
        DIRECTORY_UNAVAILABLE_DETAIL
    };
    render_screen_content(
        selector,
        ScreenCopy {
            title: DIRECTORY_UNAVAILABLE_TITLE.into(),
            detail: detail.into(),
            path,
        },
        true,
        actions,
        status,
        cx,
    )
}
struct ScreenCopy {
    title: SharedString,
    detail: SharedString,
    path: Option<SharedString>,
}
fn render_screen_content(
    selector: SharedString,
    copy: ScreenCopy,
    announce: bool,
    actions: Vec<Button>,
    status: Option<AnyElement>,
    cx: &App,
) -> AnyElement {
    let ScreenCopy {
        title,
        detail,
        path,
    } = copy;
    let appearance = super::appearance::shared_chrome(cx);
    // The configured terminal background, never one the shell chose.
    let surface =
        appearance.pane_surface(appearance_runtime::current(cx).terminal.colors.background);
    let detail_id = SharedString::from(format!("{selector}-detail"));
    let element_selector = selector.clone();
    div()
        .id(selector)
        .role(gpui::accesskit::Role::Group)
        .aria_label(title.clone())
        .aria_description(
            path.as_ref()
                .map_or_else(|| detail.to_string(), |path| format!("{path}. {detail}")),
        )
        .debug_selector(move || element_selector.to_string())
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
                .children(path.map(|path| {
                    div()
                        .whitespace_normal()
                        .chrome_text(appearance.typography.style(TextRole::Body))
                        .text_color(gpui_color(appearance.colors.text))
                        .child(path)
                }))
                .child(
                    div()
                        .id(detail_id)
                        .when(announce, |detail_text| {
                            detail_text
                                .role(gpui::accesskit::Role::Status)
                                .aria_label(detail.clone())
                        })
                        .whitespace_normal()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(appearance.colors.text_secondary))
                        .child(detail),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_center()
                .gap(appearance.spacing(8.0))
                .children(actions),
        )
        // The slot keeps its height, so the action stays put while its status changes.
        .child(
            div()
                .h(appearance.spacing(20.0))
                .flex()
                .items_center()
                .children(status),
        )
        .into_any_element()
}

/// An action's progress, read by assistive technology as one status.
pub(crate) fn progress_status(
    selector: SharedString,
    progress_id: SharedString,
    label: SharedString,
    cx: &App,
) -> AnyElement {
    let appearance = super::appearance::shared_chrome(cx);
    let element_selector = selector.clone();
    div()
        .id(selector)
        .role(gpui::accesskit::Role::Status)
        .aria_label(label.clone())
        .debug_selector(move || element_selector.to_string())
        .flex()
        .items_center()
        .gap(appearance.spacing(6.0))
        .child(
            ProgressRing::new(progress_id, label.clone(), ProgressState::Indeterminate)
                .size(ProgressSize::Compact),
        )
        .child(
            div()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(appearance.colors.text_secondary))
                .child(label),
        )
        .into_any_element()
}

/// An action's outcome as plain text, read by assistive technology as one status.
pub(crate) fn note_status(selector: SharedString, label: SharedString, cx: &App) -> AnyElement {
    let appearance = super::appearance::shared_chrome(cx);
    let element_selector = selector.clone();
    div()
        .id(selector)
        .role(gpui::accesskit::Role::Status)
        .aria_label(label.clone())
        .debug_selector(move || element_selector.to_string())
        .chrome_text(appearance.typography.style(TextRole::Secondary))
        .text_color(gpui_color(appearance.colors.text_secondary))
        .child(label)
        .into_any_element()
}

/// The interruption notice over a frozen server-backed screen, with the action that retries now.
pub(crate) fn render_notice(
    selector: SharedString,
    title: SharedString,
    detail: SharedString,
    intent: StatusIntent,
    retry: Option<Button>,
    anchored: bool,
    cx: &App,
) -> AnyElement {
    let appearance = super::appearance::shared_chrome(cx);
    let shell = spaceterm_ui::floating_surface_theme(cx).shell(FloatingRole::Notice);
    let floating_colors = &appearance.floating_colors;
    let intent_color = intent.color(&appearance.floating_control_colors);
    let icon = intent.glyph(appearance.capabilities.differentiate_without_color);
    let icon_size = appearance.icons.metrics(IconRole::Status).glyph_size;
    let description = format!("{title} {detail}");
    let element_selector = selector.clone();
    shell
        .mount(
            div()
                .id(selector)
                .role(gpui::accesskit::Role::Status)
                .aria_label(title.clone())
                .aria_description(description)
                .debug_selector(move || element_selector.to_string())
                .when(anchored, |notice| {
                    notice
                        .absolute()
                        .right(appearance.spacing(12.0))
                        .bottom(appearance.spacing(12.0))
                })
                .w(appearance.spacing(300.0))
                .max_w(gpui::relative(if anchored { 0.94 } else { 1.0 }))
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
                                .child(Icon::new(icon, icon_size, gpui_color(intent_color)))
                                .child(
                                    div()
                                        .chrome_text(
                                            appearance.typography.style(TextRole::BodyEmphasis),
                                        )
                                        .child(title),
                                ),
                        )
                        .child(
                            div()
                                .whitespace_normal()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(floating_colors.text_muted))
                                .child(detail),
                        )
                        .children(retry.map(|retry| div().child(retry))),
                ),
        )
        .into_any_element()
}

/// The standard Retry action of an interruption notice.
pub(crate) fn retry_button(
    selector: SharedString,
    on_activate: impl Fn(&mut gpui::Window, &mut App) + 'static,
) -> Button {
    let debug_selector = selector.clone();
    Button::new(selector, "Retry")
        .variant(ButtonVariant::Link)
        .size(ButtonSize::Compact)
        .tab_stop(true)
        .debug_selector(debug_selector)
        .on_activate(move |_, window, cx| on_activate(window, cx))
}

/// Plain Tab the Pane did not take moves focus, so a surface's actions stay reachable from the
/// keyboard while terminal input is paused.
pub(crate) fn with_tab_traversal(surface: Stateful<gpui::Div>) -> Stateful<gpui::Div> {
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

/// Publishes a server-backed Pane as one labeled terminal without text, because reading shell
/// content through a server-owned Terminal Session is not available yet.
pub(crate) struct UnavailableTerminalAccessibility;

impl TerminalAccessibilityAdapterFactory for UnavailableTerminalAccessibility {
    fn create(
        &self,
        _: &gpui::Window,
        _: TerminalAccessibilityModel,
        _: &crate::appearance::ResolvedFontDescriptor,
        _: Pixels,
    ) -> Box<dyn TerminalAccessibilityAdapter> {
        Box::new(Self)
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
                "Terminal text in a server-owned Terminal Session is not yet available to \
                 assistive technologies.",
            )
    }
}

pub(crate) use connection::{
    ServerPaneConnection, ServerPaneScreen, ServerPaneSync, ServerPaneView,
};

mod connection {
    use std::sync::Arc;

    use super::Dormant;
    use crate::local_server::ServerError;
    use crate::terminal::server_session::{DevelopmentPaneState, DevelopmentPaneStatus};

    /// What a Pane needs from the server connection behind it.
    ///
    /// Only an explicit request reaches the server through this: rendering, reconciliation, and
    /// reconnection never select, take over, or start a shell.
    pub(crate) trait ServerPaneConnection: Send + Sync {
        fn status(&self) -> DevelopmentPaneStatus;

        /// Identifies the connection that last supplied an authoritative screen. It changes when a
        /// new connection reaches the Terminal Session and when another client takes control, and
        /// it outlives any status the Pane misses.
        fn connection_generation(&self) -> u64;

        /// Asks the server for this Pane, which starts or acquires only this Pane.
        fn select(&self);

        /// Asks for a reconnection attempt now instead of at the next automatic one.
        fn retry(&self);

        /// Asks the server to make this client the Controlling Client.
        fn take_over(&self);

        /// Asks the server to start a fresh shell in a Pane retained across a server restart.
        fn start_restored(&self);
        fn directory_progress(&self) -> (bool, Option<&'static str>) {
            (false, None)
        }
        fn directory_server(&self) -> Option<crate::local_server::LocalServer> {
            None
        }
        fn recovery_target(&self) -> Option<spaceterm_server::pane::directories::Target> {
            None
        }
        fn repair_directory(
            &self,
            _: spaceterm_server::pane::directories::Target,
            _: std::path::PathBuf,
        ) {
        }
    }

    impl ServerPaneConnection for DevelopmentPaneState {
        fn status(&self) -> DevelopmentPaneStatus {
            Self::status(self)
        }

        fn connection_generation(&self) -> u64 {
            Self::connection_generation(self)
        }

        fn select(&self) {
            Self::select(self);
        }

        fn retry(&self) {
            Self::retry(self);
        }

        fn take_over(&self) {
            Self::take_over(self);
        }

        fn start_restored(&self) {
            Self::start_restored(self);
        }
        fn directory_progress(&self) -> (bool, Option<&'static str>) {
            Self::directory_progress(self)
        }
        fn directory_server(&self) -> Option<crate::local_server::LocalServer> {
            Self::directory_server(self)
        }
        fn recovery_target(&self) -> Option<spaceterm_server::pane::directories::Target> {
            Self::recovery_target(self)
        }
        fn repair_directory(
            &self,
            target: spaceterm_server::pane::directories::Target,
            replacement: std::path::PathBuf,
        ) {
            Self::repair_directory(self, target, replacement);
        }
    }

    /// What a server-backed Pane presents in place of its terminal, if anything.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum ServerPaneScreen {
        /// Another client is the Controlling Client. `offered` is whether Take Over can be asked
        /// for now; `ended` means the Terminal Session ended while the other client held it.
        Takeover {
            taking_over: bool,
            unconfirmed: bool,
            offered: bool,
            ended: bool,
        },
        /// The server restarted, so the Pane kept its place and lost its shell.
        Restored {
            starting: bool,
            unconfirmed: bool,
            offered: bool,
        },
        /// The server refused to start a shell because the Pane's directory is unavailable.
        /// Repairing it belongs to directory repair, which is not available yet.
        DirectoryUnavailable { starting: bool, offered: bool },
    }

    /// What one status reading changed about a Pane's presentation.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct ServerPaneSync {
        /// The presentation changed, so the Pane repaints.
        pub(crate) changed: bool,
        /// Forget every Screen, title, and accessible value the Pane presented before.
        pub(crate) clear_presentation: bool,
        /// A screen replaced the Pane just now, so focus must leave its terminal.
        pub(crate) entering_screen: bool,
        /// Terminal input is paused and is never replayed.
        pub(crate) interrupted: bool,
    }

    /// One Pane's view of its server connection: the last status read, and the screen that
    /// replaces the Pane while no authoritative shell output belongs to this client.
    ///
    /// The same rules as the Development Pane window apply, minus the window's own lifecycle: an
    /// unanswered Take Over or Start New Shell is claimed by nobody, and a screen stays through
    /// interruptions until a fresh screen arrives for this client.
    pub(crate) struct ServerPaneView {
        connection: Arc<dyn ServerPaneConnection>,
        status: DevelopmentPaneStatus,
        generation: u64,
        takeover_shown: bool,
        take_over_unconfirmed: bool,
        dormant: Option<Dormant>,
        start_pending: bool,
        start_unconfirmed: bool,
        directory_progress: (bool, Option<&'static str>),
    }

    impl ServerPaneView {
        pub(crate) fn new(connection: Arc<dyn ServerPaneConnection>) -> Self {
            let mut view = Self {
                status: connection.status(),
                generation: connection.connection_generation(),
                connection: Arc::clone(&connection),
                takeover_shown: false,
                take_over_unconfirmed: false,
                dormant: None,
                start_pending: false,
                start_unconfirmed: false,
                directory_progress: connection.directory_progress(),
            };
            view.apply();
            view
        }

        /// Asks the server for this Pane, which starts or acquires only this Pane. Only an
        /// explicit Pane selection or Tab opening calls it; rendering and reconciliation never do.
        pub(crate) fn select(&self) {
            self.connection.select();
        }

        /// Reads the connection and reports what changed.
        pub(crate) fn sync(&mut self) -> ServerPaneSync {
            // Status first, because a connection publishes its generation before it reports
            // Connected.
            let status = self.connection.status();
            let generation = self.connection.connection_generation();
            let generation_changed = generation != self.generation;
            let progress = self.connection.directory_progress();
            if status == self.status && !generation_changed && progress == self.directory_progress {
                return ServerPaneSync::default();
            }
            self.directory_progress = progress;
            // Take Over that returns to the Takeover Screen on the same connection generation is
            // unconfirmed: the server may have refused it, or committed it and lost the reply.
            // A new generation means this client held control and another client took it since.
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
            // lost its answer. The Pane claims neither and offers Start New Shell again.
            if self.start_pending && generation_changed && status == DevelopmentPaneStatus::Restored
            {
                self.start_pending = false;
                self.start_unconfirmed = true;
            }
            self.status = status;
            self.generation = generation;
            let mut sync = self.apply();
            sync.changed = true;
            sync
        }

        fn apply(&mut self) -> ServerPaneSync {
            if self.status == DevelopmentPaneStatus::Connected {
                self.takeover_shown = false;
                self.take_over_unconfirmed = false;
                self.dormant = None;
                self.start_pending = false;
                self.start_unconfirmed = false;
            }
            let entering_dormant = self.apply_dormancy();
            // Another client controls the Pane, so this client forgets what it showed. Neither an
            // interruption nor a Take Over in progress reveals that screen again.
            let entering_takeover = !self.takeover_shown
                && matches!(
                    self.status,
                    DevelopmentPaneStatus::Takeover
                        | DevelopmentPaneStatus::TakeoverReconnecting
                        | DevelopmentPaneStatus::TakeoverFailed(_)
                        | DevelopmentPaneStatus::TakingOver
                );
            let controlled_elsewhere = matches!(
                self.status,
                DevelopmentPaneStatus::Takeover
                    | DevelopmentPaneStatus::TakeoverReconnecting
                    | DevelopmentPaneStatus::TakeoverFailed(_)
            );
            if entering_takeover {
                self.takeover_shown = true;
                // Another client started the shell, so nothing here waits to start it.
                self.dormant = None;
                self.start_pending = false;
                self.start_unconfirmed = false;
            }
            ServerPaneSync {
                changed: false,
                clear_presentation: entering_takeover
                    || controlled_elsewhere
                    || self.dormant.is_some(),
                entering_screen: entering_takeover || entering_dormant,
                interrupted: self.status != DevelopmentPaneStatus::Connected,
            }
        }

        /// Presents a Pane retained across a server restart. Its former screen and title belong to
        /// programs that ended with the server. Only an explicit Start New Shell starts a shell.
        fn apply_dormancy(&mut self) -> bool {
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
                    return false;
                }
                _ => return false,
            };
            let entering = self.dormant.is_none();
            if dormant == Dormant::DirectoryUnavailable {
                self.start_pending = false;
                self.start_unconfirmed = false;
            }
            self.dormant = Some(dormant);
            self.takeover_shown = false;
            self.take_over_unconfirmed = false;
            entering
        }

        /// The Session ended while another client controlled it, so nothing remains to take over.
        pub(crate) fn session_ended(&self) -> bool {
            self.status == DevelopmentPaneStatus::TakeoverFailed(ServerError::SessionUnavailable)
        }

        /// The screen replacing the Pane, if one does.
        pub(crate) fn screen(&self) -> Option<ServerPaneScreen> {
            if let Some(dormant) = self.dormant {
                return Some(match dormant {
                    Dormant::DirectoryUnavailable => ServerPaneScreen::DirectoryUnavailable {
                        starting: self.start_pending || self.directory_progress.0,
                        offered: self.can_start_shell(),
                    },
                    Dormant::Restored => ServerPaneScreen::Restored {
                        starting: self.starting_shell(),
                        unconfirmed: self.start_unconfirmed,
                        offered: self.can_start_shell(),
                    },
                });
            }
            self.takeover_shown.then(|| ServerPaneScreen::Takeover {
                taking_over: self.status == DevelopmentPaneStatus::TakingOver,
                unconfirmed: self.take_over_unconfirmed && !self.session_ended(),
                offered: self.status == DevelopmentPaneStatus::Takeover,
                ended: self.session_ended(),
            })
        }

        fn starting_shell(&self) -> bool {
            self.start_pending
                && matches!(
                    self.status,
                    DevelopmentPaneStatus::Restored
                        | DevelopmentPaneStatus::Connecting
                        | DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                )
        }

        pub(crate) fn can_start_shell(&self) -> bool {
            matches!(
                self.status,
                DevelopmentPaneStatus::Restored
                    | DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
            ) && !self.start_pending
                && !self.directory_progress.0
        }

        pub(crate) fn directory_note(&self) -> Option<&'static str> {
            self.directory_progress.1
        }
        pub(crate) fn connection(&self) -> Arc<dyn ServerPaneConnection> {
            Arc::clone(&self.connection)
        }

        pub(crate) fn can_take_over(&self) -> bool {
            self.status == DevelopmentPaneStatus::Takeover
        }

        /// The interruption notice's title, detail, and intent, or `None` while nothing
        /// interrupts the Pane. A screen that explains the state itself reports no notice.
        pub(crate) fn notice(
            &self,
        ) -> Option<(gpui::SharedString, gpui::SharedString, super::StatusIntent)> {
            use gpui::SharedString;
            match self.status {
                DevelopmentPaneStatus::Reconnecting
                | DevelopmentPaneStatus::TakeoverReconnecting => Some((
                    SharedString::new_static("Reconnecting…"),
                    SharedString::new_static(if self.takeover_shown {
                        "Take Over is available once SpaceTerm reconnects. The shell keeps \
                         running on the server."
                    } else if self.dormant == Some(Dormant::Restored) {
                        "Start New Shell is available once SpaceTerm reconnects."
                    } else if self.dormant.is_some() {
                        "No shell is running in this Pane."
                    } else {
                        "Typing is paused and is not sent. The shell keeps running on the server."
                    }),
                    super::StatusIntent::Warning,
                )),
                // The screen replacing the Pane says what happened, and nothing here repairs it.
                DevelopmentPaneStatus::TakeoverFailed(_) if self.session_ended() => None,
                DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                    if self.dormant == Some(Dormant::DirectoryUnavailable) =>
                {
                    None
                }
                DevelopmentPaneStatus::Failed(error)
                | DevelopmentPaneStatus::TakeoverFailed(error) => Some((
                    SharedString::new_static("Not Connected"),
                    SharedString::from(error.to_string()),
                    super::StatusIntent::Error,
                )),
                _ => None,
            }
        }

        /// Whether a person can ask for a connection attempt now.
        pub(crate) fn can_retry(&self) -> bool {
            !self.session_ended()
                && self.status != DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                && matches!(
                    self.status,
                    DevelopmentPaneStatus::Reconnecting
                        | DevelopmentPaneStatus::TakeoverReconnecting
                        | DevelopmentPaneStatus::TakeoverFailed(_)
                        | DevelopmentPaneStatus::Failed(_)
                )
        }

        /// Retries the connection only. It never starts a shell in a dormant Pane.
        pub(crate) fn retry(&mut self) -> ServerPaneSync {
            if !self.can_retry() {
                return ServerPaneSync::default();
            }
            self.connection.retry();
            self.sync()
        }

        pub(crate) fn take_over(&mut self) -> ServerPaneSync {
            if !self.can_take_over() {
                return ServerPaneSync::default();
            }
            self.take_over_unconfirmed = false;
            self.connection.take_over();
            let mut sync = self.sync();
            sync.changed = true;
            sync
        }

        pub(crate) fn start_shell(&mut self) -> ServerPaneSync {
            if !self.can_start_shell() {
                return ServerPaneSync::default();
            }
            self.start_unconfirmed = false;
            self.start_pending = true;
            self.connection.start_restored();
            let mut sync = self.sync();
            sync.changed = true;
            sync
        }
    }
}
