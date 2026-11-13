//! Application-owned update policy. The platform adapter owns transport and installation.

#[cfg(feature = "developer-tools")]
pub(crate) mod preview;

pub(crate) mod confirm;
pub(crate) mod copy;
pub(crate) mod ladder;
pub(crate) mod machines;
pub(crate) mod policy;
pub(crate) mod push;
pub(crate) mod release_feed;
pub(crate) mod release_notes;
pub(crate) mod reminders;
pub(crate) mod state;

use ladder::Stage;
use policy::{UpdateHistory, UpdatePreferences};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};

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
    all(not(spaceterm_sparkle), not(target_os = "linux"), not(test)),
    expect(
        dead_code,
        reason = "native failures are produced by the packaged updater"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum UpdateError {
    #[error("Updates require the installed release of SpaceTerm.")]
    Unavailable,
    #[error("Try again when you are online.")]
    Check,
    #[error("Check your network, then try again.")]
    Download,
    #[error("Try again later.")]
    Verification,
    #[error("Try the update again.")]
    Installation,
    /// The update ran out of disk space while it unpacked or staged the release.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(dead_code, reason = "only the Linux updater stages releases itself")
    )]
    #[error("Free up disk space, then try the update again.")]
    DiskFull,
    /// The newer release needs a newer system library than this computer has, so SpaceTerm
    /// can't install it.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Sparkle skips a release for a newer macOS by itself"
        )
    )]
    #[error(
        "The newest SpaceTerm needs a newer system than this computer runs. Update its operating system to install it."
    )]
    SystemTooOld,
    /// Desktop wording names the remedy, which differs by platform.
    #[error("SpaceTerm can\u{2019}t replace its installation.")]
    ReadOnly,
    /// SpaceTerm was still running when its approved relaunch ended or ran out of time. The update
    /// stays downloaded and Try Again relaunches into it.
    #[error("SpaceTerm couldn\u{2019}t quit to install the update.")]
    Relaunch,
    /// Settings couldn't be saved when the approved relaunch began, so SpaceTerm kept running.
    /// The update stays downloaded and Try Again relaunches into it.
    #[error("SpaceTerm couldn\u{2019}t save Settings, so it didn\u{2019}t relaunch.")]
    SettingsUnsaved,
}

impl UpdateError {
    /// Whether an approved relaunch failed, with its release still downloaded.
    pub(crate) fn relaunch_failed(self) -> bool {
        matches!(self, Self::Relaunch | Self::SettingsUnsaved)
    }
}

#[cfg_attr(
    all(not(spaceterm_sparkle), not(target_os = "linux")),
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

/// The adapter of an identity that receives no updates.
pub(crate) struct UnavailableUpdates;

impl UpdateAdapter for UnavailableUpdates {
    fn start(&self, _: async_channel::Sender<UpdateEvent>) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn check(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn download(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn cancel(&self) {}
    fn install(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
}

/// Fetches release metadata and archives over HTTPS only. Callers classify failures without
/// their native detail.
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        dead_code,
        reason = "only the Linux updater downloads release archives itself"
    )
)]
pub(crate) trait UpdateTransport: Send + Sync {
    /// Returns the body of a successful response, refusing one longer than `limit` bytes.
    fn get(&self, url: &str, limit: usize) -> std::io::Result<Vec<u8>>;
    /// Streams the body of a successful response, failing a read past `limit` bytes.
    fn open(&self, url: &str, limit: u64) -> std::io::Result<Box<dyn std::io::Read + Send>>;
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
    /// Settings are saved and SpaceTerm may quit for the approved relaunch. Returns true when the
    /// updater now asks SpaceTerm to quit by itself, once it holds the relaunch. Until then an
    /// ordinary quit installs the release without starting SpaceTerm again.
    fn release_relaunch(&self) -> bool {
        false
    }
    fn load_history(&self) -> UpdateHistory {
        UpdateHistory::default()
    }
    fn save_history(&self, _history: &UpdateHistory) {}
    /// Seconds since the Unix epoch, the clock checks and reminders are scheduled by.
    fn now(&self) -> u64 {
        now()
    }
    /// Calls `changed` after each change to the system clock, from any thread. The scheduler then
    /// measures its next due time again, since its timer counts elapsed time, not the clock.
    fn observe_clock_changes(&self, _changed: Box<dyn Fn() + Send + Sync>) {}
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
    /// The updater still holds the installer of a relaunch that ran out of time. It can no longer
    /// cancel it or end its cycle, so only a quit installs it. Dismissing the failure keeps it.
    installer_held: bool,
    /// The updater holds the approved relaunch and relaunches on any quit, which SpaceTerm can't
    /// take back. Until the updater ends its cycle, every quit is that relaunch.
    relaunch_released: bool,
    manual_check: bool,
    /// How many failures this run reported, which names the current one.
    failures: u64,
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
            installer_held: false,
            relaunch_released: false,
            manual_check: false,
            failures: 0,
        }
    }

    fn fail(&mut self, error: UpdateError) {
        self.failures += 1;
        self.state = UpdateState::Failed { error };
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
        self.relaunch_released = false;
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

    /// The ready release, for a Relaunch chosen anywhere. It answers a confirmation that is
    /// already open, which then settles without effect.
    fn relaunch_ready(&mut self) -> Option<InstallConfirmation> {
        if self.cancelling {
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

    /// Whether the cycle only waits for the person, so a due check may end it and check again:
    /// a release found and not downloaded, or downloaded and not yet relaunched.
    fn replaceable(&self) -> bool {
        self.cycle_active
            && !self.cancelling
            && !self.confirmation_open
            && !self.install_authorized
            && !self.installer_held
            && matches!(
                self.state,
                UpdateState::Available { .. } | UpdateState::Ready { .. }
            )
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
            self.installer_held = false;
            self.relaunch_released = false;
            // The updater gave up its installer while SpaceTerm kept running.
            if !self.cancelling && matches!(self.state, UpdateState::Installing { .. }) {
                self.confirmation_open = false;
                self.fail_relaunch(UpdateError::Relaunch);
                return Some(UpdateNotice::Failed);
            }
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
                    self.fail(UpdateError::Verification);
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
                // A release this computer's system can't run is news only to a person who
                // asked. A scheduled check finds the same one again.
                let visible = self.manual_check
                    || (self.version.is_some() && error != UpdateError::SystemTooOld);
                self.install_authorized = false;
                self.fail(error);
                return visible.then_some(UpdateNotice::Failed);
            }
            _ => {}
        }
        None
    }

    /// The approved relaunch did not quit SpaceTerm. Quit requests follow ADR 0049 again, and the
    /// release stays pending for Try Again.
    fn fail_relaunch(&mut self, error: UpdateError) {
        self.install_authorized = false;
        self.installer_held = self.cycle_active;
        self.fail(error);
    }

    /// Approves the relaunch again after it failed while the updater still holds its installer.
    fn retry_relaunch(&mut self) -> bool {
        if !self.installer_held {
            return false;
        }
        let Some(version) = self.version.clone() else {
            return false;
        };
        self.installer_held = false;
        self.install_authorized = true;
        self.state = UpdateState::Installing { version };
        true
    }
}

/// How long an approved relaunch may take before SpaceTerm reports that it is still running.
/// How long a download that SpaceTerm tries again by itself keeps failing before the title bar
/// shows it. Settings and the menu bar extra show it at once.
const RETRY_QUIETLY_FOR: u64 = policy::DAY;
pub(crate) const RELAUNCH_DEADLINE: Duration = Duration::from_secs(30);

/// Whether Sparkle may install the appcast item: a stable release whose enclosure is that
/// release's disk image, named as the release publishes it.
#[cfg(any(spaceterm_sparkle, test))]
pub(crate) fn allows_release(version: &str, display: &str, url: &str, informational: bool) -> bool {
    use spaceterm_core::update::{facts::Version, feed};
    let Some(release) = Version::parse(version)
        .ok()
        .filter(|v| v.release().is_some())
    else {
        return false;
    };
    !informational
        && version == display
        && url
            == format!(
                "{}/download/v{version}/{}",
                feed::RELEASES,
                feed::desktop_dmg(&release)
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

pub(crate) type ResumeQuit = Rc<dyn Fn(&mut App)>;

/// What SpaceTerm's relaunch by itself observes about the person's use of SpaceTerm.
pub(crate) trait Presence {
    /// Whether an alert, sheet or menu is open, portable or native. A window that can't be read
    /// counts as presenting.
    fn presenting(&self, cx: &mut App) -> bool;
    /// A count that grows with every keyboard, input-method and pointer event SpaceTerm
    /// receives, counted before any view can handle it. `None` where the platform can't observe
    /// all of them: SpaceTerm then never counts as unused.
    fn input(&self, cx: &mut App) -> Option<u64>;
}

/// Whether a portable alert, sheet or menu is open in any SpaceTerm window.
pub(crate) fn portable_presenting(cx: &mut App) -> bool {
    cx.windows().into_iter().any(|handle| {
        handle
            .update(cx, |_, window, cx| {
                spaceterm_ui::window_modal_is_open(window, cx)
                    || spaceterm_ui::window_menu_is_open(window, cx)
            })
            .unwrap_or(true)
    })
}

/// [`Presence`] where SpaceTerm can read only its portable alerts and menus. GPUI dispatch can't
/// show every key and pointer event (a view may consume one, an input method may bypass
/// dispatch), so input is unobservable and SpaceTerm never relaunches by itself. Linux uses it.
pub(crate) struct PortablePresence;

impl Presence for PortablePresence {
    fn presenting(&self, cx: &mut App) -> bool {
        portable_presenting(cx)
    }

    fn input(&self, _: &mut App) -> Option<u64> {
        None
    }
}

/// The platform's [`Presence`]: AppKit on macOS, where every input event can be counted; the
/// portable one elsewhere and off the main thread.
fn platform_presence() -> Rc<dyn Presence> {
    #[cfg(target_os = "macos")]
    if let Some(presence) = crate::platform::macos_presence::MacosPresence::install() {
        return Rc::new(presence);
    }
    Rc::new(PortablePresence)
}

/// How long SpaceTerm waits to look again when the idle period has passed but an alert, sheet,
/// menu or unsaved Settings holds its relaunch.
const IDLE_RECHECK: u64 = 5 * 60;

pub(crate) struct ApplicationUpdates {
    model: UpdateModel,
    adapter: Rc<dyn UpdateAdapter>,
    pending_quit: Option<ResumeQuit>,
    settings: Option<crate::settings::Settings>,
    history: UpdateHistory,
    stage: Stage,
    reminder: Option<Stage>,
    /// This launch is the first run on a newer version than the last launch.
    updated_this_launch: bool,
    presence: Rc<dyn Presence>,
    /// The last keyboard, input-method or pointer input to SpaceTerm on the update clock. The
    /// launch counts as input.
    last_input: u64,
    /// The input count as last read. `None` while the platform can't observe input.
    seen_input: Option<u64>,
    publication: u64,
    prepared: bool,
    automatic_download: bool,
    /// When an automatic download first failed, while SpaceTerm tries it again by itself. A
    /// finished download, a current SpaceTerm or the person's own try ends it.
    retrying_since: Option<u64>,
    /// A download the person started failed. That failure is theirs to see until a download
    /// finishes, SpaceTerm is current, a different release is found, or they dismiss it or
    /// stop a try, so no later try dates it.
    own_download_failed: bool,
    /// The failure the person dismissed on this desktop, by [`UpdateModel::failures`]. Its
    /// title-bar step stays hidden until a newer failure replaces it.
    dismissed_failure: Option<u64>,
    download_requested: bool,
    /// The person chose Relaunch before the release was ready, so it relaunches once it is.
    relaunch_requested: bool,
    /// The downloaded release a scheduled check replaces with the same or a newer one, until a
    /// release is ready again, SpaceTerm is current or the person checks. The main area keeps
    /// its Relaunch meanwhile (design 4.5).
    replacing: Option<String>,
    /// A due check ended a cycle that only waited for the person, so a check follows once the
    /// updater finishes it.
    recheck: bool,
    /// The check that follows is one the person asked for.
    recheck_manual: bool,
    /// How many relaunches the person approved, which names the current one.
    relaunch_approvals: u64,
    last_attempt: u64,
    /// The history as last written, so a write happens only when the contents change.
    saved_history: UpdateHistory,
    /// Wakes the scheduler to measure its next due time again.
    reschedule: async_channel::Sender<()>,
    /// Settings changes, until the scheduler takes the receiver.
    settings_changes: Option<async_channel::Receiver<()>>,
    /// Fails an approved relaunch that has not quit SpaceTerm in time.
    _relaunch_deadline: Option<Task<()>>,
    _events: Task<()>,
    _schedule: Task<()>,
    _wake: Subscription,
}

/// What ended one wait of the update scheduler.
enum ScheduleWake {
    Due,
    Changed,
    SettingsClosed,
}

impl EventEmitter<UpdateNotice> for ApplicationUpdates {}

#[derive(Clone)]
pub(crate) struct UpdateService(pub(crate) Entity<ApplicationUpdates>);
impl Global for UpdateService {}

impl ApplicationUpdates {
    pub(crate) fn install(adapter: Rc<dyn UpdateAdapter>, cx: &mut App) {
        Self::install_with(adapter, platform_presence(), cx);
    }

    /// Installs the update service with `presence` as the source of what SpaceTerm shows the
    /// person, for its relaunch by itself.
    pub(crate) fn install_with(
        adapter: Rc<dyn UpdateAdapter>,
        presence: Rc<dyn Presence>,
        cx: &mut App,
    ) {
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
            let (reschedule, rescheduled) = async_channel::bounded(1);
            // One timer runs to the next due check or reminder. Changes that move that time wake
            // the scheduler instead, so it does nothing between due times.
            let schedule = cx.spawn(async move |this, cx| {
                if !available {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let mut settings: Option<async_channel::Receiver<()>> = None;
                loop {
                    let Ok((wait, attached)) = this.update(cx, |this, cx| {
                        let time = this.adapter.now();
                        this.tick(time, cx);
                        this.relaunch_if_unused(time, cx);
                        let wait = this.next_due(time).map(|due| due - time);
                        (wait, this.settings_changes.take())
                    }) else {
                        break;
                    };
                    if attached.is_some() {
                        settings = attached;
                    }
                    let timer = wait.map(|seconds| {
                        cx.background_executor().timer(Duration::from_secs(seconds))
                    });
                    let due = async {
                        match timer {
                            Some(timer) => timer.await,
                            None => std::future::pending().await,
                        }
                        ScheduleWake::Due
                    };
                    let changed = async {
                        let _ = rescheduled.recv().await;
                        ScheduleWake::Changed
                    };
                    let edited = async {
                        match &settings {
                            Some(changes) if changes.recv().await.is_ok() => ScheduleWake::Changed,
                            Some(_) => ScheduleWake::SettingsClosed,
                            None => std::future::pending().await,
                        }
                    };
                    let woke =
                        futures_lite::future::or(due, futures_lite::future::or(changed, edited));
                    if matches!(woke.await, ScheduleWake::SettingsClosed) {
                        settings = None;
                    }
                }
            });
            {
                let reschedule = reschedule.clone();
                adapter.observe_clock_changes(Box::new(move || {
                    let _ = reschedule.try_send(());
                }));
            }
            // Sleep moves wall time while the elapsed-time timer stands still.
            let wake = {
                let source = crate::system_wake::source(cx);
                cx.subscribe(&source, |this, _, _: &crate::system_wake::Woke, _| {
                    this.reschedule()
                })
            };
            let seen_input = presence.input(cx);
            let saved_history = adapter.load_history();
            let mut history = saved_history.clone();
            let updated_this_launch = history.launched(env!("SPACETERM_VERSION"), adapter.now());
            let mut model = UpdateModel::new(available);
            if available {
                model.version = history
                    .pending_version(env!("SPACETERM_VERSION"))
                    .map(str::to_owned);
            }
            let mut updates = Self {
                saved_history,
                history,
                settings: None,
                stage: Stage::Offer,
                reminder: None,
                updated_this_launch,
                presence,
                last_input: adapter.now(),
                seen_input,
                publication: 0,
                prepared: false,
                automatic_download: false,
                retrying_since: None,
                own_download_failed: false,
                dismissed_failure: None,
                download_requested: false,
                relaunch_requested: false,
                replacing: None,
                recheck: false,
                recheck_manual: false,
                relaunch_approvals: 0,
                last_attempt: 0,
                reschedule,
                settings_changes: None,
                _relaunch_deadline: None,
                model,
                adapter,
                pending_quit: None,
                _events: events,
                _schedule: schedule,
                _wake: wake,
            };
            updates.save_history();
            updates
        });
        cx.set_global(UpdateService(entity));
    }

    pub(crate) fn attach_settings(&mut self, settings: crate::settings::Settings) {
        self.settings_changes = Some(settings.subscribe());
        self.settings = Some(settings);
        self.reschedule();
    }

    /// SpaceTerm has no active window, which lets a due reminder appear. While a window is
    /// active the title-bar pill shows the step instead.
    pub(crate) fn left_spaceterm(&self) {
        self.reschedule();
    }

    fn reschedule(&self) {
        let _ = self.reschedule.try_send(());
    }

    /// Writes the history only when it differs from what was written last.
    fn save_history(&mut self) {
        if self.history != self.saved_history {
            self.adapter.save_history(&self.history);
            self.saved_history = self.history.clone();
        }
    }

    /// The next time after `time` at which a check or a reminder is due, if time alone brings
    /// one. A check waits for a running cycle to finish, and a due reminder waits for a window.
    fn next_due(&mut self, time: u64) -> Option<u64> {
        let preferences = self.preferences();
        let check = (!self.model.cycle_active || self.model.replaceable())
            .then(|| {
                self.last_attempt
                    .saturating_add(preferences.check_interval.seconds())
            })
            .filter(|due| *due > time);
        let change = self.history.next_change(time).filter(|due| *due > time);
        let observed = self.seen_input.is_some();
        let unused = (observed && self.relaunches_when_unused(time)).then(|| {
            Some(self.last_input.saturating_add(ladder::IDLE_PERIOD))
                .filter(|due| *due > time)
                .unwrap_or(time + IDLE_RECHECK)
        });
        check.into_iter().chain(change).chain(unused).min()
    }

    /// Whether SpaceTerm relaunches into its downloaded update by itself once nobody uses it:
    /// from the Ask stage, while the release is downloaded and nothing about it is in progress.
    fn relaunches_when_unused(&mut self, time: u64) -> bool {
        matches!(self.model.state, UpdateState::Ready { .. })
            && !self.model.cancelling
            && !self.model.confirmation_open
            && !self.relaunch_requested
            && self.relaunch_approval().is_none()
            && self.history.stage(time) >= Stage::Ask
    }

    /// Relaunches into the downloaded update once nobody has used SpaceTerm for
    /// [`ladder::IDLE_PERIOD`]: no keyboard or pointer input to SpaceTerm, no alert, sheet or
    /// menu open, and Settings saved. It takes the approved-relaunch path, so no quit warning
    /// appears and every Terminal Session keeps running (ADR 0049).
    /// The scheduler reads the input count at each wake, so input between two wakes counts as
    /// input at the later one. Where input can't be observed, it never relaunches.
    fn relaunch_if_unused(&mut self, time: u64, cx: &mut Context<Self>) {
        let input = self.presence.input(cx);
        if input != self.seen_input {
            self.last_input = self.last_input.max(time);
        }
        self.seen_input = input;
        if input.is_none() || !self.relaunches_when_unused(time) {
            return;
        }
        if time < self.last_input.saturating_add(ladder::IDLE_PERIOD)
            || self.presence.presenting(cx)
            || !self.settings_saved()
        {
            return;
        }
        self.relaunch(cx);
    }

    /// Whether Settings hold no edit that has not been saved.
    fn settings_saved(&self) -> bool {
        self.settings.as_ref().is_none_or(|settings| {
            let snapshot = settings.snapshot();
            std::sync::Arc::ptr_eq(&snapshot.committed, &snapshot.candidate)
                || *snapshot.committed == *snapshot.candidate
        })
    }

    fn preferences(&self) -> UpdatePreferences {
        self.settings
            .as_ref()
            .map(|settings| settings.snapshot().committed.updates)
            .unwrap_or_default()
    }

    /// The stage of a reminder that is due now and waits to be shown.
    pub(crate) fn reminder(&self) -> Option<Stage> {
        self.reminder
    }
    /// The ladder stage of the waiting SpaceTerm update, from the earliest publication among
    /// the versions it has waited for. Offer when nothing waits.
    pub(crate) fn ladder_stage(&self) -> Stage {
        self.stage
    }
    /// When the waiting SpaceTerm update started waiting, in Unix seconds, if one waits.
    pub(crate) fn waiting_since(&self) -> Option<u64> {
        self.history.waiting_since()
    }
    /// The time on the update clock, in Unix seconds.
    pub(crate) fn now(&self) -> u64 {
        self.adapter.now()
    }
    pub(crate) fn last_check(&self) -> Option<u64> {
        self.history.last_check
    }

    /// Whether a person asked for the current or last check, from Settings or the menu.
    pub(crate) fn manual_check(&self) -> bool {
        self.model.manual_check
    }

    /// Whether this build came by an update whose release notes the person has not opened.
    /// Observers of [`UpdateService`] hear when this changes.
    pub(crate) fn release_notes_unread(&self) -> bool {
        self.history.notes_unread(env!("SPACETERM_VERSION"))
    }

    /// Whether `What's New` is owed: this launch is the first run on a newer version, and its
    /// release notes have not opened since. It never returns on a later launch. Whether the
    /// build carries notes is the presentation's to check.
    pub(crate) fn whats_new(&self) -> bool {
        self.updated_this_launch && self.release_notes_unread()
    }

    /// Finished updates of this desktop and every Machine, newest first.
    #[allow(dead_code, reason = "Recent is no longer recorded or shown")]
    pub(crate) fn recent(&self) -> &[policy::RecentUpdate] {
        self.history.recent()
    }

    /// Records that the person opened this build's release notes.
    pub(crate) fn read_release_notes(&mut self, cx: &mut Context<Self>) {
        self.history.read_notes(env!("SPACETERM_VERSION"));
        self.save_history();
        cx.notify();
    }

    pub(crate) fn dismiss_reminder(&mut self, cx: &mut Context<Self>) {
        self.reminder = None;
        self.history.reminded(self.adapter.now());
        self.save_history();
        self.reschedule();
        cx.notify();
    }

    fn tick(&mut self, time: u64, cx: &mut Context<Self>) {
        let preferences = self.preferences();
        self.stage = self.history.stage(time);
        let outstanding = self.model.version.is_some()
            && !matches!(
                self.model.state,
                UpdateState::UpToDate | UpdateState::Installing { .. }
            );
        if outstanding && cx.active_window().is_none() && self.history.reminder_due(time) {
            self.reminder = Some(self.stage);
            self.history.reminded(time);
        }
        self.save_history();
        if !self.prepared
            && preferences.automatic_downloads
            && matches!(self.model.state, UpdateState::Available { .. })
            && !self.model.cancelling
        {
            self.automatic_download = true;
            self.download(cx);
        }
        // An installation SpaceTerm can't replace fails every update the same way, so scheduled
        // checks stop until the person tries again.
        let unreplaceable = matches!(
            self.model.state,
            UpdateState::Failed {
                error: UpdateError::ReadOnly
            }
        );
        if !unreplaceable
            && time.saturating_sub(self.last_attempt) >= preferences.check_interval.seconds()
        {
            // A release waiting for the person must not hide a newer one: the check ends that
            // cycle first, so the next Relaunch installs the newest release.
            if self.model.replaceable() {
                if let UpdateState::Ready { version } = &self.model.state {
                    self.replacing = Some(version.clone());
                }
                self.last_attempt = time;
                self.recheck = true;
                self.cancel(cx);
            } else {
                self.check(false, cx);
            }
        }
        cx.notify();
    }

    /// The downloaded release a scheduled check replaces, while it does.
    pub(crate) fn replaces_download(&self) -> Option<&str> {
        self.replacing.as_deref()
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
        // A person who asks while a check already runs joins it: that check answers them, and
        // no second one starts.
        if manual && matches!(self.model.state, UpdateState::Checking) {
            if !self.model.manual_check {
                self.model.manual_check = true;
                cx.notify();
            }
            return;
        }
        // A found release that waits for its download ends its cycle first, as a due check
        // does, and the person's check follows once the updater finishes it.
        if manual
            && self.model.replaceable()
            && matches!(self.model.state, UpdateState::Available { .. })
        {
            self.last_attempt = self.adapter.now();
            self.recheck = true;
            self.recheck_manual = true;
            self.cancel(cx);
            return;
        }
        if !self.model.begin_check(manual) {
            return;
        }
        if manual {
            self.retrying_since = None;
            self.replacing = None;
        }
        self.last_attempt = self.adapter.now();
        self.reschedule();
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

    /// Downloads the pending release and never relaunches: Relaunch stays a separate choice. A
    /// release this run has not found yet, as after Stop Download, a failure or an offline
    /// launch, is checked for first.
    pub(crate) fn download_now(&mut self, cx: &mut Context<Self>) {
        self.relaunch_requested = false;
        self.watch_download();
        match self.model.state {
            UpdateState::Available { .. } => self.download(cx),
            UpdateState::Idle | UpdateState::UpToDate | UpdateState::Failed { .. } => {
                self.retry_download(cx);
            }
            // The check in progress downloads what it finds, as the person's own download.
            UpdateState::Checking => self.download_requested = true,
            _ => {}
        }
    }

    /// The person asked for the download, so its progress and outcome are theirs to see.
    fn watch_download(&mut self) {
        self.automatic_download = false;
        self.retrying_since = None;
    }

    /// Whether the person started the download in progress. An automatic one is quiet.
    pub(crate) fn download_asked(&self) -> bool {
        !self.automatic_download
    }

    /// Whether an automatic download failed and SpaceTerm has tried it again by itself for less
    /// than [`RETRY_QUIETLY_FOR`]. The failure then stays out of the title bar.
    pub(crate) fn retries_download(&self) -> bool {
        // A scheduled check's replacement download is SpaceTerm's own, so it retries quietly
        // whatever the automatic-download preference.
        (self.preferences().automatic_downloads || self.replacing.is_some())
            && self
                .retrying_since
                .is_some_and(|since| self.adapter.now().saturating_sub(since) < RETRY_QUIETLY_FOR)
    }

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        self.relaunch_requested = false;
        self.retrying_since = None;
        self.own_download_failed = false;
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
        if self.model.confirm(token, accepted) {
            self.approve_relaunch(cx);
            if let Err(error) = self.adapter.install() {
                self.receive(UpdateEvent::Failed(error), cx);
            }
        }
        cx.notify();
    }

    /// The person approved the relaunch: its outcome is theirs to hear, and it must quit
    /// SpaceTerm within the deadline. The quit saves Settings before the updater may relaunch.
    fn approve_relaunch(&mut self, cx: &mut Context<Self>) {
        self.automatic_download = false;
        self.relaunch_requested = false;
        self.relaunch_approvals += 1;
        self._relaunch_deadline = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RELAUNCH_DEADLINE).await;
            let _ = this.update(cx, |this, cx| {
                this.fail_relaunch(UpdateError::Relaunch, cx);
            });
        }));
        cx.defer(|cx| cx.dispatch_action(&crate::app::QuitApplication));
    }

    fn fail_relaunch(&mut self, error: UpdateError, cx: &mut Context<Self>) {
        if matches!(self.model.state, UpdateState::Installing { .. }) {
            self.model.fail_relaunch(error);
            cx.emit(UpdateNotice::Failed);
            cx.notify();
        }
    }

    pub(crate) fn request_install_confirmation(&mut self, cx: &mut Context<Self>) {
        if matches!(self.model.state, UpdateState::Ready { .. }) && !self.model.confirmation_open {
            cx.emit(UpdateNotice::ReadyToInstall);
        }
    }

    /// Installs the ready release and relaunches into it without asking: relaunching keeps every
    /// Terminal Session running. It answers an open "Is Ready" alert the same way. A release
    /// that isn't ready yet is checked for and downloaded first, and relaunches once it is ready,
    /// with no second click. After a failed relaunch it tries again: it quits again while the
    /// updater still holds its installer.
    pub(crate) fn relaunch(&mut self, cx: &mut Context<Self>) {
        self.watch_download();
        if self.model.retry_relaunch() {
            self.approve_relaunch(cx);
            cx.notify();
            return;
        }
        if let Some(token) = self.model.relaunch_ready() {
            self.finish_install_confirmation(token, true, cx);
            return;
        }
        match self.model.state {
            UpdateState::Available { .. } => self.download(cx),
            UpdateState::Idle | UpdateState::UpToDate | UpdateState::Failed { .. } => {
                self.retry_download(cx);
            }
            _ => {}
        }
        self.relaunch_requested = matches!(
            self.model.state,
            UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Verifying { .. }
        );
    }

    /// The relaunch the person approved, while it is still in progress or the updater already
    /// holds it. While it holds, a quit request is that relaunch rather than an exit.
    pub(crate) fn relaunch_approval(&self) -> Option<RelaunchApproval> {
        (self.model.relaunch_released
            || self.model.install_authorized
                && matches!(self.model.state, UpdateState::Installing { .. }))
        .then_some(RelaunchApproval(self.relaunch_approvals))
    }

    /// Whether a relaunch failed while the updater still holds its installer. Relaunch tries it
    /// again and a quit installs it without starting SpaceTerm again, even after the failure was
    /// dismissed.
    pub(crate) fn holds_failed_relaunch(&self) -> bool {
        self.model.installer_held
    }

    /// Hides the current failure's title-bar step on this desktop. The failure, its Try Again
    /// and an installer the updater already holds all stay, so every row keeps showing the
    /// failure until newer updater facts replace it. SpaceTerm's next try by itself is quiet
    /// again.
    pub(crate) fn dismiss_failure(&mut self, cx: &mut Context<Self>) {
        if matches!(self.model.state, UpdateState::Failed { .. }) {
            self.dismissed_failure = Some(self.model.failures);
            self.retrying_since = None;
            self.own_download_failed = false;
            cx.notify();
        }
    }

    /// Whether the person dismissed the current failure on this desktop.
    pub(crate) fn failure_dismissed(&self) -> bool {
        matches!(self.model.state, UpdateState::Failed { .. })
            && self.dismissed_failure == Some(self.model.failures)
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
        let time = self.adapter.now();
        let found = matches!(event, UpdateEvent::Available(_));
        let ready = matches!(event, UpdateEvent::Ready);
        let failed = matches!(event, UpdateEvent::Failed(_));
        let current = matches!(event, UpdateEvent::UpToDate);
        if let UpdateEvent::Available(version) = &event {
            // Relaunch installs the release the replacing check found, so the main area names it.
            if self.replacing.is_some() {
                self.replacing = Some(version.clone());
            }
            self.history
                .observe(env!("SPACETERM_VERSION"), version, self.publication, time);
            self.history.last_check = Some(time);
            self.stage = self.history.stage(time);
            self.save_history();
        }
        // The check succeeded: it found only a release this computer's system can't run.
        if matches!(event, UpdateEvent::Failed(UpdateError::SystemTooOld)) {
            self.history.last_check = Some(time);
            self.save_history();
        }
        if current {
            self.history.clear();
            self.history.last_check = Some(time);
            self.stage = Stage::Offer;
            self.reminder = None;
            self.save_history();
        }
        // A different release starts over: the failure the person saw was of another one.
        if let UpdateEvent::Available(version) = &event
            && self.model.version.as_ref() != Some(version)
        {
            self.own_download_failed = false;
        }
        let downloading = matches!(
            self.model.state,
            UpdateState::Downloading { .. } | UpdateState::Verifying { .. }
        );
        if matches!(event, UpdateEvent::Failed(_)) && downloading && !self.automatic_download {
            self.own_download_failed = true;
        } else if matches!(event, UpdateEvent::Failed(UpdateError::Download))
            && self.automatic_download
            && !self.own_download_failed
        {
            self.retrying_since.get_or_insert(time);
        }
        if ready || current {
            self.retrying_since = None;
            self.own_download_failed = false;
            self.replacing = None;
        }
        let finished = matches!(event, UpdateEvent::Finished);
        let notice = self.model.receive(event);
        if found && matches!(self.model.state, UpdateState::Available { .. }) {
            // A scheduled check that replaced a downloaded release downloads what it found, so
            // the Relaunch the main area kept stays true.
            let automatic = self.preferences().automatic_downloads;
            if !self.prepared
                && (self.download_requested
                    || self.relaunch_requested
                    || self.replacing.is_some()
                    || automatic)
            {
                self.automatic_download = !self.model.manual_check
                    && !self.download_requested
                    && !self.relaunch_requested;
                self.download_requested = false;
                self.download(cx);
            }
            self.tick(time, cx);
        }
        if failed || current || finished {
            self.relaunch_requested = false;
        }
        if ready
            && std::mem::take(&mut self.relaunch_requested)
            && !self.model.cancelling
            && let Some(token) = self.model.relaunch_ready()
        {
            self.finish_install_confirmation(token, true, cx);
        }
        if let Some(notice) = notice {
            // Download completion is quiet. Only the person's Relaunch installs it.
            if notice != UpdateNotice::ReadyToInstall
                && !(notice == UpdateNotice::Failed && self.automatic_download)
            {
                cx.emit(notice);
            }
        }
        let recheck = finished && std::mem::take(&mut self.recheck);
        let recheck_manual = recheck && std::mem::take(&mut self.recheck_manual);
        if finished && let Some(resume) = self.pending_quit.take() {
            cx.defer(move |cx| resume(cx));
        } else if recheck {
            self.check(recheck_manual, cx);
        }
        // A finished cycle lets a due check start, and a found release can bring reminders.
        self.reschedule();
        cx.notify();
    }
}

/// One approval of an update relaunch. Try Again after a failed relaunch is a new approval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RelaunchApproval(u64);

/// The approved update relaunch in progress, if any. A quit request is that relaunch then.
pub(crate) fn relaunch_approval(cx: &App) -> Option<RelaunchApproval> {
    cx.try_global::<UpdateService>()
        .and_then(|service| service.0.read(cx).relaunch_approval())
}

/// Settings are saved for the approved relaunch `approval`, so the updater may relaunch now.
/// Returns true when the updater asks SpaceTerm to quit by itself once it holds the relaunch.
pub(crate) fn release_relaunch(approval: RelaunchApproval, cx: &mut App) -> bool {
    cx.try_global::<UpdateService>()
        .cloned()
        .is_some_and(|service| {
            service.0.update(cx, |updates, _| {
                let released = updates.relaunch_approval() == Some(approval)
                    && updates.adapter.release_relaunch();
                updates.model.relaunch_released |= released;
                released
            })
        })
}

/// Settings couldn't be saved when the approved relaunch began. The relaunch fails now and says
/// why, and quitting follows ADR 0049 again.
pub(crate) fn fail_unsaved_relaunch(approval: RelaunchApproval, cx: &mut App) {
    if let Some(service) = cx.try_global::<UpdateService>().cloned() {
        service.0.update(cx, |updates, cx| {
            if updates.relaunch_approval() == Some(approval) {
                // A later Try Again starts its own deadline.
                updates._relaunch_deadline = None;
                updates.fail_relaunch(UpdateError::SettingsUnsaved, cx);
            }
        });
    }
}

/// Ordinary quit can finish a verified installer without forcing termination or relaunch.
pub(crate) fn prepare_before_quit(cx: &mut App, resume: ResumeQuit) -> bool {
    let Some(service) = cx.try_global::<UpdateService>().cloned() else {
        return false;
    };
    service.0.update(cx, |updates, cx| {
        if !updates.model.cycle_active || updates.model.install_authorized {
            return false;
        }
        // A relaunch that timed out left its installer with the updater, which can no longer
        // cancel it or end its cycle. The quit proceeds, and the updater installs on it.
        if updates.model.installer_held {
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
        /// Relaunches handed to the updater after Settings saved.
        pub(crate) releases: std::cell::Cell<usize>,
        /// The updater asks to quit by itself once it holds the relaunch, as Sparkle does.
        pub(crate) releases_itself: std::cell::Cell<bool>,
        pub(crate) checks: std::cell::Cell<usize>,
        pub(crate) saves: std::cell::Cell<usize>,
        /// The history as last written.
        pub(crate) saved: std::cell::RefCell<Option<UpdateHistory>>,
        pub(crate) history: UpdateHistory,
        /// Replaces the system clock, in seconds since the Unix epoch.
        pub(crate) clock: std::cell::RefCell<Option<Box<dyn Fn() -> u64>>>,
        pub(crate) clock_changed: std::cell::RefCell<Option<Box<dyn Fn() + Send + Sync>>>,
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
            self.checks.set(self.checks.get() + 1);
            Ok(())
        }
        fn save_history(&self, history: &UpdateHistory) {
            self.saves.set(self.saves.get() + 1);
            *self.saved.borrow_mut() = Some(history.clone());
        }
        fn now(&self) -> u64 {
            self.clock
                .borrow()
                .as_ref()
                .map_or_else(now, |clock| clock())
        }
        fn observe_clock_changes(&self, changed: Box<dyn Fn() + Send + Sync>) {
            *self.clock_changed.borrow_mut() = Some(changed);
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
        fn release_relaunch(&self) -> bool {
            self.releases.set(self.releases.get() + 1);
            self.releases_itself.get()
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

    #[test]
    fn unavailable_updates_reject_every_operation() {
        let (sender, _receiver) = async_channel::unbounded();
        let updates = UnavailableUpdates;
        assert_eq!(updates.start(sender), Err(UpdateError::Unavailable));
        assert_eq!(updates.check(), Err(UpdateError::Unavailable));
        assert_eq!(updates.download(), Err(UpdateError::Unavailable));
        assert_eq!(updates.install(), Err(UpdateError::Unavailable));
        assert_eq!(updates.finish_on_quit(), Err(UpdateError::Unavailable));
    }

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

    /// An approved relaunch that has not quit SpaceTerm within the deadline fails visibly and
    /// gives up its authority to quit. Try Again after the updater ended its cycle checks again
    /// and relaunches into the release once it is ready, with no second click.
    #[gpui::test]
    fn quitting_after_a_relaunch_timed_out_does_not_wait_for_the_updater(
        cx: &mut gpui::TestAppContext,
    ) {
        let adapter = Rc::new(RecordingAdapter::available());
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.relaunch(cx));
        cx.executor().advance_clock(RELAUNCH_DEADLINE);
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| relaunch_approval(cx)), None);

        // The updater already holds its installer, so it can't cancel and never ends its cycle.
        let waits = cx.update(|cx| prepare_before_quit(cx, Rc::new(|_| {})));
        assert!(!waits, "a confirmed quit proceeds at once");
        assert_eq!(adapter.cancellations.get(), 0);
    }

    /// Times out an approved relaunch while the updater keeps its installer, then acknowledges
    /// the failure the way the title bar's explanation does.
    fn acknowledge_a_timed_out_relaunch(
        cx: &mut gpui::TestAppContext,
    ) -> (Rc<RecordingAdapter>, Entity<ApplicationUpdates>) {
        let adapter = Rc::new(RecordingAdapter::available());
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.relaunch(cx));
        cx.executor().advance_clock(RELAUNCH_DEADLINE);
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.dismiss_failure(cx));
        (adapter, service)
    }

    #[gpui::test]
    fn try_again_after_acknowledging_a_failed_relaunch_quits_again(cx: &mut gpui::TestAppContext) {
        let (adapter, service) = acknowledge_a_timed_out_relaunch(cx);
        let quits = Rc::new(std::cell::Cell::new(0));
        let counted = quits.clone();
        cx.update(|cx| {
            cx.on_action(move |_: &crate::app::QuitApplication, _| counted.set(counted.get() + 1));
        });

        service.update(cx, |updates, cx| updates.relaunch(cx));
        cx.run_until_parked();
        assert_eq!(quits.get(), 1, "Try Again quits again");
        assert!(cx.update(|cx| relaunch_approval(cx)).is_some());
        assert_eq!(
            adapter.checks.get(),
            1,
            "the held installer needs no new check"
        );
    }

    #[gpui::test]
    fn quitting_after_acknowledging_a_failed_relaunch_does_not_wait_for_the_updater(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, _service) = acknowledge_a_timed_out_relaunch(cx);

        let waits = cx.update(|cx| prepare_before_quit(cx, Rc::new(|_| {})));
        assert!(!waits, "a confirmed quit proceeds at once");
        assert_eq!(adapter.cancellations.get(), 0);
    }

    #[gpui::test]
    fn a_relaunch_that_does_not_quit_in_time_fails_and_try_again_relaunches(
        cx: &mut gpui::TestAppContext,
    ) {
        let adapter = Rc::new(RecordingAdapter::available());
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        let notices = Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = notices.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&service, move |_, notice, _| {
                received.borrow_mut().push(*notice)
            })
        });
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.relaunch(cx));
        assert_eq!(adapter.installations.get(), 1);
        assert!(cx.update(|cx| relaunch_approval(cx)).is_some());

        cx.executor()
            .advance_clock(RELAUNCH_DEADLINE - Duration::from_secs(1));
        cx.run_until_parked();
        assert!(cx.update(|cx| relaunch_approval(cx)).is_some());
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        let relaunch_failed = UpdateState::Failed {
            error: UpdateError::Relaunch,
        };
        assert_eq!(
            service.read_with(cx, |updates, _| updates.state().clone()),
            relaunch_failed
        );
        assert_eq!(cx.update(|cx| relaunch_approval(cx)), None);
        assert_eq!(*notices.borrow(), [UpdateNotice::Failed]);

        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(
            service.read_with(cx, |updates, _| updates.state().clone()),
            relaunch_failed
        );
        assert_eq!(notices.borrow().len(), 1, "one failure is reported once");

        service.update(cx, |updates, cx| updates.relaunch(cx));
        assert_eq!(adapter.checks.get(), 2);
        adapter.emit(UpdateEvent::ReleaseMetadata {
            published_at: now(),
            prepared: true,
        });
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        assert_eq!(adapter.installations.get(), 2);
        assert!(cx.update(|cx| relaunch_approval(cx)).is_some());
    }

    /// Relaunch chosen before the release is downloaded downloads it, then relaunches once it is
    /// ready, with no second click, even with automatic downloads off.
    #[gpui::test]
    fn relaunch_before_the_download_relaunches_once_the_release_is_ready(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, service) = launch_fixture(cx);
        let document = crate::settings::SettingsDocument {
            updates: UpdatePreferences {
                automatic_downloads: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = crate::settings::Settings::load(
            crate::settings::storage::testing::MemoryStorage::with_document(&document),
        );
        service.update(cx, |updates, _| updates.attach_settings(settings));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(adapter.downloads.get(), 0);

        service.update(cx, |updates, cx| updates.relaunch(cx));
        assert_eq!(adapter.downloads.get(), 1);
        adapter.emit(UpdateEvent::Downloading {
            received: 1,
            total: 2,
        });
        adapter.emit(UpdateEvent::Verifying);
        cx.run_until_parked();
        assert_eq!(adapter.installations.get(), 0);
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        assert_eq!(adapter.installations.get(), 1);
        assert!(cx.update(|cx| relaunch_approval(cx)).is_some());
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

    /// Design 4.5: a scheduled check of a downloaded release ends its cycle and downloads it
    /// again. The main area keeps `Relaunch to Update` through the whole check, and the row
    /// agrees once the release is downloaded again.
    #[gpui::test]
    fn a_scheduled_check_keeps_relaunch_to_update_in_the_main_area(cx: &mut gpui::TestAppContext) {
        scheduled_check_keeps_relaunch("0.1.1", true, cx);
    }

    /// The same when the scheduled check finds a newer release and downloads that one by itself.
    #[gpui::test]
    fn a_scheduled_check_that_finds_a_newer_release_keeps_relaunch_to_update(
        cx: &mut gpui::TestAppContext,
    ) {
        scheduled_check_keeps_relaunch("0.1.2", true, cx);
    }

    /// With automatic downloads off, the check downloads the release the person downloaded again,
    /// so `Relaunch to Update` stays true.
    #[gpui::test]
    fn a_scheduled_check_downloads_again_a_release_the_person_downloaded(
        cx: &mut gpui::TestAppContext,
    ) {
        scheduled_check_keeps_relaunch("0.1.1", false, cx);
    }

    /// With automatic downloads off, the replacement download fails. SpaceTerm retries it
    /// quietly, so the main area keeps `Relaunch to Update`.
    #[gpui::test]
    fn a_failed_replacement_download_keeps_relaunch_to_update(cx: &mut gpui::TestAppContext) {
        scheduled_check_keeps_relaunch_or_fails("0.1.1", false, true, cx);
    }

    fn scheduled_check_keeps_relaunch(found: &str, automatic: bool, cx: &mut gpui::TestAppContext) {
        scheduled_check_keeps_relaunch_or_fails(found, automatic, false, cx);
    }

    fn scheduled_check_keeps_relaunch_or_fails(
        found: &str,
        automatic: bool,
        fails: bool,
        cx: &mut gpui::TestAppContext,
    ) {
        let adapter = Rc::new(RecordingAdapter::available());
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        let mut document = crate::settings::SettingsDocument::default();
        document.updates.automatic_downloads = automatic;
        let settings = crate::settings::Settings::load(
            crate::settings::storage::testing::MemoryStorage::with_document(&document),
        );
        service.update(cx, |updates, _| updates.attach_settings(settings));
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.download(cx));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        let main = |cx: &mut gpui::TestAppContext| {
            cx.update(|cx| state::current(cx).pill().map(|pill| pill.label.to_string()))
        };
        let relaunch = Some(copy::title_bar::RELAUNCH_TO_UPDATE.to_owned());
        assert_eq!(main(cx), relaunch);

        let later = adapter.now() + 30 * policy::DAY;
        service.update(cx, |updates, cx| updates.tick(later, cx));
        assert_eq!(adapter.cancellations.get(), 1, "the check ends the cycle");
        assert_eq!(main(cx), relaunch, "while the cycle ends");
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(main(cx), relaunch, "while it checks");
        adapter.emit(UpdateEvent::Available(found.into()));
        cx.run_until_parked();
        assert_eq!(main(cx), relaunch, "after it finds {found}");
        assert_eq!(
            service.read_with(cx, |updates, _| updates
                .replaces_download()
                .map(str::to_owned)),
            Some(found.to_owned()),
            "the main area names the release Relaunch installs"
        );
        assert_eq!(adapter.downloads.get(), 2, "it downloads {found} again");
        if fails {
            adapter.emit(UpdateEvent::Failed(UpdateError::Download));
            cx.run_until_parked();
            assert_eq!(main(cx), relaunch, "after the replacement download fails");
            return;
        }
        adapter.emit(UpdateEvent::Downloading {
            received: 1,
            total: 2,
        });
        cx.run_until_parked();
        assert_eq!(main(cx), relaunch, "while it downloads again");
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        assert_eq!(main(cx), relaunch);
        let row = cx.update(|cx| state::current(cx).app_row());
        assert_eq!(
            row.status.as_deref(),
            Some(format!("{found} downloaded").as_str()),
            "the row agrees"
        );
    }

    /// The scheduler sleeps from one due check or reminder to the next. Every step lands exactly
    /// on the due time, nothing runs or writes in between, and settings, clock and window changes
    /// move the next due time.
    #[gpui::test]
    fn the_scheduler_wakes_only_when_a_check_or_reminder_is_due(cx: &mut gpui::TestAppContext) {
        use policy::{CheckInterval, DAY};
        const START: u64 = 1_000_000;
        let adapter = Rc::new(RecordingAdapter::available());
        let executor = cx.executor();
        let began = executor.now();
        let offset = Rc::new(std::cell::Cell::new(0u64));
        {
            let offset = Rc::clone(&offset);
            *adapter.clock.borrow_mut() = Some(Box::new(move || {
                START + offset.get() + (executor.now() - began).as_secs()
            }));
        }
        let wall = |adapter: &RecordingAdapter| adapter.now();
        // SpaceTerm runs in the background, where reminders appear.
        let window = cx.add_window(|_, _| gpui::EmptyView);
        gpui::VisualTestContext::from_window(window.into(), cx).deactivate_window();
        let document = crate::settings::SettingsDocument {
            updates: UpdatePreferences {
                automatic_downloads: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = crate::settings::Settings::load(
            crate::settings::storage::testing::MemoryStorage::with_document(&document),
        );
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        service.update(cx, |updates, _| updates.attach_settings(settings.clone()));
        cx.run_until_parked();
        // Steps to the next timer and reports how far it was. Nothing may run in between.
        let step = |cx: &mut gpui::TestAppContext| {
            cx.run_until_parked();
            let before = cx.executor().now();
            assert!(cx.dispatcher.scheduler().advance_clock_to_next_timer());
            let waited = (cx.executor().now() - before).as_secs();
            cx.run_until_parked();
            waited
        };
        // The release came out a day before launch, so it asks a day after launch.
        let found = |adapter: &RecordingAdapter, cx: &mut gpui::TestAppContext| {
            adapter.emit(UpdateEvent::ReleaseMetadata {
                published_at: START - DAY,
                prepared: false,
            });
            adapter.emit(UpdateEvent::Available("0.1.1".into()));
            adapter.emit(UpdateEvent::Finished);
            cx.run_until_parked();
        };

        assert_eq!(step(cx), 1, "the first check waits one second after launch");
        assert_eq!(adapter.checks.get(), 1);
        found(&adapter, cx);
        let first_check = START + 1;

        // The update reaches Ask before the daily check is due again.
        let saves = adapter.saves.get();
        assert_eq!(wall(&adapter) + step(cx), START + DAY);
        assert_eq!(adapter.checks.get(), 1);
        assert_eq!(
            service.read_with(cx, |updates, _| updates.reminder()),
            Some(Stage::Ask),
            "the reminder appears when the update has waited two days"
        );
        assert_eq!(adapter.saves.get(), saves + 1, "one write at the due time");
        service.read_with(cx, |updates, _| updates.left_spaceterm());
        cx.run_until_parked();
        assert_eq!(
            adapter.saves.get(),
            saves + 1,
            "a wake that changes nothing writes nothing"
        );
        service.update(cx, |updates, cx| updates.dismiss_reminder(cx));

        assert_eq!(wall(&adapter) + step(cx), first_check + DAY);
        assert_eq!(
            adapter.checks.get(),
            2,
            "the daily check runs when it is due"
        );
        found(&adapter, cx);

        // Hourly checks start from the last check as soon as the setting changes.
        let mut hourly = (*settings.snapshot().committed).clone();
        hourly.updates.check_interval = CheckInterval::Hourly;
        settings
            .update_committed(settings.snapshot().committed.revision, hourly)
            .unwrap()
            .run()
            .unwrap();
        assert_eq!(wall(&adapter) + step(cx), first_check + DAY + 3600);
        assert_eq!(adapter.checks.get(), 3);
        found(&adapter, cx);

        // A clock set forward past the due check reaches it once the change is reported.
        offset.set(2 * 3600);
        (adapter.clock_changed.borrow().as_ref().unwrap())();
        cx.run_until_parked();
        assert_eq!(
            adapter.checks.get(),
            4,
            "a clock change brings an overdue check now"
        );
        found(&adapter, cx);

        // While a window is active the pill shows the step, so the next daily reminder waits,
        // then appears once SpaceTerm has no active window.
        window
            .update(cx, |_, window, _| window.activate_window())
            .unwrap();
        let mut checks = adapter.checks.get();
        while wall(&adapter) < START + 2 * DAY {
            step(cx);
            if adapter.checks.get() > checks {
                checks = adapter.checks.get();
                found(&adapter, cx);
            }
        }
        assert_eq!(
            service.read_with(cx, |updates, _| updates.reminder()),
            None,
            "a due reminder waits while a window is active"
        );
        gpui::VisualTestContext::from_window(window.into(), cx).deactivate_window();
        service.read_with(cx, |updates, _| updates.left_spaceterm());
        cx.run_until_parked();
        assert_eq!(
            service.read_with(cx, |updates, _| updates.reminder()),
            Some(Stage::Ask)
        );
    }

    /// Sleep moves wall time past a due check and reminder while the elapsed-time timer stands
    /// still. The system's wake brings both at once, a second wake changes nothing, and the next
    /// timer runs to the next due time measured from the wake.
    #[gpui::test]
    fn a_wake_from_sleep_runs_due_work_once_and_times_the_next(cx: &mut gpui::TestAppContext) {
        use policy::DAY;
        const START: u64 = 1_000_000;
        let adapter = Rc::new(RecordingAdapter::available());
        let executor = cx.executor();
        let began = executor.now();
        let slept = Rc::new(std::cell::Cell::new(0u64));
        {
            let slept = Rc::clone(&slept);
            *adapter.clock.borrow_mut() = Some(Box::new(move || {
                START + slept.get() + (executor.now() - began).as_secs()
            }));
        }
        // SpaceTerm runs in the background, where reminders appear.
        let window = cx.add_window(|_, _| gpui::EmptyView);
        gpui::VisualTestContext::from_window(window.into(), cx).deactivate_window();
        let document = crate::settings::SettingsDocument {
            updates: UpdatePreferences {
                automatic_downloads: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = crate::settings::Settings::load(
            crate::settings::storage::testing::MemoryStorage::with_document(&document),
        );
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        service.update(cx, |updates, _| updates.attach_settings(settings.clone()));
        cx.run_until_parked();
        // The release came out a day before launch, so it asks a day after launch.
        let found = |adapter: &RecordingAdapter, cx: &mut gpui::TestAppContext| {
            adapter.emit(UpdateEvent::ReleaseMetadata {
                published_at: START - DAY,
                prepared: false,
            });
            adapter.emit(UpdateEvent::Available("0.1.1".into()));
            adapter.emit(UpdateEvent::Finished);
            cx.run_until_parked();
        };
        assert!(cx.dispatcher.scheduler().advance_clock_to_next_timer());
        cx.run_until_parked();
        assert_eq!(adapter.checks.get(), 1);
        found(&adapter, cx);

        // The machine sleeps past the daily check and the update reaching Ask.
        slept.set(DAY + 600);
        let woke = adapter.now();
        cx.run_until_parked();
        assert_eq!(adapter.checks.get(), 1, "the timer still waits after sleep");
        assert_eq!(service.read_with(cx, |updates, _| updates.reminder()), None);

        cx.update(crate::system_wake::deliver);
        cx.run_until_parked();
        assert_eq!(adapter.checks.get(), 2, "the overdue check runs on wake");
        assert_eq!(
            service.read_with(cx, |updates, _| updates.reminder()),
            Some(Stage::Ask),
            "the due reminder appears on wake"
        );
        let mut saved = adapter.saved.borrow().clone().unwrap();
        assert_eq!(
            saved.last_check,
            Some(START + 1),
            "the check has not finished"
        );
        assert!(
            !saved.reminder_due(woke),
            "the history retains the reminder shown on wake"
        );
        found(&adapter, cx);
        assert_eq!(
            adapter.saved.borrow().as_ref().unwrap().last_check,
            Some(woke),
            "the history retains the check finished on wake"
        );

        let saves = adapter.saves.get();
        cx.update(crate::system_wake::deliver);
        cx.run_until_parked();
        assert_eq!(adapter.checks.get(), 2, "a second wake runs nothing");
        assert_eq!(adapter.saves.get(), saves, "a second wake writes nothing");
        service.update(cx, |updates, cx| updates.dismiss_reminder(cx));
        assert_eq!(service.read_with(cx, |updates, _| updates.reminder()), None);

        // The next due time is a day after the wake: the next check and the next daily
        // reminder.
        // A timer set before the sleep may still fire first; it brings nothing.
        while adapter.checks.get() == 2 {
            assert_eq!(service.read_with(cx, |updates, _| updates.reminder()), None);
            assert!(cx.dispatcher.scheduler().advance_clock_to_next_timer());
            cx.run_until_parked();
        }
        assert_eq!(adapter.now(), woke + DAY);
        assert_eq!(adapter.checks.get(), 3);
        assert_eq!(
            service.read_with(cx, |updates, _| updates.reminder()),
            Some(Stage::Ask)
        );
    }

    fn launch_fixture(
        cx: &mut gpui::TestAppContext,
    ) -> (Rc<RecordingAdapter>, Entity<ApplicationUpdates>) {
        let adapter = Rc::new(RecordingAdapter::available());
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        service.update(cx, |updates, cx| updates.check(false, cx));
        cx.run_until_parked();
        (adapter, service)
    }

    /// What's New is owed on the first run after an update until the notes open, and never on
    /// a later launch.
    #[gpui::test]
    fn whats_new_lasts_from_the_first_run_on_a_newer_version_until_the_notes_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut adapter = RecordingAdapter::available();
        adapter.history.launched("0.0.1", 0);
        let adapter = Rc::new(adapter);
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        assert!(service.read_with(cx, |updates, _| updates.whats_new()));
        service.update(cx, |updates, cx| updates.read_release_notes(cx));
        assert!(!service.read_with(cx, |updates, _| updates.whats_new()));

        let mut unread = RecordingAdapter::available();
        unread.history.launched("0.0.1", 0);
        unread.history.launched(env!("SPACETERM_VERSION"), 1);
        let unread = Rc::new(unread);
        cx.update(|cx| ApplicationUpdates::install(unread.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        assert!(service.read_with(cx, |updates, _| updates.release_notes_unread()));
        assert!(
            !service.read_with(cx, |updates, _| updates.whats_new()),
            "a later launch of the same version owes nothing"
        );
        assert_eq!(
            service.read_with(cx, |updates, _| updates.ladder_stage()),
            Stage::Offer,
            "nothing waits"
        );
    }

    /// A [`Presence`] the test sets, on a platform that observes input.
    #[derive(Default)]
    struct ScriptedPresence {
        presenting: std::cell::Cell<bool>,
        input: std::cell::Cell<u64>,
    }

    impl ScriptedPresence {
        /// One more keyboard, input-method or pointer event.
        fn event(&self) {
            self.input.set(self.input.get() + 1);
        }
    }

    impl Presence for ScriptedPresence {
        fn presenting(&self, _: &mut App) -> bool {
            self.presenting.get()
        }
        fn input(&self, _: &mut App) -> Option<u64> {
            Some(self.input.get())
        }
    }

    /// SpaceTerm launched at `LAUNCH` and downloaded a release published at `published_at`.
    /// The update clock follows the test executor's, which only timers move.
    struct Unused {
        adapter: Rc<RecordingAdapter>,
        service: Entity<ApplicationUpdates>,
        presence: Rc<ScriptedPresence>,
    }

    const LAUNCH: u64 = 1_000_000;

    impl Unused {
        fn new(published_at: u64, cx: &mut gpui::TestAppContext) -> Self {
            let presence = Rc::new(ScriptedPresence::default());
            let (adapter, service, _window) = Self::ready(published_at, presence.clone(), cx);
            Self {
                adapter,
                service,
                presence,
            }
        }

        /// Launches with `presence` and a downloaded release published at `published_at`.
        fn ready(
            published_at: u64,
            presence: Rc<dyn Presence>,
            cx: &mut gpui::TestAppContext,
        ) -> (
            Rc<RecordingAdapter>,
            Entity<ApplicationUpdates>,
            gpui::AnyWindowHandle,
        ) {
            let adapter = Rc::new(RecordingAdapter::available());
            let executor = cx.executor();
            let began = executor.now();
            *adapter.clock.borrow_mut() = Some(Box::new(move || {
                LAUNCH + (executor.now() - began).as_secs()
            }));
            let window = cx.add_window(|_, _| gpui::EmptyView);
            cx.update(|cx| ApplicationUpdates::install_with(adapter.clone(), presence.clone(), cx));
            let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
            cx.run_until_parked();
            assert!(cx.dispatcher.scheduler().advance_clock_to_next_timer());
            cx.run_until_parked();
            assert_eq!(adapter.checks.get(), 1, "the first check after launch");
            adapter.emit(UpdateEvent::ReleaseMetadata {
                published_at,
                prepared: false,
            });
            adapter.emit(UpdateEvent::Available("0.1.1".into()));
            cx.run_until_parked();
            assert_eq!(adapter.downloads.get(), 1, "an automatic download");
            adapter.emit(UpdateEvent::Ready);
            cx.run_until_parked();
            (adapter, service, window.into())
        }

        /// Runs to the next timer and returns the update clock there.
        fn step(&self, cx: &mut gpui::TestAppContext) -> u64 {
            cx.run_until_parked();
            assert!(cx.dispatcher.scheduler().advance_clock_to_next_timer());
            cx.run_until_parked();
            self.adapter.now()
        }

        fn relaunched(&self, cx: &mut gpui::TestAppContext) -> bool {
            let relaunched = self.adapter.installations.get() > 0;
            assert_eq!(
                relaunched,
                self.service
                    .read_with(cx, |updates, _| updates.relaunch_approval())
                    .is_some(),
                "only the approved-relaunch path installs"
            );
            relaunched
        }
    }

    /// From the Ask stage, SpaceTerm relaunches into a downloaded update by itself after an
    /// hour with no input, on the approved-relaunch path that keeps every Terminal Session.
    #[gpui::test]
    fn a_downloaded_update_relaunches_by_itself_after_an_hour_unused_from_ask(
        cx: &mut gpui::TestAppContext,
    ) {
        let unused = Unused::new(LAUNCH - ladder::ASK_AFTER, cx);
        assert!(!unused.relaunched(cx));
        assert_eq!(
            unused.step(cx),
            LAUNCH + ladder::IDLE_PERIOD,
            "the launch counts as input"
        );
        assert!(unused.relaunched(cx));
        assert_eq!(
            unused
                .service
                .read_with(cx, |updates, _| updates.state().clone()),
            UpdateState::Installing {
                version: "0.1.1".into()
            }
        );
    }

    /// Before Ask, however long nobody uses SpaceTerm, the update waits for the person; the
    /// relaunch comes once Ask arrives.
    #[gpui::test]
    fn an_update_at_offer_never_relaunches_by_itself(cx: &mut gpui::TestAppContext) {
        let ask = LAUNCH + 3 * ladder::IDLE_PERIOD;
        let unused = Unused::new(ask - ladder::ASK_AFTER, cx);
        while unused.adapter.now() < ask {
            unused.step(cx);
            if unused.adapter.now() < ask {
                assert!(!unused.relaunched(cx), "Offer at {}", unused.adapter.now());
            }
        }
        assert_eq!(unused.adapter.now(), ask);
        assert!(unused.relaunched(cx));
    }

    /// Any keyboard, input-method or pointer event starts the hour again, counted at the next
    /// wake. A scroll, a click without movement or a move away and back are each one event; the
    /// macOS adapter's native tests show that each one is counted.
    #[gpui::test]
    fn input_to_spaceterm_postpones_the_relaunch_by_an_hour(cx: &mut gpui::TestAppContext) {
        let unused = Unused::new(LAUNCH - ladder::ASK_AFTER, cx);
        unused.presence.event();
        assert_eq!(unused.step(cx), LAUNCH + ladder::IDLE_PERIOD);
        assert!(!unused.relaunched(cx), "input came during the hour");
        let first = unused.adapter.now();

        cx.executor().advance_clock(Duration::from_secs(600));
        unused.presence.event();
        assert_eq!(unused.step(cx), first + ladder::IDLE_PERIOD);
        assert!(!unused.relaunched(cx), "input came during the second hour");
        let second = unused.adapter.now();
        assert_eq!(unused.step(cx), second + ladder::IDLE_PERIOD);
        assert!(unused.relaunched(cx));
    }

    /// Where input can't be observed, as with the portable adapter Linux uses, SpaceTerm never
    /// relaunches by itself and wakes no extra time for it.
    #[gpui::test]
    fn a_platform_that_cannot_observe_input_never_relaunches_by_itself(
        cx: &mut gpui::TestAppContext,
    ) {
        assert_eq!(cx.update(|cx| PortablePresence.input(cx)), None);
        let (adapter, service, _window) =
            Unused::ready(LAUNCH - ladder::ASK_AFTER, Rc::new(PortablePresence), cx);
        cx.run_until_parked();
        assert!(cx.dispatcher.scheduler().advance_clock_to_next_timer());
        cx.run_until_parked();
        assert!(
            adapter.now() >= LAUNCH + ladder::ASK_CADENCE,
            "the next wake is the next reminder or check, not the idle period"
        );
        assert_eq!(adapter.installations.get(), 0);
        assert!(
            service
                .read_with(cx, |updates, _| updates.relaunch_approval())
                .is_none()
        );
    }

    /// An open alert, sheet or menu, or Settings not saved, holds the relaunch until it ends.
    #[gpui::test]
    fn an_open_alert_or_unsaved_settings_holds_the_relaunch(cx: &mut gpui::TestAppContext) {
        let unused = Unused::new(LAUNCH - ladder::ASK_AFTER, cx);
        let settings = crate::settings::Settings::load(
            crate::settings::storage::testing::MemoryStorage::with_document(
                &crate::settings::SettingsDocument::default(),
            ),
        );
        unused
            .service
            .update(cx, |updates, _| updates.attach_settings(settings.clone()));
        unused.presence.presenting.set(true);
        let idle = unused.step(cx);
        assert!(!unused.relaunched(cx), "an alert is open");
        assert_eq!(unused.step(cx), idle + IDLE_RECHECK);
        assert!(!unused.relaunched(cx));

        let snapshot = settings.snapshot();
        let preview = settings.begin_preview(snapshot.committed.revision).unwrap();
        let mut edited = (*snapshot.committed).clone();
        edited.updates.check_interval = policy::CheckInterval::Hourly;
        settings.update_preview(&preview, edited).unwrap();
        unused.presence.presenting.set(false);
        unused.step(cx);
        assert!(!unused.relaunched(cx), "Settings are not saved");

        settings.cancel_preview(&preview).unwrap();
        cx.run_until_parked();
        assert!(unused.relaunched(cx), "the relaunch follows at once");
    }

    #[gpui::test]
    fn saved_download_opt_out_keeps_optional_updates_available_for_manual_download(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, service) = launch_fixture(cx);
        let document = crate::settings::SettingsDocument {
            updates: UpdatePreferences {
                automatic_downloads: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = crate::settings::Settings::load(
            crate::settings::storage::testing::MemoryStorage::with_document(&document),
        );
        service.update(cx, |updates, _| updates.attach_settings(settings));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
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

    /// An installation SpaceTerm can't replace fails every download the same way, so scheduled
    /// checks stop and the failure stays. The person's Try Again still checks.
    #[gpui::test]
    fn an_unreplaceable_installation_stops_scheduled_checks(cx: &mut gpui::TestAppContext) {
        let adapter = Rc::new(RecordingAdapter::available());
        let executor = cx.executor();
        let began = executor.now();
        *adapter.clock.borrow_mut() = Some(Box::new(move || {
            1_000_000 + (executor.now() - began).as_secs()
        }));
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(adapter.checks.get(), 1);
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(adapter.downloads.get(), 1);
        adapter.emit(UpdateEvent::Failed(UpdateError::ReadOnly));
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();

        cx.executor()
            .advance_clock(Duration::from_secs(3 * policy::DAY));
        cx.run_until_parked();
        assert_eq!(adapter.checks.get(), 1);
        assert_eq!(
            service.read_with(cx, |updates, _| updates.state().clone()),
            UpdateState::Failed {
                error: UpdateError::ReadOnly
            }
        );

        service.update(cx, |updates, cx| updates.retry_download(cx));
        assert_eq!(adapter.checks.get(), 2);
    }

    #[gpui::test]
    fn optional_background_download_is_quiet(cx: &mut gpui::TestAppContext) {
        let (adapter, service) = launch_fixture(cx);
        let notices = Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = notices.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&service, move |_, event, _| {
                received.borrow_mut().push(*event)
            })
        });
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(adapter.downloads.get(), 1);
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        assert!(notices.borrow().is_empty());
        assert_eq!(adapter.installations.get(), 0);
    }

    /// Only a download the person started is theirs to watch in the title bar.
    #[gpui::test]
    fn only_a_download_the_person_started_is_asked(cx: &mut gpui::TestAppContext) {
        let (adapter, service) = launch_fixture(cx);
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(adapter.downloads.get(), 1);
        assert!(!service.read_with(cx, |updates, _| updates.download_asked()));
        // Stop Download, then Download.
        service.update(cx, |updates, cx| updates.cancel(cx));
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.download_now(cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(adapter.downloads.get(), 2);
        assert!(service.read_with(cx, |updates, _| updates.download_asked()));
    }

    /// Download chosen while a scheduled check runs is the person's own download.
    #[gpui::test]
    fn download_chosen_during_a_scheduled_check_is_asked(cx: &mut gpui::TestAppContext) {
        let (adapter, service) = launch_fixture(cx);
        assert_eq!(
            service.read_with(cx, |updates, _| updates.state().clone()),
            UpdateState::Checking
        );
        service.update(cx, |updates, cx| updates.download_now(cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert_eq!(adapter.downloads.get(), 1);
        assert!(service.read_with(cx, |updates, _| updates.download_asked()));
    }

    /// An automatic download that fails is tried again by itself, quietly for a day from its
    /// first failure. The person's own try is never quiet.
    #[gpui::test]
    fn a_failed_automatic_download_is_retried_quietly_for_a_day(cx: &mut gpui::TestAppContext) {
        let adapter = Rc::new(RecordingAdapter::available());
        let time = Rc::new(std::cell::Cell::new(1_000_000_u64));
        let clock = time.clone();
        *adapter.clock.borrow_mut() = Some(Box::new(move || clock.get()));
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        let fail_a_download = |cx: &mut gpui::TestAppContext| {
            adapter.emit(UpdateEvent::Available("0.1.1".into()));
            adapter.emit(UpdateEvent::Failed(UpdateError::Download));
            adapter.emit(UpdateEvent::Finished);
            cx.run_until_parked();
        };
        let retries = |cx: &mut gpui::TestAppContext| {
            service.read_with(cx, |updates, _| updates.retries_download())
        };
        service.update(cx, |updates, cx| updates.check(false, cx));
        assert!(!retries(cx));
        fail_a_download(cx);
        assert!(retries(cx));

        // The next scheduled check fails the same way. The day counts from the first failure.
        time.set(time.get() + policy::DAY - 1);
        service.update(cx, |updates, cx| updates.check(false, cx));
        fail_a_download(cx);
        assert!(retries(cx));
        time.set(time.get() + 1);
        assert!(!retries(cx));

        time.set(1_000_000);
        assert!(retries(cx));
        service.update(cx, |updates, cx| updates.download_now(cx));
        fail_a_download(cx);
        assert!(!retries(cx), "the person's own download failed");
    }

    /// The updater on a clock the test sets, and the steps the quiet-retry tests share: an
    /// automatic download that fails, and whether SpaceTerm retries quietly.
    fn clocked_fixture(
        cx: &mut gpui::TestAppContext,
    ) -> (Rc<RecordingAdapter>, Entity<ApplicationUpdates>) {
        let adapter = Rc::new(RecordingAdapter::available());
        *adapter.clock.borrow_mut() = Some(Box::new(|| 1_000_000));
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        (adapter, service)
    }

    fn fail_an_automatic_download(
        adapter: &RecordingAdapter,
        service: &Entity<ApplicationUpdates>,
        cx: &mut gpui::TestAppContext,
    ) {
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Failed(UpdateError::Download));
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
    }

    fn retries(service: &Entity<ApplicationUpdates>, cx: &mut gpui::TestAppContext) -> bool {
        service.read_with(cx, |updates, _| updates.retries_download())
    }

    /// A download the person started that fails stays theirs to see. SpaceTerm's later tries
    /// by itself do not make it quiet, until the person dismisses it.
    #[gpui::test]
    fn a_persons_failed_download_stays_visible_through_automatic_retries(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, service) = clocked_fixture(cx);
        service.update(cx, |updates, cx| updates.check(false, cx));
        service.update(cx, |updates, cx| updates.download_now(cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Failed(UpdateError::Download));
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert!(!retries(&service, cx));

        fail_an_automatic_download(&adapter, &service, cx);
        assert!(!retries(&service, cx), "the person's failure still stands");

        service.update(cx, |updates, cx| updates.dismiss_failure(cx));
        fail_an_automatic_download(&adapter, &service, cx);
        assert!(retries(&service, cx));
    }

    /// A person's failed download stops standing when that release's story ends: a download
    /// finishes, SpaceTerm is current, the person stops their own try again, or a different
    /// release is found. SpaceTerm's next failure by itself is then quiet again.
    #[gpui::test]
    fn a_persons_failed_download_ends_with_its_release(cx: &mut gpui::TestAppContext) {
        let (adapter, service) = clocked_fixture(cx);
        let fail_a_persons_download = |error, cx: &mut gpui::TestAppContext| {
            service.update(cx, |updates, cx| updates.check(false, cx));
            service.update(cx, |updates, cx| updates.download_now(cx));
            adapter.emit(UpdateEvent::Available("0.1.1".into()));
            adapter.emit(UpdateEvent::Failed(error));
            adapter.emit(UpdateEvent::Finished);
            cx.run_until_parked();
        };
        let finish = |cx: &mut gpui::TestAppContext| {
            adapter.emit(UpdateEvent::Finished);
            cx.run_until_parked();
        };
        let quiet_again = |cx: &mut gpui::TestAppContext, why: &str| {
            fail_an_automatic_download(&adapter, &service, cx);
            assert!(retries(&service, cx), "{why}");
            service.update(cx, |updates, cx| updates.dismiss_failure(cx));
        };

        // Any failure of the person's download stands, not only a download that broke off.
        fail_a_persons_download(UpdateError::Verification, cx);
        fail_an_automatic_download(&adapter, &service, cx);
        assert!(!retries(&service, cx));
        service.update(cx, |updates, cx| updates.dismiss_failure(cx));

        fail_a_persons_download(UpdateError::Download, cx);
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        finish(cx);
        quiet_again(cx, "a download finished");

        fail_a_persons_download(UpdateError::Download, cx);
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::UpToDate);
        finish(cx);
        quiet_again(cx, "SpaceTerm is current");

        fail_a_persons_download(UpdateError::Download, cx);
        service.update(cx, |updates, cx| updates.download_now(cx));
        service.update(cx, |updates, cx| updates.cancel(cx));
        finish(cx);
        quiet_again(cx, "the person stopped their own try");

        fail_a_persons_download(UpdateError::Download, cx);
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.2".into()));
        adapter.emit(UpdateEvent::Failed(UpdateError::Download));
        finish(cx);
        assert!(retries(&service, cx), "a different release");
    }

    /// The quiet retry ends when the person acts on the release, when the download works, when
    /// SpaceTerm is current, and when automatic downloads are turned off.
    #[gpui::test]
    fn the_quiet_retry_ends_when_the_person_acts_or_the_download_works(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, service) = clocked_fixture(cx);
        let quiet = |cx: &mut gpui::TestAppContext| {
            fail_an_automatic_download(&adapter, &service, cx);
            assert!(retries(&service, cx));
        };
        let finish = |cx: &mut gpui::TestAppContext| {
            adapter.emit(UpdateEvent::Finished);
            cx.run_until_parked();
        };

        quiet(cx);
        service.update(cx, |updates, cx| updates.dismiss_failure(cx));
        assert!(!retries(&service, cx), "Dismiss");

        quiet(cx);
        service.update(cx, |updates, cx| updates.check(true, cx));
        assert!(!retries(&service, cx), "Check Now");
        finish(cx);

        quiet(cx);
        service.update(cx, |updates, cx| updates.check(false, cx));
        assert!(retries(&service, cx), "a scheduled check keeps it");
        service.update(cx, |updates, cx| updates.cancel(cx));
        assert!(!retries(&service, cx), "Stop Download");
        finish(cx);

        quiet(cx);
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::UpToDate);
        finish(cx);
        assert!(!retries(&service, cx), "SpaceTerm is current");

        quiet(cx);
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        finish(cx);
        assert!(!retries(&service, cx), "the download worked");

        quiet(cx);
        let document = crate::settings::SettingsDocument {
            updates: UpdatePreferences {
                automatic_downloads: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = crate::settings::Settings::load(
            crate::settings::storage::testing::MemoryStorage::with_document(&document),
        );
        service.update(cx, |updates, _| updates.attach_settings(settings));
        assert!(!retries(&service, cx), "automatic downloads are off");
    }

    /// Relaunch chosen while an automatic download runs makes that download the person's.
    #[gpui::test]
    fn relaunch_chosen_during_an_automatic_download_is_asked(cx: &mut gpui::TestAppContext) {
        let (adapter, service) = launch_fixture(cx);
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        assert!(!service.read_with(cx, |updates, _| updates.download_asked()));
        service.update(cx, |updates, cx| updates.relaunch(cx));
        assert!(service.read_with(cx, |updates, _| updates.download_asked()));
    }

    /// Download only downloads, from every state that offers it: a found release, and a known
    /// one after Stop Download, which a check finds again first. Relaunch stays a second click.
    #[gpui::test]
    fn download_never_relaunches(cx: &mut gpui::TestAppContext) {
        let (adapter, service) = launch_fixture(cx);
        let document = crate::settings::SettingsDocument {
            updates: UpdatePreferences {
                automatic_downloads: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = crate::settings::Settings::load(
            crate::settings::storage::testing::MemoryStorage::with_document(&document),
        );
        service.update(cx, |updates, _| updates.attach_settings(settings));
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.download_now(cx));
        assert_eq!(adapter.downloads.get(), 1);
        // Stop Download, then Download again.
        service.update(cx, |updates, cx| updates.cancel(cx));
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.download_now(cx));
        assert_eq!(
            adapter.checks.get(),
            2,
            "the known release is checked again"
        );
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        assert_eq!(adapter.downloads.get(), 2);
        assert_eq!(adapter.installations.get(), 0);
        assert_eq!(cx.update(|cx| relaunch_approval(cx)), None);
        assert_eq!(
            service.read_with(cx, |updates, _| updates.state().clone()),
            UpdateState::Ready {
                version: "0.1.1".into()
            }
        );
    }

    /// A downloaded release that waits for Relaunch doesn't hide a newer one: the due check
    /// ends its cycle, checks again, and the next Relaunch installs the newest release.
    #[gpui::test]
    fn a_newer_release_replaces_a_ready_one_before_the_relaunch(cx: &mut gpui::TestAppContext) {
        let adapter = Rc::new(RecordingAdapter::available());
        let executor = cx.executor();
        let began = executor.now();
        *adapter.clock.borrow_mut() = Some(Box::new(move || {
            1_000_000 + (executor.now() - began).as_secs()
        }));
        cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
        let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(adapter.checks.get(), 1);
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();

        cx.executor()
            .advance_clock(Duration::from_secs(policy::DAY));
        cx.run_until_parked();
        assert_eq!(
            adapter.cancellations.get(),
            1,
            "the due check ends the cycle"
        );
        assert_eq!(
            adapter.checks.get(),
            1,
            "the check waits for the cycle to end"
        );
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(adapter.checks.get(), 2);
        adapter.emit(UpdateEvent::Available("0.1.2".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();

        service.update(cx, |updates, cx| updates.relaunch(cx));
        assert_eq!(adapter.installations.get(), 1);
        assert_eq!(
            service.read_with(cx, |updates, _| updates.state().clone()),
            UpdateState::Installing {
                version: "0.1.2".into()
            }
        );
    }

    /// Relaunch chosen anywhere while the "Is Ready" alert is open relaunches, and the alert's
    /// later answer changes nothing.
    #[gpui::test]
    fn relaunch_works_while_the_ready_alert_is_open(cx: &mut gpui::TestAppContext) {
        let (adapter, service) = launch_fixture(cx);
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        let open = service.update(cx, |updates, _| {
            updates.begin_install_confirmation().unwrap()
        });
        service.update(cx, |updates, cx| updates.relaunch(cx));
        assert_eq!(adapter.installations.get(), 1);
        assert!(cx.update(|cx| relaunch_approval(cx)).is_some());
        service.update(cx, |updates, cx| {
            updates.finish_install_confirmation(open, false, cx)
        });
        assert!(cx.update(|cx| relaunch_approval(cx)).is_some());
        assert_eq!(adapter.installations.get(), 1);
    }

    /// Settings that can't be saved fail the relaunch at once and say why. The updater never
    /// received the relaunch, so the ordinary quit that follows proceeds and only installs.
    #[gpui::test]
    fn an_unsaved_relaunch_fails_at_once_and_an_ordinary_quit_only_installs(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, service) = launch_fixture(cx);
        let notices = Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = notices.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&service, move |_, notice, _| {
                received.borrow_mut().push(*notice)
            })
        });
        adapter.emit(UpdateEvent::Available("0.1.1".into()));
        adapter.emit(UpdateEvent::Ready);
        cx.run_until_parked();
        service.update(cx, |updates, cx| updates.relaunch(cx));
        let approval = cx.update(|cx| relaunch_approval(cx)).unwrap();
        cx.update(|cx| fail_unsaved_relaunch(approval, cx));
        assert_eq!(
            service.read_with(cx, |updates, _| updates.state().clone()),
            UpdateState::Failed {
                error: UpdateError::SettingsUnsaved
            }
        );
        assert_eq!(*notices.borrow(), [UpdateNotice::Failed]);
        assert_eq!(cx.update(|cx| relaunch_approval(cx)), None);
        assert!(!cx.update(|cx| release_relaunch(approval, cx)));
        assert_eq!(adapter.releases.get(), 0);

        let waits = cx.update(|cx| prepare_before_quit(cx, Rc::new(|_| {})));
        assert!(!waits, "the ordinary quit proceeds and installs on quit");
        cx.executor().advance_clock(RELAUNCH_DEADLINE);
        cx.run_until_parked();
        assert_eq!(
            notices.borrow().len(),
            1,
            "the deadline reports nothing more"
        );
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
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(UpdateEvent::Failed(UpdateError::Check));
        adapter.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert_eq!(
            service.read_with(cx, |updates, _| updates
                .pending_version()
                .map(str::to_owned)),
            Some("0.1.1".into())
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
    fn an_updater_that_ends_its_cycle_during_a_relaunch_fails_the_relaunch() {
        let mut model = ready();
        let token = model.confirmation().unwrap();
        assert!(model.confirm(token, true));
        assert_eq!(
            model.receive(UpdateEvent::Finished),
            Some(UpdateNotice::Failed)
        );
        assert_eq!(
            model.state,
            UpdateState::Failed {
                error: UpdateError::Relaunch
            }
        );
        assert!(!model.install_authorized);
        assert_eq!(model.version.as_deref(), Some("0.1.1"));
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
        // The appcast enclosure the release publishes; test_release pins the same URL.
        let expected = "https://github.com/sadiksaifi/SpaceTerm/releases/download/v0.1.1/SpaceTerm-0.1.1-macos-arm64.dmg";
        assert!(allows_release("0.1.1", "0.1.1", expected, false));
        for url in [
            expected.replace("https:", "http:"),
            expected.replace("SpaceTerm/releases", "other/releases"),
            expected.replace("github.com", "github.com.evil.invalid"),
            format!("{expected}?redirect=1"),
            expected.replace("arm64.dmg", "arm64.pkg"),
            expected.replace("macos-arm64", "darwin-arm64"),
        ] {
            assert!(!allows_release("0.1.1", "0.1.1", &url, false));
        }
        assert!(!allows_release("0.1.1", "0.2.0", expected, false));
        assert!(!allows_release("0.1.1", "0.1.1", expected, true));
    }
}
