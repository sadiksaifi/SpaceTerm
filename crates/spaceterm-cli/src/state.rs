//! The CLI's own state under `clients/cli`: Saved Connections, and per route and identity the
//! client record, the Workspace edit journal, attachments and pending runs.
//!
//! ```text
//! clients/cli/
//!   connections.json, connections.lock
//!   routes/<local | 64-hex route>/
//!     base/            client.json (local route only), operations.json, operations.lock,
//!                      attachments.json, runs.json, runs.lock, inventory.json (paired routes only)
//!     agents/<name>/   the same files except client.json
//! ```
//!
//! The base identity and each agent identity have their own writer lock, `operations.lock`. A
//! mutating command holds it from journal open and sequence allocation through acknowledgement,
//! so the commands of one identity mutate one at a time. The desktop's `clients/desktop` is never
//! read or written here.
use crate::contract::{Code, Failure, RecoveredIdentity, from_server_error};
use serde::{Deserialize, Serialize};
use spaceterm_server::hierarchy::{ClientJournal, Id};
use spaceterm_server::protocol::{AgentName, RunId};
use spaceterm_server::{Directory, Lock, LockWait, ServerConnection, ServerError};
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
        connection: &ServerConnection,
    ) -> Result<&mut ClientJournal, Failure> {
        if let Some(lock) = self.lock.take() {
            self.journal = Some(
                connection
                    .shared_journal_locked(lock)
                    .map_err(from_server_error)?,
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

/// One identity's `runs.json`, held under `runs.lock` while it is open. `run` persists a run
/// here before it dispatches it, and leftover settlement removes it once acknowledged.
pub struct Runs {
    directory: Directory,
    _lock: Lock,
    record: RunsRecord,
}

impl Runs {
    pub fn pending(&self) -> &[PendingRun] {
        &self.record.runs
    }

    /// Records `run` before its dispatch. A full table refuses with `capacity`.
    pub fn add(&mut self, run: PendingRun) -> Result<(), Failure> {
        if self
            .record
            .runs
            .iter()
            .any(|pending| pending.run == run.run)
        {
            return Ok(());
        }
        if self.record.runs.len() >= MAX_PENDING_RUNS {
            return Err(Failure::new(Code::Capacity));
        }
        self.record.runs.push(run);
        self.save()
    }

    /// Forgets `run` after the server acknowledged its receipt, or never admitted it.
    pub fn remove(&mut self, run: RunId) -> Result<(), Failure> {
        let before = self.record.runs.len();
        self.record.runs.retain(|pending| pending.run != run);
        if self.record.runs.len() == before {
            return Ok(());
        }
        self.save()
    }

    fn save(&self) -> Result<(), Failure> {
        self.directory
            .write_json(RUNS, &self.record)
            .map_err(from_server_error)
    }
}

impl CliState {
    pub fn open(identity: &str) -> Result<Self, ServerError> {
        Ok(Self {
            root: spaceterm_server::cli_state_root(identity)?,
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
    ) -> Result<Directory, ServerError> {
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

    /// The identity's pending runs, under `runs.lock`. Contention waits until `deadline`, then
    /// fails with `identity_busy`.
    pub fn runs(
        &self,
        route: &RouteKey,
        agent: Option<&AgentName>,
        deadline: Instant,
    ) -> Result<Runs, Code> {
        let directory = self
            .identity_dir(route, agent)
            .map_err(|_| Code::StorageUnavailable)?;
        let limit = deadline.saturating_duration_since(Instant::now());
        let lock = match directory.wait_lock(RUNS_LOCK, limit, None) {
            Ok(LockWait::Locked(lock)) => lock,
            Ok(LockWait::Held | LockWait::Cancelled) => return Err(Code::IdentityBusy),
            Err(_) => return Err(Code::StorageUnavailable),
        };
        let record = directory
            .read_json::<RunsRecord>("runs.json")
            .map_err(|_| Code::StorageUnavailable)?
            .unwrap_or_default();
        if record.runs.len() > MAX_PENDING_RUNS {
            return Err(Code::StorageUnavailable);
        }
        Ok(Runs {
            directory,
            _lock: lock,
            record,
        })
    }

    /// The base identity and every agent this CLI has used on `route`, without creating any.
    pub fn identities(&self, route: &RouteKey) -> Result<Vec<Option<AgentName>>, ServerError> {
        let mut identities = vec![None];
        let agents = self.route_path(route).join("agents");
        let entries = match std::fs::read_dir(&agents) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(identities),
            Err(_) => return Err(ServerError::StorageUnavailable),
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
    pub fn held(&self, route: &RouteKey) -> Result<Vec<HeldIdentity>, ServerError> {
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
        runs.add(run.clone()).unwrap();
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
        assert_eq!(runs.pending(), [run.clone()]);
        runs.remove(run.run).unwrap();
        drop(runs);
        assert!(state.held(&RouteKey::Local).unwrap().is_empty());
        std::fs::remove_dir_all(&state.root).unwrap();
    }
}
