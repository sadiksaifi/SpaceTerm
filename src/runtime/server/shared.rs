//! Shared hierarchy ownership composed with the existing fenced Pane runtime.
use super::*;
use spaceterm_server::hierarchy::{CloseInventory, ConfirmedPane, Id, Inventory, State};
use std::collections::BTreeMap;

pub struct SharedWorkspaceHost {
    prototype: DevelopmentPaneHost,
    repositories: super::repositories::ServerRepositories,
    state: State,
    panes: BTreeMap<Id, DevelopmentPaneHost>,
    store: Option<OrganizationStore>,
    uncertain: bool,
    attachment_generations: BTreeMap<(Id, Id), (u64, u64)>,
}
impl SharedWorkspaceHost {
    pub fn local() -> Result<Self, ServerError> {
        Ok(Self::new(DevelopmentPaneHost::local()?))
    }
    pub(super) fn new(prototype: DevelopmentPaneHost) -> Self {
        let mut state = State::empty();
        state.begin_boot(prototype.boot);
        let repositories = super::repositories::ServerRepositories::new(prototype.boot);
        Self {
            prototype,
            repositories,
            state,
            panes: BTreeMap::new(),
            store: None,
            uncertain: false,
            attachment_generations: BTreeMap::new(),
        }
    }
    fn commit(&mut self, mut next: State) -> Result<(), Refusal> {
        // Runtime facts authorize an in-memory comparison, never a later recovery read.
        next.inventory.close = None;
        next.inventory.repositories = None;
        if let Some(store) = &self.store {
            let envelope = next
                .envelope(&self.prototype.directory)
                .map_err(|_| Refusal::StorageUnavailable)?;
            if store.write(Some(&envelope)).is_err() {
                self.uncertain = true;
                return Err(Refusal::StorageUnavailable);
            }
        }
        self.state = next;
        Ok(())
    }
    fn recover(&mut self) -> Result<(), Refusal> {
        if self.uncertain {
            let record = self
                .store
                .as_ref()
                .ok_or(Refusal::StorageUnavailable)?
                .read()
                .map_err(|_| Refusal::StorageUnavailable)?;
            self.state = record
                .as_ref()
                .map_or_else(State::empty, State::from_legacy);
            self.state.begin_boot(self.prototype.boot);
            self.uncertain = false;
        }
        Ok(())
    }
    fn synchronize(&mut self) -> Result<(), Refusal> {
        self.recover()?;
        // Durable deletion is the cleanup intent. An uncertain commit is read back before any
        // process is stopped, and an incomplete stop blocks replies and new work until resolved.
        for (id, host) in &mut self.panes {
            if self.state.pane(*id).is_none() && host.pane.as_ref().is_some_and(|p| !p.ended) {
                host.shutdown().map_err(|_| Refusal::CleanupUnresolved)?;
            }
        }
        let mut next = self.state.clone();
        for (id, host) in &mut self.panes {
            host.retain_directory()?;
            host.retire_exited()?;
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
            self.commit(next)?;
        }
        // Keep an exited runtime until its controller has observed its final screen.
        self.panes.retain(|_, h| h.pane.is_some());
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
            };
            self.panes.insert(id, host);
        }
        self.panes.get_mut(&id).ok_or(Refusal::Missing)
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
        let host = self.pane_host(id)?;
        if host.pane.is_none() && creator.is_some_and(|owner| owner != device) {
            return Ok(Reply::Takeover {
                target: PaneTarget {
                    server: host.boot,
                    session: id,
                },
            });
        }
        // Passive subscriptions may resume existing grants, but cannot acquire or start work.
        if !explicit && host.pane.as_ref().is_some_and(|p| p.device != Some(device)) {
            return Ok(Reply::Takeover {
                target: host.pane.as_ref().ok_or(Refusal::Missing)?.lease.into(),
            });
        }
        host.select(device, connection, explicit, size, None, style)
    }
    fn handle(&mut self, device: Id, connection: Id, request: Request) -> Result<Reply, Refusal> {
        if matches!(request, Request::Inventory) {
            self.repositories.subscribe(connection);
        }
        self.synchronize()?;
        match request {
            Request::ClaimPane {
                attachment,
                pane: id,
                claim,
                size,
                style,
            } => {
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
                // A new attachment observes an old grant without resuming it. Selection can only
                // acquire unclaimed work; Take Over remains explicit.
                if matches!(claim, AttachmentClaim::Select { explicit: false })
                    && let Some(pane) = &host.pane
                    && pane.device == Some(device)
                    && pane.attachment_generation != Some(attachment.generation)
                {
                    return Ok(Reply::Takeover {
                        target: pane.lease.into(),
                    });
                }
                let reply = match claim {
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
                            Reply::Attached {
                                lease: host.start(device, connection, size, style)?,
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
            Request::Capabilities => Ok(Reply::Capabilities {
                development_pane: true,
                pane_takeover: true,
                interactive_input: true,
                native_effects: true,
                reading: true,
                graphics: true,
            }),
            Request::Inventory => Ok(Reply::Inventory {
                inventory: self.inventory(),
                next_sequence: self.state.next_sequence(device),
            }),
            Request::Mutate { sequence, mutation } => {
                // Replay is resolved before filesystem validation; a changed directory cannot
                // turn an already committed pin into a different outcome.
                if let Some(old) = self.state.outcome(device, sequence) {
                    return if old.mutation == mutation {
                        Ok(Reply::Outcome(old.clone()))
                    } else {
                        Err(Refusal::Stale)
                    };
                }
                let mut next = self.state.clone();
                next.inventory = self.inventory();
                next.apply(
                    device,
                    sequence,
                    mutation,
                    &self.prototype.directory,
                    |path| local_filesystem().validate_directory(path).is_ok(),
                )?;
                let outcome = next
                    .outcome(device, sequence)
                    .ok_or(Refusal::Failed)?
                    .clone();
                self.commit(next)?;
                self.synchronize()?;
                Ok(Reply::Outcome(outcome))
            }
            Request::Resolve { sequence } => Ok(Reply::Outcome(
                self.state
                    .outcome(device, sequence)
                    .ok_or(Refusal::Missing)?
                    .clone(),
            )),
            Request::Acknowledge { sequence } => {
                let mut next = self.state.clone();
                next.acknowledge(device, sequence)?;
                self.commit(next)?;
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
                if target.server != self.prototype.boot {
                    return Err(Refusal::Stale);
                }
                let host = self.pane_host(target.session)?;
                if host.pane.is_some() {
                    return Err(Refusal::Stale);
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
                let id = self.legacy_pane(device, create)?;
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
                let id = self.legacy_pane(device, create)?;
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
                    Request::ReadingStatus { lease }
                    | Request::Read { lease, .. }
                    | Request::GraphicsChunk { lease, .. }
                    | Request::SelectionChunk { lease, .. }
                    | Request::Poll { lease }
                    | Request::NativeEffects { lease, .. }
                    | Request::CompleteClipboard { lease, .. }
                    | Request::Focus { lease, .. }
                    | Request::Input { lease, .. }
                    | Request::Key { lease, .. }
                    | Request::Resize { lease, .. }
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
                    .ok_or(Refusal::Missing)?;
                let reply = host.handle(device, connection, request)?;
                self.synchronize()?;
                Ok(reply)
            }
        }
    }
    fn legacy_pane(&mut self, device: Id, create: bool) -> Result<Id, Refusal> {
        if self.state.inventory.workspaces.is_empty() && create {
            let mut next = self.state.clone();
            next.ensure_first_workspace(device, &self.prototype.directory)?;
            self.commit(next)?;
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
impl PaneService for SharedWorkspaceHost {
    fn restore(&mut self, store: OrganizationStore) -> Result<(), ServerError> {
        self.state = store
            .read()?
            .as_ref()
            .map_or_else(State::empty, State::from_legacy);
        // This also fences a later recovery read of the previous boot's retained record.
        self.state.begin_boot(self.prototype.boot);
        self.store = Some(store);
        Ok(())
    }
    fn tick(&mut self) {
        let _ = self.synchronize();
    }
    fn request(&mut self, device: Id, connection: Id, request: Request) -> Reply {
        self.handle(device, connection, request)
            .unwrap_or_else(Reply::Refused)
    }
    fn disconnected(&mut self, connection: Id) {
        self.repositories.disconnected(connection);
        for host in self.panes.values_mut() {
            host.disconnected(connection);
        }
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
        self.repositories.shutdown();
        self.synchronize()
            .map_err(|_| ServerError::StorageUnavailable)?;
        for host in self.panes.values_mut() {
            host.shutdown()?;
        }
        self.synchronize()
            .map_err(|_| ServerError::StorageUnavailable)?;
        Ok(())
    }
}
