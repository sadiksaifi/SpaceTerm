//! Application-owned update policy. The platform adapter owns transport and installation.

#[cfg(all(feature = "development-app", target_os = "macos"))]
pub(crate) mod preview;

pub(crate) mod policy;

use policy::{UpdateHistory, UpdatePreferences, UpdateStage};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Task};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UpdateState {
    Unavailable,
    Idle,
    Checking,
    UpToDate,
    Available {
        version: String,
    },
    Downloading {
        version: String,
        received: u64,
        total: u64,
    },
    Verifying {
        version: String,
    },
    Ready {
        version: String,
    },
    Installing {
        version: String,
    },
    Failed {
        error: UpdateError,
    },
}

#[cfg_attr(
    all(not(spaceterm_sparkle), not(test)),
    expect(
        dead_code,
        reason = "native failures are produced by the packaged updater"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum UpdateError {
    #[error("Updates are available in the installed release of SpaceTerm.")]
    Unavailable,
    #[error("SpaceTerm could not check for updates. Try again when you are online.")]
    Check,
    #[error("The update could not be downloaded. Please try again.")]
    Download,
    #[error("The update could not be verified. Please try again later.")]
    Verification,
    #[error("The update could not be installed. Please try again.")]
    Installation,
    #[error("Install SpaceTerm in Applications before updating it.")]
    ReadOnly,
}

#[cfg_attr(
    not(spaceterm_sparkle),
    expect(
        dead_code,
        reason = "native events are produced by the packaged updater"
    )
)]
#[derive(Clone, Debug)]
pub(crate) enum UpdateEvent {
    Available(String),
    /// Publication time from the signed feed; prepared means Sparkle retained an installer.
    ReleaseMetadata {
        published_at: u64,
        prepared: bool,
    },
    UpToDate,
    Downloading {
        received: u64,
        total: u64,
    },
    Verifying,
    Ready,
    Installing,
    Failed(UpdateError),
    /// The adapter has ended its cycle, including cancellation of a staged installer.
    Finished,
}

pub(crate) trait UpdateAdapter {
    fn start(&self, events: async_channel::Sender<UpdateEvent>) -> Result<(), UpdateError>;
    fn check(&self) -> Result<(), UpdateError>;
    fn download(&self) -> Result<(), UpdateError>;
    fn cancel(&self);
    fn install(&self) -> Result<(), UpdateError>;
    /// Preserve a verified installer on ordinary quit without requesting termination.
    fn finish_on_quit(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn load_history(&self) -> UpdateHistory {
        UpdateHistory::default()
    }
    fn save_history(&self, _history: &UpdateHistory) {}
    fn preview_running_session(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UpdateNotice {
    CheckFinished,
    ReadyToInstall,
    Failed,
}

/// A confirmation authorizes exactly the release that was ready when it was presented.
#[derive(Clone, Debug)]
pub(crate) struct InstallConfirmation {
    generation: u64,
    pub(crate) version: String,
}

struct UpdateModel {
    state: UpdateState,
    version: Option<String>,
    generation: u64,
    confirmation_open: bool,
    install_authorized: bool,
    cancelling: bool,
    cycle_active: bool,
    manual_check: bool,
}

impl UpdateModel {
    fn new(available: bool) -> Self {
        Self {
            state: if available {
                UpdateState::Idle
            } else {
                UpdateState::Unavailable
            },
            version: None,
            generation: 0,
            confirmation_open: false,
            install_authorized: false,
            cancelling: false,
            cycle_active: false,
            manual_check: false,
        }
    }

    fn begin_check(&mut self, manual: bool) -> bool {
        if self.cycle_active || matches!(self.state, UpdateState::Unavailable) {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        self.confirmation_open = false;
        self.install_authorized = false;
        self.cancelling = false;
        self.cycle_active = true;
        self.manual_check = manual;
        self.state = UpdateState::Checking;
        true
    }

    fn confirmation(&mut self) -> Option<InstallConfirmation> {
        if self.confirmation_open || self.cancelling {
            return None;
        }
        let UpdateState::Ready { version } = &self.state else {
            return None;
        };
        self.confirmation_open = true;
        Some(InstallConfirmation {
            generation: self.generation,
            version: version.clone(),
        })
    }

    fn confirm(&mut self, confirmation: InstallConfirmation, accepted: bool) -> bool {
        if self.generation != confirmation.generation || !self.confirmation_open {
            return false;
        }
        self.confirmation_open = false;
        if !accepted
            || self.cancelling
            || !matches!(&self.state, UpdateState::Ready { version } if *version == confirmation.version)
        {
            return false;
        }
        self.install_authorized = true;
        self.state = UpdateState::Installing {
            version: confirmation.version,
        };
        true
    }

    fn receive(&mut self, event: UpdateEvent) -> Option<UpdateNotice> {
        if !self.cycle_active {
            return None;
        }
        if matches!(event, UpdateEvent::Finished) {
            self.cycle_active = false;
            if self.cancelling
                || matches!(
                    self.state,
                    UpdateState::Checking
                        | UpdateState::Available { .. }
                        | UpdateState::Downloading { .. }
                        | UpdateState::Verifying { .. }
                        | UpdateState::Ready { .. }
                )
            {
                self.state = UpdateState::Idle;
            }
            self.cancelling = false;
            self.confirmation_open = false;
            return None;
        }
        if self.cancelling {
            return None;
        }
        match event {
            UpdateEvent::Available(version) if matches!(self.state, UpdateState::Checking) => {
                if stable_version(&version).is_none() {
                    self.state = UpdateState::Failed {
                        error: UpdateError::Verification,
                    };
                    return Some(UpdateNotice::Failed);
                }
                self.version = Some(version.clone());
                self.state = UpdateState::Available { version };
                return self.manual_check.then_some(UpdateNotice::CheckFinished);
            }
            UpdateEvent::UpToDate => {
                self.version = None;
                self.state = UpdateState::UpToDate;
                return self.manual_check.then_some(UpdateNotice::CheckFinished);
            }
            UpdateEvent::Downloading { received, total } => {
                if let Some(version) = self.version.clone() {
                    self.state = UpdateState::Downloading {
                        version,
                        received,
                        total,
                    };
                }
            }
            UpdateEvent::Verifying => {
                if let Some(version) = self.version.clone() {
                    self.state = UpdateState::Verifying { version };
                }
            }
            UpdateEvent::Ready => {
                if let Some(version) = self.version.clone() {
                    self.state = UpdateState::Ready { version };
                    return Some(UpdateNotice::ReadyToInstall);
                }
            }
            UpdateEvent::Installing => {
                if self.install_authorized
                    && let Some(version) = self.version.clone()
                {
                    self.state = UpdateState::Installing { version };
                }
            }
            UpdateEvent::Failed(error) => {
                let visible = self.manual_check || self.version.is_some();
                self.state = UpdateState::Failed { error };
                return visible.then_some(UpdateNotice::Failed);
            }
            _ => {}
        }
        None
    }
}

#[cfg(any(spaceterm_sparkle, test))]
pub(crate) fn allows_release(version: &str, display: &str, url: &str, informational: bool) -> bool {
    !informational
        && stable_version(version).is_some()
        && version == display
        && url
            == format!(
                "https://github.com/sadiksaifi/SpaceTerm/releases/download/v{version}/SpaceTerm-{version}-darwin-arm64.dmg"
            )
}

pub(crate) fn stable_version(value: &str) -> Option<[u64; 3]> {
    if value.len() > 48 {
        return None;
    }
    let mut parts = value.split('.');
    let mut result = [0; 3];
    for number in &mut result {
        let part = parts.next()?;
        if part.is_empty()
            || part.len() > 1 && part.starts_with('0')
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        *number = part.parse().ok()?;
    }
    parts.next().is_none().then_some(result)
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaunchState {
    Open,
    Checking,
    Required,
    Installing,
}

type ResumeQuit = Rc<dyn Fn(&mut App)>;

pub(crate) struct ApplicationUpdates {
    model: UpdateModel,
    adapter: Rc<dyn UpdateAdapter>,
    pending_quit: Option<ResumeQuit>,
    launch: LaunchState,
    resume_launch: Option<ResumeQuit>,
    settings: Option<crate::settings::UserSettings>,
    history: UpdateHistory,
    stage: UpdateStage,
    reminder: Option<UpdateStage>,
    publication: u64,
    prepared: bool,
    automatic_download: bool,
    download_requested: bool,
    last_attempt: u64,
    _launch_timeout: Option<Task<()>>,
    _events: Task<()>,
    _schedule: Task<()>,
}

impl EventEmitter<UpdateNotice> for ApplicationUpdates {}

#[derive(Clone)]
pub(crate) struct UpdateService(pub(crate) Entity<ApplicationUpdates>);
impl Global for UpdateService {}

impl ApplicationUpdates {
    pub(crate) fn install(adapter: Rc<dyn UpdateAdapter>, cx: &mut App) {
        let (sender, receiver) = async_channel::unbounded();
        let available = adapter.start(sender).is_ok();
        let entity = cx.new(|cx: &mut Context<Self>| {
            let events = cx.spawn(async move |this, cx| {
                while let Ok(event) = receiver.recv().await {
                    if this.update(cx, |this, cx| this.receive(event, cx)).is_err() {
                        break;
                    }
                }
            });
            let schedule = cx.spawn(async move |this, cx| {
                if !available {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
                loop {
                    if this.update(cx, |this, cx| this.tick(now(), cx)).is_err() {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_secs(60))
                        .await;
                }
            });
            let history = adapter.load_history();
            let mut model = UpdateModel::new(available);
            if available {
                model.version = history
                    .pending_version(env!("SPACETERM_VERSION"))
                    .map(str::to_owned);
            }
            Self {
                history,
                launch: LaunchState::Open,
                resume_launch: None,
                settings: None,
                stage: UpdateStage::Optional,
                reminder: None,
                publication: 0,
                prepared: false,
                automatic_download: false,
                download_requested: false,
                last_attempt: 0,
                _launch_timeout: None,
                model,
                adapter,
                pending_quit: None,
                _events: events,
                _schedule: schedule,
            }
        });
        cx.set_global(UpdateService(entity));
    }

    pub(crate) fn attach_settings(&mut self, settings: crate::settings::UserSettings) {
        self.settings = Some(settings);
    }

    fn preferences(&self) -> UpdatePreferences {
        self.settings
            .as_ref()
            .map(|settings| settings.snapshot().committed.updates)
            .unwrap_or_default()
    }

    pub(crate) fn launch_state(&self) -> LaunchState {
        self.launch
    }
    pub(crate) fn reminder(&self) -> Option<UpdateStage> {
        self.reminder
    }
    pub(crate) fn last_check(&self) -> Option<u64> {
        self.history.last_check
    }

    pub(crate) fn dismiss_reminder(&mut self, cx: &mut Context<Self>) {
        self.reminder = None;
        self.history.reminded(now());
        self.adapter.save_history(&self.history);
        cx.notify();
    }

    /// Begins once, before any Workspace or Terminal Session exists. A bounded check and
    /// download stall timeout release access; later completion then requires confirmation.
    pub(crate) fn begin_launch(&mut self, resume: ResumeQuit, cx: &mut Context<Self>) {
        self.resume_launch = Some(resume);
        if matches!(self.model.state, UpdateState::Unavailable)
            || self.adapter.preview_running_session()
        {
            self.open_launch(cx);
            self.check(false, cx);
            return;
        }
        self.launch = LaunchState::Checking;
        self.arm_launch_timeout(Duration::from_secs(10), cx);
        self.check(false, cx);
        cx.notify();
    }

    fn arm_launch_timeout(&mut self, duration: Duration, cx: &mut Context<Self>) {
        self._launch_timeout = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| this.open_launch(cx));
        }));
    }

    pub(crate) fn release_launch(cx: &mut App) {
        if let Some(service) = cx.try_global::<UpdateService>().cloned() {
            service.0.update(cx, |updates, cx| {
                if updates.launch != LaunchState::Installing {
                    updates.open_launch(cx);
                }
            });
        }
    }

    fn open_launch(&mut self, cx: &mut Context<Self>) {
        self.launch = LaunchState::Open;
        self._launch_timeout = None;
        if let Some(resume) = self.resume_launch.take() {
            cx.defer(move |cx| resume(cx));
        }
        cx.notify();
    }

    fn tick(&mut self, time: u64, cx: &mut Context<Self>) {
        let preferences = self.preferences();
        self.stage = self.history.stage(time);
        self.adapter.save_history(&self.history);
        let outstanding = self.model.version.is_some()
            && !matches!(
                self.model.state,
                UpdateState::UpToDate | UpdateState::Installing { .. }
            );
        if outstanding
            && self.launch == LaunchState::Open
            && cx.active_window().is_some()
            && self
                .history
                .reminder_due(time, preferences.reminder_interval)
        {
            self.reminder = Some(self.stage);
            self.history.reminded(time);
            self.adapter.save_history(&self.history);
        }
        if !self.prepared
            && preferences.automatic_downloads
            && matches!(self.model.state, UpdateState::Available { .. })
            && !self.model.cancelling
        {
            self.automatic_download = true;
            self.download(cx);
        }
        if time.saturating_sub(self.last_attempt) >= preferences.check_interval.seconds() {
            self.check(false, cx);
        }
        cx.notify();
    }

    /// Retained presentation metadata, never authority to install without a verified check.
    pub(crate) fn pending_version(&self) -> Option<&str> {
        self.model.version.as_deref()
    }

    pub(crate) fn retry_download(&mut self, cx: &mut Context<Self>) {
        self.check(true, cx);
        if matches!(self.model.state, UpdateState::Checking) {
            self.download_requested = true;
        }
    }

    pub(crate) fn state(&self) -> &UpdateState {
        &self.model.state
    }

    pub(crate) fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if !self.model.begin_check(manual) {
            return;
        }
        self.last_attempt = now();
        self.publication = 0;
        self.prepared = false;
        self.automatic_download = false;
        self.download_requested = false;
        if let Err(error) = self.adapter.check() {
            self.receive(UpdateEvent::Failed(error), cx);
            self.receive(UpdateEvent::Finished, cx);
        }
        cx.notify();
    }

    pub(crate) fn download(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Available { version } = &self.model.state else {
            return;
        };
        self.model.state = UpdateState::Downloading {
            version: version.clone(),
            received: 0,
            total: 0,
        };
        if let Err(error) = self.adapter.download() {
            self.receive(UpdateEvent::Failed(error), cx);
        }
        cx.notify();
    }

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.model.cycle_active && !self.model.install_authorized {
            self.model.cancelling = true;
            self.model.confirmation_open = false;
            self.adapter.cancel();
            cx.notify();
        }
    }

    pub(crate) fn is_cancelling(&self) -> bool {
        self.model.cancelling
    }

    /// Claimed once across all windows; dropping the prompt must settle it with `false`.
    pub(crate) fn begin_install_confirmation(&mut self) -> Option<InstallConfirmation> {
        self.model.confirmation()
    }

    pub(crate) fn finish_install_confirmation(
        &mut self,
        token: InstallConfirmation,
        accepted: bool,
        cx: &mut Context<Self>,
    ) {
        if self.model.confirm(token, accepted)
            && let Err(error) = self.adapter.install()
        {
            self.receive(UpdateEvent::Failed(error), cx);
        }
        cx.notify();
    }

    pub(crate) fn request_install_confirmation(&mut self, cx: &mut Context<Self>) {
        if matches!(self.model.state, UpdateState::Ready { .. }) && !self.model.confirmation_open {
            cx.emit(UpdateNotice::ReadyToInstall);
        }
    }

    /// Retry the already authorized restart if the application's quit confirmation was cancelled.
    pub(crate) fn retry_install(&mut self, cx: &mut Context<Self>) {
        if self.model.install_authorized
            && matches!(self.model.state, UpdateState::Installing { .. })
            && let Err(error) = self.adapter.install()
        {
            self.receive(UpdateEvent::Failed(error), cx);
        }
    }

    fn receive(&mut self, event: UpdateEvent, cx: &mut Context<Self>) {
        if !self.model.cycle_active {
            return;
        }
        if let UpdateEvent::ReleaseMetadata {
            published_at,
            prepared,
        } = event
        {
            self.publication = published_at;
            self.prepared = prepared;
            return;
        }
        let time = now();
        let found = matches!(event, UpdateEvent::Available(_));
        let ready = matches!(event, UpdateEvent::Ready);
        let failed = matches!(event, UpdateEvent::Failed(_));
        let current = matches!(event, UpdateEvent::UpToDate);
        if let UpdateEvent::Available(version) = &event {
            self.history
                .observe(env!("SPACETERM_VERSION"), version, self.publication, time);
            self.history.last_check = Some(time);
            self.stage = self.history.stage(time);
            self.adapter.save_history(&self.history);
        }
        if current {
            self.history.clear();
            self.history.last_check = Some(time);
            self.stage = UpdateStage::Optional;
            self.reminder = None;
            self.adapter.save_history(&self.history);
        }
        let finished = matches!(event, UpdateEvent::Finished);
        let progress = matches!(
            event,
            UpdateEvent::Downloading { .. } | UpdateEvent::Verifying
        );
        let notice = self.model.receive(event);
        if found && matches!(self.model.state, UpdateState::Available { .. }) {
            if self.launch == LaunchState::Checking {
                if self.prepared || self.stage == UpdateStage::Overdue {
                    self.launch = LaunchState::Required;
                    self.arm_launch_timeout(Duration::from_secs(30), cx);
                } else {
                    self.open_launch(cx);
                }
            }
            if !self.prepared
                && (self.download_requested
                    || self.preferences().automatic_downloads
                    || self.launch == LaunchState::Required)
            {
                self.automatic_download = !self.model.manual_check && !self.download_requested;
                self.download_requested = false;
                self.download(cx);
            }
            self.tick(time, cx);
        }
        if progress && self.launch != LaunchState::Open {
            self.arm_launch_timeout(Duration::from_secs(30), cx);
        }
        if ready && self.launch != LaunchState::Open && !self.model.cancelling {
            self.model.install_authorized = true;
            if let Some(version) = self.model.version.clone() {
                self.model.state = UpdateState::Installing { version };
                self.launch = LaunchState::Installing;
                // Once termination is authorized, never create work beneath a pending installer.
                self._launch_timeout = None;
                if let Err(error) = self.adapter.install() {
                    self.receive(UpdateEvent::Failed(error), cx);
                }
            }
        }
        if failed || current || (finished && self.launch != LaunchState::Installing) {
            self.open_launch(cx);
        }
        if let Some(notice) = notice {
            // Download completion is quiet. Only an explicit restart action opens a confirmation.
            if notice != UpdateNotice::ReadyToInstall
                && self.launch == LaunchState::Open
                && !(notice == UpdateNotice::Failed && self.automatic_download)
            {
                cx.emit(notice);
            }
        }
        if finished && let Some(resume) = self.pending_quit.take() {
            cx.defer(move |cx| resume(cx));
        }
        cx.notify();
    }
}

/// Ordinary quit can finish a verified installer without forcing termination or relaunch.
pub(crate) fn prepare_before_quit(cx: &mut App, resume: ResumeQuit) -> bool {
    let Some(service) = cx.try_global::<UpdateService>().cloned() else {
        return false;
    };
    service.0.update(cx, |updates, cx| {
        // Quit is authorized. Neither updater cleanup nor a timeout may resume startup.
        updates.resume_launch = None;
        updates._launch_timeout = None;
        if !updates.model.cycle_active || updates.model.install_authorized {
            return false;
        }
        if matches!(updates.model.state, UpdateState::Ready { .. }) {
            updates.model.confirmation_open = false;
            if updates.adapter.finish_on_quit().is_ok() {
                updates.model.install_authorized = true;
                return false;
            }
        }
        if updates.pending_quit.is_none() {
            updates.pending_quit = Some(resume);
        }
        updates.cancel(cx);
        true
    })
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    #[derive(Default)]
    pub(crate) struct RecordingAdapter {
        available: bool,
        pub(crate) events: std::cell::RefCell<Option<async_channel::Sender<UpdateEvent>>>,
        pub(crate) cancellations: std::cell::Cell<usize>,
        pub(crate) installations: std::cell::Cell<usize>,
        pub(crate) downloads: std::cell::Cell<usize>,
        pub(crate) deferred_installs: std::cell::Cell<usize>,
        pub(crate) history: UpdateHistory,
    }

    impl UpdateAdapter for RecordingAdapter {
        fn start(&self, sender: async_channel::Sender<UpdateEvent>) -> Result<(), UpdateError> {
            if !self.available {
                return Err(UpdateError::Unavailable);
            }
            *self.events.borrow_mut() = Some(sender);
            Ok(())
        }
        fn check(&self) -> Result<(), UpdateError> {
            Ok(())
        }
        fn download(&self) -> Result<(), UpdateError> {
            self.downloads.set(self.downloads.get() + 1);
            Ok(())
        }
        fn load_history(&self) -> UpdateHistory {
            self.history.clone()
        }
        fn finish_on_quit(&self) -> Result<(), UpdateError> {
            self.deferred_installs.set(self.deferred_installs.get() + 1);
            Ok(())
        }
        fn cancel(&self) {
            self.cancellations.set(self.cancellations.get() + 1);
        }
        fn install(&self) -> Result<(), UpdateError> {
            self.installations.set(self.installations.get() + 1);
            Ok(())
        }
    }

    impl RecordingAdapter {
        pub(crate) fn available() -> Self {
            Self {
                available: true,
                ..Self::default()
            }
        }
        pub(crate) fn emit(&self, event: UpdateEvent) {
            self.events
                .borrow()
                .as_ref()
                .unwrap()
                .try_send(event)
                .unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::RecordingAdapter;
    use super::*;

    #[gpui::test]
    fn ordinary_quit_preserves_verified_update_and_revokes_open_confirmation(
        cx: &mut gpui::TestAppContext,
    ) {
        let adapter = Rc::new(RecordingAdapter::available());
        let resumes = Rc::new(std::cell::Cell::new(0));
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.download(cx));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        let token = service.update(cx, |updates, _| {
            updates.begin_install_confirmation().unwrap()
        });
        cx.update(|cx| {
            let resumed = resumes.clone();
            assert!(!prepare_before_quit(
                cx,
                Rc::new(move |_| resumed.set(resumed.get() + 1))
            ));
        });
        service.update(cx, |updates, cx| {
            updates.finish_install_confirmation(token, true, cx)
        });
        cx.run_until_parked();
        assert_eq!(resumes.get(), 0);
        assert_eq!(adapter.installations.get(), 0);
        assert_eq!(adapter.cancellations.get(), 0);
        assert_eq!(adapter.deferred_installs.get(), 1);
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(resumes.get(), 0);
        cx.update(|cx| {
            assert!(!prepare_before_quit(
                cx,
                Rc::new(|_| panic!("no update remains"))
            ))
        });
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(resumes.get(), 0);
    }

    #[gpui::test]
    fn ordinary_quit_cancels_an_incomplete_download_before_resuming_once(
        cx: &mut gpui::TestAppContext,
    ) {
        let adapter = Rc::new(RecordingAdapter::available());
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        let resumes = Rc::new(std::cell::Cell::new(0));
        let resumed = resumes.clone();
        cx.update(|cx| {
            assert!(prepare_before_quit(
                cx,
                Rc::new(move |_| resumed.set(resumed.get() + 1))
            ))
        });
        assert_eq!(resumes.get(), 0);
        assert_eq!(adapter.cancellations.get(), 1);
        adapter.emit(UpdateEvent::Finished);
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(resumes.get(), 1);
        assert_eq!(adapter.deferred_installs.get(), 0);
    }

    fn launch_fixture(
        cx: &mut gpui::TestAppContext,
    ) -> (
        Rc<RecordingAdapter>,
        Entity<ApplicationUpdates>,
        Rc<std::cell::Cell<usize>>,
    ) {
        let adapter = Rc::new(RecordingAdapter::available());
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        let opened = Rc::new(std::cell::Cell::new(0));
        service.update(cx, |updates, cx| {
            let opened = opened.clone();
            updates.begin_launch(Rc::new(move |_| opened.set(opened.get() + 1)), cx);
        });
        cx.run_until_parked();
        (adapter, service, opened)
    }

    #[gpui::test]
    fn fresh_launch_waits_for_an_overdue_update_without_creating_work(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, service, opened) = launch_fixture(cx);
        let document = crate::appearance::SettingsDocument {
            updates: UpdatePreferences {
                automatic_downloads: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = crate::settings::UserSettings::load(
            crate::ui::settings_window::test_support::MemoryStorage::with_document(&document),
        );
        service.update(cx, |updates, _| updates.attach_settings(settings));
        adapter.emit(UpdateEvent::ReleaseMetadata {
            published_at: now() - 2 * policy::DAY,
            prepared: false,
        });
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(opened.get(), 0);
        assert_eq!(adapter.downloads.get(), 1);
        assert_eq!(
            service.read_with(cx, |updates, _| updates.launch_state()),
            LaunchState::Required
        );
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        assert_eq!(adapter.installations.get(), 1);
        assert_eq!(opened.get(), 0);
        // A slow installer must never gain a Workspace underneath its authorized termination.
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        assert_eq!(opened.get(), 0);
    }

    #[gpui::test]
    fn prepared_update_installs_on_launch_even_before_deadline(cx: &mut gpui::TestAppContext) {
        let (adapter, _, opened) = launch_fixture(cx);
        adapter.emit(UpdateEvent::ReleaseMetadata {
            published_at: now(),
            prepared: true,
        });
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        assert_eq!(adapter.downloads.get(), 0);
        assert_eq!(adapter.installations.get(), 1);
        assert_eq!(opened.get(), 0);
    }

    #[gpui::test]
    fn offline_or_failed_update_releases_launch_once(cx: &mut gpui::TestAppContext) {
        let (adapter, service, opened) = launch_fixture(cx);
        adapter.emit(UpdateEvent::ReleaseMetadata {
            published_at: now() - 2 * policy::DAY,
            prepared: false,
        });
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Failed(UpdateError::Download));
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(opened.get(), 1);
        assert_eq!(
            service.read_with(cx, |updates, _| updates.launch_state()),
            LaunchState::Open
        );
        assert_eq!(adapter.installations.get(), 0);
    }

    #[gpui::test]
    fn timed_out_check_cannot_auto_install_later_over_active_work(cx: &mut gpui::TestAppContext) {
        let (adapter, service, opened) = launch_fixture(cx);
        cx.executor().advance_clock(Duration::from_secs(10));
        cx.run_until_parked();
        assert_eq!(opened.get(), 1);
        adapter.emit(UpdateEvent::ReleaseMetadata {
            published_at: now() - 2 * policy::DAY,
            prepared: false,
        });
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        assert_eq!(adapter.installations.get(), 0);
        assert_eq!(
            service.read_with(cx, |updates, _| updates.launch_state()),
            LaunchState::Open
        );
        let confirmation = service.update(cx, |updates, _| {
            updates.begin_install_confirmation().unwrap()
        });
        service.update(cx, |updates, cx| {
            updates.finish_install_confirmation(confirmation, true, cx)
        });
        assert_eq!(adapter.installations.get(), 1);
    }

    #[gpui::test]
    fn saved_download_opt_out_keeps_optional_updates_available_for_manual_download(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, service, opened) = launch_fixture(cx);
        let document = crate::appearance::SettingsDocument {
            updates: UpdatePreferences {
                automatic_downloads: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = crate::settings::UserSettings::load(
            crate::ui::settings_window::test_support::MemoryStorage::with_document(&document),
        );
        service.update(cx, |updates, _| updates.attach_settings(settings));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(opened.get(), 1);
        assert_eq!(adapter.downloads.get(), 0);
        service.update(cx, |updates, cx| updates.download(cx));
        assert_eq!(adapter.downloads.get(), 1);
        adapter.emit(UpdateEvent::Failed(UpdateError::Download));
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.retry_download(cx));
        assert_eq!(
            service.read_with(cx, |updates, _| updates
                .pending_version()
                .map(str::to_owned)),
            Some("0.1.1".into())
        );
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(adapter.downloads.get(), 2);
    }

    #[gpui::test]
    fn optional_background_download_is_quiet_and_does_not_hold_launch(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, service, opened) = launch_fixture(cx);
        let notices = Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = notices.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&service, move |_, event, _| {
                received.borrow_mut().push(*event)
            })
        });
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(opened.get(), 1);
        assert_eq!(adapter.downloads.get(), 1);
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        assert!(notices.borrow().is_empty());
        assert_eq!(adapter.installations.get(), 0);
    }

    #[gpui::test]
    fn known_pending_release_stays_visible_on_an_offline_launch(cx: &mut gpui::TestAppContext) {
        let mut history = UpdateHistory::default();
        history.observe("0.1.0", "0.1.1", now() - policy::DAY, now());
        let mut adapter = RecordingAdapter::available();
        adapter.history = serde_json::from_slice(&serde_json::to_vec(&history).unwrap()).unwrap();
        let adapter = Rc::new(adapter);
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        service.update(cx, |updates, cx| updates.begin_launch(Rc::new(|_| {}), cx));
        adapter.emit(UpdateEvent::Failed(UpdateError::Check));
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(
            service.read_with(cx, |updates, _| updates
                .pending_version()
                .map(str::to_owned)),
            Some("0.1.1".into())
        );
        assert_eq!(
            service.read_with(cx, |updates, _| updates.launch_state()),
            LaunchState::Open
        );
    }

    #[test]
    fn pending_release_survives_cancellation_and_failed_rechecks_until_confirmed_current() {
        let mut model = ready();
        let later = model.confirmation().unwrap();
        assert!(!model.confirm(later, false));
        model.cancelling = true;
        model.receive(UpdateEvent::Finished);
        assert_eq!(model.version.as_deref(), Some("0.1.1"));
        assert!(model.begin_check(true));
        assert_eq!(model.version.as_deref(), Some("0.1.1"));
        model.receive(UpdateEvent::Failed(UpdateError::Check));
        model.receive(UpdateEvent::Finished);
        assert_eq!(model.version.as_deref(), Some("0.1.1"));
        assert!(model.begin_check(true));
        model.receive(UpdateEvent::UpToDate);
        assert_eq!(model.version, None);
    }

    fn ready() -> UpdateModel {
        let mut model = UpdateModel::new(true);
        model.begin_check(false);
        model.receive(UpdateEvent::Available("0.1.1".into()));
        model.receive(UpdateEvent::Ready);
        model
    }

    #[test]
    fn downloaded_update_requires_one_explicit_confirmation_for_that_release() {
        let mut model = ready();
        assert!(!model.install_authorized);
        let token = model.confirmation().unwrap();
        assert!(model.confirmation().is_none());
        assert!(!model.confirm(token.clone(), false));
        assert_eq!(
            model.state,
            UpdateState::Ready {
                version: "0.1.1".into()
            }
        );
        let accepted = model.confirmation().unwrap();
        assert!(model.confirm(accepted, true));
        assert!(!model.confirm(token, true));
    }

    #[test]
    fn cancellation_revokes_confirmation_and_waits_for_installer_to_finish() {
        let mut model = ready();
        let token = model.confirmation().unwrap();
        model.cancelling = true;
        model.receive(UpdateEvent::Ready);
        assert!(!model.confirm(token, true));
        assert!(model.cycle_active);
        model.receive(UpdateEvent::Finished);
        assert_eq!(model.state, UpdateState::Idle);
        assert!(!model.cycle_active);
    }

    #[test]
    fn checks_are_serial_and_background_results_do_not_interrupt_work() {
        let mut model = UpdateModel::new(true);
        assert!(model.begin_check(false));
        assert!(!model.begin_check(true));
        assert_eq!(model.receive(UpdateEvent::UpToDate), None);
        model.receive(UpdateEvent::Finished);
        assert!(model.begin_check(true));
        assert_eq!(
            model.receive(UpdateEvent::UpToDate),
            Some(UpdateNotice::CheckFinished)
        );
    }

    #[test]
    fn remote_versions_are_bounded_stable_numbers_before_being_presented() {
        for bad in [
            "0.1.0-beta.1",
            "01.1.0",
            "1.2",
            "1.2.3.4",
            "1.2.3\n",
            "<script>",
            "999999999999999999999999.0.0",
        ] {
            assert_eq!(stable_version(bad), None);
        }
        assert_eq!(stable_version("0.1.0"), Some([0, 1, 0]));
    }

    #[test]
    fn release_metadata_cannot_redirect_installation_to_another_asset_or_repository() {
        let expected = "https://github.com/sadiksaifi/SpaceTerm/releases/download/v0.1.1/SpaceTerm-0.1.1-darwin-arm64.dmg";
        assert!(allows_release("0.1.1", "0.1.1", expected, false));
        for url in [
            expected.replace("https:", "http:"),
            expected.replace("SpaceTerm/releases", "other/releases"),
            expected.replace("github.com", "github.com.evil.invalid"),
            format!("{expected}?redirect=1"),
            expected.replace("arm64.dmg", "arm64.pkg"),
        ] {
            assert!(!allows_release("0.1.1", "0.1.1", &url, false));
        }
        assert!(!allows_release("0.1.1", "0.2.0", expected, false));
        assert!(!allows_release("0.1.1", "0.1.1", expected, true));
    }
}
