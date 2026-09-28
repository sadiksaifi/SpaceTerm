//! Application-owned update policy. The platform adapter owns transport and installation.

#[cfg(feature = "development-app")]
pub(crate) mod preview;

use std::rc::Rc;
use std::time::Duration;

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
        self.version = None;
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

type ResumeQuit = Rc<dyn Fn(&mut App)>;

pub(crate) struct ApplicationUpdates {
    model: UpdateModel,
    adapter: Rc<dyn UpdateAdapter>,
    pending_quit: Option<ResumeQuit>,
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
                // Give the first window time to become usable. Checks never download or install.
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                loop {
                    if this.update(cx, |this, cx| this.check(false, cx)).is_err() {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_secs(24 * 60 * 60))
                        .await;
                }
            });
            Self {
                model: UpdateModel::new(available),
                adapter,
                pending_quit: None,
                _events: events,
                _schedule: schedule,
            }
        });
        cx.set_global(UpdateService(entity));
    }

    pub(crate) fn state(&self) -> &UpdateState {
        &self.model.state
    }

    pub(crate) fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if !self.model.begin_check(manual) {
            return;
        }
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
        let finished = matches!(event, UpdateEvent::Finished);
        if let Some(notice) = self.model.receive(event) {
            cx.emit(notice);
        }
        if finished && let Some(resume) = self.pending_quit.take() {
            cx.defer(move |cx| resume(cx));
        }
        cx.notify();
    }
}

/// Quitting must cancel an unconfirmed staged installer before the process terminates.
pub(crate) fn cancel_before_quit(cx: &mut App, resume: ResumeQuit) -> bool {
    let Some(service) = cx.try_global::<UpdateService>().cloned() else {
        return false;
    };
    service.0.update(cx, |updates, cx| {
        if !updates.model.cycle_active || updates.model.install_authorized {
            return false;
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
    fn ordinary_quit_waits_for_staged_update_cancellation_and_revokes_open_confirmation(
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
            assert!(cancel_before_quit(
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
        assert_eq!(adapter.cancellations.get(), 1);
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(resumes.get(), 1);
        cx.update(|cx| {
            assert!(!cancel_before_quit(
                cx,
                Rc::new(|_| panic!("no update remains"))
            ))
        });
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(resumes.get(), 1);
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
