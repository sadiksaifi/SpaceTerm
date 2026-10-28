//! Presentation shared by every Pane whose Terminal Session a SpaceTerm Server owns.
//!
//! Every server-backed Pane shows the same screens, interruption notices, and action statuses, so
//! their copy and composition live here once. The surfaces are deliberately plain: a screen replaces the Pane outright, so nothing of a former
//! shell's output paints or reaches assistive technology after another client takes control or the
//! server restarts.

use crate::server_connection::ServerError;
use gpui::prelude::*;
use gpui::{AnyElement, App, SharedString, Stateful, div};
use spaceterm_server::pane::control::Controller;
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, FloatingRole, Icon, IconName, ProgressRing, ProgressSize,
    ProgressState,
};
use std::{cell::RefCell, rc::Rc};

mod directory_recovery;
#[cfg(test)]
mod tests;
pub(crate) use directory_recovery::{DirectoryRecovery, RecoveryActions};

use super::appearance::gpui_color;
use super::appearance_runtime;
use super::chrome_icons::IconRole;
use super::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use super::terminal_pane::StatusIntent;

/// What the window knows while it cannot ask the server who controls the Pane: only that this
/// window does not.
pub(crate) const TAKEOVER_TITLE: &str = "Take Over to Continue";
pub(crate) const TAKEOVER_DETAIL: &str = "Take Over to type and resize here.";
pub(crate) const UNCONTROLLED_TITLE: &str = "No Client Controls This Pane";
/// The detail wherever this window may take over a Pane no other client holds.
pub(crate) const TAKEOVER_THIS_PANE_DETAIL: &str = "Take Over to type and resize this Pane.";
pub(crate) const TAKEOVER_FAILED_DETAIL: &str = "Take Over is available once SpaceTerm connects.";
/// The Takeover Screen's detail once End All Sessions ended the Pane's shell. The control grant
/// outlives the Session, so the title still names who controls the Pane.
pub(crate) const TAKEOVER_ENDED_DETAIL: &str =
    "End All Sessions ended this Pane\u{2019}s shell. Take Over, then choose Start Session.";
/// The Takeover Screen's title and detail while it offers Take Over, naming the Controlling Client
/// the server reported. `None` means the server has not said, so the screen claims only what this
/// window knows. `dormant` means End All Sessions ended the Pane's shell, so the detail never
/// says the shell keeps running.
pub(crate) fn takeover_copy(
    controller: Option<&Controller>,
    dormant: bool,
) -> (SharedString, &'static str) {
    let detail = |running: &'static str, ended: &'static str| if dormant { ended } else { running };
    match controller {
        Some(Controller::Client {
            name,
            this_client: false,
        }) => (
            format!("Controlled by {}", name.as_str()).into(),
            detail(TAKEOVER_DETAIL, TAKEOVER_ENDED_DETAIL),
        ),
        Some(Controller::Client {
            this_client: true, ..
        }) => (
            TAKEOVER_TITLE.into(),
            detail(TAKEOVER_THIS_PANE_DETAIL, TAKEOVER_ENDED_DETAIL),
        ),
        Some(Controller::Nobody) => (
            UNCONTROLLED_TITLE.into(),
            detail(TAKEOVER_THIS_PANE_DETAIL, TAKEOVER_ENDED_DETAIL),
        ),
        None => (
            TAKEOVER_TITLE.into(),
            detail(TAKEOVER_DETAIL, TAKEOVER_ENDED_DETAIL),
        ),
    }
}
/// The notice a Pane of a server this client is not connected to shows. Only connecting the
/// server's Saved Connection reaches the Pane again.
pub(crate) const OFFLINE_TITLE: &str = "Server Not Connected";
pub(crate) const OFFLINE_DETAIL: &str = "Connect to this server in Settings to use this Pane.";
pub(crate) const SESSION_ENDED_TITLE: &str = "Session Ended";
pub(crate) const SESSION_ENDED_DETAIL: &str = "This Terminal Session is no longer available.";
pub(crate) const TAKING_OVER: &str = "Taking Over…";
pub(crate) const CLOSE_PANE: &str = "Close Pane";
pub(crate) const TAKE_OVER_UNCONFIRMED: &str = "Take Over was not confirmed.";
pub(crate) const RESTORED_TITLE: &str = "Server Restarted";
/// End All Sessions ended this Pane's shell and cleared its output. The Pane, its Tab, and its
/// control stay, so only an explicit start brings a shell back.
pub(crate) const ENDED_DETAIL: &str =
    "End All Sessions ended this Pane\u{2019}s Terminal Session and cleared its output.";
/// The glyph of a Pane waiting for an explicit start: a Terminal Session that isn't running.
/// It stays muted, because nothing went wrong.
pub(crate) const DORMANT_GLYPH: IconName = IconName::Terminal;

/// The copy and selectors of a Pane waiting for an explicit start. End All Sessions and a server
/// restart both leave a Pane there, and each screen names its own cause and action.
pub(crate) struct DormantCopy {
    /// Names the screen's selector.
    pub(crate) screen: &'static str,
    pub(crate) title: &'static str,
    pub(crate) detail: &'static str,
    /// Names the action's selector, and its status slot's.
    pub(crate) action: &'static str,
    pub(crate) label: &'static str,
    pub(crate) description: &'static str,
    pub(crate) progress: &'static str,
    pub(crate) unconfirmed: &'static str,
}

impl DormantCopy {
    /// `ended` means End All Sessions ended the Session; otherwise the server restarted.
    pub(crate) const fn of(ended: bool) -> Self {
        if ended {
            Self {
                screen: "session-ended",
                title: SESSION_ENDED_TITLE,
                detail: ENDED_DETAIL,
                action: "start-session",
                label: "Start Session",
                description: START_DESCRIPTION,
                progress: "Starting Session…",
                unconfirmed: "Start Session was not confirmed.",
            }
        } else {
            Self {
                screen: "restored",
                title: RESTORED_TITLE,
                detail: RESTORED_DETAIL,
                action: "start-shell",
                label: "Start Session",
                description: START_DESCRIPTION,
                progress: STARTING_SHELL,
                unconfirmed: START_SHELL_UNCONFIRMED,
            }
        }
    }
}
const START_DESCRIPTION: &str =
    "Start a new Terminal Session in this Pane\u{2019}s last directory.";
pub(crate) const RESTORED_DETAIL: &str = "This Pane\u{2019}s Terminal Session ended.";
pub(crate) const DIRECTORY_UNAVAILABLE_TITLE: &str = "Directory Unavailable";
pub(crate) const DIRECTORY_UNAVAILABLE_DETAIL: &str =
    "Restore access and choose Try Again, or choose another directory.";
pub(crate) const WORKTREE_MISSING_TITLE: &str = "Worktree Missing";
pub(crate) const WORKTREE_MISSING_DETAIL: &str =
    "Restore it with Git and choose Try Again, or choose another directory.";
pub(crate) const STARTING_SHELL: &str = "Starting Session…";
const RETRY_DIRECTORY: &str = "Try Again";
const CHOOSE_DIRECTORY: &str = "Choose Directory…";
const RETRY_DIRECTORY_DESCRIPTION: &str = "Try starting the shell in this directory again.";
const CHOOSE_DIRECTORY_DESCRIPTION: &str = "Choose a directory for this Pane only; keep its Workspace\u{2019}s Pinned Directory and Worktree.";
const CHOOSE_DIRECTORY_UNSUPPORTED: &str =
    "Update this SpaceTerm Server to choose another directory.";
pub(crate) const START_SHELL_UNCONFIRMED: &str = "Starting the shell was not confirmed.";
/// A start the server may have run. Starting again could leave a second shell in the Pane, so the
/// screen offers a check, which never starts a shell, and names the one restart that ends the wait
/// along with what it ends.
pub(crate) const START_UNKNOWN_TITLE: &str = "Shell Start Not Confirmed";
pub(crate) const START_UNKNOWN_DETAIL: &str = "Check whether the shell started.";
/// What restarting ends, wherever the server restarts: from Server Settings on this computer or
/// on the server's own computer.
pub(crate) const START_UNKNOWN_RESTART: &str =
    "Restarting the server ends every Terminal Session on it.";
const CHECK_START_DESCRIPTION: &str = "Check whether this Pane\u{2019}s shell started.";
const SERVER_SETTINGS: &str = "Server Settings\u{2026}";
const SERVER_SETTINGS_DESCRIPTION: &str = "Open Server Settings.";
pub(crate) const CHECKING_START: &str = "Checking\u{2026}";
pub(crate) const START_STILL_UNKNOWN: &str =
    "SpaceTerm still can\u{2019}t tell whether the shell started.";

/// The notice title and cause of a connection SpaceTerm stopped retrying. Incompatibility names
/// the refusal and never which side is older, because release numbers don't say.
fn failure_copy(error: ServerError) -> (&'static str, SharedString) {
    match error {
        ServerError::Protocol => (
            "Connection Failed",
            SharedString::new_static("SpaceTerm stopped reconnecting."),
        ),
        ServerError::IncompatibleProtocol => (
            "Incompatible Server",
            SharedString::new_static("SpaceTerm and this server are incompatible."),
        ),
        ServerError::MissingRequiredCapability => (
            "Incompatible Server",
            SharedString::new_static("This server needs an update."),
        ),
        error => (OFFLINE_TITLE, SharedString::from(error.to_string())),
    }
}

/// Why a Pane has no shell after the server restarted. The screen naming it replaces the Pane,
/// because the programs it showed ended with the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dormant {
    /// The Pane waits for a person to start a fresh shell.
    Restored,
    /// End All Sessions ended the Pane's Session. It waits for a person to start a fresh one.
    Ended,
    /// The server refused to start a shell, because the Pane's directory is unavailable.
    DirectoryUnavailable,
}

/// Remembers announcements this client actually published, across hidden Tabs and reconnects.
#[derive(Default)]
pub(crate) struct ScreenAnnouncements {
    entry: PublishedAnnouncement,
    outcome: PublishedAnnouncement,
}

#[derive(Clone, Default)]
struct PublishedAnnouncement(Rc<RefCell<Option<SharedString>>>);

impl PublishedAnnouncement {
    fn node(&self, message: &SharedString, enabled: bool) -> gpui::accesskit::Node {
        let mut published = self.0.borrow_mut();
        let changed = enabled && !message.is_empty() && published.as_ref() != Some(message);
        if enabled {
            *published = Some(message.clone());
        }
        let mut node = gpui::accesskit::Node::new(gpui::accesskit::Role::Status);
        node.set_label(message.as_ref());
        node.set_value(message.as_ref());
        node.set_live(if changed {
            gpui::accesskit::Live::Polite
        } else {
            gpui::accesskit::Live::Off
        });
        node
    }
}

pub(crate) struct ScreenStatus {
    content: AnyElement,
    note: SharedString,
}

/// A centered screen replacing a Pane: a title, its detail, one action, and a status slot under
/// the action. The screen is one Group named by its title, which a caller can give the Pane's
/// focus so the keyboard lands on something assistive technology announces.
pub(crate) fn render_screen(
    selector: SharedString,
    copy: ScreenCopy,
    announce: bool,
    actions: ScreenActions,
    status: Option<ScreenStatus>,
    announcements: &ScreenAnnouncements,
    cx: &App,
) -> Stateful<gpui::Div> {
    render_screen_content(selector, copy, announce, actions, status, announcements, cx)
}

pub(crate) fn render_directory_screen(
    selector: SharedString,
    path: Option<SharedString>,
    actions: ScreenActions,
    status: Option<ScreenStatus>,
    missing_worktree: bool,
    announcements: &ScreenAnnouncements,
    cx: &App,
) -> Stateful<gpui::Div> {
    let detail = if missing_worktree {
        WORKTREE_MISSING_DETAIL
    } else {
        DIRECTORY_UNAVAILABLE_DETAIL
    };
    render_screen_content(
        selector,
        ScreenCopy {
            title: if missing_worktree {
                WORKTREE_MISSING_TITLE.into()
            } else {
                DIRECTORY_UNAVAILABLE_TITLE.into()
            },
            detail: detail.into(),
            path,
            intent: StatusIntent::Warning,
            glyph: None,
        },
        true,
        actions,
        status,
        announcements,
        cx,
    )
}

/// The title, detail, and intent of a screen, and the path it is about, if any. The intent
/// picks the icon the interruption notice uses for it, and colors it only when the person must
/// act.
pub(crate) struct ScreenCopy {
    title: SharedString,
    detail: SharedString,
    path: Option<SharedString>,
    intent: StatusIntent,
    glyph: Option<IconName>,
}
impl ScreenCopy {
    pub(crate) fn new(title: SharedString, detail: SharedString) -> Self {
        Self {
            title,
            detail,
            path: None,
            intent: StatusIntent::Information,
            glyph: None,
        }
    }

    pub(crate) fn intent(mut self, intent: StatusIntent) -> Self {
        self.intent = intent;
        self
    }

    /// A glyph that names what the screen is about, in place of the intent's.
    pub(crate) fn glyph(mut self, glyph: IconName) -> Self {
        self.glyph = Some(glyph);
        self
    }
}

/// A screen's Primary action and the action it offers as a link, in that reading and Tab order.
pub(crate) struct ScreenActions {
    pub(crate) primary: Option<Button>,
    pub(crate) link: Option<Button>,
}

fn render_screen_content(
    selector: SharedString,
    copy: ScreenCopy,
    announce: bool,
    actions: ScreenActions,
    status: Option<ScreenStatus>,
    announcements: &ScreenAnnouncements,
    cx: &App,
) -> Stateful<gpui::Div> {
    let ScreenCopy {
        title,
        detail,
        path,
        intent,
        glyph,
    } = copy;
    let appearance = super::appearance::shared_chrome(cx);
    // The configured terminal background, never one the shell chose.
    let surface =
        appearance.pane_surface(appearance_runtime::current(cx).terminal.colors.background);
    let detail_id = SharedString::from(format!("{selector}-detail"));
    let slot_id = SharedString::from(format!("{selector}-status-slot"));
    let message: SharedString = match &path {
        Some(_) => detail.clone(),
        None => format!("{title}. {detail}").into(),
    };
    let entry = announcements.entry.clone();
    let outcome = announcements.outcome.clone();
    let note = status
        .as_ref()
        .map_or_else(SharedString::default, |status| status.note.clone());
    let element_selector = selector.clone();
    let differentiate = appearance.capabilities.differentiate_without_color;
    let mut state = super::empty_state::empty_state(title.clone(), cx)
        .icon(glyph.unwrap_or_else(|| intent.glyph(differentiate)))
        .tone(match intent {
            StatusIntent::Warning => spaceterm_ui::EmptyStateTone::Warning,
            StatusIntent::Error => spaceterm_ui::EmptyStateTone::Error,
            StatusIntent::Information | StatusIntent::Success => {
                spaceterm_ui::EmptyStateTone::Quiet
            }
        })
        .detail_element(
            div()
                .id(detail_id)
                .role(gpui::accesskit::Role::Group)
                .a11y_synthetic_children(move |builder| {
                    let mut node = entry.node(&message, announce);
                    if let Some(bounds) = builder.parent_node().bounds() {
                        node.set_bounds(bounds);
                    }
                    builder.push_child(builder.synthetic_node_id("entry"), node);
                })
                .child(detail),
        )
        // The slot keeps at least one line's height, so the actions stay put while their status
        // changes, and a status too long for a narrow Pane wraps below them.
        .footer(
            div()
                .id(slot_id)
                .role(gpui::accesskit::Role::Group)
                .a11y_synthetic_children(move |builder| {
                    let mut node = outcome.node(&note, true);
                    if let Some(bounds) = builder.parent_node().bounds() {
                        node.set_bounds(bounds);
                    }
                    builder.push_child(builder.synthetic_node_id("outcome"), node);
                })
                .mt(appearance.spacing(12.0))
                .min_h(appearance.spacing(20.0))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .children(status.map(|status| status.content)),
        );
    if let Some(path) = path {
        state = state.supplement(
            div()
                .chrome_text(appearance.typography.style(TextRole::Body))
                .text_color(gpui_color(appearance.colors.text))
                .child(path),
        );
    }
    if let Some(primary) = actions.primary {
        state = state.primary(primary);
    }
    if let Some(link) = actions.link {
        state = state.link(link);
    }
    // The screen scrolls, and its content centers only while it fits. A Pane too short for it
    // shows the title first instead of clipping it under the Pane Caption, and a notice placed
    // after the content never covers the actions.
    div()
        .id(selector)
        .role(gpui::accesskit::Role::Group)
        .aria_label(title)
        .debug_selector(move || element_selector.to_string())
        .absolute()
        .inset_0()
        .bg(gpui_color(surface))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .child(
            div()
                .my_auto()
                .flex()
                .flex_col()
                .items_center()
                .px(appearance.spacing(24.0))
                .py(appearance.spacing(12.0))
                .child(state),
        )
}

/// Places an interruption notice after a screen's content, at the Pane's bottom edge while the
/// content fits. A notice floating over a screen would cover its actions in a short Pane.
pub(crate) fn with_notice(
    screen: Stateful<gpui::Div>,
    notice: Option<AnyElement>,
    cx: &App,
) -> Stateful<gpui::Div> {
    let appearance = super::appearance::shared_chrome(cx);
    screen.children(notice.map(|notice| {
        div()
            .flex()
            .justify_end()
            .px(appearance.spacing(12.0))
            .pb(appearance.spacing(12.0))
            .child(notice)
    }))
}

/// Progress is available for exploration, without interrupting the action's outcome speech.
pub(crate) fn progress_status(
    selector: SharedString,
    progress_id: SharedString,
    label: SharedString,
    cx: &App,
) -> ScreenStatus {
    let appearance = super::appearance::shared_chrome(cx);
    let element_selector = selector.clone();
    let content = div()
        .id(selector)
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
        .into_any_element();
    ScreenStatus {
        content,
        note: SharedString::default(),
    }
}

/// An action's outcome as plain text, read by assistive technology as one status.
pub(crate) fn note_status(selector: SharedString, label: SharedString, cx: &App) -> ScreenStatus {
    let appearance = super::appearance::shared_chrome(cx);
    let element_selector = selector.clone();
    let content = div()
        .id(selector)
        .debug_selector(move || element_selector.to_string())
        .whitespace_normal()
        .text_center()
        .chrome_text(appearance.typography.style(TextRole::Secondary))
        .text_color(gpui_color(appearance.colors.text_secondary))
        .child(label.clone())
        .into_any_element();
    ScreenStatus {
        content,
        note: label,
    }
}

/// The Try Again and Choose Directory pair the directory screen offers.
///
/// The caller owns the element ids, what it knows about availability, and what each action does.
/// The copy and the enablement policy belong here, so the screen always explains itself the same
/// way.
pub(crate) struct DirectoryActions<Retry, Choose> {
    /// Element id and debug selector for Try Again.
    pub(crate) retry: SharedString,
    /// Element id and debug selector for Choose Directory.
    pub(crate) choose: SharedString,
    /// Whether the screen can act at all: nothing is starting and no picker is open.
    pub(crate) enabled: bool,
    pub(crate) offer: ChooseDirectory,
    pub(crate) on_retry: Retry,
    pub(crate) on_choose: Choose,
}

/// What the owning server and this client allow for Choose Directory, beyond a busy screen.
#[derive(Clone, Copy)]
pub(crate) enum ChooseDirectory {
    /// The owning server offers a recovery target for this Pane.
    Offered,
    /// The owning server negotiated no directory recovery.
    Unsupported,
    /// This client cannot change the Workspace, for a reason it already explains.
    Withheld(&'static str),
}

impl ChooseDirectory {
    const fn description(self) -> &'static str {
        match self {
            Self::Offered => CHOOSE_DIRECTORY_DESCRIPTION,
            Self::Unsupported => CHOOSE_DIRECTORY_UNSUPPORTED,
            Self::Withheld(explanation) => explanation,
        }
    }

    const fn offered(self) -> bool {
        matches!(self, Self::Offered)
    }
}

impl<Retry, Choose> DirectoryActions<Retry, Choose>
where
    Retry: Fn(&mut gpui::Window, &mut App) + 'static,
    Choose: Fn(&mut gpui::Window, &mut App) + 'static,
{
    /// The Primary Choose Directory, then Try Again as a link, in reading and Tab order.
    pub(crate) fn buttons(self) -> ScreenActions {
        let Self {
            retry,
            choose,
            enabled,
            offer,
            on_retry,
            on_choose,
        } = self;
        ScreenActions {
            primary: Some(
                Button::new(choose.clone(), CHOOSE_DIRECTORY)
                    .disabled(!enabled || !offer.offered())
                    .tab_stop(true)
                    // A disabled primary still says why, so nothing is refused silently.
                    .accessibility_description(offer.description())
                    .debug_selector(choose)
                    .on_activate(move |_, window, cx| on_choose(window, cx)),
            ),
            link: Some(
                Button::new(retry.clone(), RETRY_DIRECTORY)
                    .disabled(!enabled)
                    .tab_stop(true)
                    .accessibility_description(RETRY_DIRECTORY_DESCRIPTION)
                    .debug_selector(retry)
                    .on_activate(move |_, window, cx| on_retry(window, cx)),
            ),
        }
    }
}

/// The directory screen's status slot: progress while a shell starts, otherwise the last outcome.
pub(crate) fn directory_status(
    selector: SharedString,
    progress: SharedString,
    starting: bool,
    note: Option<&'static str>,
    cx: &App,
) -> Option<ScreenStatus> {
    if starting {
        return Some(progress_status(
            selector,
            progress,
            STARTING_SHELL.into(),
            cx,
        ));
    }
    note.map(|note| note_status(selector, note.into(), cx))
}

/// The screen of a Pane whose start may have run. Check Shell checks, and never starts a shell. Server
/// Settings opens the existing Restart Server journey, which asks before it ends anything.
///
/// The caller owns the element ids, what each action does, and whether this client can restart
/// the Pane's server.
pub(crate) struct StartUnknownScreen<Retry, Settings> {
    /// Element id and debug selector for the screen.
    pub(crate) screen: SharedString,
    /// Element id and debug selector for Retry, and the prefix of its status slot's.
    pub(crate) retry: SharedString,
    /// Element id and debug selector for Server Settings, when this client can restart the
    /// Pane's server there. `None` explains the restart without offering it.
    pub(crate) server_settings: Option<SharedString>,
    /// A check is waiting for the server's answer.
    pub(crate) checking: bool,
    /// The last check ended without settling the start.
    pub(crate) unanswered: bool,
    pub(crate) on_retry: Retry,
    pub(crate) on_server_settings: Settings,
}

impl<Retry, Settings> StartUnknownScreen<Retry, Settings>
where
    Retry: Fn(&mut gpui::Window, &mut App) + 'static,
    Settings: Fn(&mut gpui::Window, &mut App) + 'static,
{
    pub(crate) fn render(
        self,
        announcements: &ScreenAnnouncements,
        cx: &App,
    ) -> Stateful<gpui::Div> {
        let Self {
            screen,
            retry,
            server_settings,
            checking,
            unanswered,
            on_retry,
            on_server_settings,
        } = self;
        let status_selector = SharedString::from(format!("{retry}-status"));
        let status = if checking {
            Some(progress_status(
                status_selector,
                format!("{retry}-progress").into(),
                CHECKING_START.into(),
                cx,
            ))
        } else if unanswered {
            Some(note_status(status_selector, START_STILL_UNKNOWN.into(), cx))
        } else {
            None
        };
        // The Primary check first, then Server Settings as a link, as the directory screen
        // orders its pair.
        let actions = ScreenActions {
            primary: Some(
                Button::new(retry.clone(), "Check Shell")
                    .disabled(checking)
                    .tab_stop(true)
                    .accessibility_description(CHECK_START_DESCRIPTION)
                    .debug_selector(retry)
                    .on_activate(move |_, window, cx| on_retry(window, cx)),
            ),
            link: server_settings.map(|selector| {
                Button::new(selector.clone(), SERVER_SETTINGS)
                    .tab_stop(true)
                    .accessibility_description(SERVER_SETTINGS_DESCRIPTION)
                    .debug_selector(selector)
                    .on_activate(move |_, window, cx| on_server_settings(window, cx))
            }),
        };
        render_screen_content(
            screen,
            ScreenCopy::new(
                START_UNKNOWN_TITLE.into(),
                format!("{START_UNKNOWN_DETAIL} {START_UNKNOWN_RESTART}").into(),
            )
            .intent(StatusIntent::Warning),
            true,
            actions,
            status,
            announcements,
            cx,
        )
    }
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
    let description = detail.clone();
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

/// The Start Server action of a Pane whose server the person stopped. Retrying could never
/// connect, and only an explicit Start reverses Stop.
pub(crate) fn start_server_button(
    selector: SharedString,
    on_activate: impl Fn(&mut gpui::Window, &mut App) + 'static,
) -> Button {
    let debug_selector = selector.clone();
    Button::new(selector, "Start Server")
        .variant(ButtonVariant::Link)
        .size(ButtonSize::Compact)
        .tab_stop(true)
        .debug_selector(debug_selector)
        .on_activate(move |_, window, cx| on_activate(window, cx))
}

/// The standard Try Again action of an interruption notice.
pub(crate) fn retry_button(
    selector: SharedString,
    on_activate: impl Fn(&mut gpui::Window, &mut App) + 'static,
) -> Button {
    let debug_selector = selector.clone();
    Button::new(selector, "Try Again")
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

pub(crate) use connection::{
    ServerPaneConnection, ServerPaneScreen, ServerPaneSync, ServerPaneView,
};

mod connection {
    use std::sync::Arc;

    use super::{Controller, Dormant, ScreenAnnouncements};
    use crate::server_connection::ServerError;
    use crate::terminal::server_session::{DevelopmentPaneState, DevelopmentPaneStatus};

    /// What a Pane needs from the server connection behind it.
    ///
    /// Only an explicit request reaches the server through this: rendering, reconciliation, and
    /// reconnection never select, take over, or start a shell.
    pub(crate) trait ServerPaneConnection: Send + Sync {
        fn status(&self) -> DevelopmentPaneStatus;

        /// Whether the original presentation admission still occupies a startup slot. Receiving
        /// a screen is a separate input barrier and may wait while this Pane is hidden.
        fn startup_pending(&self) -> bool {
            matches!(
                self.status(),
                DevelopmentPaneStatus::Connecting
                    | DevelopmentPaneStatus::TakingOver
                    | DevelopmentPaneStatus::Reconnecting
            )
        }

        /// Who controlled the Pane when the server last offered Take Over, if it ever did.
        fn controller(&self) -> Option<Controller>;

        /// Hints `hint` now and whenever what [`ServerPaneView::sync`] reads may have changed,
        /// so a view reads the connection only then.
        fn observe(&self, hint: async_channel::Sender<()>);

        /// Whether End All Sessions ended the Pane's shell and nobody has started a new one, as
        /// the server last said, including while another client controls the Pane.
        fn session_dormant(&self) -> bool {
            false
        }

        /// Identifies the connection that last supplied an authoritative screen. It changes when a
        /// new connection reaches the Terminal Session and when another client takes control, and
        /// it outlives any status the Pane misses.
        fn connection_generation(&self) -> u64;

        /// Asks the server for this Pane, which starts or acquires only this Pane.
        fn select(&self);

        /// Starts this client's exact new Pane after membership binds, without acquiring control
        /// or authorizing a retained shell after a server restart.
        fn start_created(&self) {}

        /// Asks for a reconnection attempt now instead of at the next automatic one.
        fn retry(&self);

        /// Asks the server to make this client the Controlling Client.
        fn take_over(&self);

        /// Asks the server to start a fresh shell in a Pane retained across a server restart.
        fn start_restored(&self);

        /// Whether a shell start this client asked for may have run without an answer. While it
        /// may have, nothing starts another shell, and Retry only checks.
        fn start_unconfirmed(&self) -> bool {
            false
        }
        fn directory_progress(&self) -> (bool, Option<&'static str>) {
            (false, None)
        }
        fn directory_server(&self) -> Option<crate::server_connection::ServerConnection> {
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

        fn startup_pending(&self) -> bool {
            Self::startup_pending(self)
        }

        fn controller(&self) -> Option<Controller> {
            Self::controller(self)
        }

        fn observe(&self, hint: async_channel::Sender<()>) {
            Self::observe(self, hint);
        }

        fn session_dormant(&self) -> bool {
            Self::session_dormant(self)
        }

        fn connection_generation(&self) -> u64 {
            Self::connection_generation(self)
        }

        fn select(&self) {
            Self::select(self);
        }

        fn start_created(&self) {
            Self::start_created(self);
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

        fn start_unconfirmed(&self) -> bool {
            Self::start_unconfirmed(self)
        }
        fn directory_progress(&self) -> (bool, Option<&'static str>) {
            Self::directory_progress(self)
        }
        fn directory_server(&self) -> Option<crate::server_connection::ServerConnection> {
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
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum ServerPaneScreen {
        /// Another client is the Controlling Client. `offered` is whether Take Over can be asked
        /// for now; `ended` means the Terminal Session ended while the other client held it.
        /// `controller` is who the server last said controls the Pane, while that still holds.
        Takeover {
            taking_over: bool,
            unconfirmed: bool,
            offered: bool,
            ended: bool,
            controller: Option<Controller>,
            /// End All Sessions ended the shell, so nothing here says it keeps running.
            dormant: bool,
        },
        /// The Pane kept its place and lost its shell: the server restarted, or `ended` says
        /// End All Sessions ended it. Either way only an explicit start brings a shell back.
        Restored {
            starting: bool,
            unconfirmed: bool,
            offered: bool,
            ended: bool,
        },
        /// The server refused to start a shell because the Pane's directory is unavailable.
        /// Repairing it belongs to directory repair, which is not available yet.
        DirectoryUnavailable { starting: bool, offered: bool },
        /// A start may have run without an answer, so only a check is offered. `checking` waits
        /// for the server; `unanswered` means the last check settled nothing.
        StartUnknown { checking: bool, unanswered: bool },
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
    /// An unanswered Take Over or Start New Shell is claimed by nobody, and a screen stays through
    /// interruptions until a fresh screen arrives for this client.
    pub(crate) struct ServerPaneView {
        announcements: ScreenAnnouncements,
        connection: Arc<dyn ServerPaneConnection>,
        status: DevelopmentPaneStatus,
        controller: Option<Controller>,
        session_dormant: bool,
        generation: u64,
        takeover_shown: bool,
        take_over_unconfirmed: bool,
        dormant: Option<Dormant>,
        start_pending: bool,
        start_unconfirmed: bool,
        /// The connection's word that a start may have run, which withholds every new start.
        start_unknown: bool,
        check_pending: bool,
        check_unanswered: bool,
        /// An interruption lost a start's answer, so the Pane says the start is unconfirmed until
        /// the server settles it.
        unknown_shown: bool,
        directory_progress: (bool, Option<&'static str>),
    }

    impl ServerPaneView {
        pub(crate) fn new(connection: Arc<dyn ServerPaneConnection>) -> Self {
            let mut view = Self {
                announcements: ScreenAnnouncements::default(),
                status: connection.status(),
                controller: connection.controller(),
                session_dormant: connection.session_dormant(),
                generation: connection.connection_generation(),
                connection: Arc::clone(&connection),
                takeover_shown: false,
                take_over_unconfirmed: false,
                dormant: None,
                start_pending: false,
                start_unconfirmed: false,
                start_unknown: connection.start_unconfirmed(),
                check_pending: false,
                check_unanswered: false,
                unknown_shown: false,
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
            let controller = self.connection.controller();
            let session_dormant = self.connection.session_dormant();
            let start_unknown = self.connection.start_unconfirmed();
            if status == self.status
                && !generation_changed
                && progress == self.directory_progress
                && controller == self.controller
                && session_dormant == self.session_dormant
                && start_unknown == self.start_unknown
            {
                return ServerPaneSync::default();
            }
            // A check ends when the server answers it: a new connection reached the Pane, or the
            // start is settled either way.
            if self.check_pending && (generation_changed || !start_unknown) {
                self.check_pending = false;
                self.check_unanswered = start_unknown;
            }
            if !start_unknown {
                self.check_unanswered = false;
            }
            self.start_unknown = start_unknown;
            self.directory_progress = progress;
            self.controller = controller;
            self.session_dormant = session_dormant;
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
            if self.start_pending
                && generation_changed
                && matches!(
                    status,
                    DevelopmentPaneStatus::Restored | DevelopmentPaneStatus::SessionEnded
                )
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
                self.announcements = ScreenAnnouncements::default();
                self.takeover_shown = false;
                self.take_over_unconfirmed = false;
                self.dormant = None;
                self.start_pending = false;
                self.start_unconfirmed = false;
                self.check_pending = false;
                self.check_unanswered = false;
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
            // A start in flight that meets an interruption has lost its answer, whether a click or
            // a first selection sent it. The Pane says so through checks and reconnects until the
            // server settles the start.
            if !self.start_unknown || self.status == DevelopmentPaneStatus::Connected {
                self.unknown_shown = false;
            }
            let entering_unknown = !self.unknown_shown
                && self.start_unknown
                && matches!(
                    self.status,
                    DevelopmentPaneStatus::Reconnecting
                        | DevelopmentPaneStatus::TakeoverReconnecting
                        | DevelopmentPaneStatus::Failed(_)
                        | DevelopmentPaneStatus::TakeoverFailed(_)
                );
            if entering_unknown {
                self.unknown_shown = true;
            }
            ServerPaneSync {
                changed: false,
                clear_presentation: entering_takeover
                    || controlled_elsewhere
                    || self.dormant.is_some()
                    || self.unknown_shown,
                entering_screen: entering_takeover || entering_dormant || entering_unknown,
                interrupted: self.status != DevelopmentPaneStatus::Connected,
            }
        }

        /// Presents a Pane retained across a server restart. Its former screen and title belong to
        /// programs that ended with the server. Only an explicit Start New Shell starts a shell.
        fn apply_dormancy(&mut self) -> bool {
            let dormant = match self.status {
                DevelopmentPaneStatus::Restored => Dormant::Restored,
                DevelopmentPaneStatus::SessionEnded => Dormant::Ended,
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
            // A Pane that changes why it waits presents its new screen, and its focus follows.
            let entering = self.dormant != Some(dormant);
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
            if self.unknown_shown {
                return Some(ServerPaneScreen::StartUnknown {
                    checking: self.check_pending,
                    unanswered: self.check_unanswered,
                });
            }
            if let Some(dormant) = self.dormant {
                return Some(match dormant {
                    Dormant::DirectoryUnavailable => ServerPaneScreen::DirectoryUnavailable {
                        starting: self.start_pending
                            || self.directory_progress.0
                            || self.start_in_flight(),
                        offered: self.can_start_shell(),
                    },
                    Dormant::Restored | Dormant::Ended => ServerPaneScreen::Restored {
                        starting: self.starting_shell() || self.start_in_flight(),
                        unconfirmed: self.start_unconfirmed,
                        offered: self.can_start_shell(),
                        ended: dormant == Dormant::Ended,
                    },
                });
            }
            let taking_over = self.status == DevelopmentPaneStatus::TakingOver;
            let offered = self.can_take_over();
            self.takeover_shown.then(|| ServerPaneScreen::Takeover {
                taking_over,
                unconfirmed: self.take_over_unconfirmed && !self.session_ended(),
                offered,
                ended: self.session_ended(),
                // An interrupted connection cannot say who controls the Pane now.
                controller: self.controller.clone().filter(|_| offered || taking_over),
                dormant: self.session_dormant,
            })
        }

        fn starting_shell(&self) -> bool {
            self.start_pending
                && matches!(
                    self.status,
                    DevelopmentPaneStatus::Restored
                        | DevelopmentPaneStatus::SessionEnded
                        | DevelopmentPaneStatus::Connecting
                        | DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                )
        }

        /// A start the server has not answered and no interruption has lost, such as one a first
        /// selection sent. It shows as starting.
        fn start_in_flight(&self) -> bool {
            self.start_unknown && !self.unknown_shown
        }

        pub(crate) fn can_start_shell(&self) -> bool {
            !self.start_unknown
                && matches!(
                    self.status,
                    DevelopmentPaneStatus::Restored
                        | DevelopmentPaneStatus::SessionEnded
                        | DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                )
                && !self.start_pending
                && !self.directory_progress.0
        }

        pub(crate) fn directory_note(&self) -> Option<&'static str> {
            self.directory_progress.1
        }
        pub(crate) fn announcements(&self) -> &ScreenAnnouncements {
            &self.announcements
        }
        pub(crate) fn connection(&self) -> Arc<dyn ServerPaneConnection> {
            Arc::clone(&self.connection)
        }

        pub(crate) fn can_take_over(&self) -> bool {
            self.status == DevelopmentPaneStatus::Takeover && !self.start_unknown
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
                    // The screen explains the unconfirmed start and offers its check, so the notice
                    // adds only what reconnecting does.
                    SharedString::new_static(if self.unknown_shown {
                        "SpaceTerm checks whether the shell started after reconnecting."
                    } else if self.takeover_shown {
                        "Take Over is available after reconnection."
                    } else if matches!(self.dormant, Some(Dormant::Restored | Dormant::Ended)) {
                        "Start Session is available after reconnection."
                    } else if self.dormant.is_some() {
                        "No shell is running."
                    } else {
                        "Typing is paused."
                    }),
                    super::StatusIntent::Warning,
                )),
                DevelopmentPaneStatus::Offline => Some((
                    SharedString::new_static(super::OFFLINE_TITLE),
                    SharedString::new_static(super::OFFLINE_DETAIL),
                    super::StatusIntent::Warning,
                )),
                // The screen replacing the Pane says what happened, and nothing here repairs it.
                DevelopmentPaneStatus::TakeoverFailed(_) if self.session_ended() => None,
                DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                    if self.dormant == Some(Dormant::DirectoryUnavailable) =>
                {
                    None
                }
                DevelopmentPaneStatus::Failed(ServerError::Stopped)
                | DevelopmentPaneStatus::TakeoverFailed(ServerError::Stopped) => Some((
                    SharedString::new_static("Server Stopped"),
                    SharedString::new_static("The programs in this Pane ended."),
                    super::StatusIntent::Warning,
                )),
                DevelopmentPaneStatus::Failed(error)
                | DevelopmentPaneStatus::TakeoverFailed(error) => {
                    let (title, cause) = super::failure_copy(error);
                    // The screen explains an unconfirmed start, so the notice names only the cause.
                    let next = if self.unknown_shown {
                        None
                    } else if error == ServerError::Protocol {
                        Some(self.after_reconnecting())
                    } else {
                        None
                    };
                    Some((
                        SharedString::new_static(title),
                        match next {
                            Some(next) => next.into(),
                            None => cause,
                        },
                        super::StatusIntent::Error,
                    ))
                }
                _ => None,
            }
        }

        /// What becomes of the Pane once SpaceTerm connects, after it stopped reconnecting.
        fn after_reconnecting(&self) -> &'static str {
            if self.takeover_shown {
                "Choose Try Again to reconnect, then Take Over to use this Pane."
            } else if matches!(self.dormant, Some(Dormant::Restored | Dormant::Ended)) {
                "Choose Try Again to reconnect, then Start Session to open a shell."
            } else if self.dormant.is_some() {
                "No shell is running in this Pane. Choose Try Again to reconnect."
            } else {
                "Choose Try Again to reconnect."
            }
        }

        /// Whether the person stopped the server behind this Pane, which only Start reverses.
        pub(crate) fn server_stopped(&self) -> bool {
            matches!(
                self.status,
                DevelopmentPaneStatus::Failed(ServerError::Stopped)
                    | DevelopmentPaneStatus::TakeoverFailed(ServerError::Stopped)
            )
        }

        /// Whether a person can ask for a connection attempt now. A stopped server offers
        /// Start Server instead, because a retry could never connect. A start that may have run
        /// can be checked from any status, because the check is passive.
        pub(crate) fn can_retry(&self) -> bool {
            let interrupted = self.status
                != DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                && matches!(
                    self.status,
                    DevelopmentPaneStatus::Reconnecting
                        | DevelopmentPaneStatus::TakeoverReconnecting
                        | DevelopmentPaneStatus::TakeoverFailed(_)
                        | DevelopmentPaneStatus::Failed(_)
                );
            !self.session_ended()
                && !self.server_stopped()
                && !self.check_pending
                && (interrupted || self.unknown_shown)
        }

        /// Whether the interruption notice offers Retry. The unconfirmed-start screen offers its own
        /// Retry, so the notice doesn't repeat it.
        pub(crate) fn notice_offers_retry(&self) -> bool {
            self.can_retry() && !self.unknown_shown
        }

        /// Connects again once the person started the server this Pane stopped with. The
        /// connection keeps its mode, so the server says what became of the Pane; nothing takes
        /// control or starts a shell. Any other Pane is left alone.
        pub(crate) fn server_started(&mut self) -> ServerPaneSync {
            if !self.server_stopped() {
                return ServerPaneSync::default();
            }
            self.connection.retry();
            self.sync()
        }

        /// Retries the connection only. It never starts a shell in a dormant Pane. While a start
        /// may have run, the retry checks it until the server answers.
        pub(crate) fn retry(&mut self) -> ServerPaneSync {
            if !self.can_retry() {
                return ServerPaneSync::default();
            }
            let checking = self.unknown_shown;
            if checking {
                self.check_pending = true;
                self.check_unanswered = false;
            }
            self.connection.retry();
            let mut sync = self.sync();
            sync.changed |= checking;
            sync
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
