//! Application-scoped server read ports and recovery of original Pane repairs.

use super::{Inner, native_ports};
use crate::ui::server_desktop_contracts::*;
use gpui::App;
use spaceterm_client::api::ConnectionState;
use spaceterm_core::authority::ConnectionBinding;
use std::{rc::Rc, sync::Arc};

#[derive(Clone, Copy)]
pub(super) struct GitPublication {
    connection: ConnectionBinding,
    epoch: ServerEpoch,
    attempt: u64,
    revision: OrganizationRevision,
}

/// Covers the interval before canonical operation allocation becomes durable.
/// The runtime owns admitted tasks even when their window closes.
pub(super) struct RepairAdmission {
    pub(super) pane: PaneId,
    pub(super) owner: std::rc::Weak<Inner>,
}
impl Drop for RepairAdmission {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner
                .state
                .borrow_mut()
                .repair_admissions
                .remove(&self.pane);
        }
    }
}

impl Inner {
    /// Publications are delivered at application scope, even without an attached window.
    pub(super) fn sync_git(&self, server: ServerId, cx: &mut App) {
        let source = &self.sources[&server];
        let current = match *source.state.borrow() {
            ConnectionState::Connected { info }
                if self
                    .tracker
                    .journal()
                    .ordinary_use_allowed(&source.binding)
                    .unwrap_or(false)
                    && source.replica.borrow().is_ready() =>
            {
                Some(info)
            }
            _ => None,
        };
        let snapshot = source.replica.borrow().retained().cloned();
        let ready = current.zip(snapshot).filter(|(info, snapshot)| {
            info.server == server
                && info.epoch == snapshot.cursor.epoch
                && info.organization_revision == snapshot.cursor.revision
        });
        let Some((info, snapshot)) = ready else {
            if self.git_publications.borrow_mut().remove(&server).is_some() {
                self.git
                    .update(cx, |git, cx| git.connection_lost(server, cx));
            }
            return;
        };
        let previous = self.git_publications.borrow().get(&server).copied();
        let reconnect = previous.is_none_or(|old| {
            old.connection != info.binding || old.epoch != info.epoch || old.attempt != info.attempt
        });
        if reconnect {
            let client = source
                .handle
                .as_ref()
                .and_then(|handle| handle.queries(self.tracker.journal().clone()).ok())
                .and_then(|queries| {
                    let client = queries
                        .git(self.tracker.clone(), self.operations.clone())
                        .ok()?;
                    spaceterm_client::git::rpc::GitClient::new(
                        server,
                        Arc::new(native_ports::GitPort {
                            client,
                            queries,
                            journal: self.tracker.journal().clone(),
                            executor: self.executor.clone(),
                        }),
                    )
                    .ok()
                });
            let Some(client) = client else {
                return;
            };
            if self
                .git
                .update(cx, |git, cx| git.connect(client, info.epoch, cx))
                .is_err()
            {
                self.git_publications.borrow_mut().remove(&server);
                self.git
                    .update(cx, |git, cx| git.connection_lost(server, cx));
                return;
            }
        }
        let revision = snapshot.cursor.revision;
        if let Some(old) = previous.filter(|old| old.revision != revision) {
            self.git.update(cx, |git, cx| {
                git.organization_changed(server, info.epoch, old.revision, revision, cx);
            });
        }
        self.git_publications.borrow_mut().insert(
            server,
            GitPublication {
                connection: info.binding,
                epoch: info.epoch,
                attempt: info.attempt,
                revision,
            },
        );
    }
}

pub(super) fn directory_connection(
    inner: &Rc<Inner>,
    server: ServerId,
) -> Result<Arc<dyn spaceterm_client::directories::DirectoryConnectionPort>, DesktopClientError> {
    let source = inner
        .sources
        .get(&server)
        .ok_or(DesktopClientError::TargetGone)?;
    let handle = source
        .handle
        .as_ref()
        .ok_or(DesktopClientError::UnsupportedCapability)?;
    let queries = handle
        .queries(inner.tracker.journal().clone())
        .map_err(DesktopClientError::from)?;
    Ok(Arc::new(native_ports::DirectoryPort {
        queries,
        executor: inner.executor.clone(),
    }))
}

pub(super) fn retained_pane_repair(
    inner: &Rc<Inner>,
    pane: PaneId,
    cx: &mut App,
) -> Result<Option<(OperationId, Command)>, DesktopClientError> {
    use spaceterm_client::journal::RetainedOperationState;
    use spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest;
    let source = inner
        .sources
        .get(&pane.server())
        .ok_or(DesktopClientError::TargetGone)?;
    let journal = inner.tracker.journal();
    if !journal
        .ordinary_use_allowed(&source.binding)
        .map_err(DesktopClientError::from)?
    {
        return Err(DesktopClientError::AccessDenied);
    }
    let state = journal.snapshot().map_err(DesktopClientError::from)?;
    let stream = state
        .bindings()
        .iter()
        .find(|saved| {
            saved
                .binding()
                .is_ok_and(|binding| binding == source.binding)
        })
        .ok_or(DesktopClientError::AccessDenied)?
        .streams()
        .parts()
        .ordinary;
    let published = source
        .replica
        .borrow()
        .retained()
        .map(|snapshot| snapshot.cursor.revision);
    for record in state.operations().iter().filter(|record| {
        record.binding.id.stream() == stream
            && match record.state {
                RetainedOperationState::Pending => true,
                RetainedOperationState::Completed {
                    publication_revision,
                    ..
                } => published.is_none_or(|revision| revision < publication_revision),
                RetainedOperationState::Retired => false,
            }
    }) {
        let request = CanonicalOperationRequest::decode(&record.request, journal.codec())
            .map_err(|_| DesktopClientError::LocalStorageUnavailable)?;
        if matches!(request.command(), Command::RepairPaneDirectory { pane: target, .. } if *target == pane)
        {
            let ticket = inner
                .tracker
                .ticket(record.binding.id)
                .map_err(DesktopClientError::from)?;
            inner.watch_operation(ticket, cx);
            return Ok(Some((record.binding.id, request.command().clone())));
        }
    }
    if inner.state.borrow().repair_admissions.contains(&pane) {
        return Err(DesktopClientError::AdmissionFull);
    }
    Ok(None)
}
