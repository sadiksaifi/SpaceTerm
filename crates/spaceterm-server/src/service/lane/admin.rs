//! The administration steps every SpaceTerm Client shares over a server's service lane: settle
//! the operations this identity left pending, read Status and the update check, preview Stop,
//! Restart, Update or Return, and execute a confirmed preview until it settles.
//!
//! The CLI and the desktop present each result their own way. Both take the same steps here, so
//! a confirmation, a dispatched operation and its settlement mean the same on either.
use super::client::{Lane, LaneError, Leftover, Outcome, Session, settle_leftover};
use super::{
    LaneAction, LaneCheck, LaneEffects, LaneOperationState, LaneReceipt, LaneReply, LaneRequest,
    LaneStatus, LaneUpdateKind, LaneVersion,
};
use crate::ServerError;
use crate::service::operations::OperationId;
use crate::storage::Directory;
use std::time::{Duration, Instant};

/// How long a read or a preview may take.
pub const REQUEST_LIMIT: Duration = Duration::from_secs(30);
/// How long the server may take to fetch and verify a candidate it stages.
pub const STAGE_LIMIT: Duration = Duration::from_secs(30 * 60);
/// How long a Stop is followed: until the server closes the lane.
pub const STOP_WAIT: Duration = Duration::from_secs(90);
/// How long any other action is followed: until it settles, or until a server that replaces
/// itself, as Restart, Update and Return do, returns.
pub const RETURN_WAIT: Duration = Duration::from_secs(180);

/// How long an action is followed after its `Execute`. Only Stop ends the lane it is followed
/// over, so every other action, including one added later, is followed until it settles.
pub fn wait(action: &LaneAction) -> Duration {
    match action {
        LaneAction::Stop => STOP_WAIT,
        _ => RETURN_WAIT,
    }
}

/// The record of one route and identity's admitted operations, kept in `directory`.
const PENDING: &str = "service-operations.json";
const PENDING_FILE: &std::ffi::CStr = c"service-operations.json";
/// At most this many pending operations are kept. The server holds no more than 16.
const MAX_PENDING: usize = 16;

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
        apply: impl FnOnce(&mut Vec<String>) -> Result<(), ServerError>,
    ) -> Result<Vec<String>, ServerError> {
        let _lock = self.directory.lock("service-operations.lock", false)?;
        let mut pending: Vec<String> = self.directory.read_json(PENDING)?.unwrap_or_default();
        let before = pending.clone();
        apply(&mut pending)?;
        if pending != before {
            self.directory.write_json(PENDING_FILE, &pending)?;
        }
        Ok(pending)
    }

    pub fn list(&self) -> Result<Vec<String>, ServerError> {
        self.change(|_| Ok(()))
    }

    /// Records an operation right before its `Execute` is sent.
    pub fn add(&self, operation: &OperationId) -> Result<(), ServerError> {
        self.change(|pending| {
            if pending.len() >= MAX_PENDING {
                return Err(ServerError::Capacity);
            }
            pending.push(operation.to_string());
            Ok(())
        })
        .map(drop)
    }

    pub fn remove(&self, operation: &str) -> Result<(), ServerError> {
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
        _ => Err(LaneError::Server(ServerError::Protocol)),
    }
}

/// The server's approved build, the newest signed release for it, and what it has staged. The
/// check is passive: it stages and recovers nothing.
pub fn check(session: &mut Session) -> Result<LaneCheck, LaneError> {
    match session.call(&LaneRequest::Check, Instant::now() + REQUEST_LIMIT)? {
        LaneReply::Check(check) => Ok(check),
        _ => Err(LaneError::Server(ServerError::Protocol)),
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
        _ => Err(LaneError::Server(ServerError::Protocol)),
    }
}

/// The return to the previous runtime the server offers, when its settled state allows one.
pub fn probe_return(session: &mut Session) -> Result<Option<Preview>, LaneError> {
    offered_return(session.request(
        &LaneRequest::Preview {
            action: LaneAction::Return,
        },
        Instant::now() + REQUEST_LIMIT,
    ))
}

/// The return a server offers, from its reply to `Preview { Return }`. `return_unavailable`
/// means no return applies, so an update proceeds.
pub fn offered_return(reply: Result<LaneReply, ServerError>) -> Result<Option<Preview>, LaneError> {
    match reply? {
        LaneReply::Preview {
            token,
            expires_at,
            effects,
        } if effects.kind == Some(LaneUpdateKind::Return) => Ok(Some(Preview {
            action: LaneAction::Return,
            token,
            expires_at,
            effects,
        })),
        LaneReply::Error { code } if code == "return_unavailable" => Ok(None),
        LaneReply::Error { code } => Err(LaneError::Code(code)),
        _ => Err(LaneError::Server(ServerError::Protocol)),
    }
}

/// Asks the server to fetch, verify and stage the newest signed release for itself.
pub fn stage_fetch(session: &mut Session) -> Result<LaneVersion, LaneError> {
    let reply = session.call(&LaneRequest::StageFetch, Instant::now() + STAGE_LIMIT)?;
    staged(reply)
}

/// The candidate a `Staged` reply names.
pub fn staged(reply: LaneReply) -> Result<LaneVersion, LaneError> {
    match reply {
        LaneReply::Staged {
            installation,
            version,
            commit,
        } => Ok(LaneVersion {
            version,
            commit,
            sha256: installation,
        }),
        _ => Err(LaneError::Server(ServerError::Protocol)),
    }
}

/// The change Update offers a running server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdatePlan {
    /// The settled state allows return to the previous runtime, which is offered instead.
    Return(Preview),
    /// The server staged `candidate` from its signed release feed.
    Update {
        candidate: LaneVersion,
        preview: Preview,
    },
}

/// Prepares Update the way `spaceterm server update` does without local files: a return the
/// server offers comes first, otherwise the server stages its newest release and previews it.
pub fn plan_update(session: &mut Session) -> Result<UpdatePlan, LaneError> {
    if let Some(preview) = probe_return(session)? {
        return Ok(UpdatePlan::Return(preview));
    }
    let candidate = stage_fetch(session)?;
    let preview = preview(
        session,
        LaneAction::Update {
            installation: candidate.sha256.clone(),
        },
    )?;
    Ok(UpdatePlan::Update { candidate, preview })
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
                .map_err(|_| LaneError::Server(ServerError::StorageUnavailable))
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

    fn effects(kind: LaneUpdateKind) -> LaneEffects {
        LaneEffects {
            ends_sessions: 0,
            sessions: Vec::new(),
            start_requires_machine: false,
            from: None,
            to: None,
            kind: Some(kind),
            to_major: None,
        }
    }

    #[test]
    fn a_running_server_offers_return_only_when_its_preview_is_a_return() {
        let preview = |kind| {
            Ok(LaneReply::Preview {
                token: "c1-00".into(),
                expires_at: "2026-10-30T12:10:00Z".into(),
                effects: effects(kind),
            })
        };
        assert!(matches!(
            offered_return(preview(LaneUpdateKind::Return)),
            Ok(Some(Preview { action: LaneAction::Return, token, .. })) if token == "c1-00"
        ));
        assert_eq!(
            offered_return(Ok(LaneReply::Error {
                code: "return_unavailable".into()
            })),
            Ok(None)
        );
        assert_eq!(
            offered_return(Ok(LaneReply::Error {
                code: "not_installed".into()
            })),
            Err(LaneError::Code("not_installed".into()))
        );
        assert_eq!(
            offered_return(preview(LaneUpdateKind::Update)),
            Err(LaneError::Server(ServerError::Protocol))
        );
    }

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

    /// A new action needs no change here: only Stop is followed for the shorter wait.
    #[test]
    fn only_stop_is_followed_for_the_shorter_wait() {
        assert_eq!(wait(&LaneAction::Stop), STOP_WAIT);
        for action in [
            LaneAction::Restart,
            LaneAction::Return,
            LaneAction::Update {
                installation: "00".into(),
            },
        ] {
            assert_eq!(wait(&action), RETURN_WAIT, "{action:?}");
        }
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
            Err(ServerError::Capacity)
        );
        pending.remove(&ids[0].to_string()).unwrap();
        let listed = pending.list().unwrap();
        assert_eq!(listed.len(), MAX_PENDING - 1);
        assert!(!listed.contains(&ids[0].to_string()));
        let _ = std::fs::remove_dir_all(root);
    }
}
