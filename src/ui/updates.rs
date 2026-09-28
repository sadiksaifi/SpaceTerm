//! Update presentation: the title bar control, the application menu commands, and their prompts.
//!
//! [`crate::updates`] owns update policy and state. This module renders that state, turns user
//! commands into its operations, and settles every install confirmation it claims.

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, Global, SharedString, Subscription, Window, WindowHandle, actions, div,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ButtonShape, ButtonSize, ButtonVariant,
    DeterminateProgress, FrameSpinner, Icon, IconName, ModalAction, ModalActionEmphasis,
    ModalActionIntent, ModalActionRole, ModalId, ProgressRing, ProgressSize, Tooltip,
};

use super::WorkspaceManager;
use crate::updates::{ApplicationUpdates, UpdateError, UpdateNotice, UpdateService, UpdateState};

actions!(spaceterm, [CheckForUpdates, OpenReleaseNotes]);

pub(crate) const CHECK_FOR_UPDATES_TITLE: &str = "Check for Updates…";
pub(crate) const RELEASE_NOTES_TITLE: &str = "Release Notes";
const RELEASE_NOTES_URL: &str = "https://github.com/sadiksaifi/SpaceTerm/releases/latest";
const CURRENT_VERSION: &str = env!("SPACETERM_VERSION");

/// Installs the menu commands and the one application-wide owner of update prompts.
///
/// The update service must already be installed. Without it, the commands explain that updates
/// are unavailable and no window shows an update control.
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
        install_requested: false,
        _subscriptions: [notices, states],
    });
}

/// What the user is waiting to hear about. Update state itself stays in the service.
struct UpdatePrompts {
    results: CheckResults,
    /// The user asked to install, so the confirmation is shown even over another prompt.
    install_requested: bool,
    _subscriptions: [Subscription; 2],
}

impl Global for UpdatePrompts {}

/// Decides which results reach the user, each exactly once.
///
/// The service changes its state and then emits a notice and notifies observers. GPUI may deliver
/// those two in either order, and a menu check that joins a scheduled check receives no notice. So
/// both deliveries claim the current result here, and the second finds it already presented.
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
            UpdateNotice::Failed => true,
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

fn take_install_request(cx: &mut App) -> bool {
    if !cx.has_global::<UpdatePrompts>() {
        return false;
    }
    let requested = cx.global::<UpdatePrompts>().install_requested;
    if requested {
        cx.update_global::<UpdatePrompts, _>(|prompts, _| prompts.install_requested = false);
    }
    requested
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
        UpdateState::Ready { .. } => request_install(&updates, cx),
        UpdateState::Installing { version } => present_in_front_window(cx, move |window, cx| {
            present_finish_install(&version, window, cx)
        }),
    }
}

fn request_install(updates: &Entity<ApplicationUpdates>, cx: &mut App) {
    if cx.has_global::<UpdatePrompts>() {
        cx.update_global::<UpdatePrompts, _>(|prompts, _| prompts.install_requested = true);
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
            let requested = take_install_request(cx);
            let Some(handle) = front_workspace_window(cx) else {
                return;
            };
            let _ = handle.update(cx, |_, window, cx| {
                // An unrequested confirmation waits for the button rather than stacking behind
                // another decision. The ready control stays prominent until the user acts.
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
fn front_workspace_window(cx: &App) -> Option<WindowHandle<WorkspaceManager>> {
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
        format!(
            "You have version {CURRENT_VERSION}. Download the update now. SpaceTerm asks before it \
             restarts to install it."
        ),
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
            _ => {}
        },
    );
}

fn failure_alert(error: UpdateError) -> Alert<PromptAction> {
    let (title, retry, intent) = match error {
        UpdateError::Unavailable => ("Updates Unavailable", false, AlertIntent::Informational),
        UpdateError::Check => ("Couldn’t Check for Updates", true, AlertIntent::Warning),
        UpdateError::Download => ("Couldn’t Download the Update", true, AlertIntent::Warning),
        UpdateError::Verification => ("Couldn’t Verify the Update", true, AlertIntent::Warning),
        UpdateError::Installation => ("Couldn’t Install the Update", true, AlertIntent::Warning),
        UpdateError::ReadOnly => (
            "Move SpaceTerm to Applications",
            false,
            AlertIntent::Warning,
        ),
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

fn stop_download_alert(version: &str) -> Alert<PromptAction> {
    Alert::new(
        ModalId::new("update-stop-download"),
        "Stop update download",
        format!("Stop Downloading SpaceTerm {version}?"),
        "You can download it again later with Check for Updates in the SpaceTerm menu.",
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
        "SpaceTerm quits, installs the update, and opens again. If commands are still running, \
         SpaceTerm asks before it quits.",
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
            if action == Some(PromptAction::ReleaseNotes) {
                cx.open_url(RELEASE_NOTES_URL);
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
        format!("Restart to Finish Installing SpaceTerm {version}?"),
        "SpaceTerm installs the update when it quits.",
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
    present(finish_install_alert(version), window, cx, |action, cx| {
        if action == Some(PromptAction::Restart) {
            retry_install(cx);
        }
    });
}

fn retry_install(cx: &mut App) {
    if let Some(updates) = service(cx) {
        updates.update(cx, |updates, cx| updates.retry_install(cx));
    }
}

/// The glyph that leads the title bar control.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ControlGlyph {
    Download,
    Progress(f32),
    Activity,
    Restart,
}

/// What activating the title bar control does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlCommand {
    Download,
    OfferStop,
    RequestInstall,
    RetryInstall,
}

/// One frame of the title bar control.
///
/// The label stays the release version while the glyph and emphasis carry the phase, so the
/// control keeps its width and place from download to restart. Accent paint appears only when
/// the next step is the user's. Every glyph is unframed so the capsule is the only outline.
#[derive(Clone, Debug, PartialEq)]
struct ControlPresentation {
    label: SharedString,
    tooltip: SharedString,
    glyph: ControlGlyph,
    prominent: bool,
    command: Option<ControlCommand>,
}

impl ControlPresentation {
    fn resolve(state: &UpdateState, cancelling: bool) -> Option<Self> {
        let working = |version: &str, tooltip: String, glyph| Self {
            label: version.to_owned().into(),
            tooltip: tooltip.into(),
            glyph,
            prominent: false,
            command: Some(ControlCommand::OfferStop),
        };
        let presentation = match state {
            // Until a release exists there is nothing to show. A check reports through its alert.
            UpdateState::Unavailable
            | UpdateState::Idle
            | UpdateState::Checking
            | UpdateState::UpToDate
            | UpdateState::Failed { .. } => return None,
            UpdateState::Available { version } => Self {
                label: version.clone().into(),
                tooltip: format!("Download SpaceTerm {version}").into(),
                glyph: ControlGlyph::Download,
                prominent: true,
                command: Some(ControlCommand::Download),
            },
            UpdateState::Downloading { version, .. } | UpdateState::Verifying { version }
                if cancelling =>
            {
                Self {
                    command: None,
                    ..working(
                        version,
                        "Stopping the Update".to_owned(),
                        ControlGlyph::Activity,
                    )
                }
            }
            UpdateState::Downloading {
                version,
                received,
                total,
            } if *total > 0 => {
                let fraction = (*received as f64 / *total as f64).clamp(0.0, 1.0);
                working(
                    version,
                    format!(
                        "Downloading SpaceTerm {version}, {}%",
                        (fraction * 100.0).floor()
                    ),
                    ControlGlyph::Progress(fraction as f32),
                )
            }
            UpdateState::Downloading { version, .. } => working(
                version,
                format!("Downloading SpaceTerm {version}"),
                ControlGlyph::Activity,
            ),
            UpdateState::Verifying { version } => working(
                version,
                format!("Verifying SpaceTerm {version}"),
                ControlGlyph::Activity,
            ),
            UpdateState::Ready { version } => Self {
                label: version.clone().into(),
                tooltip: format!("Restart to Install SpaceTerm {version}").into(),
                glyph: ControlGlyph::Restart,
                prominent: true,
                command: Some(ControlCommand::RequestInstall),
            },
            // The install was confirmed. Reaching this state visibly means the quit was cancelled.
            UpdateState::Installing { version } => Self {
                label: version.clone().into(),
                tooltip: format!("Restart to Finish Installing SpaceTerm {version}").into(),
                glyph: ControlGlyph::Restart,
                prominent: true,
                command: Some(ControlCommand::RetryInstall),
            },
        };
        Some(presentation)
    }
}

/// The update control at the trailing end of one window's title bar.
///
/// Every window shows the same application update. The control is absent unless an update exists.
pub(super) struct UpdateControl {
    _subscription: Option<Subscription>,
}

impl UpdateControl {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            _subscription: service(cx).map(|updates| cx.observe(&updates, |_, _, cx| cx.notify())),
        }
    }

    fn activate(command: ControlCommand, window: &mut Window, cx: &mut App) {
        let Some(updates) = service(cx) else { return };
        match command {
            ControlCommand::Download => updates.update(cx, |updates, cx| updates.download(cx)),
            ControlCommand::OfferStop => {
                let (state, cancelling) = {
                    let updates = updates.read(cx);
                    (updates.state().clone(), updates.is_cancelling())
                };
                let (UpdateState::Downloading { version, .. } | UpdateState::Verifying { version }) =
                    state
                else {
                    return;
                };
                if cancelling {
                    return;
                }
                let Some(handle) = window.window_handle().downcast::<WorkspaceManager>() else {
                    return;
                };
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, cx| {
                        present_stop_download(&version, window, cx)
                    });
                });
            }
            ControlCommand::RequestInstall => request_install(&updates, cx),
            ControlCommand::RetryInstall => retry_install(cx),
        }
    }
}

impl Render for UpdateControl {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(updates) = service(cx) else {
            return div().into_any_element();
        };
        let updates = updates.read(cx);
        let Some(presentation) =
            ControlPresentation::resolve(updates.state(), updates.is_cancelling())
        else {
            return div().into_any_element();
        };
        let appearance = super::appearance::chrome(cx);
        let glyph_size = appearance
            .icons
            .metrics(super::chrome_icons::IconRole::Caption)
            .glyph_size;
        let glyph = presentation.glyph;
        let tooltip = presentation.tooltip.clone();
        let command = presentation.command;
        let button = Button::new("update-control", presentation.label)
            .variant(if presentation.prominent {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Secondary
            })
            .size(ButtonSize::Small)
            .shape(ButtonShape::Capsule)
            .disabled(command.is_none())
            .debug_selector("update-control")
            .leading(move |foreground| match glyph {
                ControlGlyph::Download => {
                    Icon::new(IconName::Download, glyph_size, foreground).into_any_element()
                }
                ControlGlyph::Restart => {
                    Icon::new(IconName::RotateCw, glyph_size, foreground).into_any_element()
                }
                ControlGlyph::Progress(fraction) => DeterminateProgress::new(f64::from(fraction))
                    .map_or_else(
                        |_| {
                            FrameSpinner::new("update-control-activity", "Update")
                                .size(ProgressSize::Compact)
                                .into_any_element()
                        },
                        |progress| {
                            ProgressRing::new("update-control-progress", "Update", progress)
                                .size(ProgressSize::Compact)
                                .inherited()
                                .into_any_element()
                        },
                    ),
                ControlGlyph::Activity => FrameSpinner::new("update-control-activity", "Update")
                    .size(ProgressSize::Compact)
                    .into_any_element(),
            })
            .tooltip(
                Tooltip::new("update-control-tooltip", tooltip)
                    .debug_selector("update-control-tooltip"),
            )
            .when_some(command, |button, command| {
                button.on_activate(move |_, window, cx| Self::activate(command, window, cx))
            });
        // The control owns its clicks inside the title bar's drag region. Its trailing edge
        // stops where the Pane beneath it stops, and it keeps one frame gap from the Tabs.
        let frame = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
        div()
            .debug_selector(|| "update-control-area".to_owned())
            .flex_none()
            .pl(frame.space())
            .pr(frame.window_edge_inset())
            .child(div().block_mouse_except_scroll().child(button))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(state: UpdateState) -> Option<ControlPresentation> {
        ControlPresentation::resolve(&state, false)
    }

    #[test]
    fn control_should_appear_only_when_an_update_exists() {
        for state in [
            UpdateState::Unavailable,
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Failed {
                error: UpdateError::Check,
            },
        ] {
            assert_eq!(resolve(state.clone()), None, "{state:?}");
        }
    }

    #[test]
    fn control_should_keep_the_version_and_emphasize_only_the_users_turn() {
        let version = || "0.4.2".to_owned();
        let cases = [
            (
                UpdateState::Available { version: version() },
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
                ControlGlyph::Activity,
                false,
                Some(ControlCommand::OfferStop),
            ),
            (
                UpdateState::Verifying { version: version() },
                ControlGlyph::Activity,
                false,
                Some(ControlCommand::OfferStop),
            ),
            (
                UpdateState::Ready { version: version() },
                ControlGlyph::Restart,
                true,
                Some(ControlCommand::RequestInstall),
            ),
            (
                UpdateState::Installing { version: version() },
                ControlGlyph::Restart,
                true,
                Some(ControlCommand::RetryInstall),
            ),
        ];
        for (state, glyph, prominent, command) in cases {
            let presentation = resolve(state.clone()).expect("an update shows the control");
            assert_eq!(presentation.label.as_ref(), "0.4.2", "{state:?}");
            assert_eq!(presentation.glyph, glyph, "{state:?}");
            assert_eq!(presentation.prominent, prominent, "{state:?}");
            assert_eq!(presentation.command, command, "{state:?}");
        }
    }

    #[test]
    fn stopping_update_should_block_further_commands() {
        let presentation = ControlPresentation::resolve(
            &UpdateState::Downloading {
                version: "0.4.2".to_owned(),
                received: 1,
                total: 2,
            },
            true,
        )
        .expect("a stopping update stays visible until the service settles");
        assert_eq!(presentation.command, None);
        assert_eq!(presentation.glyph, ControlGlyph::Activity);
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
    fn download_tooltip_should_report_whole_percent() {
        let presentation = resolve(UpdateState::Downloading {
            version: "0.4.2".to_owned(),
            received: 2,
            total: 3,
        })
        .expect("download shows the control");
        assert_eq!(
            presentation.tooltip.as_ref(),
            "Downloading SpaceTerm 0.4.2, 66%"
        );
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
            .map(failure_alert),
        );
        for alert in alerts {
            assert_eq!(alert.validate(&policy), Ok(()));
        }
    }
}
