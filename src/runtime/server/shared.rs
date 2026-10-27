//! Shared hierarchy ownership composed with the existing fenced Pane runtime.
use super::*;
use spaceterm_server::hierarchy::{
    CloseInventory, ConfirmedControl, ConfirmedPane, Id, Inventory, Mutation, Outcome,
    RetainedOutcome, State,
};
use spaceterm_server::protocol::PaneEvent;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

#[cfg(test)]
#[path = "shared/close_control_tests.rs"]
mod close_control_tests;
mod edits;
mod journal;
mod publication;
#[cfg(test)]
#[path = "shared/session_end_tests.rs"]
mod session_end_tests;
mod worktrees;
use journal::{Journal, Retention, Revision};
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::service::Dispatch;
use spaceterm_server::connection::sync::SyncWorker;
use spaceterm_server::protocol::RequestId;
pub(super) use worktrees::pinned_in_worktree;

pub struct SharedWorkspaceHost {
    writes: worktrees::Writes,
    prototype: DevelopmentPaneHost,
    repositories: super::repositories::ServerRepositories,
    state: State,
    panes: BTreeMap<Id, DevelopmentPaneHost>,
    /// Pane identities introduced in this server run, independent of their control grants.
    fresh_panes: BTreeSet<Id>,
    /// Retains organization off the lock. Absent until [`HostCommands::restore`].
    journal: Option<Arc<Journal>>,
    /// The server's one sync worker. It writes the journal and sends deferred replies, in
    /// submission order.
    sync: SyncWorker,
    /// Every admitted connection, and the inventory each still waits to receive.
    publication: Arc<publication::Publication>,
    /// Edits whose replies wait for a Worktree write to settle.
    waiting: Vec<edits::Waiting>,
    /// The host's wake, and the deadline the host last received from a tick.
    ticker: Option<Arc<dyn spaceterm_server::connection::Wake>>,
    armed: Option<Instant>,
    /// Why the last synchronization failed. Requests are refused until one succeeds.
    blocked: Option<Refusal>,
    organization_prepared: bool,
    attachment_generations: BTreeMap<(Id, Id), (u64, u64)>,
}
/// The settled answer to an edit, or the Worktree write it still waits for.
enum Answer {
    /// Final. Reply once `revision` is retained.
    Outcome {
        outcome: Box<RetainedOutcome>,
        revision: Option<Revision>,
    },
    Pending,
}

impl SharedWorkspaceHost {
    /// `sync` is the server's one sync worker, shared with its connections.
    pub fn local(sync: SyncWorker) -> Result<Self, ServerError> {
        Ok(Self::with_sync(DevelopmentPaneHost::local()?, sync))
    }

    /// Compose an account service whose new terminals follow the current login agent.
    pub fn account_service(sync: SyncWorker) -> Result<Self, ServerError> {
        Ok(Self::with_sync(
            DevelopmentPaneHost::local_with_pty_factory(crate::platform::service_pty_factory())?,
            sync,
        ))
    }
    /// A host with a sync worker of its own.
    #[cfg(test)]
    pub(super) fn new(prototype: DevelopmentPaneHost) -> Self {
        let (sync, _) = SyncWorker::spawn().expect("sync worker");
        Self::with_sync(prototype, sync)
    }
    fn with_sync(prototype: DevelopmentPaneHost, sync: SyncWorker) -> Self {
        let mut state = State::empty();
        state.begin_boot(prototype.boot);
        let repositories = super::repositories::ServerRepositories::new(prototype.boot);
        Self {
            prototype,
            writes: worktrees::Writes::new(),
            repositories,
            state,
            panes: BTreeMap::new(),
            fresh_panes: BTreeSet::new(),
            journal: None,
            sync,
            publication: Arc::default(),
            waiting: Vec::new(),
            ticker: None,
            armed: None,
            blocked: None,
            organization_prepared: false,
            attachment_generations: BTreeMap::new(),
        }
    }
    /// Prepare the first organization once, without starting a Terminal Session.
    /// Retained organization, including an empty inventory, is already prepared.
    pub fn prepare_initial_organization(&mut self) -> Result<(), Refusal> {
        self.recover()?;
        if self.organization_prepared {
            return Ok(());
        }
        let mut next = self.state.clone();
        if next.inventory.workspaces.is_empty() {
            let organization = Organization::first(self.prototype.directory.clone())
                .map_err(|_| Refusal::Failed)?;
            let settings = next.worktree_settings.take();
            next = State::from_organization(&organization);
            next.worktree_settings = settings;
            next.begin_boot(self.prototype.boot);
        }
        let revision = self.commit(next)?;
        self.retained(revision)
    }
    /// Makes `next` the state and orders its write. The caller decides who waits for the write:
    /// [`Self::retained`] waits before acting on it, [`Self::retain_later`] lets the sync worker
    /// write it. Nothing outside the state relies on it until it is retained.
    fn commit(&mut self, mut next: State) -> Result<Option<Revision>, Refusal> {
        // Keep provenance through an uncertain write and same-run recovery. Uncommitted
        // identities cannot be selected; recovery prunes those absent from the saved state.
        self.fresh_panes.extend(
            next.inventory
                .workspaces
                .iter()
                .flat_map(|w| &w.tabs)
                .flat_map(|t| &t.panes)
                .filter(|p| self.state.pane(p.id).is_none())
                .map(|p| p.id),
        );
        // Runtime facts authorize an in-memory comparison, never a later recovery read.
        next.inventory.close = None;
        next.inventory.repositories = None;
        let revision = match &self.journal {
            Some(journal) => Some(
                journal.submit(
                    next.envelope(&self.prototype.directory)
                        .map_err(|_| Refusal::StorageUnavailable)?,
                ),
            ),
            None => None,
        };
        self.state = next;
        self.fresh_panes.retain(|id| self.state.pane(*id).is_some());
        self.organization_prepared = true;
        Ok(revision)
    }
    /// Returns once `revision` is retained. A failed write leaves the state uncertain until the
    /// next request or tick reads it back.
    fn retained(&self, revision: Option<Revision>) -> Result<(), Refusal> {
        match (&self.journal, revision) {
            (Some(journal), Some(revision)) => journal
                .sync(revision)
                .map_err(|_| Refusal::StorageUnavailable),
            _ => Ok(()),
        }
    }
    /// Writes `revision` on the sync worker. Nothing waits for it.
    fn retain_later(&self, revision: Option<Revision>) {
        let (Some(journal), Some(revision)) = (&self.journal, revision) else {
            return;
        };
        let writer = Arc::clone(journal);
        if self
            .sync
            .submit(move || {
                let _ = writer.sync(revision);
            })
            .is_err()
        {
            let _ = journal.sync(revision);
        }
    }
    /// The newest revision, which retains every outcome already in the state.
    fn latest(&self) -> Option<Revision> {
        self.journal.as_ref().map(|journal| journal.latest())
    }
    /// Whether every commit so far is retained, so durable deletions may end their processes.
    fn settled(&self) -> bool {
        self.journal
            .as_ref()
            .is_none_or(|journal| journal.settled())
    }
    fn recover(&mut self) -> Result<(), Refusal> {
        if let Some(journal) = &self.journal
            && journal.uncertain()
        {
            let record = journal.recover().map_err(|_| Refusal::StorageUnavailable)?;
            self.restore_organization(record.as_ref());
        }
        Ok(())
    }
    /// Refuses work while retained state is uncertain or the last synchronization failed, for
    /// example because a deleted Pane's process did not stop.
    fn ready(&mut self) -> Result<(), Refusal> {
        self.recover()?;
        self.blocked.map_or(Ok(()), Err)
    }
    fn synchronized(&mut self) -> Result<(), Refusal> {
        let result = self.synchronize();
        self.blocked = result.err();
        result
    }
    fn restore_organization(&mut self, record: Option<&Organization>) {
        self.state = record.map_or_else(State::empty, State::from_organization);
        // Recovery in the same boot preserves creator reservations; a restart clears them.
        self.state.begin_boot(self.prototype.boot);
        self.fresh_panes.retain(|id| self.state.pane(*id).is_some());
        self.organization_prepared = record.is_some();
    }
    /// Applies runtime changes to the state and ends the processes durable deletions name. It
    /// runs on the tick, never on a request's path.
    fn synchronize(&mut self) -> Result<(), Refusal> {
        self.recover()?;
        // Durable deletion is the cleanup intent. An uncertain commit is read back before any
        // process is stopped, and an incomplete stop blocks replies and new work until resolved.
        let durable = self.settled();
        if durable
            && let Some(ends) = self.state.session_ends()
            && ends.boot == self.prototype.boot
        {
            for original in &ends.sessions {
                if let Some(session) = original.session
                    && let Some(host) = self.panes.get_mut(&original.id)
                {
                    host.end_session(session)?;
                }
            }
        }
        for (id, host) in &mut self.panes {
            if durable
                && self.state.pane(*id).is_none()
                && host.pane.as_ref().is_some_and(|p| !p.ended)
            {
                host.shutdown().map_err(|_| Refusal::CleanupUnresolved)?;
            }
        }
        let mut next = self.state.clone();
        for (id, host) in &mut self.panes {
            host.retain_directory()?;
            host.retire_exited()?;
            if let Some(metadata) = host
                .pane
                .as_ref()
                .and_then(|pane| pane.runtime.metadata_snapshot())
            {
                // Keep live OSC 7 display separate from the owning shell's Repository Source
                // Directory and retain_directory's independently confirmed restoration target.
                next.present_directory(*id, metadata.directory.path.as_ref().into());
                next.present_repository_directory(
                    *id,
                    metadata.repository_directory.as_ref().into(),
                );
            }
            if let Some(org) = &host.organization {
                if let Some(path) = &org.confirmed_directory {
                    next.confirm_directory(*id, path.clone());
                }
            } else if next.pane(*id).is_some() {
                next.remove_pane(*id);
            }
        }
        self.repositories.update(&mut next);
        if next != self.state {
            let revision = self.commit(next)?;
            self.retain_later(revision);
        }
        // Presentation is ephemeral. Apply it after durable synchronization so title and
        // activity frames publish to every observer without rewriting Organization storage.
        for (id, host) in &self.panes {
            let snapshot = host
                .pane
                .as_ref()
                .and_then(|pane| pane.runtime.metadata_snapshot());
            self.state.present_presentation(
                *id,
                snapshot
                    .as_deref()
                    .map(super::metadata::encode)
                    .map(Box::new),
            );
        }
        // Keep an exited runtime until its controller has observed its final screen, and a
        // deleted Pane's process until its deletion is retained and the process stopped.
        self.panes.retain(|id, h| {
            if self.state.pane(*id).is_some() {
                h.pane.is_some() || h.directory_unavailable || h.dormant.is_some()
            } else {
                h.pane.as_ref().is_some_and(|p| !p.ended)
            }
        });
        self.attachment_generations.retain(|(_, workspace), _| {
            self.state
                .inventory
                .workspaces
                .iter()
                .any(|w| w.id == *workspace)
        });
        Ok(())
    }
    fn inventory(&self) -> Inventory {
        let mut inventory = self.state.inventory.clone();
        let sessions = inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|pane| ConfirmedPane {
                id: pane.id,
                session: self
                    .panes
                    .get(&pane.id)
                    .and_then(|host| host.pane.as_ref())
                    .map(|pane| pane.lease.session),
            })
            .collect();
        inventory.repositories = Some(self.repositories.snapshot());
        inventory.close = Some(CloseInventory {
            boot: self.prototype.boot,
            sessions,
            controls: Some(
                inventory
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.tabs)
                    .flat_map(|t| &t.panes)
                    .filter_map(|pane| {
                        let host = self.panes.get(&pane.id);
                        let (owner, epoch, session) =
                            if let Some(live) = host.and_then(|h| h.pane.as_ref()) {
                                (live.device, live.control_epoch, Some(live.lease.session))
                            } else if let Some(dormant) = host.and_then(|h| h.dormant) {
                                (dormant.device, dormant.control_epoch, None)
                            } else {
                                (pane.creator, 0, None)
                            };
                        Some(ConfirmedControl {
                            pane: pane.id,
                            session,
                            owner: owner?,
                            epoch,
                        })
                    })
                    .collect(),
            ),
        });
        inventory
    }
    fn pane_host(&mut self, id: Id) -> Result<&mut DevelopmentPaneHost, Refusal> {
        let (w, t, p) = self.state.pane(id).ok_or(Refusal::Missing)?;
        if !self.panes.contains_key(&id) {
            let mut org =
                Organization::first(p.starting_directory.clone()).map_err(|_| Refusal::Failed)?;
            org.workspace = w.id;
            org.tab = t.id;
            org.pane = p.id;
            org.confirmed_directory.clone_from(&p.confirmed_directory);
            let host = DevelopmentPaneHost {
                boot: self.prototype.boot,
                pane: None,
                factory: self.prototype.factory.clone(),
                directory: p.starting_directory.clone(),
                organization: Some(org),
                store: None,
                uncertain: false,
                directory_unavailable: false,
                failed_directory_session: None,
                dormant: None,
                directories: super::directories::DirectoryOperations::default(),
                names: Arc::clone(&self.prototype.names),
                notifier: self.prototype.notifier.clone(),
            };
            self.panes.insert(id, host);
        }
        let host = self.panes.get_mut(&id).ok_or(Refusal::Missing)?;
        if host.pane.is_none()
            && let Some(org) = &mut host.organization
        {
            org.confirmed_directory.clone_from(&p.confirmed_directory);
        }
        Ok(host)
    }
    fn select(
        &mut self,
        device: Id,
        connection: Id,
        id: Id,
        explicit: bool,
        size: PaneSize,
        style: Option<PresentationStyle>,
    ) -> Result<Reply, Refusal> {
        size.validate().map_err(|_| Refusal::Unsupported)?;
        let creator = self.state.pane(id).ok_or(Refusal::Missing)?.2.creator;
        let fresh = self.fresh_panes.contains(&id);
        let host = self.pane_host(id)?;
        if !explicit
            && let Some(dormant) = host.dormant
            && dormant.device != Some(device)
        {
            return Ok(host.takeover(
                PaneTarget {
                    server: host.boot,
                    session: id,
                },
                dormant.device,
                device,
            ));
        }
        if host.pane.is_none()
            && host.dormant.is_none()
            && creator.is_some_and(|owner| owner != device)
        {
            let target = PaneTarget {
                server: host.boot,
                session: id,
            };
            return Ok(host.takeover(target, creator, device));
        }
        // Passive subscriptions may resume existing grants, but cannot acquire or start work.
        if !explicit
            && let Some(pane) = &host.pane
            && pane.device != Some(device)
        {
            return Ok(host.takeover(pane.lease.into(), pane.device, device));
        }
        let mut reply = host.select(device, connection, explicit, size, None, style)?;
        if let Reply::Restored {
            fresh: reply_fresh,
            session_ended,
            ..
        } = &mut reply
        {
            *reply_fresh = fresh && !*session_ended;
        }
        Ok(reply)
    }
    fn repair_directory(
        &mut self,
        device: Id,
        connection: Id,
        target: pane::directories::Target,
        replacement: Option<std::path::PathBuf>,
        size: PaneSize,
        style: Option<PresentationStyle>,
    ) -> Result<Reply, Refusal> {
        size.validate().map_err(|_| Refusal::Unsupported)?;
        if target.boot != self.prototype.boot {
            return Err(Refusal::Stale);
        }
        let id = target.pane;
        if self.pane_host(id)?.pane.is_some() {
            return self.select(device, connection, id, false, size, style);
        }
        let (_, _, pane) = self.state.pane(id).ok_or(Refusal::Missing)?;
        let host = self.panes.get(&id).ok_or(Refusal::Missing)?;
        let controller = host.dormant.map_or(pane.creator, |dormant| dormant.device);
        if controller.is_some_and(|controller| controller != device) {
            return self.select(device, connection, id, false, size, style);
        }
        if target.original
            != host
                .organization
                .as_ref()
                .ok_or(Refusal::Missing)?
                .startup_directory()
        {
            return Err(Refusal::Stale);
        }
        let directory = local_filesystem()
            .validate_directory(replacement.as_deref().unwrap_or(&target.original))
            .map_err(|_| Refusal::DirectoryUnavailable)?;
        if replacement.is_some() {
            let mut next = self.state.clone();
            next.repair_directory(id, directory.path().to_owned())?;
            // Commit just this Pane's directory before launching. Uncertain writes start nothing.
            let revision = self.commit(next)?;
            self.retained(revision)?;
            self.pane_host(id)?
                .organization
                .as_mut()
                .ok_or(Refusal::Missing)?
                .confirmed_directory = Some(directory.path().to_owned());
        }
        self.pane_host(id)?
            .start_directory(device, connection, size, style, directory)
            .map(|lease| Reply::Attached { lease })
    }
    /// Validates and commits one edit. The answer is final once its revision is retained, unless
    /// a Worktree write must settle first.
    fn mutate(&mut self, device: Id, sequence: u64, mutation: Mutation) -> Result<Answer, Refusal> {
        self.finish_worktrees(false)?;
        // Replay is resolved before filesystem validation; a changed directory cannot turn an
        // already committed pin into a different outcome.
        if let Some(old) = self.state.outcome(device, sequence) {
            return if old.mutation == mutation {
                Ok(self.answer(old.clone()))
            } else {
                Err(Refusal::Stale)
            };
        }
        if let Mutation::Worktree(operation) = mutation {
            return self.begin_worktree(device, sequence, operation);
        }
        if !self.state.pending_worktrees().is_empty() {
            return Ok(Answer::Pending);
        }
        let mut next = self.state.clone();
        next.inventory = self.inventory();
        let mut directory = Ok(self.prototype.directory.clone());
        if let Mutation::SplitPane {
            workspace,
            tab,
            source,
            ..
        } = &mutation
            && let Some(workspace) = next
                .inventory
                .workspaces
                .iter()
                .find(|w| w.id == *workspace)
            && let Some(root) = workspace
                .tabs
                .iter()
                .find(|t| t.id == *tab)
                .and_then(|t| t.worktree.as_deref())
        {
            directory = worktrees::starting_directory(workspace, Some(*source), root);
        }
        // A directory refusal finishes this journaled edit, so retry cannot admit it later.
        next.apply(
            device,
            sequence,
            mutation,
            directory.as_deref().unwrap_or(&self.prototype.directory),
            |path| directory.is_ok() && local_filesystem().validate_directory(path).is_ok(),
        )?;
        let outcome = next
            .outcome(device, sequence)
            .ok_or(Refusal::Failed)?
            .clone();
        Ok(Answer::Outcome {
            outcome: Box::new(outcome),
            revision: self.commit(next)?,
        })
    }
    /// The recorded outcome of an Original Operation.
    fn resolve(&mut self, device: Id, sequence: u64) -> Result<Answer, Refusal> {
        self.finish_worktrees(false)?;
        let outcome = self
            .state
            .outcome(device, sequence)
            .ok_or(Refusal::Missing)?
            .clone();
        Ok(self.answer(outcome))
    }
    /// A recorded outcome is retained with the newest revision, or still waits for Git.
    fn answer(&self, outcome: RetainedOutcome) -> Answer {
        if outcome.outcome == Outcome::Pending {
            Answer::Pending
        } else {
            Answer::Outcome {
                outcome: Box::new(outcome),
                revision: self.latest(),
            }
        }
    }
    /// Retires a delivered outcome. Nothing waits for the write: a lost acknowledgement leaves
    /// the outcome recorded, and the client acknowledges it again after reconnecting.
    fn retire_outcome(&mut self, device: Id, sequence: u64) -> Result<(), Refusal> {
        let mut next = self.state.clone();
        next.acknowledge(device, sequence)?;
        if next != self.state {
            let revision = self.commit(next)?;
            self.retain_later(revision);
        }
        Ok(())
    }
    /// An in-process edit waits for its revision under the lock.
    fn reply(&self, answer: Answer) -> Result<Reply, Refusal> {
        match answer {
            Answer::Outcome { outcome, revision } => {
                self.retained(revision)?;
                Ok(Reply::Outcome(*outcome))
            }
            Answer::Pending => Ok(Reply::MutationPending),
        }
    }
    fn handle(&mut self, device: Id, connection: Id, request: Request) -> Result<Reply, Refusal> {
        self.ready()?;
        match request {
            Request::WorktreeRead(read) => Ok(Reply::WorktreeRead(
                self.worktree_read(device, connection, read),
            )),
            Request::DirectoryRead(request) => Ok(Reply::DirectoryRead(
                self.prototype.directories.request(connection, request),
            )),
            Request::DirectoryCreate(request) => Ok(Reply::DirectoryCreate(
                self.prototype.directories.create(connection, request),
            )),
            Request::RepairDirectory {
                target,
                replacement,
                size,
                style,
            } => self.repair_directory(device, connection, target, replacement, size, style),
            Request::ClaimPane {
                attachment,
                pane: id,
                claim,
                size,
                style,
            } => {
                size.validate().map_err(|_| Refusal::Unsupported)?;
                let workspace = self.state.pane(id).ok_or(Refusal::Missing)?.0.id;
                if workspace != attachment.workspace || attachment.generation == 0 {
                    return Err(Refusal::Stale);
                }
                let generations = self
                    .attachment_generations
                    .entry((device, workspace))
                    .or_default();
                if attachment.generation < generations.0 || attachment.generation <= generations.1 {
                    return Err(Refusal::Stale);
                }
                generations.0 = attachment.generation;
                let host = self.pane_host(id)?;
                if matches!(claim, AttachmentClaim::Select { explicit: false })
                    && let Some(dormant) = host.dormant
                    && dormant.device == Some(device)
                    && dormant.attachment_generation != Some(attachment.generation)
                {
                    return Ok(host.takeover(
                        PaneTarget {
                            server: host.boot,
                            session: id,
                        },
                        dormant.device,
                        device,
                    ));
                }
                // A new attachment observes an old grant without resuming it. Selection can only
                // acquire unclaimed work; Take Over remains explicit.
                if matches!(claim, AttachmentClaim::Select { explicit: false })
                    && let Some(pane) = &host.pane
                    && pane.device == Some(device)
                    && pane.attachment_generation != Some(attachment.generation)
                {
                    return Ok(host.takeover(pane.lease.into(), pane.device, device));
                }
                let reply = match claim {
                    AttachmentClaim::Repair {
                        target,
                        replacement,
                    } => {
                        if target.pane != id {
                            return Err(Refusal::Stale);
                        }
                        self.repair_directory(device, connection, target, replacement, size, style)?
                    }
                    AttachmentClaim::Select { explicit } => {
                        self.select(device, connection, id, explicit, size, style)?
                    }
                    AttachmentClaim::Start { server } => {
                        if server != self.prototype.boot {
                            return Err(Refusal::Stale);
                        }
                        self.select(device, connection, id, true, size, style)?
                    }
                    AttachmentClaim::TakeOver { target } => {
                        let host = self.pane_host(id)?;
                        if let Some(pane) = &host.pane {
                            if PaneTarget::from(pane.lease) != target {
                                return Err(Refusal::Stale);
                            }
                            host.handle(
                                device,
                                connection,
                                Request::TakeOver {
                                    target,
                                    size,
                                    style,
                                },
                            )?
                        } else {
                            if target.server != host.boot || target.session != id {
                                return Err(Refusal::Stale);
                            }
                            if host.dormant.is_some() {
                                host.take_over_ended(device, Some(attachment.generation))?
                            } else {
                                Reply::Attached {
                                    lease: host.start(device, connection, size, style)?,
                                }
                            }
                        }
                    }
                };
                if matches!(reply, Reply::Attached { .. }) {
                    self.pane_host(id)?
                        .pane
                        .as_mut()
                        .ok_or(Refusal::Missing)?
                        .attachment_generation = Some(attachment.generation);
                }
                Ok(reply)
            }
            Request::ReleaseAttachment { attachment } => {
                // Deletion or restart already ended the original grants. Never restore membership.
                if !self
                    .state
                    .inventory
                    .workspaces
                    .iter()
                    .any(|w| w.id == attachment.workspace)
                {
                    return Ok(Reply::Unchanged);
                }
                if attachment.generation == 0 {
                    return Err(Refusal::Stale);
                }
                let generations = self
                    .attachment_generations
                    .entry((device, attachment.workspace))
                    .or_default();
                generations.1 = generations.1.max(attachment.generation);
                for (id, host) in &mut self.panes {
                    if self
                        .state
                        .pane(*id)
                        .is_some_and(|(w, _, _)| w.id == attachment.workspace)
                        && let Some(dormant) = &mut host.dormant
                        && dormant.device == Some(device)
                        && dormant
                            .attachment_generation
                            .is_some_and(|generation| generation <= attachment.generation)
                    {
                        dormant.device = None;
                        dormant.attachment_generation = None;
                    }
                    if self
                        .state
                        .pane(*id)
                        .is_some_and(|(w, _, _)| w.id == attachment.workspace)
                        && let Some(pane) = &mut host.pane
                        && pane.device == Some(device)
                        && pane
                            .attachment_generation
                            .is_some_and(|generation| generation <= attachment.generation)
                    {
                        pane.invalidate_input();
                        pane.lease.control = nonce().map_err(|_| Refusal::Failed)?;
                        pane.device = None;
                        pane.connected = false;
                        pane.attachment_generation = None;
                    }
                }
                Ok(Reply::Unchanged)
            }
            Request::Inventory => {
                self.repositories.subscribe(connection);
                self.finish_worktrees(false)?;
                self.synchronized()?;
                Ok(Reply::Inventory {
                    inventory: self.inventory(),
                    next_sequence: self.state.next_sequence(device),
                })
            }
            Request::Mutate { sequence, mutation } => {
                let answer = self.mutate(device, sequence, mutation)?;
                self.reply(answer)
            }
            Request::Resolve { sequence } => {
                let answer = self.resolve(device, sequence)?;
                self.reply(answer)
            }
            Request::Acknowledge { sequence } => {
                self.retire_outcome(device, sequence)?;
                Ok(Reply::Unchanged)
            }
            Request::SelectPane {
                pane,
                explicit,
                size,
                style,
            } => self.select(device, connection, pane, explicit, size, style),
            Request::TakeOver {
                target,
                size,
                style,
            } if self.state.pane(target.session).is_some() => {
                size.validate().map_err(|_| Refusal::Unsupported)?;
                if target.server != self.prototype.boot {
                    return Err(Refusal::Stale);
                }
                let host = self.pane_host(target.session)?;
                if host.pane.is_some() {
                    return Err(Refusal::Stale);
                }
                if host.dormant.is_some() {
                    return host.take_over_ended(device, None);
                }
                host.start(device, connection, size, style)
                    .map(|lease| Reply::Attached { lease })
            }
            Request::Shutdown => {
                self.shutdown().map_err(|_| Refusal::CleanupUnresolved)?;
                Ok(Reply::Closed)
            }
            Request::StartRestored {
                server,
                pane,
                size,
                style,
            } => {
                if server != self.prototype.boot {
                    return Err(Refusal::Stale);
                }
                self.select(device, connection, pane, true, size, style)
            }
            Request::Attach {
                create,
                size,
                resume,
                style,
            } => {
                let id = self.legacy_pane(create)?;
                self.pane_host(id)?.handle(
                    device,
                    connection,
                    Request::Attach {
                        create,
                        size,
                        resume,
                        style,
                    },
                )
            }
            Request::Select {
                create,
                size,
                resume,
                style,
            } => {
                let id = self.legacy_pane(create)?;
                self.pane_host(id)?.handle(
                    device,
                    connection,
                    Request::Select {
                        create,
                        size,
                        resume,
                        style,
                    },
                )
            }
            request => {
                let target = match &request {
                    Request::TakeOver { target, .. } => *target,
                    Request::Accessibility { lease, .. }
                    | Request::EndAccessibilityDemand { lease }
                    | Request::AccessibilitySelection { lease, .. }
                    | Request::ReadingStatus { lease }
                    | Request::Read { lease, .. }
                    | Request::GraphicsChunk { lease, .. }
                    | Request::SelectionChunk { lease, .. }
                    | Request::CompleteClipboard { lease, .. }
                    | Request::Focus { lease, .. }
                    | Request::Input { lease, .. }
                    | Request::Keys { lease, .. }
                    | Request::Key { lease, .. }
                    | Request::Resize { lease, .. }
                    | Request::ClearScreenAndScrollback { lease }
                    | Request::Style { lease, .. }
                    | Request::Release { lease }
                    | Request::Close { lease } => (*lease).into(),
                    _ => return Err(Refusal::Unsupported),
                };
                let host = self
                    .panes
                    .values_mut()
                    .find(|h| {
                        h.pane
                            .as_ref()
                            .is_some_and(|p| PaneTarget::from(p.lease) == target)
                    })
                    .ok_or_else(|| {
                        if self.state.session_ends().is_some_and(|ends| {
                            ends.boot == target.server
                                && ends
                                    .sessions
                                    .iter()
                                    .any(|p| p.session == Some(target.session))
                        }) {
                            Refusal::Stale
                        } else {
                            Refusal::Missing
                        }
                    })?;
                host.handle(device, connection, request)
            }
        }
    }
    fn legacy_pane(&mut self, create: bool) -> Result<Id, Refusal> {
        if create {
            self.prepare_initial_organization()?;
        }
        self.state
            .inventory
            .workspaces
            .first()
            .and_then(|w| w.tabs.first())
            .and_then(|t| t.panes.first())
            .map(|p| p.id)
            .ok_or(Refusal::Missing)
    }
}
impl SharedWorkspaceHost {
    fn retain_in(&mut self, retention: Box<dyn Retention>) -> Result<(), ServerError> {
        let record = retention.read()?;
        self.restore_organization(record.as_ref());
        // Existing organization predates seeding and must never be overwritten by a client.
        if self.state.worktree_settings.is_none() && record.is_some() {
            self.state.worktree_settings = Some(Default::default());
        }
        let publication = Arc::clone(&self.publication);
        self.journal = Some(Arc::new(Journal::new(
            retention,
            Box::new(move |revision| publication.release(revision)),
        )));
        self.prepare_initial_organization()
            .map_err(|_| ServerError::StorageUnavailable)
    }
    /// Runs timed work, pushes the inventory it changed, and returns when work is next due.
    /// `None` means nothing waits: no write, Git or directory work or Git facts. A live terminal
    /// rings the host's wake when its screen, metadata or lifetime changes, so it needs no tick.
    pub(super) fn tick_until(&mut self, now: Instant) -> Option<Instant> {
        let _ = self.finish_worktrees(false);
        self.prototype.directories.tick();
        let _ = self.synchronized();
        self.answer_waiting();
        self.publish();
        self.armed = self.due(now);
        self.armed
    }
    /// When timed work is next due, without running it.
    fn due(&self, now: Instant) -> Option<Instant> {
        let waiting = self.blocked.is_some()
            || !self.settled()
            || self.writes.busy()
            || !self.waiting.is_empty()
            || self.prototype.directories.busy();
        let soon = waiting.then_some(now + super::dispatch::TICK);
        // A failed synchronization never reaches the Git read it would start, so its due time
        // stays past. Retrying waits for the tick instead.
        let repositories = (self.blocked.is_none() && self.settled())
            .then(|| self.repositories.due(now))
            .flatten();
        [soon, repositories].into_iter().flatten().min()
    }
    /// Wakes the host when a message started timed work due before the deadline it waits for.
    fn rearm(&mut self) {
        let Some(due) = self.due(Instant::now()) else {
            return;
        };
        if self.armed.is_none_or(|armed| due < armed) {
            self.armed = Some(due);
            if let Some(ticker) = &self.ticker {
                ticker.wake();
            }
        }
    }
}
impl HostCommands for SharedWorkspaceHost {
    fn restore(&mut self, store: OrganizationStore) -> Result<(), ServerError> {
        self.retain_in(Box::new(store))
    }
    fn tick(&mut self, now: Instant) -> Option<Instant> {
        self.tick_until(now)
    }
    fn ticker(&mut self, wake: Arc<dyn spaceterm_server::connection::Wake>) {
        self.ticker = Some(wake);
    }
    fn stopping(&mut self) {
        self.refuse_waiting();
    }
    fn admitted(&mut self, client: Id, name: control::ClientName) {
        self.prototype.admitted(client, name);
    }
    fn connected(&mut self, client: Id, connection: Id, outbox: &Outbox) {
        self.subscribe(client, connection, outbox.clone());
        self.rearm();
    }
    fn mutate(
        &mut self,
        client: Id,
        connection: Id,
        id: RequestId,
        sequence: u64,
        mutation: Option<Mutation>,
    ) -> Dispatch {
        let dispatch = self.edit(client, connection, id, sequence, mutation);
        self.rearm();
        dispatch
    }
    fn acknowledge(&mut self, client: Id, sequence: u64) {
        self.acknowledged(client, sequence);
        self.rearm();
    }
    fn request(&mut self, device: Id, connection: Id, request: Request) -> Reply {
        // Claims and releases move control and start or end sessions. Terminal work on a lease
        // changes no inventory; the tick pushes what it changes in a Pane's metadata.
        let control = matches!(
            request,
            Request::ClaimPane { .. }
                | Request::SelectPane { .. }
                | Request::TakeOver { .. }
                | Request::StartRestored { .. }
                | Request::RepairDirectory { .. }
                | Request::ReleaseAttachment { .. }
                | Request::Release { .. }
                | Request::Close { .. }
                | Request::Attach { .. }
                | Request::Select { .. }
        );
        let reply = self
            .handle(device, connection, request)
            .unwrap_or_else(Reply::Refused);
        if control {
            self.publish();
        }
        self.rearm();
        reply
    }
    fn lease_live(&self, lease: &Lease) -> bool {
        self.panes.values().any(|host| host.lease_live(lease))
    }
    fn set_notifier(&mut self, notifier: crate::terminal::session::SubscriptionNotifier) {
        for host in self.panes.values_mut() {
            host.notifier = Some(Arc::clone(&notifier));
        }
        self.prototype.notifier = Some(notifier);
    }
    fn observe(&mut self, device: Id, lease: &Lease) -> super::publish::Observed {
        let target = PaneTarget::from(*lease);
        let Some(host) = self.panes.values_mut().find(|host| {
            host.pane
                .as_ref()
                .is_some_and(|pane| PaneTarget::from(pane.lease) == target)
        }) else {
            // A Pane removed, closed or ended by End All Sessions no longer has this lease.
            let event = if self
                .panes
                .values()
                .any(|host| host.failed_directory_session == Some(target))
            {
                PaneEvent::DirectoryUnavailable
            } else if self.state.session_ends().is_some_and(|ends| {
                ends.boot == target.server
                    && ends
                        .sessions
                        .iter()
                        .any(|pane| pane.session == Some(target.session))
            }) {
                PaneEvent::SessionEnded
            } else {
                PaneEvent::Ended
            };
            return super::publish::Observed::Gone { last: None, event };
        };
        let observed = host.observe(device, lease);
        if matches!(observed, super::publish::Observed::Gone { .. }) {
            let _ = self.synchronize();
        }
        observed
    }
    fn view(&mut self, lease: &Lease, visible: bool, max_hz: u8) {
        for host in self.panes.values_mut() {
            host.view(lease, visible, max_hz);
        }
    }
    fn input_written(&mut self, lease: &Lease, serial: u32) {
        for host in self.panes.values_mut() {
            host.input_written(lease, serial);
        }
    }
    fn unbound(&mut self, connection: Id, lease: &Lease) {
        for host in self.panes.values_mut() {
            host.unbound(connection, lease);
        }
    }
    fn disconnected(&mut self, connection: Id) {
        self.unsubscribe(connection);
        self.cancel_worktree_reads(connection);
        self.prototype.directories.cancel_connection(connection);
        self.repositories.disconnected(connection);
        for host in self.panes.values_mut() {
            host.disconnected(connection);
        }
    }
    fn revoked(&mut self, client: Id) {
        self.prototype.revoked(client);
        for host in self.panes.values_mut() {
            host.revoked(client);
        }
        for workspace in &mut self.state.inventory.workspaces {
            for tab in &mut workspace.tabs {
                for pane in &mut tab.panes {
                    if pane.creator == Some(client) {
                        pane.creator = None;
                    }
                }
            }
        }
        self.attachment_generations
            .retain(|(owner, _), _| *owner != client);
        self.publish();
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
        self.stop_worktree_reads();
        self.finish_worktrees(true)
            .map_err(|_| ServerError::StorageUnavailable)?;
        self.prototype.directories.stop();
        self.repositories.shutdown();
        self.synchronize()
            .map_err(|_| ServerError::StorageUnavailable)?;
        for host in self.panes.values_mut() {
            host.shutdown()?;
        }
        self.synchronize()
            .map_err(|_| ServerError::StorageUnavailable)?;
        self.retained(self.latest())
            .map_err(|_| ServerError::StorageUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{host, screen, shared_inventory, size};
    use super::*;
    use spaceterm_server::attachments::Attachment;

    fn retained(host: &SharedWorkspaceHost) -> Organization {
        serde_json::from_slice(
            &serde_json::to_vec(&host.state.envelope(&host.prototype.directory).unwrap()).unwrap(),
        )
        .unwrap()
    }

    fn journaled() -> (SharedWorkspaceHost, Arc<journal::memory::Memory>) {
        let memory = Arc::new(journal::memory::Memory::default());
        let mut host = SharedWorkspaceHost::new(host());
        host.retain_in(Box::new(journal::memory::Shared(Arc::clone(&memory))))
            .unwrap();
        (host, memory)
    }

    fn rename(host: &SharedWorkspaceHost, name: &str) -> Mutation {
        let workspace = &host.state.inventory.workspaces[0];
        Mutation::RenameWorkspace {
            workspace: workspace.id,
            version: workspace.name_version,
            name: name.into(),
        }
    }

    fn retained_outcome(memory: &journal::memory::Memory, device: Id, sequence: u64) -> bool {
        State::from_organization(&memory.retained().unwrap())
            .outcome(device, sequence)
            .is_some()
    }

    #[test]
    fn an_acknowledgement_replies_before_the_sync_worker_retains_it() {
        let (mut host, memory) = journaled();
        let device = [1; 32];
        let mutation = rename(&host, "Edited");
        assert!(matches!(
            host.request(
                device,
                device,
                Request::Mutate {
                    sequence: 1,
                    mutation
                }
            ),
            Reply::Outcome(_)
        ));
        assert!(
            retained_outcome(&memory, device, 1),
            "an outcome is retained before its reply"
        );
        let release = memory.hold();
        assert!(matches!(
            host.request(device, device, Request::Acknowledge { sequence: 1 }),
            Reply::Unchanged
        ));
        assert!(
            retained_outcome(&memory, device, 1),
            "the write is still held"
        );
        release.send(()).unwrap();
        host.sync.flush().unwrap();
        assert!(!retained_outcome(&memory, device, 1));
        assert!(host.settled());
        host.shutdown().unwrap();
    }

    fn drain(outbox: &Outbox) -> Vec<spaceterm_server::protocol::ServerMessage> {
        use spaceterm_server::protocol::{RawFrame, frame::Assembler};
        let mut assembler = Assembler::default();
        let mut messages = Vec::new();
        while let Some((frame, _)) = outbox.next_frame() {
            if let Some(message) = assembler.server(RawFrame::parse(&frame).unwrap()).unwrap() {
                messages.push(message);
            }
        }
        messages
    }

    #[test]
    fn an_edit_is_pushed_and_replied_only_once_its_revision_is_retained() {
        use spaceterm_server::protocol::{ReplyBody, ServerMessage};
        let (mut host, memory) = journaled();
        let device = [1; 32];
        let outbox = Outbox::new(Arc::new(|| {}));
        HostCommands::connected(&mut host, device, device, &outbox);
        assert!(matches!(
            drain(&outbox).as_slice(),
            [ServerMessage::Inventory(revision)] if revision.next_sequence == 1
        ));
        let id = RequestId(1);
        outbox.begin(id).unwrap();
        let release = memory.hold();
        let mutation = rename(&host, "Edited");
        let Dispatch::After(work) =
            HostCommands::mutate(&mut host, device, device, id, 1, Some(mutation))
        else {
            panic!("an edit replies after its write");
        };
        // What the connection does with `After` work, under the lock.
        host.sync.reply(&outbox, id, work).unwrap();
        assert!(
            drain(&outbox).is_empty(),
            "neither the revision nor the reply leaves while its write is held"
        );
        release.send(()).unwrap();
        host.sync.flush().unwrap();
        assert!(matches!(
            drain(&outbox).as_slice(),
            [ServerMessage::Inventory(revision), ServerMessage::Reply(reply)]
                if revision.inventory.workspaces[0].name == "Edited"
                    && revision.next_sequence == 2
                    && matches!(reply.result, Ok(ReplyBody::Outcome(_)))
        ));
        assert!(retained_outcome(&memory, device, 1));
        HostCommands::acknowledge(&mut host, device, 1);
        host.sync.flush().unwrap();
        assert!(!retained_outcome(&memory, device, 1));
        assert!(drain(&outbox).is_empty(), "an acknowledgement has no reply");
        host.shutdown().unwrap();
    }

    #[test]
    fn a_revision_whose_write_fails_is_never_pushed() {
        use spaceterm_server::protocol::ServerMessage;
        use std::sync::atomic::Ordering;
        let (mut host, memory) = journaled();
        let device = [1; 32];
        let outbox = Outbox::new(Arc::new(|| {}));
        HostCommands::connected(&mut host, device, device, &outbox);
        drain(&outbox);
        memory.fail.store(true, Ordering::SeqCst);
        let id = RequestId(1);
        outbox.begin(id).unwrap();
        let mutation = rename(&host, "Lost");
        let Dispatch::After(work) =
            HostCommands::mutate(&mut host, device, device, id, 1, Some(mutation))
        else {
            panic!("an edit replies after its write");
        };
        host.sync.reply(&outbox, id, work).unwrap();
        host.sync.flush().unwrap();
        assert!(matches!(
            drain(&outbox).as_slice(),
            [ServerMessage::Reply(reply)] if matches!(reply.result, Err(Refusal::StorageUnavailable))
        ));
        memory.fail.store(false, Ordering::SeqCst);
        host.tick(Instant::now());
        assert!(
            drain(&outbox).iter().all(|message| !matches!(
                message,
                ServerMessage::Inventory(revision)
                    if revision.inventory.workspaces[0].name == "Lost"
            )),
            "recovery publishes only retained organization"
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn a_failing_synchronization_waits_a_tick_before_trying_again() {
        use std::sync::atomic::Ordering;
        let (mut host, memory) = journaled();
        let outbox = Outbox::new(Arc::new(|| {}));
        // A subscriber's Git facts are due at once.
        HostCommands::connected(&mut host, [1; 32], [1; 32], &outbox);
        memory.fail.store(true, Ordering::SeqCst);
        let mutation = rename(&host, "Uncertain");
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Mutate {
                    sequence: 1,
                    mutation
                }
            ),
            Reply::Refused(Refusal::StorageUnavailable)
        ));
        let now = Instant::now();
        let next = HostCommands::tick(&mut host, now).expect("recovery is retried");
        assert!(next > now, "the host would spin while recovery fails");
        memory.fail.store(false, Ordering::SeqCst);
        host.shutdown().unwrap();
    }

    #[test]
    fn a_deleted_session_ends_only_after_its_deletion_is_retained() {
        use spaceterm_server::hierarchy::Outcome;
        let (mut host, _memory) = journaled();
        let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
        let Reply::Attached { lease } = host.request(
            [1; 32],
            [1; 32],
            Request::SelectPane {
                pane,
                explicit: true,
                size: size(),
                style: None,
            },
        ) else {
            panic!("explicit first use");
        };
        let confirmation = shared_inventory(&mut host).close.unwrap();
        let Ok(Answer::Outcome { outcome, revision }) =
            host.mutate([2; 32], 1, Mutation::EndAllSessions { confirmation })
        else {
            panic!("End All Sessions answers at once");
        };
        assert!(matches!(outcome.outcome, Outcome::Accepted { .. }));
        host.tick(Instant::now());
        assert!(
            super::super::tests::screen(&mut host, [1; 32], lease).is_ok(),
            "the session runs while its ending is not retained"
        );
        host.retained(revision).unwrap();
        host.tick(Instant::now());
        assert!(matches!(
            super::super::tests::screen(&mut host, [1; 32], lease),
            Err(PaneEvent::SessionEnded)
        ));
        host.shutdown().unwrap();
    }

    #[test]
    fn a_failed_write_is_refused_and_read_back_before_the_original_operation_retries() {
        use std::sync::atomic::Ordering;
        let (mut host, memory) = journaled();
        let device = [1; 32];
        let original = shared_inventory(&mut host).workspaces[0].name.clone();
        let mutation = rename(&host, "Lost");
        memory.fail.store(true, Ordering::SeqCst);
        assert!(matches!(
            host.request(
                device,
                device,
                Request::Mutate {
                    sequence: 1,
                    mutation: mutation.clone()
                }
            ),
            Reply::Refused(Refusal::StorageUnavailable)
        ));
        assert!(matches!(
            host.request([2; 32], [2; 32], Request::Inventory),
            Reply::Refused(Refusal::StorageUnavailable)
        ));
        memory.fail.store(false, Ordering::SeqCst);
        assert_eq!(shared_inventory(&mut host).workspaces[0].name, original);
        assert!(matches!(
            host.request(device, device, Request::Mutate { sequence: 1, mutation }),
            Reply::Outcome(record) if matches!(record.outcome, spaceterm_server::hierarchy::Outcome::Accepted { .. })
        ));
        assert_eq!(shared_inventory(&mut host).workspaces[0].name, "Lost");
        host.shutdown().unwrap();
    }

    #[test]
    fn same_run_organization_recovery_keeps_a_fresh_pane_waiting_for_first_use() {
        let mut host = SharedWorkspaceHost::new(host());
        host.prepare_initial_organization().unwrap();
        let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
        let record = retained(&host);
        host.restore_organization(Some(&record));
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::SelectPane {
                    pane,
                    explicit: false,
                    size: size(),
                    style: None,
                }
            ),
            Reply::Restored {
                fresh: true,
                session_ended: false,
                ..
            }
        ));
        assert!(
            shared_inventory(&mut host)
                .close
                .unwrap()
                .sessions
                .iter()
                .all(|pane| pane.session.is_none()),
            "passive observation starts no shell"
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn end_all_sessions_clears_runtime_and_preserves_control_without_incidental_restart() {
        use spaceterm_server::hierarchy::{Mutation, Outcome};
        use spaceterm_server::pane::control::Controller;
        let mut host = SharedWorkspaceHost::new(host());
        host.prepare_initial_organization().unwrap();
        host.admitted([1; 32], "Studio".into());
        let original = shared_inventory(&mut host);
        let pane = original.workspaces[0].tabs[0].root_pane;
        let select = |explicit| Request::SelectPane {
            pane,
            explicit,
            size: size(),
            style: None,
        };
        let Reply::Attached { lease } = host.request([1; 32], [1; 32], select(true)) else {
            panic!("explicit first use");
        };
        host.disconnected([1; 32]);
        let mutation = Mutation::EndAllSessions {
            confirmation: shared_inventory(&mut host).close.unwrap(),
        };
        assert!(matches!(host.request([2; 32], [2; 32], Request::Mutate {
            sequence: 1, mutation: mutation.clone(),
        }), Reply::Outcome(result) if result.outcome == Outcome::Accepted { target: lease.server }));
        let ended = shared_inventory(&mut host);
        assert!(
            ended
                .close
                .unwrap()
                .sessions
                .iter()
                .all(|p| p.session.is_none()),
            "no runtime survives End All Sessions"
        );
        assert_eq!(ended.workspaces, original.workspaces);
        assert!(matches!(
            host.request([1; 32], [3; 32], select(false)),
            Reply::Restored {
                fresh: false,
                session_ended: true,
                ..
            }
        ));
        assert!(
            matches!(host.request([2; 32], [2; 32], select(true)), Reply::Takeover {
            controller: Controller::Client { name, .. }, ..
        } if name.as_str() == "Studio")
        );
        assert!(matches!(
            screen(&mut host, [1; 32], lease),
            Err(PaneEvent::SessionEnded)
        ));
        let Reply::Attached { lease: replacement } = host.request([1; 32], [3; 32], select(true))
        else {
            panic!("controller explicitly starts replacement");
        };
        assert_ne!(replacement.session, lease.session);
        assert!(matches!(
            host.request(
                [2; 32],
                [2; 32],
                Request::Mutate {
                    sequence: 1,
                    mutation
                }
            ),
            Reply::Outcome(_)
        ));
        assert!(matches!(
            host.request([2; 32], [2; 32], Request::Resolve { sequence: 1 }),
            Reply::Outcome(_)
        ));
        assert_eq!(
            shared_inventory(&mut host).close.unwrap().sessions[0].session,
            Some(replacement.session)
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn retained_empty_organization_is_prepared_even_without_a_previous_mutation() {
        let original = SharedWorkspaceHost::new(host());
        let record = retained(&original);
        let mut restored = SharedWorkspaceHost::new(host());
        restored.restore_organization(Some(&record));
        restored.prepare_initial_organization().unwrap();
        assert!(shared_inventory(&mut restored).workspaces.is_empty());
        assert!(matches!(
            restored.request(
                [1; 32],
                [1; 32],
                Request::Select {
                    create: true,
                    size: size(),
                    resume: None,
                    style: None,
                }
            ),
            Reply::Refused(Refusal::Missing)
        ));
        restored.shutdown().unwrap();
    }

    #[test]
    fn clients_learn_who_controls_a_pane_as_control_moves_and_ends() {
        use spaceterm_server::hierarchy::{Axis, Mutation, Outcome};
        use spaceterm_server::pane::control::Controller;
        let mut host = SharedWorkspaceHost::new(host());
        host.prepare_initial_organization().unwrap();
        let inventory = shared_inventory(&mut host);
        let (workspace, tab) = (&inventory.workspaces[0], &inventory.workspaces[0].tabs[0]);
        let pane = tab.root_pane;
        host.admitted([1; 32], "Studio".into());
        host.admitted([2; 32], "Laptop".into());
        let select = |pane, explicit| Request::SelectPane {
            pane,
            explicit,
            size: size(),
            style: None,
        };
        let observe = |host: &mut SharedWorkspaceHost, client: u8, pane| match host.request(
            [client; 32],
            [client; 32],
            select(pane, false),
        ) {
            Reply::Takeover { controller, .. } => controller,
            _ => panic!("client {client} should be offered Take Over"),
        };
        let named = |name: &str| Controller::Client {
            name: name.into(),
            this_client: false,
        };
        let Reply::Attached { lease: first } = host.request([1; 32], [1; 32], select(pane, true))
        else {
            panic!("first use starts the shell");
        };
        assert_eq!(observe(&mut host, 2, pane), named("Studio"));

        let Reply::Attached { lease: second } = host.request(
            [2; 32],
            [2; 32],
            Request::TakeOver {
                target: first.into(),
                size: size(),
                style: None,
            },
        ) else {
            panic!("Take Over moves control");
        };
        assert_eq!(observe(&mut host, 1, pane), named("Laptop"));
        host.admitted([2; 32], "Renamed Laptop".into());
        assert_eq!(
            observe(&mut host, 1, pane),
            named("Renamed Laptop"),
            "a client's latest admission names it"
        );

        assert!(matches!(
            host.request([2; 32], [2; 32], Request::Release { lease: second }),
            Reply::Unchanged
        ));
        assert_eq!(observe(&mut host, 1, pane), Controller::Nobody);
        assert_eq!(observe(&mut host, 2, pane), Controller::Nobody);

        let Reply::Outcome(split) = host.request(
            [2; 32],
            [2; 32],
            Request::Mutate {
                sequence: 1,
                mutation: Mutation::SplitPane {
                    workspace: workspace.id,
                    tab: tab.id,
                    source: pane,
                    axis: Axis::Down,
                },
            },
        ) else {
            panic!("split unavailable");
        };
        let Outcome::Accepted { target: created } = split.outcome else {
            panic!("split refused");
        };
        assert_eq!(
            observe(&mut host, 1, created),
            named("Renamed Laptop"),
            "the client that created a Pane controls it before its shell starts"
        );
        host.revoked([2; 32]);
        assert!(matches!(
            host.request([1; 32], [1; 32], select(created, false)),
            Reply::Restored { .. }
        ));
        assert!(matches!(
            host.request([1; 32], [1; 32], select(created, true)),
            Reply::Attached { .. }
        ));
        let Reply::Attached { lease } = host.request([1; 32], [1; 32], select(pane, true)) else {
            panic!("unclaimed Pane remains available");
        };
        host.revoked([2; 32]);
        assert!(screen(&mut host, [1; 32], lease).is_ok());
        host.shutdown().unwrap();
    }

    #[test]
    fn retained_attachments_observe_dormant_restart_then_resume_only_running_owned_work() {
        let mut original = SharedWorkspaceHost::new(host());
        original.prepare_initial_organization().unwrap();
        let initial = shared_inventory(&mut original);
        let workspace = &initial.workspaces[0];
        let pane = workspace.tabs[0].root_pane;
        let attachment = Attachment {
            workspace: workspace.id,
            generation: 1,
        };
        let claim = |explicit| Request::ClaimPane {
            attachment,
            pane,
            claim: AttachmentClaim::Select { explicit },
            size: size(),
            style: None,
        };
        let Reply::Attached { lease: old } = original.request([1; 32], [1; 32], claim(true)) else {
            panic!("first selection starts a shell");
        };
        original.shutdown().unwrap();
        let record = retained(&original);
        let mut restored = SharedWorkspaceHost::new(host());
        restored.restore_organization(Some(&record));
        restored.prepare_initial_organization().unwrap();
        assert_eq!(
            shared_inventory(&mut restored).workspaces,
            initial.workspaces
        );
        assert!(matches!(
            restored.request([1; 32], [2; 32], claim(false)),
            Reply::Restored {
                fresh: false,
                session_ended: false,
                ..
            }
        ));
        assert!(
            shared_inventory(&mut restored)
                .close
                .unwrap()
                .sessions
                .iter()
                .all(|p| p.session.is_none())
        );
        assert!(matches!(
            restored.request(
                [1; 32],
                [2; 32],
                Request::ClaimPane {
                    attachment,
                    pane,
                    claim: AttachmentClaim::Start { server: old.server },
                    size: size(),
                    style: None,
                }
            ),
            Reply::Refused(Refusal::Stale)
        ));
        let Reply::Attached { lease: started } = restored.request([1; 32], [2; 32], claim(true))
        else {
            panic!("explicit first use starts the dormant Pane");
        };
        assert_ne!(started.server, old.server);
        assert_ne!(started.session, old.session);
        restored.disconnected([2; 32]);
        let Reply::Attached { lease: resumed } = restored.request([1; 32], [3; 32], claim(false))
        else {
            panic!("reopen resumes the running owned Session");
        };
        assert_eq!(resumed.session, started.session);
        assert!(matches!(
            restored.request([2; 32], [4; 32], claim(false)),
            Reply::Takeover { .. }
        ));
        assert_eq!(
            shared_inventory(&mut restored).close.unwrap().sessions[0].session,
            Some(started.session)
        );
        restored.shutdown().unwrap();
    }
}
