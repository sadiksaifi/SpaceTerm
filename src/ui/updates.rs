//! Update presentation: the title-bar split pill, its alerts, menu commands and prompts.
//! [`crate::updates`] owns update policy, and [`crate::updates::state`] owns the one update view
//! every surface renders. Only a check the person asked for answers with an alert.

pub(crate) mod commands;
mod control;
pub(crate) mod whats_new;

pub(super) use control::UpdateControl;

use gpui::{
    App, BorrowAppContext, Context, Entity, Global, Subscription, Window, WindowHandle, actions,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionRole, ModalId,
};

use super::WorkspaceManager;
use crate::updates::copy::dialogs;
use crate::updates::machines::store::{MachineUpdates, MachineUpdatesNotice};
use crate::updates::reminders::{ReminderActivated, UpdateReminders};
use crate::updates::state::{self, Command};
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
            commands::reminder_clicked(clicked.key, clicked.token.as_deref(), cx);
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
            state if answers_check(state) => {
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

    /// Only a check the person asked for hears its result. A failure nobody waits on shows once
    /// in the title bar instead.
    fn notice(&mut self, notice: UpdateNotice, state: &UpdateState) -> Option<UpdateState> {
        if matches!(notice, UpdateNotice::ReadyToInstall)
            || !self.awaiting
            || self.presented.as_ref() == Some(state)
        {
            return None;
        }
        self.claim(state)
    }

    fn claim(&mut self, state: &UpdateState) -> Option<UpdateState> {
        if !answers_check(state) {
            return None;
        }
        self.awaiting = false;
        self.presented = Some(state.clone());
        Some(state.clone())
    }
}

/// Whether `state` answers a check. With automatic downloads, the check's release is already
/// downloading or downloaded when the answer arrives.
fn answers_check(state: &UpdateState) -> bool {
    matches!(
        state,
        UpdateState::UpToDate
            | UpdateState::Available { .. }
            | UpdateState::Downloading { .. }
            | UpdateState::Verifying { .. }
            | UpdateState::Ready { .. }
            | UpdateState::Failed { .. }
    )
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
    // A dismissed relaunch failure still holds its installer, which no new check can replace.
    if updates.read(cx).holds_failed_relaunch() {
        present_relaunch_failure(cx);
        return;
    }
    let state = updates.read(cx).state().clone();
    // A download failure the row shows answers with its own D6, as `Update Failed` does, so the
    // alert never reports a retry the pill and the row don't show. Its Try Again checks and
    // downloads again.
    if matches!(state, UpdateState::Failed { .. })
        && let Some(explanation) = download_failure(cx)
    {
        commands::perform(
            Command::Explain(explanation),
            commands::Presenter::Front,
            cx,
        );
        return;
    }
    match state {
        UpdateState::Unavailable => present_in_front_window(cx, |window, cx| {
            present_failure(UpdateError::Unavailable, window, cx)
        }),
        UpdateState::Failed { error } if error.relaunch_failed() => present_relaunch_failure(cx),
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
        // The same answer a check gives once it finds the download under way.
        UpdateState::Downloading { version, .. } | UpdateState::Verifying { version } => {
            if !updates.read(cx).is_cancelling() {
                present_in_front_window(cx, move |window, cx| {
                    present_downloading(&version, window, cx)
                });
            }
        }
        UpdateState::Ready { .. } => request_install(&updates, InstallRequest::FrontWorkspace, cx),
        // SpaceTerm is relaunching. The relaunch deadline reports it if it doesn't quit.
        UpdateState::Installing { .. } => {}
    }
}

/// D6 for the failed download SpaceTerm's row shows, if it shows one.
fn download_failure(cx: &App) -> Option<state::Explanation> {
    let app = state::current(cx).app;
    matches!(app.stage, state::AppStage::DownloadFailed { .. })
        .then(|| dialogs::app_failure(&app))
        .flatten()
}

fn request_install(updates: &Entity<ApplicationUpdates>, request: InstallRequest, cx: &mut App) {
    if cx.has_global::<UpdatePrompts>() {
        cx.update_global::<UpdatePrompts, _>(|prompts, _| {
            prompts.install_requested = Some(request);
        });
    }
    updates.update(cx, |updates, cx| updates.request_install_confirmation(cx));
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
        UpdateState::Downloading { version, .. } | UpdateState::Verifying { version } => {
            present_in_front_window(cx, move |window, cx| {
                present_downloading(&version, window, cx)
            });
        }
        UpdateState::Ready { .. } => present_in_front_window(cx, present_install),
        UpdateState::Failed { error } if error.relaunch_failed() => present_relaunch_failure(cx),
        // A failure the person can act on opens the same alert as `Update Failed`.
        UpdateState::Failed { error } => match dialogs::app_failure(&state::current(cx).app) {
            Some(explanation) => commands::perform(
                Command::Explain(explanation),
                commands::Presenter::Front,
                cx,
            ),
            None => {
                present_in_front_window(cx, move |window, cx| present_failure(error, window, cx))
            }
        },
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
    Relaunch,
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
        format!("{CURRENT_VERSION} is the newest version."),
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
        format!("You have {CURRENT_VERSION}."),
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

fn downloading_alert(version: &str) -> Alert<PromptAction> {
    Alert::new(
        ModalId::new("update-downloading"),
        "Update downloading",
        format!("SpaceTerm {version} Is Downloading"),
        format!("You have {CURRENT_VERSION}."),
        vec![
            action(
                PromptAction::Acknowledge,
                "OK",
                ModalActionRole::Affirmative,
                "update-downloading-ok",
            )
            .with_emphasis(ModalActionEmphasis::Prominent)
            .default_action(true),
            action(
                PromptAction::ReleaseNotes,
                RELEASE_NOTES_TITLE,
                ModalActionRole::Auxiliary,
                "update-downloading-release-notes",
            ),
        ],
    )
}

/// Answers a check whose release automatic downloads already began to download.
fn present_downloading<T: 'static>(version: &str, window: &mut Window, cx: &mut Context<T>) {
    present(downloading_alert(version), window, cx, |action, cx| {
        if action == Some(PromptAction::ReleaseNotes) {
            cx.open_url(RELEASE_NOTES_URL);
        }
    });
}

/// Choosing Later quiets the reminder until it comes due again. The update stays pending and the
/// Updates menu keeps offering it.
fn put_off(cx: &mut App) {
    if let Some(updates) = service(cx) {
        updates.update(cx, |updates, cx| updates.dismiss_reminder(cx));
    }
}

/// The alert for a check that could not run or finish. Every update failure the person can act
/// on opens its shared alert instead (D6 to D8, from `copy::dialogs`).
fn failure_alert(error: UpdateError) -> Alert<PromptAction> {
    let (title, retry, intent) = match error {
        UpdateError::Unavailable => ("Updates Unavailable", false, AlertIntent::Informational),
        UpdateError::SystemTooOld => (
            "Update Needs a Newer System",
            false,
            AlertIntent::Informational,
        ),
        _ => (
            "Couldn\u{2019}t Check for Updates",
            true,
            AlertIntent::Warning,
        ),
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
        error.to_string(),
        actions,
    )
    .intent(intent)
}

fn present_failure<T: 'static>(error: UpdateError, window: &mut Window, cx: &mut Context<T>) {
    present(failure_alert(error), window, cx, |action, cx| {
        if action == Some(PromptAction::TryAgain) {
            check_for_updates(cx);
        }
    });
}

/// The person chose Relaunch and is waiting for SpaceTerm to quit, so the failure appears even
/// with no Workspace Window open: SpaceTerm opens one to show it, as its relaunch would have.
fn present_relaunch_failure(cx: &mut App) {
    let Some(updates) = service(cx) else { return };
    let version = updates.read(cx).pending_version().map(str::to_owned);
    let unsaved = matches!(
        updates.read(cx).state(),
        UpdateState::Failed {
            error: UpdateError::SettingsUnsaved
        }
    );
    if front_workspace_window(cx).is_none() {
        crate::app::activate_default_window(cx, None);
        cx.activate(true);
    }
    let explanation = dialogs::relaunch_failure(version.as_deref(), unsaved);
    commands::perform(
        Command::Explain(explanation),
        commands::Presenter::Front,
        cx,
    );
}

fn install_alert(version: &str) -> Alert<PromptAction> {
    let (label, title, message) = dialogs::downloaded(version);
    Alert::new(
        ModalId::new("update-install"),
        label,
        title,
        message,
        vec![
            action(
                PromptAction::Relaunch,
                crate::updates::copy::title_bar::RELAUNCH_TO_UPDATE,
                ModalActionRole::Affirmative,
                "update-install-relaunch",
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
            let accepted = action == Some(PromptAction::Relaunch);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{Appearance, builtin_chrome_base};

    mod control_tests;

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

            // A download failure that no check awaits shows in the title bar, not an alert.
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
                [],
                "{order}"
            );

            // Automatic downloads begin the found release before the answer arrives. The menu
            // check still hears it once, and the download's progress adds nothing.
            let downloading = |received| UpdateState::Downloading {
                version: "0.4.2".to_owned(),
                received,
                total: 10,
            };
            let mut results = CheckResults::default();
            results.request();
            assert_eq!(
                deliver(
                    &mut results,
                    &downloading(0),
                    Some(UpdateNotice::CheckFinished),
                    observer_first
                ),
                [downloading(0)],
                "{order}"
            );
            assert_eq!(
                deliver(&mut results, &downloading(5), None, observer_first),
                []
            );

            // Scheduled results stay silent.
            let mut results = CheckResults::default();
            assert_eq!(deliver(&mut results, &up_to_date, None, observer_first), []);
            assert_eq!(
                deliver(&mut results, &downloading(0), None, observer_first),
                []
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
            install_alert("0.4.2"),
            downloading_alert("0.4.2"),
        ];
        alerts.extend(
            [
                UpdateError::Unavailable,
                UpdateError::Check,
                UpdateError::Download,
                UpdateError::Verification,
                UpdateError::Installation,
                UpdateError::DiskFull,
                UpdateError::SystemTooOld,
                UpdateError::ReadOnly,
                UpdateError::Relaunch,
                UpdateError::SettingsUnsaved,
            ]
            .map(failure_alert),
        );
        for alert in alerts {
            assert_eq!(alert.validate(&policy), Ok(()));
        }
        let confirmation = crate::updates::confirm::Confirmation {
            title: "Restart SpaceTerm Core on penguin?".into(),
            message: "SpaceTerm Core restarts from 0.11.3 to 0.11.4. This ends 3 Terminal Sessions. Workspaces, Tabs and Panes stay.".into(),
            button: "Restart Core".into(),
            cancel: "Remind Me Tomorrow".into(),
            actions: Vec::new(),
            approval: crate::updates::confirm::Approval::default(),
        };
        assert_eq!(
            commands::confirmation_alert(&confirmation).validate(&policy),
            Ok(())
        );
        let failure =
            |then: Option<crate::updates::state::Command>| crate::updates::confirm::Explanation {
                title: "Couldn't Restart SpaceTerm Core on penguin".into(),
                message: "SpaceTerm can't reach penguin. Try again when it's connected.".into(),
                then: then.map(Box::new),
                proceed: None,
                dismiss: None,
            };
        for then in [
            None,
            Some(crate::updates::state::Command::Machine(
                [2; 32],
                crate::updates::machines::MachineAction::CheckAgain,
            )),
        ] {
            assert_eq!(
                commands::explanation_alert(&failure(then)).validate(&policy),
                Ok(())
            );
        }
    }

    /// Presents `alert` on its own window and reads what assistive technology reads.
    fn presented(
        alert: Alert<PromptAction>,
        cx: &mut gpui::TestAppContext,
    ) -> spaceterm_ui::a11y_testing::A11yTree {
        use gpui::prelude::*;

        struct AlertHost;

        impl Render for AlertHost {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                spaceterm_ui::ModalLayer::new(gpui::div().size_full())
            }
        }

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let (host, cx) = cx.add_window_view(|_, _| AlertHost);
        let _handle = cx.update(|window, cx| {
            host.update(cx, |_, cx| {
                alert
                    .present(window, cx, |_, _| {})
                    .expect("the alert presents")
            })
        });
        cx.run_until_parked();
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
    }

    #[gpui::test]
    fn the_downloaded_alert_relaunches_to_update_and_keeps_terminal_sessions_running(
        cx: &mut gpui::TestAppContext,
    ) {
        let tree = presented(install_alert("0.4.2"), cx);
        tree.text("SpaceTerm 0.4.2 Is Downloaded");
        for action in ["Relaunch to Update", "Release Notes", "Later"] {
            tree.node(action);
        }
        tree.text("Relaunching keeps Terminal Sessions running.");
        let alert = tree.node("Update downloaded");
        assert_eq!(alert["aria"]["role"], "AlertDialog");
        assert_eq!(tree.with_role("Button").len(), 3);
    }
}
