//! SpaceTerm Core updates in two steps. Install verifies and unpacks a new version beside the
//! running one and ends nothing. Restart moves the server to that Pending Version through a
//! helper from the previous runtime, and a failed start returns to the previous version.
//!
//! The update record, `updates.json` in the account's Core data, holds what survives a restart:
//! the Pending Version, the skipped version, the last failure and the last feed check. The
//! running server's [`Updates`] owns it; the activation records how a restart ended.
use super::super::{Service, State, boot};
use crate::CoreError;
use crate::storage::Directory;
use crate::update::facts::{
    FailureKind, InstallFailure, OVERDUE_AFTER, Operation, PendingVersion, Platform, Protocols,
    StartFailure, StrandedDevice, UpdateCheck, UpdateFacts, UpdateFailure, Version,
};
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

/// Admits a restart in the running server: it sets the membership fence, records the operation
/// and launches the helper from this runtime. The service lane implements it.
pub(crate) trait Restarter: Send + Sync {
    /// Restart into the installation named by `candidate`, or into the running version.
    /// `silent` refuses with SessionsOpen unless no Terminal Session is live under the fence.
    fn restart(
        self: Arc<Self>,
        candidate: Option<[u8; 32]>,
        silent: bool,
    ) -> Result<(), RestartRefusal>;
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
        record.last_failure = Some(UpdateFailure {
            id: record.failures,
            at: now(),
            version: version.clone(),
            kind: FailureKind::Start(failure),
        });
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
}

/// An install in progress. It reports `Installing` progress until it ends. Dropping it without
/// [`Installing::install`] or [`Installing::fail`] records no failure, as a desktop push that
/// breaks off does.
pub struct Installing<'a> {
    updates: &'a Updates,
    version: Version,
    total: u64,
}

/// A feed check in progress. It reports `Checking` until it ends.
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
        let (running, protocols) = approved(&service, &state)?;
        let record = load(&data)?;
        let shared = Shared {
            service,
            data,
            live: Mutex::new(Live {
                record,
                sessions: 0,
                operation: None,
                facts: placeholder(&running),
            }),
            running,
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
    /// live Terminal Session count until `sessions` returns `None`, when the server stops.
    pub(crate) fn run(
        &self,
        restarter: Weak<dyn Restarter>,
        stranded: Arc<Stranded>,
        sessions: impl Fn() -> Option<u32> + Send + 'static,
    ) -> Result<(), CoreError> {
        let _ = self.shared.restarter.set(restarter);
        let _ = self.shared.stranded.set(stranded);
        self.refresh();
        let shared = Arc::downgrade(&self.shared);
        std::thread::Builder::new()
            .name("spaceterm-core-updates".into())
            .spawn(move || {
                while let Some(count) = sessions() {
                    let Some(shared) = shared.upgrade() else {
                        return;
                    };
                    Self { shared }.set_sessions(count);
                    std::thread::sleep(WATCH_INTERVAL);
                }
            })
            .map(drop)
            .map_err(|_| CoreError::StartupFailed)
    }

    /// Whether an install of `version` would be admitted now. Sources call this before they
    /// download, so a refused version costs nothing.
    ///
    /// # Errors
    /// Returns NotNewer, Skipped or Busy.
    pub fn admits(&self, version: &Version) -> Result<(), InstallError> {
        let live = self.shared.lock();
        self.shared.admits(&live, version)
    }

    /// Begin installing `version` of `total` archive bytes. Facts report `Installing` from now.
    ///
    /// # Errors
    /// Returns what [`Self::admits`] returns.
    pub fn begin_install(
        &self,
        version: Version,
        total: u64,
    ) -> Result<Installing<'_>, InstallError> {
        {
            let mut live = self.shared.lock();
            self.shared.admits(&live, &version)?;
            live.operation = Some(Operation::Installing {
                version: version.clone(),
                received: 0,
                total,
            });
        }
        self.refresh();
        Ok(Installing {
            updates: self,
            version,
            total,
        })
    }

    /// Install an archive whose bytes are all present, such as a local file. The declared
    /// version is the archive's own.
    ///
    /// # Errors
    /// As [`Installing::install`].
    pub fn install(&self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        let metadata = self
            .shared
            .service
            .inspect_update_candidate(archive.archive, archive.signature, archive.sha256)
            .map_err(|error| verification(archive.archive, error))?;
        let version = Version::parse(&metadata.version)
            .map_err(|_| InstallError::Failed(InstallFailure::Verification))?;
        let size = std::fs::metadata(archive.archive).map_or(0, |v| v.len());
        let installing = self.begin_install(version, size)?;
        installing.progress(size);
        installing.install(archive)
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
        let (candidate, to) = {
            let mut live = self.shared.lock();
            if live.operation.is_some() {
                return Err(RestartRefusal::Busy);
            }
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
                if live.record.silent.as_ref() == Some(&attempt) {
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
            let to = pending
                .as_ref()
                .map_or_else(|| self.shared.running.clone(), |p| p.version.clone());
            let candidate = pending
                .map(|pending| digest_bytes(&pending.digest))
                .transpose()
                .map_err(RestartRefusal::Core)?;
            live.operation = Some(Operation::Restarting { to: to.clone() });
            (candidate, to)
        };
        self.refresh();
        if let Err(refusal) = restarter.restart(candidate, request == Restart::Silent) {
            let mut live = self.shared.lock();
            live.operation = None;
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
        Ok(Restarting { to })
    }

    /// Begin a feed check. Facts report `Checking` until [`Checking::finish`] or drop.
    pub fn begin_check(&self) -> Checking<'_> {
        {
            let mut live = self.shared.lock();
            if live.operation.is_none() {
                live.operation = Some(Operation::Checking);
            }
        }
        self.refresh();
        Checking { updates: self }
    }

    /// The server's live Terminal Session count changed. At zero, a waiting Pending Version that
    /// is not breaking restarts silently.
    pub fn set_sessions(&self, sessions: u32) {
        self.shared.lock().sessions = sessions;
        self.refresh();
        if sessions == 0 {
            let _ = self.restart(Restart::Silent);
        }
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

    fn end_operation(&self) {
        let mut live = self.shared.lock();
        if matches!(
            live.operation,
            Some(Operation::Installing { .. } | Operation::Checking)
        ) {
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

    fn install_now(&self, version: &Version, archive: Archive<'_>) -> Result<Stored, InstallError> {
        let service = &self.shared.service;
        let metadata = service
            .inspect_update_candidate(archive.archive, archive.signature, archive.sha256)
            .map_err(|error| verification(archive.archive, error))?;
        if metadata.version != version.as_str() {
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
        {
            let live = self.shared.lock();
            self.shared.admits_version(&live, version)?;
        }
        let size = std::fs::metadata(archive.archive).map_or(0, |v| v.len());
        let installations = service.installations_directory();
        if storage_full(
            &installations,
            size.saturating_mul(2).saturating_add(INSTALL_MARGIN),
        ) {
            return Err(InstallError::Failed(InstallFailure::DiskFull));
        }
        // At most two versions stay on disk: the running one and the one installed now.
        let record = change(data, |record| {
            record.pending = None;
            record.clone()
        })
        .map_err(InstallError::Core)?;
        self.shared.lock().record = record;
        prune(&installations, &state.installation).map_err(InstallError::Core)?;
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
            installed_at: now(),
        };
        let record = change(data, |record| {
            record.pending = Some(stored.clone());
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
        Ok(stored)
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn admits(&self, live: &Live, version: &Version) -> Result<(), InstallError> {
        if live.operation.is_some() {
            return Err(InstallError::Busy);
        }
        self.admits_version(live, version)
    }

    fn admits_version(&self, live: &Live, version: &Version) -> Result<(), InstallError> {
        if live.record.skipped.as_ref() == Some(version) {
            return Err(InstallError::Skipped);
        }
        let newest = [
            Some(&self.running),
            live.record.pending.as_ref().map(|p| &p.version),
        ];
        for have in newest.into_iter().flatten() {
            if order(version, have).is_some_and(Ordering::is_le) {
                return Err(InstallError::NotNewer);
            }
        }
        Ok(())
    }

    fn compose(&self, live: &Live, revision: u64, now: u64) -> UpdateFacts {
        let pending = live.record.pending.as_ref().map(|stored| {
            let breaking = breaking(&self.protocols, &stored.protocols);
            let overdue = stored.installed_at.saturating_add(OVERDUE_AFTER);
            PendingVersion {
                version: stored.version.clone(),
                protocols: stored.protocols,
                breaking,
                installed_at: stored.installed_at,
                overdue_since: (now >= overdue).then_some(overdue),
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
                live.record.check.clone()
            } else {
                UpdateCheck::Disabled
            },
            skipped: live.record.skipped.clone(),
            last_failure: live.record.last_failure.clone(),
        }
    }
}

impl Installing<'_> {
    /// `received` of the archive's bytes arrived.
    pub fn progress(&self, received: u64) {
        {
            let mut live = self.updates.shared.lock();
            live.operation = Some(Operation::Installing {
                version: self.version.clone(),
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
    /// Returns Failed for disk full or a verification failure, NotNewer or Skipped when the
    /// verified build is a version the Core refuses, and Busy while a restart is in progress.
    pub fn install(self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        self.progress(self.total);
        let result = self.updates.install_now(&self.version, archive);
        if let Err(InstallError::Failed(failure)) = result
            && (archive.source == Source::Feed
                || (archive.source == Source::Push && failure == InstallFailure::Verification))
        {
            self.updates.record_failure(&self.version, failure);
        }
        self.updates.end_operation();
        result?;
        self.updates
            .facts()
            .pending
            .ok_or(InstallError::Core(CoreError::Changed))
    }

    /// The source failed, for example its download broke off. A feed install records it.
    pub fn fail(self, failure: InstallFailure) {
        self.updates.record_failure(&self.version, failure);
        self.updates.end_operation();
    }
}

impl Drop for Installing<'_> {
    fn drop(&mut self) {
        self.updates.end_operation();
    }
}

impl Checking<'_> {
    /// Record the check's outcome, which survives restarts.
    pub fn finish(self, check: UpdateCheck) {
        if let Ok(record) = change(&self.updates.shared.data, |record| {
            record.check = check;
            record.clone()
        }) {
            self.updates.shared.lock().record = record;
        }
    }
}

impl Drop for Checking<'_> {
    fn drop(&mut self) {
        self.updates.end_operation();
    }
}

impl FactsSource for Updates {
    fn facts(&self) -> UpdateFacts {
        self.refresh();
        self.shared.lock().facts.clone()
    }

    fn wait(&self, revision: u64, timeout: Duration) -> UpdateFacts {
        let deadline = Instant::now() + timeout;
        let mut live = self.shared.lock();
        while live.facts.revision == revision {
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
        live.facts.clone()
    }
}

/// The approved runtime's version and protocols, from its verified build document.
fn approved(service: &Service, state: &State) -> Result<(Version, Protocols), CoreError> {
    let directory = service.runtime(state);
    let metadata = super::build(&directory, &service.identity)?;
    let version = Version::parse(&metadata.version).map_err(|_| CoreError::RuntimeUnavailable)?;
    Ok((version, metadata.protocols))
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
        | CoreError::IncompatibleProtocol => InstallError::Failed(InstallFailure::Verification),
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

/// Removes every retained installation except `keep`, under the store's lock.
fn prune(parent: &Path, keep: &str) -> Result<(), CoreError> {
    let directory = super::owned_directory(parent)?;
    let _store = super::lock_directory(&directory)?;
    for name in super::files::names(&directory)? {
        let Some(text) = name.to_str() else {
            continue;
        };
        if text == keep || !super::digest_valid(text) {
            continue;
        }
        let path = parent.join(text);
        if std::fs::symlink_metadata(&path)
            .map_err(|_| CoreError::StorageUnavailable)?
            .is_dir()
        {
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
mod tests;
