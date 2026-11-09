//! SpaceTerm Core updates in two steps. Install verifies and unpacks a new version beside the
//! running one and ends nothing. Restart moves the server to that Pending Version through a
//! helper from the previous runtime, and a failed start returns to the previous version.
//!
//! The update record, `updates.json` in the account's Core data, holds what survives a restart:
//! the Pending Version, the skipped version, the last failure and the last feed check. The
//! running server's [`Updates`] owns it; the activation records how a restart ended.
use super::super::{RetainedInstallation, Service, State, boot};
use crate::CoreError;
use crate::storage::Directory;
use crate::update::facts::{
    CheckFailure, FailureKind, InstallFailure, OVERDUE_AFTER, OVERDUE_SINCE_AFTER, Operation,
    PendingVersion, Platform, Protocols, StartFailure, StrandedDevice, UpdateCheck, UpdateFacts,
    UpdateFailure, Version,
};
use crate::update::ordering::{self, Candidate, Floor};
use crate::update::source::FactsSource;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

const RECORD: &std::ffi::CStr = c"updates.json";
const LOCK: &str = "updates.lock";
/// How often the engine reads the live Terminal Session count and the clock.
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
/// How long a Core has no live Terminal Session and no session request before it restarts by
/// itself into a Pending Version. Each session request starts it again.
pub const SILENT_GRACE: Duration = Duration::from_secs(10);
/// Free space an install needs beyond its archive, for the unpacked files.
const INSTALL_MARGIN: u64 = 64 * 1024 * 1024;

/// Where an archive came from. Every source follows the same version order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// This Core's own signed feed. Release builds only.
    Feed,
    /// A desktop pushed its embedded build over the service lane.
    Push,
    /// A local archive, from `install.sh` or `spaceterm update` with a file.
    Local,
}

/// A signed SpaceTerm Core archive on disk, with its raw 64-byte Ed25519 signature.
#[derive(Clone, Copy, Debug)]
pub struct Archive<'a> {
    pub archive: &'a Path,
    pub signature: &'a Path,
    /// 64 lowercase hex digits: the SHA-256 the source declared for `archive`.
    pub sha256: &'a str,
    pub source: Source,
}

/// Why an install did not produce a Pending Version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// The version is not newer than the newest version this Core has.
    #[error("This SpaceTerm Core version is not newer than the installed one.")]
    NotNewer,
    /// The version is the one this Core skipped after it failed to start.
    #[error("This SpaceTerm Core version failed to start before.")]
    Skipped,
    /// The running version is older than the version's Update Floor, so the Core updates to
    /// `floor` first.
    #[error("SpaceTerm Core needs an older update first.")]
    BelowFloor { floor: Floor },
    /// Another install or a restart is in progress.
    #[error("Another SpaceTerm Core update is in progress.")]
    Busy,
    /// The install failed and changed nothing. Feed installs, and pushes that fail
    /// verification, record it as the Core's `last_failure`.
    #[error("The SpaceTerm Core update couldn\u{2019}t be installed.")]
    Failed(InstallFailure),
    #[error("{0}")]
    Core(CoreError),
}

/// What asked for a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    /// A user confirmed it. It ends every Terminal Session and may cross a breaking change.
    /// Without a Pending Version the server restarts into the running version.
    Asked,
    /// The Core decided on its own. It goes ahead only into a Pending Version that is not
    /// breaking, and only at zero Terminal Sessions, at most once per Pending Version and boot.
    /// The engine asks for it once [`SILENT_GRACE`] passed with no session and no session
    /// request.
    Silent,
}

/// Why a restart did not begin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RestartRefusal {
    /// A silent restart found no Pending Version, or already tried this one in this boot.
    #[error("No SpaceTerm Core update is waiting.")]
    NothingPending,
    /// A silent restart found open Terminal Sessions.
    #[error("Terminal Sessions are open.")]
    SessionsOpen,
    /// A silent restart found a breaking Pending Version, which always asks.
    #[error("This SpaceTerm Core update needs confirmation.")]
    Breaking,
    /// Another install or a restart is in progress.
    #[error("Another SpaceTerm Core update is in progress.")]
    Busy,
    #[error("{0}")]
    Core(CoreError),
}

/// A restart the helper now performs. The Core closes its connections next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restarting {
    pub to: Version,
}

/// Admits a restart in the running server: it sets the membership fence and records the
/// operation, then returns the launch of the helper from this runtime. The service lane
/// implements it.
pub(crate) trait Restarter: Send + Sync {
    /// Admit a restart into the installation named by `candidate`, or into the running version.
    /// `silent` refuses with SessionsOpen unless no Terminal Session is live under the fence.
    /// The engine publishes the restart before it calls the returned launch, because the helper
    /// may stop the server at once.
    fn restart(
        self: Arc<Self>,
        candidate: Option<[u8; 32]>,
        silent: bool,
    ) -> Result<Launch, RestartRefusal>;
}

/// Launches the helper of an admitted restart.
pub(crate) type Launch = Box<dyn FnOnce() + Send>;

/// What the running server reports to the engine on each watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Activity {
    /// Live Terminal Sessions now.
    pub(crate) sessions: u32,
    /// A session request, such as a Pane claim or a Workspace edit, arrived since the last
    /// watch.
    pub(crate) requested: bool,
}

/// The Paired Devices that cannot connect to a Core of a work major: those whose last admitted
/// Hello offered no version with it.
pub(crate) type Stranded = dyn Fn(u16) -> Vec<StrandedDevice> + Send + Sync;

/// The update record. Versions that a restart moves between both read it, so it accepts fields
/// it does not know.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    pending: Option<Stored>,
    skipped: Option<Version>,
    last_failure: Option<UpdateFailure>,
    /// The id of the last failure recorded, so ids increase across restarts.
    failures: u64,
    check: UpdateCheck,
    /// The boot and Pending Version of the last silent restart, which is tried once.
    silent: Option<Attempt>,
    /// The activation whose start failure was recorded last, so it is recorded once.
    activation: Option<[u8; 32]>,
    /// The version a feed check installed as a Stepping Stone. A Core that runs it checks its
    /// feed at once, and that check clears it.
    stepping_stone: Option<Version>,
    /// The last restart a reboot, logout or crash interrupted. It stays apart from
    /// `last_failure`, whose values every runtime an update moves between decodes: one of
    /// 0.11.2 or earlier knows no `Interrupted`, and skips this field.
    interrupted: Option<Interruption>,
    /// Why the checks fail, when `check` names a reason 0.11.2 and earlier don't know. `check`
    /// then names `verification`, which every runtime and desktop that reads this record
    /// decodes, and this field, which they skip, keeps the reason.
    check_reason: Option<CheckFailure>,
}

impl Record {
    /// The last check's outcome, with the reason `check_reason` keeps.
    fn check(&self) -> UpdateCheck {
        match (&self.check, self.check_reason) {
            (
                UpdateCheck::Failing {
                    since,
                    last_success,
                    ..
                },
                Some(reason),
            ) => UpdateCheck::Failing {
                since: *since,
                last_success: *last_success,
                reason,
            },
            (check, _) => check.clone(),
        }
    }

    /// Records `check`, naming a reason 0.11.2 doesn't know only in `check_reason`.
    fn set_check(&mut self, check: UpdateCheck) {
        self.check_reason = None;
        self.check = match check {
            UpdateCheck::Failing {
                since,
                last_success,
                reason: reason @ (CheckFailure::ClockBehind | CheckFailure::SteppingStoneSkipped),
            } => {
                self.check_reason = Some(reason);
                UpdateCheck::Failing {
                    since,
                    last_success,
                    reason: CheckFailure::Verification,
                }
            }
            check => check,
        };
    }
}

/// A restart whose readiness a reboot, logout or crash left unmeasured.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Interruption {
    activation: [u8; 32],
    failure: UpdateFailure,
}

impl Default for Record {
    fn default() -> Self {
        Self {
            pending: None,
            skipped: None,
            last_failure: None,
            failures: 0,
            check: UpdateCheck::Never,
            silent: None,
            activation: None,
            stepping_stone: None,
            interrupted: None,
            check_reason: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    /// The installation's directory name: its archive's SHA-256.
    digest: String,
    version: Version,
    protocols: Protocols,
    installed_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Attempt {
    boot: String,
    digest: String,
}

fn load(data: &Directory) -> Result<Record, CoreError> {
    Ok(data
        .read_json::<Record>("updates.json")?
        .unwrap_or_default())
}

/// Edits the update record under its lock, writing it only when it changed.
fn change<T>(data: &Directory, edit: impl FnOnce(&mut Record) -> T) -> Result<T, CoreError> {
    let _lock = data.lock(LOCK, false)?;
    let before = load(data)?;
    let mut record = before.clone();
    let result = edit(&mut record);
    if record != before {
        data.write_json(RECORD, &record)?;
    }
    Ok(result)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The installation a restart moves into: the Pending Version's archive digest.
pub(in crate::service) fn pending_digest(data: &Directory) -> Result<Option<String>, CoreError> {
    Ok(load(data)?.pending.map(|pending| pending.digest))
}

/// The activation into `candidate` became ready: it is the running version now.
pub(in crate::service) fn record_completed(
    data: &Directory,
    candidate: &str,
) -> Result<(), CoreError> {
    change(data, |record| {
        if record
            .pending
            .as_ref()
            .is_some_and(|pending| pending.digest == candidate)
        {
            record.pending = None;
        }
        record.silent = None;
    })
}

/// The activation into `candidate` did not become ready, and the Core returned to the previous
/// version. A version that is not ready is skipped; a transient failure keeps it pending.
pub(in crate::service) fn record_start_failure(
    data: &Directory,
    activation: [u8; 32],
    candidate: &str,
    version: &str,
    failure: StartFailure,
) -> Result<(), CoreError> {
    let version = Version::parse(version).map_err(|_| CoreError::UnsafeStorage)?;
    change(data, |record| {
        if record.activation == Some(activation) {
            return;
        }
        record.activation = Some(activation);
        record.failures += 1;
        let recorded = UpdateFailure {
            id: record.failures,
            at: now(),
            version: version.clone(),
            kind: FailureKind::Start(failure),
        };
        if failure == StartFailure::Interrupted {
            record.interrupted = Some(Interruption {
                activation,
                failure: recorded,
            });
            return;
        }
        record.last_failure = Some(recorded);
        if failure == StartFailure::NotReady {
            record.skipped = Some(version);
            if record
                .pending
                .as_ref()
                .is_some_and(|pending| pending.digest == candidate)
            {
                record.pending = None;
            }
        }
    })
}

/// Whether the update record names the restart of `activation` as interrupted.
pub(in crate::service) fn interrupted(data: &Directory, activation: [u8; 32]) -> bool {
    load(data).is_ok_and(|record| {
        record
            .interrupted
            .is_some_and(|interrupted| interrupted.activation == activation)
    })
}

/// Applies the outcome the activation journal proves to the update record. The activation
/// writes its journal first, so a power loss or a lost helper can leave the record behind it.
/// Both applications are idempotent.
fn reconcile(data: &Directory, state: &State) -> Result<(), CoreError> {
    use super::super::activation::Outcome;
    match state
        .activation
        .as_ref()
        .and_then(|operation| operation.outcome())
    {
        Some(Outcome::Completed { candidate }) => record_completed(data, candidate),
        Some(Outcome::Returned {
            activation,
            candidate,
            version,
            failure,
        }) => record_start_failure(data, activation, candidate, version, failure),
        None => Ok(()),
    }
}

/// Whether the volume holding `path` has less than `needed` bytes free for this account.
pub(in crate::service) fn storage_full(path: &Path, needed: u64) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: statvfs fills the zeroed structure for a terminated path.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(name.as_ptr(), &mut stats) } != 0 {
        return false;
    }
    #[allow(clippy::useless_conversion)]
    let free = u64::from(stats.f_bavail).saturating_mul(u64::from(stats.f_frsize));
    free < needed
}

/// Orders two versions of one build identity. Release versions order numerically.
/// Development versions have no order, so only equal ones compare.
fn order(a: &Version, b: &Version) -> Option<Ordering> {
    match (a.release(), b.release()) {
        (Some(a), Some(b)) => Some(a.cmp(&b)),
        _ => (a == b).then_some(Ordering::Equal),
    }
}

/// Whether `pending` crosses a work or service major from `running`.
fn breaking(running: &Protocols, pending: &Protocols) -> bool {
    running.work.major != pending.work.major || running.service.major != pending.service.major
}

const fn platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        Platform::MacosArm64
    }
    #[cfg(target_os = "linux")]
    {
        Platform::LinuxX86_64
    }
}

/// The update engine of one SpaceTerm Core, shared by its listeners, its service lane and its
/// update sources. It owns the update record and produces the Core's [`UpdateFacts`].
#[derive(Clone)]
pub struct Updates {
    shared: Arc<Shared>,
}

struct Shared {
    service: Service,
    data: Directory,
    running: Version,
    /// The approved runtime this server started from: the build a restart without a Pending
    /// Version starts again.
    build: RetainedInstallation,
    protocols: Protocols,
    live: Mutex<Live>,
    changed: Condvar,
    restarter: OnceLock<Weak<dyn Restarter>>,
    stranded: OnceLock<Arc<Stranded>>,
}

struct Live {
    record: Record,
    sessions: u32,
    operation: Option<Operation>,
    facts: UpdateFacts,
    /// Since when the server had no live Terminal Session and no session request. None while
    /// a session is live.
    quiet_since: Option<Instant>,
    /// A restart or confirmed change is being admitted. It holds the update operation like
    /// `operation`, but the facts show it only once the admission succeeded, so a refused one
    /// never shows.
    reserved: bool,
    /// A silent restart that settled while this server kept running waits until then before
    /// the next one, so a helper that keeps failing does not fence the server every second.
    silent_after: Option<Instant>,
    /// The server is shutting down, so no caller waits for the facts to change.
    stopped: bool,
    /// Retained installations may still wait for removal, because another service operation,
    /// such as the activation that started this server, held the service lock.
    retire_pending: bool,
}

impl Live {
    /// Whether an update operation runs or is being admitted.
    fn busy(&self) -> bool {
        self.operation.is_some() || self.reserved
    }
}

/// A change a client confirmed over the service lane, which the engine owns while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Confirmed {
    /// A restart into the installation named by `candidate`, or into the running version.
    Restart {
        candidate: Option<[u8; 32]>,
    },
    Stop,
    /// Remove Machine. It moves into no installation, so a Pending Version doesn't matter.
    Uninstall,
}

/// How long a silent restart that settled without the server stopping waits before it retries.
const SILENT_RETRY: Duration = Duration::from_secs(300);

/// An install in progress. It reports `Installing` progress until it ends. Dropping it without
/// [`Installing::install`] or [`Installing::fail`] records no failure, as a desktop push that
/// breaks off does. It owns its engine handle, so a push can span several lane requests.
pub struct Installing {
    updates: Updates,
    candidate: Candidate,
    total: u64,
    /// A feed check installs the candidate as a Stepping Stone.
    stepping_stone: bool,
}

/// A feed check in progress. It owns the Core's one update operation and reports `Checking`
/// until it ends or becomes the install of the release it found.
pub struct Checking<'a> {
    updates: &'a Updates,
}

impl Updates {
    /// The engine of the server running `service`'s approved runtime.
    ///
    /// # Errors
    /// Returns NotInstalled without service state, and the storage or verification
    /// classifications for unreadable account storage or an approved runtime that no longer
    /// verifies.
    pub fn open(service: Service) -> Result<Self, CoreError> {
        let (data, state) = service.read()?.ok_or(CoreError::NotInstalled)?;
        let (build, protocols) = approved(&service, &state)?;
        let running = Version::parse(&build.version).map_err(|_| CoreError::RuntimeUnavailable)?;
        reconcile(&data, &state)?;
        let record = load(&data)?;
        let shared = Shared {
            service,
            data,
            live: Mutex::new(Live {
                record,
                sessions: 0,
                operation: None,
                facts: placeholder(&running),
                quiet_since: None,
                reserved: false,
                silent_after: None,
                stopped: false,
                retire_pending: false,
            }),
            running,
            build,
            protocols,
            changed: Condvar::new(),
            restarter: OnceLock::new(),
            stranded: OnceLock::new(),
        };
        {
            let mut live = shared.lock();
            live.facts = shared.compose(&live, 1, now());
        }
        Ok(Self {
            shared: Arc::new(shared),
        })
    }

    /// The persisted facts of a stopped Core, with `sessions: 0` and no operation.
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn stored(service: &Service) -> Result<UpdateFacts, CoreError> {
        Ok(Self::open(service.clone())?.facts())
    }

    /// Connect the running server's restart admission and Paired Device view, then follow its
    /// live Terminal Sessions and session requests until `activity` returns `None`, when the
    /// server stops.
    pub(crate) fn run(
        &self,
        restarter: Weak<dyn Restarter>,
        stranded: Arc<Stranded>,
        activity: impl Fn() -> Option<Activity> + Send + 'static,
    ) -> Result<(), CoreError> {
        let _ = self.shared.restarter.set(restarter);
        let _ = self.shared.stranded.set(stranded);
        self.refresh();
        let shared = Arc::downgrade(&self.shared);
        std::thread::Builder::new()
            .name("spaceterm-core-updates".into())
            .spawn(move || {
                while let Some(activity) = activity() {
                    let Some(shared) = shared.upgrade() else {
                        return;
                    };
                    Self { shared }.observe(activity, Instant::now());
                    std::thread::sleep(WATCH_INTERVAL);
                }
            })
            .map(drop)
            .map_err(|_| CoreError::StartupFailed)
    }

    /// Whether an install of `candidate` would be admitted now. Sources call this before they
    /// download, so a refused version costs nothing.
    ///
    /// # Errors
    /// Returns NotNewer, Skipped, BelowFloor or Busy.
    pub fn admits(&self, candidate: &Candidate) -> Result<(), InstallError> {
        let live = self.shared.lock();
        self.shared.admits(&live, candidate)
    }

    /// Whether `candidate` is a version this Core installs, whatever update operation runs now.
    ///
    /// # Errors
    /// Returns NotNewer, Skipped or BelowFloor.
    pub fn admits_version(&self, candidate: &Candidate) -> Result<(), InstallError> {
        let live = self.shared.lock();
        self.shared.admits_version(&live, candidate)
    }

    /// Begin installing `candidate` of `total` archive bytes. Facts report `Installing` from
    /// now.
    ///
    /// # Errors
    /// Returns what [`Self::admits`] returns.
    pub fn begin_install(
        &self,
        candidate: Candidate,
        total: u64,
    ) -> Result<Installing, InstallError> {
        {
            let mut live = self.shared.lock();
            self.shared.admits(&live, &candidate)?;
            live.operation = Some(Operation::Installing {
                version: candidate.version().clone(),
                received: 0,
                total,
            });
        }
        self.refresh();
        Ok(Installing {
            updates: self.clone(),
            candidate,
            total,
            stepping_stone: false,
        })
    }

    /// Install an archive whose bytes are all present, such as a local file. The declared
    /// version and floor are the archive's own.
    ///
    /// # Errors
    /// As [`Installing::install`].
    pub fn install(&self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        let candidate = self
            .shared
            .service
            .inspect_update_candidate(archive.archive, archive.signature, archive.sha256)
            .and_then(|metadata| metadata.candidate())
            .map_err(|error| verification(archive.archive, error))?;
        let size = std::fs::metadata(archive.archive).map_or(0, |v| v.len());
        let installing = self.begin_install(candidate, size)?;
        installing.progress(size);
        installing.install(archive)
    }

    /// The approved runtime this server started from, whose version its facts report as
    /// running.
    pub fn running(&self) -> &RetainedInstallation {
        &self.shared.build
    }

    /// Whether this Core runs a Stepping Stone its feed check installed and has not checked its
    /// feed since.
    pub fn stepping_stone(&self) -> bool {
        self.shared.lock().record.stepping_stone.as_ref() == Some(&self.shared.running)
    }

    /// Begin a restart. A restart into a Pending Version goes through a helper from the running
    /// runtime with a readiness deadline; a failed start restores the retained-state snapshot,
    /// returns to this version and skips the Pending Version unless the failure is transient.
    ///
    /// # Errors
    /// Returns a [`RestartRefusal`] when the restart did not begin.
    pub fn restart(&self, request: Restart) -> Result<Restarting, RestartRefusal> {
        let restarter = self
            .shared
            .restarter
            .get()
            .and_then(Weak::upgrade)
            .ok_or(RestartRefusal::Core(CoreError::NotRunning))?;
        let (candidate, to, operation) = {
            let mut live = self.shared.lock();
            if live.busy() {
                return Err(RestartRefusal::Busy);
            }
            // Another process, such as a local installer, may have changed the record.
            live.record = load(&self.shared.data).map_err(RestartRefusal::Core)?;
            let pending = live.record.pending.clone();
            if request == Restart::Silent {
                let pending = pending.as_ref().ok_or(RestartRefusal::NothingPending)?;
                if breaking(&self.shared.protocols, &pending.protocols) {
                    return Err(RestartRefusal::Breaking);
                }
                if live.sessions > 0 {
                    return Err(RestartRefusal::SessionsOpen);
                }
                let attempt = Attempt {
                    boot: boot().map_err(RestartRefusal::Core)?,
                    digest: pending.digest.clone(),
                };
                if live.record.silent.as_ref() == Some(&attempt)
                    || live
                        .silent_after
                        .is_some_and(|after| Instant::now() < after)
                {
                    return Err(RestartRefusal::NothingPending);
                }
                // Recorded before admission, so a start that fails returns to a runtime that
                // does not try the same Pending Version again in this boot.
                change(&self.shared.data, |record| {
                    record.silent = Some(attempt.clone());
                })
                .map_err(RestartRefusal::Core)?;
                live.record.silent = Some(attempt);
            }
            let (to, operation) = self.shared.restart_into(pending.as_ref());
            let candidate = pending
                .map(|pending| digest_bytes(&pending.digest))
                .transpose()
                .map_err(RestartRefusal::Core)?;
            // The facts show the restart only once the fence admitted it.
            live.reserved = true;
            (candidate, to, operation)
        };
        let admitted = restarter.restart(candidate, request == Restart::Silent);
        let mut live = self.shared.lock();
        live.reserved = false;
        if let Err(refusal) = &admitted {
            let refusal = *refusal;
            if request == Restart::Silent
                && let Ok(record) = change(&self.shared.data, |record| {
                    record.silent = None;
                    record.clone()
                })
            {
                live.record = record;
            }
            drop(live);
            self.refresh();
            return Err(refusal);
        }
        live.operation = Some(operation);
        drop(live);
        self.refresh();
        // Every follower hears the restart before the helper can stop this server.
        if let Ok(launch) = admitted {
            launch();
        }
        Ok(Restarting { to })
    }

    /// Holds the update operation while `admit`, the lane's admission of the confirmed change,
    /// runs. Once it admitted the change, `change` is the update operation, so every client's
    /// facts show `Restarting`, `RestartingSameVersion`, `Stopping` or `Uninstalling`. A failed
    /// admission never shows.
    ///
    /// # Errors
    /// Returns ServiceBusy while another update operation runs, Changed when `candidate` is not
    /// the Pending Version, and what `admit` returns.
    pub(crate) fn confirm<T>(
        &self,
        change: Confirmed,
        admit: impl FnOnce() -> Result<T, CoreError>,
    ) -> Result<T, CoreError> {
        let operation = {
            let mut live = self.shared.lock();
            if live.busy() {
                return Err(CoreError::ServiceBusy);
            }
            // Another process, such as a local installer, may have changed the record.
            live.record = load(&self.shared.data)?;
            let operation = match change {
                Confirmed::Stop => Operation::Stopping,
                Confirmed::Uninstall => Operation::Uninstalling,
                Confirmed::Restart { candidate } => {
                    let pending = live.record.pending.as_ref();
                    let digest = pending
                        .map(|pending| digest_bytes(&pending.digest))
                        .transpose()?;
                    if digest != candidate {
                        return Err(CoreError::Changed);
                    }
                    self.shared.restart_into(pending).1
                }
            };
            live.reserved = true;
            operation
        };
        let admitted = admit();
        let mut live = self.shared.lock();
        live.reserved = false;
        if admitted.is_ok() {
            live.operation = Some(operation);
        }
        drop(live);
        self.refresh();
        admitted
    }

    /// Every admitted service operation settled while this server still runs, so a restart or
    /// Stop it owned did not stop it. That operation ends, and a silent restart may try again
    /// after [`SILENT_RETRY`].
    pub(crate) fn operations_settled(&self) {
        let mut live = self.shared.lock();
        if !matches!(
            live.operation,
            Some(
                Operation::Restarting { .. }
                    | Operation::RestartingSameVersion
                    | Operation::Stopping
                    | Operation::Uninstalling
            )
        ) {
            return;
        }
        live.operation = None;
        if live.record.silent.is_some()
            && let Ok(record) = change(&self.shared.data, |record| {
                record.silent = None;
                record.clone()
            })
        {
            live.record = record;
            live.silent_after = Some(Instant::now() + SILENT_RETRY);
        }
        drop(live);
        self.refresh();
    }

    /// Begin a feed check. Facts report `Checking` until the check is dropped or
    /// [`Checking::begin_install`] turns it into an install.
    ///
    /// # Errors
    /// Returns Busy while another install, check or restart owns the update operation.
    pub fn begin_check(&self) -> Result<Checking<'_>, InstallError> {
        {
            let mut live = self.shared.lock();
            if live.busy() {
                return Err(InstallError::Busy);
            }
            live.operation = Some(Operation::Checking);
        }
        self.refresh();
        Ok(Checking { updates: self })
    }

    /// The server reported `activity` at `now`. Once it had no live Terminal Session and no
    /// session request for [`SILENT_GRACE`], a waiting Pending Version that is not breaking
    /// restarts silently.
    pub(crate) fn observe(&self, activity: Activity, now: Instant) {
        let due = {
            let mut live = self.shared.lock();
            live.sessions = activity.sessions;
            if activity.sessions > 0 {
                live.quiet_since = None;
            } else if activity.requested || live.quiet_since.is_none() {
                live.quiet_since = Some(now);
            }
            live.quiet_since
                .is_some_and(|since| now.saturating_duration_since(since) >= SILENT_GRACE)
        };
        self.refresh();
        // The activation that started this server has settled once its lock is free.
        if self.shared.lock().retire_pending && self.retire_installations() {
            self.shared.lock().retire_pending = false;
        }
        if due {
            let _ = self.restart(Restart::Silent);
        }
    }

    /// The server is shutting down. Every caller waiting for the facts to change returns now,
    /// and later waits return at once, so connection threads finish without the long poll and
    /// facts followers end.
    pub(crate) fn stop(&self) {
        self.shared.lock().stopped = true;
        self.shared.changed.notify_all();
    }

    fn refresh(&self) {
        let mut live = self.shared.lock();
        let next = self.shared.compose(&live, live.facts.revision, now());
        if next != live.facts {
            live.facts = UpdateFacts {
                revision: live.facts.revision + 1,
                ..next
            };
            self.shared.changed.notify_all();
        }
    }

    /// Ends the update operation `owned` names, and leaves any other one.
    fn end_operation(&self, owned: fn(&Operation) -> bool) {
        let mut live = self.shared.lock();
        if live.operation.as_ref().is_some_and(owned) {
            live.operation = None;
        }
        drop(live);
        self.refresh();
    }

    fn record_failure(&self, version: &Version, failure: InstallFailure) {
        let failed = change(&self.shared.data, |record| {
            record.failures += 1;
            record.last_failure = Some(UpdateFailure {
                id: record.failures,
                at: now(),
                version: version.clone(),
                kind: FailureKind::Install(failure),
            });
            record.clone()
        });
        if let Ok(record) = failed {
            self.shared.lock().record = record;
        }
    }

    /// Verifies the archive's signed build document against what its source declared, then
    /// unpacks it beside the running version.
    fn install_now(
        &self,
        candidate: &Candidate,
        archive: Archive<'_>,
        stepping_stone: bool,
    ) -> Result<Stored, InstallError> {
        let version = candidate.version();
        let service = &self.shared.service;
        let metadata = service
            .inspect_update_candidate(archive.archive, archive.signature, archive.sha256)
            .map_err(|error| verification(archive.archive, error))?;
        if metadata.version != version.as_str() || metadata.floor != candidate.floor() {
            return Err(InstallError::Failed(InstallFailure::Verification));
        }
        let data = &self.shared.data;
        let _operation = match data
            .wait_lock("service.lock", super::super::OPERATION_GRACE, None)
            .map_err(InstallError::Core)?
        {
            crate::LockWait::Locked(lock) => lock,
            crate::LockWait::Held | crate::LockWait::Cancelled => return Err(InstallError::Busy),
        };
        let state = Service::state(data)
            .map_err(InstallError::Core)?
            .ok_or(InstallError::Core(CoreError::NotInstalled))?;
        if state
            .activation
            .as_ref()
            .is_some_and(super::super::activation::Operation::pending)
        {
            return Err(InstallError::Busy);
        }
        // Another engine, such as a second local installer, may have installed since this one
        // read the record. The service lock serializes installs, so decide on the record now.
        {
            let record = load(data).map_err(InstallError::Core)?;
            let mut live = self.shared.lock();
            live.record = record;
            self.shared.admits_version(&live, candidate)?;
        }
        let size = std::fs::metadata(archive.archive).map_or(0, |v| v.len());
        let installations = service.installations_directory();
        let replaced = self.shared.lock().record.pending.clone();
        // Obsolete installations go first, so the space check counts what they free. A waiting
        // Pending Version stays until this one has staged.
        let waiting = replaced.as_ref().map(|pending| pending.digest.as_str());
        prune(service, &state, &[waiting]).map_err(InstallError::Core)?;
        if storage_full(
            &installations,
            size.saturating_mul(2).saturating_add(INSTALL_MARGIN),
        ) {
            return Err(InstallError::Failed(InstallFailure::DiskFull));
        }
        let retained = service
            .stage_candidate(archive.archive, archive.signature, archive.sha256)
            .map_err(|error| {
                if storage_full(&installations, size.saturating_add(INSTALL_MARGIN)) {
                    InstallError::Failed(InstallFailure::DiskFull)
                } else {
                    verification(archive.archive, error)
                }
            })?;
        let digest = retained
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(InstallError::Core(CoreError::RuntimeUnavailable))?
            .to_owned();
        let stored = Stored {
            digest,
            version: version.clone(),
            protocols: metadata.protocols,
            // A version that replaces a waiting one waits as long as that one did, so it is
            // Overdue as soon.
            installed_at: replaced
                .as_ref()
                .map_or_else(now, |pending| pending.installed_at),
        };
        let record = change(data, |record| {
            record.pending = Some(stored.clone());
            record.stepping_stone = stepping_stone.then(|| version.clone());
            // A newer version than the skipped one releases it.
            if record
                .skipped
                .as_ref()
                .is_some_and(|skipped| order(version, skipped) == Some(Ordering::Greater))
            {
                record.skipped = None;
            }
            record.clone()
        })
        .map_err(InstallError::Core)?;
        self.shared.lock().record = record;
        // At most two versions stay on disk: the running one and the one installed now.
        prune(service, &state, &[Some(stored.digest.as_str())]).map_err(InstallError::Core)?;
        Ok(stored)
    }

    /// Removes what a crash or a finished update left behind: staged downloads, unfinished
    /// unpackings, and retained installations that are neither the running one nor the Pending
    /// Version. A server calls this once as it starts, before it serves any request, so no
    /// download or push of its own is staged yet.
    pub(crate) fn remove_leftovers(&self) {
        crate::update::sources::clear_staging(self.shared.service.paths.data.as_path());
        let retired = self.retire_installations();
        self.shared.lock().retire_pending = !retired;
    }

    /// Removes the retained installations no longer needed, once no other service operation
    /// holds the service lock. An activation holds it until it settles: the helper of a release
    /// before this one removes nothing itself. Returns whether it ran.
    fn retire_installations(&self) -> bool {
        match self.shared.data.try_lock("service.lock") {
            Ok(Some(_operation)) => retire_obsolete(&self.shared.service).is_ok(),
            _ => false,
        }
    }
}

/// Removes the retained installations `service` no longer needs, unless an activation may
/// still return to its previous one. On macOS each is unregistered from Launch Services first.
/// The caller holds the service lock, so no install or activation changes them meanwhile.
pub(in crate::service) fn retire_obsolete(service: &Service) -> Result<(), CoreError> {
    let (data, state) = service.read()?.ok_or(CoreError::NotInstalled)?;
    let pending = load(&data)?.pending.map(|pending| pending.digest);
    prune(service, &state, &[pending.as_deref()])
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The version a restart moves into, and the operation the facts show for it: the Pending
    /// Version's `Restarting`, or `RestartingSameVersion` without one.
    fn restart_into(&self, pending: Option<&Stored>) -> (Version, Operation) {
        match pending {
            Some(pending) => (
                pending.version.clone(),
                Operation::Restarting {
                    to: pending.version.clone(),
                },
            ),
            None => (self.running.clone(), Operation::RestartingSameVersion),
        }
    }

    fn admits(&self, live: &Live, candidate: &Candidate) -> Result<(), InstallError> {
        if live.busy() {
            return Err(InstallError::Busy);
        }
        self.admits_version(live, candidate)
    }

    /// Every source passes through the one version order in [`crate::update::ordering`].
    fn admits_version(&self, live: &Live, candidate: &Candidate) -> Result<(), InstallError> {
        ordering::admit(
            candidate,
            &self.running,
            live.record.pending.as_ref().map(|pending| &pending.version),
            live.record.skipped.as_ref(),
        )
        .map_err(|refusal| match refusal {
            ordering::Refusal::NotNewer => InstallError::NotNewer,
            ordering::Refusal::Skipped => InstallError::Skipped,
            ordering::Refusal::BelowFloor { floor } => InstallError::BelowFloor { floor },
        })
    }

    fn compose(&self, live: &Live, revision: u64, now: u64) -> UpdateFacts {
        let pending = live.record.pending.as_ref().map(|stored| {
            let breaking = breaking(&self.protocols, &stored.protocols);
            let overdue = stored.installed_at.saturating_add(OVERDUE_AFTER);
            let since = stored.installed_at.saturating_add(OVERDUE_SINCE_AFTER);
            PendingVersion {
                version: stored.version.clone(),
                protocols: stored.protocols,
                breaking,
                installed_at: stored.installed_at,
                overdue_since: (now >= overdue).then_some(since),
                stranded: match (breaking, self.stranded.get()) {
                    (true, Some(stranded)) => stranded(stored.protocols.work.major),
                    _ => Vec::new(),
                },
            }
        });
        UpdateFacts {
            revision,
            platform: platform(),
            running: self.running.clone(),
            pending,
            sessions: live.sessions,
            operation: live.operation.clone(),
            check: if self.service.identity == "spaceterm" {
                live.record.check()
            } else {
                UpdateCheck::Disabled
            },
            skipped: live.record.skipped.clone(),
            // The newest failure, whichever field holds it.
            last_failure: live
                .record
                .last_failure
                .iter()
                .chain(live.record.interrupted.iter().map(|i| &i.failure))
                .max_by_key(|failure| failure.id)
                .cloned(),
        }
    }
}

impl Installing {
    /// `received` of the archive's bytes arrived.
    pub fn progress(&self, received: u64) {
        {
            let mut live = self.updates.shared.lock();
            live.operation = Some(Operation::Installing {
                version: self.candidate.version().clone(),
                received: received.min(self.total),
                total: self.total,
            });
        }
        self.updates.refresh();
    }

    /// Verify the received archive against the declared version and unpack it beside the
    /// running version. At most two versions stay on disk: the running one and this one.
    ///
    /// # Errors
    /// Returns Failed for disk full or a verification failure, NotNewer, Skipped or BelowFloor
    /// when the verified build is a version the Core refuses, and Busy while a restart is in
    /// progress.
    pub fn install(self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        self.progress(self.total);
        let result = self
            .updates
            .install_now(&self.candidate, archive, self.stepping_stone);
        if let Err(InstallError::Failed(failure)) = result
            && (archive.source == Source::Feed
                || (archive.source == Source::Push && failure == InstallFailure::Verification))
        {
            self.updates
                .record_failure(self.candidate.version(), failure);
        }
        self.updates.end_operation(INSTALLING);
        result?;
        self.updates
            .facts()
            .pending
            .ok_or(InstallError::Core(CoreError::Changed))
    }

    /// The source failed, for example its download broke off. A feed install records it.
    pub fn fail(self, failure: InstallFailure) {
        self.updates
            .record_failure(self.candidate.version(), failure);
        self.updates.end_operation(INSTALLING);
    }
}

impl Drop for Installing {
    fn drop(&mut self) {
        self.updates.end_operation(INSTALLING);
    }
}

impl<'a> Checking<'a> {
    /// Whether an install of `candidate` would be admitted once this check hands over to it.
    ///
    /// # Errors
    /// Returns NotNewer, Skipped or BelowFloor.
    pub fn admits(&self, candidate: &Candidate) -> Result<(), InstallError> {
        let live = self.updates.shared.lock();
        self.updates.shared.admits_version(&live, candidate)
    }

    /// Turns this check into the install of `candidate` of `total` archive bytes, with no
    /// moment when another install could take the update operation. A `stepping_stone` makes
    /// the Core that restarts into it check its feed at once.
    ///
    /// # Errors
    /// Returns NotNewer, Skipped or BelowFloor.
    pub fn begin_install(
        self,
        candidate: Candidate,
        total: u64,
        stepping_stone: bool,
    ) -> Result<Installing, InstallError> {
        {
            let mut live = self.updates.shared.lock();
            self.updates.shared.admits_version(&live, &candidate)?;
            live.operation = Some(Operation::Installing {
                version: candidate.version().clone(),
                received: 0,
                total,
            });
        }
        let installing = Installing {
            updates: self.updates.clone(),
            candidate,
            total,
            stepping_stone,
        };
        // The install owns the operation now, so the check must not end it.
        std::mem::forget(self);
        installing.updates.refresh();
        Ok(installing)
    }

    /// Record the check's outcome, which survives restarts. The check still owns the update
    /// operation until it is dropped or begins an install.
    pub fn finish(&self, check: UpdateCheck) {
        let running = &self.updates.shared.running;
        if let Ok(record) = change(&self.updates.shared.data, |record| {
            record.set_check(check);
            if record.stepping_stone.as_ref() == Some(running) {
                record.stepping_stone = None;
            }
            record.clone()
        }) {
            self.updates.shared.lock().record = record;
        }
    }
}

impl Drop for Checking<'_> {
    fn drop(&mut self) {
        self.updates
            .end_operation(|operation| *operation == Operation::Checking);
    }
}

/// Whether an update operation is an install's.
const INSTALLING: fn(&Operation) -> bool =
    |operation| matches!(operation, Operation::Installing { .. });

impl FactsSource for Updates {
    fn facts(&self) -> UpdateFacts {
        self.refresh();
        self.shared.lock().facts.clone()
    }

    fn wait(&self, revision: u64, timeout: Duration) -> Option<UpdateFacts> {
        let deadline = Instant::now() + timeout;
        let mut live = self.shared.lock();
        while live.facts.revision == revision && !live.stopped {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            live = self
                .shared
                .changed
                .wait_timeout(live, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        (!live.stopped).then(|| live.facts.clone())
    }
}

/// The approved runtime's version and protocols, from its verified build document.
fn approved(
    service: &Service,
    state: &State,
) -> Result<(RetainedInstallation, Protocols), CoreError> {
    let directory = service.runtime(state);
    let metadata = super::build(&directory, &service.identity)?;
    Ok((
        RetainedInstallation {
            directory,
            version: metadata.version,
            commit: metadata.commit,
        },
        metadata.protocols,
    ))
}

fn placeholder(running: &Version) -> UpdateFacts {
    UpdateFacts {
        revision: 0,
        platform: platform(),
        running: running.clone(),
        pending: None,
        sessions: 0,
        operation: None,
        check: UpdateCheck::Never,
        skipped: None,
        last_failure: None,
    }
}

/// Classifies a failed verification or retention of `archive`.
fn verification(archive: &Path, error: CoreError) -> InstallError {
    match error {
        CoreError::RuntimeUnavailable
        | CoreError::CandidateChanged
        | CoreError::CandidateMissing
        | CoreError::IncompatibleProtocol(_) => InstallError::Failed(InstallFailure::Verification),
        CoreError::StorageUnavailable
            if archive
                .parent()
                .is_some_and(|parent| storage_full(parent, INSTALL_MARGIN)) =>
        {
            InstallError::Failed(InstallFailure::DiskFull)
        }
        error => InstallError::Core(error),
    }
}

/// Removes every unfinished unpacking and every retained installation except the one `state`
/// approves, both installations of an activation that may still return to its previous one,
/// and those `keep` names. It runs under the store's lock, which every unpacking holds until
/// it publishes, so each unfinished one is a crash's. On macOS each installation is
/// unregistered from Launch Services before it goes, so System Settings never names a bundle
/// that no longer exists.
fn prune(service: &Service, state: &State, keep: &[Option<&str>]) -> Result<(), CoreError> {
    let rollback = state
        .activation
        .as_ref()
        .filter(|operation| operation.retains_previous())
        .map(super::super::activation::Operation::installations);
    let kept = |text: &str| {
        text == state.installation
            || keep.contains(&Some(text))
            || rollback.is_some_and(|digests| digests.contains(&text))
    };
    remove_entries(&service.installations_directory(), |text| {
        if unfinished(text) {
            return Some(None);
        }
        (super::digest_valid(text) && !kept(text)).then(|| Some(service.supervisor.as_ref()))
    })
}

/// The name of an unpacking that never published: `.prepare-` and 32 hex digits.
fn unfinished(text: &str) -> bool {
    text.strip_prefix(".prepare-").is_some_and(|rest| {
        rest.len() == 32
            && rest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Removes each directory in `parent` that `remove` chooses, under the store's lock. A chosen
/// installation is first retired from the service manager `remove` names.
fn remove_entries<'a>(
    parent: &Path,
    remove: impl Fn(&str) -> Option<Option<&'a dyn super::super::Supervisor>>,
) -> Result<(), CoreError> {
    if !parent.exists() {
        return Ok(());
    }
    let directory = super::owned_directory(parent)?;
    let _store = super::lock_directory(&directory)?;
    for name in super::files::names(&directory)? {
        let Some(text) = name.to_str() else {
            continue;
        };
        let Some(supervisor) = remove(text) else {
            continue;
        };
        let path = parent.join(text);
        if std::fs::symlink_metadata(&path)
            .map_err(|_| CoreError::StorageUnavailable)?
            .is_dir()
        {
            if let Some(supervisor) = supervisor {
                supervisor.retire(&path);
            }
            make_writable(&path)?;
            std::fs::remove_dir_all(&path).map_err(|_| CoreError::StorageUnavailable)?;
        }
    }
    super::verify_directory_path(&directory, parent)
}

/// Retained installations keep read-only directories; removal needs them writable.
fn make_writable(path: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| CoreError::StorageUnavailable)?;
    if metadata.is_dir() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| CoreError::StorageUnavailable)?;
        for entry in std::fs::read_dir(path).map_err(|_| CoreError::StorageUnavailable)? {
            make_writable(&entry.map_err(|_| CoreError::StorageUnavailable)?.path())?;
        }
    }
    Ok(())
}

fn digest_bytes(text: &str) -> Result<[u8; 32], CoreError> {
    if !super::digest_valid(text) {
        return Err(CoreError::UnsafeStorage);
    }
    let mut out = [0; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| CoreError::UnsafeStorage)?;
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests;
