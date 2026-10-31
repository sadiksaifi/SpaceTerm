//! The service-operation ledger: one record per Stop, Restart, Update, Return or Network Access
//! change that the service lane admitted, from admission through its outcome. It lives in
//! `service-operations.json` beside the server identity, under `service-operations.lock`, so an
//! outcome survives the server that admitted it and the helper that performed it.
//!
//! Every change is a compare-and-set under that lock. The server admits and launches a record,
//! the helper alone moves it to `Running`, and whichever side last holds evidence settles it. The
//! lane claims and settles a Network Access change itself, because no helper performs one. A
//! record that reached a terminal state never changes again.
use crate::ServerError;
use crate::storage::Directory;
use serde::{Deserialize, Serialize};
use std::ffi::CStr;
use std::time::Duration;

/// The most unacknowledged records a ledger keeps. A full ledger refuses `Execute`.
pub const MAX_RECORDS: usize = 16;
const FILE: &CStr = c"service-operations.json";
const FILE_NAME: &str = "service-operations.json";
const LOCK: &str = "service-operations.lock";
/// The helper holds this lock from before it claims its record until it exits. A free lock
/// therefore proves that no helper is acting on any record.
pub(crate) const HELPER_LOCK: &str = "service-helper.lock";
/// How long a launched helper may take to claim its record before the server that launched it
/// settles the record as never performed.
pub(crate) const LAUNCH_GRACE: Duration = Duration::from_secs(60);

/// Names one service operation. The client chooses it, so a repeated `Execute` dispatches
/// nothing new.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OperationId(pub [u8; 16]);

impl OperationId {
    /// Parses the lane form: 32 lowercase hexadecimal characters.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != 32
            || !text
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        {
            return None;
        }
        let mut id = [0; 16];
        for (index, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self(id))
    }

    pub fn random() -> Result<Self, ServerError> {
        let bytes = crate::protocol::random()?;
        let mut id = [0; 16];
        id.copy_from_slice(&bytes[..16]);
        Ok(Self(id))
    }

    /// The first 8 hexadecimal characters, for names such as the helper's unit.
    pub fn short(&self) -> String {
        self.to_string()[..8].to_owned()
    }
}

impl std::fmt::Display for OperationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Stop,
    Restart,
    Update,
    Return,
    /// The lane applies it in-process. No helper claims it.
    Network(super::lane::network::NetworkAccess),
}

/// Who asked for an operation. Only the same actor acknowledges its record, and revoking a
/// pairing forgets that pairing's settled records and orphans its unsettled one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub pairing: Option<crate::direct::PairingId>,
    pub client: crate::hierarchy::Id,
}

/// Why an admitted operation was never performed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotPerformed {
    /// The server that admitted it ended before its helper started.
    ServerRestarted,
    /// The helper could not be started, or never claimed its record.
    LaunchFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Admitted,
    Launched,
    Running,
    Done,
    /// `code` is a code from the CLI output contract.
    Failed {
        code: String,
    },
    NotPerformed(NotPerformed),
    Unresolved,
}

impl State {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed { .. } | Self::NotPerformed(_) | Self::Unresolved
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: OperationId,
    pub actor: Actor,
    pub server: crate::hierarchy::Id,
    /// The run of the server that admitted it. Every server start has a fresh boot.
    pub boot: crate::hierarchy::Id,
    pub action: Action,
    pub observation_digest: [u8; 32],
    /// The installation digest an Update activates.
    pub candidate: Option<[u8; 32]>,
    pub state: State,
    /// Seconds since the Unix epoch.
    pub updated_at: u64,
    /// The actor's pairing was revoked while the record was unsettled. No actor can acknowledge
    /// it, so the ledger deletes it as soon as it settles. Until then it keeps the membership
    /// fence and refuses other operations like any unsettled record.
    pub orphaned: bool,
}

/// How [`Ledger::admit`] treated a record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Persisted now. The caller dispatches it.
    New(Record),
    /// The ID was already admitted. Nothing is dispatched again.
    Existing(Record),
}

/// What settlement may rely on besides the ledger itself.
pub(crate) trait Evidence {
    /// Whether a helper holds [`HELPER_LOCK`].
    fn helper_active(&self) -> Result<bool, ServerError>;
    /// The settled outcome of an Update or Return that the activation journal proves, if any.
    /// `ServiceBusy` means the journal is mid-step, so settlement waits.
    fn activation(&self, record: &Record) -> Result<Option<State>, ServerError>;
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    records: Vec<Record>,
}

/// One account's ledger. Every change is written and synced before it is reported.
pub struct Ledger {
    data: Directory,
}

impl Ledger {
    /// Opens the ledger of an existing server. It never creates server state.
    pub fn open(identity: &str) -> Result<Self, ServerError> {
        let paths = crate::host::Paths::for_identity(identity)?;
        Ok(Self::in_directory(Directory::open(&paths.data, false)?))
    }

    pub(crate) fn in_directory(data: Directory) -> Self {
        Self { data }
    }

    /// Persists `record` as `Admitted`. A repeated ID returns the existing record. The ledger
    /// refuses a second operation while one is unsettled, and any operation when it is full.
    pub fn admit(&mut self, record: Record) -> Result<Admission, ServerError> {
        if record.state != State::Admitted || record.orphaned {
            return Err(ServerError::Protocol);
        }
        self.change(|records| {
            if let Some(existing) = records.iter().find(|v| v.id == record.id) {
                return if existing.actor == record.actor {
                    Ok(Admission::Existing(existing.clone()))
                } else {
                    Err(ServerError::Changed)
                };
            }
            if records.iter().any(|v| !v.state.terminal()) {
                return Err(ServerError::ServiceBusy);
            }
            if records.len() >= MAX_RECORDS {
                return Err(ServerError::Capacity);
            }
            let mut record = record;
            record.updated_at = now();
            records.push(record.clone());
            Ok(Admission::New(record))
        })
    }

    /// Moves a record forward. `Launched` after the helper already claimed or settled the
    /// record keeps the later state. A terminal record never changes.
    pub fn mark(&mut self, operation: &OperationId, state: State) -> Result<Record, ServerError> {
        self.change(|records| {
            let record = records
                .iter_mut()
                .find(|v| v.id == *operation)
                .ok_or(ServerError::Changed)?;
            let allowed = match (&record.state, &state) {
                (State::Admitted, State::Launched) => true,
                (State::Launched | State::Running, State::Launched) => return Ok(record.clone()),
                (from, _) if from.terminal() => return Err(ServerError::Changed),
                (_, to) if to.terminal() => true,
                _ => false,
            };
            if !allowed {
                return Err(ServerError::Changed);
            }
            record.state = state;
            record.updated_at = now();
            Ok(record.clone())
        })
    }

    /// The helper's claim, or the lane's for a Network Access change: `Admitted` or `Launched`
    /// becomes `Running`. `None` means the record was settled without it, so the claimant must
    /// do nothing.
    pub(crate) fn claim(&mut self, operation: &OperationId) -> Result<Option<Record>, ServerError> {
        self.change(|records| {
            let record = records
                .iter_mut()
                .find(|v| v.id == *operation)
                .ok_or(ServerError::Changed)?;
            if !matches!(record.state, State::Admitted | State::Launched) {
                return Ok(None);
            }
            record.state = State::Running;
            record.updated_at = now();
            Ok(Some(record.clone()))
        })
    }

    pub fn resolve(&self, operation: &OperationId) -> Result<Option<Record>, ServerError> {
        let _lock = self.data.lock(LOCK, false)?;
        Ok(self.read()?.into_iter().find(|v| v.id == *operation))
    }

    /// Like [`Self::resolve`], but never waits for the ledger's lock. A held lock is
    /// `ServiceBusy`, so a caller with its own deadline asks again.
    pub fn resolve_now(&self, operation: &OperationId) -> Result<Option<Record>, ServerError> {
        let _lock = self.data.try_lock(LOCK)?.ok_or(ServerError::ServiceBusy)?;
        Ok(self.read()?.into_iter().find(|v| v.id == *operation))
    }

    /// Every record, for status and the fence watcher.
    pub(crate) fn records(&self) -> Result<Vec<Record>, ServerError> {
        let _lock = self.data.lock(LOCK, false)?;
        self.read()
    }

    /// Deletes a settled record, only for its own actor. An absent record is already gone.
    pub fn acknowledge(
        &mut self,
        operation: &OperationId,
        actor: &Actor,
    ) -> Result<(), ServerError> {
        self.change(|records| {
            let Some(index) = records.iter().position(|v| v.id == *operation) else {
                return Ok(());
            };
            if records[index].actor != *actor {
                return Err(ServerError::AuthenticationRefused);
            }
            if !records[index].state.terminal() {
                return Err(ServerError::ServiceBusy);
            }
            records.remove(index);
            Ok(())
        })
    }

    /// Revoking a pairing deletes its settled records. An unsettled record stays, orphaned, until
    /// its helper or start-up recovery settles it, because a helper may still be acting on it.
    pub fn forget_pairing(
        &mut self,
        pairing: &crate::direct::PairingId,
    ) -> Result<(), ServerError> {
        self.change(|records| {
            for record in records
                .iter_mut()
                .filter(|v| v.actor.pairing.as_ref() == Some(pairing))
            {
                record.orphaned = true;
            }
            Ok(())
        })
    }

    /// Settles records a previous server left behind, before it accepts connections.
    pub(crate) fn recover_at_start(
        &mut self,
        boot: &crate::hierarchy::Id,
        evidence: &dyn Evidence,
    ) -> Result<(), ServerError> {
        self.settle(boot, evidence, None)
    }

    /// Settles every record that no live actor can still move: records of an earlier server
    /// whose helper never claimed them, records whose helper exited without an outcome, and
    /// records of this server whose helper missed [`LAUNCH_GRACE`]. `launch_grace` overrides
    /// the grace in tests.
    pub(crate) fn settle(
        &mut self,
        boot: &crate::hierarchy::Id,
        evidence: &dyn Evidence,
        launch_grace: Option<Duration>,
    ) -> Result<(), ServerError> {
        let grace = launch_grace.unwrap_or(LAUNCH_GRACE).as_secs();
        self.change(|records| {
            if records.iter().all(|v| v.state.terminal()) || evidence.helper_active()? {
                return Ok(());
            }
            let now = now();
            for record in records.iter_mut().filter(|v| !v.state.terminal()) {
                let earlier = record.boot != *boot;
                let next = match record.state {
                    State::Admitted if earlier => {
                        State::NotPerformed(NotPerformed::ServerRestarted)
                    }
                    State::Launched if earlier => {
                        State::NotPerformed(NotPerformed::ServerRestarted)
                    }
                    State::Launched if now.saturating_sub(record.updated_at) >= grace => {
                        State::NotPerformed(NotPerformed::LaunchFailed)
                    }
                    State::Running => match record.action {
                        Action::Update | Action::Return => match evidence.activation(record) {
                            Ok(state) => state.unwrap_or(State::Unresolved),
                            // The activation is mid-step under the service lock. Settle later.
                            Err(ServerError::ServiceBusy) => continue,
                            Err(error) => return Err(error),
                        },
                        Action::Restart if earlier => State::Done,
                        Action::Restart | Action::Stop => State::Unresolved,
                        // The server that applied it ended before it recorded the outcome.
                        Action::Network(_) if earlier => State::Unresolved,
                        // The lane is applying it now.
                        Action::Network(_) => continue,
                    },
                    _ => continue,
                };
                record.state = next;
                record.updated_at = now;
            }
            Ok(())
        })
    }

    fn read(&self) -> Result<Vec<Record>, ServerError> {
        let document: Document = self.data.read_json(FILE_NAME)?.unwrap_or(Document {
            schema: 1,
            records: Vec::new(),
        });
        if document.schema != 1 || document.records.len() > MAX_RECORDS {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(document.records)
    }

    fn change<T>(
        &mut self,
        apply: impl FnOnce(&mut Vec<Record>) -> Result<T, ServerError>,
    ) -> Result<T, ServerError> {
        let _lock = self.data.lock(LOCK, false)?;
        let before = self.read()?;
        let mut records = before.clone();
        let result = apply(&mut records)?;
        records.retain(|v| !(v.orphaned && v.state.terminal()));
        if records != before {
            self.data
                .write_json(FILE, &Document { schema: 1, records })?;
        }
        Ok(result)
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |v| v.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let suffix =
                u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap());
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("spaceterm-ledger-{suffix:x}"));
            Self { root }
        }
        fn ledger(&self) -> Ledger {
            Ledger::in_directory(Directory::open(&self.root, true).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    struct Facts {
        helper: bool,
        activation: Option<State>,
    }
    impl Evidence for Facts {
        fn helper_active(&self) -> Result<bool, ServerError> {
            Ok(self.helper)
        }
        fn activation(&self, _: &Record) -> Result<Option<State>, ServerError> {
            Ok(self.activation.clone())
        }
    }
    const IDLE: Facts = Facts {
        helper: false,
        activation: None,
    };

    fn record(id: u8, action: Action, boot: u8) -> Record {
        Record {
            id: OperationId([id; 16]),
            actor: Actor {
                pairing: Some([7; 32]),
                client: [8; 32],
            },
            server: [1; 32],
            boot: [boot; 32],
            action,
            observation_digest: [2; 32],
            candidate: None,
            state: State::Admitted,
            updated_at: 0,
            orphaned: false,
        }
    }

    #[test]
    fn operation_ids_round_trip_their_hex_form_and_reject_others() {
        let id = OperationId([0xab; 16]);
        assert_eq!(id.to_string(), "ab".repeat(16));
        assert_eq!(OperationId::parse(&id.to_string()), Some(id));
        assert_eq!(id.short(), "abababab");
        for bad in [
            "",
            "AB".repeat(16).as_str(),
            &"ab".repeat(15),
            &"zz".repeat(16),
        ] {
            assert_eq!(OperationId::parse(bad), None);
        }
    }

    #[test]
    fn a_repeated_id_returns_its_record_and_one_unsettled_operation_admits_no_other() {
        let fixture = Fixture::new();
        let mut ledger = fixture.ledger();
        let first = record(1, Action::Restart, 1);
        assert!(matches!(
            ledger.admit(first.clone()).unwrap(),
            Admission::New(_)
        ));
        assert!(
            matches!(ledger.admit(first.clone()).unwrap(), Admission::Existing(v) if v.id == first.id)
        );
        let mut stranger = first.clone();
        stranger.actor.client = [9; 32];
        assert_eq!(ledger.admit(stranger), Err(ServerError::Changed));
        assert_eq!(
            ledger.admit(record(2, Action::Stop, 1)),
            Err(ServerError::ServiceBusy)
        );
        ledger.mark(&first.id, State::Done).unwrap();
        assert!(matches!(
            ledger.admit(record(2, Action::Stop, 1)).unwrap(),
            Admission::New(_)
        ));
    }

    #[test]
    fn a_full_ledger_refuses_admission_until_an_outcome_is_acknowledged() {
        let fixture = Fixture::new();
        let mut ledger = fixture.ledger();
        for id in 0..MAX_RECORDS as u8 {
            ledger.admit(record(id, Action::Stop, 1)).unwrap();
            ledger
                .mark(&OperationId([id; 16]), State::Unresolved)
                .unwrap();
        }
        assert_eq!(
            ledger.admit(record(99, Action::Stop, 1)),
            Err(ServerError::Capacity)
        );
        let actor = record(0, Action::Stop, 1).actor;
        let stranger = Actor {
            pairing: None,
            client: [3; 32],
        };
        assert_eq!(
            ledger.acknowledge(&OperationId([0; 16]), &stranger),
            Err(ServerError::AuthenticationRefused)
        );
        ledger.acknowledge(&OperationId([0; 16]), &actor).unwrap();
        ledger.acknowledge(&OperationId([0; 16]), &actor).unwrap();
        assert!(matches!(
            ledger.admit(record(99, Action::Stop, 1)).unwrap(),
            Admission::New(_)
        ));
    }

    #[test]
    fn records_only_move_forward_and_a_terminal_record_never_changes() {
        let fixture = Fixture::new();
        let mut ledger = fixture.ledger();
        let id = OperationId([1; 16]);
        ledger.admit(record(1, Action::Stop, 1)).unwrap();
        assert_eq!(ledger.mark(&id, State::Running), Err(ServerError::Changed));
        assert_eq!(ledger.claim(&id).unwrap().unwrap().state, State::Running);
        // The server persists Launched after spawning; a helper that already claimed wins.
        assert_eq!(
            ledger.mark(&id, State::Launched).unwrap().state,
            State::Running
        );
        assert_eq!(ledger.mark(&id, State::Done).unwrap().state, State::Done);
        assert_eq!(
            ledger.mark(&id, State::Unresolved),
            Err(ServerError::Changed)
        );
        assert_eq!(ledger.claim(&id).unwrap(), None);
        let actor = record(1, Action::Stop, 1).actor;
        ledger.admit(record(2, Action::Stop, 1)).unwrap();
        assert_eq!(
            ledger.acknowledge(&OperationId([2; 16]), &actor),
            Err(ServerError::ServiceBusy)
        );
    }

    #[test]
    fn a_new_server_settles_what_its_predecessor_and_a_dead_helper_left() {
        let fixture = Fixture::new();
        let mut ledger = fixture.ledger();
        let state = |ledger: &Ledger, id: u8| {
            ledger
                .resolve(&OperationId([id; 16]))
                .unwrap()
                .unwrap()
                .state
        };
        // Admitted by a server that never launched its helper.
        ledger.admit(record(1, Action::Stop, 1)).unwrap();
        ledger.recover_at_start(&[2; 32], &IDLE).unwrap();
        assert_eq!(
            state(&ledger, 1),
            State::NotPerformed(NotPerformed::ServerRestarted)
        );
        // A Restart whose helper ended while the new server started.
        ledger.admit(record(2, Action::Restart, 1)).unwrap();
        ledger.claim(&OperationId([2; 16])).unwrap();
        ledger.recover_at_start(&[2; 32], &IDLE).unwrap();
        assert_eq!(state(&ledger, 2), State::Done);
        // A Stop whose helper ended mid-action cannot be proven.
        ledger.admit(record(3, Action::Stop, 2)).unwrap();
        ledger.claim(&OperationId([3; 16])).unwrap();
        ledger.recover_at_start(&[3; 32], &IDLE).unwrap();
        assert_eq!(state(&ledger, 3), State::Unresolved);
        // An Update settles from the activation journal, or stays unresolved without it.
        ledger.admit(record(4, Action::Update, 3)).unwrap();
        ledger.claim(&OperationId([4; 16])).unwrap();
        let journal = Facts {
            helper: false,
            activation: Some(State::Failed {
                code: "startup_failed".into(),
            }),
        };
        ledger.recover_at_start(&[4; 32], &journal).unwrap();
        assert_eq!(
            state(&ledger, 4),
            State::Failed {
                code: "startup_failed".into()
            }
        );
        ledger.admit(record(5, Action::Update, 4)).unwrap();
        ledger.claim(&OperationId([5; 16])).unwrap();
        ledger.recover_at_start(&[5; 32], &IDLE).unwrap();
        assert_eq!(state(&ledger, 5), State::Unresolved);
    }

    #[test]
    fn a_network_access_change_settles_only_once_its_server_ended() {
        let fixture = Fixture::new();
        let mut ledger = fixture.ledger();
        let network = Action::Network(super::super::lane::network::NetworkAccess::All);
        let id = OperationId([1; 16]);
        ledger.admit(record(1, network, 1)).unwrap();
        ledger.claim(&id).unwrap();
        // The server that claimed it is applying it now.
        ledger.settle(&[1; 32], &IDLE, None).unwrap();
        assert_eq!(ledger.resolve(&id).unwrap().unwrap().state, State::Running);
        // That server ended before it recorded the outcome.
        ledger.recover_at_start(&[2; 32], &IDLE).unwrap();
        assert_eq!(
            ledger.resolve(&id).unwrap().unwrap().state,
            State::Unresolved
        );
        // One that never started the change was not performed.
        ledger.admit(record(2, network, 2)).unwrap();
        ledger.recover_at_start(&[3; 32], &IDLE).unwrap();
        assert_eq!(
            ledger
                .resolve(&OperationId([2; 16]))
                .unwrap()
                .unwrap()
                .state,
            State::NotPerformed(NotPerformed::ServerRestarted)
        );
    }

    #[test]
    fn settlement_never_touches_a_record_while_a_helper_acts_or_its_server_launches_it() {
        let fixture = Fixture::new();
        let mut ledger = fixture.ledger();
        let id = OperationId([1; 16]);
        ledger.admit(record(1, Action::Restart, 1)).unwrap();
        ledger.claim(&id).unwrap();
        let busy = Facts {
            helper: true,
            activation: None,
        };
        ledger.recover_at_start(&[2; 32], &busy).unwrap();
        assert_eq!(ledger.resolve(&id).unwrap().unwrap().state, State::Running);
        ledger.mark(&id, State::Done).unwrap();
        // The admitting server is mid-Execute: its own Admitted record is left alone.
        let id = OperationId([2; 16]);
        ledger.admit(record(2, Action::Stop, 1)).unwrap();
        ledger.settle(&[1; 32], &IDLE, None).unwrap();
        assert_eq!(ledger.resolve(&id).unwrap().unwrap().state, State::Admitted);
        ledger.mark(&id, State::Launched).unwrap();
        ledger.settle(&[1; 32], &IDLE, None).unwrap();
        assert_eq!(ledger.resolve(&id).unwrap().unwrap().state, State::Launched);
        // A helper that missed its grace is never performed, and cannot claim afterwards.
        ledger
            .settle(&[1; 32], &IDLE, Some(Duration::ZERO))
            .unwrap();
        assert_eq!(
            ledger.resolve(&id).unwrap().unwrap().state,
            State::NotPerformed(NotPerformed::LaunchFailed)
        );
        assert_eq!(ledger.claim(&id).unwrap(), None);
    }

    #[test]
    fn revoking_a_pairing_forgets_its_outcomes_and_orphans_its_unsettled_operation_until_it_settles()
     {
        let fixture = Fixture::new();
        let mut ledger = fixture.ledger();
        let settled = OperationId([1; 16]);
        let running = OperationId([2; 16]);
        ledger.admit(record(1, Action::Stop, 1)).unwrap();
        ledger.mark(&settled, State::Done).unwrap();
        ledger.admit(record(2, Action::Restart, 1)).unwrap();
        ledger.claim(&running).unwrap();
        ledger.forget_pairing(&[7; 32]).unwrap();
        assert_eq!(ledger.resolve(&settled).unwrap(), None);
        let orphan = ledger.resolve(&running).unwrap().unwrap();
        assert!(orphan.orphaned && orphan.state == State::Running);
        // The orphan still refuses another device's operation.
        let mut other = record(3, Action::Stop, 1);
        other.actor.pairing = Some([9; 32]);
        assert_eq!(ledger.admit(other.clone()), Err(ServerError::ServiceBusy));
        // Its helper's outcome deletes it, since nobody is left to acknowledge it.
        assert_eq!(
            ledger.mark(&running, State::Done).unwrap().state,
            State::Done
        );
        assert_eq!(ledger.resolve(&running).unwrap(), None);
        assert!(matches!(ledger.admit(other).unwrap(), Admission::New(_)));
        // An orphan that start-up recovery settles is deleted the same way.
        ledger.mark(&OperationId([3; 16]), State::Done).unwrap();
        ledger.admit(record(4, Action::Stop, 1)).unwrap();
        ledger.forget_pairing(&[7; 32]).unwrap();
        ledger.recover_at_start(&[2; 32], &IDLE).unwrap();
        assert_eq!(ledger.resolve(&OperationId([4; 16])).unwrap(), None);
    }
}
