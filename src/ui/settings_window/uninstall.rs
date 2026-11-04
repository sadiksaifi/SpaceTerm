//! Settings > Advanced > Uninstall SpaceTerm. One confirmed step removes SpaceTerm Core and then
//! the SpaceTerm app from this computer, and SpaceTerm quits. Workspaces, paired devices and
//! settings stay unless the person opts in to deleting them.
//!
//! The alert names the Terminal Sessions that end, as SpaceTerm Core observed them. SpaceTerm
//! Core refuses the uninstall when those sessions changed before the person confirmed.

use std::path::{Path, PathBuf};
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

/// Why an uninstall step stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemovalFailure {
    /// The Terminal Sessions changed after the person was asked, so nothing was removed.
    Changed,
    /// SpaceTerm Core couldn't be removed. Trying again finishes a partial removal.
    Core,
    /// SpaceTerm Core is removed, and the app couldn't remove itself.
    App,
    /// SpaceTerm and SpaceTerm Core are removed, and some of the data the person asked to delete
    /// stayed.
    Data,
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
pub(super) trait SpaceTermRemoval: Send + Sync {
    /// Reads the Terminal Sessions an uninstall ends. Removes nothing.
    fn observe(&self) -> Result<Observed, RemovalFailure>;
    /// Removes SpaceTerm Core and, with [`Data::Delete`], its data. Refuses with
    /// [`RemovalFailure::Changed`] when the sessions differ from `observed`.
    fn remove_core(&self, observed: &Observed, data: Data) -> Result<(), RemovalFailure>;
    /// Removes the SpaceTerm app this process runs from.
    fn remove_app(&self) -> Result<(), RemovalFailure>;
    /// Deletes this desktop's own settings, saved Machines and state.
    fn delete_desktop_data(&self) -> Result<(), RemovalFailure>;
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

/// What the person reads when an uninstall step stops.
pub(super) fn failure_message(failure: RemovalFailure, here: &str, mac: bool) -> String {
    match failure {
        RemovalFailure::Changed => format!(
            "The running sessions on {here} changed, so nothing was uninstalled. Try again."
        ),
        RemovalFailure::Core => {
            format!("SpaceTerm couldn\u{2019}t remove SpaceTerm Core from {here}, so try again.")
        }
        RemovalFailure::App if mac => "SpaceTerm Core was removed, but SpaceTerm couldn\u{2019}t \
                                       remove itself, so move SpaceTerm to the Trash."
            .to_owned(),
        RemovalFailure::App => "SpaceTerm Core was removed, but SpaceTerm couldn\u{2019}t remove \
                                itself, so run install.sh --uninstall."
            .to_owned(),
        RemovalFailure::Data => format!(
            "SpaceTerm and SpaceTerm Core were removed from {here}, but SpaceTerm couldn\u{2019}t \
             delete all of its Workspaces, paired devices and settings."
        ),
    }
}

/// What the person reads once SpaceTerm and SpaceTerm Core are gone.
pub(super) fn done_message(data: Data, here: &str) -> String {
    let what = match data {
        Data::Keep => "kept",
        Data::Delete => "deleted",
    };
    format!(
        "SpaceTerm and SpaceTerm Core were removed from {here}. Workspaces, paired devices and \
         settings were {what}."
    )
}

impl SettingsWindow {
    fn here(&self) -> String {
        self.this_computer.to_lowercase()
    }

    fn on_mac(&self) -> bool {
        self.this_computer == "This Mac"
    }

    /// The Advanced row's description.
    pub(super) fn uninstall_description(&self) -> &'static str {
        if self.on_mac() {
            "Removes SpaceTerm and SpaceTerm Core from this Mac."
        } else {
            "Removes SpaceTerm and SpaceTerm Core from this computer."
        }
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
                    .spawn(async move { removal.remove_core(&observed, data) })
                    .await
            };
            if let Err(failure) = core {
                let _ = settings.update_in(cx, |settings, window, cx| {
                    settings.uninstall_stopped(failure, window, cx);
                });
                return;
            }
            let _ = settings.update_in(cx, |settings, window, cx| {
                settings.uninstall_progress(
                    format!("Removing SpaceTerm from {here}\u{2026}"),
                    window,
                    cx,
                );
            });
            let app = {
                let removal = Arc::clone(&removal);
                cx.background_executor()
                    .spawn(async move { removal.remove_app() })
                    .await
            };
            if let Err(failure) = app {
                let _ = settings.update_in(cx, |settings, window, cx| {
                    settings.uninstall_stopped(failure, window, cx);
                });
                return;
            }
            let mut deleted = Ok(());
            if data == Data::Delete {
                let _ = settings.update(cx, |settings, _| settings.forget_every_machine());
                deleted = cx
                    .background_executor()
                    .spawn(async move { removal.delete_desktop_data() })
                    .await;
            }
            let _ = settings.update_in(cx, |settings, window, cx| match deleted {
                Ok(()) => settings.uninstalled(data, window, cx),
                Err(failure) => settings.uninstall_stopped(failure, window, cx),
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

    /// The desktop's saved Machines and their keys go with the data the person asked to delete.
    fn forget_every_machine(&mut self) {
        let Some(rows) = self.direct_routes.as_ref() else {
            return;
        };
        for (route, _) in rows.connections.routes() {
            let _ = rows.connections.forget(&route);
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
        let message = failure_message(failure, &self.here(), self.on_mac());
        // Once SpaceTerm Core is gone, the app has nothing left to run.
        let quits = matches!(failure, RemovalFailure::App | RemovalFailure::Data);
        self.present_uninstall_end(
            "Couldn\u{2019}t Uninstall SpaceTerm",
            message,
            quits,
            window,
            cx,
        );
    }

    fn uninstalled(&mut self, data: Data, window: &mut Window, cx: &mut Context<Self>) {
        self.close_uninstall_progress(window, cx);
        let message = done_message(data, &self.here());
        self.present_uninstall_end("SpaceTerm Was Uninstalled", message, true, window, cx);
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
                cx.quit();
            } else {
                let _ = owner.update(cx, |settings, cx| settings.uninstall_ended(cx));
            }
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm uninstall result");
            if quits {
                cx.quit();
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

/// The SpaceTerm this process runs from, as its installer laid it out.
#[derive(Clone, Debug, Eq, PartialEq)]
enum InstalledApp {
    /// `<bundle>.app/Contents/MacOS/<executable>`.
    Bundle(PathBuf),
    /// `<root>/bin/spaceterm-desktop` beside `<root>/share/spaceterm`, with the launcher, icon and
    /// desktop entry `install.sh` added for `application_id`.
    Tree {
        root: PathBuf,
        home: PathBuf,
        data_home: PathBuf,
        application_id: &'static str,
    },
}

impl InstalledApp {
    /// The installed app running `executable`, or `None` for a build that runs from elsewhere,
    /// such as a source checkout.
    fn locate(executable: &Path) -> Option<Self> {
        if cfg!(target_os = "macos") {
            let macos = executable.parent()?;
            let contents = macos.parent()?;
            let bundle = contents.parent()?;
            return (macos.file_name()? == "MacOS"
                && contents.file_name()? == "Contents"
                && bundle.extension()? == "app")
                .then(|| Self::Bundle(bundle.to_path_buf()));
        }
        let bin = executable.parent()?;
        let root = bin.parent()?;
        if executable.file_name()? != "spaceterm-desktop"
            || bin.file_name()? != "bin"
            || !root.join("share").join("spaceterm").is_dir()
        {
            return None;
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())?;
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".local").join("share"));
        Some(Self::Tree {
            root: root.to_path_buf(),
            home,
            data_home,
            application_id: ApplicationIdentity::current().application_id(),
        })
    }

    /// Removes what `install.sh` installed for this app and nothing else.
    fn remove(&self) -> std::io::Result<()> {
        match self {
            Self::Bundle(bundle) => remove_directory(bundle),
            Self::Tree {
                root,
                home,
                data_home,
                application_id,
            } => {
                let launcher = home.join(".local").join("bin").join("spaceterm-desktop");
                if std::fs::read_link(&launcher)
                    .is_ok_and(|target| target == root.join("bin").join("spaceterm-desktop"))
                {
                    std::fs::remove_file(&launcher)?;
                }
                let icon = data_home
                    .join("icons/hicolor/scalable/apps")
                    .join(format!("{application_id}.svg"));
                if std::fs::symlink_metadata(&icon).is_ok_and(|icon| icon.is_symlink()) {
                    std::fs::remove_file(&icon)?;
                }
                remove_file(
                    &data_home
                        .join("applications")
                        .join(format!("{application_id}.desktop")),
                )?;
                if let (Some(parent), Some(name)) = (root.parent(), root.file_name()) {
                    let mut staging = std::ffi::OsString::from(".");
                    staging.push(name);
                    staging.push(".update");
                    remove_directory(&parent.join(staging))?;
                }
                remove_directory(root)
            }
        }
    }
}

fn remove_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Removes a directory and everything in it. A symbolic link in its place is refused.
fn remove_directory(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => Err(std::io::ErrorKind::InvalidInput.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The production removal: this account's SpaceTerm Core service, the installed app and this
/// desktop's own directories.
struct InstalledSpaceTerm {
    service: Service,
    app: InstalledApp,
    directories: Option<AppDirectories>,
}

/// The removal this build offers, or `None` when it doesn't run from an installed app.
pub(super) fn installed(service: &Service) -> Option<Arc<dyn SpaceTermRemoval>> {
    let app = InstalledApp::locate(&std::env::current_exe().ok()?)?;
    Some(Arc::new(InstalledSpaceTerm {
        service: service.clone(),
        app,
        directories: AppDirectories::resolve(ApplicationIdentity::current().directory_name()).ok(),
    }))
}

impl SpaceTermRemoval for InstalledSpaceTerm {
    fn observe(&self) -> Result<Observed, RemovalFailure> {
        // A stopped Core uninstalls without a preview, so only a running one is asked.
        let status = self.service.status().map_err(RemovalFailure::of_core)?;
        if !status.running {
            return Ok(Observed::default());
        }
        match self
            .service
            .uninstall_cli(None, Data::Keep)
            .map_err(RemovalFailure::of_core)?
        {
            CliUninstall::Preview { sessions, .. } => Ok(Observed::new(sessions)),
            // The Core stopped between the two reads and is already removed.
            CliUninstall::Completed => Ok(Observed::default()),
        }
    }

    fn remove_core(&self, observed: &Observed, data: Data) -> Result<(), RemovalFailure> {
        // The approval binds the data choice, so the uninstall previews again with it.
        match self
            .service
            .uninstall_cli(None, data)
            .map_err(RemovalFailure::of_core)?
        {
            CliUninstall::Completed => Ok(()),
            CliUninstall::Preview {
                token, sessions, ..
            } => {
                if Observed::new(sessions) != *observed {
                    return Err(RemovalFailure::Changed);
                }
                match self
                    .service
                    .uninstall_cli(Some(&token), data)
                    .map_err(RemovalFailure::of_core)?
                {
                    CliUninstall::Completed => Ok(()),
                    CliUninstall::Preview { .. } => Err(RemovalFailure::Changed),
                }
            }
        }
    }

    fn remove_app(&self) -> Result<(), RemovalFailure> {
        self.app.remove().map_err(|_| RemovalFailure::App)
    }

    fn delete_desktop_data(&self) -> Result<(), RemovalFailure> {
        let directories = self.directories.as_ref().ok_or(RemovalFailure::Data)?;
        let mut result = Ok(());
        for root in [
            &directories.config,
            &directories.data,
            &directories.state,
            &directories.cache,
        ] {
            if remove_directory(root).is_err() {
                result = Err(RemovalFailure::Data);
            }
        }
        result
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
        RemoveCore(Observed, Data),
        RemoveApp,
        DeleteDesktopData,
    }

    /// Removal steps whose answers a test sets and whose requests it records.
    pub(in crate::ui::settings_window) struct ScriptedRemoval {
        pub(in crate::ui::settings_window) observed: Mutex<Result<Observed, RemovalFailure>>,
        pub(in crate::ui::settings_window) core: Mutex<Result<(), RemovalFailure>>,
        pub(in crate::ui::settings_window) app: Mutex<Result<(), RemovalFailure>>,
        pub(in crate::ui::settings_window) data: Mutex<Result<(), RemovalFailure>>,
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

        fn remove_core(&self, observed: &Observed, data: Data) -> Result<(), RemovalFailure> {
            self.record(Step::RemoveCore(observed.clone(), data));
            *self.core.lock().unwrap()
        }

        fn remove_app(&self) -> Result<(), RemovalFailure> {
            self.record(Step::RemoveApp);
            *self.app.lock().unwrap()
        }

        fn delete_desktop_data(&self) -> Result<(), RemovalFailure> {
            self.record(Step::DeleteDesktopData);
            *self.data.lock().unwrap()
        }
    }
}

#[cfg(test)]
#[path = "uninstall_tests.rs"]
mod tests;
