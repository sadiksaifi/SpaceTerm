//! The administration steps every SpaceTerm Client shares over a server's service lane: settle
//! the operations this identity left pending, read Status and the update facts, install a new
//! version from the Core's feed or by pushing an archive, preview Stop, Restart or a Network
//! Access change, and execute a confirmed preview until it settles.
//!
//! The CLI and the desktop present each result their own way. Both take the same steps here, so
//! a confirmation, a dispatched operation and its settlement mean the same on either.
use super::client::{Lane, LaneError, Leftover, Outcome, Session, settle_leftover};
use super::network::{LaneNetworkEffects, NetworkAccess};
use super::{
    LaneAction, LaneEffects, LaneOperationState, LaneReceipt, LaneReply, LaneRequest, LaneStatus,
    MAX_CHUNK,
};
use crate::CoreError;
use crate::service::operations::OperationId;
use crate::storage::Directory;
use crate::update::facts::{UpdateFacts, Version};
use base64::Engine;
use std::io::Read;
use std::time::{Duration, Instant};

/// How long a read or a preview may take.
pub const REQUEST_LIMIT: Duration = Duration::from_secs(30);
/// How long the server may take to fetch, verify and install a release from its feed, or to
/// verify and install the last chunk of a push.
pub const STAGE_LIMIT: Duration = Duration::from_secs(30 * 60);
/// How long a Stop is followed: until the server closes the lane.
pub const STOP_WAIT: Duration = Duration::from_secs(90);
/// How long any other action is followed: until it settles, or until a server that replaces
/// itself, as Restart and Update do, returns.
pub const RETURN_WAIT: Duration = Duration::from_secs(180);
/// How long a Network Access change is followed. It covers the server's own
/// [`super::network::LISTEN_LIMIT`] for the new listener, and a reconnect after it. One the new
/// listener no longer serves never reconnects, so waiting as long as for a server that replaces
/// itself only delays an unconfirmed outcome.
pub const NETWORK_WAIT: Duration = Duration::from_secs(30);

/// How long an action is followed after its `Execute`. Only Stop ends the lane it is followed
/// over, so every other action, including one added later, is followed until it settles.
pub fn wait(action: &LaneAction) -> Duration {
    match action {
        LaneAction::Stop => STOP_WAIT,
        LaneAction::Network { .. } => NETWORK_WAIT,
        _ => RETURN_WAIT,
    }
}

/// The record of one route and identity's admitted operations, kept in `directory`.
const PENDING: &str = "service-operations.json";
const PENDING_FILE: &std::ffi::CStr = c"service-operations.json";
/// At most this many pending operations are kept. The server holds no more than 16.
const MAX_PENDING: usize = 16;

/// Whether `value` is a client's `service-operations.json` this build reads.
pub(crate) fn readable(value: &serde_json::Value) -> bool {
    crate::migration::strict::<Vec<String>>(value)
}

/// The service operations one client identity admitted on one route and has not acknowledged,
/// so the next lane connection settles them.
pub struct PendingOperations {
    directory: Directory,
}

impl PendingOperations {
    /// The record in `directory`, which belongs to exactly one route and Client Identity.
    pub fn new(directory: Directory) -> Self {
        Self { directory }
    }

    fn change(
        &self,
        apply: impl FnOnce(&mut Vec<String>) -> Result<(), CoreError>,
    ) -> Result<Vec<String>, CoreError> {
        let _lock = self.directory.lock("service-operations.lock", false)?;
        let mut pending: Vec<String> = self.directory.read_json(PENDING)?.unwrap_or_default();
        let before = pending.clone();
        apply(&mut pending)?;
        if pending != before {
            self.directory.write_json(PENDING_FILE, &pending)?;
        }
        Ok(pending)
    }

    pub fn list(&self) -> Result<Vec<String>, CoreError> {
        self.change(|_| Ok(()))
    }

    /// Records an operation right before its `Execute` is sent.
    pub fn add(&self, operation: &OperationId) -> Result<(), CoreError> {
        self.change(|pending| {
            if pending.len() >= MAX_PENDING {
                return Err(CoreError::Capacity);
            }
            pending.push(operation.to_string());
            Ok(())
        })
        .map(drop)
    }

    pub fn remove(&self, operation: &str) -> Result<(), CoreError> {
        self.change(|pending| {
            pending.retain(|v| v != operation);
            Ok(())
        })
        .map(drop)
    }
}

/// Opens one lane connection and settles what earlier commands of this identity left on it.
/// Returns the receipts it settled and acknowledged. An operation that has not settled stays
/// recorded for a later connection.
pub fn open(
    lane: &Lane,
    pending: &PendingOperations,
) -> Result<(Session, Vec<LaneReceipt>), LaneError> {
    let mut session = lane.session()?;
    let mut settled = Vec::new();
    for operation in pending.list()? {
        let Some(id) = OperationId::parse(&operation) else {
            pending.remove(&operation)?;
            continue;
        };
        match settle_leftover(&mut session, &id)? {
            Leftover::Settled(receipt) => {
                settled.push(receipt);
                pending.remove(&operation)?;
            }
            Leftover::Gone => pending.remove(&operation)?,
            Leftover::Pending => {}
        }
    }
    Ok((session, settled))
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
    match session.call(&LaneRequest::Facts {}, Instant::now() + REQUEST_LIMIT)? {
        LaneReply::Facts { boot, facts } => Ok((boot, *facts)),
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
    /// The version its build document names.
    pub version: &'a Version,
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
            version: archive.version.to_string(),
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

/// Executes a confirmed preview of any action and follows it under [`Lane::perform`]. The
/// operation is recorded in `pending` right before its `Execute`, after `dispatch` allows it,
/// and removed once its receipt settled and was acknowledged.
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
    dispatch: &mut dyn FnMut() -> Result<(), LaneError>,
    interrupted: &dyn Fn() -> Option<Instant>,
) -> Result<Outcome, LaneError> {
    let outcome = lane.perform(
        session,
        action,
        token,
        wait,
        &mut |operation| {
            dispatch()?;
            pending
                .add(operation)
                .map_err(|_| LaneError::Core(CoreError::StorageUnavailable))
        },
        interrupted,
    )?;
    if let Outcome::Settled { receipt, .. } = &outcome {
        let _ = pending.remove(&receipt.operation);
    }
    Ok(outcome)
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

    /// A new action needs no change here: only Stop and Network Access changes are followed
    /// for a shorter wait.
    #[test]
    fn only_stop_and_network_access_are_followed_for_a_shorter_wait() {
        assert_eq!(wait(&LaneAction::Stop), STOP_WAIT);
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
            OperationId::random().unwrap()
        ));
        let pending = PendingOperations::new(Directory::open(&root, true).unwrap());
        let ids: Vec<_> = (0..MAX_PENDING)
            .map(|_| OperationId::random().unwrap())
            .collect();
        for id in &ids {
            pending.add(id).unwrap();
        }
        assert_eq!(
            pending.add(&OperationId::random().unwrap()),
            Err(CoreError::Capacity)
        );
        pending.remove(&ids[0].to_string()).unwrap();
        let listed = pending.list().unwrap();
        assert_eq!(listed.len(), MAX_PENDING - 1);
        assert!(!listed.contains(&ids[0].to_string()));
        let _ = std::fs::remove_dir_all(root);
    }
}
