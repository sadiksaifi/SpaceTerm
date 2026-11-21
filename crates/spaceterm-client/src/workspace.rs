//! Original Workspace operations over an admitted work connection.
pub mod journal;

pub use journal::{ClientJournal, ClientOperation, JournalStore};

use crate::{
    CoreError, PairedMachine, WaitLimit,
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
    #[error("The wait for the Workspace edit ended at the caller's request.")]
    Cancelled,
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
            EditError::Cancelled => Self::ServiceBusy,
        }
    }
}

/// A validated outcome of the Original Operation, or a Missing reply from Resolve.
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

/// Asks only what became of the already journaled edit. A Missing reply leaves the journal
/// pending. Core can answer Missing while a deferred edit is still in flight.
pub fn resolve_original<J: EditJournal>(
    journal: &mut J,
    exchange: impl FnOnce(Operation) -> Result<ReplyBody, EditError>,
    published: impl FnOnce(&mut J, &RetainedOutcome) -> Result<(), EditError>,
) -> Result<EditProgress, EditError> {
    journal.verify_storage()?;
    let sequence = journal.pending().ok_or(CoreError::Protocol)?.sequence;
    match exchange(Operation::Resolve { sequence }) {
        Err(EditError::Request(RequestError::Refused(Refusal::Missing))) => {
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
    /// The limit bounds network waits. The injected journal completes its writes synchronously.
    /// Cancellation after journaling keeps the Original Operation available for recovery.
    pub fn submit(
        &mut self,
        mutation: Mutation,
        wait: impl Into<WaitLimit>,
    ) -> Result<RetainedOutcome, EditError> {
        if !mutation.is_bounded() {
            return Err(CoreError::Unsupported.into());
        }
        let wait = wait.into();
        self.journal.verify_storage()?;
        if self.journal.pending().is_some() {
            return Err(EditError::Unconfirmed);
        }
        let (link, inventory) = self.reconciled_link(&wait)?;
        check_wait(&wait)?;
        self.journal.submit(inventory.next_sequence, mutation)?;
        self.dispatch(&link, &wait)
    }

    /// Retries exactly the pending Original Operation after reconciling current inventory.
    pub fn retry(
        &mut self,
        wait: impl Into<WaitLimit>,
    ) -> Result<Option<RetainedOutcome>, EditError> {
        let wait = wait.into();
        let (link, _) = self.reconciled_link(&wait)?;
        if self.journal.pending().is_none() {
            return Ok(None);
        }
        self.dispatch(&link, &wait).map(Some)
    }

    /// Resolves a pending edit without dispatching it again. Missing leaves the edit pending
    /// and returns [`EditError::Unconfirmed`].
    pub fn resolve(
        &mut self,
        wait: impl Into<WaitLimit>,
    ) -> Result<Option<EditProgress>, EditError> {
        let wait = wait.into();
        let (link, _) = self.reconciled_link(&wait)?;
        if self.journal.pending().is_none() {
            return Ok(None);
        }
        match resolve_original(
            &mut self.journal,
            |operation| request_by(&link, operation, &wait),
            |_, outcome| {
                let _ = link.acknowledge(outcome.sequence);
                Ok(())
            },
        )? {
            EditProgress::Missing => Err(EditError::Unconfirmed),
            outcome => Ok(Some(outcome)),
        }
    }

    pub fn journal(&self) -> &ClientJournal {
        &self.journal
    }

    pub fn presented(&mut self, sequence: u64) -> Result<(), CoreError> {
        self.journal.presented(sequence)
    }

    fn reconciled_link(
        &self,
        wait: &WaitLimit,
    ) -> Result<(CoreLink, Arc<InventoryRevision>), EditError> {
        self.journal.verify_storage()?;
        check_wait(wait)?;
        let link = self.machine.link_with_limit(wait);
        check_wait(wait)?;
        let link = link?;
        let inventory = loop {
            let incoming = link.next_inventory_by(0, wait.next_poll());
            check_wait(wait)?;
            if let Some((_, inventory)) = incoming? {
                break inventory;
            }
        };
        if let Some(last) = self.journal.last() {
            link.acknowledge(last.sequence)?;
        }
        Ok((link, inventory))
    }

    fn dispatch(
        &mut self,
        link: &CoreLink,
        wait: &WaitLimit,
    ) -> Result<RetainedOutcome, EditError> {
        match dispatch_original(
            &mut self.journal,
            |operation| request_by(link, operation, wait),
            || (),
            |_, outcome, ()| {
                // The receipt is already durable. If this link cannot accept the ack,
                // the next usable link sends it again from the same journal.
                let _ = link.acknowledge(outcome.sequence);
                Ok(())
            },
        )? {
            EditProgress::Outcome { outcome, .. } => Ok(*outcome),
            EditProgress::Missing => Err(CoreError::Protocol.into()),
        }
    }
}

fn request_by(
    link: &CoreLink,
    operation: Operation,
    wait: &WaitLimit,
) -> Result<ReplyBody, EditError> {
    check_wait(wait)?;
    let pending = link.request(operation)?;
    loop {
        if let Some(reply) = pending.wait_until(wait.next_poll()) {
            return Ok(reply?);
        }
        check_wait(wait)?;
    }
}

fn check_wait(wait: &WaitLimit) -> Result<(), EditError> {
    if wait.is_cancelled() {
        Err(EditError::Cancelled)
    } else if Instant::now() >= wait.deadline() {
        Err(EditError::Unconfirmed)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::link::LinkEvent;
    use crate::direct::{
        DeviceCredential, DirectConnection, DirectConnector, DirectError, MachinePin, PairedDevice,
    };
    use crate::hierarchy::{CreateControl, Inventory};
    use crate::protocol::{ClientMessage, CoreMessage, RawFrame, Reply, frame};
    use std::cell::Cell;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    #[derive(Default)]
    struct JournalBytes(Cell<Option<Vec<u8>>>);
    impl JournalStore for JournalBytes {
        fn read(&self) -> Result<Option<Vec<u8>>, CoreError> {
            let bytes = self.0.take();
            self.0.set(bytes.clone());
            Ok(bytes)
        }
        fn replace(&self, bytes: &[u8]) -> Result<(), CoreError> {
            self.0.set(Some(bytes.to_vec()));
            Ok(())
        }
    }

    struct NoConnector;
    impl DirectConnector for NoConnector {
        fn connect(&self) -> Result<DirectConnection, DirectError> {
            panic!("expected an admitted link");
        }
    }

    fn pairing() -> PairedDevice {
        PairedDevice {
            pin: MachinePin {
                build: "spaceterm-test".into(),
                core: [2; 32],
                public_key: [3; 32],
                certificate: [4; 32],
            },
            pairing: [5; 32],
            client: [6; 32],
        }
    }

    fn machine(connector: impl DirectConnector + 'static) -> PairedMachine {
        PairedMachine::new(
            "spaceterm-test",
            [1; 32],
            pairing(),
            Arc::new(connector),
            Arc::new(|| "Workspace Client".into()),
            None,
            Arc::new(|| None),
        )
        .unwrap()
    }

    fn link(machine: &PairedMachine) -> (CoreLink, mpsc::Receiver<()>) {
        let (wake, frames) = mpsc::channel();
        let link = CoreLink::new(
            Arc::new(|_: LinkEvent| {}),
            Arc::new(move || {
                let _ = wake.send(());
            }),
            Instant::now(),
        );
        machine.work().adopt(link.clone());
        (link, frames)
    }

    fn receive(link: &CoreLink, message: CoreMessage) {
        for bytes in frame::encode_core(&message).unwrap().frames {
            link.receive(RawFrame::parse(&bytes).unwrap(), Instant::now())
                .unwrap();
        }
    }

    fn inventory(link: &CoreLink) {
        receive(
            link,
            CoreMessage::Inventory(Box::new(InventoryRevision {
                inventory: Inventory {
                    repositories: None,
                    revision: 0,
                    workspaces: Vec::new(),
                    close: None,
                },
                next_sequence: 1,
            })),
        );
    }

    fn sent(link: &CoreLink, wake: &mpsc::Receiver<()>) -> ClientMessage {
        let mut assembler = frame::Assembler::default();
        loop {
            wake.recv_timeout(Duration::from_secs(2)).unwrap();
            while let Some((bytes, _)) = link.next_frame() {
                if let Some(message) = assembler.client(RawFrame::parse(&bytes).unwrap()).unwrap() {
                    return message;
                }
            }
        }
    }

    fn mutation() -> Mutation {
        Mutation::CreateWorkspace {
            name: "Original".into(),
            directory: None,
            control: CreateControl::Creator,
        }
    }

    #[cfg(unix)]
    #[test]
    fn journal_accepts_a_valid_edit_after_encoding_fails() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let mut journal =
            ClientJournal::open(Box::<JournalBytes>::default(), [2; 32], [6; 32]).unwrap();
        let invalid = Mutation::CreateWorkspace {
            name: "Original".into(),
            directory: Some(PathBuf::from(OsString::from_vec(vec![b'/', 0xff]))),
            control: CreateControl::Creator,
        };
        assert!(invalid.is_bounded());
        assert!(matches!(
            journal.submit(1, invalid),
            Err(CoreError::StorageUnavailable)
        ));

        journal.submit(1, mutation()).unwrap();
    }

    #[test]
    fn default_bounded_dial_never_starts_an_unbounded_connector() {
        assert_eq!(
            NoConnector
                .connect_by(Instant::now() + Duration::from_secs(1))
                .err(),
            Some(DirectError::Transport)
        );
    }

    #[test]
    fn mobile_cancels_an_active_quic_dial_before_journaling() {
        struct Connector {
            endpoint: String,
            credential: DeviceCredential,
        }
        impl DirectConnector for Connector {
            fn connect(&self) -> Result<DirectConnection, DirectError> {
                panic!("expected the bounded dial");
            }
            fn connect_with(&self, wait: &WaitLimit) -> Result<DirectConnection, DirectError> {
                DirectConnection::connect_with(&self.endpoint, &pairing(), &self.credential, wait)
            }
        }
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let machine = machine(Connector {
            endpoint: format!("quic://{}", socket.local_addr().unwrap()),
            credential: DeviceCredential::generate().unwrap(),
        });
        let mut editor = machine
            .workspace_edits(Box::<JournalBytes>::default())
            .unwrap();
        let started = Instant::now();
        let cancel = Arc::new(AtomicBool::new(false));
        let wait =
            WaitLimit::new(started + Duration::from_secs(4)).with_cancellation(Arc::clone(&cancel));
        std::thread::scope(|scope| {
            scope.spawn(|| {
                socket.recv_from(&mut [0; 1500]).unwrap();
                cancel.store(true, Ordering::Release);
            });
            assert_eq!(editor.submit(mutation(), wait), Err(EditError::Cancelled));
        });
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(editor.journal().pending().is_none());
    }

    #[test]
    fn mobile_cancels_the_inventory_wait_without_recording_or_sending_an_edit() {
        let machine = machine(NoConnector);
        let (link, _) = link(&machine);
        let mut editor = machine
            .workspace_edits(Box::<JournalBytes>::default())
            .unwrap();
        let started = Instant::now();
        let cancel = Arc::new(AtomicBool::new(false));
        let wait =
            WaitLimit::new(started + Duration::from_secs(4)).with_cancellation(Arc::clone(&cancel));
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(80));
                cancel.store(true, Ordering::Release);
            });
            assert_eq!(editor.submit(mutation(), wait), Err(EditError::Cancelled));
        });
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(editor.journal().pending().is_none());
        assert!(link.next_frame().is_none());
    }

    #[test]
    fn mobile_cancels_the_reply_wait_and_keeps_the_possibly_dispatched_original() {
        let machine = machine(NoConnector);
        let (link, wake) = link(&machine);
        inventory(&link);
        let mut editor = machine
            .workspace_edits(Box::<JournalBytes>::default())
            .unwrap();
        let started = Instant::now();
        let cancel = Arc::new(AtomicBool::new(false));
        let wait =
            WaitLimit::new(started + Duration::from_secs(4)).with_cancellation(Arc::clone(&cancel));
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let ClientMessage::Request(request) = sent(&link, &wake) else {
                    panic!("expected the original mutation");
                };
                assert!(matches!(
                    request.operation,
                    Operation::Mutate { sequence: 1, .. }
                ));
                cancel.store(true, Ordering::Release);
            });
            assert_eq!(editor.submit(mutation(), wait), Err(EditError::Cancelled));
        });
        assert!(started.elapsed() < Duration::from_secs(2));
        let pending = editor.journal().pending().unwrap();
        assert_eq!(pending.sequence, 1);
        assert_eq!(pending.mutation, mutation());
    }

    #[test]
    fn oversized_workspace_edits_are_refused_before_dispatch() {
        let mut editor = machine(NoConnector)
            .workspace_edits(Box::<JournalBytes>::default())
            .unwrap();
        let oversized = Mutation::CreateWorkspace {
            name: "x".repeat(257),
            directory: None,
            control: CreateControl::Creator,
        };
        assert_eq!(
            editor.submit(oversized, Instant::now() + Duration::from_secs(2)),
            Err(EditError::Core(CoreError::Unsupported))
        );
        assert!(editor.journal().pending().is_none());
    }

    #[test]
    fn mobile_resolve_missing_leaves_the_original_pending_for_a_deferred_edit() {
        let machine = machine(NoConnector);
        let (link, wake) = link(&machine);
        inventory(&link);
        let mut editor = machine
            .workspace_edits(Box::<JournalBytes>::default())
            .unwrap();
        let original = mutation();
        editor.journal.submit(1, original.clone()).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let ClientMessage::Request(request) = sent(&link, &wake) else {
                    panic!("expected Resolve");
                };
                assert!(matches!(
                    request.operation,
                    Operation::Resolve { sequence: 1 }
                ));
                receive(
                    &link,
                    CoreMessage::Reply(Box::new(Reply {
                        id: request.id,
                        result: Err(Refusal::Missing),
                    })),
                );
            });
            assert!(matches!(
                editor.resolve(Instant::now() + Duration::from_secs(2)),
                Err(EditError::Unconfirmed)
            ));
        });
        let pending = editor.journal().pending().unwrap();
        assert_eq!(pending.sequence, 1);
        assert_eq!(pending.mutation, original);
    }

    #[test]
    fn mobile_returns_a_confirmed_receipt_and_resends_its_acknowledgement() {
        struct CloseOnRetire {
            link: CoreLink,
            bytes: JournalBytes,
        }
        impl JournalStore for CloseOnRetire {
            fn read(&self) -> Result<Option<Vec<u8>>, CoreError> {
                self.bytes.read()
            }
            fn replace(&self, bytes: &[u8]) -> Result<(), CoreError> {
                if self.bytes.read()?.is_some() {
                    self.link.close();
                }
                self.bytes.replace(bytes)
            }
        }
        let machine = machine(NoConnector);
        let (first, wake) = link(&machine);
        inventory(&first);
        let mut editor = machine
            .workspace_edits(Box::new(CloseOnRetire {
                link: first.clone(),
                bytes: JournalBytes::default(),
            }))
            .unwrap();
        let receipt = RetainedOutcome {
            sequence: 1,
            mutation: mutation(),
            outcome: Outcome::Accepted { target: [7; 32] },
        };
        std::thread::scope(|scope| {
            let receipt = &receipt;
            scope.spawn(move || {
                let ClientMessage::Request(request) = sent(&first, &wake) else {
                    panic!("expected the original mutation");
                };
                assert!(matches!(
                    request.operation,
                    Operation::Mutate { sequence: 1, .. }
                ));
                receive(
                    &first,
                    CoreMessage::Reply(Box::new(Reply {
                        id: request.id,
                        result: Ok(ReplyBody::Outcome(receipt.clone())),
                    })),
                );
            });
            assert_eq!(
                editor
                    .submit(mutation(), Instant::now() + Duration::from_secs(2))
                    .unwrap(),
                *receipt
            );
        });
        assert!(editor.journal().pending().is_none());
        assert_eq!(editor.journal().last(), Some(&receipt));
        let (reconnected, wake) = link(&machine);
        inventory(&reconnected);
        assert!(
            editor
                .resolve(Instant::now() + Duration::from_secs(2))
                .unwrap()
                .is_none()
        );
        let ClientMessage::Acknowledge(ack) = sent(&reconnected, &wake) else {
            panic!("expected the retained acknowledgement");
        };
        assert_eq!(ack.sequence, 1);
    }

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
