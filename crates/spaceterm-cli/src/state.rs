//! The CLI's own state under `clients/cli`: Saved Connections, and per route and identity the
//! client record, the Workspace edit journal, attachments and pending runs.
//!
//! ```text
//! clients/cli/
//!   connections.json, connections.lock
//!   routes/<local | 64-hex route>/
//!     base/            client.json (local route only), operations.json, operations.lock,
//!                      attachments.json, runs.json, runs.lock, runs/<id>.lock,
//!                      inventory.json (paired routes only)
//!     agents/<name>/   the same files except client.json
//! ```
//!
//! The base identity and each agent identity have their own writer lock, `operations.lock`. A
//! mutating command holds it from journal open and sequence allocation through acknowledgement,
//! so the commands of one identity mutate one at a time. The desktop's `clients/desktop` is never
//! read or written here.
use crate::contract::{
    Code, Details, Failure, RecoveredIdentity, StateResetDetails, StateTooOldDetails, Warning,
    from_core_error,
};
use serde::{Deserialize, Serialize};
use spaceterm_connections::migration::StoreCredentials;
use spaceterm_core::hierarchy::{ClientJournal, Id};
use spaceterm_core::migration::{
    self, DataKind, DirectoryStore, FileRule, Mount, Refusal, Registry, ResetReason, StoreError,
};
use spaceterm_core::protocol::{AgentName, RunId};
use spaceterm_core::update::ordering::Floor;
use spaceterm_core::{CoreError, Directory, Lock, LockWait, MachineConnection};
use std::path::PathBuf;
use std::time::Instant;

/// The writer lock's file in each identity directory.
pub const WRITER_LOCK: &str = "operations.lock";
/// The lock held across read-modify-write of `runs.json`.
pub const RUNS_LOCK: &str = "runs.lock";
const RUNS: &std::ffi::CStr = c"runs.json";
/// The most runs one identity keeps pending. The server keeps at most 8 per Pane.
pub const MAX_PENDING_RUNS: usize = 16;

pub struct CliState {
    root: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RouteKey {
    Local,
    Paired([u8; 32]),
}

impl RouteKey {
    /// The route's directory name: `local`, or the route ID as 64 hex.
    pub fn directory_name(&self) -> String {
        match self {
            Self::Local => "local".to_owned(),
            Self::Paired(route) => route.iter().map(|byte| format!("{byte:02x}")).collect(),
        }
    }
}

/// Held by a mutating command from journal open through acknowledgement. Its journal opens
/// under the same lock, so the command never contends with itself.
pub struct WriterGuard {
    lock: Option<Lock>,
    journal: Option<ClientJournal>,
}

impl WriterGuard {
    /// This identity's Workspace edit journal, opened once under the guard's lock. `connection`
    /// must be the handle of the same route and identity.
    pub fn journal(
        &mut self,
        connection: &MachineConnection,
    ) -> Result<&mut ClientJournal, Failure> {
        if let Some(lock) = self.lock.take() {
            self.journal = Some(
                connection
                    .operation_journal_locked(lock)
                    .map_err(from_core_error)?,
            );
        }
        self.journal
            .as_mut()
            .ok_or(Failure::new(Code::StorageUnavailable))
    }
}

/// An identity of one route with unsettled journal entries or runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldIdentity {
    pub identity: RecoveredIdentity,
    pub operations: u32,
    pub runs: u32,
}

impl HeldIdentity {
    pub fn agent(&self) -> Option<&AgentName> {
        match &self.identity {
            RecoveredIdentity::Base => None,
            RecoveredIdentity::Agent(agent) => Some(agent),
        }
    }
}

/// A run dispatched under one identity whose receipt it has not acknowledged yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRun {
    pub run: RunId,
    pub pane: Id,
    pub session: Id,
    /// Seconds since the Unix epoch.
    pub created_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunsRecord {
    runs: Vec<PendingRun>,
}

/// The lock the CLI holds while it brings its state root to this build's generation.
const GENERATION_LOCK: &str = "state.lock";

const RULES: &[FileRule] = &[
    FileRule {
        name: "connections.json",
        kind: DataKind::SavedConnections,
        read: spaceterm_connections::readable,
        stamp: None,
    },
    FileRule {
        name: "runs.json",
        kind: DataKind::ClientRecords,
        read: |value| {
            RunsRecord::deserialize(value).is_ok_and(|record| record.runs.len() <= MAX_PENDING_RUNS)
        },
        stamp: None,
    },
    FileRule {
        name: "last-connected.json",
        kind: DataKind::View,
        read: crate::admin::readable_last_connected,
        stamp: None,
    },
];

/// The CLI state root's generations, steps and records.
pub const REGISTRY: Registry = Registry {
    admission: spaceterm_core::migration::Admission::GenerationOne,
    generations: migration::generations::CLI_STATE,
    steps: &[],
    files: &[RULES, migration::client_files::RULES],
};

/// The CLI state root of `identity`, `clients/cli` and everything under it.
pub fn store(identity: &str) -> Result<DirectoryStore, CoreError> {
    Ok(store_at(spaceterm_core::cli_state_root(identity)?))
}

fn store_at(root: PathBuf) -> DirectoryStore {
    DirectoryStore::new(root.clone(), vec![Mount::tree(root)])
}

/// The credential stores this CLI's Saved Connections use.
fn credentials(identity: &str) -> StoreCredentials {
    let identity = identity.to_owned();
    StoreCredentials::new(move |kind| {
        spaceterm_connections::credentials::for_cli_record(&identity, kind)
    })
}

/// This build's Update Floor: the Stepping Stone release that carries state too old for this
/// build forward. Development and Preflight builds have none.
pub fn build_floor() -> Option<Floor> {
    #[derive(Deserialize)]
    struct Build {
        floor: Option<String>,
    }
    let build: Build =
        serde_json::from_slice(include_bytes!(concat!(env!("OUT_DIR"), "/build-info.json")))
            .ok()?;
    Floor::parse(&build.floor?).ok()
}

/// A command's share of its CLI state root, and the rebuildable data the CLI reset since a
/// command last reported it.
pub struct Prepared {
    pub share: StateShare,
    /// `data_reset`, which the command's result reports once.
    pub notice: Option<Warning>,
}

/// Brings this CLI's state to this build's generation before any command reads it. The command
/// holds the returned shared guard until it ends: every other command shares the root, and only
/// a migration or a reset takes it alone.
pub fn prepare(identity: &str) -> Result<Prepared, Failure> {
    let returned = || {
        let executable = std::env::current_exe().map_err(|_| CoreError::RuntimeUnavailable)?;
        spaceterm_core::service::Service::for_identity(identity)?.returned_to(&executable)
    };
    let root = spaceterm_core::cli_state_root(identity).map_err(from_core_error)?;
    let held = prepare_at(root.clone(), &mut credentials(identity), &returned)?;
    Ok(Prepared {
        share: StateShare {
            root: Some(root),
            lock: Some(held.lock),
            registry: &REGISTRY,
        },
        notice: (!held.reset.is_empty()).then(|| Warning::data_reset(held.reset)),
    })
}

/// A command's share of its CLI state root, which [`prepare`] takes. A command that waits for a
/// SpaceTerm Core to restart lets it go meanwhile, so the Core can migrate the state.
pub struct StateShare {
    /// None for a command that shares no state root.
    root: Option<PathBuf>,
    lock: Option<Lock>,
    /// The generations this build reads.
    registry: &'static Registry,
}

impl Default for StateShare {
    fn default() -> Self {
        Self {
            root: None,
            lock: None,
            registry: &REGISTRY,
        }
    }
}

impl spaceterm_core::service::lane::admin::Share for StateShare {
    fn release(&mut self) {
        self.lock = None;
    }
    fn resume(&mut self) -> bool {
        let Some(root) = &self.root else {
            return true;
        };
        if self.lock.is_some() {
            return true;
        }
        let Ok(directory) = Directory::open(root, false) else {
            return false;
        };
        let Ok(lock) = directory.lock_shared(GENERATION_LOCK) else {
            return false;
        };
        if !migration::is_current(self.registry, &store_at(root.clone())).unwrap_or(false) {
            return false;
        }
        self.lock = Some(lock);
        true
    }
}

/// Brings this CLI's state to this build's generation for SpaceTerm Core before it reports
/// readiness, so no update starts while the state waits for this build's steps. The background
/// updater may restart through a Stepping Stone with no CLI command in between.
pub fn prepare_for_core(identity: &str) -> Result<(), CoreError> {
    let returned = || {
        let executable = std::env::current_exe().map_err(|_| CoreError::RuntimeUnavailable)?;
        spaceterm_core::service::Service::for_identity(identity)?.returned_to(&executable)
    };
    prepare_for_core_at(
        spaceterm_core::cli_state_root(identity)?,
        &REGISTRY,
        &mut credentials(identity),
        &returned,
        CORE_LOCK_LIMIT,
    )
}

/// How long SpaceTerm Core waits for the commands that share the CLI's state before it
/// migrates. A command may be waiting for this Core to start while it shares the state.
const CORE_LOCK_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

fn prepare_for_core_at(
    root: PathBuf,
    registry: &Registry,
    credentials: &mut dyn migration::Credentials,
    returned: &dyn Fn() -> Result<bool, CoreError>,
    limit: std::time::Duration,
) -> Result<(), CoreError> {
    let directory = Directory::open(&root, true)?;
    let store = store_at(root);
    // A CLI that keeps the state, alone or shared, past the limit refuses this start, and the
    // Core stays unready.
    let deadline = Instant::now() + limit;
    let left = || deadline.saturating_duration_since(Instant::now());
    {
        let LockWait::Locked(_shared) = directory.wait_lock_shared(GENERATION_LOCK, left())? else {
            return Err(CoreError::ServiceBusy);
        };
        if migration::is_current(registry, &store).map_err(storage)? {
            return Ok(());
        }
    }
    let LockWait::Locked(_alone) = directory.wait_lock(GENERATION_LOCK, left(), None)? else {
        return Err(CoreError::ServiceBusy);
    };
    let returned = returned().unwrap_or(false);
    migration::run(registry, &store, credentials, returned).map_err(refusal)?;
    Ok(())
}

/// How many times [`prepare_at`] migrates before it gives up on a root that other CLIs keep
/// changing.
const ATTEMPTS: usize = 4;

/// A shared hold on a current CLI state root.
struct Held {
    lock: Lock,
    /// The rebuildable data a reset replaced since a command last took the notice.
    reset: Vec<DataKind>,
}

fn prepare_at(
    root: PathBuf,
    credentials: &mut dyn migration::Credentials,
    returned: &dyn Fn() -> Result<bool, CoreError>,
) -> Result<Held, Failure> {
    prepare_in(root, &REGISTRY, build_floor(), credentials, returned)
}

/// [`prepare_at`] for the generations of `registry`, in a build whose Update Floor is `floor`.
fn prepare_in(
    root: PathBuf,
    registry: &Registry,
    floor: Option<Floor>,
    credentials: &mut dyn migration::Credentials,
    returned: &dyn Fn() -> Result<bool, CoreError>,
) -> Result<Held, Failure> {
    let directory = Directory::open(&root, true).map_err(from_core_error)?;
    let store = store_at(root);
    for _ in 0..ATTEMPTS {
        let shared = directory
            .lock_shared(GENERATION_LOCK)
            .map_err(from_core_error)?;
        if migration::is_current(registry, &store)
            .map_err(|error| state_failure(error.into(), floor))?
        {
            // A notice that can't be taken stays for the next command.
            let reset = migration::take_notice(&store).unwrap_or_default();
            return Ok(Held {
                lock: shared,
                reset,
            });
        }
        drop(shared);
        let _alone = directory
            .lock(GENERATION_LOCK, false)
            .map_err(from_core_error)?;
        // An activation that returned to this CLI may leave the state its candidate migrated.
        let returned = returned().unwrap_or(false);
        migration::run(registry, &store, credentials, returned)
            .map_err(|refused| state_failure(refused, floor))?;
    }
    Err(Failure::new(Code::StorageUnavailable))
}

/// Why a command can't read this CLI's state, in a build whose Update Floor is `floor`. State
/// too old for this build names the Stepping Stone to install first, when the build has one.
fn state_failure(refused: Refusal, floor: Option<Floor>) -> Failure {
    match (refused, floor) {
        (
            Refusal::Reset {
                reason: ResetReason::TooOld,
                kinds,
            },
            Some(floor),
        ) => Failure::with(
            Code::StateTooOld,
            Details::StateTooOld(StateTooOldDetails { floor, kinds }),
        ),
        (Refusal::Reset { reason, kinds }, _) => Failure::with(
            Code::StateResetRequired,
            Details::StateReset(StateResetDetails { reason, kinds }),
        ),
        (refused, _) => from_core_error(refusal(refused)),
    }
}

/// What `spaceterm reset` found or did to this CLI's state.
#[derive(Debug, PartialEq, Eq)]
pub enum Reset {
    /// The state needs no reset.
    NotNeeded,
    /// The reset waits for the token that confirms it.
    Preview {
        reason: ResetReason,
        /// The kinds of data the reset replaces.
        kinds: Vec<DataKind>,
        token: crate::admin::offline_token::Issued,
    },
    /// The files of `kinds` moved aside, and their data starts over.
    Done { kinds: Vec<DataKind> },
}

/// What `spaceterm reset` reports: its result, and the rebuildable data reset since a command
/// last reported it.
pub struct ResetReport {
    pub reset: Reset,
    pub notice: Option<Warning>,
}

const RESET_PURPOSE: &str = "reset";

/// Previews this CLI's state reset, or with the preview's `token`, performs it. The token binds
/// the reason and the kinds of data, so a changed reset needs a new preview. Only the CLI state
/// root changes. SpaceTerm Core's Machine ID and pairings live in its own data root.
pub fn reset(identity: &str, token: Option<&str>) -> Result<ResetReport, Failure> {
    let returned = || {
        let executable = std::env::current_exe().map_err(|_| CoreError::RuntimeUnavailable)?;
        spaceterm_core::service::Service::for_identity(identity)?.returned_to(&executable)
    };
    let root = spaceterm_core::cli_state_root(identity).map_err(from_core_error)?;
    reset_in(
        root,
        &REGISTRY,
        &mut credentials(identity),
        &returned,
        &Interruption {
            interrupted: &crate::interrupt::interrupted,
            dispatch: &crate::interrupt::dispatch,
        },
        token,
        std::time::SystemTime::now(),
    )
}

/// How [`reset_in`] meets `SIGINT`: it stops waiting for the state root once `interrupted`
/// reports it, and marks its first effect with `dispatch` only after it holds the root alone.
struct Interruption<'a> {
    interrupted: &'a dyn Fn() -> bool,
    dispatch: &'a dyn Fn() -> Result<(), Failure>,
}

fn reset_in(
    root: PathBuf,
    registry: &Registry,
    credentials: &mut dyn migration::Credentials,
    returned: &dyn Fn() -> Result<bool, CoreError>,
    interruption: &Interruption<'_>,
    token: Option<&str>,
    now: std::time::SystemTime,
) -> Result<ResetReport, Failure> {
    let directory = Directory::open(&root, true).map_err(from_core_error)?;
    let store = store_at(root);
    // Another command may share the root for as long as it runs, so the wait stops on SIGINT.
    let _alone = loop {
        match directory
            .wait_lock(
                GENERATION_LOCK,
                crate::interrupt::TICK,
                Some(interruption.interrupted),
            )
            .map_err(from_core_error)?
        {
            LockWait::Locked(lock) => break lock,
            LockWait::Held => {}
            LockWait::Cancelled => return Err(Failure::new(Code::Interrupted)),
        }
    };
    (interruption.dispatch)()?;
    let returned = returned().unwrap_or(false);
    let refused = migration::run(registry, &store, credentials, returned);
    let notice = || {
        let reset = migration::take_notice(&store).unwrap_or_default();
        (!reset.is_empty()).then(|| Warning::data_reset(reset))
    };
    let (reason, kinds) = match refused {
        Ok(_) => {
            return Ok(ResetReport {
                reset: Reset::NotNeeded,
                notice: notice(),
            });
        }
        Err(Refusal::Reset { reason, kinds }) => (reason, kinds),
        Err(refused) => return Err(state_failure(refused, None)),
    };
    let observation = serde_json::to_vec(&(reason, &kinds))
        .map_err(|_| Failure::new(Code::StorageUnavailable))?;
    let reset = match token {
        None => Reset::Preview {
            reason,
            kinds,
            token: crate::admin::offline_token::issue(RESET_PURPOSE, &observation, now),
        },
        Some(token) => {
            crate::admin::offline_token::verify(RESET_PURPOSE, &observation, token, now)?;
            let kinds = migration::reset_confirmed(registry, &store, credentials)
                .map_err(|refused| state_failure(refused, None))?;
            Reset::Done { kinds }
        }
    };
    Ok(ResetReport {
        reset,
        notice: notice(),
    })
}

fn refusal(refusal: Refusal) -> CoreError {
    match refusal {
        Refusal::Newer => CoreError::IncompatibleState,
        Refusal::Reset { .. } => CoreError::DataResetRequired,
        Refusal::Storage(error) => storage(error),
    }
}

fn storage(error: StoreError) -> CoreError {
    match error {
        StoreError::Unavailable => CoreError::StorageUnavailable,
        StoreError::Unsafe => CoreError::UnsafeStorage,
    }
}

/// One identity's run journal. Each transaction locks and reloads `runs.json`.
/// A live command holds only its own run's lock while waiting for its receipt.
pub struct Runs {
    directory: Directory,
    live: Directory,
}

impl Runs {
    fn lock(&self) -> Result<Lock, Failure> {
        match self
            .directory
            .wait_lock(RUNS_LOCK, std::time::Duration::from_secs(30), None)
            .map_err(from_core_error)?
        {
            LockWait::Locked(lock) => Ok(lock),
            LockWait::Held | LockWait::Cancelled => Err(Failure::new(Code::IdentityBusy)),
        }
    }

    fn read(&self) -> Result<RunsRecord, Failure> {
        let record = self
            .directory
            .read_json::<RunsRecord>("runs.json")
            .map_err(from_core_error)?
            .unwrap_or_default();
        if record.runs.len() > MAX_PENDING_RUNS {
            return Err(Failure::new(Code::StorageUnavailable));
        }
        Ok(record)
    }

    pub fn pending(&self) -> Result<Vec<PendingRun>, Failure> {
        let _lock = self.lock()?;
        Ok(self.read()?.runs)
    }

    fn run_lock_name(run: RunId) -> std::ffi::CString {
        std::ffi::CString::new(format!("{}.lock", crate::io::hex(&run.0)))
            .expect("a hex run ID has no NUL")
    }

    /// Claims a pending run for recovery. A live command or another settler owns it otherwise.
    pub fn try_run(&self, run: RunId) -> Result<Option<Lock>, Failure> {
        let Some(live) = self
            .live
            .try_lock(&format!("{}.lock", crate::io::hex(&run.0)))
            .map_err(from_core_error)?
        else {
            return Ok(None);
        };
        let _lock = self.lock()?;
        if !self.read()?.runs.iter().any(|pending| pending.run == run) {
            self.live
                .remove(&Self::run_lock_name(run))
                .map_err(from_core_error)?;
            return Ok(None);
        }
        Ok(Some(live))
    }

    /// Records `run` before dispatch and holds its live lock until the command exits.
    pub fn add(&mut self, run: PendingRun) -> Result<Lock, Failure> {
        let _lock = self.lock()?;
        let mut record = self.read()?;
        let recorded = record.runs.iter().any(|pending| pending.run == run.run);
        if !recorded && record.runs.len() >= MAX_PENDING_RUNS {
            return Err(Failure::new(Code::Capacity));
        }
        let live = self
            .live
            .try_lock(&format!("{}.lock", crate::io::hex(&run.run.0)))
            .map_err(from_core_error)?
            .ok_or(Failure::new(Code::IdentityBusy))?;
        if !recorded {
            record.runs.push(run);
            self.directory
                .write_json(RUNS, &record)
                .map_err(from_core_error)?;
        }
        Ok(live)
    }

    /// Forgets `run` after the server acknowledged its receipt, or never admitted it.
    /// The caller holds the run's live lock until this returns, including while it is unlinked.
    pub fn remove(&mut self, run: RunId) -> Result<(), Failure> {
        let _lock = self.lock()?;
        let mut record = self.read()?;
        let before = record.runs.len();
        record.runs.retain(|pending| pending.run != run);
        if record.runs.len() != before {
            self.directory
                .write_json(RUNS, &record)
                .map_err(from_core_error)?;
        }
        self.live
            .remove(&Self::run_lock_name(run))
            .map_err(from_core_error)?;
        Ok(())
    }
}

impl CliState {
    pub fn open(identity: &str) -> Result<Self, CoreError> {
        Ok(Self {
            root: spaceterm_core::cli_state_root(identity)?,
        })
    }

    fn route_path(&self, route: &RouteKey) -> PathBuf {
        self.root.join("routes").join(route.directory_name())
    }

    fn identity_path(&self, route: &RouteKey, agent: Option<&AgentName>) -> PathBuf {
        let route = self.route_path(route);
        match agent {
            None => route.join("base"),
            Some(agent) => route.join("agents").join(agent.as_str()),
        }
    }

    /// The identity's private directory, created on first use.
    pub fn identity_dir(
        &self,
        route: &RouteKey,
        agent: Option<&AgentName>,
    ) -> Result<Directory, CoreError> {
        Directory::open(&self.identity_path(route, agent), true)
    }

    /// The identity's writer lock. Contention waits until `deadline`, then fails with
    /// `identity_busy`.
    pub fn writer(
        &self,
        route: &RouteKey,
        agent: Option<&AgentName>,
        deadline: Instant,
    ) -> Result<WriterGuard, Code> {
        let directory = self
            .identity_dir(route, agent)
            .map_err(|_| Code::StorageUnavailable)?;
        let limit = deadline.saturating_duration_since(Instant::now());
        match directory.wait_lock(WRITER_LOCK, limit, None) {
            Ok(LockWait::Locked(lock)) => Ok(WriterGuard {
                lock: Some(lock),
                journal: None,
            }),
            Ok(LockWait::Held | LockWait::Cancelled) => Err(Code::IdentityBusy),
            Err(_) => Err(Code::StorageUnavailable),
        }
    }

    /// The identity's writer lock if no other process holds it now.
    pub fn try_writer(
        &self,
        route: &RouteKey,
        agent: Option<&AgentName>,
    ) -> Result<Option<WriterGuard>, Code> {
        let directory = self
            .identity_dir(route, agent)
            .map_err(|_| Code::StorageUnavailable)?;
        match directory.try_lock(WRITER_LOCK) {
            Ok(lock) => Ok(lock.map(|lock| WriterGuard {
                lock: Some(lock),
                journal: None,
            })),
            Err(_) => Err(Code::StorageUnavailable),
        }
    }

    /// Opens a run journal without retaining its transaction lock.
    pub fn runs(
        &self,
        route: &RouteKey,
        agent: Option<&AgentName>,
        deadline: Instant,
    ) -> Result<Runs, Code> {
        let directory = self
            .identity_dir(route, agent)
            .map_err(|_| Code::StorageUnavailable)?;
        let live = Directory::open(&self.identity_path(route, agent).join("runs"), true)
            .map_err(|_| Code::StorageUnavailable)?;
        let limit = deadline.saturating_duration_since(Instant::now());
        let _lock = match directory.wait_lock(RUNS_LOCK, limit, None) {
            Ok(LockWait::Locked(lock)) => lock,
            Ok(LockWait::Held | LockWait::Cancelled) => return Err(Code::IdentityBusy),
            Err(_) => return Err(Code::StorageUnavailable),
        };
        let runs = Runs { directory, live };
        runs.read().map_err(|failure| failure.code)?;
        Ok(runs)
    }

    /// The base identity and every agent this CLI has used on `route`, without creating any.
    pub fn identities(&self, route: &RouteKey) -> Result<Vec<Option<AgentName>>, CoreError> {
        let mut identities = vec![None];
        let agents = self.route_path(route).join("agents");
        let entries = match std::fs::read_dir(&agents) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(identities),
            Err(_) => return Err(CoreError::StorageUnavailable),
        };
        let mut names: Vec<AgentName> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok()?.parse().ok())
            .collect();
        names.sort();
        identities.extend(names.into_iter().map(Some));
        Ok(identities)
    }

    /// Every identity of `route` that holds unsettled outcomes, read without their locks.
    pub fn held(&self, route: &RouteKey) -> Result<Vec<HeldIdentity>, CoreError> {
        let mut held = Vec::new();
        for agent in self.identities(route)? {
            let path = self.identity_path(route, agent.as_ref());
            if std::fs::symlink_metadata(&path).is_err() {
                continue;
            }
            let directory = Directory::open(&path, false)?;
            let operations = ClientJournal::held(&directory)?;
            let runs = directory
                .read_json::<RunsRecord>("runs.json")?
                .map_or(0, |record| record.runs.len().min(u32::MAX as usize) as u32);
            if operations > 0 || runs > 0 {
                held.push(HeldIdentity {
                    identity: agent.map_or(RecoveredIdentity::Base, RecoveredIdentity::Agent),
                    operations,
                    runs,
                });
            }
        }
        Ok(held)
    }
}

#[cfg(test)]
impl CliState {
    /// A state rooted at `root`, for tests that keep their files apart.
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary(name: &str) -> CliState {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("st-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        CliState::at(root)
    }

    #[test]
    fn the_state_root_reaches_this_generation_or_refuses_unchanged() {
        let state = temporary("generation");
        let mut none = migration::NoCredentials;
        let never = || Ok(false);
        drop(prepare_at(state.root.clone(), &mut none, &never).unwrap());
        let marker = state.root.join("generation.json");
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            br#"{"generation":1}"#,
            "an empty root starts at the current generation"
        );
        let runs = state.root.join("routes/local/base");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(runs.join("runs.json"), br#"{"runs":[]}"#).unwrap();
        std::fs::write(&marker, br#"{"generation":2}"#).unwrap();
        assert_eq!(
            prepare_at(state.root.clone(), &mut none, &never).err(),
            Some(from_core_error(CoreError::IncompatibleState))
        );
        assert_eq!(std::fs::read(&marker).unwrap(), br#"{"generation":2}"#);
        assert_eq!(
            std::fs::read(runs.join("runs.json")).unwrap(),
            br#"{"runs":[]}"#
        );
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn a_command_shares_the_state_root_so_no_migration_runs_beneath_it() {
        let state = temporary("migration-shared");
        let guard = prepare_at(state.root.clone(), &mut migration::NoCredentials, &|| {
            Ok(false)
        })
        .unwrap();
        let directory = Directory::open(&state.root, false).unwrap();
        assert!(directory.try_lock(GENERATION_LOCK).unwrap().is_none());
        let other = prepare_at(state.root.clone(), &mut migration::NoCredentials, &|| {
            Ok(false)
        })
        .unwrap();
        drop((guard, other));
        assert!(directory.try_lock(GENERATION_LOCK).unwrap().is_some());
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn an_activation_return_brings_back_the_state_its_candidate_migrated() {
        use migration::{Area, Store};
        let state = temporary("migration-return");
        drop(
            prepare_at(state.root.clone(), &mut migration::NoCredentials, &|| {
                Ok(false)
            })
            .unwrap(),
        );
        let store = store_at(state.root.clone());
        store
            .write(Area::Backup, "generation.json", br#"{"generation":1}"#)
            .unwrap();
        store
            .write(Area::Root, "generation.json", br#"{"generation":2}"#)
            .unwrap();
        assert_eq!(
            prepare_at(state.root.clone(), &mut migration::NoCredentials, &|| Ok(
                false
            ))
            .err(),
            Some(from_core_error(CoreError::IncompatibleState))
        );
        drop(
            prepare_at(state.root.clone(), &mut migration::NoCredentials, &|| {
                Ok(true)
            })
            .unwrap(),
        );
        assert_eq!(
            store.read(Area::Root, "generation.json").unwrap(),
            Some(br#"{"generation":1}"#.to_vec())
        );
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    /// Release 1.1.0 carries the CLI's state from generation 1 to 2. Release 1.2.0 retires
    /// generation 1, so its floor is 1.1.0.
    const STEPPING_STONE: Registry = Registry {
        admission: spaceterm_core::migration::Admission::GenerationOne,
        generations: migration::Generations {
            oldest: 1,
            current: 2,
        },
        steps: &[&[]],
        files: REGISTRY.files,
    };
    const AFTER_STONE: Registry = Registry {
        admission: spaceterm_core::migration::Admission::GenerationOne,
        generations: migration::Generations {
            oldest: 2,
            current: 3,
        },
        steps: &[&[]],
        files: REGISTRY.files,
    };

    #[test]
    fn a_background_update_through_a_stepping_stone_runs_every_cli_state_migration() {
        use migration::{Area, Store};
        let state = temporary("migration-two-hop");
        let never = || Ok(false);
        drop(prepare_at(state.root.clone(), &mut migration::NoCredentials, &never).unwrap());
        let runs = state.root.join("routes/local/base/runs.json");
        std::fs::create_dir_all(runs.parent().unwrap()).unwrap();
        std::fs::write(&runs, br#"{"runs":[]}"#).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let private = |path: &std::path::Path, mode| {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
            };
            for directory in ["routes", "routes/local", "routes/local/base"] {
                private(&state.root.join(directory), 0o700);
            }
            private(&runs, 0o600);
        }
        // The updater restarts SpaceTerm Core into 1.1.0, then into 1.2.0. No CLI command runs.
        for release in [&STEPPING_STONE, &AFTER_STONE] {
            prepare_for_core_at(
                state.root.clone(),
                release,
                &mut migration::NoCredentials,
                &never,
                CORE_LOCK_LIMIT,
            )
            .unwrap();
        }
        // The first CLI command of 1.2.0 finds its state current.
        let store = store_at(state.root.clone());
        assert!(migration::is_current(&AFTER_STONE, &store).unwrap());
        assert_eq!(
            store.read(Area::Root, "generation.json").unwrap(),
            Some(br#"{"generation":3}"#.to_vec())
        );
        assert_eq!(std::fs::read(&runs).unwrap(), br#"{"runs":[]}"#);
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn a_cli_state_migration_at_core_start_waits_a_bounded_time_for_sharing_commands() {
        use migration::{Area, Store};
        let state = temporary("migration-core-busy");
        let never = || Ok(false);
        let command =
            prepare_at(state.root.clone(), &mut migration::NoCredentials, &never).unwrap();
        // A current root needs no migration, so a sharing command never delays the start.
        prepare_for_core_at(
            state.root.clone(),
            &REGISTRY,
            &mut migration::NoCredentials,
            &never,
            std::time::Duration::from_secs(3600),
        )
        .unwrap();
        let limit = std::time::Duration::from_millis(200);
        let started = Instant::now();
        assert_eq!(
            prepare_for_core_at(
                state.root.clone(),
                &STEPPING_STONE,
                &mut migration::NoCredentials,
                &never,
                limit,
            ),
            Err(CoreError::ServiceBusy)
        );
        assert!(started.elapsed() >= limit);
        let store = store_at(state.root.clone());
        assert_eq!(
            store.read(Area::Root, "generation.json").unwrap(),
            Some(br#"{"generation":1}"#.to_vec())
        );
        drop(command);
        prepare_for_core_at(
            state.root.clone(),
            &STEPPING_STONE,
            &mut migration::NoCredentials,
            &never,
            limit,
        )
        .unwrap();
        assert!(migration::is_current(&STEPPING_STONE, &store).unwrap());
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn a_command_waiting_for_a_restart_lets_a_cli_state_migration_run() {
        use spaceterm_core::service::lane::admin::Share;
        let state = temporary("migration-released");
        let never = || Ok(false);
        let mut share = StateShare {
            registry: &REGISTRY,
            root: Some(state.root.clone()),
            lock: Some(
                prepare_at(state.root.clone(), &mut migration::NoCredentials, &never)
                    .unwrap()
                    .lock,
            ),
        };
        // Nothing migrated while the command waited, so it shares the state again.
        share.release();
        assert!(share.resume());
        share.release();
        // The Pending Version the restart started migrates the state before it is ready.
        prepare_for_core_at(
            state.root.clone(),
            &STEPPING_STONE,
            &mut migration::NoCredentials,
            &never,
            std::time::Duration::from_millis(200),
        )
        .unwrap();
        // This build no longer reads the state, so the command leaves it alone.
        assert!(!share.resume());
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn a_first_start_after_a_stopped_update_lets_the_previous_core_undo_its_cli_state_migration() {
        use migration::{Area, Store};
        let state = temporary("migration-start-return");
        let never = || Ok(false);
        drop(prepare_at(state.root.clone(), &mut migration::NoCredentials, &never).unwrap());
        // The stopped update installed a candidate whose CLI migrates the state to generation
        // 2. Its `spaceterm start` then shares that state.
        prepare_for_core_at(
            state.root.clone(),
            &STEPPING_STONE,
            &mut migration::NoCredentials,
            &never,
            CORE_LOCK_LIMIT,
        )
        .unwrap();
        let directory = Directory::open(&state.root, false).unwrap();
        let global = crate::args::Global {
            machine: None,
            agent: None,
            json: true,
            quiet: false,
            verbose: false,
            no_input: true,
            timeout: None,
        };
        let context = crate::admin::Context {
            identity: "spaceterm-development",
            global: &global,
            credentials: crate::connection::CliCredentials::new("spaceterm-development"),
            facts: Default::default(),
            state: std::cell::RefCell::new(StateShare {
                root: Some(state.root.clone()),
                lock: Some(directory.lock_shared(GENERATION_LOCK).unwrap()),
                registry: &STEPPING_STONE,
            }),
        };
        // The candidate fails before readiness, and Start waits while the activation returns to
        // the previous Core, which restores generation 1 before it is ready.
        let returned = context.unshared(|| {
            prepare_for_core_at(
                state.root.clone(),
                &REGISTRY,
                &mut migration::NoCredentials,
                &|| Ok(true),
                std::time::Duration::from_millis(200),
            )
        });
        assert_eq!(returned, Ok(()));
        let store = store_at(state.root.clone());
        assert_eq!(
            store.read(Area::Root, "generation.json").unwrap(),
            Some(br#"{"generation":1}"#.to_vec())
        );
        // The candidate's CLI no longer reads the state.
        assert!(context.require_state().is_err());
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn setup_opens_no_cli_connection_after_a_return_undid_its_cli_state_migration() {
        let state = temporary("migration-setup-return");
        let never = || Ok(false);
        drop(prepare_at(state.root.clone(), &mut migration::NoCredentials, &never).unwrap());
        // The candidate's CLI migrated the state to generation 2 and shares it.
        prepare_for_core_at(
            state.root.clone(),
            &STEPPING_STONE,
            &mut migration::NoCredentials,
            &never,
            CORE_LOCK_LIMIT,
        )
        .unwrap();
        let directory = Directory::open(&state.root, false).unwrap();
        let global = crate::args::Global {
            machine: None,
            agent: None,
            json: true,
            quiet: false,
            verbose: false,
            no_input: true,
            timeout: None,
        };
        let context = crate::admin::Context {
            identity: "spaceterm-development",
            global: &global,
            credentials: crate::connection::CliCredentials::new("spaceterm-development"),
            facts: Default::default(),
            state: std::cell::RefCell::new(StateShare {
                root: Some(state.root.clone()),
                lock: Some(directory.lock_shared(GENERATION_LOCK).unwrap()),
                registry: &STEPPING_STONE,
            }),
        };
        let client = state.root.join("routes/local/base/client.json");
        // A CLI connection reads or creates the route's client identity.
        let connect = |()| {
            std::fs::create_dir_all(client.parent().unwrap()).unwrap();
            std::fs::write(&client, b"{}").unwrap();
            Ok(())
        };
        // Nothing changed while the service calls ran, so setup connects.
        assert_eq!(context.after_service(|| Ok(()), connect), Ok(()));
        std::fs::remove_file(&client).unwrap();
        // This time the service call settled an activation that returned to the previous Core,
        // which restored generation 1.
        let restore = || {
            prepare_for_core_at(
                state.root.clone(),
                &REGISTRY,
                &mut migration::NoCredentials,
                &|| Ok(true),
                std::time::Duration::from_millis(200),
            )
            .map_err(crate::contract::from_core_error)
        };
        let refused = context.after_service(restore, connect).unwrap_err();
        assert_eq!(
            refused,
            crate::contract::from_core_error(CoreError::IncompatibleState)
        );
        assert!(!client.exists(), "setup touched the CLI state");
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn a_cli_state_migration_at_core_start_waits_a_bounded_time_for_a_stalled_migration() {
        use migration::{Area, Store};
        let state = temporary("migration-core-stalled");
        let never = || Ok(false);
        drop(prepare_at(state.root.clone(), &mut migration::NoCredentials, &never).unwrap());
        let directory = Directory::open(&state.root, false).unwrap();
        // Another CLI stalls while it migrates or restores the state.
        let stalled = directory.lock(GENERATION_LOCK, false).unwrap();
        let limit = std::time::Duration::from_millis(200);
        let (sent, received) = std::sync::mpsc::channel();
        let root = state.root.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = prepare_for_core_at(
                root,
                &STEPPING_STONE,
                &mut migration::NoCredentials,
                &|| Ok(false),
                limit,
            );
            let _ = sent.send((result, started.elapsed()));
        });
        let (result, waited) = received
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the Core start waited past its limit");
        assert_eq!(result, Err(CoreError::ServiceBusy));
        assert!(waited >= limit);
        assert_eq!(
            store_at(state.root.clone())
                .read(Area::Root, "generation.json")
                .unwrap(),
            Some(br#"{"generation":1}"#.to_vec())
        );
        drop(stalled);
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    /// Writes `bytes` at `path` under `root` with the private modes the state root requires.
    fn private_file(root: &std::path::Path, path: &str, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        let file = root.join(path);
        let mut directory = file.parent().unwrap();
        std::fs::create_dir_all(directory).unwrap();
        while directory.starts_with(root) {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            directory = directory.parent().unwrap();
        }
        std::fs::write(&file, bytes).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    /// A CLI state root at generation 1 with a Saved Connections file, which resets only after
    /// the person confirms, and a last-connected record, which is rebuildable view state.
    fn generation_one(name: &str) -> CliState {
        let state = temporary(name);
        private_file(&state.root, "generation.json", br#"{"generation":1}"#);
        private_file(&state.root, "connections.json", b"{}");
        private_file(
            &state.root,
            "routes/local/base/last-connected.json",
            br#"{"at":1}"#,
        );
        state
    }

    #[test]
    fn state_too_old_for_this_build_names_the_stepping_stone_to_install() {
        let state = generation_one("too-old");
        let floor = Floor::parse("1.1.0").unwrap();
        let refused = prepare_in(
            state.root.clone(),
            &AFTER_STONE,
            Some(floor),
            &mut migration::NoCredentials,
            &|| Ok(false),
        )
        .err()
        .unwrap();
        assert_eq!(refused.code, Code::StateTooOld);
        assert_eq!(
            serde_json::to_value(&refused.details).unwrap(),
            serde_json::json!({"floor": "1.1.0", "kinds": ["saved_connections"]})
        );
        // A build without an Update Floor has no Stepping Stone to name, so only a reset helps.
        let refused = prepare_in(
            state.root.clone(),
            &AFTER_STONE,
            None,
            &mut migration::NoCredentials,
            &|| Ok(false),
        )
        .err()
        .unwrap();
        assert_eq!(refused.code, Code::StateResetRequired);
        assert_eq!(
            serde_json::to_value(&refused.details).unwrap(),
            serde_json::json!({"reason": "too_old", "kinds": ["saved_connections"]})
        );
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn an_automatic_reset_is_reported_once_by_the_next_command() {
        let state = temporary("reset-notice");
        private_file(&state.root, "generation.json", br#"{"generation":1}"#);
        private_file(
            &state.root,
            "routes/local/base/last-connected.json",
            br#"{"at":1}"#,
        );
        let prepare = || {
            prepare_in(
                state.root.clone(),
                &AFTER_STONE,
                None,
                &mut migration::NoCredentials,
                &|| Ok(false),
            )
            .unwrap()
            .reset
        };
        assert_eq!(prepare(), [DataKind::View]);
        assert_eq!(prepare(), []);
        assert!(
            state
                .root
                .join("reset/gen-1/routes/local/base/last-connected.json")
                .exists(),
            "the reset file was kept aside"
        );
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn reset_previews_the_kinds_of_data_and_moves_them_aside_once_confirmed() {
        let state = generation_one("reset-confirmed");
        let now = std::time::SystemTime::now();
        let reset = |token: Option<&str>| {
            reset_in(
                state.root.clone(),
                &AFTER_STONE,
                &mut migration::NoCredentials,
                &|| Ok(false),
                &UNINTERRUPTED,
                token,
                now,
            )
        };
        let preview = reset(None).unwrap();
        let Reset::Preview {
            reason,
            kinds,
            token,
        } = preview.reset
        else {
            panic!("a reset waits for its confirmation");
        };
        assert_eq!(reason, ResetReason::TooOld);
        assert_eq!(kinds, [DataKind::SavedConnections]);
        assert_eq!(
            preview.notice, None,
            "the rebuildable view state waits for the confirmation too"
        );
        assert!(state.root.join("connections.json").exists());
        assert!(
            state
                .root
                .join("routes/local/base/last-connected.json")
                .exists()
        );
        let done = reset(Some(&token.token)).unwrap();
        assert_eq!(
            done.reset,
            Reset::Done {
                kinds: vec![DataKind::View, DataKind::SavedConnections]
            }
        );
        assert!(!state.root.join("connections.json").exists());
        assert_eq!(
            std::fs::read(state.root.join("reset/gen-1/connections.json")).unwrap(),
            b"{}"
        );
        assert_eq!(reset(None).unwrap().reset, Reset::NotNeeded);
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    const UNINTERRUPTED: Interruption<'static> = Interruption {
        interrupted: &|| false,
        dispatch: &|| Ok(()),
    };

    #[test]
    fn an_interrupt_stops_a_reset_that_waits_for_another_command() {
        let state = generation_one("reset-interrupted");
        let directory = Directory::open(&state.root, false).unwrap();
        let shared = directory.lock_shared(GENERATION_LOCK).unwrap();
        // The other command ends after two seconds, well past the interrupt.
        let other = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            drop(shared);
        });
        let dispatched = std::cell::Cell::new(false);
        let started = Instant::now();
        let refused = reset_in(
            state.root.clone(),
            &AFTER_STONE,
            &mut migration::NoCredentials,
            &|| Ok(false),
            &Interruption {
                interrupted: &|| started.elapsed() >= std::time::Duration::from_millis(200),
                dispatch: &|| {
                    dispatched.set(true);
                    Ok(())
                },
            },
            None,
            std::time::SystemTime::now(),
        );
        assert_eq!(
            refused.err().map(|failure| failure.code),
            Some(Code::Interrupted)
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(!dispatched.get(), "an interrupted wait marks no effect");
        assert!(
            state
                .root
                .join("routes/local/base/last-connected.json")
                .exists(),
            "an interrupted reset changes nothing"
        );
        other.join().unwrap();
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn a_reset_token_confirms_only_the_reset_it_previewed() {
        let state = generation_one("reset-changed");
        let now = std::time::SystemTime::now();
        let reset = |token: Option<&str>| {
            reset_in(
                state.root.clone(),
                &AFTER_STONE,
                &mut migration::NoCredentials,
                &|| Ok(false),
                &UNINTERRUPTED,
                token,
                now,
            )
        };
        let Reset::Preview { token, .. } = reset(None).unwrap().reset else {
            panic!("a reset waits for its confirmation");
        };
        // Another client's records appear, so the reset would replace more than the preview named.
        private_file(
            &state.root,
            "routes/local/base/runs.json",
            br#"{"runs":[]}"#,
        );
        assert_eq!(
            reset(Some(&token.token)).err().map(|failure| failure.code),
            Some(Code::Changed)
        );
        assert!(state.root.join("connections.json").exists());
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn every_cli_state_migration_fixture_migrates_and_reads_strictly() {
        use spaceterm_core::migration::fixtures;
        fixtures::check(&REGISTRY, &fixtures::directory("cli-state"), &[], |root| {
            Box::new(store_at(root.to_path_buf()))
        });
    }

    /// `mise run migration:fixture cli-state` writes this build's generation's fixture.
    #[test]
    #[ignore = "writes the fixture of the current generation"]
    fn capture_cli_state_migration_fixture() {
        use spaceterm_core::migration::fixtures;
        let root = fixtures::scratch("cli-state");
        let mut none = migration::NoCredentials;
        drop(prepare_at(root.clone(), &mut none, &|| Ok(false)).unwrap());
        let directory = |path: &str| Directory::open(&root.join(path), true).unwrap();
        directory("")
            .write_json(
                c"connections.json",
                &serde_json::json!({
                    "connections": [],
                    "pending": [],
                    "enrollments": [],
                    "confirmations": [],
                }),
            )
            .unwrap();
        let state = CliState::at(root.clone());
        let mut runs = state.runs(&RouteKey::Local, None, Instant::now()).unwrap();
        drop(
            runs.add(PendingRun {
                run: RunId([1; 16]),
                pane: [2; 32],
                session: [3; 32],
                created_at: 1,
            })
            .unwrap(),
        );
        let base = state.identity_path(&RouteKey::Local, None);
        fixtures::write_client_records(&base);
        directory("routes/local/base")
            .write_json(c"last-connected.json", &serde_json::json!({ "at": 1 }))
            .unwrap();
        let paired = state.identity_path(&RouteKey::Paired([9; 32]), None);
        Directory::open(&paired, true)
            .unwrap()
            .write_json(
                c"route.json",
                &serde_json::json!({
                    "version": 1,
                    "paired": spaceterm_core::direct::PairedDevice {
                        pin: spaceterm_core::direct::MachinePin {
                            build: "spaceterm-development".into(),
                            core: [1; 32],
                            public_key: [2; 32],
                            certificate: [3; 32],
                        },
                        pairing: [4; 32],
                        client: [5; 32],
                    },
                }),
            )
            .unwrap();
        fixtures::capture(
            &REGISTRY,
            &root,
            &store_at(root.clone()),
            &fixtures::directory("cli-state"),
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn identities_live_under_their_route() {
        let state = CliState::at(PathBuf::from("/state/clients/cli"));
        let agent: AgentName = "agent-1".parse().unwrap();
        assert_eq!(
            state.identity_path(&RouteKey::Local, None),
            PathBuf::from("/state/clients/cli/routes/local/base")
        );
        assert_eq!(
            state.identity_path(&RouteKey::Paired([0xab; 32]), Some(&agent)),
            PathBuf::from(format!(
                "/state/clients/cli/routes/{}/agents/agent-1",
                "ab".repeat(32)
            ))
        );
    }

    #[test]
    fn one_identity_writes_at_a_time_and_others_do_not_wait() {
        let state = temporary("writer");
        let agent: AgentName = "agent-1".parse().unwrap();
        let held = state
            .writer(&RouteKey::Local, None, Instant::now())
            .unwrap();
        assert!(matches!(
            state.writer(
                &RouteKey::Local,
                None,
                Instant::now() + std::time::Duration::from_millis(50)
            ),
            Err(Code::IdentityBusy)
        ));
        assert!(state.try_writer(&RouteKey::Local, None).unwrap().is_none());
        assert!(
            state
                .writer(&RouteKey::Local, Some(&agent), Instant::now())
                .is_ok(),
            "another agent has its own lock"
        );
        drop(held);
        assert!(state.try_writer(&RouteKey::Local, None).unwrap().is_some());
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn refused_runs_leave_no_lock_files() {
        let state = temporary("full-runs");
        let mut runs = state.runs(&RouteKey::Local, None, Instant::now()).unwrap();
        let pending = |id| PendingRun {
            run: RunId([id; 16]),
            pane: [2; 32],
            session: [3; 32],
            created_at: 0,
        };
        let live: Vec<_> = (0..MAX_PENDING_RUNS as u8)
            .map(|id| runs.add(pending(id)).unwrap())
            .collect();
        let path = state.identity_path(&RouteKey::Local, None).join("runs");
        let before: std::collections::BTreeSet<_> = std::fs::read_dir(&path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        for id in 16..48 {
            assert!(
                matches!(runs.add(pending(id)), Err(failure) if failure.code == Code::Capacity)
            );
        }
        let after = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(before, after, "refused runs must not create lock files");
        assert_eq!(runs.pending().unwrap().len(), MAX_PENDING_RUNS);
        drop(live);
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn concurrent_runs_keep_independent_locks_and_journal_updates() {
        let state = temporary("parallel-runs");
        let mut first = state.runs(&RouteKey::Local, None, Instant::now()).unwrap();
        let mut second = state.runs(&RouteKey::Local, None, Instant::now()).unwrap();
        let pending = |id| PendingRun {
            run: RunId([id; 16]),
            pane: [id; 32],
            session: [3; 32],
            created_at: 0,
        };
        let first_live = first.add(pending(1)).unwrap();
        let second_live = second.add(pending(2)).unwrap();
        assert_eq!(first.pending().unwrap(), vec![pending(1), pending(2)]);
        assert!(second.try_run(pending(1).run).unwrap().is_none());
        assert!(first.try_run(pending(2).run).unwrap().is_none());
        first.remove(pending(1).run).unwrap();
        assert_eq!(second.pending().unwrap(), vec![pending(2)]);
        assert!(
            !state
                .identity_path(&RouteKey::Local, None)
                .join("runs")
                .join(format!("{}.lock", crate::io::hex(&pending(1).run.0)))
                .exists()
        );
        drop(first_live);
        assert!(second.try_run(pending(1).run).unwrap().is_none());
        drop(second_live);
        assert!(first.try_run(pending(2).run).unwrap().is_some());
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn a_settler_on_an_unlinked_run_lock_rechecks_the_journal() {
        use std::os::fd::AsRawFd;
        let state = temporary("settler-race");
        let mut runs = state.runs(&RouteKey::Local, None, Instant::now()).unwrap();
        let run = PendingRun {
            run: RunId([4; 16]),
            pane: [2; 32],
            session: [3; 32],
            created_at: 0,
        };
        let live = runs.add(run.clone()).unwrap();
        let path = state
            .identity_path(&RouteKey::Local, None)
            .join("runs")
            .join(format!("{}.lock", crate::io::hex(&run.run.0)));
        // The settler opened the old inode before remove unlinks it.
        let old = std::fs::File::open(&path).unwrap();
        runs.remove(run.run).unwrap();
        assert!(!path.exists());
        // SAFETY: old is the live descriptor of this test's lock file.
        assert_ne!(
            unsafe { libc::flock(old.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        drop(live);
        // SAFETY: old remains live after the pathname was unlinked.
        assert_eq!(
            unsafe { libc::flock(old.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(
            !runs
                .pending()
                .unwrap()
                .iter()
                .any(|pending| pending.run == run.run)
        );
        drop(old);
        std::fs::remove_dir_all(&state.root).unwrap();
    }

    #[test]
    fn pending_runs_count_as_held_until_removed() {
        let state = temporary("runs");
        let agent: AgentName = "agent-1".parse().unwrap();
        assert!(state.held(&RouteKey::Local).unwrap().is_empty());
        let run = PendingRun {
            run: RunId([1; 16]),
            pane: [2; 32],
            session: [3; 32],
            created_at: 0,
        };
        let mut runs = state
            .runs(&RouteKey::Local, Some(&agent), Instant::now())
            .unwrap();
        let live = runs.add(run.clone()).unwrap();
        drop(runs);
        // An unrelated directory is not an identity.
        std::fs::create_dir_all(
            state
                .route_path(&RouteKey::Local)
                .join("agents/Not_an_agent"),
        )
        .unwrap();
        assert_eq!(
            state.identities(&RouteKey::Local).unwrap(),
            vec![None, Some(agent.clone())]
        );
        assert_eq!(
            state.held(&RouteKey::Local).unwrap(),
            vec![HeldIdentity {
                identity: RecoveredIdentity::Agent(agent.clone()),
                operations: 0,
                runs: 1,
            }]
        );
        let mut runs = state
            .runs(&RouteKey::Local, Some(&agent), Instant::now())
            .unwrap();
        assert_eq!(runs.pending().unwrap(), vec![run.clone()]);
        runs.remove(run.run).unwrap();
        drop(live);
        drop(runs);
        assert!(state.held(&RouteKey::Local).unwrap().is_empty());
        std::fs::remove_dir_all(&state.root).unwrap();
    }
}
