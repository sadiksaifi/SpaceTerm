//! Original Workspace operations over an admitted work connection.
pub mod journal;

pub use journal::{ClientJournal, ClientOperation, JournalStore};

use crate::{
    CoreError, PairedMachine,
    connection::link::{CoreLink, LinkError, RequestError},
    hierarchy::{Mutation, Outcome, RetainedOutcome},
    protocol::{InventoryRevision, Operation, Refusal, ReplyBody},
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The pending Original Operation and settled outcomes of an exclusive journal writer.
pub trait EditJournal {
    fn verify_storage(&self) -> Result<(), CoreError>;
    fn pending(&self) -> Option<&ClientOperation>;
    fn submit(&mut self, sequence: u64, mutation: Mutation) -> Result<(), CoreError>;
    fn retire(&mut self, result: RetainedOutcome) -> Result<(), CoreError>;
    fn abandon(&mut self) -> Result<(), CoreError>;
    fn settled(&mut self, sequence: u64) -> Result<(), CoreError>;
}

impl EditJournal for ClientJournal {
    fn verify_storage(&self) -> Result<(), CoreError> {
        self.verify_storage()
    }
    fn pending(&self) -> Option<&ClientOperation> {
        self.pending()
    }
    fn submit(&mut self, sequence: u64, mutation: Mutation) -> Result<(), CoreError> {
        self.submit(sequence, mutation)
    }
    fn retire(&mut self, result: RetainedOutcome) -> Result<(), CoreError> {
        self.retire(result)
    }
    fn abandon(&mut self) -> Result<(), CoreError> {
        self.abandon()
    }
    fn settled(&mut self, sequence: u64) -> Result<(), CoreError> {
        self.settled(sequence)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("{0}")]
    Core(#[from] CoreError),
    #[error("{0}")]
    Request(#[from] RequestError),
    #[error("The Original Operation's outcome is unconfirmed.")]
    Unconfirmed,
}

impl From<LinkError> for EditError {
    fn from(error: LinkError) -> Self {
        Self::Request(RequestError::Link(error))
    }
}

impl From<EditError> for CoreError {
    fn from(error: EditError) -> Self {
        match error {
            EditError::Core(error) => error,
            EditError::Request(error) => error.into(),
            EditError::Unconfirmed => Self::Protocol,
        }
    }
}

/// A validated outcome of the Original Operation, or an authoritative absence from Resolve.
pub enum EditProgress<T = ()> {
    Outcome {
        outcome: Box<RetainedOutcome>,
        /// None while a Worktree write is still pending and has not been retired.
        reconciled: Option<T>,
    },
    Missing,
}

/// Dispatches the journaled Original Operation with its retained sequence and mutation.
/// Callers reconcile the connection's initial inventory before retrying a pending edit.
/// `reconcile` runs once when the outcome arrives, before durable retirement.
/// `published` runs after retirement and may publish membership before acknowledging it.
pub fn dispatch_original<J: EditJournal, T>(
    journal: &mut J,
    exchange: impl FnOnce(Operation) -> Result<ReplyBody, EditError>,
    reconcile: impl FnOnce() -> T,
    published: impl FnOnce(&mut J, &RetainedOutcome, &T) -> Result<(), EditError>,
) -> Result<EditProgress<T>, EditError> {
    journal.verify_storage()?;
    let pending = journal.pending().cloned().ok_or(CoreError::Protocol)?;
    let reply = exchange(Operation::Mutate {
        sequence: pending.sequence,
        mutation: pending.mutation,
    })?;
    retire(journal, reply, reconcile, published)
}

/// Asks only what became of the already journaled edit. A confirmed absence releases the
/// pending original, and a lost reply leaves it in place for a later connection.
pub fn resolve_original<J: EditJournal>(
    journal: &mut J,
    exchange: impl FnOnce(Operation) -> Result<ReplyBody, EditError>,
    published: impl FnOnce(&mut J, &RetainedOutcome) -> Result<(), EditError>,
) -> Result<EditProgress, EditError> {
    journal.verify_storage()?;
    let sequence = journal.pending().ok_or(CoreError::Protocol)?.sequence;
    match exchange(Operation::Resolve { sequence }) {
        Err(EditError::Request(RequestError::Refused(Refusal::Missing))) => {
            journal.abandon()?;
            Ok(EditProgress::Missing)
        }
        reply => retire(
            journal,
            reply?,
            || (),
            |journal, outcome, ()| published(journal, outcome),
        ),
    }
}

fn retire<J: EditJournal, T>(
    journal: &mut J,
    reply: ReplyBody,
    reconcile: impl FnOnce() -> T,
    published: impl FnOnce(&mut J, &RetainedOutcome, &T) -> Result<(), EditError>,
) -> Result<EditProgress<T>, EditError> {
    let ReplyBody::Outcome(outcome) = reply else {
        return Err(CoreError::Protocol.into());
    };
    let pending = journal.pending().ok_or(CoreError::Protocol)?;
    if outcome.sequence != pending.sequence || outcome.mutation != pending.mutation {
        return Err(CoreError::Protocol.into());
    }
    if outcome.outcome == Outcome::Pending {
        return Ok(EditProgress::Outcome {
            outcome: Box::new(outcome),
            reconciled: None,
        });
    }
    let reconciled = reconcile();
    journal.retire(outcome.clone())?;
    published(journal, &outcome, &reconciled)?;
    Ok(EditProgress::Outcome {
        outcome: Box::new(outcome),
        reconciled: Some(reconciled),
    })
}

/// Acknowledges a retained receipt and confirms its removal before forgetting the local copy.
/// The request adapter may tighten the deadline, for example when a CLI is interrupted.
pub fn confirm_acknowledgement(
    journal: &mut impl EditJournal,
    link: &CoreLink,
    sequence: u64,
    deadline: Instant,
    mut exchange: impl FnMut(Operation, Instant) -> Result<ReplyBody, EditError>,
) -> Result<(), CoreError> {
    journal.verify_storage()?;
    if link.acknowledge(sequence).is_err() {
        return Ok(());
    }
    let until = deadline.min(Instant::now() + Duration::from_secs(2));
    loop {
        match exchange(Operation::Resolve { sequence }, until) {
            Err(EditError::Request(RequestError::Refused(Refusal::Missing))) => {
                return journal.settled(sequence);
            }
            Ok(ReplyBody::Outcome(_)) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => return Ok(()),
        }
    }
}

/// Workspace edits bound to a paired Machine and its exclusively held Original Operation journal.
pub struct WorkspaceEditor {
    machine: PairedMachine,
    journal: ClientJournal,
}

impl WorkspaceEditor {
    pub(crate) fn new(
        machine: PairedMachine,
        store: Box<dyn JournalStore>,
    ) -> Result<Self, CoreError> {
        let paired = machine.paired();
        let client = machine
            .agent()
            .map_or(paired.client, |agent| agent.client(&paired.client));
        let journal = ClientJournal::open(store, paired.pin.core, client)?;
        Ok(Self { machine, journal })
    }

    /// Records and dispatches a new edit after work admission and initial inventory. A pending
    /// original must be resolved or retried before a new edit can be admitted.
    pub fn submit(
        &mut self,
        mutation: Mutation,
        deadline: Instant,
    ) -> Result<RetainedOutcome, EditError> {
        if !mutation.is_bounded() {
            return Err(CoreError::Unsupported.into());
        }
        self.journal.verify_storage()?;
        if self.journal.pending().is_some() {
            return Err(EditError::Unconfirmed);
        }
        let (link, inventory) = self.reconciled_link(deadline)?;
        if Instant::now() >= deadline {
            return Err(EditError::Unconfirmed);
        }
        self.journal.submit(inventory.next_sequence, mutation)?;
        self.dispatch(&link, deadline)
    }

    /// Retries exactly the pending Original Operation after reconciling current inventory.
    pub fn retry(&mut self, deadline: Instant) -> Result<Option<RetainedOutcome>, EditError> {
        let (link, _) = self.reconciled_link(deadline)?;
        if self.journal.pending().is_none() {
            return Ok(None);
        }
        self.dispatch(&link, deadline).map(Some)
    }

    /// Resolves a pending edit without dispatching it again.
    pub fn resolve(&mut self, deadline: Instant) -> Result<Option<EditProgress>, EditError> {
        let (link, _) = self.reconciled_link(deadline)?;
        if self.journal.pending().is_none() {
            return Ok(None);
        }
        resolve_original(
            &mut self.journal,
            |operation| request_by(&link, operation, deadline),
            |_, outcome| link.acknowledge(outcome.sequence).map_err(EditError::from),
        )
        .map(Some)
    }

    pub fn journal(&self) -> &ClientJournal {
        &self.journal
    }

    pub fn presented(&mut self, sequence: u64) -> Result<(), CoreError> {
        self.journal.presented(sequence)
    }

    fn reconciled_link(
        &self,
        deadline: Instant,
    ) -> Result<(CoreLink, Arc<InventoryRevision>), EditError> {
        self.journal.verify_storage()?;
        let link = self.machine.link_by(deadline)?;
        let (_, inventory) = link
            .next_inventory_by(0, deadline)?
            .ok_or(EditError::Unconfirmed)?;
        if let Some(last) = self.journal.last() {
            link.acknowledge(last.sequence)?;
        }
        Ok((link, inventory))
    }

    fn dispatch(
        &mut self,
        link: &CoreLink,
        deadline: Instant,
    ) -> Result<RetainedOutcome, EditError> {
        match dispatch_original(
            &mut self.journal,
            |operation| request_by(link, operation, deadline),
            || (),
            |_, outcome, ()| link.acknowledge(outcome.sequence).map_err(EditError::from),
        )? {
            EditProgress::Outcome { outcome, .. } => Ok(*outcome),
            EditProgress::Missing => Err(CoreError::Protocol.into()),
        }
    }
}

fn request_by(
    link: &CoreLink,
    operation: Operation,
    deadline: Instant,
) -> Result<ReplyBody, EditError> {
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .ok_or(EditError::Unconfirmed)
    };
    remaining()?;
    let pending = link.request(operation)?;
    Ok(pending
        .wait_for(remaining()?)
        .ok_or(EditError::Unconfirmed)??)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct UnsynchronizedStore(Cell<bool>);

    impl JournalStore for UnsynchronizedStore {
        fn read(&self) -> Result<Option<Vec<u8>>, CoreError> {
            if self.0.get() {
                Err(CoreError::StorageUnavailable)
            } else {
                Ok(None)
            }
        }

        fn replace(&self, _: &[u8]) -> Result<(), CoreError> {
            self.0.set(true);
            Err(CoreError::StorageUnavailable)
        }
    }

    #[test]
    fn an_unsynchronized_original_cannot_reach_the_transport() {
        let mut journal =
            ClientJournal::open(Box::<UnsynchronizedStore>::default(), [1; 32], [2; 32]).unwrap();
        assert_eq!(
            journal.submit(
                1,
                Mutation::CreateWorkspace {
                    name: "New Workspace".into(),
                    directory: None,
                    control: crate::hierarchy::CreateControl::Creator,
                }
            ),
            Err(CoreError::StorageUnavailable)
        );
        let result = dispatch_original(
            &mut journal,
            |_| panic!("a Workspace edit reached the transport without a synchronized journal"),
            || (),
            |_, _, _| Ok(()),
        );
        assert!(matches!(
            result,
            Err(EditError::Core(CoreError::StorageUnavailable))
        ));
    }
}
