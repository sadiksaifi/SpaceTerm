//! Update presentation: the title-bar update control, its alerts, menu commands, prompts, and
//! launch view. [`crate::updates`] owns update policy and state, and
//! [`crate::updates::presentation`] owns what every update surface says.

pub(crate) mod commands;
mod control;
mod launch;
pub(crate) mod whats_new;

pub(super) use control::UpdateControl;
pub(crate) use launch::{request_window_close as request_launch_window_close, show_launch};

use gpui::{
    AnyWindowHandle, App, BorrowAppContext, Context, Entity, Global, Subscription, Window,
    WindowHandle, actions,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::WorkspaceManager;
use super::settings_window::SettingsWindow;
use crate::desktop_profile::{DesktopPresentation, DesktopWording};
use crate::updates::machines::store::{MachineUpdates, MachineUpdatesNotice};
use crate::updates::reminders::{ReminderActivated, UpdateReminders};
use crate::updates::{ApplicationUpdates, UpdateError, UpdateNotice, UpdateService, UpdateState};

actions!(spaceterm, [CheckForUpdates]);

#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the native application menu names it")
)]
pub(crate) const CHECK_FOR_UPDATES_TITLE: &str = "Check for Updates…";
pub(crate) const RELEASE_NOTES_TITLE: &str = "Release Notes";
const RELEASE_NOTES_URL: &str = "https://github.com/sadiksaifi/SpaceTerm/releases/latest";
const CURRENT_VERSION: &str = env!("SPACETERM_VERSION");

/// Installs the menu commands and the one application-wide owner of update prompts. The update
/// service must already be installed.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &CheckForUpdates, cx| check_for_updates(cx));
    whats_new::init(cx);
    let Some(service) = cx.try_global::<UpdateService>().cloned() else {
        return;
    };
    let notices = cx.subscribe(&service.0, |_, notice: &UpdateNotice, cx| {
        receive_notice(*notice, cx);
    });
    let states = cx.observe(&service.0, |updates, cx| observe_state(&updates, cx));
    let mut subscriptions = vec![notices, states];
    subscriptions.extend(MachineUpdates::global(cx).map(|machines| {
        cx.subscribe(&machines, |_, notice: &MachineUpdatesNotice, cx| {
            commands::receive_failure(notice, cx);
        })
    }));
    subscriptions.extend(UpdateReminders::global(cx).map(|reminders| {
        cx.subscribe(&reminders, |_, clicked: &ReminderActivated, cx| {
            commands::reminder_clicked(clicked.0, cx);
        })
    }));
    cx.set_global(UpdatePrompts {
        results: CheckResults::default(),
        install_requested: None,
        _subscriptions: subscriptions,
    });
}

/// Whether the update presentation subscribed to the update services.
#[cfg(test)]
pub(crate) fn presenting(cx: &App) -> bool {
    cx.has_global::<UpdatePrompts>()
}

/// What the user is waiting to hear about. Update state itself stays in the service.
struct UpdatePrompts {
    results: CheckResults,
    /// The user asked to install, so the confirmation is shown even over another prompt.
    install_requested: Option<InstallRequest>,
    _subscriptions: Vec<Subscription>,
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
        actions.push(
            action(
                PromptAction::TryAgain,
                "Try Again",
                ModalActionRole::Affirmative,
                "update-failure-try-again",
            )
            .with_emphasis(ModalActionEmphasis::Prominent)
            .default_action(true),
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{Appearance, builtin_chrome_base};

    mod control_tests;

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
        let updates = cx.update(|cx| service(cx).expect("the update service is installed"));
        let mut document = crate::settings::SettingsDocument::default();
        document.updates.automatic_downloads = false;
        let settings = crate::settings::Settings::load(MemoryStorage::with_document(&document));
        updates.update(cx, |updates, _| updates.attach_settings(settings));
        (adapter, updates)
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
        let confirmation = crate::updates::presentation::Confirmation {
            title: "Restart SpaceTerm Core on penguin?".into(),
            message: "This will close your 3 running sessions.".into(),
            button: "Restart".into(),
            actions: Vec::new(),
            approval: crate::updates::presentation::Approval::default(),
        };
        assert_eq!(
            commands::confirmation_alert(&confirmation).validate(&policy),
            Ok(())
        );
        let explanation = crate::updates::presentation::Explanation {
            title: "Couldn't restart SpaceTerm Core on penguin".into(),
            message: "SpaceTerm can't reach penguin. Try again when it is connected.".into(),
            then: None,
        };
        assert_eq!(
            commands::explanation_alert(&explanation).validate(&policy),
            Ok(())
        );
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
        let alert = tree.node("Install update");
        assert_eq!(alert["aria"]["role"], "AlertDialog");
        assert_eq!(tree.with_role("Button").len(), 3);
    }
}
