//! Update presentation: the title bar Updates menu, menu commands, prompts, and launch view.
//! [`crate::updates`] owns update policy and state.

mod dropdown;
mod launch;
pub(crate) mod server_rows;

pub(super) use dropdown::UpdateControl;
pub(crate) use launch::{request_window_close as request_launch_window_close, show_launch};
pub(crate) use server_rows::{LocalUpdateFacts, LocalUpdateObservation, install_local_updates};

use gpui::{
    AnyWindowHandle, App, BorrowAppContext, Context, Entity, Global, SharedString, Subscription,
    Window, WindowHandle, actions,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::WorkspaceManager;
use super::settings_window::SettingsWindow;
use crate::desktop_profile::{DesktopPresentation, DesktopWording};
use crate::updates::policy::UpdateStage;
use crate::updates::{ApplicationUpdates, UpdateError, UpdateNotice, UpdateService, UpdateState};
use server_rows::{ServerNeed, ServerUpdateAction, ServerUpdateRow};

actions!(spaceterm, [CheckForUpdates, OpenReleaseNotes]);

pub(crate) const CHECK_FOR_UPDATES_TITLE: &str = "Check for Updates…";
pub(crate) const RELEASE_NOTES_TITLE: &str = "Release Notes";
const RELEASE_NOTES_URL: &str = "https://github.com/sadiksaifi/SpaceTerm/releases/latest";
const CURRENT_VERSION: &str = env!("SPACETERM_VERSION");

/// Installs the menu commands and the one application-wide owner of update prompts. The update
/// service must already be installed.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &CheckForUpdates, cx| check_for_updates(cx));
    cx.on_action(|_: &OpenReleaseNotes, cx| cx.open_url(RELEASE_NOTES_URL));
    let Some(service) = cx.try_global::<UpdateService>().cloned() else {
        return;
    };
    let notices = cx.subscribe(&service.0, |_, notice: &UpdateNotice, cx| {
        receive_notice(*notice, cx);
    });
    let states = cx.observe(&service.0, |updates, cx| observe_state(&updates, cx));
    cx.set_global(UpdatePrompts {
        results: CheckResults::default(),
        install_requested: None,
        _subscriptions: [notices, states],
    });
}

/// What the user is waiting to hear about. Update state itself stays in the service.
struct UpdatePrompts {
    results: CheckResults,
    /// The user asked to install, so the confirmation is shown even over another prompt.
    install_requested: Option<InstallRequest>,
    _subscriptions: [Subscription; 2],
}

/// Where the user asked to install, which is where the confirmation belongs.
#[derive(Clone, Copy, Debug)]
enum InstallRequest {
    /// The application menu or the Updates menu, answered in the front Workspace window.
    FrontWorkspace,
    /// A window that hosts update prompts itself, such as Settings.
    Window(AnyWindowHandle),
}

impl Global for UpdatePrompts {}

/// Decides which results reach the user, each exactly once. GPUI may deliver the service's notice
/// and its observer notification in either order, so both claim the current result here.
#[derive(Debug, Default)]
struct CheckResults {
    /// A menu check is waiting for its result, including one that joined a scheduled check.
    awaiting: bool,
    /// The result presented last, until the service leaves it.
    presented: Option<UpdateState>,
}

impl CheckResults {
    fn request(&mut self) {
        self.awaiting = true;
        self.presented = None;
    }

    fn observe(&mut self, state: &UpdateState) -> Option<UpdateState> {
        if self
            .presented
            .as_ref()
            .is_some_and(|presented| presented != state)
        {
            self.presented = None;
        }
        match state {
            UpdateState::Checking => None,
            UpdateState::UpToDate | UpdateState::Available { .. } | UpdateState::Failed { .. } => {
                if self.awaiting {
                    self.claim(state)
                } else {
                    None
                }
            }
            _ => {
                self.awaiting = false;
                None
            }
        }
    }

    /// An unrequested failure is presented too, because it interrupts a download the user began.
    fn notice(&mut self, notice: UpdateNotice, state: &UpdateState) -> Option<UpdateState> {
        let unawaited = match notice {
            UpdateNotice::CheckFinished => false,
            UpdateNotice::Failed => !matches!(
                state,
                UpdateState::Failed {
                    error: UpdateError::Check
                }
            ),
            UpdateNotice::ReadyToInstall => return None,
        };
        if !(self.awaiting || unawaited) || self.presented.as_ref() == Some(state) {
            return None;
        }
        self.claim(state)
    }

    fn claim(&mut self, state: &UpdateState) -> Option<UpdateState> {
        if !matches!(
            state,
            UpdateState::UpToDate | UpdateState::Available { .. } | UpdateState::Failed { .. }
        ) {
            return None;
        }
        self.awaiting = false;
        self.presented = Some(state.clone());
        Some(state.clone())
    }
}

fn update_results<R>(update: impl FnOnce(&mut CheckResults) -> R, cx: &mut App) -> Option<R> {
    if !cx.has_global::<UpdatePrompts>() {
        return None;
    }
    Some(cx.update_global::<UpdatePrompts, _>(|prompts, _| update(&mut prompts.results)))
}

fn take_install_request(cx: &mut App) -> Option<InstallRequest> {
    if !cx.has_global::<UpdatePrompts>() {
        return None;
    }
    cx.update_global::<UpdatePrompts, _>(|prompts, _| prompts.install_requested.take())
}

fn service(cx: &App) -> Option<Entity<ApplicationUpdates>> {
    cx.try_global::<UpdateService>()
        .map(|service| service.0.clone())
}

/// The menu command meets the update where it is instead of starting over.
fn check_for_updates(cx: &mut App) {
    let Some(updates) = service(cx) else {
        present_in_front_window(cx, |window, cx| {
            present_failure(UpdateError::Unavailable, window, cx)
        });
        return;
    };
    let state = updates.read(cx).state().clone();
    match state {
        UpdateState::Unavailable => present_in_front_window(cx, |window, cx| {
            present_failure(UpdateError::Unavailable, window, cx)
        }),
        UpdateState::Idle | UpdateState::UpToDate | UpdateState::Failed { .. } => {
            update_results(CheckResults::request, cx);
            updates.update(cx, |updates, cx| updates.check(true, cx));
        }
        // The result arrives as the same alert either way; a second command adds nothing.
        UpdateState::Checking => {
            update_results(CheckResults::request, cx);
        }
        UpdateState::Available { version } => present_in_front_window(cx, move |window, cx| {
            present_available(&version, window, cx)
        }),
        UpdateState::Downloading { version, .. } | UpdateState::Verifying { version } => {
            if !updates.read(cx).is_cancelling() {
                present_in_front_window(cx, move |window, cx| {
                    present_stop_download(&version, window, cx)
                });
            }
        }
        UpdateState::Ready { .. } => request_install(&updates, InstallRequest::FrontWorkspace, cx),
        UpdateState::Installing { version } => present_in_front_window(cx, move |window, cx| {
            present_finish_install(&version, window, cx)
        }),
    }
}

fn request_install(updates: &Entity<ApplicationUpdates>, request: InstallRequest, cx: &mut App) {
    if cx.has_global::<UpdatePrompts>() {
        cx.update_global::<UpdatePrompts, _>(|prompts, _| {
            prompts.install_requested = Some(request);
        });
    }
    updates.update(cx, |updates, cx| updates.request_install_confirmation(cx));
}

/// Asks to install from a window that presents the confirmation itself.
pub(crate) fn request_install_from(window: AnyWindowHandle, cx: &mut App) {
    if let Some(updates) = service(cx) {
        request_install(&updates, InstallRequest::Window(window), cx);
    }
}

fn receive_notice(notice: UpdateNotice, cx: &mut App) {
    let Some(updates) = service(cx) else { return };
    match notice {
        UpdateNotice::CheckFinished | UpdateNotice::Failed => {
            let state = updates.read(cx).state().clone();
            if let Some(result) =
                update_results(|results| results.notice(notice, &state), cx).flatten()
            {
                present_result(result, cx);
            }
        }
        UpdateNotice::ReadyToInstall => {
            let request = take_install_request(cx);
            // An unrequested confirmation waits for the button rather than stacking behind
            // another decision. The Updates menu keeps the ready step prominent until the user acts.
            let requested = request.is_some();
            if let Some(InstallRequest::Window(handle)) = request
                && let Some(settings) = handle.downcast::<SettingsWindow>()
                && settings
                    .update(cx, |_, window, cx| present_install(window, cx))
                    .is_ok()
            {
                return;
            }
            let Some(handle) = front_workspace_window(cx) else {
                return;
            };
            let _ = handle.update(cx, |_, window, cx| {
                if requested || !spaceterm_ui::window_modal_is_open(window, cx) {
                    present_install(window, cx);
                }
            });
        }
    }
}

/// A scheduled check emits no notice, so a menu check that joined it reports from the state.
fn observe_state(updates: &Entity<ApplicationUpdates>, cx: &mut App) {
    let state = updates.read(cx).state().clone();
    if let Some(result) = update_results(|results| results.observe(&state), cx).flatten() {
        present_result(result, cx);
    }
}

fn present_result(state: UpdateState, cx: &mut App) {
    match state {
        UpdateState::UpToDate => present_in_front_window(cx, present_up_to_date),
        UpdateState::Available { version } => present_in_front_window(cx, move |window, cx| {
            present_available(&version, window, cx)
        }),
        UpdateState::Failed { error } => {
            present_in_front_window(cx, move |window, cx| present_failure(error, window, cx))
        }
        _ => {}
    }
}

/// Prompts belong to the Workspace window the user is looking at.
pub(crate) fn front_workspace_window(cx: &App) -> Option<WindowHandle<WorkspaceManager>> {
    cx.active_window()
        .and_then(|window| window.downcast::<WorkspaceManager>())
        .or_else(|| {
            cx.window_stack()
                .into_iter()
                .flatten()
                .find_map(|window| window.downcast::<WorkspaceManager>())
        })
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<WorkspaceManager>())
        })
}

fn present_in_front_window(
    cx: &mut App,
    present: impl FnOnce(&mut Window, &mut Context<WorkspaceManager>) + 'static,
) {
    let Some(handle) = front_workspace_window(cx) else {
        return;
    };
    let _ = handle.update(cx, |_, window, cx| present(window, cx));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PromptAction {
    Acknowledge,
    Download,
    Later,
    ReleaseNotes,
    TryAgain,
    StopDownload,
    KeepDownloading,
    Restart,
}

fn action(
    id: PromptAction,
    label: &'static str,
    role: ModalActionRole,
    selector: &'static str,
) -> ModalAction<PromptAction> {
    ModalAction::new(id, label, role, selector)
}

fn present<T: 'static>(
    alert: Alert<PromptAction>,
    window: &Window,
    cx: &mut Context<T>,
    on_result: impl FnOnce(Option<PromptAction>, &mut App) + 'static,
) -> bool {
    let result = alert.present(window, cx, move |outcome, cx| {
        on_result(
            match outcome {
                AlertOutcome::Activated { action_id, .. } => Some(action_id),
                AlertOutcome::Dismissed { .. } => None,
            },
            cx,
        );
    });
    if result.is_err() {
        eprintln!("failed to present an update prompt");
    }
    result.is_ok()
}

fn up_to_date_alert() -> Alert<PromptAction> {
    Alert::new(
        ModalId::new("update-up-to-date"),
        "SpaceTerm is up to date",
        "SpaceTerm Is Up to Date",
        format!("Version {CURRENT_VERSION} is the latest release."),
        vec![action(
            PromptAction::Acknowledge,
            "OK",
            ModalActionRole::Cancel,
            "update-up-to-date-ok",
        )],
    )
}

fn present_up_to_date<T: 'static>(window: &mut Window, cx: &mut Context<T>) {
    present(up_to_date_alert(), window, cx, |_, _| {});
}

fn available_alert(version: &str) -> Alert<PromptAction> {
    Alert::new(
        ModalId::new("update-available"),
        "Update available",
        format!("SpaceTerm {version} Is Available"),
        format!("You have version {CURRENT_VERSION}."),
        vec![
            action(
                PromptAction::Download,
                "Download",
                ModalActionRole::Affirmative,
                "update-available-download",
            )
            .with_emphasis(ModalActionEmphasis::Prominent)
            .default_action(true),
            action(
                PromptAction::ReleaseNotes,
                RELEASE_NOTES_TITLE,
                ModalActionRole::Auxiliary,
                "update-available-release-notes",
            ),
            action(
                PromptAction::Later,
                "Later",
                ModalActionRole::Cancel,
                "update-available-later",
            ),
        ],
    )
}

fn present_available<T: 'static>(version: &str, window: &mut Window, cx: &mut Context<T>) {
    present(
        available_alert(version),
        window,
        cx,
        |action, cx| match action {
            Some(PromptAction::Download) => {
                if let Some(updates) = service(cx) {
                    updates.update(cx, |updates, cx| updates.download(cx));
                }
            }
            Some(PromptAction::ReleaseNotes) => cx.open_url(RELEASE_NOTES_URL),
            Some(PromptAction::Later) => put_off(cx),
            _ => {}
        },
    );
}

/// Choosing Later quiets the reminder until it comes due again. The update stays pending and the
/// Updates menu keeps offering it.
fn put_off(cx: &mut App) {
    if let Some(updates) = service(cx) {
        updates.update(cx, |updates, cx| updates.dismiss_reminder(cx));
    }
}

/// Only the read-only failure names a remedy, which the desktop wording supplies.
pub(crate) fn failure_message(error: UpdateError, wording: DesktopWording) -> String {
    match error {
        UpdateError::ReadOnly => wording.read_only_update.1.to_owned(),
        error => error.to_string(),
    }
}

fn failure_alert(error: UpdateError, wording: DesktopWording) -> Alert<PromptAction> {
    let (title, retry, intent) = match error {
        UpdateError::Unavailable => ("Updates Unavailable", false, AlertIntent::Informational),
        UpdateError::Check => ("Couldn’t Check for Updates", true, AlertIntent::Warning),
        UpdateError::Download => (
            "Couldn\u{2019}t Download the Update",
            true,
            AlertIntent::Warning,
        ),
        UpdateError::Verification => (
            "Couldn\u{2019}t Verify the Update",
            true,
            AlertIntent::Warning,
        ),
        UpdateError::Installation => (
            "Couldn\u{2019}t Install the Update",
            true,
            AlertIntent::Warning,
        ),
        UpdateError::ReadOnly => (wording.read_only_update.0, false, AlertIntent::Warning),
    };
    let mut actions = Vec::with_capacity(2);
    if retry {
        actions.push(action(
            PromptAction::TryAgain,
            "Try Again",
            ModalActionRole::Affirmative,
            "update-failure-try-again",
        ));
    }
    actions.push(action(
        PromptAction::Acknowledge,
        "OK",
        ModalActionRole::Cancel,
        "update-failure-ok",
    ));
    Alert::new(
        ModalId::new("update-failure"),
        title,
        title,
        failure_message(error, wording),
        actions,
    )
    .intent(intent)
}

fn present_failure<T: 'static>(error: UpdateError, window: &mut Window, cx: &mut Context<T>) {
    let wording = DesktopPresentation::get(cx).wording();
    present(failure_alert(error, wording), window, cx, |action, cx| {
        if action == Some(PromptAction::TryAgain) {
            check_for_updates(cx);
        }
    });
}

fn stop_download_alert(version: &str) -> Alert<PromptAction> {
    Alert::new(
        ModalId::new("update-stop-download"),
        "Stop update download",
        format!("Stop Downloading SpaceTerm {version}?"),
        "You can download it again later.",
        vec![
            action(
                PromptAction::StopDownload,
                "Stop Download",
                ModalActionRole::Affirmative,
                "update-stop-download-stop",
            )
            .with_intent(ModalActionIntent::Destructive),
            action(
                PromptAction::KeepDownloading,
                "Keep Downloading",
                ModalActionRole::Cancel,
                "update-stop-download-keep",
            ),
        ],
    )
}

fn present_stop_download<T: 'static>(version: &str, window: &mut Window, cx: &mut Context<T>) {
    present(stop_download_alert(version), window, cx, |action, cx| {
        if action == Some(PromptAction::StopDownload)
            && let Some(updates) = service(cx)
        {
            updates.update(cx, |updates, cx| updates.cancel(cx));
        }
    });
}

fn install_alert(version: &str) -> Alert<PromptAction> {
    Alert::new(
        ModalId::new("update-install"),
        "Install update",
        format!("Restart to Install SpaceTerm {version}?"),
        "SpaceTerm quits, installs the update, and opens again. Your Terminal Sessions keep running.",
        vec![
            action(
                PromptAction::Restart,
                "Restart and Install",
                ModalActionRole::Affirmative,
                "update-install-restart",
            )
            .with_emphasis(ModalActionEmphasis::Prominent)
            .default_action(true),
            action(
                PromptAction::ReleaseNotes,
                RELEASE_NOTES_TITLE,
                ModalActionRole::Auxiliary,
                "update-install-release-notes",
            ),
            action(
                PromptAction::Later,
                "Later",
                ModalActionRole::Cancel,
                "update-install-later",
            ),
        ],
    )
}

/// Claims the one open confirmation and settles it on every path, including window removal.
fn present_install<T: 'static>(window: &mut Window, cx: &mut Context<T>) {
    let Some(updates) = service(cx) else { return };
    let Some(confirmation) = updates.update(cx, |updates, _| updates.begin_install_confirmation())
    else {
        return;
    };
    let unpresented = confirmation.clone();
    let settle = updates.clone();
    let presented = present(
        install_alert(&confirmation.version),
        window,
        cx,
        move |action, cx| {
            let accepted = action == Some(PromptAction::Restart);
            settle.update(cx, |updates, cx| {
                updates.finish_install_confirmation(confirmation, accepted, cx)
            });
            match action {
                Some(PromptAction::ReleaseNotes) => cx.open_url(RELEASE_NOTES_URL),
                Some(PromptAction::Later) => put_off(cx),
                _ => {}
            }
        },
    );
    if !presented {
        updates.update(cx, |updates, cx| {
            updates.finish_install_confirmation(unpresented, false, cx)
        });
    }
}

fn finish_install_alert(version: &str) -> Alert<PromptAction> {
    Alert::new(
        ModalId::new("update-finish-install"),
        "Finish installing update",
        format!("Restart to Install SpaceTerm {version}?"),
        "The update installs when SpaceTerm quits.",
        vec![
            action(
                PromptAction::Restart,
                "Restart",
                ModalActionRole::Affirmative,
                "update-finish-install-restart",
            )
            .with_emphasis(ModalActionEmphasis::Prominent)
            .default_action(true),
            action(
                PromptAction::Later,
                "Later",
                ModalActionRole::Cancel,
                "update-finish-install-later",
            ),
        ],
    )
}

fn present_finish_install<T: 'static>(version: &str, window: &mut Window, cx: &mut Context<T>) {
    present(
        finish_install_alert(version),
        window,
        cx,
        |action, cx| match action {
            Some(PromptAction::Restart) => retry_install(cx),
            Some(PromptAction::Later) => put_off(cx),
            _ => {}
        },
    );
}

fn retry_install(cx: &mut App) {
    if let Some(updates) = service(cx) {
        updates.update(cx, |updates, cx| updates.retry_install(cx));
    }
}

/// The glyph that leads the update pill.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ControlGlyph {
    Download,
    Progress(f32),
    Activity,
    Restart,
    Warning,
}

/// How far the pill reaches for attention after the user has put an update off.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlReminder {
    /// The pill takes the primary paint even where it would otherwise stay quiet.
    Gentle,
    /// The pill takes the warning paint and glyph, and the status says the update is overdue.
    Overdue,
}

/// What activating the client's pill or command row does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlCommand {
    Download,
    RetryDownload,
    OfferStop,
    RequestInstall,
    RetryInstall,
}

/// One frame of the client's pending update in the pill and the Updates menu. It has no dismiss
/// affordance and stays while an update is pending.
#[derive(Clone, Debug, PartialEq)]
struct ControlPresentation {
    /// Progress or a problem, shown as a status row above the command.
    status: Option<SharedString>,
    overdue_failure: Option<SharedString>,
    /// The command row's title, or the state itself while no command applies.
    label: SharedString,
    /// The pill's compact step and version.
    pill: SharedString,
    glyph: ControlGlyph,
    prominent: bool,
    command: Option<ControlCommand>,
    reminder: Option<ControlReminder>,
}

impl ControlPresentation {
    /// `pending_version` is the release the service still knows about while its state has moved
    /// on to a check, a failure, or idle. Without one there is nothing pending to show.
    fn resolve(
        state: &UpdateState,
        pending_version: Option<&str>,
        cancelling: bool,
        wording: DesktopWording,
    ) -> Option<Self> {
        let control = |step: &str,
                       version: &str,
                       status: Option<String>,
                       label: String,
                       glyph,
                       prominent,
                       command| {
            Self {
                status: status.map(SharedString::from),
                overdue_failure: match state {
                    UpdateState::Failed { error } => Some(
                        match error {
                            UpdateError::ReadOnly => wording.overdue_read_only_update,
                            UpdateError::Check => {
                                "Update overdue. Couldn\u{2019}t check for updates."
                            }
                            UpdateError::Download => "Update overdue. Download failed. Try again.",
                            UpdateError::Installation => {
                                "Update overdue. Install failed. Try again."
                            }
                            UpdateError::Verification => {
                                "Update overdue. Couldn\u{2019}t verify the update."
                            }
                            UpdateError::Unavailable => {
                                "Update overdue. Use the installed release to update."
                            }
                        }
                        .into(),
                    ),
                    _ => None,
                },
                label: label.into(),
                pill: format!("{step} {version}").into(),
                glyph,
                prominent,
                command,
                reminder: None,
            }
        };
        const STOP: &str = "Stop Download…";
        let presentation = match state {
            UpdateState::Unavailable | UpdateState::UpToDate => return None,
            UpdateState::Idle => {
                let version = pending_version?;
                control(
                    "Download",
                    version,
                    None,
                    format!("Download SpaceTerm {version}"),
                    ControlGlyph::Download,
                    false,
                    Some(ControlCommand::RetryDownload),
                )
            }
            UpdateState::Checking => {
                let version = pending_version?;
                control(
                    "Checking",
                    version,
                    None,
                    format!("Checking for SpaceTerm {version}"),
                    ControlGlyph::Activity,
                    false,
                    None,
                )
            }
            UpdateState::Failed { error } => {
                let version = pending_version?;
                control(
                    "Try Again",
                    version,
                    Some(failure_message(*error, wording)),
                    format!("Download SpaceTerm {version} Again"),
                    ControlGlyph::Warning,
                    false,
                    Some(ControlCommand::RetryDownload),
                )
            }
            UpdateState::Available { version } => control(
                "Download",
                version,
                None,
                format!("Download SpaceTerm {version}"),
                ControlGlyph::Download,
                true,
                Some(ControlCommand::Download),
            ),
            UpdateState::Downloading { version, .. } | UpdateState::Verifying { version }
                if cancelling =>
            {
                control(
                    "Stopping",
                    version,
                    None,
                    format!("Stopping the Download of SpaceTerm {version}"),
                    ControlGlyph::Activity,
                    false,
                    None,
                )
            }
            UpdateState::Downloading {
                version,
                received,
                total,
            } if *total > 0 => {
                let fraction = (*received as f64 / *total as f64).clamp(0.0, 1.0);
                control(
                    "Downloading",
                    version,
                    Some(format!(
                        "Downloading SpaceTerm {version}, {}%",
                        (fraction * 100.0).floor()
                    )),
                    STOP.to_owned(),
                    ControlGlyph::Progress(fraction as f32),
                    false,
                    Some(ControlCommand::OfferStop),
                )
            }
            UpdateState::Downloading { version, .. } => control(
                "Downloading",
                version,
                Some(format!("Downloading SpaceTerm {version}")),
                STOP.to_owned(),
                ControlGlyph::Activity,
                false,
                Some(ControlCommand::OfferStop),
            ),
            // Verification finishes the download from the user's point of view.
            UpdateState::Verifying { version } => control(
                "Downloading",
                version,
                Some(format!("Verifying SpaceTerm {version}")),
                STOP.to_owned(),
                ControlGlyph::Activity,
                false,
                Some(ControlCommand::OfferStop),
            ),
            // The confirmation follows, so the title ends in an ellipsis.
            UpdateState::Ready { version } => control(
                "Install",
                version,
                None,
                format!("Restart to Install SpaceTerm {version}…"),
                ControlGlyph::Restart,
                true,
                Some(ControlCommand::RequestInstall),
            ),
            // The install was confirmed. Reaching this state visibly means the quit was cancelled.
            UpdateState::Installing { version } => control(
                "Installing",
                version,
                None,
                format!("Restart to Install SpaceTerm {version}"),
                ControlGlyph::Restart,
                true,
                Some(ControlCommand::RetryInstall),
            ),
        };
        Some(presentation)
    }

    /// Applies the service's pending reminder. A reminder speaks only when the next step is the
    /// user's: while the service works there is nothing to ask for.
    fn remind(mut self, reminder: Option<UpdateStage>) -> Self {
        let reminder = match reminder {
            Some(UpdateStage::Warning) => ControlReminder::Gentle,
            Some(UpdateStage::Overdue) => ControlReminder::Overdue,
            Some(UpdateStage::Optional) | None => return self,
        };
        if matches!(self.command, Some(ControlCommand::OfferStop) | None) {
            return self;
        }
        // A failed step leads with a warning glyph and stays neutral until the update is overdue.
        let failed = self.glyph == ControlGlyph::Warning;
        self.prominent = !(failed && reminder == ControlReminder::Gentle);
        if reminder == ControlReminder::Overdue {
            self.glyph = ControlGlyph::Warning;
            self.status = self.overdue_failure.clone().or_else(|| {
                Some(match &self.status {
                    Some(status) => format!("This update is overdue. {status}").into(),
                    None => "This update is overdue.".into(),
                })
            });
        }
        self.reminder = Some(reminder);
        self
    }
}

/// The paint family of the pill. Each tone resolves to theme tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PillTone {
    /// An action the user should take now.
    Primary,
    /// Progress or information.
    Neutral,
    /// An overdue update or a server that needs attention.
    Warning,
}

/// What activating the pill's leading part does.
#[derive(Clone, Debug, Eq, PartialEq)]
enum PillCommand {
    /// The client's step, or nothing while the client's update works on its own.
    Client(Option<ControlCommand>),
    /// One server's existing Settings journey.
    Server(ServerUpdateAction),
    /// The pill has no leading part, so it opens the Updates menu.
    Menu,
}

/// The title bar update pill. It appears only while something asks for action, and its chevron
/// opens the Updates menu, which keeps every row.
#[derive(Clone, Debug, PartialEq)]
enum UpdatePill {
    /// The client's step leads. `servers` counts servers that also ask for action, and `offered`
    /// records whether one of them offers a verified update.
    Client {
        step: ControlPresentation,
        servers: usize,
        offered: bool,
    },
    /// One server's update or attention leads.
    Server {
        name: SharedString,
        detail: SharedString,
        action: ServerUpdateAction,
    },
    /// Several servers ask for action. Updates happen one server at a time, so the menu chooses.
    /// `offered` records whether one of them offers a verified update.
    Servers { count: usize, offered: bool },
}

impl UpdatePill {
    /// Servers ask for action only through typed needs, never through offline or unknown status.
    fn resolve(client: Option<ControlPresentation>, servers: &[ServerUpdateRow]) -> Option<Self> {
        let mut asking = servers.iter().filter_map(|row| {
            row.action
                .as_ref()
                .filter(|action| action.need().is_some())
                .map(|action| (row, action))
        });
        let count = asking.clone().count();
        let offered = asking
            .clone()
            .any(|(_, action)| action.need() == Some(ServerNeed::Update));
        if let Some(step) = client {
            return Some(Self::Client {
                step,
                servers: count,
                offered,
            });
        }
        match count {
            0 => None,
            1 => asking.next().map(|(row, action)| Self::Server {
                name: row.label.clone(),
                detail: row.detail.clone(),
                action: action.clone(),
            }),
            count => Some(Self::Servers { count, offered }),
        }
    }

    fn label(&self) -> SharedString {
        match self {
            Self::Client { step, .. } => step.pill.clone(),
            Self::Server { action, .. } => match action.need() {
                Some(ServerNeed::Attention) => "Server Needs Attention".into(),
                _ => "Update Server".into(),
            },
            Self::Servers { count, .. } => format!("Review {count} Servers").into(),
        }
    }

    /// The full sentence behind the compact label, for the tooltip and assistive technology.
    fn description(&self) -> SharedString {
        match self {
            Self::Client { step, servers, .. } => {
                let step = step.status.clone().unwrap_or_else(|| step.label.clone());
                match servers {
                    0 => step,
                    1 => format!("{step}. A server needs action in Updates.").into(),
                    count => format!("{step}. {count} servers need action in Updates.").into(),
                }
            }
            Self::Server { name, detail, .. } => format!("{name}: {detail}").into(),
            Self::Servers { count, .. } => format!("{count} servers need action").into(),
        }
    }

    /// Warnings lead. Otherwise a prominent client step or a verified server update offer is the
    /// action to take now, even behind the client's progress or retry.
    fn tone(&self) -> PillTone {
        match self {
            Self::Client { step, .. } if step.reminder == Some(ControlReminder::Overdue) => {
                PillTone::Warning
            }
            Self::Client { step, offered, .. } if step.prominent || *offered => PillTone::Primary,
            Self::Server { action, .. } if action.need() == Some(ServerNeed::Attention) => {
                PillTone::Warning
            }
            Self::Server { .. } | Self::Servers { offered: true, .. } => PillTone::Primary,
            _ => PillTone::Neutral,
        }
    }

    fn glyph(&self) -> Option<ControlGlyph> {
        match self {
            Self::Client { step, .. } => Some(step.glyph),
            Self::Server { action, .. } if action.need() == Some(ServerNeed::Attention) => {
                Some(ControlGlyph::Warning)
            }
            _ => None,
        }
    }

    fn command(&self) -> PillCommand {
        match self {
            Self::Client { step, .. } => PillCommand::Client(step.command),
            Self::Server { action, .. } => PillCommand::Server(action.clone()),
            Self::Servers { .. } => PillCommand::Menu,
        }
    }

    /// Whether the chevron carries a dot for servers waiting behind the client's step.
    fn marked(&self) -> bool {
        matches!(self, Self::Client { servers, .. } if *servers > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{Appearance, builtin_chrome_base};
    use crate::ui::ServerTarget;

    mod offer_tests;

    const PENDING: Option<&str> = Some("0.4.2");

    fn resolve(state: UpdateState) -> Option<ControlPresentation> {
        ControlPresentation::resolve(&state, PENDING, false, testing_wording())
    }

    fn testing_wording() -> DesktopWording {
        crate::desktop_profile::testing_presentation().wording()
    }

    fn version() -> String {
        "0.4.2".to_owned()
    }

    /// Neutral and disabled progress glyphs keep the installed accent, which must remain readable
    /// on every ordinary fill. Enabled Primary pills inherit the button foreground instead.
    #[test]
    fn the_progress_ring_stays_readable_on_every_ordinary_button_fill() {
        const MINIMUM_GRAPHIC_CONTRAST: f64 = 3.0;
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = builtin_chrome_base(appearance);
            for fill in [
                colors.element_background,
                colors.element_hover,
                colors.element_active,
                colors.element_disabled,
            ] {
                let fill = fill.source_over(colors.title_bar_background);
                let contrast = colors
                    .progress_indicator
                    .source_over(fill)
                    .contrast_ratio(fill);
                assert!(
                    contrast >= MINIMUM_GRAPHIC_CONTRAST,
                    "{appearance:?}: {contrast:.2}"
                );
            }
        }
    }

    #[test]
    fn control_should_appear_only_while_an_update_is_pending() {
        for state in [
            UpdateState::Unavailable,
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Failed {
                error: UpdateError::Check,
            },
        ] {
            assert_eq!(
                ControlPresentation::resolve(&state, None, false, testing_wording()),
                None,
                "{state:?}"
            );
        }
        assert_eq!(resolve(UpdateState::UpToDate), None);
    }

    #[test]
    fn control_should_name_the_step_and_version_and_emphasize_only_the_users_turn() {
        let cases = [
            (
                UpdateState::Available { version: version() },
                "Download SpaceTerm 0.4.2",
                ControlGlyph::Download,
                true,
                Some(ControlCommand::Download),
            ),
            (
                UpdateState::Downloading {
                    version: version(),
                    received: 1,
                    total: 4,
                },
                "Stop Download…",
                ControlGlyph::Progress(0.25),
                false,
                Some(ControlCommand::OfferStop),
            ),
            (
                UpdateState::Downloading {
                    version: version(),
                    received: 0,
                    total: 0,
                },
                "Stop Download…",
                ControlGlyph::Activity,
                false,
                Some(ControlCommand::OfferStop),
            ),
            (
                UpdateState::Verifying { version: version() },
                "Stop Download…",
                ControlGlyph::Activity,
                false,
                Some(ControlCommand::OfferStop),
            ),
            (
                UpdateState::Ready { version: version() },
                "Restart to Install SpaceTerm 0.4.2…",
                ControlGlyph::Restart,
                true,
                Some(ControlCommand::RequestInstall),
            ),
            (
                UpdateState::Installing { version: version() },
                "Restart to Install SpaceTerm 0.4.2",
                ControlGlyph::Restart,
                true,
                Some(ControlCommand::RetryInstall),
            ),
            // A known pending update outlives a stopped download, a recheck, and a failure.
            (
                UpdateState::Idle,
                "Download SpaceTerm 0.4.2",
                ControlGlyph::Download,
                false,
                Some(ControlCommand::RetryDownload),
            ),
            (
                UpdateState::Checking,
                "Checking for SpaceTerm 0.4.2",
                ControlGlyph::Activity,
                false,
                None,
            ),
            (
                UpdateState::Failed {
                    error: UpdateError::Check,
                },
                "Download SpaceTerm 0.4.2 Again",
                ControlGlyph::Warning,
                false,
                Some(ControlCommand::RetryDownload),
            ),
            (
                UpdateState::Failed {
                    error: UpdateError::Installation,
                },
                "Download SpaceTerm 0.4.2 Again",
                ControlGlyph::Warning,
                false,
                Some(ControlCommand::RetryDownload),
            ),
        ];
        for (state, label, glyph, prominent, command) in cases {
            let presentation = resolve(state.clone()).expect("a pending update shows the control");
            assert_eq!(presentation.label.as_ref(), label, "{state:?}");
            assert_eq!(presentation.glyph, glyph, "{state:?}");
            assert_eq!(presentation.prominent, prominent, "{state:?}");
            assert_eq!(presentation.command, command, "{state:?}");
        }
    }

    fn server(name: &str, action: ServerUpdateAction) -> ServerUpdateRow {
        ServerUpdateRow {
            label: format!("{name} · account 501").into(),
            detail: "status".into(),
            action: Some(action),
        }
    }

    /// Offline, unknown, unauthorized, and up-to-date servers keep their rows in the menu only.
    fn quiet_servers() -> Vec<ServerUpdateRow> {
        vec![
            server("local", ServerUpdateAction::Settings(ServerTarget::Local)),
            server(
                "offline",
                ServerUpdateAction::Settings(ServerTarget::Direct([6; 32])),
            ),
            server(
                "paired",
                ServerUpdateAction::Settings(ServerTarget::Direct([7; 32])),
            ),
        ]
    }

    fn pill(
        client: Option<UpdateState>,
        reminder: Option<UpdateStage>,
        servers: &[ServerUpdateRow],
    ) -> Option<(String, PillTone, Option<ControlGlyph>, PillCommand)> {
        let client = client
            .and_then(resolve)
            .map(|presentation| presentation.remind(reminder));
        UpdatePill::resolve(client, servers).map(|pill| {
            (
                pill.label().to_string(),
                pill.tone(),
                pill.glyph(),
                pill.command(),
            )
        })
    }

    #[test]
    fn pill_should_hide_until_the_client_or_a_server_asks_for_action() {
        assert_eq!(pill(None, None, &quiet_servers()), None);
        assert_eq!(pill(Some(UpdateState::UpToDate), None, &[]), None);
    }

    #[test]
    fn pill_should_name_the_clients_next_step_in_its_tone() {
        let downloading = UpdateState::Downloading {
            version: version(),
            received: 1,
            total: 4,
        };
        let cases = [
            (
                UpdateState::Available { version: version() },
                None,
                "Download 0.4.2",
                PillTone::Primary,
                ControlGlyph::Download,
                Some(ControlCommand::Download),
            ),
            (
                downloading,
                None,
                "Downloading 0.4.2",
                PillTone::Neutral,
                ControlGlyph::Progress(0.25),
                Some(ControlCommand::OfferStop),
            ),
            (
                UpdateState::Verifying { version: version() },
                None,
                "Downloading 0.4.2",
                PillTone::Neutral,
                ControlGlyph::Activity,
                Some(ControlCommand::OfferStop),
            ),
            (
                UpdateState::Ready { version: version() },
                None,
                "Install 0.4.2",
                PillTone::Primary,
                ControlGlyph::Restart,
                Some(ControlCommand::RequestInstall),
            ),
            (
                UpdateState::Ready { version: version() },
                Some(UpdateStage::Overdue),
                "Install 0.4.2",
                PillTone::Warning,
                ControlGlyph::Warning,
                Some(ControlCommand::RequestInstall),
            ),
            // An overdue update that is not downloaded yet still names the step that works.
            (
                UpdateState::Available { version: version() },
                Some(UpdateStage::Overdue),
                "Download 0.4.2",
                PillTone::Warning,
                ControlGlyph::Warning,
                Some(ControlCommand::Download),
            ),
            (
                UpdateState::Failed {
                    error: UpdateError::Download,
                },
                None,
                "Try Again 0.4.2",
                PillTone::Neutral,
                ControlGlyph::Warning,
                Some(ControlCommand::RetryDownload),
            ), // A gentle reminder raises a waiting step but leaves a failed one neutral.
            (
                UpdateState::Failed {
                    error: UpdateError::Download,
                },
                Some(UpdateStage::Warning),
                "Try Again 0.4.2",
                PillTone::Neutral,
                ControlGlyph::Warning,
                Some(ControlCommand::RetryDownload),
            ),
        ];
        for (state, reminder, label, tone, glyph, command) in cases {
            assert_eq!(
                pill(Some(state.clone()), reminder, &quiet_servers()),
                Some((
                    label.to_owned(),
                    tone,
                    Some(glyph),
                    PillCommand::Client(command)
                )),
                "{state:?} {reminder:?}"
            );
        }
    }

    #[test]
    fn pill_should_lead_with_one_server_and_open_the_menu_for_several() {
        let update = server("local", ServerUpdateAction::Local);
        let attention = server(
            "penguin",
            ServerUpdateAction::Review(ServerTarget::Direct([3; 32])),
        );
        let with = |row: &ServerUpdateRow| {
            let mut servers = quiet_servers();
            servers.push(row.clone());
            servers
        };

        assert_eq!(
            pill(None, None, &with(&update)),
            Some((
                "Update Server".to_owned(),
                PillTone::Primary,
                None,
                PillCommand::Server(ServerUpdateAction::Local)
            ))
        );
        assert_eq!(
            pill(None, None, &with(&attention)),
            Some((
                "Server Needs Attention".to_owned(),
                PillTone::Warning,
                Some(ControlGlyph::Warning),
                PillCommand::Server(ServerUpdateAction::Review(ServerTarget::Direct([3; 32])))
            ))
        );
        let mut both = with(&update);
        both.push(attention.clone());
        assert_eq!(
            pill(None, None, &both),
            Some((
                "Review 2 Servers".to_owned(),
                PillTone::Primary,
                None,
                PillCommand::Menu
            ))
        );

        // The client's step leads, and the chevron marks the servers waiting in the menu.
        let ready = UpdateState::Ready { version: version() };
        let client = |servers: &[ServerUpdateRow]| {
            UpdatePill::resolve(resolve(ready.clone()), servers).map(|pill| pill.marked())
        };
        assert_eq!(client(&both), Some(true));
        assert_eq!(client(&quiet_servers()), Some(false));
        assert_eq!(
            pill(Some(ready.clone()), None, &both).map(|pill| pill.0),
            Some("Install 0.4.2".to_owned())
        );
    }

    #[test]
    fn stopping_update_should_block_further_commands() {
        for state in [
            UpdateState::Downloading {
                version: version(),
                received: 1,
                total: 2,
            },
            UpdateState::Verifying { version: version() },
        ] {
            let presentation =
                ControlPresentation::resolve(&state, PENDING, true, testing_wording())
                    .expect("a stopping update stays visible until the service settles");
            assert_eq!(
                presentation.label.as_ref(),
                "Stopping the Download of SpaceTerm 0.4.2"
            );
            assert_eq!(presentation.command, None);
            assert_eq!(presentation.glyph, ControlGlyph::Activity);
        }
    }

    /// Delivers one service change as GPUI may: observers first, or the notice first.
    fn deliver(
        results: &mut CheckResults,
        state: &UpdateState,
        notice: Option<UpdateNotice>,
        observer_first: bool,
    ) -> Vec<UpdateState> {
        let mut presented = Vec::new();
        for observer in [observer_first, !observer_first] {
            if observer {
                presented.extend(results.observe(state));
            } else if let Some(notice) = notice {
                presented.extend(results.notice(notice, state));
            }
        }
        presented
    }

    #[test]
    fn check_result_should_be_presented_once_in_either_delivery_order() {
        let up_to_date = UpdateState::UpToDate;
        let rejected = UpdateState::Failed {
            error: UpdateError::Verification,
        };
        let download_failed = UpdateState::Failed {
            error: UpdateError::Download,
        };
        for observer_first in [true, false] {
            let order = if observer_first {
                "observer first"
            } else {
                "notice first"
            };

            // A menu check the service started emits a notice with its result.
            let mut results = CheckResults::default();
            results.request();
            assert_eq!(
                deliver(&mut results, &UpdateState::Checking, None, observer_first),
                []
            );
            assert_eq!(
                deliver(
                    &mut results,
                    &up_to_date,
                    Some(UpdateNotice::CheckFinished),
                    observer_first
                ),
                std::slice::from_ref(&up_to_date),
                "{order}"
            );

            // A menu check that joined a scheduled check hears only a rejected release.
            let mut results = CheckResults::default();
            assert_eq!(
                deliver(&mut results, &UpdateState::Checking, None, observer_first),
                []
            );
            results.request();
            assert_eq!(
                deliver(
                    &mut results,
                    &rejected,
                    Some(UpdateNotice::Failed),
                    observer_first
                ),
                std::slice::from_ref(&rejected),
                "{order}"
            );
            let mut results = CheckResults::default();
            results.request();
            assert_eq!(
                deliver(&mut results, &up_to_date, None, observer_first),
                std::slice::from_ref(&up_to_date),
                "{order}"
            );

            // A download failure interrupts the user once even though no check awaits it.
            let available = UpdateState::Available {
                version: "0.4.2".to_owned(),
            };
            let mut results = CheckResults::default();
            results.request();
            assert_eq!(
                deliver(
                    &mut results,
                    &available,
                    Some(UpdateNotice::CheckFinished),
                    observer_first
                ),
                std::slice::from_ref(&available),
                "{order}"
            );
            assert_eq!(
                deliver(
                    &mut results,
                    &download_failed,
                    Some(UpdateNotice::Failed),
                    observer_first
                ),
                std::slice::from_ref(&download_failed),
                "{order}"
            );

            // Scheduled results stay silent.
            let mut results = CheckResults::default();
            assert_eq!(deliver(&mut results, &up_to_date, None, observer_first), []);
        }
    }

    #[test]
    fn reminder_should_raise_the_users_step_and_warn_only_when_overdue() {
        let remind = |state: UpdateState, stage| {
            resolve(state)
                .expect("a pending update shows the control")
                .remind(stage)
        };

        let quiet = remind(UpdateState::Idle, None);
        assert_eq!((quiet.prominent, quiet.reminder), (false, None));
        let optional = remind(UpdateState::Idle, Some(UpdateStage::Optional));
        assert_eq!(optional, quiet);

        let gentle = remind(UpdateState::Idle, Some(UpdateStage::Warning));
        assert_eq!(gentle.label.as_ref(), "Download SpaceTerm 0.4.2");
        assert_eq!(gentle.reminder, Some(ControlReminder::Gentle));
        assert!(gentle.prominent);
        assert_eq!(gentle.glyph, ControlGlyph::Download);

        let overdue = remind(
            UpdateState::Ready { version: version() },
            Some(UpdateStage::Overdue),
        );
        assert!(overdue.prominent);
        assert_eq!(
            overdue.status.as_ref().map(AsRef::as_ref),
            Some("This update is overdue.")
        );
        assert_eq!(
            overdue.label.as_ref(),
            "Restart to Install SpaceTerm 0.4.2…"
        );
        assert_eq!(overdue.reminder, Some(ControlReminder::Overdue));
        assert_eq!(overdue.glyph, ControlGlyph::Warning);
        assert_eq!(overdue.command, Some(ControlCommand::RequestInstall));
    }

    #[test]
    fn reminder_should_wait_while_the_service_works() {
        for state in [
            UpdateState::Downloading {
                version: version(),
                received: 1,
                total: 2,
            },
            UpdateState::Checking,
            UpdateState::Verifying { version: version() },
        ] {
            let working = resolve(state.clone()).expect("a pending update shows the control");
            assert_eq!(
                working.clone().remind(Some(UpdateStage::Overdue)),
                working,
                "{state:?}"
            );
        }
    }

    #[test]
    fn failed_check_should_reach_only_a_check_awaiting_it() {
        let failed = UpdateState::Failed {
            error: UpdateError::Check,
        };
        let mut results = CheckResults::default();
        assert_eq!(results.notice(UpdateNotice::Failed, &failed), None);
        results.request();
        assert_eq!(
            results.notice(UpdateNotice::Failed, &failed),
            Some(failed.clone())
        );
    }

    #[gpui::test]
    fn later_should_keep_the_ready_control_without_a_dismiss_affordance(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::updates::UpdateEvent;
        use crate::updates::testing::RecordingAdapter;

        let adapter = std::rc::Rc::new(RecordingAdapter::available());
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        cx.update(server_rows::testing::install_up_to_date);
        let updates = cx.update(|cx| service(cx).expect("the update service is installed"));
        updates.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available(version()));
        cx.run_until_parked();
        updates.update(cx, |updates, cx| updates.download(cx));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        let (_, cx) = cx.add_window_view(UpdateControl::new);
        cx.run_until_parked();
        assert!(cx.debug_bounds("update-control").is_some());

        cx.update(|_, cx| put_off(cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("update-control").is_some());
        assert!(cx.debug_bounds("update-reminder-dismiss").is_none());
        updates.read_with(cx, |updates, _| {
            assert_eq!(updates.pending_version(), PENDING);
            assert_eq!(updates.state(), &UpdateState::Ready { version: version() });
        });
    }

    #[test]
    fn download_status_should_report_whole_percent() {
        let presentation = resolve(UpdateState::Downloading {
            version: "0.4.2".to_owned(),
            received: 2,
            total: 3,
        })
        .expect("download shows the control");
        assert_eq!(
            presentation.status.as_ref().map(AsRef::as_ref),
            Some("Downloading SpaceTerm 0.4.2, 66%")
        );
    }

    /// Installs the update service with automatic downloads off, so each step waits for the user.
    fn manual_updates(
        cx: &mut gpui::TestAppContext,
    ) -> (
        std::rc::Rc<crate::updates::testing::RecordingAdapter>,
        Entity<ApplicationUpdates>,
    ) {
        use crate::settings::storage::testing::MemoryStorage;
        use crate::updates::testing::RecordingAdapter;

        let adapter = std::rc::Rc::new(RecordingAdapter::available());
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        cx.update(server_rows::testing::install_up_to_date);
        let updates = cx.update(|cx| service(cx).expect("the update service is installed"));
        let mut document = crate::settings::SettingsDocument::default();
        document.updates.automatic_downloads = false;
        let settings = crate::settings::Settings::load(MemoryStorage::with_document(&document));
        updates.update(cx, |updates, _| updates.attach_settings(settings));
        (adapter, updates)
    }

    /// The pill hides while nothing asks for action, then splits into the step and the Updates
    /// menu behind its chevron.
    #[gpui::test]
    fn pill_should_appear_with_a_step_and_keep_the_updates_menu_behind_its_chevron(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::updates::UpdateEvent;
        use spaceterm_ui::a11y_testing::A11yTree;

        let (adapter, updates) = manual_updates(cx);
        let (_, cx) = cx.add_window_view(UpdateControl::new);
        cx.run_until_parked();
        assert!(cx.debug_bounds("update-control").is_none());
        assert!(cx.debug_bounds("update-control-menu").is_none());

        updates.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available(version()));
        cx.run_until_parked();
        let step = cx.debug_bounds("update-control").expect("the step shows");
        let chevron = cx
            .debug_bounds("update-control-menu")
            .expect("the chevron shows");
        assert_eq!(
            chevron.left(),
            step.right(),
            "the pill's parts did not join"
        );
        assert_eq!(chevron.size.height, step.size.height);
        let tree = A11yTree::read(cx);
        tree.node("Download 0.4.2");
        tree.node("Updates");
    }

    #[gpui::test]
    fn pressing_the_step_downloads_without_opening_the_updates_menu(cx: &mut gpui::TestAppContext) {
        use crate::updates::UpdateEvent;

        let (adapter, updates) = manual_updates(cx);
        updates.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available(version()));
        cx.run_until_parked();
        let (_, visual) = cx.add_window_view(UpdateControl::new);
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        let step = visual.debug_bounds("update-control").unwrap().center();
        visual.simulate_click(step, gpui::Modifiers::none());
        visual.run_until_parked();
        assert!(visual.debug_bounds("update-client-command").is_none());
        assert_eq!(adapter.downloads.get(), 1);
        updates.read_with(visual, |updates, _| {
            assert!(matches!(updates.state(), UpdateState::Downloading { .. }));
        });
        assert!(
            visual.debug_bounds("update-control").is_some(),
            "the pill stays while the download runs"
        );
    }

    #[gpui::test]
    fn choosing_the_client_row_downloads_once_after_the_updates_menu_closes(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::updates::UpdateEvent;

        let (adapter, updates) = manual_updates(cx);
        updates.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available(version()));
        cx.run_until_parked();
        let (_, visual) = cx.add_window_view(UpdateControl::new);
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        let chevron = visual.debug_bounds("update-control-menu").unwrap().center();
        visual.simulate_click(chevron, gpui::Modifiers::none());
        visual.run_until_parked();
        assert_eq!(adapter.downloads.get(), 0, "opening admits no download");
        assert!(visual.debug_bounds("update-server-0-status").is_some());
        let row = visual
            .debug_bounds("update-client-command")
            .unwrap()
            .center();
        visual.simulate_click(row, gpui::Modifiers::none());
        visual.run_until_parked();
        assert!(visual.debug_bounds("update-client-command").is_none());
        assert_eq!(adapter.downloads.get(), 1);
        updates.read_with(visual, |updates, _| {
            assert!(matches!(updates.state(), UpdateState::Downloading { .. }));
        });
    }

    #[test]
    fn every_update_prompt_should_satisfy_the_desktop_alert_policy() {
        let policy = spaceterm_ui::ModalDesktopPolicy::mac_os();
        let mut alerts = vec![
            up_to_date_alert(),
            available_alert("0.4.2"),
            stop_download_alert("0.4.2"),
            install_alert("0.4.2"),
            finish_install_alert("0.4.2"),
        ];
        alerts.extend(
            [
                UpdateError::Unavailable,
                UpdateError::Check,
                UpdateError::Download,
                UpdateError::Verification,
                UpdateError::Installation,
                UpdateError::ReadOnly,
            ]
            .map(|error| failure_alert(error, testing_wording())),
        );
        for alert in alerts {
            assert_eq!(alert.validate(&policy), Ok(()));
        }
    }

    #[gpui::test]
    fn restarting_to_install_shows_no_claim_about_ending_terminal_sessions(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::prelude::*;
        use spaceterm_ui::a11y_testing::A11yTree;

        struct InstallAlertHost;

        impl Render for InstallAlertHost {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                spaceterm_ui::ModalLayer::new(gpui::div().size_full())
            }
        }

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        cx.update(server_rows::testing::install_up_to_date);
        let (host, cx) = cx.add_window_view(|_, _| InstallAlertHost);
        let _handle = cx.update(|window, cx| {
            host.update(cx, |_, cx| {
                install_alert("0.4.2")
                    .present(window, cx, |_, _| {})
                    .expect("the install confirmation presents")
            })
        });
        cx.run_until_parked();

        let tree = A11yTree::read(cx);
        tree.text("Restart to Install SpaceTerm 0.4.2?");
        for action in ["Restart and Install", "Release Notes", "Later"] {
            tree.node(action);
        }
        tree.text(
            "SpaceTerm quits, installs the update, and opens again. Your Terminal Sessions keep running.",
        );
        assert!(cx.debug_bounds("modal-alert-detail").is_none());
    }
}
