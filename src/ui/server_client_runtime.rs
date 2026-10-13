//! The application-scoped desktop client runtime.
//!
//! It admits local UI requests, at most [`MAX_PENDING_UI_REQUESTS`] at a time, and forwards each
//! one to the injected [`DesktopClientProvider`]. Client-core and client-view supply the
//! production provider. Production composition constructs none until native credential delivery
//! exists, so no window presents server state it is not authorized to show.
//!
//! The provider reports between requests through a [`DesktopClientNotifier`]. Frame changes share
//! one wakeup slot and name each changed server once. Subscribers then read that server's latest
//! frame from the provider. Operation outcomes travel separately and are never coalesced.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "production composition constructs the runtime once a client provider exists"
    )
)]

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use gpui::{App, Context, EventEmitter, Task};

use super::server_desktop_contracts::{
    DesktopClientError, DesktopRequest, DesktopServerFrame, OperationId, Outcome, PaneId,
    PaneViewToken, ServerId, UiRequestId,
};

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;

/// Local UI requests the runtime holds at once. The next one is refused before any effect.
pub(crate) const MAX_PENDING_UI_REQUESTS: usize = 128;

/// Operation outcomes queued for the runtime at once. A full queue makes the provider wait. No
/// outcome is dropped, merged or reordered, and client-core retains each one durably meanwhile.
pub(crate) const MAX_QUEUED_OUTCOMES: usize = 128;

/// The client-core and client-view services one runtime presents.
pub(crate) trait DesktopClientProvider {
    /// The latest coherent publication for `server`.
    fn frame(&self, server: ServerId) -> Option<Arc<DesktopServerFrame>>;

    /// The comparison token of `pane`'s current presentation.
    fn pane_view_token(&self, pane: PaneId) -> Result<PaneViewToken, DesktopClientError>;

    /// Starts one admitted request without blocking on network or filesystem IO. A refusal
    /// returns before any effect. The task resolves when the local request finishes, with the
    /// durable operation it submitted, if any. It must not update the runtime.
    fn begin(
        &self,
        request: DesktopRequest,
        cx: &mut App,
    ) -> Result<Task<Result<Option<OperationId>, DesktopClientError>>, DesktopClientError>;

    /// Saves the personal view and closes client connections. It never releases control,
    /// detaches a Workspace, or ends server work.
    fn prepare_quit(&self, cx: &mut App) -> Task<Result<(), DesktopClientError>>;
}

/// One operation outcome client-core retains whether or not a window still listens.
#[derive(Clone, PartialEq)]
pub(crate) struct OperationOutcomeNotice {
    pub(crate) server: ServerId,
    pub(crate) operation: OperationId,
    pub(crate) outcome: Arc<Outcome>,
}

/// Names the outcome kind only. Outcomes can carry names, directories and other field values.
impl std::fmt::Debug for OperationOutcomeNotice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OperationOutcomeNotice")
            .field("server", &self.server)
            .field("operation", &self.operation)
            .field("outcome", &format_args!("{}", outcome_kind(&self.outcome)))
            .finish()
    }
}

/// The provider's side of one runtime's notifications. It may move to client-core's threads.
#[derive(Clone)]
pub(crate) struct DesktopClientNotifier {
    changed: Arc<Mutex<BTreeSet<ServerId>>>,
    wakeup: async_channel::Sender<()>,
    outcomes: async_channel::Sender<OperationOutcomeNotice>,
}

impl DesktopClientNotifier {
    /// Records that `server` has a newer frame. It never blocks. Changes the runtime has not read
    /// yet share one pending wakeup and name each server once.
    pub(crate) fn frame_changed(&self, server: ServerId) {
        self.changed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(server);
        // A full slot already holds a wakeup that reads this change. A closed one has no runtime.
        self.wakeup.try_send(()).ok();
    }

    /// Delivers one operation outcome. It waits while [`MAX_QUEUED_OUTCOMES`] are queued and
    /// never drops one. It returns [`DesktopClientError::Cancelled`] once the runtime is gone.
    pub(crate) async fn operation_outcome(
        &self,
        notice: OperationOutcomeNotice,
    ) -> Result<(), DesktopClientError> {
        self.outcomes
            .send(notice)
            .await
            .map_err(|_| DesktopClientError::Cancelled)
    }
}

/// The runtime's side of its notifications.
pub(crate) struct DesktopClientNotifications {
    changed: Arc<Mutex<BTreeSet<ServerId>>>,
    wakeup: async_channel::Receiver<()>,
    outcomes: async_channel::Receiver<OperationOutcomeNotice>,
}

/// One runtime's notification channel: a single frame wakeup slot and a bounded outcome queue.
pub(crate) fn notification_channel() -> (DesktopClientNotifier, DesktopClientNotifications) {
    let changed = Arc::new(Mutex::new(BTreeSet::new()));
    let (wakeup, wakeups) = async_channel::bounded(1);
    let (outcome_sender, outcomes) = async_channel::bounded(MAX_QUEUED_OUTCOMES);
    (
        DesktopClientNotifier {
            changed: Arc::clone(&changed),
            wakeup,
            outcomes: outcome_sender,
        },
        DesktopClientNotifications {
            changed,
            wakeup: wakeups,
            outcomes,
        },
    )
}

pub(crate) struct DesktopClientDependencies {
    pub(crate) provider: Rc<dyn DesktopClientProvider>,
    pub(crate) notifications: DesktopClientNotifications,
}

#[derive(Clone, PartialEq)]
pub(crate) enum DesktopClientEvent {
    /// `server` has a newer frame. Read it with [`DesktopClientRuntime::frame`].
    FrameChanged(ServerId),
    RequestCompleted {
        request: UiRequestId,
        operation: Option<OperationId>,
    },
    RequestFailed {
        request: UiRequestId,
        error: DesktopClientError,
    },
    OperationOutcome {
        server: ServerId,
        operation: OperationId,
        outcome: Arc<Outcome>,
    },
}

/// Names identities, error classes and the outcome kind only.
impl std::fmt::Debug for DesktopClientEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrameChanged(server) => formatter
                .debug_tuple("DesktopClientEvent::FrameChanged")
                .field(server)
                .finish(),
            Self::RequestCompleted { request, operation } => formatter
                .debug_struct("DesktopClientEvent::RequestCompleted")
                .field("request", request)
                .field("operation", operation)
                .finish(),
            Self::RequestFailed { request, error } => formatter
                .debug_struct("DesktopClientEvent::RequestFailed")
                .field("request", request)
                .field("error", error)
                .finish(),
            Self::OperationOutcome {
                server,
                operation,
                outcome,
            } => formatter
                .debug_struct("DesktopClientEvent::OperationOutcome")
                .field("server", server)
                .field("operation", operation)
                .field("outcome", &format_args!("{}", outcome_kind(outcome)))
                .finish(),
        }
    }
}

fn outcome_kind(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Applied => "Applied",
        Outcome::CreatedWorkspace { .. } => "CreatedWorkspace",
        Outcome::CreatedTab { .. } => "CreatedTab",
        Outcome::CreatedPane { .. } => "CreatedPane",
        Outcome::CreatedWorktree { .. } => "CreatedWorktree",
        Outcome::ControlAcquired(_) => "ControlAcquired",
        Outcome::ControlUnavailable(_) => "ControlUnavailable",
        Outcome::StartupAwaitingMeasurement { .. } => "StartupAwaitingMeasurement",
        Outcome::SessionStarted(_) => "SessionStarted",
        Outcome::Assessed { .. } => "Assessed",
        Outcome::Conflict(_) => "Conflict",
        Outcome::Failed(_) => "Failed",
        Outcome::CreatedWithControl(_) => "CreatedWithControl",
        Outcome::ControlGranted(_) => "ControlGranted",
        Outcome::Assessment(_) => "Assessment",
        Outcome::Pairing(_) => "Pairing",
        Outcome::Git(_) => "Git",
    }
}

pub(crate) struct DesktopClientRuntime {
    provider: Rc<dyn DesktopClientProvider>,
    next_request: UiRequestId,
    pending: HashMap<UiRequestId, Task<()>>,
    _frames: Task<()>,
    _outcomes: Task<()>,
}

impl EventEmitter<DesktopClientEvent> for DesktopClientRuntime {}

impl DesktopClientRuntime {
    pub(crate) fn new(dependencies: DesktopClientDependencies, cx: &mut Context<Self>) -> Self {
        let DesktopClientNotifications {
            changed,
            wakeup,
            outcomes,
        } = dependencies.notifications;
        let frames = cx.spawn(async move |this, cx| {
            while wakeup.recv().await.is_ok() {
                let servers =
                    std::mem::take(&mut *changed.lock().unwrap_or_else(PoisonError::into_inner));
                let delivered = this.update(cx, |_, cx| {
                    for server in servers {
                        cx.emit(DesktopClientEvent::FrameChanged(server));
                    }
                });
                if delivered.is_err() {
                    break;
                }
            }
        });
        let outcomes = cx.spawn(async move |this, cx| {
            while let Ok(notice) = outcomes.recv().await {
                let event = DesktopClientEvent::OperationOutcome {
                    server: notice.server,
                    operation: notice.operation,
                    outcome: notice.outcome,
                };
                if this.update(cx, |_, cx| cx.emit(event)).is_err() {
                    break;
                }
            }
        });
        Self {
            provider: dependencies.provider,
            next_request: UiRequestId::FIRST,
            pending: HashMap::new(),
            _frames: frames,
            _outcomes: outcomes,
        }
    }

    pub(crate) fn frame(&self, server: ServerId) -> Option<Arc<DesktopServerFrame>> {
        self.provider.frame(server)
    }

    pub(crate) fn pane_view_token(&self, pane: PaneId) -> Result<PaneViewToken, DesktopClientError> {
        self.provider.pane_view_token(pane)
    }

    /// Admits one local request and returns its local identity. It performs no blocking IO.
    pub(crate) fn request(
        &mut self,
        request: DesktopRequest,
        cx: &mut Context<Self>,
    ) -> Result<UiRequestId, DesktopClientError> {
        if self.pending.len() >= MAX_PENDING_UI_REQUESTS {
            return Err(DesktopClientError::AdmissionFull);
        }
        let id = self.next_request;
        let next = id
            .checked_successor()
            .ok_or(DesktopClientError::CounterExhausted)?;
        let completion = self.provider.begin(request, cx)?;
        self.next_request = next;
        let finished = cx.spawn(async move |this, cx| {
            let event = match completion.await {
                Ok(operation) => DesktopClientEvent::RequestCompleted {
                    request: id,
                    operation,
                },
                Err(error) => DesktopClientEvent::RequestFailed { request: id, error },
            };
            this.update(cx, |runtime, cx| {
                runtime.pending.remove(&id);
                cx.emit(event);
            })
            .ok();
        });
        self.pending.insert(id, finished);
        Ok(id)
    }

    /// Prepares this client to quit. Control, attachments and server work stay as they are.
    pub(crate) fn prepare_client_quit(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DesktopClientError>> {
        self.provider.prepare_quit(cx)
    }
}
