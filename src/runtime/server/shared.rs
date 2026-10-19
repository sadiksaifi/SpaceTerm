//! Shared hierarchy ownership composed with the existing fenced Pane runtime.
use super::*;
use spaceterm_server::hierarchy::{CloseInventory, ConfirmedPane, Id, Inventory, State};
use std::collections::BTreeMap;

mod worktrees;

pub struct SharedWorkspaceHost {
    writes: worktrees::Writes,
    prototype: DevelopmentPaneHost,
    repositories: super::repositories::ServerRepositories,
    state: State,
    panes: BTreeMap<Id, DevelopmentPaneHost>,
    store: Option<OrganizationStore>,
    uncertain: bool,
    organization_prepared: bool,
    attachment_generations: BTreeMap<(Id, Id), (u64, u64)>,
}
impl SharedWorkspaceHost {
    pub fn local() -> Result<Self, ServerError> {
        Ok(Self::new(DevelopmentPaneHost::local()?))
    }

    /// Compose an account service whose new terminals follow the current login agent.
    pub fn account_service() -> Result<Self, ServerError> {
        Ok(Self::new(DevelopmentPaneHost::local_with_pty_factory(
            crate::platform::service_pty_factory(),
        )?))
    }
    pub(super) fn new(prototype: DevelopmentPaneHost) -> Self {
        let mut state = State::empty();
        state.begin_boot(prototype.boot);
        let repositories = super::repositories::ServerRepositories::new(prototype.boot);
        Self {
            prototype,
            writes: worktrees::Writes::new(),
            repositories,
            state,
            panes: BTreeMap::new(),
            store: None,
            uncertain: false,
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
            next = State::from_legacy(&organization);
            next.worktree_settings = settings;
            next.begin_boot(self.prototype.boot);
        }
        self.commit(next)
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
        self.organization_prepared = true;
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
            self.restore_organization(record.as_ref());
            self.uncertain = false;
        }
        Ok(())
    }
    fn restore_organization(&mut self, record: Option<&Organization>) {
        self.state = record.map_or_else(State::empty, State::from_legacy);
        // Recovery in the same boot preserves creator reservations; a restart clears them.
        self.state.begin_boot(self.prototype.boot);
        self.organization_prepared = record.is_some();
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
                if host.pane.is_some()
                    && let Some(path) = &org.confirmed_directory
                {
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
        self.panes.retain(|id, h| {
            self.state.pane(*id).is_some() && (h.pane.is_some() || h.directory_unavailable)
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
                directories: super::directories::DirectoryReads::default(),
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
        if pane.creator.is_some_and(|creator| creator != device) {
            return self.select(device, connection, id, false, size, style);
        }
        if target.original != super::repositories::pane_directory(pane) {
            return Err(Refusal::Stale);
        }
        let directory = local_filesystem()
            .validate_directory(replacement.as_deref().unwrap_or(&target.original))
            .map_err(|_| Refusal::DirectoryUnavailable)?;
        if replacement.is_some() {
            let mut next = self.state.clone();
            next.repair_directory(id, directory.path().to_owned())?;
            // Commit just this Pane's directory before launching. Uncertain writes start nothing.
            self.commit(next)?;
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
    fn handle(&mut self, device: Id, connection: Id, request: Request) -> Result<Reply, Refusal> {
        if matches!(request, Request::Inventory) {
            self.repositories.subscribe(connection);
        }
        self.finish_worktrees(false)?;
        self.synchronize()?;
        match request {
            Request::WorktreeRead(read) => Ok(Reply::WorktreeRead(
                self.worktree_read(device, connection, read),
            )),
            Request::DirectoryRead(request) => Ok(Reply::DirectoryRead(
                self.prototype.directories.request(connection, request),
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
                accessibility: true,
                directory_recovery: true,
                worktree_writes: true,
                workspace_directories: true,
                workspace_policy: true,
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
                        if old.outcome == spaceterm_server::hierarchy::Outcome::Pending {
                            Ok(Reply::MutationPending)
                        } else {
                            Ok(Reply::Outcome(old.clone()))
                        }
                    } else {
                        Err(Refusal::Stale)
                    };
                }
                if let spaceterm_server::hierarchy::Mutation::Worktree(operation) = mutation {
                    return self.begin_worktree(device, sequence, operation);
                }
                if !self.state.pending_worktrees().is_empty() {
                    return Ok(Reply::MutationPending);
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
            Request::Resolve { sequence }
                if self.state.outcome(device, sequence).is_some_and(|o| {
                    o.outcome == spaceterm_server::hierarchy::Outcome::Pending
                }) =>
            {
                Ok(Reply::MutationPending)
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
                if matches!(request, Request::Poll { .. })
                    && self
                        .panes
                        .values()
                        .any(|host| host.failed_directory_session == Some(target))
                {
                    return Err(Refusal::DirectoryUnavailable);
                }
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
impl PaneService for SharedWorkspaceHost {
    fn restore(&mut self, store: OrganizationStore) -> Result<(), ServerError> {
        let record = store.read()?;
        self.restore_organization(record.as_ref());
        // Existing organization predates seeding and must never be overwritten by a client.
        if self.state.worktree_settings.is_none() && record.is_some() {
            self.state.worktree_settings = Some(Default::default());
        }
        self.store = Some(store);
        self.prepare_initial_organization()
            .map_err(|_| ServerError::StorageUnavailable)
    }
    fn tick(&mut self) {
        let _ = self.finish_worktrees(false);
        self.prototype.directories.tick();
        let _ = self.synchronize();
    }
    fn request(&mut self, device: Id, connection: Id, request: Request) -> Reply {
        self.handle(device, connection, request)
            .unwrap_or_else(Reply::Refused)
    }
    fn disconnected(&mut self, connection: Id) {
        self.cancel_worktree_reads(connection);
        self.prototype.directories.cancel_connection(connection);
        self.repositories.disconnected(connection);
        for host in self.panes.values_mut() {
            host.disconnected(connection);
        }
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{host, shared_inventory, size};
    use super::*;
    use spaceterm_server::attachments::Attachment;

    fn retained(host: &SharedWorkspaceHost) -> Organization {
        serde_json::from_slice(
            &serde_json::to_vec(&host.state.envelope(&host.prototype.directory).unwrap()).unwrap(),
        )
        .unwrap()
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
            Reply::Restored { .. }
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
