//! Shared hierarchy ownership composed with the existing fenced Pane runtime.
use super::*;
use spaceterm_server::hierarchy::{Id, State};
use std::collections::BTreeMap;

pub struct SharedWorkspaceHost {
    prototype: DevelopmentPaneHost,
    state: State,
    panes: BTreeMap<Id, DevelopmentPaneHost>,
    store: Option<OrganizationStore>,
    uncertain: bool,
}
impl SharedWorkspaceHost {
    pub fn local() -> Result<Self, ServerError> {
        Ok(Self::new(DevelopmentPaneHost::local()?))
    }
    pub(super) fn new(prototype: DevelopmentPaneHost) -> Self {
        Self {
            prototype,
            state: State::empty(),
            panes: BTreeMap::new(),
            store: None,
            uncertain: false,
        }
    }
    fn commit(&mut self, next: State) -> Result<(), Refusal> {
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
            self.uncertain = false;
        }
        Ok(())
    }
    fn synchronize(&mut self) -> Result<(), Refusal> {
        self.recover()?;
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
        if next != self.state {
            self.commit(next)?;
        }
        // Keep an exited runtime until its controller has observed its final screen.
        self.panes.retain(|_, h| h.pane.is_some());
        Ok(())
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
        self.synchronize()?;
        match request {
            Request::Capabilities => Ok(Reply::Capabilities {
                development_pane: true,
                pane_takeover: true,
            }),
            Request::Inventory => Ok(Reply::Inventory {
                inventory: self.state.inventory.clone(),
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
                    Request::Poll { lease }
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
        // Control grants are live state. A restarted server begins with every Pane unclaimed.
        for w in &mut self.state.inventory.workspaces {
            for t in &mut w.tabs {
                for p in &mut t.panes {
                    p.creator = None;
                }
            }
        }
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
        for host in self.panes.values_mut() {
            host.disconnected(connection);
        }
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
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
