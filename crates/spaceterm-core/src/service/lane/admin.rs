//! The administration steps every SpaceTerm Client shares over a server's service lane: settle
//! the operations this identity left pending, read Status and the update facts, install a new
//! version from the Core's feed or by pushing an archive, preview Stop, Restart or a Network
//! Access change, and execute a confirmed preview until it settles.
//!
//! The CLI and the desktop present each result their own way. Both take the same steps here, so
//! a confirmation, a dispatched operation and its settlement mean the same on either.
use super::client::{Lane, LaneError, Leftover, Outcome, Session};
use super::network::{LaneNetworkEffects, NetworkAccess};
use super::{
    LaneAction, LaneEffects, LaneOperationState, LaneReceipt, LaneReply, LaneRequest, LaneStatus,
    MAX_CHUNK,
};
use crate::CoreError;
use crate::service::operations::OperationId;
use crate::storage::{Directory, Lock, LockWait};
use crate::update::facts::UpdateFacts;
use crate::update::ordering::Candidate;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::{Duration, Instant};

/// How long a read or a preview may take.
pub const REQUEST_LIMIT: Duration = Duration::from_secs(30);
/// How long the server may take to fetch, verify and install a release from its feed, or to
/// verify and install the last chunk of a push.
pub const STAGE_LIMIT: Duration = Duration::from_secs(30 * 60);
/// How long a Stop or an Uninstall is followed: until SpaceTerm Core closes the lane.
pub const STOP_WAIT: Duration = Duration::from_secs(90);
/// How long any other action is followed: until it settles, or until a server that replaces
/// itself, as Restart and Update do, returns.
pub const RETURN_WAIT: Duration = Duration::from_secs(180);
/// How long a Network Access change is followed. It covers the server's own
/// [`super::network::LISTEN_LIMIT`] for the new listener, and a reconnect after it. One the new
/// listener no longer serves never reconnects, so waiting as long as for a server that replaces
/// itself only delays an unconfirmed outcome.
pub const NETWORK_WAIT: Duration = Duration::from_secs(30);

/// How long an approved action waits for another SpaceTerm Core operation to settle, as a
/// desktop launch waits for the service lock under ADR 0034.
pub const BUSY_WAIT: Duration = Duration::from_secs(180);
/// How often a waiting approval asks again whether SpaceTerm Core is still busy.
const BUSY_POLL: Duration = Duration::from_secs(1);
use super::client::BUSY;

/// How long an action is followed after its `Execute`. Only Stop and Uninstall end the lane they
/// are followed over, so every other action, including one added later, is followed until it
/// settles.
pub fn wait(action: &LaneAction) -> Duration {
    match action {
        LaneAction::Stop | LaneAction::Uninstall => STOP_WAIT,
        LaneAction::Network { .. } => NETWORK_WAIT,
        _ => RETURN_WAIT,
    }
}

/// The record of one route and identity's admitted operations, kept in `directory`.
const PENDING: &str = "service-operations.json";
const PENDING_FILE: &std::ffi::CStr = c"service-operations.json";
/// The outcomes one route and identity acknowledged, kept beside its pending operations so a
/// surface that asks about one after SpaceTerm Core forgot its receipt still reads it.
const OUTCOMES: &str = "service-outcomes.json";
const OUTCOMES_FILE: &std::ffi::CStr = c"service-outcomes.json";
/// Held across each read-modify-write of the pending operations or the kept outcomes.
const RECORD_LOCK: &str = "service-operations.lock";
/// Held shared by each flow that follows an operation it recorded, and exclusively while a lane
/// connection settles the identity's leftovers, so that settlement never acknowledges a receipt
/// a running flow still waits to read.
const FOLLOWING: &str = "service-operations-following.lock";
/// At most this many pending operations are kept. The server holds no more than 16.
const MAX_PENDING: usize = 16;
/// At most this many acknowledged outcomes are kept. The oldest goes first.
const MAX_OUTCOMES: usize = 32;
/// How long a caller that set no deadline of its own waits for another holder of the record.
const RECORD_WAIT: Duration = REQUEST_LIMIT;

/// Whether `value` is a client's `service-operations.json` this build reads.
pub(crate) fn readable(value: &serde_json::Value) -> bool {
    crate::migration::strict::<Vec<String>>(value)
}

/// Whether `value` is a client's `service-outcomes.json` this build reads.
pub(crate) fn readable_outcomes(value: &serde_json::Value) -> bool {
    Vec::<KeptOutcome>::deserialize(value).is_ok_and(|kept| {
        kept.len() <= MAX_OUTCOMES
            && kept
                .iter()
                .all(|outcome| super::client::settled(&outcome.receipt()))
    })
}

/// One acknowledged outcome as `service-outcomes.json` keeps it: the settled receipt of the
/// operation it names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptOutcome {
    operation: String,
    action: LaneAction,
    state: LaneOperationState,
    code: Option<String>,
    boot: String,
}

impl KeptOutcome {
    fn of(receipt: &LaneReceipt) -> Self {
        Self {
            operation: receipt.operation.clone(),
            action: receipt.action.clone(),
            state: receipt.state,
            code: receipt.code.clone(),
            boot: receipt.boot.clone(),
        }
    }

    fn receipt(&self) -> LaneReceipt {
        LaneReceipt {
            operation: self.operation.clone(),
            action: self.action.clone(),
            state: self.state,
            code: self.code.clone(),
            boot: self.boot.clone(),
        }
    }
}

/// The service operations one client identity admitted on one route and has not acknowledged,
/// so the next lane connection settles them, and the outcomes it acknowledged, so every surface
/// that later asks about one of them reads the same outcome.
pub struct PendingOperations {
    directory: Directory,
}

impl PendingOperations {
    /// The record in `directory`, which belongs to exactly one route and Client Identity.
    pub fn new(directory: Directory) -> Self {
        Self { directory }
    }

    /// The record's lock, waiting for another holder until `deadline`. A holder that keeps it
    /// past then is [`LaneError::Deadline`].
    fn lock_by(&self, deadline: Instant) -> Result<Lock, LaneError> {
        let limit = deadline.saturating_duration_since(Instant::now());
        match self.directory.wait_lock(RECORD_LOCK, limit, None) {
            Ok(LockWait::Locked(lock)) => Ok(lock),
            Ok(LockWait::Held | LockWait::Cancelled) => Err(LaneError::Deadline),
            Err(error) => Err(LaneError::Core(error)),
        }
    }

    fn change_by(
        &self,
        deadline: Instant,
        apply: impl FnOnce(&mut Vec<String>) -> Result<(), CoreError>,
    ) -> Result<Vec<String>, LaneError> {
        let _lock = self.lock_by(deadline)?;
        let mut pending: Vec<String> = self.directory.read_json(PENDING)?.unwrap_or_default();
        let before = pending.clone();
        apply(&mut pending)?;
        if pending != before {
            self.directory.write_json(PENDING_FILE, &pending)?;
        }
        Ok(pending)
    }

    pub fn list(&self) -> Result<Vec<String>, CoreError> {
        self.list_by(Instant::now() + RECORD_WAIT)
            .map_err(record_error)
    }

    /// The pending operations, read by `deadline`.
    pub fn list_by(&self, deadline: Instant) -> Result<Vec<String>, LaneError> {
        self.change_by(deadline, |_| Ok(()))
    }

    /// Records an operation right before its `Execute` is sent.
    pub fn add(&self, operation: &OperationId) -> Result<(), CoreError> {
        self.add_by(operation, Instant::now() + RECORD_WAIT)
            .map_err(record_error)
    }

    /// [`Self::add`] by `deadline`.
    fn add_by(&self, operation: &OperationId, deadline: Instant) -> Result<(), LaneError> {
        self.change_by(deadline, |pending| {
            if pending.len() >= MAX_PENDING {
                return Err(CoreError::Capacity);
            }
            pending.push(operation.to_string());
            Ok(())
        })
        .map(drop)
    }

    pub fn remove(&self, operation: &str) -> Result<(), CoreError> {
        self.remove_by(operation, Instant::now() + RECORD_WAIT)
            .map_err(record_error)
    }

    /// [`Self::remove`] by `deadline`. A deadline that already passed tries once.
    pub fn remove_by(&self, operation: &str, deadline: Instant) -> Result<(), LaneError> {
        self.change_by(deadline, |pending| {
            pending.retain(|v| v != operation);
            Ok(())
        })
        .map(drop)
    }

    /// Keeps the outcome of each settled receipt in `receipts` before it is acknowledged, so a
    /// reply lost after SpaceTerm Core forgot the receipt still leaves the outcome here.
    pub(crate) fn keep_by(
        &self,
        receipts: &[LaneReceipt],
        deadline: Instant,
    ) -> Result<(), LaneError> {
        if receipts.is_empty() {
            return Ok(());
        }
        let _lock = self.lock_by(deadline)?;
        let mut kept: Vec<KeptOutcome> = self.directory.read_json(OUTCOMES)?.unwrap_or_default();
        for receipt in receipts {
            kept.retain(|outcome| outcome.operation != receipt.operation);
            kept.push(KeptOutcome::of(receipt));
        }
        let excess = kept.len().saturating_sub(MAX_OUTCOMES);
        kept.drain(..excess);
        self.directory.write_json(OUTCOMES_FILE, &kept)?;
        Ok(())
    }

    /// The settled receipt of `operation` this route and identity acknowledged, read by
    /// `deadline`.
    pub fn kept_by(
        &self,
        operation: &str,
        deadline: Instant,
    ) -> Result<Option<LaneReceipt>, LaneError> {
        let _lock = self.lock_by(deadline)?;
        let kept: Vec<KeptOutcome> = self.directory.read_json(OUTCOMES)?.unwrap_or_default();
        Ok(kept
            .iter()
            .find(|outcome| outcome.operation == operation)
            .map(KeptOutcome::receipt))
    }

    /// The claim of a flow that follows an operation it is about to record, held until the flow
    /// returns. It waits until `deadline` for a settlement that runs now.
    fn follow(&self, deadline: Instant) -> Result<Lock, LaneError> {
        let limit = deadline.saturating_duration_since(Instant::now());
        match self.directory.wait_lock_shared(FOLLOWING, limit) {
            Ok(LockWait::Locked(lock)) => Ok(lock),
            Ok(LockWait::Held | LockWait::Cancelled) => Err(LaneError::Deadline),
            Err(_) => Err(LaneError::Core(CoreError::StorageUnavailable)),
        }
    }

    /// The claim that lets a connection settle this identity's leftovers, or None while a flow
    /// follows one of them.
    fn settle(&self) -> Result<Option<Lock>, CoreError> {
        self.directory.try_lock(FOLLOWING)
    }
}

/// A lane connection that [`open`] did not return. `settled` holds every receipt it
/// acknowledged before `error`, whose outcomes the caller still reports.
#[derive(Debug, PartialEq, Eq)]
pub struct Unopened {
    pub error: LaneError,
    pub settled: Vec<LaneReceipt>,
}

impl From<LaneError> for Unopened {
    fn from(error: LaneError) -> Self {
        Self {
            error,
            settled: Vec::new(),
        }
    }
}

impl From<CoreError> for Unopened {
    fn from(error: CoreError) -> Self {
        LaneError::Core(error).into()
    }
}

/// A record failure for a caller that set no deadline: a holder that kept the record past
/// [`RECORD_WAIT`] leaves it unavailable.
fn record_error(error: LaneError) -> CoreError {
    match error {
        LaneError::Core(error) => error,
        _ => CoreError::StorageUnavailable,
    }
}

/// Opens one lane connection by `deadline` and settles what earlier commands of this identity
/// left on it. Returns the receipts it settled and acknowledged. An operation that has not
/// settled stays recorded for a later connection, and so does every leftover while a flow of
/// this identity follows an operation, because that flow reads its own receipt.
///
/// Every leftover is resolved before any is acknowledged, so a failed read loses no outcome.
/// Each settled outcome is kept before its acknowledgement is sent, so an acknowledgement whose
/// reply was lost after SpaceTerm Core forgot the receipt loses no outcome either: a later
/// connection that finds the receipt gone returns the kept outcome instead. A failed
/// acknowledgement returns the receipts acknowledged before it in [`Unopened`].
pub fn open(
    lane: &Lane,
    pending: &PendingOperations,
    deadline: Instant,
) -> Result<(Session, Vec<LaneReceipt>), Unopened> {
    let mut session = lane.session_by(deadline)?;
    let Some(_settling) = pending.settle()? else {
        return Ok((session, Vec::new()));
    };
    let mut settled = Vec::new();
    let mut gone = Vec::new();
    for operation in pending.list_by(deadline)? {
        let Some(id) = OperationId::parse(&operation) else {
            gone.push(operation);
            continue;
        };
        let by = deadline.min(Instant::now() + REQUEST_LIMIT);
        match super::client::leftover(&mut session, &id, by)? {
            Leftover::Settled(receipt) => settled.push((id, receipt)),
            Leftover::Gone => gone.push(operation),
            Leftover::Pending => {}
        }
    }
    let receipts: Vec<_> = settled.iter().map(|(_, receipt)| receipt.clone()).collect();
    // Every outcome is kept before any receipt is acknowledged.
    pending.keep_by(&receipts, deadline)?;
    // An operation SpaceTerm Core no longer holds was acknowledged by an earlier connection
    // whose reply was lost, or its pairing was revoked. Only the first kept an outcome.
    let mut acknowledged = Vec::new();
    for operation in gone {
        if let Some(receipt) = pending.kept_by(&operation, deadline)? {
            acknowledged.push(receipt);
        }
        pending.remove_by(&operation, deadline)?;
    }
    for (id, receipt) in settled {
        let by = deadline.min(Instant::now() + REQUEST_LIMIT);
        if let Err(error) = acknowledge_kept(&mut session, pending, &id, by) {
            return Err(Unopened {
                error,
                settled: acknowledged,
            });
        }
        acknowledged.push(receipt);
    }
    Ok((session, acknowledged))
}

/// Settles the receipt of `id`, which this route and identity recorded: keeps its outcome, then
/// acknowledges it. Every connection that settles a receipt of a recorded operation does it this
/// way, whether a flow follows its own operation or a later connection settles leftovers.
fn settle_receipt(
    session: &mut Session,
    pending: &PendingOperations,
    id: &OperationId,
    receipt: &LaneReceipt,
    deadline: Instant,
) -> Result<(), LaneError> {
    pending.keep_by(std::slice::from_ref(receipt), deadline)?;
    acknowledge_kept(session, pending, id, deadline)
}

/// Acknowledges the receipt of `id`, whose outcome is already kept, and forgets the operation
/// once SpaceTerm Core confirmed the acknowledgement. An acknowledgement that may not have
/// arrived leaves the operation recorded: a later connection acknowledges a receipt SpaceTerm
/// Core still holds, and returns the kept outcome of one it no longer holds.
fn acknowledge_kept(
    session: &mut Session,
    pending: &PendingOperations,
    id: &OperationId,
    deadline: Instant,
) -> Result<(), LaneError> {
    super::client::acknowledge(session, id, deadline)?;
    // An acknowledged record the server no longer holds settles as gone next time.
    let _ = pending.remove_by(&id.to_string(), deadline);
    Ok(())
}

/// Check Status: what became of `operation`, which this identity admitted on this route, read
/// by `deadline`. It reads the outcome this identity kept when any of its surfaces acknowledged
/// the receipt, and otherwise asks SpaceTerm Core. A settled receipt is kept before it is
/// acknowledged. It is acknowledged only while no flow of this identity follows an operation,
/// because such a flow reads its own receipt. It never repeats the action.
pub fn check(
    lane: &Lane,
    pending: &PendingOperations,
    operation: &OperationId,
    deadline: Instant,
) -> Result<Leftover, LaneError> {
    let key = operation.to_string();
    if let Some(receipt) = pending.kept_by(&key, deadline)? {
        return Ok(Leftover::Settled(receipt));
    }
    let mut session = lane.session_by(deadline)?;
    let by = deadline.min(Instant::now() + REQUEST_LIMIT);
    match super::client::leftover(&mut session, operation, by)? {
        Leftover::Settled(receipt) => {
            pending.keep_by(std::slice::from_ref(&receipt), deadline)?;
            if let Some(_settling) = pending.settle()? {
                let _ = acknowledge_kept(&mut session, pending, operation, by);
            }
            Ok(Leftover::Settled(receipt))
        }
        // Another surface of this identity may have kept and acknowledged it meanwhile.
        Leftover::Gone => Ok(pending
            .kept_by(&key, deadline)?
            .map_or(Leftover::Gone, Leftover::Settled)),
        Leftover::Pending => Ok(Leftover::Pending),
    }
}

/// The server's own service facts.
pub fn status(session: &mut Session) -> Result<LaneStatus, LaneError> {
    match session.call(&LaneRequest::Status, Instant::now() + REQUEST_LIMIT)? {
        LaneReply::Status(status) => Ok(status),
        _ => Err(LaneError::Core(CoreError::Protocol)),
    }
}

/// A server's preview of one action: the token that confirms it, until `expires_at`, and what
/// it affects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub action: LaneAction,
    pub token: String,
    pub expires_at: String,
    pub effects: LaneEffects,
}

/// Asks the server to preview `action`.
pub fn preview(session: &mut Session, action: LaneAction) -> Result<Preview, LaneError> {
    let reply = session.call(
        &LaneRequest::Preview {
            action: action.clone(),
        },
        Instant::now() + REQUEST_LIMIT,
    )?;
    previewed(action, reply)
}

/// [`preview`] that stops waiting as soon as `deadline()` passes, which ends the session.
pub fn preview_within(
    session: &mut Session,
    action: LaneAction,
    deadline: &dyn Fn() -> Instant,
) -> Result<Preview, LaneError> {
    let reply = session.call_within(
        &LaneRequest::Preview {
            action: action.clone(),
        },
        deadline,
    )?;
    previewed(action, reply)
}

fn previewed(action: LaneAction, reply: LaneReply) -> Result<Preview, LaneError> {
    match reply {
        LaneReply::Preview {
            token,
            expires_at,
            effects,
        } => Ok(Preview {
            action,
            token,
            expires_at,
            effects,
        }),
        _ => Err(LaneError::Core(CoreError::Protocol)),
    }
}

/// A server's preview of a Network Access change, which carries its own effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPreview {
    pub access: NetworkAccess,
    pub token: String,
    pub expires_at: String,
    pub effects: LaneNetworkEffects,
}

/// Asks the server to preview a change of its Network Access to `access`.
pub fn preview_network(
    session: &mut Session,
    access: NetworkAccess,
) -> Result<NetworkPreview, LaneError> {
    let reply = session.call(
        &LaneRequest::Preview {
            action: LaneAction::Network { access },
        },
        Instant::now() + REQUEST_LIMIT,
    )?;
    match reply {
        LaneReply::NetworkPreview {
            token,
            expires_at,
            effects,
        } => Ok(NetworkPreview {
            access,
            token,
            expires_at,
            effects,
        }),
        _ => Err(LaneError::Core(CoreError::Protocol)),
    }
}

/// The Core's update facts and the run of the Core that reported them.
pub fn facts(session: &mut Session) -> Result<(String, UpdateFacts), LaneError> {
    let deadline = Instant::now() + REQUEST_LIMIT;
    facts_by(session, deadline)
}

/// [`facts`] that stops waiting at `deadline`, which ends the session.
pub fn facts_by(
    session: &mut Session,
    deadline: Instant,
) -> Result<(String, UpdateFacts), LaneError> {
    let sent = Instant::now();
    match session.call_within(&LaneRequest::Facts {}, &|| {
        deadline.min(sent + REQUEST_LIMIT)
    })? {
        LaneReply::Facts { boot, facts } => Ok((boot, *facts)),
        _ => Err(LaneError::Core(CoreError::Protocol)),
    }
}

/// The Core's detailed update facts and the run of the Core that reported them. None from a
/// Core whose service lane selected a minor below
/// [`crate::update::facts::MINOR_DETAILED_FACTS`]; read [`facts`] and
/// [`crate::update::continuation`] from such a Core.
pub fn detailed_facts(
    session: &mut Session,
) -> Result<Option<crate::update::facts::DetailedUpdateFacts>, LaneError> {
    Ok(detailed_facts_of_run(session)?.map(|(_, detailed)| detailed))
}

/// As [`detailed_facts`], with the run of the Core that reported them.
pub fn detailed_facts_of_run(
    session: &mut Session,
) -> Result<Option<(String, crate::update::facts::DetailedUpdateFacts)>, LaneError> {
    detailed_facts_of_run_by(session, Instant::now() + REQUEST_LIMIT)
}

/// [`detailed_facts`] that stops waiting at `deadline`, which ends the session.
pub fn detailed_facts_by(
    session: &mut Session,
    deadline: Instant,
) -> Result<Option<crate::update::facts::DetailedUpdateFacts>, LaneError> {
    Ok(detailed_facts_of_run_by(session, deadline)?.map(|(_, detailed)| detailed))
}

fn detailed_facts_of_run_by(
    session: &mut Session,
    deadline: Instant,
) -> Result<Option<(String, crate::update::facts::DetailedUpdateFacts)>, LaneError> {
    if session.selected().minor < crate::update::facts::MINOR_DETAILED_FACTS {
        return Ok(None);
    }
    let sent = Instant::now();
    match session.call_within(&LaneRequest::DetailedFacts {}, &|| {
        deadline.min(sent + REQUEST_LIMIT)
    })? {
        LaneReply::DetailedFacts { boot, detailed } => Ok(Some((boot, *detailed))),
        _ => Err(LaneError::Core(CoreError::Protocol)),
    }
}

/// Asks the Core to install the release its own feed names. Returns whether it installed a
/// version now, and its facts after. A Core that never fetches a feed answers `no_update_feed`.
pub fn install_from_feed(session: &mut Session) -> Result<(bool, UpdateFacts), LaneError> {
    installed(session.call(&LaneRequest::StageFetch, Instant::now() + STAGE_LIMIT)?)
}

fn installed(reply: LaneReply) -> Result<(bool, UpdateFacts), LaneError> {
    match reply {
        LaneReply::Installed { installed, facts } => Ok((installed, *facts)),
        _ => Err(LaneError::Core(CoreError::Protocol)),
    }
}

/// A signed SpaceTerm Core archive to push, with what its sender verified about it.
pub struct PushArchive<'a> {
    /// The version and Update Floor its build document names.
    pub candidate: &'a Candidate,
    pub length: u64,
    /// 64 lowercase hex digits.
    pub sha256: &'a str,
    /// The raw 64-byte Ed25519 signature.
    pub signature: &'a [u8],
}

/// Pushes `archive`, whose bytes `bytes` yields in order, and installs it as the Core's Pending
/// Version. `progress` sees the bytes the Core acknowledged. The Core refuses a version it would
/// not install before any byte is sent.
pub fn push(
    session: &mut Session,
    archive: &PushArchive<'_>,
    bytes: &mut dyn Read,
    progress: &mut dyn FnMut(u64),
) -> Result<UpdateFacts, LaneError> {
    let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let reply = session.call(
        &LaneRequest::StageUpload {
            version: archive.candidate.version().to_string(),
            floor: archive.candidate.floor().map(|floor| floor.to_string()),
            length: archive.length,
            sha256: archive.sha256.to_owned(),
            signature: encode(archive.signature),
        },
        Instant::now() + REQUEST_LIMIT,
    )?;
    let LaneReply::UploadProgress { upload, .. } = reply else {
        return Err(LaneError::Core(CoreError::Protocol));
    };
    let mut buffer = vec![0; MAX_CHUNK];
    let mut offset = 0u64;
    progress(0);
    loop {
        let count = read_full(bytes, &mut buffer)
            .map_err(|_| LaneError::Core(CoreError::CandidateMissing))?;
        if count == 0 || offset + count as u64 > archive.length {
            return Err(LaneError::Core(CoreError::CandidateChanged));
        }
        let last = offset + count as u64 == archive.length;
        let reply = session.call(
            &LaneRequest::UploadChunk {
                upload: upload.clone(),
                offset,
                data: encode(&buffer[..count]),
            },
            Instant::now() + if last { STAGE_LIMIT } else { REQUEST_LIMIT },
        )?;
        offset += count as u64;
        progress(offset);
        match reply {
            LaneReply::UploadProgress { next_offset, .. } if next_offset == offset && !last => {}
            reply @ LaneReply::Installed { .. } if last => {
                return installed(reply).map(|(_, facts)| facts);
            }
            _ => return Err(LaneError::Core(CoreError::Protocol)),
        }
    }
}

fn read_full(from: &mut dyn Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match from.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// The client's share of the state its pending record lives in. [`execute`] lets the share go
/// while it waits for a receipt, because a SpaceTerm Core that starts meanwhile, such as the
/// Pending Version a Restart starts, may migrate that state.
pub trait Share {
    /// Lets the state go once the record is written.
    fn release(&mut self);
    /// Shares the state again after the wait, waiting until `deadline` at most for a migration
    /// that holds it alone. False when the state left this build's generation meanwhile, or the
    /// migration still holds it at `deadline`: the record then stays for the client that reads
    /// it.
    fn resume(&mut self, deadline: Instant) -> bool;
}

/// The share of a client that holds no lock on its state, such as the desktop's.
pub struct Unshared;

impl Share for Unshared {
    fn release(&mut self) {}
    fn resume(&mut self, _: Instant) -> bool {
        true
    }
}

/// Executes a confirmed preview of any action and follows it under [`Lane::perform`]. The
/// operation is recorded in `pending` right before its `Execute`, after `dispatch` allows it,
/// and removed once its receipt settled and was acknowledged. `share` is released between the
/// two.
#[allow(
    clippy::too_many_arguments,
    reason = "one execution's parts, used once"
)]
pub fn execute(
    lane: &Lane,
    session: Session,
    action: LaneAction,
    token: String,
    wait: Duration,
    pending: &PendingOperations,
    share: &mut dyn Share,
    dispatch: &mut dyn FnMut() -> Result<(), LaneError>,
    interrupted: &dyn Fn() -> Option<Instant>,
) -> Result<Outcome, LaneError> {
    let id = crate::service::operations::random_operation_id()?;
    let mut journal = Journal::default();
    let (outcome, shared) = execute_as(
        lane,
        session,
        (action, token, id),
        Instant::now() + wait,
        pending,
        &mut journal,
        share,
        (dispatch, &mut || {}),
        interrupted,
    );
    forget_refused(&outcome, id, journal.journaled && shared, pending);
    outcome
}

/// One approval's record in the pending operations: whether it was written, and the claim that
/// keeps another lane connection of this identity from acknowledging its receipt meanwhile.
#[derive(Default)]
struct Journal {
    journaled: bool,
    _following: Option<Lock>,
}

/// One `Execute` attempt of operation `id` under [`Lane::perform_as`]. The operation is recorded
/// in `pending` before its first `Execute` only, after `dispatch` allows each attempt. Its
/// settled receipt's outcome is kept before the receipt is acknowledged, and the operation is
/// removed only once SpaceTerm Core confirmed the acknowledgement. `accepted` runs once the server is
/// known to have admitted it. Also returns whether `share` was shared again afterwards.
#[allow(
    clippy::too_many_arguments,
    reason = "one execution's parts, used once"
)]
fn execute_as(
    lane: &Lane,
    session: Session,
    (action, token, id): (LaneAction, String, OperationId),
    until: Instant,
    pending: &PendingOperations,
    journal: &mut Journal,
    share: &mut dyn Share,
    (dispatch, accepted): (&mut dyn FnMut() -> Result<(), LaneError>, &mut dyn FnMut()),
    interrupted: &dyn Fn() -> Option<Instant>,
) -> (Result<Outcome, LaneError>, bool) {
    // Recording the operation and settling its receipt both write the record and move the share.
    let journal = std::cell::RefCell::new(journal);
    let share = std::cell::RefCell::new(share);
    // Whether the state the record lives in was shared again, once the flow settled its receipt.
    let mut resumed = None;
    let outcome = lane.perform_as(
        session,
        (action, token, id),
        until,
        &mut |operation| {
            dispatch()?;
            let mut journal = journal.borrow_mut();
            if !journal.journaled {
                // A record another caller holds past the deadline dispatches nothing.
                let following = pending.follow(until)?;
                pending
                    .add_by(operation, until)
                    .map_err(|error| match error {
                        LaneError::Deadline => LaneError::Deadline,
                        _ => LaneError::Core(CoreError::StorageUnavailable),
                    })?;
                journal._following = Some(following);
                journal.journaled = true;
            }
            share.borrow_mut().release();
            Ok(())
        },
        (accepted, &mut |session, id, receipt, by| {
            // The record is written only while the state it lives in is shared. Without it,
            // the receipt stays with SpaceTerm Core for the client that reads the record.
            let shared = share.borrow_mut().resume(until);
            resumed = Some(shared);
            if shared && journal.borrow().journaled {
                let _ = settle_receipt(session, pending, id, receipt, by);
            }
        }),
        interrupted,
    );
    let shared = match resumed {
        Some(shared) => shared,
        None => share.borrow_mut().resume(until),
    };
    (outcome, shared)
}

/// Removes the record of operation `id` once its last `Execute` was refused because another
/// operation holds the Core, or was never sent because the deadline passed first. Every attempt
/// under `id` either got that definite refusal, was never sent, or was followed until it
/// answered, so nothing admits `id` later. `journaled` is false when the record was never
/// written or its state left this build's generation.
fn forget_refused(
    outcome: &Result<Outcome, LaneError>,
    id: OperationId,
    journaled: bool,
    pending: &PendingOperations,
) {
    if let Err(error) = outcome
        && journaled
        && (busy(error) || *error == LaneError::Deadline)
    {
        // Past the caller's deadline this only tries: a record left behind settles as gone on a
        // later connection, because nothing admitted it.
        let _ = pending.remove_by(&id.to_string(), Instant::now());
    }
}

/// Whether `error` is a refusal because another service operation holds SpaceTerm Core, which
/// ends when that operation settles. A full ledger or an unresolved activation is not.
pub fn busy(error: &LaneError) -> bool {
    matches!(error, LaneError::Code(code) if code == BUSY)
        || *error == LaneError::Core(CoreError::ServiceBusy)
}

/// Asks the server to preview `action`, waiting until `deadline` while another operation holds
/// it. `waiting(true)` runs when the wait begins, so the client can say so, and `waiting(false)`
/// when the preview it began for ends, however it ends.
pub fn preview_when_ready(
    lane: &Lane,
    session: Session,
    action: LaneAction,
    deadline: Instant,
    interrupted: &dyn Fn() -> Option<Instant>,
    waiting: &mut dyn FnMut(bool),
) -> Result<(Session, Preview), LaneError> {
    let mut wait = BusyWait::new(deadline, waiting);
    let previewed = preview_after_waits(lane, session, action, deadline, interrupted, &mut wait);
    wait.ended();
    previewed
}

fn preview_after_waits(
    lane: &Lane,
    session: Session,
    action: LaneAction,
    deadline: Instant,
    interrupted: &dyn Fn() -> Option<Instant>,
    wait: &mut BusyWait<'_>,
) -> Result<(Session, Preview), LaneError> {
    let mut session = session;
    loop {
        // Each preview stops at the caller's deadline, or at once after an interrupt.
        let sent = Instant::now();
        let by = || {
            interrupted()
                .into_iter()
                .fold(deadline.min(sent + REQUEST_LIMIT), Instant::min)
        };
        match preview_within(&mut session, action.clone(), &by) {
            Ok(preview) => return Ok((session, preview)),
            Err(error) if busy(&error) => {
                session = wait.for_lane(lane, interrupted, error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Executes the person's approved `token` for `action` and follows it as [`execute`] does, for at
/// most `follow` and never past `deadline`. A refusal because another operation holds SpaceTerm
/// Core admits nothing and keeps the token, so the same approval executes again once that
/// operation settles, until `deadline`. When what the action affects changed meanwhile,
/// SpaceTerm Core refuses the token as `changed`, and the person reviews a fresh preview. After
/// an interrupt nothing is executed again. `waiting(true)` runs when a wait begins, and
/// `waiting(false)` once SpaceTerm Core admitted the operation after it, or the approval ended.
#[allow(clippy::too_many_arguments, reason = "one approval's parts, used once")]
pub fn execute_approved(
    lane: &Lane,
    session: Session,
    action: LaneAction,
    token: String,
    deadline: Instant,
    follow: Duration,
    pending: &PendingOperations,
    share: &mut dyn Share,
    dispatch: &mut dyn FnMut() -> Result<(), LaneError>,
    interrupted: &dyn Fn() -> Option<Instant>,
    waiting: &mut dyn FnMut(bool),
) -> Result<Outcome, LaneError> {
    let mut busy_wait = BusyWait::new(deadline, waiting);
    let outcome = execute_after_waits(
        lane,
        session,
        (action, token),
        (deadline, follow),
        pending,
        share,
        dispatch,
        interrupted,
        &mut busy_wait,
    );
    busy_wait.ended();
    outcome
}

#[allow(clippy::too_many_arguments, reason = "one approval's parts, used once")]
fn execute_after_waits(
    lane: &Lane,
    session: Session,
    (action, token): (LaneAction, String),
    (deadline, follow): (Instant, Duration),
    pending: &PendingOperations,
    share: &mut dyn Share,
    dispatch: &mut dyn FnMut() -> Result<(), LaneError>,
    interrupted: &dyn Fn() -> Option<Instant>,
    busy_wait: &mut BusyWait<'_>,
) -> Result<Outcome, LaneError> {
    let mut session = session;
    // One operation ID for the whole approval, journaled once. An attempt whose reply was lost
    // is followed under that ID until it answers, so a later attempt never admits another.
    let id = crate::service::operations::random_operation_id()?;
    let mut journal = Journal::default();
    // The definite refusal of the last attempt, which a deadline that passes before the next
    // attempt reports.
    let mut refused = None;
    // Whether the state the record lives in was shared again after the last attempt.
    let mut shared = true;
    loop {
        // Each attempt follows for `follow` at most and never past the approval's one absolute
        // `deadline`, which every step below reads as an instant.
        let until = deadline.min(Instant::now() + follow);
        if Instant::now() >= until {
            // No attempt is sent once the deadline passed.
            let outcome = Err(refused.unwrap_or(LaneError::Deadline));
            forget_refused(&outcome, id, journal.journaled && shared, pending);
            return outcome;
        }
        let (outcome, resumed) = execute_as(
            lane,
            session,
            (action.clone(), token.clone(), id),
            until,
            pending,
            &mut journal,
            share,
            (&mut *dispatch, &mut || busy_wait.ended()),
            interrupted,
        );
        shared = resumed;
        match outcome {
            Err(error) if busy(&error) => {
                match busy_wait.for_lane(lane, interrupted, error.clone()) {
                    Ok(next) => {
                        session = next;
                        refused = Some(error);
                    }
                    Err(error) => {
                        let outcome = Err(error);
                        forget_refused(&outcome, id, journal.journaled && shared, pending);
                        return outcome;
                    }
                }
            }
            Err(LaneError::Deadline) => {
                // A deadline that passed before this attempt was sent reports the refusal of
                // the attempt before it.
                let outcome = Err(refused.unwrap_or(LaneError::Deadline));
                forget_refused(&outcome, id, journal.journaled && shared, pending);
                return outcome;
            }
            outcome => return outcome,
        }
    }
}

/// One approval's wait for another operation, until its absolute deadline.
struct BusyWait<'a> {
    deadline: Instant,
    /// `true` when a wait begins and `false` when it ends.
    waiting: &'a mut dyn FnMut(bool),
    /// A wait began and has not ended.
    announced: bool,
}

impl<'a> BusyWait<'a> {
    fn new(deadline: Instant, waiting: &'a mut dyn FnMut(bool)) -> Self {
        Self {
            deadline: deadline.min(Instant::now() + BUSY_WAIT),
            waiting,
            announced: false,
        }
    }

    /// The wait that began is over: what it waited for ran or ended.
    fn ended(&mut self) {
        if std::mem::take(&mut self.announced) {
            (self.waiting)(false);
        }
    }

    /// Waits [`BUSY_POLL`] for the operation that refused with `refused`, then opens a new lane
    /// connection. A server that restarts meanwhile answers again later, so a connection that
    /// fails keeps waiting. Once the deadline passes or the caller is interrupted, it returns
    /// `refused`. The dial runs on its own thread, so an interrupt ends the wait at once while
    /// a stalled connection still holds it.
    fn for_lane(
        &mut self,
        lane: &Lane,
        interrupted: &dyn Fn() -> Option<Instant>,
        refused: LaneError,
    ) -> Result<Session, LaneError> {
        if !self.announced {
            self.announced = true;
            (self.waiting)(true);
        }
        let deadline = self.deadline;
        loop {
            if interrupted().is_some() || Instant::now() + BUSY_POLL >= deadline {
                return Err(refused);
            }
            std::thread::sleep(BUSY_POLL);
            if interrupted().is_some() {
                return Err(refused);
            }
            let dialing = lane.clone();
            let dialed = super::client::within(
                "spaceterm-lane-io",
                &|| Some(interrupted().map_or(deadline, |by| by.min(Instant::now()))),
                move || dialing.session(),
            );
            match dialed {
                Some(Ok(session)) => return Ok(session),
                // A stopped SpaceTerm Core answers no lane until the person starts it.
                Some(Err(CoreError::Stopped)) => {
                    return Err(LaneError::Core(CoreError::Stopped));
                }
                Some(Err(_)) => {}
                None => return Err(refused),
            }
        }
    }
}

/// What a finished flow establishes about its action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Done,
    /// The action ran and failed, with the lane code the server recorded, when it recorded one.
    Failed(Option<String>),
    /// The server settled the operation without performing it.
    NotPerformed,
    /// Nothing proves whether the action happened: the receipt is unresolved, or no settled
    /// receipt arrived in time.
    Uncertain,
    /// The server closed the lane after a Stop was admitted, or may have been. The server is
    /// stopping or stopped, and only its own machine can read the outcome.
    Shutdown,
}

/// What a settled receipt establishes.
pub fn receipt_verdict(receipt: &LaneReceipt) -> Verdict {
    match receipt.state {
        LaneOperationState::Done => Verdict::Done,
        LaneOperationState::Failed => Verdict::Failed(receipt.code.clone()),
        LaneOperationState::NotPerformed => Verdict::NotPerformed,
        _ => Verdict::Uncertain,
    }
}

/// What a finished flow establishes.
pub fn verdict(outcome: &Outcome) -> Verdict {
    match outcome {
        Outcome::Settled { receipt, .. } => receipt_verdict(receipt),
        Outcome::Shutdown { .. } => Verdict::Shutdown,
        Outcome::Unconfirmed { .. } => Verdict::Uncertain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(state: LaneOperationState, code: Option<&str>) -> LaneReceipt {
        LaneReceipt {
            operation: "00".into(),
            action: LaneAction::Restart,
            state,
            code: code.map(str::to_owned),
            boot: "b".into(),
        }
    }

    #[test]
    fn only_a_settled_receipt_proves_what_happened() {
        let settled = |state, code| Outcome::Settled {
            receipt: receipt(state, code),
            boot: "a".into(),
        };
        assert_eq!(
            verdict(&settled(LaneOperationState::Done, None)),
            Verdict::Done
        );
        assert_eq!(
            verdict(&settled(LaneOperationState::Failed, Some("startup_failed"))),
            Verdict::Failed(Some("startup_failed".into()))
        );
        assert_eq!(
            verdict(&settled(LaneOperationState::NotPerformed, None)),
            Verdict::NotPerformed
        );
        assert_eq!(
            verdict(&settled(LaneOperationState::Unresolved, None)),
            Verdict::Uncertain
        );
        assert_eq!(
            verdict(&Outcome::Shutdown {
                operation: "00".into(),
                boot: String::new(),
            }),
            Verdict::Shutdown
        );
        assert_eq!(
            verdict(&Outcome::Unconfirmed {
                operation: "00".into(),
                boot: "a".into(),
                last: Some(receipt(LaneOperationState::Running, None)),
            }),
            Verdict::Uncertain
        );
    }

    /// A new action needs no change here: only the actions that end the lane and Network Access
    /// changes are followed for a shorter wait.
    #[test]
    fn only_stop_uninstall_and_network_access_are_followed_for_a_shorter_wait() {
        assert_eq!(wait(&LaneAction::Stop), STOP_WAIT);
        assert_eq!(wait(&LaneAction::Uninstall), STOP_WAIT);
        assert_eq!(
            wait(&LaneAction::Network {
                access: NetworkAccess::All
            }),
            NETWORK_WAIT
        );
        // A change settles within the server's listener limit, and the caller then reconnects.
        assert!(NETWORK_WAIT >= super::super::network::LISTEN_LIMIT + Duration::from_secs(10));
        assert_eq!(wait(&LaneAction::Restart), RETURN_WAIT);
    }

    #[test]
    fn the_pending_record_keeps_at_most_sixteen_operations_and_forgets_removed_ones() {
        // The record refuses a path through a symbolic link, as macOS's temporary directory is.
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "spaceterm-pending-{}",
            crate::service::operations::random_operation_id().unwrap()
        ));
        let pending = PendingOperations::new(Directory::open(&root, true).unwrap());
        let ids: Vec<_> = (0..MAX_PENDING)
            .map(|_| crate::service::operations::random_operation_id().unwrap())
            .collect();
        for id in &ids {
            pending.add(id).unwrap();
        }
        assert_eq!(
            pending.add(&crate::service::operations::random_operation_id().unwrap()),
            Err(CoreError::Capacity)
        );
        pending.remove(&ids[0].to_string()).unwrap();
        let listed = pending.list().unwrap();
        assert_eq!(listed.len(), MAX_PENDING - 1);
        assert!(!listed.contains(&ids[0].to_string()));
        let _ = std::fs::remove_dir_all(root);
    }
}
