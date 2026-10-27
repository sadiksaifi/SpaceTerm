//! Pushes Workspace Inventory to every connection and answers shared edits after the revision
//! that holds their outcome.
//!
//! An edit commits under the lock, and its revision is queued on every connection before its
//! reply. The reply itself waits on the sync worker until that revision is retained, so the lock
//! is never held for a sync and a client never retires an outcome the server could lose. An edit
//! that waits for a Worktree write is answered when the write settles.
use super::*;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::service::{AfterLock, Dispatch};
use spaceterm_server::protocol::{InventoryRevision, ReplyBody, RequestId};

/// One admitted connection.
pub(super) struct Subscriber {
    device: Id,
    outbox: Outbox,
    /// The next operation sequence last pushed to this client.
    sent: Option<u64>,
}

/// An edit whose reply waits for a Worktree write to settle.
pub(super) struct Waiting {
    connection: Id,
    id: RequestId,
    device: Id,
    sequence: u64,
    /// Dispatched again once Worktree writes settle, replaying a recorded outcome. `None`
    /// resolves the recorded operation instead.
    mutation: Option<Mutation>,
}

impl SharedWorkspaceHost {
    /// Starts pushing inventory to a connection, beginning with the current revision.
    pub(super) fn subscribe(&mut self, device: Id, connection: Id, outbox: Outbox) {
        self.repositories.subscribe(connection);
        self.subscribers.insert(
            connection,
            Subscriber {
                device,
                outbox,
                sent: None,
            },
        );
        self.publish();
    }

    /// Forgets a closed connection and the replies it can no longer receive.
    pub(super) fn unsubscribe(&mut self, connection: Id) {
        self.subscribers.remove(&connection);
        self.waiting.retain(|waiting| waiting.connection != connection);
    }

    /// Pushes the inventory to each connection that has not received it. A newer revision
    /// replaces one a connection has not written yet.
    pub(super) fn publish(&mut self) {
        if self.subscribers.is_empty() {
            return;
        }
        let inventory = self.inventory();
        let changed = self.published.as_ref() != Some(&inventory);
        for subscriber in self.subscribers.values_mut() {
            let next_sequence = self.state.next_sequence(subscriber.device);
            if changed || subscriber.sent != Some(next_sequence) {
                // A closing connection no longer needs it.
                let _ = subscriber.outbox.inventory(InventoryRevision {
                    inventory: inventory.clone(),
                    next_sequence,
                });
                subscriber.sent = Some(next_sequence);
            }
        }
        self.published = Some(inventory);
    }

    /// Validates and commits an edit, or with `None` resolves a recorded one, pushes the
    /// revision that holds its outcome, and replies once that revision is retained.
    pub(super) fn edit(
        &mut self,
        device: Id,
        connection: Id,
        id: RequestId,
        sequence: u64,
        mutation: Option<Mutation>,
    ) -> Dispatch {
        let answer = self.settle(device, sequence, mutation.clone());
        let dispatch = match answer {
            Ok(Answer::Outcome { outcome, revision }) => Dispatch::After(self.deliver(outcome, revision)),
            Ok(Answer::Pending) => {
                self.waiting.push(Waiting {
                    connection,
                    id,
                    device,
                    sequence,
                    mutation,
                });
                Dispatch::Deferred
            }
            Err(refusal) => Dispatch::reply(Err(refusal)),
        };
        self.publish();
        dispatch
    }

    /// Answers the edits whose Worktree writes settled.
    pub(super) fn answer_waiting(&mut self) {
        if self.waiting.is_empty() || self.writes.busy() {
            return;
        }
        let mut answered = Vec::new();
        for waiting in std::mem::take(&mut self.waiting) {
            let answer = self.settle(waiting.device, waiting.sequence, waiting.mutation.clone());
            match answer {
                Ok(Answer::Pending) => self.waiting.push(waiting),
                Ok(Answer::Outcome { outcome, revision }) => {
                    answered.push((waiting, Ok(self.deliver(outcome, revision))));
                }
                Err(refusal) => answered.push((waiting, Err(refusal))),
            }
        }
        // Each reply follows the revision that holds its outcome.
        self.publish();
        for (waiting, answer) in answered {
            let Some(subscriber) = self.subscribers.get(&waiting.connection) else {
                continue;
            };
            // A closing connection no longer needs the reply.
            let _ = match answer {
                Ok(work) => self.sync.reply(&subscriber.outbox, waiting.id, work),
                Err(refusal) => subscriber.outbox.reply(waiting.id, Err(refusal)),
            };
        }
    }

    fn settle(
        &mut self,
        device: Id,
        sequence: u64,
        mutation: Option<Mutation>,
    ) -> Result<Answer, Refusal> {
        self.ready()?;
        match mutation {
            Some(mutation) => self.mutate(device, sequence, mutation),
            None => self.resolve(device, sequence),
        }
    }

    /// The reply to a settled edit, sent from the sync worker once `revision` is retained.
    fn deliver(
        &self,
        outcome: Box<RetainedOutcome>,
        revision: Option<Revision>,
    ) -> AfterLock {
        let journal = self.journal.clone();
        Box::new(move || {
            if let (Some(journal), Some(revision)) = (journal, revision) {
                journal
                    .sync(revision)
                    .map_err(|_| Refusal::StorageUnavailable)?;
            }
            Ok(ReplyBody::Outcome(*outcome))
        })
    }

    /// Retires a delivered outcome. Nothing replies, so a refusal is dropped.
    pub(super) fn acknowledged(&mut self, device: Id, sequence: u64) {
        let _ = self
            .ready()
            .and_then(|()| self.retire_outcome(device, sequence));
    }
}
