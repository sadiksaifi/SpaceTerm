//! The application-scoped desktop client runtime.
//!
//! It admits local UI requests, at most [`MAX_PENDING_UI_REQUESTS`] at a time, and forwards each
//! one to the injected [`DesktopClientProvider`]. Client-core and client-view supply the
//! composed provider over the recovered client registry, journal, personal view, and authenticated
//! Pane ports. Native enrollment installs it once; window reopening reuses it. Missing native
//! composition fails closed rather than substituting a local Terminal or test connection.
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
    DesktopClientError, DesktopRequest, DesktopServerFrame, DesktopServerInfo, DesktopViewRevision,
    OperationId, Outcome, PaneId, PaneViewToken, ServerId, UiRequestId,
};

pub(crate) mod composed;

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
    /// Paired servers in saved order, including those with no attachments. Pending self-unpair
    /// bindings are absent. The runtime presents the local server first without changing ownership.
    fn servers(&self) -> Vec<DesktopServerInfo> {
        Vec::new()
    }

    /// Binds the UI identity before beginning, so a personal application receipt can name the
    /// exact request. Providers that apply personal edits override this entry point.
    fn begin_ui_request(
        &self,
        _id: UiRequestId,
        request: DesktopRequest,
        cx: &mut App,
    ) -> Result<Task<Result<Option<OperationId>, DesktopClientError>>, DesktopClientError> {
        self.begin(request, cx)
    }

    /// The latest coherent publication for `server`.
    fn frame(&self, server: ServerId) -> Option<Arc<DesktopServerFrame>>;

    /// The comparison token of `pane`'s current presentation.
    fn pane_view_token(&self, pane: PaneId) -> Result<PaneViewToken, DesktopClientError>;

    /// The authenticated disposable presentation channel for an attached Pane.
    fn pane_channel(
        &self,
        _pane: PaneId,
    ) -> Result<spaceterm_client::pane_channel::PaneChannel, DesktopClientError> {
        Err(DesktopClientError::UnsupportedCapability)
    }

    /// Borrowed authenticated reads from the paired server's existing connection.
    fn directory_connection(
        &self,
        _server: ServerId,
    ) -> Result<Arc<dyn spaceterm_client::directories::DirectoryConnectionPort>, DesktopClientError>
    {
        Err(DesktopClientError::UnsupportedCapability)
    }

    /// Restores a retained repair before a window may submit a replacement operation.
    fn retained_pane_repair(
        &self,
        _pane: PaneId,
        _cx: &mut App,
    ) -> Result<Option<(OperationId, spaceterm_core::operations::Command)>, DesktopClientError>
    {
        Ok(None)
    }

    /// A portable decision and the exact channel observation that produced its facts.
    fn pane_presentation(
        &self,
        _pane: PaneId,
        _displayed: Option<spaceterm_core::authority::SessionKey>,
    ) -> Result<PanePresentationDelivery, DesktopClientError> {
        Err(DesktopClientError::UnsupportedCapability)
    }

    fn pane_measurement(
        &self,
        _pane: PaneId,
    ) -> Option<super::server_desktop_contracts::PaneMeasurementRequest> {
        None
    }

    /// Revalidates pending use after native context or personal-view changes. It begins none.
    fn revalidate_pane(&self, _pane: PaneId, _cx: &mut App) {}

    /// Starts one admitted request without blocking on network or filesystem IO. A refusal
    /// returns before any effect. The task resolves when the local request finishes, with the
    /// durable operation it submitted, if any. It must not update the runtime.
    fn begin(
        &self,
        request: DesktopRequest,
        cx: &mut App,
    ) -> Result<Task<Result<Option<OperationId>, DesktopClientError>>, DesktopClientError>;

    /// The client-core tracker retains this outcome independently of UI subscriptions. A caller
    /// reads it after learning the original operation identity, including outcome-before-completion.
    fn outcome(&self, _operation: OperationId) -> Option<Arc<Outcome>> {
        None
    }

    /// The original outcome's publication revision, retained by the client operation tracker.
    /// A frame's current revision is not evidence of this operation's result revision.
    fn outcome_revision(
        &self,
        _operation: OperationId,
    ) -> Option<spaceterm_core::authority::OrganizationRevision> {
        None
    }

    /// Saves personal view state without closing the application-scoped connections.
    fn save_view(&self, _cx: &mut App) -> Task<Result<(), DesktopClientError>> {
        Task::ready(Err(DesktopClientError::LocalStorageUnavailable))
    }

    /// Saves the personal view and closes client connections. It never releases control,
    /// detaches a Workspace, or ends server work.
    fn prepare_quit(&self, cx: &mut App) -> Task<Result<(), DesktopClientError>>;
}

pub(crate) struct PanePresentationDelivery {
    pub(crate) token: PaneViewToken,
    pub(crate) observation: spaceterm_client::pane_channel::PaneChannelState,
    pub(crate) decision: spaceterm_client::view::PresentationDecision,
    pub(crate) device: super::pane_presentation::DisplayName,
    pub(crate) cause: super::pane_presentation::OfflineCause,
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

enum ProviderNotice {
    Outcome(OperationOutcomeNotice),
    PersonalEditApplied {
        request: UiRequestId,
        revision: DesktopViewRevision,
    },
}

/// The provider's side of one runtime's notifications. It may move to client-core's threads.
#[derive(Clone)]
pub(crate) struct DesktopClientNotifier {
    changed: Arc<Mutex<BTreeSet<ServerId>>>,
    wakeup: async_channel::Sender<()>,
    outcomes: async_channel::Sender<ProviderNotice>,
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

    /// Reports the exact publication that first includes an applied personal request. Delivery
    /// is bounded and never coalesced with frame wakeups or replaced by a completion event.
    pub(crate) async fn personal_edit_applied(
        &self,
        request: UiRequestId,
        revision: DesktopViewRevision,
    ) -> Result<(), DesktopClientError> {
        self.outcomes
            .send(ProviderNotice::PersonalEditApplied { request, revision })
            .await
            .map_err(|_| DesktopClientError::Cancelled)
    }

    /// Delivers one operation outcome. It waits while [`MAX_QUEUED_OUTCOMES`] are queued and
    /// never drops one. It returns [`DesktopClientError::Cancelled`] once the runtime is gone.
    pub(crate) async fn operation_outcome(
        &self,
        notice: OperationOutcomeNotice,
    ) -> Result<(), DesktopClientError> {
        self.outcomes
            .send(ProviderNotice::Outcome(notice))
            .await
            .map_err(|_| DesktopClientError::Cancelled)
    }
}

/// The runtime's side of its notifications.
pub(crate) struct DesktopClientNotifications {
    changed: Arc<Mutex<BTreeSet<ServerId>>>,
    wakeup: async_channel::Receiver<()>,
    outcomes: async_channel::Receiver<ProviderNotice>,
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
    /// Explicit provider application receipt. It is independent of request completion and
    /// frame delivery. No observer infers it from equal view contents.
    PersonalEditApplied {
        request: UiRequestId,
        revision: DesktopViewRevision,
    },
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
            Self::PersonalEditApplied { request, revision } => formatter
                .debug_struct("DesktopClientEvent::PersonalEditApplied")
                .field("request", request)
                .field("revision", revision)
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
    pending: HashMap<UiRequestId, (bool, Task<()>)>,
    saving: bool,
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
                let event = match notice {
                    ProviderNotice::Outcome(notice) => DesktopClientEvent::OperationOutcome {
                        server: notice.server,
                        operation: notice.operation,
                        outcome: notice.outcome,
                    },
                    ProviderNotice::PersonalEditApplied { request, revision } => {
                        DesktopClientEvent::PersonalEditApplied { request, revision }
                    }
                };
                if this
                    .update(cx, |_, cx| {
                        cx.emit(event);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            provider: dependencies.provider,
            next_request: UiRequestId::FIRST,
            pending: HashMap::new(),
            saving: false,
            _frames: frames,
            _outcomes: outcomes,
        }
    }

    pub(crate) fn servers(&self) -> Vec<DesktopServerInfo> {
        let mut servers = self.provider.servers();
        servers.sort_by_key(|info| !info.local);
        servers
    }

    pub(crate) fn frame(&self, server: ServerId) -> Option<Arc<DesktopServerFrame>> {
        self.provider.frame(server)
    }

    pub(crate) fn pane_view_token(
        &self,
        pane: PaneId,
    ) -> Result<PaneViewToken, DesktopClientError> {
        self.provider.pane_view_token(pane)
    }

    pub(crate) fn pane_channel(
        &self,
        pane: PaneId,
    ) -> Result<spaceterm_client::pane_channel::PaneChannel, DesktopClientError> {
        self.provider.pane_channel(pane)
    }

    pub(crate) fn directory_connection(
        &self,
        server: ServerId,
    ) -> Result<Arc<dyn spaceterm_client::directories::DirectoryConnectionPort>, DesktopClientError>
    {
        self.provider.directory_connection(server)
    }

    pub(crate) fn retained_pane_repair(
        &self,
        pane: PaneId,
        cx: &mut App,
    ) -> Result<Option<(OperationId, spaceterm_core::operations::Command)>, DesktopClientError>
    {
        self.provider.retained_pane_repair(pane, cx)
    }

    pub(crate) fn pane_presentation(
        &self,
        pane: PaneId,
        displayed: Option<spaceterm_core::authority::SessionKey>,
    ) -> Result<PanePresentationDelivery, DesktopClientError> {
        self.provider.pane_presentation(pane, displayed)
    }

    pub(crate) fn pane_measurement(
        &self,
        pane: PaneId,
    ) -> Option<super::server_desktop_contracts::PaneMeasurementRequest> {
        self.provider.pane_measurement(pane)
    }

    pub(crate) fn revalidate_pane(&self, pane: PaneId, cx: &mut App) {
        self.provider.revalidate_pane(pane, cx);
    }

    /// Admits one local request and returns its local identity. It performs no blocking IO.
    pub(crate) fn request(
        &mut self,
        request: DesktopRequest,
        cx: &mut Context<Self>,
    ) -> Result<UiRequestId, DesktopClientError> {
        if self.saving {
            return Err(DesktopClientError::Cancelled);
        }
        if self.pending.len() >= MAX_PENDING_UI_REQUESTS {
            return Err(DesktopClientError::AdmissionFull);
        }
        let id = self.next_request;
        let next = id
            .checked_successor()
            .ok_or(DesktopClientError::CounterExhausted)?;
        let local_durability = matches!(
            request,
            DesktopRequest::PersonalEdit(_)
                | DesktopRequest::AttachWorkspace(_)
                | DesktopRequest::DetachWorkspace(_)
                | DesktopRequest::RetryDetachWorkspace { .. }
        );
        let completion = self.provider.begin_ui_request(id, request, cx)?;
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
                cx.notify();
            })
            .ok();
        });
        self.pending.insert(id, (local_durability, finished));
        Ok(id)
    }

    /// Network-dependent tasks remain owned, but only local durability can delay saving.
    pub(crate) fn has_pending_local_requests(&self) -> bool {
        self.pending.values().any(|(local, _)| *local)
    }

    pub(crate) fn outcome(&self, operation: OperationId) -> Option<Arc<Outcome>> {
        self.provider.outcome(operation)
    }
    pub(crate) fn outcome_revision(
        &self,
        operation: OperationId,
    ) -> Option<spaceterm_core::authority::OrganizationRevision> {
        self.provider.outcome_revision(operation)
    }

    pub(crate) fn save_view(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DesktopClientError>> {
        self.save(false, cx)
    }

    fn save(
        &mut self,
        quitting: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DesktopClientError>> {
        if self.saving {
            return Task::ready(Err(DesktopClientError::AdmissionFull));
        }
        self.saving = true;
        let (signal, wakeups) = async_channel::bounded(1);
        let observer = cx.observe(&cx.entity(), move |_, _, _| {
            signal.try_send(()).ok();
        });
        cx.spawn(async move |owner, cx| {
            let _observer = observer;
            loop {
                let pending = owner
                    .update(cx, |runtime, _| runtime.has_pending_local_requests())
                    .map_err(|_| DesktopClientError::Cancelled)?;
                if !pending {
                    break;
                }
                wakeups
                    .recv()
                    .await
                    .map_err(|_| DesktopClientError::Cancelled)?;
            }
            let saved = owner
                .update(cx, |runtime, cx| {
                    if quitting {
                        runtime.provider.prepare_quit(cx)
                    } else {
                        runtime.provider.save_view(cx)
                    }
                })
                .map_err(|_| DesktopClientError::Cancelled)?;
            let result = saved.await;
            let _ = owner.update(cx, |runtime, cx| {
                runtime.saving = quitting && result.is_ok();
                cx.notify();
            });
            result
        })
    }

    /// Prepares this client to quit. Control, attachments and server work stay as they are.
    pub(crate) fn prepare_client_quit(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DesktopClientError>> {
        self.save(true, cx)
    }
}

/// Installed only after native client composition has established credential and journal authority.
pub(crate) struct InstalledDesktopClient(pub(crate) gpui::Entity<DesktopClientRuntime>);
impl gpui::Global for InstalledDesktopClient {}
