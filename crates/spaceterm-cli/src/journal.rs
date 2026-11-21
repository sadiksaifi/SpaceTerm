//! Workspace edits and their leftovers (section 9.2 and 9.3 of the CLI spec).
//!
//! A mutating command opens an [`Editor`]. It takes the identity's writer lock once, settles
//! leftovers, and then runs each edit under that lock: it records the edit with the sequence the
//! server allocated, dispatches it, waits for its outcome, retires it and acknowledges it. The
//! acknowledgement is confirmed before the journal forgets the outcome, so an identity this CLI
//! stops using never keeps an entry in the server's shared outcome table.
//!
//! ```ignore
//! let mut editor = Editor::open(&state, RouteKey::Local, agent.as_ref(), &connection, &link,
//!     &mut |agent| MachineConnection::for_cli(identity, CliRoute::Local, agent.cloned()), deadline)?;
//! let applied = editor.mutate(Mutation::CreateTab { workspace, source, control }, deadline)?;
//! // applied.target is the new Tab; editor.recovered() fills data.recovered_operations.
//! ```
//!
//! A passive command calls [`settle`] instead. It runs after admission and before a command's
//! own work. It resolves what an earlier
//! command of any identity on the same route left behind: an edit whose reply was lost, an
//! outcome whose acknowledgement may not have arrived, and run receipts. Nothing is dispatched
//! again: a pending edit is only resolved, and a run only asked about.
use crate::contract::{
    Code, Failure, RecoveredIdentity, RecoveredKind, RecoveredOperation, RecoveredOutcome,
    RecoveredState, Warning, WarningCode, WarningDetails, from_core_error, from_outcome,
};
use crate::io::run::settled;
use crate::state::{CliState, RouteKey, Runs, WriterGuard};
use spaceterm_client::workspace::{
    EditError, EditProgress, confirm_acknowledgement, dispatch_original, resolve_original,
};
use spaceterm_core::connection::link::{CoreLink, RequestError};
use spaceterm_core::hierarchy::{ClientJournal, Mutation, Outcome, RetainedOutcome};
use spaceterm_core::pane::Refusal;
use spaceterm_core::protocol::{AgentName, Operation, ReplyBody, RunReply, RunState};
use spaceterm_core::{CoreError, MachineConnection};
use std::time::Instant;

/// One identity's Workspace edits on one server. It holds the identity's writer lock from
/// before settlement until it drops, so every sequence it allocates is retired and acknowledged
/// under that one lock.
pub struct Editor<'a> {
    connection: &'a MachineConnection,
    link: &'a CoreLink,
    writer: WriterGuard,
    recovered: Vec<RecoveredOperation>,
}

/// An edit the server accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// What the edit created or changed, as `Outcome::Accepted` names it.
    pub target: spaceterm_core::hierarchy::Id,
    pub outcome: RetainedOutcome,
}

impl<'a> Editor<'a> {
    /// Takes the writer lock of `agent`, or of the base identity, on `route`, waiting until
    /// `deadline` before it fails with `identity_busy`. Then settles the leftovers of every
    /// identity this CLI holds on `route`: `connection` and `link` serve this identity, and
    /// `connect` opens a handle for each other one.
    pub fn open(
        state: &CliState,
        route: RouteKey,
        agent: Option<&AgentName>,
        connection: &'a MachineConnection,
        link: &'a CoreLink,
        connect: &mut dyn FnMut(Option<&AgentName>) -> Result<MachineConnection, CoreError>,
        deadline: Instant,
    ) -> Result<Self, Failure> {
        let mut writer = state
            .writer(&route, agent, deadline)
            .map_err(Failure::new)?;
        let recovered = settle(
            state,
            &route,
            Current {
                agent,
                connection,
                link,
                writer: Some(&mut writer),
            },
            connect,
            deadline,
        )?;
        Ok(Self {
            connection,
            link,
            writer,
            recovered,
        })
    }

    /// What settlement resolved, for `data.recovered_operations` and [`recovered_warning`].
    pub fn recovered(&self) -> &[RecoveredOperation] {
        &self.recovered
    }

    /// Runs `mutation` as this identity's next edit. An outcome other than `Accepted` maps
    /// through [`from_outcome`], so `Conflict` is `conflict` and a Worktree failure its own code.
    ///
    /// A reply that does not arrive by `deadline` leaves the edit in the journal and fails with
    /// `outcome_unconfirmed`. The next command of any identity on this route resolves it.
    pub fn mutate(&mut self, mutation: Mutation, deadline: Instant) -> Result<Applied, Failure> {
        let outcome = dispatch(
            &mut self.writer,
            self.connection,
            self.link,
            mutation,
            deadline,
        )?;
        from_outcome(&outcome.outcome)?;
        let Outcome::Accepted { target } = outcome.outcome else {
            return Err(Failure::new(Code::OutcomeUnconfirmed));
        };
        Ok(Applied { target, outcome })
    }
}

/// Runs `mutation` as this identity's next edit and returns its validated outcome.
fn dispatch(
    writer: &mut WriterGuard,
    connection: &MachineConnection,
    link: &CoreLink,
    mutation: Mutation,
    deadline: Instant,
) -> Result<RetainedOutcome, Failure> {
    let journal = writer.journal(connection, deadline)?;
    // Settlement ran first, so a pending edit here is one the server could not resolve yet.
    if journal.pending().is_some() {
        return Err(Failure::new(Code::OutcomeUnconfirmed));
    }
    let (_, revision) = link
        .next_inventory(0)
        .map_err(|error| from_core_error(error.into()))?;
    let sequence = revision.next_sequence;
    crate::interrupt::dispatch()?;
    journal
        .submit(sequence, mutation)
        .map_err(from_core_error)?;
    match dispatch_original(
        journal,
        |operation| edit_request(link, operation, deadline),
        || (),
        |journal, outcome, ()| acknowledge(journal, link, outcome.sequence, deadline),
    ) {
        Ok(EditProgress::Outcome { outcome, .. }) => Ok(*outcome),
        Ok(EditProgress::Missing) => Err(Failure::new(Code::OutcomeUnconfirmed)),
        Err(EditError::Request(RequestError::Refused(refusal))) => {
            // The operation may have been recorded before an uncertain write. Only its
            // recorded outcome, or authoritative absence, decides.
            match resolve(journal, link, deadline)? {
                Resolved::Outcome(outcome) => Ok(*outcome),
                Resolved::Missing => Err(from_core_error(refusal.into())),
                Resolved::Unknown => Err(Failure::new(Code::OutcomeUnconfirmed)),
            }
        }
        Err(error) => Err(edit_failure(error)),
    }
}

/// What the server recorded for a pending sequence.
enum Resolved {
    Outcome(Box<RetainedOutcome>),
    /// The server recorded no such sequence, so it never applied it.
    Missing,
    /// The outcome waits for a Worktree write, or the server did not answer in time.
    Unknown,
}

fn resolve(
    journal: &mut ClientJournal,
    link: &CoreLink,
    deadline: Instant,
) -> Result<Resolved, Failure> {
    match resolve_original(
        journal,
        |operation| edit_request(link, operation, deadline),
        |journal, outcome| acknowledge(journal, link, outcome.sequence, deadline),
    ) {
        Ok(EditProgress::Outcome { outcome, .. }) => Ok(Resolved::Outcome(outcome)),
        Ok(EditProgress::Missing) => {
            journal.abandon().map_err(from_core_error)?;
            Ok(Resolved::Missing)
        }
        Err(EditError::Request(_) | EditError::Unconfirmed | EditError::Cancelled) => {
            Ok(Resolved::Unknown)
        }
        Err(error) => Err(edit_failure(error)),
    }
}

fn edit_failure(error: EditError) -> Failure {
    match error {
        EditError::Core(error) => from_core_error(error),
        EditError::Request(_) | EditError::Unconfirmed | EditError::Cancelled => {
            Failure::new(Code::OutcomeUnconfirmed)
        }
    }
}

fn acknowledge(
    journal: &mut ClientJournal,
    link: &CoreLink,
    sequence: u64,
    deadline: Instant,
) -> Result<(), EditError> {
    confirm_acknowledgement(journal, link, sequence, deadline, |operation, by| {
        edit_request(link, operation, by)
    })
    .map_err(EditError::from)
}

fn edit_request(
    link: &CoreLink,
    operation: Operation,
    deadline: Instant,
) -> Result<ReplyBody, EditError> {
    request(link, operation, deadline)
        .ok_or(EditError::Unconfirmed)?
        .map_err(|refusal| RequestError::Refused(refusal).into())
}

/// Sends `operation` and waits for its reply until `deadline`. `None` means no reply arrived:
/// the deadline passed or the link ended.
fn request(
    link: &CoreLink,
    operation: Operation,
    deadline: Instant,
) -> Option<Result<ReplyBody, Refusal>> {
    let pending = link.request(operation).ok()?;
    // SIGINT after dispatch moves the deadline, so the wait checks it again in short slices.
    let reply = loop {
        let remaining = crate::interrupt::limit(deadline).checked_duration_since(Instant::now())?;
        if let Some(reply) = pending.wait_for(remaining.min(crate::interrupt::TICK)) {
            break reply;
        }
    };
    match reply {
        Ok(reply) => Some(Ok(reply)),
        Err(RequestError::Refused(refusal)) => Some(Err(refusal)),
        Err(RequestError::Link(_)) => None,
    }
}

/// The identity a command runs as, with its admitted link. [`Editor::open`] passes its writer,
/// so settlement uses the lock it already holds.
pub struct Current<'a> {
    pub agent: Option<&'a AgentName>,
    pub connection: &'a MachineConnection,
    pub link: &'a CoreLink,
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
    connect: &mut dyn FnMut(Option<&AgentName>) -> Result<MachineConnection, CoreError>,
    deadline: Instant,
) -> Result<Vec<RecoveredOperation>, Failure> {
    let Current {
        agent,
        connection,
        link,
        writer,
    } = current;
    let mut recovered = Vec::new();
    let held = state.held(route).map_err(from_core_error)?;
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
        let Ok(other_link) = handle.link_by(deadline) else {
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
    connection: &MachineConnection,
    link: &CoreLink,
    writer: Option<&mut WriterGuard>,
    deadline: Instant,
) -> Result<Vec<RecoveredOperation>, Failure> {
    let mut recovered = Vec::new();
    if let Some(writer) = writer {
        let journal = writer.journal(connection, deadline)?;
        if let Some(item) = settle_journal(journal, link, deadline)? {
            recovered.push(RecoveredOperation {
                identity: identity.clone(),
                ..item
            });
        }
    }
    // A live command owns its run receipts. Settlement skips them until that command exits.
    let mut runs = state.runs(route, agent, deadline).map_err(Failure::new)?;
    for item in settle_runs(&mut runs, link, deadline)? {
        recovered.push(RecoveredOperation {
            identity: identity.clone(),
            ..item
        });
    }
    Ok(recovered)
}

fn settle_journal(
    journal: &mut ClientJournal,
    link: &CoreLink,
    deadline: Instant,
) -> Result<Option<RecoveredOperation>, Failure> {
    if let Some(pending) = journal.pending().cloned() {
        let outcome = match resolve(journal, link, deadline)? {
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
    acknowledge(journal, link, last.sequence, deadline).map_err(edit_failure)?;
    Ok(Some(mutation_item(
        last.sequence,
        recovered_outcome(&last.outcome),
    )))
}

fn settle_runs(
    runs: &mut Runs,
    link: &CoreLink,
    deadline: Instant,
) -> Result<Vec<RecoveredOperation>, Failure> {
    let mut recovered = Vec::new();
    for pending in runs.pending()? {
        let Some(_live) = runs.try_run(pending.run)? else {
            continue;
        };
        // Another settler may have removed it between the snapshot and this run's lock.
        if !runs.pending()?.iter().any(|item| item.run == pending.run) {
            continue;
        }
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
