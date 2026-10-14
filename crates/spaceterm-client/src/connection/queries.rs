//! Correlated calls on the existing authenticated worker and bounded writer.
//! Returned server directories and Git facts confer no client-local authority.

mod account_runtime;

use crate::{
    api::{ConnectedInfo, ConnectionState},
    directories::{ClientDirectoryFailure, DirectoryConnectionPort},
    failure::{ClientFailure, ClientFailureClass, exhausted, invalid_peer, resource_limit},
    git::rpc::{
        ClientGitFailure, GitClientFuture, GitConnectionState, GitReadResult, GitReadiness,
        GitSubscriptionEvent, GitSubscriptionPort,
    },
    journal::JournalController,
    management_cli::{
        bindings::AuthenticatedManagementContext,
        ports::{ManagementClientKind, ManagementFailure, ManagementFuture, StatusBackend},
        status::{StatusResync, StatusSubscription},
    },
    registry::{ServerHandle, Shared, same_connection},
};
use spaceterm_core::{
    management::{ServerStatus, StatusPublication, StatusQuery},
    server_directories::{
        DirectoryContext, DirectoryContextParts, DirectoryQuery, DirectoryQueryResult,
    },
    worktrees::{
        GitQuery, GitStatusTarget,
        queries::{GitCorrelation, GitQueryRequest},
    },
};
use spaceterm_protocol::{
    codec::Payload,
    connection::AuthenticatedConnection,
    convert::{directories, git, management},
    wire::{v1, v2},
};
use std::{
    collections::BTreeMap,
    num::NonZeroU64,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    sync::{oneshot, watch},
    time::Instant,
};

const READ_DEADLINE: Duration = Duration::from_secs(5);
const MAX_READS: usize = 4;

type Authentication = (ConnectedInfo, Weak<Mutex<AuthenticatedConnection>>);
#[derive(Default)]
pub(crate) struct QueryHub {
    state: Mutex<HubState>,
}
#[derive(Default)]
struct HubState {
    next: u64,
    authentication: Option<Authentication>,
    entries: BTreeMap<u64, Entry>,
}
impl HubState {
    fn allocate(&mut self) -> Result<u64, ClientFailure> {
        self.entries.retain(|_, entry| match &entry.reply {
            Reply::One(reply) => !reply.is_closed(),
            Reply::Status(_, _) => true,
        });
        if self.entries.len() >= MAX_READS {
            return Err(resource_limit());
        }
        self.next = self.next.checked_add(1).ok_or_else(exhausted)?;
        Ok(self.next)
    }
}
struct Entry {
    info: ConnectedInfo,
    message: Payload,
    journal: Arc<JournalController>,
    reply: Reply,
}
enum Reply {
    One(oneshot::Sender<Result<Payload, ClientFailure>>),
    Status(StatusSubscription, Arc<StatusRefresh>),
}
impl QueryHub {
    pub(crate) fn authenticated(
        &self,
        info: ConnectedInfo,
        codec: &Arc<Mutex<AuthenticatedConnection>>,
    ) -> Result<(), ClientFailure> {
        self.state
            .lock()
            .map_err(|_| invalid_peer())?
            .authentication = Some((info, Arc::downgrade(codec)));
        Ok(())
    }
    pub(crate) fn invalidate(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.authentication = None;
            let entries = std::mem::take(&mut state.entries);
            drop(state);
            for (request, entry) in entries {
                match entry.reply {
                    Reply::One(reply) => {
                        let _ =
                            reply.send(Err(ClientFailure::new(ClientFailureClass::StaleAuthority)));
                    }
                    Reply::Status(subscription, refresh) => {
                        let _ = subscription.apply(StatusPublication::ResyncRequired {
                            request: NonZeroU64::new(request).unwrap_or(NonZeroU64::MIN),
                            generation: entry.info.attempt,
                        });
                        refresh.deadline.send_replace(None);
                    }
                }
            }
        }
    }
    fn allocate(&self) -> Result<u64, ClientFailure> {
        let mut state = self.state.lock().map_err(|_| invalid_peer())?;
        state.allocate()
    }
    fn status_subscription(
        self: &Arc<Self>,
        server: &ServerHandle,
        journal: &Arc<JournalController>,
        info: ConnectedInfo,
    ) -> Result<(StatusSubscription, Option<Payload>), ManagementFailure> {
        // The server has one status interest per connection. Registration and
        // observer acquisition share a lock, including concurrent subscribers.
        let mut state = self
            .state
            .lock()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        for entry in state.entries.values() {
            if same_connection(info, entry.info)
                && Arc::ptr_eq(journal, &entry.journal)
                && let Reply::Status(subscription, _) = &entry.reply
                && let Some(subscriber) = subscription.subscriber()
            {
                return Ok((subscriber, None));
            }
        }
        let request = state.allocate().map_err(management_failure)?;
        let query = StatusQuery {
            request: NonZeroU64::new(request).ok_or(ManagementFailure::LimitExceeded)?,
            generation: info.attempt,
            connection: info.binding,
            known_epoch: None,
            known_revision: None,
            subscribe: true,
        };
        let (deadline, _) = watch::channel(Some(Instant::now() + READ_DEADLINE));
        let resync = Arc::new(StatusRefresh {
            shared: server.shared.clone(),
            hub: Arc::downgrade(self),
            server: info.server,
            request,
            deadline,
        });
        let subscription = StatusSubscription::new(info.server, query, resync.clone())?;
        let message = status_query(info, request, true).map_err(management_failure)?;
        state.entries.insert(
            request,
            Entry {
                info,
                message: message.clone(),
                journal: journal.clone(),
                reply: Reply::Status(subscription.publisher(), resync.clone()),
            },
        );
        resync.watch_deadline();
        Ok((subscription, Some(message)))
    }
    fn insert(&self, request: u64, entry: Entry) -> Result<(), ClientFailure> {
        let mut state = self.state.lock().map_err(|_| invalid_peer())?;
        // Allocation does not hold a capacity permit across encoding. Concurrent
        // callers recheck the simultaneous ceiling when installing their request.
        if state.entries.len() >= MAX_READS || state.entries.contains_key(&request) {
            return Err(resource_limit());
        }
        state.entries.insert(request, entry);
        Ok(())
    }
    fn remove(&self, request: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.entries.remove(&request);
        }
    }
    pub(crate) fn admit(
        &self,
        info: ConnectedInfo,
        message: &Payload,
        binding: &crate::api::PairedServerBinding,
    ) -> Result<(), ClientFailure> {
        let (request, generation) = query_correlation(message).ok_or_else(invalid_peer)?;
        let state = self.state.lock().map_err(|_| invalid_peer())?;
        let entry = state
            .entries
            .get(&request)
            .ok_or_else(|| ClientFailure::new(ClientFailureClass::StaleAuthority))?;
        if !same_connection(info, entry.info)
            || generation != info.attempt
            || &entry.message != message
            || !entry.journal.ordinary_use_allowed(binding)?
        {
            return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
        }
        Ok(())
    }
    pub(crate) fn receive(
        &self,
        info: ConnectedInfo,
        message: &Payload,
    ) -> Result<bool, ClientFailure> {
        let Some((request, generation)) = response_correlation(message) else {
            return Ok(false);
        };
        let mut state = self.state.lock().map_err(|_| invalid_peer())?;
        let Some(entry) = state.entries.get(&request) else {
            // Cancelled and old-generation replies never satisfy another request.
            return Ok(true);
        };
        if !same_connection(info, entry.info) || generation != entry.info.attempt {
            return Ok(true);
        }
        let expected = query_correlation(&entry.message).ok_or_else(invalid_peer)?;
        if expected != (request, generation) {
            return Err(invalid_peer());
        }
        match &entry.reply {
            Reply::One(_) => {
                if !matching_reply(&entry.message, message) {
                    return Err(invalid_peer());
                }
                let entry = state.entries.remove(&request).ok_or_else(invalid_peer)?;
                if let Reply::One(reply) = entry.reply {
                    let _ = reply.send(Ok(message.clone()));
                }
            }
            Reply::Status(subscription, refresh) => {
                let subscription = subscription.clone();
                let journal = entry.journal.clone();
                let publication = decode_status(message, &journal)?;
                let refresh = refresh.clone();
                let resync_response =
                    matches!(publication, StatusPublication::ResyncRequired { .. });
                if matches!(publication, StatusPublication::Snapshot { .. }) || resync_response {
                    refresh.deadline.send_replace(None);
                }
                drop(state);
                subscription
                    .apply(publication)
                    .map_err(|_| invalid_peer())?;
                if resync_response {
                    refresh.request_resync();
                }
            }
        }
        Ok(true)
    }
}
struct ReadGuard {
    hub: Arc<QueryHub>,
    request: u64,
}
impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.hub.remove(self.request);
    }
}

/// One bounded page from the current authenticated pairing owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairedDevicePage {
    pub version: spaceterm_core::authority::FieldVersion,
    pub this_device: spaceterm_core::identity::DeviceId,
    pub total: u64,
    pub devices: Vec<spaceterm_core::pairing::PairingTarget>,
    pub next: Option<spaceterm_core::identity::PairingId>,
}

/// Queries use this handle's connection, canonical journal and response owner.
/// No transport, credential provider or retry ledger is created here.
#[derive(Clone)]
pub struct ServerQueries {
    server: ServerHandle,
    journal: Arc<JournalController>,
}
impl ServerQueries {
    pub(crate) fn new(
        server: ServerHandle,
        journal: Arc<JournalController>,
    ) -> Result<Self, ClientFailure> {
        if !journal.ordinary_use_allowed(&server.binding)? {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        Ok(Self { server, journal })
    }
    pub(crate) fn info(&self) -> Result<ConnectedInfo, ClientFailure> {
        if !self.journal.ordinary_use_allowed(&self.server.binding)? {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        match *self.server.state().borrow() {
            ConnectionState::Connected { info } if self.server.replica().borrow().is_ready() => {
                Ok(info)
            }
            _ => Err(ClientFailure::new(ClientFailureClass::Unavailable)),
        }
    }
    pub(crate) fn git_state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        self.state().map_err(git_failure)
    }
    pub(crate) fn server(&self) -> &ServerHandle {
        &self.server
    }
    pub(crate) fn journal(&self) -> &Arc<JournalController> {
        &self.journal
    }
    fn state(&self) -> Result<GitConnectionState, ClientFailure> {
        let info = self.info()?;
        Ok(GitConnectionState {
            context: context(info)?,
            generation: info.binding.generation,
            readiness: GitReadiness::Ready,
        })
    }
    async fn request(
        &self,
        info: ConnectedInfo,
        make: impl FnOnce(u64) -> Result<Payload, ClientFailure>,
    ) -> Result<Payload, ClientFailure> {
        let hub = &self.server.queries;
        let request = hub.allocate()?;
        let message = make(request)?;
        let (reply, response) = oneshot::channel();
        hub.insert(
            request,
            Entry {
                info,
                message: message.clone(),
                journal: self.journal.clone(),
                reply: Reply::One(reply),
            },
        )?;
        let _guard = ReadGuard {
            hub: hub.clone(),
            request,
        };
        tokio::time::timeout(READ_DEADLINE, async {
            self.server.dispatch_query(info, message).await?;
            response.await.map_err(|_| invalid_peer())?
        })
        .await
        .map_err(|_| ClientFailure::new(ClientFailureClass::Unavailable))?
    }
    pub async fn git_query(&self, query: GitQuery) -> Result<GitReadResult, ClientGitFailure> {
        let info = self.info().map_err(git_failure)?;
        if !info
            .capabilities
            .supports(spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT)
        {
            return Err(ClientGitFailure::Unavailable);
        }
        query
            .validate_scope(info.server)
            .map_err(ClientGitFailure::Remote)?;
        let reply = self
            .request(info, |request| {
                let request = GitQueryRequest::new(
                    GitCorrelation::new(request, info.attempt).map_err(|_| invalid_peer())?,
                    query,
                    None,
                    false,
                )
                .map_err(|_| invalid_peer())?;
                git::encode_query(info.major, &request).map_err(Into::into)
            })
            .await
            .map_err(git_failure)?;
        let reply = git::decode_response(&reply, info.server)
            .map_err(|_| ClientGitFailure::InvalidResponse)?;
        if !same_connection(info, self.info().map_err(git_failure)?) {
            return Err(ClientGitFailure::StaleResponse);
        }
        Ok(GitReadResult {
            epoch: info.epoch,
            scope: reply.scope,
            result: reply.result.map_err(ClientGitFailure::Remote)?,
        })
    }
    pub fn subscribe_git(
        &self,
        target: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        let info = self.info().map_err(git_failure)?;
        if !info
            .capabilities
            .supports(spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT)
        {
            return Err(ClientGitFailure::Unavailable);
        }
        if target.server() != info.server {
            return Err(ClientGitFailure::WrongServer);
        }
        // The frozen Git grammar has no unsubscribe. Demand-driven bounded
        // queries avoid retaining abandoned server interests or reconnect work.
        Ok(Box::new(GitReceiver {
            reads: self.clone(),
            target,
            first: true,
        }))
    }
    /// Reads authorized devices through this worker. Continuations retain the original store revision.
    pub async fn paired_devices(
        &self,
        after: Option<spaceterm_core::identity::PairingId>,
        version: Option<spaceterm_core::authority::FieldVersion>,
    ) -> Result<PairedDevicePage, ManagementFailure> {
        use spaceterm_core::{identity::*, pairing::*};
        let info = self.info().map_err(management_failure)?;
        if after.is_some() != version.is_some()
            || after.is_some_and(|id| id.server() != info.server)
            || version.is_some_and(|version| version.get() == 0)
        {
            return Err(ManagementFailure::Usage);
        }
        let reply = self
            .request(info, |request| {
                macro_rules! query {
                    ($p:ident, $variant:ident) => {
                        Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::PairingRequest($p::PairingRequest {
                                request,
                                action: Some($p::pairing_request::Action::List(true)),
                                inventory_after: after.map_or(0, PairingId::local),
                                inventory_version: version
                                    .map_or(0, spaceterm_core::authority::FieldVersion::get),
                                request_generation: info.attempt,
                                ..Default::default()
                            })),
                        })
                    };
                }
                match info.major {
                    1 => Ok(query!(v1, Major1)),
                    2 => Ok(query!(v2, Major2)),
                    _ => Err(invalid_peer()),
                }
            })
            .await
            .map_err(management_failure)?;
        if !same_connection(info, self.info().map_err(management_failure)?) {
            return Err(ManagementFailure::Offline);
        }
        macro_rules! decode {
            ($value:ident) => {{
                if $value.classified_failure
                    == spaceterm_protocol::wire::pairing::PairingFailure::PairingStaleAssessment
                        as i32
                {
                    return Err(ManagementFailure::StaleAssessment);
                }
                if $value.classified_failure != 0 {
                    return Err(ManagementFailure::UnsafeNativeState);
                }
                if $value.failure.is_some()
                    || $value.operation.is_some()
                    || $value.inventory_version == 0
                    || $value.devices.len() > 128
                    || $value.inventory_total < $value.devices.len() as u64
                    || $value.inventory_total > spaceterm_core::pairing::MAX_ACTIVE_PAIRINGS as u64
                    || version.is_some_and(|version| version.get() != $value.inventory_version)
                {
                    return Err(ManagementFailure::UnsafeNativeState);
                }
                let this_device = DeviceId::new(
                    inventory_identity($value.this_device.as_ref())
                        .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                );
                if this_device != self.server.binding.target().device() {
                    return Err(ManagementFailure::UnsafeNativeState);
                }
                let mut devices = Vec::with_capacity($value.devices.len());
                let mut previous = after.map_or(0, PairingId::local);
                for value in &$value.devices {
                    let id = inventory_identity(value.pairing.as_ref())
                        .map_err(|_| ManagementFailure::UnsafeNativeState)?;
                    if id[..8] != [0; 8] {
                        return Err(ManagementFailure::UnsafeNativeState);
                    }
                    let local = u64::from_be_bytes(
                        id[8..]
                            .try_into()
                            .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                    );
                    if local <= previous {
                        return Err(ManagementFailure::UnsafeNativeState);
                    }
                    previous = local;
                    let device = DeviceId::new(
                        inventory_identity(value.device.as_ref())
                            .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                    );
                    let pairing = PairingId::new(info.server, local)
                        .map_err(|_| ManagementFailure::UnsafeNativeState)?;
                    devices.push(
                        PairingTarget::new(PairingTargetParts {
                            server: info.server,
                            device,
                            pairing,
                        })
                        .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                    );
                }
                let next = if $value.inventory_next == 0 {
                    None
                } else {
                    if devices
                        .last()
                        .is_none_or(|value| value.parts().pairing.local() != $value.inventory_next)
                        || $value.inventory_total <= devices.len() as u64
                    {
                        return Err(ManagementFailure::UnsafeNativeState);
                    }
                    Some(
                        PairingId::new(info.server, $value.inventory_next)
                            .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                    )
                };
                Ok(PairedDevicePage {
                    version: spaceterm_core::authority::FieldVersion::new($value.inventory_version),
                    this_device,
                    total: $value.inventory_total,
                    devices,
                    next,
                })
            }};
        }
        match reply {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::PairingOutcome(value)),
            }) => decode!(value),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::PairingOutcome(value)),
            }) => decode!(value),
            _ => Err(ManagementFailure::UnsafeNativeState),
        }
    }

    pub async fn status(&self) -> Result<ServerStatus, ManagementFailure> {
        let info = self.info().map_err(management_failure)?;
        let reply = self
            .request(info, |request| status_query(info, request, false))
            .await
            .map_err(management_failure)?;
        if !same_connection(info, self.info().map_err(management_failure)?) {
            return Err(ManagementFailure::Offline);
        }
        match decode_status(&reply, &self.journal).map_err(management_failure)? {
            StatusPublication::Snapshot { status, .. }
                if status.parts().server == info.server && status.parts().epoch == info.epoch =>
            {
                Ok(status)
            }
            _ => Err(ManagementFailure::UnsafeNativeState),
        }
    }
    pub async fn subscribe_status(&self) -> Result<StatusSubscription, ManagementFailure> {
        let info = self.info().map_err(management_failure)?;
        let hub = &self.server.queries;
        let (subscription, message) = hub.status_subscription(&self.server, &self.journal, info)?;
        if let Some(message) = message {
            let (request, generation) =
                query_correlation(&message).ok_or(ManagementFailure::UnsafeNativeState)?;
            if let Err(error) = self.server.dispatch_query(info, message).await {
                hub.remove(request);
                let _ = subscription.apply(StatusPublication::ResyncRequired {
                    request: NonZeroU64::new(request)
                        .ok_or(ManagementFailure::UnsafeNativeState)?,
                    generation,
                });
                return Err(management_failure(error));
            }
        }
        Ok(subscription)
    }
    pub fn status_backend(&self, kind: ManagementClientKind) -> ServerStatusBackend {
        ServerStatusBackend {
            queries: self.clone(),
            kind,
        }
    }
}
impl DirectoryConnectionPort for ServerQueries {
    fn state(&self) -> Result<GitConnectionState, ClientDirectoryFailure> {
        self.state().map_err(directory_failure)
    }
    fn query(
        &self,
        query: DirectoryQuery,
    ) -> GitClientFuture<'_, Result<DirectoryQueryResult, ClientDirectoryFailure>> {
        Box::pin(async move {
            let info = self.info().map_err(directory_failure)?;
            if !info
                .capabilities
                .supports(spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES)
            {
                return Err(ClientDirectoryFailure::Unavailable);
            }
            let context = context(info).map_err(directory_failure)?;
            if query.parts().context != context {
                return Err(ClientDirectoryFailure::StaleResponse);
            }
            let response = self
                .request(info, |request| {
                    directories::encode_query(
                        info.major,
                        &directories::DirectoryRequest::new(request, info.attempt, query)
                            .map_err(|_| invalid_peer())?,
                    )
                    .map_err(Into::into)
                })
                .await
                .map_err(directory_failure)?;
            if !same_connection(info, self.info().map_err(directory_failure)?) {
                return Err(ClientDirectoryFailure::StaleResponse);
            }
            directories::decode_response(&response, context)
                .map_err(|_| ClientDirectoryFailure::InvalidResponse)?
                .result
                .map_err(ClientDirectoryFailure::Remote)
        })
    }
}
struct GitReceiver {
    reads: ServerQueries,
    target: GitStatusTarget,
    first: bool,
}
impl GitSubscriptionPort for GitReceiver {
    fn next(
        &mut self,
    ) -> GitClientFuture<'_, Option<Result<GitSubscriptionEvent, ClientGitFailure>>> {
        Box::pin(async move {
            if self.first {
                self.first = false;
            } else {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Some(
                match self
                    .reads
                    .git_query(GitQuery::Status {
                        target: self.target,
                    })
                    .await
                {
                    Ok(read) => Ok(GitSubscriptionEvent::Status(Box::new(read))),
                    Err(ClientGitFailure::Remote(error)) => {
                        Ok(GitSubscriptionEvent::LastKnown(error))
                    }
                    Err(error) => Err(error),
                },
            )
        })
    }
}
#[derive(Clone)]
pub struct ServerStatusBackend {
    queries: ServerQueries,
    kind: ManagementClientKind,
}
impl StatusBackend for ServerStatusBackend {
    fn context(&self) -> Result<AuthenticatedManagementContext, ManagementFailure> {
        let info = self.queries.info().map_err(management_failure)?;
        let state = self
            .queries
            .journal
            .snapshot()
            .map_err(management_failure)?;
        let stream = state
            .bindings()
            .iter()
            .find(|saved| {
                saved
                    .binding()
                    .is_ok_and(|binding| binding == self.queries.server.binding)
            })
            .ok_or(ManagementFailure::PairingRequired)?
            .streams()
            .parts()
            .ordinary;
        let hub = self
            .queries
            .server
            .queries
            .state
            .lock()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        let (authenticated, codec) = hub
            .authentication
            .as_ref()
            .ok_or(ManagementFailure::Offline)?;
        if !same_connection(info, *authenticated) {
            return Err(ManagementFailure::Offline);
        }
        let codec = codec.upgrade().ok_or(ManagementFailure::Offline)?;
        AuthenticatedManagementContext::from_connection(
            &*codec
                .lock()
                .map_err(|_| ManagementFailure::UnsafeNativeState)?,
            stream,
            self.kind,
        )?
        .with_connection_binding(info.binding)
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        Box::pin(self.queries.status())
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(self.queries.subscribe_status())
    }
}
struct StatusRefresh {
    shared: Weak<Shared>,
    hub: Weak<QueryHub>,
    server: spaceterm_core::identity::ServerId,
    request: u64,
    deadline: watch::Sender<Option<Instant>>,
}
impl StatusRefresh {
    /// One deadline owner per subscription, including initial and resync replies.
    /// Replacement queries share the original interest and cannot overlap.
    fn watch_deadline(self: &Arc<Self>) {
        let owner = Arc::downgrade(self);
        let mut changes = self.deadline.subscribe();
        tokio::spawn(async move {
            loop {
                let deadline = *changes.borrow_and_update();
                tokio::select! {
                    result = changes.changed() => { if result.is_err() { return; } },
                    _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } } => {
                        let Some(refresh) = owner.upgrade() else { return; };
                        let mut expired = false;
                        refresh.deadline.send_if_modified(|current| {
                            if *current == deadline { *current = None; expired = true; true } else { false }
                        });
                        if !expired { continue; }
                        let Some(hub) = refresh.hub.upgrade() else { return; };
                        let publication = hub.state.lock().ok().and_then(|state| {
                            let entry = state.entries.get(&refresh.request)?;
                            let Reply::Status(subscription, _) = &entry.reply else { return None; };
                            subscription.has_observers().then(|| (subscription.clone(), entry.info.attempt))
                        });
                        let Some((subscription, generation)) = publication else { return; };
                        let _ = subscription.apply(StatusPublication::ResyncRequired { request: NonZeroU64::new(refresh.request).unwrap_or(NonZeroU64::MIN), generation });
                        refresh.request_resync();
                    },
                }
            }
        });
    }
}
impl StatusResync for StatusRefresh {
    fn cancel(&self) {
        self.deadline.send_replace(None);
        let shared = self.shared.clone();
        let hub = self.hub.clone();
        let request = self.request;
        let server_id = self.server;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let Some(hub) = hub.upgrade() else {
                return;
            };
            let cancellation = {
                let Ok(mut state) = hub.state.lock() else {
                    return;
                };
                let Some(entry) = state.entries.get_mut(&request) else {
                    return;
                };
                match &mut entry.message {
                    Payload::Major1(message) => {
                        if let Some(v1::message::Kind::StatusQuery(query)) = &mut message.kind {
                            query.cancel = true;
                        }
                    }
                    Payload::Major2(message) => {
                        if let Some(v2::message::Kind::StatusQuery(query)) = &mut message.kind {
                            query.cancel = true;
                        }
                    }
                }
                (entry.info, entry.message.clone())
            };
            if let Some(shared) = shared.upgrade()
                && let Some(server) = shared.server(server_id)
            {
                let _ = server.dispatch_query(cancellation.0, cancellation.1).await;
            }
            hub.remove(request);
        });
    }
    fn request_resync(&self) {
        let next = Instant::now() + READ_DEADLINE;
        if !self.deadline.send_if_modified(|deadline| {
            if deadline.is_some() {
                false
            } else {
                *deadline = Some(next);
                true
            }
        }) {
            return;
        }
        let shared = self.shared.clone();
        let hub = self.hub.clone();
        let request = self.request;
        let server_id = self.server;
        let deadline = self.deadline.clone();
        tokio::spawn(async move {
            let result = async {
                let shared = shared.upgrade().ok_or_else(invalid_peer)?;
                let server = shared.server(server_id).ok_or_else(invalid_peer)?;
                let hub = hub.upgrade().ok_or_else(invalid_peer)?;
                let (info, message) = {
                    let state = hub.state.lock().map_err(|_| invalid_peer())?;
                    let entry = state.entries.get(&request).ok_or_else(invalid_peer)?;
                    if !matches!(&entry.reply, Reply::Status(subscription, _) if subscription.has_observers()) { return Err(invalid_peer()); }
                    (entry.info, entry.message.clone())
                };
                server.dispatch_query(info, message).await
            }.await;
            // Queue rejection leaves the connection live. Keep its bounded retry deadline.
            if result.is_err_and(|error| error.class() != ClientFailureClass::ResourceLimit) {
                deadline.send_if_modified(|current| {
                    if *current == Some(next) {
                        *current = None;
                        true
                    } else {
                        false
                    }
                });
            }
        });
    }
}
fn context(info: ConnectedInfo) -> Result<DirectoryContext, ClientFailure> {
    DirectoryContext::new(DirectoryContextParts {
        server: info.server,
        pairing: info.pairing,
        client: info.client,
        epoch: info.epoch,
    })
    .map_err(|_| invalid_peer())
}
fn status_query(
    info: ConnectedInfo,
    request: u64,
    subscribe: bool,
) -> Result<Payload, ClientFailure> {
    let query = StatusQuery {
        request: NonZeroU64::new(request).ok_or_else(exhausted)?,
        generation: info.attempt,
        connection: info.binding,
        known_epoch: None,
        known_revision: None,
        subscribe,
    };
    Ok(match info.major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::StatusQuery(
                management::v1::encode_query(query),
            )),
        }),
        2 => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::StatusQuery(
                management::v2::encode_query(query),
            )),
        }),
        _ => return Err(invalid_peer()),
    })
}
fn decode_status(
    message: &Payload,
    journal: &JournalController,
) -> Result<StatusPublication, ClientFailure> {
    let state = journal.snapshot()?;
    let digest = |operation| {
        state
            .operation(operation)
            .map(|record| record.binding.digest)
            .ok_or(spaceterm_protocol::ProtocolError::Malformed)
    };
    match message {
        Payload::Major1(message) => match &message.kind {
            Some(v1::message::Kind::StatusPublication(value)) => {
                management::v1::decode_publication(value, &digest).map_err(Into::into)
            }
            _ => Err(invalid_peer()),
        },
        Payload::Major2(message) => match &message.kind {
            Some(v2::message::Kind::StatusPublication(value)) => {
                management::v2::decode_publication(value, &digest).map_err(Into::into)
            }
            _ => Err(invalid_peer()),
        },
    }
}
fn query_correlation(message: &Payload) -> Option<(u64, u64)> {
    macro_rules! profile {
        ($message:ident, $p:ident) => {
            match &$message.kind {
                Some($p::message::Kind::GitQuery(value)) => {
                    Some((value.request, value.request_generation))
                }
                Some($p::message::Kind::ServerDirectoryQuery(value)) => {
                    Some((value.request, value.request_generation))
                }
                Some($p::message::Kind::PairingRequest(value))
                    if matches!(value.action, Some($p::pairing_request::Action::List(true)))
                        && value.operation.is_none() =>
                {
                    Some((value.request, value.request_generation))
                }
                Some($p::message::Kind::StatusQuery(value)) => {
                    Some((value.request, value.request_generation))
                }
                Some($p::message::Kind::AccountRuntimeRequest(value)) => {
                    Some((value.request, value.request_generation))
                }
                _ => None,
            }
        };
    }
    match message {
        Payload::Major1(message) => profile!(message, v1),
        Payload::Major2(message) => profile!(message, v2),
    }
}
fn response_correlation(message: &Payload) -> Option<(u64, u64)> {
    macro_rules! profile {
        ($message:ident, $p:ident) => {
            match &$message.kind {
                Some($p::message::Kind::GitQueryResult(value)) => {
                    Some((value.request, value.request_generation))
                }
                Some($p::message::Kind::ServerDirectoryResult(value)) => {
                    Some((value.request, value.request_generation))
                }
                Some($p::message::Kind::PairingOutcome(value)) if value.operation.is_none() => {
                    Some((value.request, value.request_generation))
                }
                Some($p::message::Kind::StatusPublication(value)) => {
                    Some((value.request, value.request_generation))
                }
                Some($p::message::Kind::AccountRuntimeReply(value)) => {
                    Some((value.request, value.request_generation))
                }
                _ => None,
            }
        };
    }
    match message {
        Payload::Major1(message) => profile!(message, v1),
        Payload::Major2(message) => profile!(message, v2),
    }
}
fn matching_reply(query: &Payload, reply: &Payload) -> bool {
    macro_rules! profile {
        ($query:ident, $reply:ident, $p:ident) => {
            matches!(
                (&$query.kind, &$reply.kind),
                (
                    Some($p::message::Kind::GitQuery(_)),
                    Some($p::message::Kind::GitQueryResult(_))
                ) | (
                    Some($p::message::Kind::ServerDirectoryQuery(_)),
                    Some($p::message::Kind::ServerDirectoryResult(_))
                ) | (
                    Some($p::message::Kind::StatusQuery(_)),
                    Some($p::message::Kind::StatusPublication(_))
                ) | (
                    Some($p::message::Kind::PairingRequest(_)),
                    Some($p::message::Kind::PairingOutcome(_))
                ) | (
                    Some($p::message::Kind::AccountRuntimeRequest(_)),
                    Some($p::message::Kind::AccountRuntimeReply(_))
                )
            )
        };
    }
    match (query, reply) {
        (Payload::Major1(query), Payload::Major1(reply)) => profile!(query, reply, v1),
        (Payload::Major2(query), Payload::Major2(reply)) => profile!(query, reply, v2),
        _ => false,
    }
}
pub(crate) fn git_failure(error: ClientFailure) -> ClientGitFailure {
    match error.class() {
        ClientFailureClass::ResourceLimit | ClientFailureClass::CounterExhausted => {
            ClientGitFailure::Capacity
        }
        ClientFailureClass::StaleAuthority => ClientGitFailure::StaleResponse,
        ClientFailureClass::AuthorizationDenied => ClientGitFailure::Unauthorized,
        ClientFailureClass::InvalidPeer | ClientFailureClass::InvalidStorage => {
            ClientGitFailure::InvalidResponse
        }
        _ => ClientGitFailure::Offline,
    }
}
fn directory_failure(error: ClientFailure) -> ClientDirectoryFailure {
    match error.class() {
        ClientFailureClass::ResourceLimit | ClientFailureClass::CounterExhausted => {
            ClientDirectoryFailure::Remote(
                spaceterm_core::server_directories::DirectoryFailure::Capacity,
            )
        }
        ClientFailureClass::StaleAuthority => ClientDirectoryFailure::StaleResponse,
        ClientFailureClass::InvalidPeer | ClientFailureClass::InvalidStorage => {
            ClientDirectoryFailure::InvalidResponse
        }
        _ => ClientDirectoryFailure::Offline,
    }
}
fn management_failure(error: ClientFailure) -> ManagementFailure {
    match error.class() {
        ClientFailureClass::ResourceLimit | ClientFailureClass::CounterExhausted => {
            ManagementFailure::LimitExceeded
        }
        ClientFailureClass::AuthorizationDenied => ManagementFailure::Revoked,
        ClientFailureClass::RecoveryRequired => ManagementFailure::Indeterminate,
        ClientFailureClass::StorageUnavailable => ManagementFailure::StorageUnavailable,
        ClientFailureClass::InvalidPeer | ClientFailureClass::InvalidStorage => {
            ManagementFailure::UnsafeNativeState
        }
        _ => ManagementFailure::Offline,
    }
}

fn inventory_identity(
    value: Option<&spaceterm_protocol::wire::common::Identity>,
) -> Result<[u8; 16], spaceterm_protocol::ProtocolError> {
    spaceterm_protocol::convert::identity::decode_global(
        value.ok_or(spaceterm_protocol::ProtocolError::Malformed)?,
    )
}
