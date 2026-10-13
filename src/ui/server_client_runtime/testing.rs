//! A recording client provider. It exists only in test composition.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{App, Task};

use super::DesktopClientProvider;
use crate::ui::server_desktop_contracts::{
    DesktopClientError, DesktopRequest, DesktopServerFrame, DesktopServerInfo, OperationId,
    Outcome, PaneId, PaneViewToken, ServerId,
};

type Completion = Result<Option<OperationId>, DesktopClientError>;

/// Records every request it begins and finishes each one only when a test says so.
#[derive(Default)]
pub(crate) struct RecordingClientProvider {
    begun: RefCell<Vec<DesktopRequest>>,
    completions: RefCell<Vec<async_channel::Sender<Completion>>>,
    refusal: Cell<Option<DesktopClientError>>,
    quits: Cell<usize>,
    frames: RefCell<BTreeMap<ServerId, Arc<DesktopServerFrame>>>,
    servers: RefCell<Vec<DesktopServerInfo>>,
    outcomes: RefCell<BTreeMap<OperationId, Arc<Outcome>>>,
    saves: Cell<usize>,
    outcome_revisions:
        RefCell<BTreeMap<OperationId, spaceterm_core::authority::OrganizationRevision>>,
    save_failure: Cell<Option<DesktopClientError>>,
    directories: RefCell<
        BTreeMap<ServerId, Arc<dyn spaceterm_client::directories::DirectoryConnectionPort>>,
    >,
    pane_tokens: RefCell<BTreeMap<PaneId, PaneViewToken>>,
}

impl RecordingClientProvider {
    pub(crate) fn set_directory_connection(
        &self,
        server: ServerId,
        port: Arc<dyn spaceterm_client::directories::DirectoryConnectionPort>,
    ) {
        self.directories.borrow_mut().insert(server, port);
    }
    pub(crate) fn set_pane_token(&self, token: PaneViewToken) {
        self.pane_tokens.borrow_mut().insert(token.pane, token);
    }
    pub(crate) fn set_servers(&self, servers: Vec<DesktopServerInfo>) {
        *self.servers.borrow_mut() = servers;
    }
    pub(crate) fn set_frame(&self, frame: Arc<DesktopServerFrame>) {
        self.frames.borrow_mut().insert(frame.server, frame);
    }
    pub(crate) fn set_outcome(&self, operation: OperationId, outcome: Arc<Outcome>) {
        self.outcomes.borrow_mut().insert(operation, outcome);
    }
    pub(crate) fn set_outcome_at(
        &self,
        operation: OperationId,
        outcome: Arc<Outcome>,
        revision: spaceterm_core::authority::OrganizationRevision,
    ) {
        self.set_outcome(operation, outcome);
        self.outcome_revisions
            .borrow_mut()
            .insert(operation, revision);
    }
    pub(crate) fn fail_save(&self, error: DesktopClientError) {
        self.save_failure.set(Some(error));
    }
    pub(crate) fn saves(&self) -> usize {
        self.saves.get()
    }

    pub(crate) fn begun(&self) -> Vec<DesktopRequest> {
        self.begun.borrow().clone()
    }

    /// Refuses every later request with `error` before any effect.
    pub(crate) fn refuse_with(&self, error: DesktopClientError) {
        self.refusal.set(Some(error));
    }

    /// Finishes the request begun at `index`, counting from zero. It panics when the runtime no
    /// longer awaits that request or it already finished.
    pub(crate) fn finish(&self, index: usize, completion: Completion) {
        let sender = self.completions.borrow()[index].clone();
        sender
            .try_send(completion)
            .expect("the runtime still awaits the request, which finishes once");
    }

    pub(crate) fn quits(&self) -> usize {
        self.quits.get()
    }
}

impl DesktopClientProvider for RecordingClientProvider {
    fn servers(&self) -> Vec<DesktopServerInfo> {
        self.servers.borrow().clone()
    }
    fn frame(&self, server: ServerId) -> Option<Arc<DesktopServerFrame>> {
        self.frames.borrow().get(&server).cloned()
    }
    fn outcome(&self, operation: OperationId) -> Option<Arc<Outcome>> {
        self.outcomes.borrow().get(&operation).cloned()
    }
    fn outcome_revision(
        &self,
        operation: OperationId,
    ) -> Option<spaceterm_core::authority::OrganizationRevision> {
        self.outcome_revisions.borrow().get(&operation).copied()
    }
    fn save_view(&self, _cx: &mut App) -> Task<Result<(), DesktopClientError>> {
        self.saves.set(self.saves.get() + 1);
        Task::ready(self.save_failure.get().map_or(Ok(()), Err))
    }

    fn directory_connection(
        &self,
        server: ServerId,
    ) -> Result<Arc<dyn spaceterm_client::directories::DirectoryConnectionPort>, DesktopClientError>
    {
        self.directories
            .borrow()
            .get(&server)
            .cloned()
            .ok_or(DesktopClientError::UnsupportedCapability)
    }
    fn pane_view_token(&self, pane: PaneId) -> Result<PaneViewToken, DesktopClientError> {
        self.pane_tokens
            .borrow()
            .get(&pane)
            .copied()
            .ok_or(DesktopClientError::TargetGone)
    }

    fn begin(
        &self,
        request: DesktopRequest,
        cx: &mut App,
    ) -> Result<Task<Completion>, DesktopClientError> {
        if let Some(error) = self.refusal.get() {
            return Err(error);
        }
        let (sender, receiver) = async_channel::bounded(1);
        self.begun.borrow_mut().push(request);
        self.completions.borrow_mut().push(sender);
        Ok(cx.spawn(async move |_| {
            receiver
                .recv()
                .await
                .unwrap_or(Err(DesktopClientError::Cancelled))
        }))
    }

    fn prepare_quit(&self, _cx: &mut App) -> Task<Result<(), DesktopClientError>> {
        self.quits.set(self.quits.get() + 1);
        Task::ready(self.save_failure.get().map_or(Ok(()), Err))
    }
}
