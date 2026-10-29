//! Workspace edits and their leftovers (section 9.2 and 9.3 of the CLI spec).
//!
//! [`mutate`] runs one edit under the identity's writer lock: it records the edit with the
//! sequence the server allocated, dispatches it, waits for its outcome, retires it and
//! acknowledges it. The acknowledgement is confirmed before the journal forgets the outcome, so
//! an identity this CLI stops using never keeps an entry in the server's shared outcome table.
//!
//! [`settle`] runs after admission and before a command's own work. It resolves what an earlier
//! command of any identity on the same route left behind: an edit whose reply was lost, an
//! outcome whose acknowledgement may not have arrived, and run receipts. Nothing is dispatched
//! again: a pending edit is only resolved, and a run only asked about.
use crate::contract::{
    Code, Failure, RecoveredIdentity, RecoveredKind, RecoveredOperation, RecoveredOutcome,
    RecoveredState, Warning, WarningCode, WarningDetails, from_outcome, from_server_error,
};
use crate::state::{CliState, RouteKey, Runs, WriterGuard};
use spaceterm_server::connection::link::{RequestError, ServerLink};
use spaceterm_server::hierarchy::{ClientJournal, Mutation, Outcome, RetainedOutcome};
use spaceterm_server::pane::Refusal;
use spaceterm_server::protocol::{AgentName, Operation, ReplyBody, RunReply, RunState};
use spaceterm_server::{ServerConnection, ServerError};
use std::time::Instant;

/// Runs `mutation` as this identity's next edit and returns its validated outcome. The caller
/// maps the outcome with [`from_outcome`].
///
/// A reply that does not arrive by `deadline` leaves the edit in the journal and fails with
/// `outcome_unconfirmed`. The next command of any identity on this route resolves it.
pub fn mutate(
    writer: &mut WriterGuard,
    connection: &ServerConnection,
    link: &ServerLink,
    mutation: Mutation,
    deadline: Instant,
) -> Result<RetainedOutcome, Failure> {
    let journal = writer.journal(connection)?;
    // Settlement ran first, so a pending edit here is one the server could not resolve yet.
    if journal.pending().is_some() {
        return Err(Failure::new(Code::OutcomeUnconfirmed));
    }
    let (_, revision) = link
        .next_inventory(0)
        .map_err(|error| from_server_error(error.into()))?;
    let sequence = revision.next_sequence;
    journal
        .submit(sequence, mutation.clone())
        .map_err(from_server_error)?;
    let reply = request(link, Operation::Mutate { sequence, mutation }, deadline);
    let outcome = match reply {
        Some(Ok(ReplyBody::Outcome(outcome))) => outcome,
        Some(Ok(_)) => return Err(Failure::new(Code::ProtocolError)),
        Some(Err(refusal)) => {
            // A refused edit may still have been recorded before an uncertain write. Only the
            // recorded outcome, or its absence, decides.
            return match resolve(journal, link, sequence, deadline)? {
                Resolved::Outcome(outcome) => Ok(outcome),
                Resolved::Missing => Err(from_server_error(refusal.into())),
                Resolved::Unknown => Err(Failure::new(Code::OutcomeUnconfirmed)),
            };
        }
        None => return Err(Failure::new(Code::OutcomeUnconfirmed)),
    };
    finish(journal, link, outcome, deadline)
}

/// What the server recorded for a pending sequence.
enum Resolved {
    Outcome(RetainedOutcome),
    /// The server recorded no such sequence, so it never applied it.
    Missing,
    /// The outcome waits for a Worktree write, or the server did not answer in time.
    Unknown,
}

fn resolve(
    journal: &mut ClientJournal,
    link: &ServerLink,
    sequence: u64,
    deadline: Instant,
) -> Result<Resolved, Failure> {
    match request(link, Operation::Resolve { sequence }, deadline) {
        Some(Ok(ReplyBody::Outcome(outcome))) => {
            Ok(Resolved::Outcome(finish(journal, link, outcome, deadline)?))
        }
        Some(Ok(_)) => Err(Failure::new(Code::ProtocolError)),
        Some(Err(Refusal::Missing)) => {
            journal.abandon().map_err(from_server_error)?;
            Ok(Resolved::Missing)
        }
        Some(Err(_)) | None => Ok(Resolved::Unknown),
    }
}

/// Retires a validated outcome, acknowledges it, and forgets it once the server confirms that
/// it forgot it too. An unconfirmed acknowledgement keeps the outcome for the next command.
fn finish(
    journal: &mut ClientJournal,
    link: &ServerLink,
    outcome: RetainedOutcome,
    deadline: Instant,
) -> Result<RetainedOutcome, Failure> {
    let pending = journal.pending().ok_or(Failure::new(Code::ProtocolError))?;
    // Validation precedes acknowledgement.
    if outcome.sequence != pending.sequence || outcome.mutation != pending.mutation {
        return Err(Failure::new(Code::ProtocolError));
    }
    if outcome.outcome == Outcome::Pending {
        return Ok(outcome);
    }
    journal.retire(outcome.clone()).map_err(from_server_error)?;
    acknowledge(journal, link, outcome.sequence, deadline)?;
    Ok(outcome)
}

/// Acknowledges `sequence`, then asks for it again: requests on one connection are answered in
/// order, so `Missing` proves the server applied the acknowledgement.
fn acknowledge(
    journal: &mut ClientJournal,
    link: &ServerLink,
    sequence: u64,
    deadline: Instant,
) -> Result<(), Failure> {
    if link.acknowledge(sequence).is_err() {
        return Ok(());
    }
    if let Some(Err(Refusal::Missing)) = request(link, Operation::Resolve { sequence }, deadline) {
        journal.settled(sequence).map_err(from_server_error)?;
    }
    Ok(())
}

/// Sends `operation` and waits for its reply until `deadline`. `None` means no reply arrived:
/// the deadline passed or the link ended.
fn request(
    link: &ServerLink,
    operation: Operation,
    deadline: Instant,
) -> Option<Result<ReplyBody, Refusal>> {
    let pending = link.request(operation).ok()?;
    match pending.wait_for(deadline.saturating_duration_since(Instant::now()))? {
        Ok(reply) => Some(Ok(reply)),
        Err(RequestError::Refused(refusal)) => Some(Err(refusal)),
        Err(RequestError::Link(_)) => None,
    }
}

/// The identity a command runs as, with its admitted link. A mutating command passes its
/// writer, so settlement uses the lock it already holds.
pub struct Current<'a> {
    pub agent: Option<&'a AgentName>,
    pub connection: &'a ServerConnection,
    pub link: &'a ServerLink,
    pub writer: Option<&'a mut WriterGuard>,
}

/// Settles the leftovers of every identity this CLI holds for `route`, after admission and
/// before the command's own work. Identities other than the current one connect through
/// `connect`, each with its own admission. An identity whose lock another process holds is
/// skipped, because that process owns its leftovers.
///
/// Returns every settled item for `data.recovered_operations`. An identity that cannot connect
/// keeps its leftovers for a later command.
pub fn settle(
    state: &CliState,
    route: &RouteKey,
    current: Current<'_>,
    connect: &mut dyn FnMut(Option<&AgentName>) -> Result<ServerConnection, ServerError>,
    deadline: Instant,
) -> Result<Vec<RecoveredOperation>, Failure> {
    let Current {
        agent,
        connection,
        link,
        writer,
    } = current;
    let mut recovered = Vec::new();
    let held = state.held(route).map_err(from_server_error)?;
    let mut writer = writer;
    for identity in held {
        let name = identity.agent();
        let label = identity.identity.clone();
        if name == agent {
            let mut own;
            let writer = match writer.as_deref_mut() {
                Some(writer) => Some(writer),
                None => {
                    own = state.try_writer(route, name).map_err(Failure::new)?;
                    own.as_mut()
                }
            };
            settle_identity(
                state, route, name, label, connection, link, writer, deadline,
            )
            .map(|mut items| recovered.append(&mut items))?;
            continue;
        }
        let Some(mut other) = state.try_writer(route, name).map_err(Failure::new)? else {
            continue;
        };
        let Ok(handle) = connect(name) else {
            continue;
        };
        let Ok(other_link) = handle.link() else {
            continue;
        };
        let items = settle_identity(
            state,
            route,
            name,
            label,
            &handle,
            &other_link,
            Some(&mut other),
            deadline,
        );
        other_link.close();
        recovered.append(&mut items?);
    }
    Ok(recovered)
}

/// The warning a command adds when it settled anything.
pub fn recovered_warning(recovered: &[RecoveredOperation]) -> Option<Warning> {
    (!recovered.is_empty()).then(|| {
        Warning::new(
            WarningCode::RecoveredOperation,
            WarningDetails::Recovered {
                count: recovered.len().min(u32::MAX as usize) as u32,
            },
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn settle_identity(
    state: &CliState,
    route: &RouteKey,
    agent: Option<&AgentName>,
    identity: RecoveredIdentity,
    connection: &ServerConnection,
    link: &ServerLink,
    writer: Option<&mut WriterGuard>,
    deadline: Instant,
) -> Result<Vec<RecoveredOperation>, Failure> {
    let mut recovered = Vec::new();
    if let Some(writer) = writer {
        let journal = writer.journal(connection)?;
        if let Some(item) = settle_journal(journal, link, deadline)? {
            recovered.push(RecoveredOperation {
                identity: identity.clone(),
                ..item
            });
        }
    }
    // `runs.lock` is held only briefly by a run that records or forgets itself, so this waits
    // for it like a writer, bounded by the command's deadline.
    if let Ok(mut runs) = state.runs(route, agent, deadline) {
        for item in settle_runs(&mut runs, link, deadline)? {
            recovered.push(RecoveredOperation {
                identity: identity.clone(),
                ..item
            });
        }
    }
    Ok(recovered)
}

fn settle_journal(
    journal: &mut ClientJournal,
    link: &ServerLink,
    deadline: Instant,
) -> Result<Option<RecoveredOperation>, Failure> {
    if let Some(pending) = journal.pending().cloned() {
        let outcome = match resolve(journal, link, pending.sequence, deadline)? {
            Resolved::Outcome(outcome) if outcome.outcome == Outcome::Pending => {
                RecoveredOutcome::unknown()
            }
            Resolved::Outcome(outcome) => recovered_outcome(&outcome.outcome),
            Resolved::Missing => RecoveredOutcome {
                state: RecoveredState::Refused,
                code: None,
                target: None,
            },
            Resolved::Unknown => RecoveredOutcome::unknown(),
        };
        return Ok(Some(mutation_item(pending.sequence, outcome)));
    }
    let Some(last) = journal.last().cloned() else {
        return Ok(None);
    };
    // The outcome was retired but its acknowledgement may not have arrived. The command that
    // dispatched it may not have reported it either.
    acknowledge(journal, link, last.sequence, deadline)?;
    Ok(Some(mutation_item(
        last.sequence,
        recovered_outcome(&last.outcome),
    )))
}

fn settle_runs(
    runs: &mut Runs,
    link: &ServerLink,
    deadline: Instant,
) -> Result<Vec<RecoveredOperation>, Failure> {
    let mut recovered = Vec::new();
    for pending in runs.pending().to_vec() {
        let operation = hex(&pending.run.0);
        let outcome = match request(link, Operation::RunStatus { run: pending.run }, deadline) {
            Some(Ok(ReplyBody::Run(RunReply::Receipt(receipt))))
                if receipt.run == pending.run && settled(receipt.state) =>
            {
                if !matches!(
                    request(
                        link,
                        Operation::RunAcknowledge { run: pending.run },
                        deadline
                    ),
                    Some(Ok(_))
                ) {
                    continue;
                }
                run_outcome(receipt.state)
            }
            // The server keeps no receipt for it: it never admitted the run, or forgot it.
            Some(Err(Refusal::Missing)) => RecoveredOutcome::unknown(),
            // Still running, unsupported, or no answer: a later command asks again.
            _ => continue,
        };
        runs.remove(pending.run)?;
        recovered.push(RecoveredOperation {
            operation,
            identity: RecoveredIdentity::Base,
            kind: RecoveredKind::Run,
            outcome,
        });
    }
    Ok(recovered)
}

fn settled(state: RunState) -> bool {
    !matches!(state, RunState::Admitted | RunState::Running)
}

/// A settled run's receipt. Any bound exit status means the command ran.
fn run_outcome(state: RunState) -> RecoveredOutcome {
    let (state, code) = match state {
        RunState::Finished {
            exit_status: Some(status),
            ..
        } => (
            RecoveredState::Applied,
            (status != 0).then_some(Code::CommandFailed),
        ),
        RunState::SessionEnded => (RecoveredState::Unknown, Some(Code::SessionEnded)),
        RunState::TakenOver => (RecoveredState::Unknown, Some(Code::Controlled)),
        RunState::Finished {
            exit_status: None, ..
        }
        | RunState::Unconfirmed(_)
        | RunState::Admitted
        | RunState::Running => (RecoveredState::Unknown, Some(Code::RunUnconfirmed)),
    };
    RecoveredOutcome {
        state,
        code,
        target: None,
    }
}

fn recovered_outcome(outcome: &Outcome) -> RecoveredOutcome {
    match outcome {
        Outcome::Accepted { target } => RecoveredOutcome {
            state: RecoveredState::Applied,
            code: None,
            target: Some(hex(target)),
        },
        Outcome::Pending => RecoveredOutcome::unknown(),
        refused => RecoveredOutcome {
            state: RecoveredState::Refused,
            code: from_outcome(refused).err().map(|failure| failure.code),
            target: None,
        },
    }
}

fn mutation_item(sequence: u64, outcome: RecoveredOutcome) -> RecoveredOperation {
    RecoveredOperation {
        operation: sequence.to_string(),
        identity: RecoveredIdentity::Base,
        kind: RecoveredKind::Mutation,
        outcome,
    }
}

impl RecoveredOutcome {
    fn unknown() -> Self {
        Self {
            state: RecoveredState::Unknown,
            code: None,
            target: None,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
