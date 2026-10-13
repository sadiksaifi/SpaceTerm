//! A recording client provider. It exists only in test composition.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use gpui::{App, Task};

use super::DesktopClientProvider;
use crate::ui::server_desktop_contracts::{
    DesktopClientError, DesktopRequest, DesktopServerFrame, OperationId, PaneId, PaneViewToken,
    ServerId,
};

type Completion = Result<Option<OperationId>, DesktopClientError>;

/// Records every request it begins and finishes each one only when a test says so.
#[derive(Default)]
pub(crate) struct RecordingClientProvider {
    begun: RefCell<Vec<DesktopRequest>>,
    completions: RefCell<Vec<async_channel::Sender<Completion>>>,
    refusal: Cell<Option<DesktopClientError>>,
    quits: Cell<usize>,
}

impl RecordingClientProvider {
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
    fn frame(&self, _server: ServerId) -> Option<Arc<DesktopServerFrame>> {
        None
    }

    fn pane_view_token(&self, _pane: PaneId) -> Result<PaneViewToken, DesktopClientError> {
        Err(DesktopClientError::TargetGone)
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
        Task::ready(Ok(()))
    }
}
