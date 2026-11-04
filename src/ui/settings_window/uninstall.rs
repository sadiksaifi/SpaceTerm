//! Settings > Advanced > Uninstall SpaceTerm. One confirmed step removes SpaceTerm Core and then
//! the SpaceTerm app from this computer, and SpaceTerm quits. Workspaces, paired devices and
//! settings stay unless the person opts in to deleting them. Then the keys of every Machine that
//! SpaceTerm or the command-line tool saved go too, before the app removes itself, so they go
//! even when the app can't. The Machines themselves are asked nothing, so each keeps listing
//! this device until someone removes it there.
//!
//! The alert names the Terminal Sessions that end, as SpaceTerm Core observed them. SpaceTerm
//! Core refuses the uninstall when those sessions changed before the person confirmed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, AnyWindowHandle, App, Context, Task, Window};
use spaceterm_core::hierarchy::Id;
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, AlertSuppression, ModalAction, ModalActionEmphasis,
    ModalActionIntent, ModalActionRole, ModalId, ProgressCancellation, ProgressDialog,
    ProgressDialogHandle, ProgressDialogUpdate, ProgressState,
};

use crate::application_identity::ApplicationIdentity;
use crate::direct_connections::credentials::{self, CredentialError, CredentialStore};
use crate::direct_connections::{ConnectionStore, DirectConnections, StoreKind};
use crate::installed_app::{InstalledApp, remove_directory};
use crate::machine_connection::CoreError;
pub(crate) use crate::machine_connection::service::Data;
use crate::machine_connection::service::{CliUninstall, Service};
use crate::platform::app_directories::AppDirectories;
use crate::ui::sidebar_window::form::action_button;

use super::SettingsWindow;

pub(super) const UNINSTALL_SELECTOR: &str = "settings-uninstall-spaceterm";
pub(super) const CONFIRM_SELECTOR: &str = "settings-uninstall-spaceterm-confirm";
pub(super) const CANCEL_SELECTOR: &str = "settings-uninstall-spaceterm-cancel";
pub(super) const QUIT_SELECTOR: &str = "settings-uninstall-spaceterm-quit";
pub(super) const DISMISS_SELECTOR: &str = "settings-uninstall-spaceterm-dismiss";
const DELETE_DATA: &str = "Also delete Workspaces, paired devices and settings";
/// The progress dialog stays until the work answers. This only bounds a step that never does.
const DEADLINE: Duration = Duration::from_secs(600);

/// The Terminal Sessions an uninstall ends, as SpaceTerm Core observed them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Observed {
    sessions: Vec<Id>,
}

impl Observed {
    pub(super) fn new(mut sessions: Vec<Id>) -> Self {
        sessions.sort_unstable();
        Self { sessions }
    }

    fn count(&self) -> usize {
        self.sessions.len()
    }
}

/// Why an uninstall stopped while SpaceTerm Core stayed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemovalFailure {
    /// The Terminal Sessions changed after the person was asked, so nothing was removed.
    Changed,
    /// SpaceTerm Core couldn't be removed. Trying again finishes a partial removal.
    Core,
}

/// The app couldn't remove itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AppKept;

/// Some of the data the person asked to delete stayed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DataKept;

/// The credential items of the command-line tool's saved Machines. SpaceTerm Core's data holds
/// their records, so they are read before SpaceTerm Core goes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct CliCredentials {
    items: Vec<(StoreKind, [u8; 32])>,
    /// The records couldn't be read, so items they name may stay.
    unread: bool,
}

/// How an uninstall ended once SpaceTerm Core was removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Ended {
    pub(super) data: Data,
    pub(super) data_kept: bool,
    pub(super) app_kept: bool,
}

impl RemovalFailure {
    fn of_core(error: CoreError) -> Self {
        match error {
            CoreError::Changed => Self::Changed,
            _ => Self::Core,
        }
    }
}

/// The blocking steps of an uninstall, run on the background executor in this order.
/// [`Self::cli_credentials`] and [`Self::delete_data`] run only with [`Data::Delete`].
pub(super) trait SpaceTermRemoval: Send + Sync {
    /// Reads the Terminal Sessions an uninstall ends. Removes nothing.
    fn observe(&self) -> Result<Observed, RemovalFailure>;
    /// Reads the credential items of the command-line tool's saved and pending Machines.
    /// Removes nothing.
    fn cli_credentials(&self) -> CliCredentials;
    /// Removes SpaceTerm Core and, with [`Data::Delete`], its data. Refuses with
    /// [`RemovalFailure::Changed`] when the sessions differ from `observed`.
    fn remove_core(&self, observed: &Observed, data: Data) -> Result<(), RemovalFailure>;
    /// Removes `cli`'s items, every Machine `machines` saved or was enrolling with its key, and
    /// this desktop's own settings, saved Machines and state. It tries each one. `machines` is
    /// `None` when this desktop installed no registry, so its keys can't be found.
    fn delete_data(
        &self,
        cli: CliCredentials,
        machines: Option<DirectConnections>,
    ) -> Result<(), DataKept>;
    /// Removes the SpaceTerm app this process runs from.
    fn remove_app(&self) -> Result<(), AppKept>;
}

/// The row's removal and the work it has underway.
pub(super) struct UninstallRow {
    removal: Arc<dyn SpaceTermRemoval>,
    working: bool,
    progress: Option<ProgressDialogHandle>,
    _task: Option<Task<()>>,
}

impl UninstallRow {
    pub(super) fn new(removal: Arc<dyn SpaceTermRemoval>) -> Self {
        Self {
            removal,
            working: false,
            progress: None,
            _task: None,
        }
    }
}

/// The alert's message before anything is removed.
pub(super) fn confirmation_message(sessions: usize, here: &str) -> String {
    match sessions {
        0 => format!("This will remove SpaceTerm and SpaceTerm Core from {here}."),
        1 => format!("This will close your 1 running session on {here}."),
        count => format!("This will close your {count} running sessions on {here}."),
    }
}

/// What the person reads when an uninstall stops while SpaceTerm Core stays.
pub(super) fn failure_message(failure: RemovalFailure, here: &str) -> String {
    match failure {
        RemovalFailure::Changed => format!(
            "The running sessions on {here} changed, so nothing was uninstalled. Try again."
        ),
        RemovalFailure::Core => {
            format!("SpaceTerm couldn\u{2019}t remove SpaceTerm Core from {here}, so try again.")
        }
    }
}

/// The last alert's title and message once SpaceTerm Core is gone.
pub(super) fn ended_message(ended: Ended, here: &str, mac: bool) -> (&'static str, String) {
    const PARTLY: &str = "Couldn\u{2019}t Uninstall SpaceTerm";
    let app_remedy = if mac {
        "move SpaceTerm to the Trash"
    } else {
        "run install.sh --uninstall"
    };
    match ended {
        Ended {
            app_kept: false,
            data_kept: false,
            data,
        } => {
            let what = match data {
                Data::Keep => "kept",
                Data::Delete => "deleted",
            };
            (
                "SpaceTerm Was Uninstalled",
                format!(
                    "SpaceTerm and SpaceTerm Core were removed from {here}. Workspaces, paired \
                     devices and settings were {what}."
                ),
            )
        }
        Ended {
            app_kept: false,
            data_kept: true,
            ..
        } => (
            PARTLY,
            format!(
                "SpaceTerm and SpaceTerm Core were removed from {here}, but SpaceTerm \
                 couldn\u{2019}t delete all of its Workspaces, paired devices and settings."
            ),
        ),
        Ended {
            app_kept: true,
            data_kept,
            ..
        } => {
            let mut message = format!(
                "SpaceTerm Core was removed, but SpaceTerm couldn\u{2019}t remove itself, so \
                 {app_remedy}."
            );
            if data_kept {
                message
                    .push_str(" Some of its Workspaces, paired devices and settings also remain.");
            }
            (PARTLY, message)
        }
    }
}

impl SettingsWindow {
    fn here(&self) -> String {
        self.this_computer_phrase.to_owned()
    }

    fn on_mac(&self) -> bool {
        self.this_computer == "This Mac"
    }

    /// The Advanced row's description.
    pub(super) fn uninstall_description(&self) -> &'static str {
        self.uninstall_description
    }

    pub(super) fn render_uninstall(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        let enabled = self.uninstall.as_ref().is_some_and(|row| !row.working);
        action_button(
            UNINSTALL_SELECTOR,
            "Uninstall SpaceTerm\u{2026}",
            enabled,
            move |window, cx| {
                let _ = owner.update(cx, |settings, cx| settings.begin_uninstall(window, cx));
            },
        )
        .into_any_element()
    }

    /// Reads the sessions an uninstall ends, then asks.
    fn begin_uninstall(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.uninstall.as_mut() else {
            return;
        };
        if row.working {
            return;
        }
        row.working = true;
        let removal = Arc::clone(&row.removal);
        row._task = Some(cx.spawn_in(window, async move |settings, cx| {
            let observed = cx
                .background_executor()
                .spawn(async move { removal.observe() })
                .await;
            let _ = settings.update_in(cx, |settings, window, cx| match observed {
                Ok(observed) => settings.confirm_uninstall(observed, window, cx),
                Err(failure) => settings.uninstall_stopped(failure, window, cx),
            });
        }));
        cx.notify();
    }

    fn confirm_uninstall(
        &mut self,
        observed: Observed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        let handle = window.window_handle();
        let result = Alert::new(
            ModalId::new("settings-uninstall-spaceterm"),
            "Uninstall SpaceTerm",
            "Uninstall SpaceTerm?",
            confirmation_message(observed.count(), &self.here()),
            vec![
                ModalAction::new(
                    true,
                    "Uninstall",
                    ModalActionRole::Affirmative,
                    CONFIRM_SELECTOR,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(false, "Cancel", ModalActionRole::Cancel, CANCEL_SELECTOR),
            ],
        )
        .intent(AlertIntent::Critical)
        .suppression(AlertSuppression::new(DELETE_DATA, false))
        .present(window, cx, move |outcome, cx| {
            let data = match outcome {
                AlertOutcome::Activated {
                    action_id: true,
                    suppression_selected,
                    ..
                } => Some(if suppression_selected == Some(true) {
                    Data::Delete
                } else {
                    Data::Keep
                }),
                _ => None,
            };
            in_window(handle, cx, move |window, cx| {
                let _ = owner.update(cx, |settings, cx| match data {
                    Some(data) => settings.uninstall(observed, data, window, cx),
                    None => settings.uninstall_ended(cx),
                });
            });
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm uninstall confirmation");
            self.uninstall_ended(cx);
        }
    }

    /// Removes SpaceTerm Core, then the app, then the data the person asked to delete.
    fn uninstall(
        &mut self,
        observed: Observed,
        data: Data,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.uninstall.as_ref() else {
            return;
        };
        let removal = Arc::clone(&row.removal);
        let here = self.here();
        // The registry is read here, on the window's thread, and used in the background.
        let machines = self
            .direct_routes
            .as_ref()
            .map(|rows| rows.connections.clone());
        let progress = ProgressDialog::<()>::new(
            ModalId::new("settings-uninstall-spaceterm-progress"),
            "Uninstalling SpaceTerm",
            "Uninstalling SpaceTerm\u{2026}",
            format!("Removing SpaceTerm Core from {here}\u{2026}"),
            ProgressState::Indeterminate,
            ProgressCancellation::programmatic_only(DEADLINE),
        )
        .present(
            window,
            cx,
            |_, _, _| spaceterm_ui::ProgressCancelDecision::Deny,
            |_, _| {},
        );
        let progress = match progress {
            Ok(progress) => progress,
            Err(_) => {
                eprintln!("failed to present the SpaceTerm uninstall progress");
                self.uninstall_ended(cx);
                return;
            }
        };
        let task = cx.spawn_in(window, async move |settings, cx| {
            let core = {
                let removal = Arc::clone(&removal);
                cx.background_executor()
                    .spawn(async move {
                        // SpaceTerm Core's data holds the command-line tool's records, so their
                        // keys are read before it goes.
                        let cli = (data == Data::Delete).then(|| removal.cli_credentials());
                        removal.remove_core(&observed, data).map(|()| cli)
                    })
                    .await
            };
            let cli = match core {
                Ok(cli) => cli,
                Err(failure) => {
                    let _ = settings.update_in(cx, |settings, window, cx| {
                        settings.uninstall_stopped(failure, window, cx);
                    });
                    return;
                }
            };
            let _ = settings.update_in(cx, |settings, window, cx| {
                settings.uninstall_progress(
                    format!("Removing SpaceTerm from {here}\u{2026}"),
                    window,
                    cx,
                );
            });
            // The data goes before the app, so it goes even when the app can't, and the keys go
            // while the app that saved them still exists.
            let ended = cx
                .background_executor()
                .spawn(async move {
                    let data_kept =
                        cli.is_some_and(|cli| removal.delete_data(cli, machines).is_err());
                    Ended {
                        data,
                        data_kept,
                        app_kept: removal.remove_app().is_err(),
                    }
                })
                .await;
            let _ = settings.update_in(cx, |settings, window, cx| {
                settings.uninstalled(ended, window, cx);
            });
        });
        if let Some(row) = self.uninstall.as_mut() {
            row.progress = Some(progress);
            row._task = Some(task);
        }
        cx.notify();
    }

    fn uninstall_progress(&mut self, status: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(progress) = self
            .uninstall
            .as_ref()
            .and_then(|row| row.progress.as_ref())
        {
            let _ = progress.update(ProgressDialogUpdate::new().status(status), window, cx);
        }
    }

    fn close_uninstall_progress(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(progress) = self.uninstall.as_mut().and_then(|row| row.progress.take()) {
            let _ = progress.complete(window, cx);
        }
    }

    fn uninstall_ended(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.uninstall.as_mut() {
            row.working = false;
            row.progress = None;
            row._task = None;
        }
        cx.notify();
    }

    /// Says why the uninstall stopped.
    fn uninstall_stopped(
        &mut self,
        failure: RemovalFailure,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_uninstall_progress(window, cx);
        let message = failure_message(failure, &self.here());
        self.present_uninstall_end(
            "Couldn\u{2019}t Uninstall SpaceTerm",
            message,
            false,
            window,
            cx,
        );
    }

    /// Says what was removed and what stayed. Once SpaceTerm Core is gone, the app has nothing
    /// left to run, so it quits.
    fn uninstalled(&mut self, ended: Ended, window: &mut Window, cx: &mut Context<Self>) {
        self.close_uninstall_progress(window, cx);
        let (title, message) = ended_message(ended, &self.here(), self.on_mac());
        self.present_uninstall_end(title, message, true, window, cx);
    }

    /// The last alert. With `quits`, its one action quits SpaceTerm.
    fn present_uninstall_end(
        &mut self,
        title: &'static str,
        message: String,
        quits: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = if quits {
            ModalAction::new((), "Quit", ModalActionRole::Affirmative, QUIT_SELECTOR)
        } else {
            ModalAction::new((), "OK", ModalActionRole::Affirmative, DISMISS_SELECTOR)
        }
        .with_emphasis(ModalActionEmphasis::Prominent);
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-uninstall-spaceterm-end"),
            title,
            title,
            message,
            vec![action],
        )
        .intent(if quits {
            AlertIntent::Informational
        } else {
            AlertIntent::Warning
        })
        .present(window, cx, move |_, cx| {
            if quits {
                cx.dispatch_action(&crate::app::QuitUninstalledApplication);
            } else {
                let _ = owner.update(cx, |settings, cx| settings.uninstall_ended(cx));
            }
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm uninstall result");
            if quits {
                cx.dispatch_action(&crate::app::QuitUninstalledApplication);
            } else {
                self.uninstall_ended(cx);
            }
        }
    }
}

/// Runs `update` in the window `handle` names once the current update returns.
fn in_window(
    handle: AnyWindowHandle,
    cx: &mut App,
    update: impl FnOnce(&mut Window, &mut App) + 'static,
) {
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, cx| update(window, cx));
    });
}

/// The production removal: this account's SpaceTerm Core service, the installed app and the
/// data the person may ask to delete.
struct InstalledSpaceTerm {
    service: Service,
    app: InstalledApp,
    data: DesktopData,
}

/// The removal this build offers, or `None` when it doesn't run from an installed app.
pub(super) fn installed(service: &Service) -> Option<Arc<dyn SpaceTermRemoval>> {
    let identity = ApplicationIdentity::current().directory_name();
    let app = InstalledApp::current(identity)?;
    Some(Arc::new(InstalledSpaceTerm {
        service: service.clone(),
        app,
        data: DesktopData {
            cli: spaceterm_core::cli_state_root(identity)
                .ok()
                .map(ConnectionStore::new),
            open: Box::new(move |kind| credentials::for_cli_record(identity, kind)),
            roots: AppDirectories::resolve(identity).ok().map(|directories| {
                vec![
                    directories.config,
                    directories.data,
                    directories.state,
                    directories.cache,
                ]
            }),
        },
    }))
}

/// Opens the credential store a command-line tool record names.
type OpenStore =
    dyn Fn(StoreKind) -> Result<Arc<dyn CredentialStore>, CredentialError> + Send + Sync;

/// The data "Also delete..." deletes besides SpaceTerm Core's own.
struct DesktopData {
    /// The command-line tool's Saved Connections, or `None` when their place can't be found.
    cli: Option<ConnectionStore>,
    open: Box<OpenStore>,
    /// This desktop's settings, data, state and cache directories, or `None` when they can't
    /// be found.
    roots: Option<Vec<PathBuf>>,
}

impl DesktopData {
    fn cli_credentials(&self) -> CliCredentials {
        match self.cli.as_ref().map(ConnectionStore::credential_items) {
            Some(Ok(items)) => CliCredentials {
                items,
                unread: false,
            },
            _ => CliCredentials {
                items: Vec::new(),
                unread: true,
            },
        }
    }

    fn delete(
        &self,
        cli: CliCredentials,
        machines: Option<DirectConnections>,
    ) -> Result<(), DataKept> {
        let mut kept = cli.unread;
        for (kind, key) in &cli.items {
            kept |= (self.open)(*kind)
                .and_then(|store| credentials::remove_asked(store.as_ref(), key))
                .is_err();
        }
        // The registry removes its keys before the directory that holds its records goes.
        kept |= machines.is_none_or(|machines| machines.forget_every_machine().is_err());
        match &self.roots {
            Some(roots) => {
                for root in roots {
                    kept |= remove_directory(root).is_err();
                }
            }
            None => kept = true,
        }
        if kept { Err(DataKept) } else { Ok(()) }
    }
}

impl SpaceTermRemoval for InstalledSpaceTerm {
    fn observe(&self) -> Result<Observed, RemovalFailure> {
        // A preview removes nothing, even while SpaceTerm Core is stopped.
        let preview = self
            .service
            .preview_uninstall(Data::Keep)
            .map_err(RemovalFailure::of_core)?;
        Ok(Observed::new(preview.sessions))
    }

    fn cli_credentials(&self) -> CliCredentials {
        self.data.cli_credentials()
    }

    fn remove_core(&self, observed: &Observed, data: Data) -> Result<(), RemovalFailure> {
        // The approval binds the data choice the person just made, so it is previewed again
        // with that choice. SpaceTerm Core refuses the token if anything moved since.
        let preview = self
            .service
            .preview_uninstall(data)
            .map_err(RemovalFailure::of_core)?;
        if Observed::new(preview.sessions) != *observed {
            return Err(RemovalFailure::Changed);
        }
        match self
            .service
            .uninstall_cli(Some(&preview.token), data)
            .map_err(RemovalFailure::of_core)?
        {
            CliUninstall::Completed => Ok(()),
            CliUninstall::Preview { .. } => Err(RemovalFailure::Changed),
        }
    }

    fn delete_data(
        &self,
        cli: CliCredentials,
        machines: Option<DirectConnections>,
    ) -> Result<(), DataKept> {
        self.data.delete(cli, machines)
    }

    fn remove_app(&self) -> Result<(), AppKept> {
        self.app.remove().map_err(|_| AppKept)
    }
}

#[cfg(test)]
pub(super) mod testing {
    use std::sync::Mutex;

    use super::*;

    /// One step a scripted removal was asked to take.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(in crate::ui::settings_window) enum Step {
        Observe,
        ReadCliCredentials,
        RemoveCore(Observed, Data),
        /// Deleting the data, and whether it was given the desktop's registry.
        DeleteData {
            machines: bool,
        },
        RemoveApp,
    }

    /// Removal steps whose answers a test sets and whose requests it records.
    pub(in crate::ui::settings_window) struct ScriptedRemoval {
        pub(in crate::ui::settings_window) observed: Mutex<Result<Observed, RemovalFailure>>,
        pub(in crate::ui::settings_window) core: Mutex<Result<(), RemovalFailure>>,
        pub(in crate::ui::settings_window) app: Mutex<Result<(), AppKept>>,
        pub(in crate::ui::settings_window) data: Mutex<Result<(), DataKept>>,
        pub(in crate::ui::settings_window) steps: Mutex<Vec<Step>>,
    }

    impl ScriptedRemoval {
        pub(in crate::ui::settings_window) fn new(sessions: usize) -> Arc<Self> {
            let sessions = (0..sessions)
                .map(|index| [u8::try_from(index).unwrap(); 32])
                .collect();
            Arc::new(Self {
                observed: Mutex::new(Ok(Observed::new(sessions))),
                core: Mutex::new(Ok(())),
                app: Mutex::new(Ok(())),
                data: Mutex::new(Ok(())),
                steps: Mutex::default(),
            })
        }

        pub(in crate::ui::settings_window) fn steps(&self) -> Vec<Step> {
            self.steps.lock().unwrap().clone()
        }

        fn record(&self, step: Step) {
            self.steps.lock().unwrap().push(step);
        }
    }

    impl SpaceTermRemoval for ScriptedRemoval {
        fn observe(&self) -> Result<Observed, RemovalFailure> {
            self.record(Step::Observe);
            self.observed.lock().unwrap().clone()
        }

        fn cli_credentials(&self) -> CliCredentials {
            self.record(Step::ReadCliCredentials);
            CliCredentials::default()
        }

        fn remove_core(&self, observed: &Observed, data: Data) -> Result<(), RemovalFailure> {
            self.record(Step::RemoveCore(observed.clone(), data));
            *self.core.lock().unwrap()
        }

        fn delete_data(
            &self,
            _: CliCredentials,
            machines: Option<DirectConnections>,
        ) -> Result<(), DataKept> {
            self.record(Step::DeleteData {
                machines: machines.is_some(),
            });
            *self.data.lock().unwrap()
        }

        fn remove_app(&self) -> Result<(), AppKept> {
            self.record(Step::RemoveApp);
            *self.app.lock().unwrap()
        }
    }
}

#[cfg(test)]
#[path = "uninstall_tests.rs"]
mod tests;
