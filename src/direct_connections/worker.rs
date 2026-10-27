use std::sync::atomic::Ordering;
use std::time::Duration;

use super::credentials::CredentialError;
use super::records::Authority;
use super::*;

pub(super) trait DirectNetwork: Send + Sync {
    fn submit_enrollment(
        &self,
        enrollment: &mut direct::Enrollment,
    ) -> Result<direct::ComparisonCode, DirectError> {
        enrollment.submit()
    }
    fn finish_enrollment(
        &self,
        enrollment: &mut direct::Enrollment,
    ) -> Result<Option<PairedDevice>, DirectError> {
        enrollment.finish()
    }
    fn connect(
        &self,
        identity: &str,
        route: &DirectRoute,
        credential: DeviceCredential,
        gate: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ServerConnection, DirectError>;
    fn revoke(
        &self,
        endpoint: &str,
        actor: &PairedDevice,
        credential: &DeviceCredential,
        operation: UnpairKey,
        target: PairingId,
    ) -> Result<direct::RevocationReceipt, DirectError>;
    fn cancel_enrollment(
        &self,
        endpoint: &str,
        pin: &direct::ServerPin,
        claim: [u8; 32],
        credential: &DeviceCredential,
    ) -> Result<Option<direct::RevocationReceipt>, DirectError>;
}

pub(super) struct NativeNetwork;

impl DirectNetwork for NativeNetwork {
    fn connect(
        &self,
        identity: &str,
        route: &DirectRoute,
        credential: DeviceCredential,
        gate: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ServerConnection, DirectError> {
        let connector = Arc::new(Connector {
            endpoint: route.endpoint.clone(),
            paired: route.paired.clone(),
            credential,
            gate,
        });
        let server = ServerConnection::direct(identity, route.id, route.paired.clone(), connector)?;
        server.refresh()?;
        Ok(server)
    }
    fn revoke(
        &self,
        endpoint: &str,
        actor: &PairedDevice,
        credential: &DeviceCredential,
        operation: UnpairKey,
        target: PairingId,
    ) -> Result<direct::RevocationReceipt, DirectError> {
        direct::revoke(endpoint, actor, credential, operation, target)
    }
    fn cancel_enrollment(
        &self,
        endpoint: &str,
        pin: &direct::ServerPin,
        claim: [u8; 32],
        credential: &DeviceCredential,
    ) -> Result<Option<direct::RevocationReceipt>, DirectError> {
        match direct::Enrollment::resume(endpoint, pin.clone(), claim, credential)?.cancel() {
            // The pinned server checks its durable claim-to-pairing binding before expiry.
            // Expired therefore confirms that this original created no retained pairing.
            Err(DirectError::Expired) => Ok(None),
            result => result,
        }
    }
}

impl DirectConnections {
    pub(super) fn start_worker(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.worker {
            return;
        }
        state.worker = true;
        let weak = Arc::downgrade(&self.0);
        let wake = Arc::clone(&self.0.work);
        if std::thread::Builder::new()
            .name("direct-connections".into())
            .spawn(move || {
                loop {
                    let Some(owner) = weak.upgrade() else {
                        break;
                    };
                    let next = work(&owner);
                    drop(owner);
                    // A change, a finished job or the owner's end wakes it, and otherwise the
                    // next retry. A connected route waits for its link to end.
                    wake.wait(next);
                }
            })
            .is_err()
        {
            state.worker = false;
            state.persistence_error = true;
        }
    }

    /// Invitation parsing is local. All network and native credential work belongs to
    /// the requested join's worker, and the invitation is never retained in a record.
    pub(crate) fn join(
        &self,
        text: Zeroizing<String>,
        name: ClientName,
    ) -> Result<JoinId, DirectError> {
        let invitation = direct::Invitation::parse(&text)?;
        if invitation.server_pin().build != self.0.identity {
            return Err(DirectError::WrongServer);
        }
        let id = random_id()?;
        let now = Instant::now();
        let deadline = now
            .checked_add(invitation.lifetime())
            .ok_or(DirectError::InvalidInvitation)?;
        {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.joins.len() >= 16 {
                let finished = state
                    .joins
                    .iter()
                    .find(|(_, j)| {
                        matches!(
                            j.status,
                            JoinStatus::Approved(_)
                                | JoinStatus::ApprovedUncertain(_)
                                | JoinStatus::PublicationUnconfirmed
                                | JoinStatus::Rejected
                                | JoinStatus::Expired
                                | JoinStatus::Failed(_)
                        )
                    })
                    .map(|(id, _)| *id);
                if let Some(finished) = finished {
                    state.joins.remove(&finished);
                } else {
                    return Err(DirectError::Capacity);
                }
            }
            state.joins.insert(
                id,
                JoinState {
                    status: JoinStatus::Validating,
                    cancelled: false,
                    retry_requested: false,
                    deadline,
                },
            );
        }
        let owner = self.clone();
        std::thread::Builder::new()
            .name("direct-enrollment".into())
            .spawn(move || {
                if let Err(error) = enroll(&owner, id, invitation, name) {
                    let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(join) = state.joins.get_mut(&id)
                        && !join.cancelled
                    {
                        join.status = match error {
                            DirectError::Expired => JoinStatus::Expired,
                            DirectError::Rejected => JoinStatus::Rejected,
                            error => JoinStatus::Failed(error),
                        };
                    }
                    drop(state);
                    owner.0.notify();
                }
            })
            .map_err(|_| DirectError::Capacity)?;
        self.0.notify();
        Ok(id)
    }
}

fn credential(
    owner: &Inner,
    saved: &SavedRoute,
    interaction: Interaction,
) -> Result<DeviceCredential, DirectError> {
    let bytes = owner
        .credentials
        .read(&saved.credential, interaction)
        .map_err(credential_error)?
        .ok_or(DirectError::CredentialUnavailable)?;
    DeviceCredential::from_bytes(&bytes)
}

pub(super) fn credential_error(error: CredentialError) -> DirectError {
    match error {
        CredentialError::InteractionRequired => DirectError::InteractionRequired,
        CredentialError::Unavailable => DirectError::CredentialUnavailable,
    }
}

struct Connector {
    endpoint: String,
    paired: PairedDevice,
    credential: DeviceCredential,
    gate: Arc<std::sync::atomic::AtomicBool>,
}

impl direct::DirectConnector for Connector {
    fn connect(&self) -> Result<direct::DirectConnection, DirectError> {
        if !self.gate.load(Ordering::Acquire) {
            return Err(DirectError::Revoked);
        }
        direct::DirectConnection::connect(&self.endpoint, &self.paired, &self.credential)
    }
}

const ROUTE_WORKERS: usize = 4;
const RECOVERY_WORKERS: usize = 2;

enum JobKey {
    Route(DirectRouteId),
    Recovery(UnpairKey),
}
struct JobPermit {
    owner: std::sync::Weak<Inner>,
    key: JobKey,
}
impl Drop for JobPermit {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
            match self.key {
                JobKey::Route(id) => {
                    state.route_jobs.remove(&id);
                }
                JobKey::Recovery(id) => {
                    state.recovery_jobs.remove(&id);
                }
            }
            drop(state);
            // A freed job slot may start waiting work.
            owner.work.wake();
        }
    }
}

/// Makes a connected route due again when its shared link ends. The link judges its own health
/// with pings, so a live link needs no check.
pub(super) fn follow_link(
    owner: &Arc<Inner>,
    id: DirectRouteId,
) -> Arc<dyn spaceterm_server::connection::link::LinkSink> {
    use spaceterm_server::connection::link::{Health, LinkEvent};
    let owner = Arc::downgrade(owner);
    Arc::new(move |event: LinkEvent| {
        let (LinkEvent::Health(Health::Closed(_)), Some(owner)) = (event, owner.upgrade()) else {
            return;
        };
        let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(runtime) = state.routes.get_mut(&id)
            && runtime.status == DirectRouteStatus::Connected
        {
            runtime.status = DirectRouteStatus::Reconnecting;
            runtime.retry.due = Instant::now();
        }
        drop(state);
        owner.notify();
    })
}
struct RouteJob {
    saved: SavedRoute,
    interaction: Interaction,
    gate: Arc<std::sync::atomic::AtomicBool>,
    server: Option<ServerConnection>,
    due: Instant,
}

/// Wakes the worker thread. A wake before the wait is never lost.
#[derive(Default)]
pub(super) struct WorkerWake {
    pending: Mutex<bool>,
    ready: std::sync::Condvar,
}

impl WorkerWake {
    pub(super) fn wake(&self) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.ready.notify_all();
    }

    /// Returns on the first wake since the last wait, or at `deadline`.
    fn wait(&self, deadline: Option<Instant>) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while !*pending {
            pending = match deadline {
                None => self.ready.wait(pending).unwrap_or_else(|e| e.into_inner()),
                Some(deadline) => {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        return;
                    };
                    self.ready
                        .wait_timeout(pending, remaining)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
            };
        }
        *pending = false;
    }
}

/// Whether the worker checks this route when its retry is due. A connected route whose link
/// the worker follows is checked only once that link ends.
fn checkable(runtime: &RouteState) -> bool {
    !matches!(
        runtime.status,
        DirectRouteStatus::NeedsInteraction | DirectRouteStatus::Refused(_)
    ) && !(runtime.status == DirectRouteStatus::Connected && runtime.health.is_some())
}

/// Starts the jobs that are due, and returns when the next one that is not running will be.
fn work(owner: &Arc<Inner>) -> Option<Instant> {
    let (routes, pending, next) = {
        let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let next = state
            .document
            .routes
            .iter()
            .filter_map(|saved| {
                let runtime = state.routes.get(&saved.route.id)?;
                (saved.active && !state.route_jobs.contains(&saved.route.id) && checkable(runtime))
                    .then_some(runtime.retry.due)
            })
            .chain(state.document.pending.iter().filter_map(|record| {
                let retry = state.pending.get(&record.operation)?;
                (retry.state != UnpairState::NeedsInteraction
                    && !state.recovery_jobs.contains(&record.operation)
                    && (!matches!(record.authority, Authority::Enrollment { .. })
                        || record.cancel_requested))
                    .then_some(retry.due)
            }))
            // Work already due but over the job limits starts when a running job ends.
            .filter(|due| *due > now)
            .min();
        let mut routes = state
            .document
            .routes
            .iter()
            .filter_map(|saved| {
                let runtime = state.routes.get(&saved.route.id)?;
                (saved.active
                    && runtime.retry.due <= now
                    && !state.route_jobs.contains(&saved.route.id)
                    && checkable(runtime))
                .then(|| RouteJob {
                    saved: saved.clone(),
                    interaction: runtime.retry.interaction,
                    gate: Arc::clone(&runtime.gate),
                    server: runtime.server.clone(),
                    due: runtime.retry.due,
                })
            })
            .collect::<Vec<_>>();
        routes.sort_by_key(|job| (job.due, job.saved.route.id));
        routes.truncate(ROUTE_WORKERS.saturating_sub(state.route_jobs.len()));
        state
            .route_jobs
            .extend(routes.iter().map(|job| job.saved.route.id));
        let mut pending = state
            .document
            .pending
            .iter()
            .filter_map(|record| {
                let retry = state.pending.get(&record.operation)?;
                (retry.due <= now
                    && retry.state != UnpairState::NeedsInteraction
                    && !state.recovery_jobs.contains(&record.operation)
                    && (!matches!(record.authority, Authority::Enrollment { .. })
                        || record.cancel_requested))
                    .then(|| (record.clone(), retry.interaction, retry.due))
            })
            .collect::<Vec<_>>();
        pending.sort_by_key(|(record, _, due)| (*due, record.operation));
        pending.truncate(RECOVERY_WORKERS.saturating_sub(state.recovery_jobs.len()));
        state
            .recovery_jobs
            .extend(pending.iter().map(|(record, _, _)| record.operation));
        (routes, pending, next)
    };
    // Separate limits reserve recovery capacity even while every route slot waits on a host.
    for (record, interaction, due) in pending {
        let id = record.operation;
        let permit = JobPermit {
            owner: Arc::downgrade(owner),
            key: JobKey::Recovery(id),
        };
        let owner = Arc::clone(owner);
        let fallback = Arc::clone(&owner);
        if std::thread::Builder::new()
            .name("direct-revocation".into())
            .spawn(move || {
                let _permit = permit;
                recover(&owner, record, interaction, due);
            })
            .is_err()
            && let Some(retry) = fallback
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending
                .get_mut(&id)
        {
            retry.delay();
        }
    }
    for job in routes {
        let id = job.saved.route.id;
        let permit = JobPermit {
            owner: Arc::downgrade(owner),
            key: JobKey::Route(id),
        };
        let owner = Arc::clone(owner);
        let fallback = Arc::clone(&owner);
        if std::thread::Builder::new()
            .name("direct-route".into())
            .spawn(move || {
                let _permit = permit;
                check_route(&owner, job);
            })
            .is_err()
            && let Some(route) = fallback
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .routes
                .get_mut(&id)
        {
            route.retry.delay();
        }
    }
    next
}

fn check_route(owner: &Arc<Inner>, job: RouteJob) {
    let result = match job.server {
        // The route's shared connection judges its own health with pings. Refreshing a live
        // connection reads that health without network traffic; only an ended one reconnects.
        Some(server) => server.refresh().map(|_| server).map_err(DirectError::from),
        None => connect(owner, &job.saved, job.interaction, Arc::clone(&job.gate)),
    };
    let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(runtime) = state.routes.get_mut(&job.saved.route.id) else {
        return;
    };
    if !Arc::ptr_eq(&runtime.gate, &job.gate)
        || !job.gate.load(Ordering::Acquire)
        || runtime.retry.due != job.due
    {
        return;
    }
    runtime.retry.interaction = Interaction::Quiet;
    match result {
        Ok(server) => {
            if runtime.server.is_none() {
                runtime.server = Some(server);
                // A new connection has a new link to follow.
                runtime.health = None;
            }
            runtime.status = DirectRouteStatus::Connected;
            runtime.retry.attempt = 0;
            runtime.retry.due = Instant::now();
            if runtime.health.is_none()
                && let Some(server) = &runtime.server
            {
                let health = follow_link(owner, job.saved.route.id);
                server.subscribe(Arc::downgrade(&health));
                runtime.health = Some(health);
            }
        }
        Err(error) => {
            runtime.status = route_failure(error, runtime.server.is_some());
            runtime.retry.delay();
        }
    }
    drop(state);
    owner.notify();
}

pub(super) fn connect(
    owner: &Inner,
    saved: &SavedRoute,
    interaction: Interaction,
    gate: Arc<std::sync::atomic::AtomicBool>,
) -> Result<ServerConnection, DirectError> {
    let credential = credential(owner, saved, interaction)?;
    owner
        .network
        .connect(&owner.identity, &saved.route, credential, gate)
}

fn route_failure(error: DirectError, had_server: bool) -> DirectRouteStatus {
    match error {
        DirectError::InteractionRequired => DirectRouteStatus::NeedsInteraction,
        DirectError::Revoked => DirectRouteStatus::Refused(RefusalReason::Revoked),
        DirectError::PinMismatch => DirectRouteStatus::Refused(RefusalReason::PinMismatch),
        DirectError::AuthenticationRefused
        | DirectError::WrongServer
        | DirectError::Substituted => DirectRouteStatus::Refused(RefusalReason::InvalidProof),
        DirectError::IncompatibleProtocol => {
            DirectRouteStatus::Refused(RefusalReason::Incompatible)
        }
        DirectError::CredentialUnavailable => {
            DirectRouteStatus::Unavailable(UnavailableReason::CredentialStore)
        }
        DirectError::StorageUnavailable => {
            DirectRouteStatus::Unavailable(UnavailableReason::Storage)
        }
        _ if had_server => DirectRouteStatus::Reconnecting,
        _ => DirectRouteStatus::Unavailable(UnavailableReason::Network),
    }
}

impl Retry {
    fn delay(&mut self) {
        self.attempt = self.attempt.saturating_add(1);
        self.due = Instant::now() + Duration::from_secs((1_u64 << self.attempt.min(6)).min(60));
        self.interaction = Interaction::Quiet;
    }
}

fn recover(owner: &Arc<Inner>, mut pending: PendingRecord, interaction: Interaction, due: Instant) {
    // No network effect precedes retaining the exact request, including after a failed save.
    {
        let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.persistence_error && !owner.save(&mut state) {
            return;
        }
    }
    let result = (|| {
        if !pending.confirmed {
            let receipt = match &pending.authority {
                Authority::Direct(actor) => {
                    let credential = credential(owner, actor, interaction)?;
                    Some(owner.network.revoke(
                        &actor.route.endpoint,
                        &actor.route.paired,
                        &credential,
                        pending.operation,
                        pending.target,
                    )?)
                }
                Authority::Account { route, pin } => {
                    let resolver = owner
                        .resolver
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .ok_or(DirectError::InteractionRequired)?;
                    let server = resolver.resolve(route, interaction)?;
                    if server.direct_status()?.pin != *pin {
                        return Err(DirectError::WrongServer);
                    }
                    Some(server.revoke_device(pending.operation, pending.target)?)
                }
                Authority::Enrollment {
                    endpoint,
                    pin,
                    claim,
                    credential,
                } => {
                    let bytes = owner
                        .credentials
                        .read(credential, interaction)
                        .map_err(credential_error)?
                        .ok_or(DirectError::CredentialUnavailable)?;
                    let key = DeviceCredential::from_bytes(&bytes)?;
                    owner
                        .network
                        .cancel_enrollment(endpoint, pin, *claim, &key)?
                }
            };
            if let Some(receipt) = receipt
                && (receipt.server != pending.server()
                    || (!matches!(pending.authority, Authority::Enrollment { .. })
                        && (receipt.operation != pending.operation
                            || receipt.target != pending.target)))
            {
                return Err(DirectError::AuthenticationRefused);
            }
            let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
            let record = state
                .document
                .pending
                .iter_mut()
                .find(|p| p.operation == pending.operation)
                .ok_or(DirectError::InvalidRequest)?;
            record.confirmed = true;
            if !owner.save(&mut state) {
                return Err(DirectError::StorageUnavailable);
            }
            pending.confirmed = true;
        }
        let retire = match &pending.authority {
            Authority::Direct(actor) if actor.route.paired.pairing == pending.target => {
                Some(actor.credential)
            }
            Authority::Enrollment { credential, .. } => Some(*credential),
            _ => None,
        };
        if let Some(credential) = retire {
            let (saved_shared, pending_shared) = {
                let state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
                (
                    state
                        .document
                        .routes
                        .iter()
                        .any(|r| r.credential == credential),
                    state.document.pending.iter().any(|p| {
                        p.operation != pending.operation
                            && !p.confirmed
                            && p.credential_key() == Some(credential)
                    }),
                )
            };
            if !saved_shared && pending_shared {
                return Ok(false);
            }
            if !saved_shared {
                owner
                    .credentials
                    .remove(&credential, interaction)
                    .map_err(credential_error)?;
            }
        }
        Ok(true)
    })();
    let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
    match result {
        Ok(true) => {
            let old = state.document.clone();
            state
                .document
                .pending
                .retain(|p| p.operation != pending.operation);
            if owner.save(&mut state) {
                state.pending.remove(&pending.operation);
            } else {
                state.document = old;
            }
        }
        Ok(false) => {
            if let Some(retry) = state.pending.get_mut(&pending.operation)
                && retry.due == due
            {
                retry.state = UnpairState::Retrying;
                retry.delay();
            }
        }
        Err(error) => {
            if let Some(retry) = state.pending.get_mut(&pending.operation)
                && retry.due == due
            {
                retry.state = if error == DirectError::InteractionRequired {
                    UnpairState::NeedsInteraction
                } else {
                    UnpairState::Unconfirmed
                };
                retry.delay();
            }
        }
    }
    drop(state);
    owner.notify();
}

/// A failed network observation keeps its live original until the person retries or cancels.
/// No network request is repeated while this join is paused.
fn enrollment_attempt<T>(
    owner: &DirectConnections,
    id: JoinId,
    stage: EnrollmentStage,
    progress: &JoinStatus,
    mut operation: impl FnMut() -> Result<T, DirectError>,
) -> Result<Option<T>, DirectError> {
    loop {
        {
            let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(join) = state.joins.get_mut(&id).filter(|join| !join.cancelled) else {
                return Ok(None);
            };
            if Instant::now() >= join.deadline {
                return Err(DirectError::Expired);
            }
            join.status = progress.clone();
        }
        owner.0.notify();
        match operation() {
            Err(DirectError::Transport) => {
                {
                    let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(join) = state.joins.get_mut(&id).filter(|join| !join.cancelled) else {
                        return Ok(None);
                    };
                    join.status = JoinStatus::Retryable { stage };
                }
                owner.0.notify();
                loop {
                    {
                        let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
                        let Some(join) = state.joins.get_mut(&id).filter(|join| !join.cancelled)
                        else {
                            return Ok(None);
                        };
                        if Instant::now() >= join.deadline {
                            return Err(DirectError::Expired);
                        }
                        if std::mem::take(&mut join.retry_requested) {
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            result => return result.map(Some),
        }
    }
}

fn enroll(
    owner: &DirectConnections,
    id: JoinId,
    invitation: direct::Invitation,
    name: ClientName,
) -> Result<(), DirectError> {
    let key = random_id()?;
    let credential = DeviceCredential::generate()?;
    owner
        .0
        .credentials
        .insert(&key, credential.expose_bytes())
        .map_err(credential_error)?;
    let result = (|| {
        // Preparing is inert, so recovery owns the original before it reaches
        // the server. A lost claim reply cannot leave an approvable orphan.
        let mut enrollment = direct::Enrollment::prepare(&invitation, name, &credential)?;
        {
            let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let cancelled = state.joins.get(&id).is_none_or(|j| j.cancelled);
            state.document.ensure_capacity()?;
            state.document.pending.push(PendingRecord::enrollment(
                id,
                invitation.server_name().to_owned(),
                invitation.endpoint().to_owned(),
                invitation.server_pin().clone(),
                enrollment.claim_id(),
                key,
                cancelled,
            ));
            state.pending.insert(id, Retry::new(Interaction::Quiet));
            if !owner.0.save(&mut state) {
                if let Some(pending) = state
                    .document
                    .pending
                    .iter_mut()
                    .find(|p| p.operation == id)
                {
                    pending.cancel_requested = true;
                }
                drop(state);
                owner.start_worker();
                return Err(DirectError::StorageUnavailable);
            }
        }
        let cancelled = owner
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .joins
            .get(&id)
            .is_none_or(|j| j.cancelled);
        if cancelled {
            owner.start_worker();
            return Ok(());
        }
        let Some(comparison_code) = enrollment_attempt(
            owner,
            id,
            EnrollmentStage::Claim,
            &JoinStatus::Validating,
            || owner.0.network.submit_enrollment(&mut enrollment),
        )?
        else {
            owner.start_worker();
            return Ok(());
        };
        let awaiting = JoinStatus::AwaitingApproval { comparison_code };
        loop {
            let cancelled = owner
                .0
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .joins
                .get(&id)
                .is_none_or(|j| j.cancelled);
            if cancelled {
                let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(pending) = state
                    .document
                    .pending
                    .iter_mut()
                    .find(|p| p.operation == id)
                {
                    pending.cancel_requested = true;
                    owner.0.save(&mut state);
                }
                drop(state);
                owner.start_worker();
                return Ok(());
            }
            let Some(paired) =
                enrollment_attempt(owner, id, EnrollmentStage::Approval, &awaiting, || {
                    owner.0.network.finish_enrollment(&mut enrollment)
                })?
            else {
                owner.start_worker();
                return Ok(());
            };
            if let Some(paired) = paired {
                let saved = SavedRoute {
                    route: DirectRoute {
                        id,
                        label: invitation.server_name().to_owned(),
                        endpoint: invitation.endpoint().to_owned(),
                        paired,
                    },
                    credential: key,
                    active: true,
                };
                complete_enrollment(owner, id, saved);
                owner.start_worker();
                owner.0.notify();
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    })();
    if result.is_err() {
        // An enrolled route or restricted revocation now owns its exact item. Never retire
        // it after a late storage failure or remove another pairing's credential.
        let referenced = {
            let state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
            state.document.routes.iter().any(|r| r.credential == key)
                || state
                    .document
                    .pending
                    .iter()
                    .any(|p| p.credential_key() == Some(key))
        };
        if referenced {
            let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pending) = state
                .document
                .pending
                .iter_mut()
                .find(|p| p.operation == id)
            {
                pending.cancel_requested = true;
                owner.0.save(&mut state);
            }
            drop(state);
            owner.start_worker();
        }
        if !referenced {
            let _ = owner.0.credentials.remove(&key, Interaction::Quiet);
        }
    }
    result
}

fn complete_enrollment(owner: &DirectConnections, id: JoinId, saved: SavedRoute) {
    let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
    let cancelled = state.joins.get(&id).is_none_or(|j| j.cancelled);
    if cancelled {
        // The retained claim also resolves an approval that won the race. Keep
        // that original instead of introducing a second recovery operation.
        if let Some(pending) = state
            .document
            .pending
            .iter_mut()
            .find(|p| p.operation == id)
        {
            pending.cancel_requested = true;
            owner.0.save(&mut state);
        }
        return;
    }
    let original = state.document.clone();
    state.document.pending.retain(|p| p.operation != id);
    state.document.routes.push(saved);
    let publication = owner.0.publish(&mut state);
    if publication.is_ok() {
        state.pending.remove(&id);
        state.routes.insert(
            id,
            RouteState {
                status: DirectRouteStatus::Connecting,
                server: None,
                gate: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                retry: Retry::new(Interaction::Quiet),
                health: None,
            },
        );
    } else {
        // An unconfirmed publication must not claim either failure or withdrawal.
        // A definite non-publication keeps the original for restricted cancellation.
        state.document = original;
        if publication == Err(SaveError::NotPublished)
            && let Some(pending) = state
                .document
                .pending
                .iter_mut()
                .find(|p| p.operation == id)
        {
            pending.cancel_requested = true;
        }
    }
    if let Some(join) = state.joins.get_mut(&id) {
        join.status = match publication {
            Ok(Durability::Synchronized) => JoinStatus::Approved(id),
            Ok(Durability::Uncertain) => JoinStatus::ApprovedUncertain(id),
            Err(SaveError::NotPublished) => JoinStatus::Failed(DirectError::StorageUnavailable),
            Err(SaveError::PublicationIndeterminate) => JoinStatus::PublicationUnconfirmed,
        };
    }
}

#[cfg(test)]
pub(super) mod enrollment_tests {
    use super::super::tests::{Answer, Credentials, MemoryRecords, Network};
    use super::*;
    use crate::platform::app_directories::AppDirectoryEnvironment;
    use crate::platform::app_paths::AppPaths;
    use crate::platform::secure_filesystem::SecureCommitOutcome;
    use crate::platform::testing::{RecordingFilesystem, resolve_app_paths};

    /// The server accepts each original before its first reply is lost at the network boundary.
    struct EnrollmentNetwork {
        lost: EnrollmentStage,
        expired: std::sync::atomic::AtomicBool,
        calls: Mutex<Vec<(EnrollmentStage, [u8; 32])>>,
        claims: Mutex<std::collections::BTreeSet<[u8; 32]>>,
        cancellations: Mutex<Vec<[u8; 32]>>,
        paired: PairedDevice,
    }

    impl DirectNetwork for EnrollmentNetwork {
        fn submit_enrollment(
            &self,
            enrollment: &mut direct::Enrollment,
        ) -> Result<direct::ComparisonCode, DirectError> {
            let claim = enrollment.claim_id();
            self.claims.lock().unwrap().insert(claim);
            let mut calls = self.calls.lock().unwrap();
            calls.push((EnrollmentStage::Claim, claim));
            if self.expired.load(Ordering::Acquire) {
                Err(DirectError::Expired)
            } else if self.lost == EnrollmentStage::Claim && calls.len() == 1 {
                Err(DirectError::Transport)
            } else {
                Ok(direct::PairingClaim {
                    id: claim,
                    invitation_id: [7; 32],
                    server: self.paired.pin.server,
                    device_public_key: self.paired.client,
                    device_name: "Network fixture".into(),
                    expires_at_unix_seconds: u64::MAX,
                    exchange_hash: [7; 32],
                }
                .comparison_code())
            }
        }
        fn finish_enrollment(
            &self,
            enrollment: &mut direct::Enrollment,
        ) -> Result<Option<PairedDevice>, DirectError> {
            let mut calls = self.calls.lock().unwrap();
            let first = !calls
                .iter()
                .any(|(stage, _)| *stage == EnrollmentStage::Approval);
            calls.push((EnrollmentStage::Approval, enrollment.claim_id()));
            if self.lost == EnrollmentStage::Approval && first {
                Err(DirectError::Transport)
            } else {
                Ok(Some(self.paired.clone()))
            }
        }
        fn connect(
            &self,
            _: &str,
            _: &DirectRoute,
            _: DeviceCredential,
            _: Arc<std::sync::atomic::AtomicBool>,
        ) -> Result<ServerConnection, DirectError> {
            Err(DirectError::Transport)
        }
        fn revoke(
            &self,
            _: &str,
            _: &PairedDevice,
            _: &DeviceCredential,
            _: UnpairKey,
            _: PairingId,
        ) -> Result<direct::RevocationReceipt, DirectError> {
            Err(DirectError::Transport)
        }
        fn cancel_enrollment(
            &self,
            _: &str,
            _: &direct::ServerPin,
            claim: [u8; 32],
            _: &DeviceCredential,
        ) -> Result<Option<direct::RevocationReceipt>, DirectError> {
            self.cancellations.lock().unwrap().push(claim);
            // Keep cleanup uncertain so tests can inspect its exact retained original.
            Err(DirectError::Transport)
        }
    }

    struct LiveJoin {
        owner: DirectConnections,
        credentials: Arc<Credentials>,
        network: Arc<EnrollmentNetwork>,
        id: JoinId,
    }

    impl LiveJoin {
        fn new(label: &str, stage: EnrollmentStage) -> Self {
            let expires = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 60;
            Self::with_server_expiry(label, stage, expires)
        }

        fn with_server_expiry(label: &str, stage: EnrollmentStage, expires: u64) -> Self {
            use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
            let credentials = Arc::new(Credentials::default());
            let network = Arc::new(EnrollmentNetwork {
                lost: stage,
                expired: std::sync::atomic::AtomicBool::new(false),
                calls: Mutex::default(),
                claims: Mutex::default(),
                cancellations: Mutex::default(),
                paired: super::super::tests::saved(1, 5).route.paired,
            });
            let owner = DirectConnections::new(
                "spaceterm-development",
                Arc::new(MemoryRecords::default()),
                credentials.clone(),
                network.clone(),
            );
            owner.resume_retained();
            let payload = Zeroizing::new(
                serde_json::to_vec(&serde_json::json!({
                    "schema": 1, "id": ([7_u8; 32]), "endpoint": "wss://127.0.0.1:9231/spaceterm",
                    "pin": network.paired.pin, "server_name": label, "token": ([8_u8; 32]),
                    "expires": expires, "valid_for_seconds": 300,
                }))
                .unwrap(),
            );
            let text = Zeroizing::new(format!(
                "spaceterm://pair/{}",
                URL_SAFE_NO_PAD.encode(&payload)
            ));
            let id = owner.join(text, "Test device".into()).unwrap();
            let result = Self {
                owner,
                credentials,
                network,
                id,
            };
            result.wait(|status| matches!(status, JoinStatus::Retryable { stage: value } if *value == stage));
            result
        }

        fn wait(&self, predicate: impl Fn(&JoinStatus) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if self
                    .owner
                    .join_status(self.id)
                    .as_ref()
                    .is_some_and(&predicate)
                {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "join did not reach its expected state"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        fn original(&self) -> PendingRecord {
            self.owner.0.state.lock().unwrap().document.pending[0].clone()
        }
    }

    pub(crate) fn retryable_join(
        label: &str,
        stage: EnrollmentStage,
    ) -> (DirectConnections, JoinId) {
        let fixture = LiveJoin::new(label, stage);
        (fixture.owner, fixture.id)
    }

    #[test]
    fn server_approved_join_ignores_wall_clock_skew_and_bounds_its_local_wait() {
        // The network boundary accepts a live invitation on a server whose wall clock
        // differs from this client's. Neither timestamp determines the local wait.
        for expires in [1, 253_402_300_799] {
            let fixture = LiveJoin::with_server_expiry(
                "Clock-skewed server",
                EnrollmentStage::Claim,
                expires,
            );
            let remaining = fixture.owner.0.state.lock().unwrap().joins[&fixture.id]
                .deadline
                .checked_duration_since(Instant::now())
                .unwrap();
            assert!(remaining <= Duration::from_secs(300));

            let original = fixture.original();
            fixture.owner.retry_join(fixture.id).unwrap();
            fixture.wait(|status| matches!(status, JoinStatus::Approved(id) if *id == fixture.id));

            let route = fixture.owner.route(&fixture.id).unwrap();
            assert_eq!(route.endpoint, "wss://127.0.0.1:9231/spaceterm");
            assert_eq!(route.paired, fixture.network.paired);
            assert!(fixture.owner.pending_unpairs().is_empty());
            assert_eq!(fixture.credentials.values.lock().unwrap().len(), 1);
            let Authority::Enrollment { claim, .. } = original.authority else {
                panic!("join must retain its original enrollment authority")
            };
            assert!(
                fixture
                    .network
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|(_, id)| *id == claim)
            );
        }
    }

    #[test]
    fn server_expiry_refuses_a_join_despite_a_future_invitation_timestamp() {
        let fixture = LiveJoin::with_server_expiry(
            "Expired server invitation",
            EnrollmentStage::Claim,
            253_402_300_799,
        );
        let original = fixture.original();
        fixture.network.expired.store(true, Ordering::Release);
        fixture.owner.retry_join(fixture.id).unwrap();
        fixture.wait(|status| matches!(status, JoinStatus::Expired));

        assert!(fixture.owner.route(&fixture.id).is_none());
        let pending = fixture.original();
        assert_eq!(pending.operation, original.operation);
        assert_eq!(
            serde_json::to_value(&pending.authority).unwrap(),
            serde_json::to_value(&original.authority).unwrap()
        );
        assert!(pending.cancel_requested);
        assert_eq!(fixture.credentials.values.lock().unwrap().len(), 1);
        assert_eq!(
            fixture.owner.retry_join(fixture.id),
            Err(DirectError::InvalidRequest)
        );
    }

    #[test]
    fn lost_claim_and_finish_replies_retry_only_the_original_and_save_one_route() {
        for stage in [EnrollmentStage::Claim, EnrollmentStage::Approval] {
            let fixture = LiveJoin::new("Server 1", stage);
            let original = fixture.original();
            let Authority::Enrollment {
                claim, credential, ..
            } = original.authority
            else {
                panic!()
            };
            assert!(!original.cancel_requested);
            assert!(fixture.owner.pending_unpairs().is_empty());
            let attempts = fixture.network.calls.lock().unwrap().len();
            std::thread::sleep(Duration::from_millis(150));
            assert_eq!(
                fixture.network.calls.lock().unwrap().len(),
                attempts,
                "paused join must not repeat network work"
            );
            assert_eq!(
                fixture.owner.retry_join([99; 32]),
                Err(DirectError::InvalidRequest)
            );

            fixture.owner.retry_join(fixture.id).unwrap();
            fixture.wait(|status| matches!(status, JoinStatus::Approved(id) if *id == fixture.id));

            let state = fixture.owner.0.state.lock().unwrap();
            assert!(state.document.pending.is_empty());
            assert_eq!(state.document.routes.len(), 1);
            assert_eq!(state.document.routes[0].credential, credential);
            assert_eq!(
                state.document.routes[0].route.paired,
                fixture.network.paired
            );
            drop(state);
            assert_eq!(fixture.credentials.values.lock().unwrap().len(), 1);
            assert_eq!(fixture.network.claims.lock().unwrap().len(), 1);
            assert!(
                fixture
                    .network
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|(_, id)| *id == claim)
            );
            assert!(fixture.network.cancellations.lock().unwrap().is_empty());
            assert_eq!(
                fixture.owner.retry_join(fixture.id),
                Err(DirectError::InvalidRequest)
            );
        }
    }

    #[test]
    fn cancelling_a_paused_join_preserves_only_its_exact_cleanup_authority() {
        for stage in [EnrollmentStage::Claim, EnrollmentStage::Approval] {
            let fixture = LiveJoin::new("Server 1", stage);
            let original = fixture.original();
            let Authority::Enrollment {
                claim, credential, ..
            } = original.authority
            else {
                panic!()
            };
            fixture.owner.cancel_join(fixture.id).unwrap();
            assert_eq!(
                fixture.owner.retry_join(fixture.id),
                Err(DirectError::InvalidRequest)
            );
            let pending = fixture.original();
            assert_eq!(pending.operation, original.operation);
            assert!(pending.cancel_requested);
            assert!(
                matches!(pending.authority, Authority::Enrollment { claim: c, credential: k, .. } if c == claim && k == credential)
            );
            assert!(fixture.owner.route(&fixture.id).is_none());
            assert!(
                fixture
                    .credentials
                    .values
                    .lock()
                    .unwrap()
                    .contains_key(&credential)
            );
        }
    }

    #[test]
    fn expiry_refuses_retry_and_retires_the_live_join_into_original_cleanup() {
        let fixture = LiveJoin::new("Server 1", EnrollmentStage::Claim);
        fixture
            .owner
            .0
            .state
            .lock()
            .unwrap()
            .joins
            .get_mut(&fixture.id)
            .unwrap()
            .deadline = Instant::now();
        assert_eq!(
            fixture.owner.retry_join(fixture.id),
            Err(DirectError::InvalidRequest)
        );
        fixture.wait(|status| matches!(status, JoinStatus::Expired));
        assert!(fixture.original().cancel_requested);
        assert_eq!(fixture.network.calls.lock().unwrap().len(), 1);
    }

    struct EnrollmentFixture {
        owner: DirectConnections,
        paths: Arc<AppPaths>,
        filesystem: Arc<RecordingFilesystem>,
        credentials: Arc<Credentials>,
        network: Arc<Network>,
        saved: SavedRoute,
    }

    impl EnrollmentFixture {
        fn new(label: &str) -> Self {
            let filesystem = Arc::new(RecordingFilesystem::default());
            let paths = Arc::new(
                resolve_app_paths(
                    &AppDirectoryEnvironment {
                        home: Some("/home/test".into()),
                        ..Default::default()
                    },
                    Some("/runtime".into()),
                    200,
                    filesystem.clone(),
                )
                .unwrap(),
            );
            let credentials = Arc::new(Credentials::default());
            let network = Network::new(Answer::Lost);
            let mut saved = super::super::tests::saved(1, 5);
            saved.active = true;
            saved.route.label = label.to_owned();
            let credential = DeviceCredential::generate().unwrap();
            credentials
                .insert(&saved.credential, credential.expose_bytes())
                .unwrap();
            // Relaunch can inspect the retained route without starting network I/O.
            credentials.locked.store(true, Ordering::Release);
            let owner = DirectConnections::new(
                "spaceterm-development",
                Arc::new(PrivateRecords::new(paths.clone())),
                credentials.clone(),
                network.clone(),
            );
            {
                let mut state = owner.0.state.lock().unwrap();
                state.document.pending.push(PendingRecord::enrollment(
                    saved.route.id,
                    saved.route.label.clone(),
                    saved.route.endpoint.clone(),
                    saved.route.paired.pin.clone(),
                    [8; 32],
                    saved.credential,
                    false,
                ));
                state
                    .pending
                    .insert(saved.route.id, Retry::new(Interaction::Quiet));
                state.joins.insert(
                    saved.route.id,
                    JoinState {
                        status: JoinStatus::Validating,
                        cancelled: false,
                        retry_requested: false,
                        deadline: Instant::now() + Duration::from_secs(300),
                    },
                );
                owner.0.records.save(&state.document).unwrap();
            }
            Self {
                owner,
                paths,
                filesystem,
                credentials,
                network,
                saved,
            }
        }

        fn relaunch(&self) -> DirectConnections {
            let owner = DirectConnections::new(
                "spaceterm-development",
                Arc::new(PrivateRecords::new(self.paths.clone())),
                self.credentials.clone(),
                self.network.clone(),
            );
            owner.resume_retained();
            owner
        }
    }

    pub(crate) fn uncertain_join(label: &str) -> (DirectConnections, JoinId, DirectRouteId) {
        let fixture = EnrollmentFixture::new(label);
        fixture.filesystem.files.lock().unwrap().commit_outcome =
            Some(SecureCommitOutcome::CommittedButUnsynced);
        let id = fixture.saved.route.id;
        complete_enrollment(&fixture.owner, id, fixture.saved);
        fixture.owner.start_worker();
        (fixture.owner, id, id)
    }

    pub(crate) fn unconfirmed_join(label: &str) -> (DirectConnections, JoinId) {
        let fixture = EnrollmentFixture::new(label);
        fixture.filesystem.files.lock().unwrap().commit_error =
            Some(crate::platform::secure_filesystem::SecureFilesystemError::Unavailable);
        let id = fixture.saved.route.id;
        complete_enrollment(&fixture.owner, id, fixture.saved);
        fixture.owner.start_worker();
        (fixture.owner, id)
    }

    #[test]
    fn cancel_after_enrollment_completion_removes_the_route_and_retains_exact_revocation() {
        for outcome in [None, Some(SecureCommitOutcome::CommittedButUnsynced)] {
            let fixture = EnrollmentFixture::new("Server 1");
            fixture.filesystem.files.lock().unwrap().commit_outcome = outcome;
            let id = fixture.saved.route.id;
            complete_enrollment(&fixture.owner, id, fixture.saved.clone());
            let gate = fixture.owner.0.state.lock().unwrap().routes[&id]
                .gate
                .clone();
            fixture.filesystem.files.lock().unwrap().commit_outcome = None;

            fixture.owner.cancel_join(id).unwrap();

            assert!(
                fixture.owner.route(&id).is_none(),
                "successful cancellation must remove an already-published route"
            );
            assert!(fixture.owner.server(&id).is_none());
            assert!(!gate.load(Ordering::Acquire));
            let relaunched = fixture.relaunch();
            for owner in [&fixture.owner, &relaunched] {
                assert!(owner.route(&id).is_none());
                assert!(owner.server(&id).is_none());
                let state = owner.0.state.lock().unwrap();
                assert_eq!(state.document.pending.len(), 1);
                let pending = &state.document.pending[0];
                assert_eq!(pending.target, fixture.saved.route.paired.pairing);
                assert!(!pending.confirmed);
                assert!(matches!(&pending.authority, Authority::Direct(actor)
                    if actor.route.id == id
                    && actor.route.endpoint == fixture.saved.route.endpoint
                    && actor.route.paired == fixture.saved.route.paired
                    && actor.credential == fixture.saved.credential));
            }
            assert!(
                fixture
                    .credentials
                    .values
                    .lock()
                    .unwrap()
                    .contains_key(&fixture.saved.credential)
            );
        }
    }

    #[test]
    fn cancel_after_self_unpair_reuses_its_exact_retained_operation() {
        for retention_fails in [false, true] {
            let fixture = EnrollmentFixture::new("Server 1");
            let id = fixture.saved.route.id;
            complete_enrollment(&fixture.owner, id, fixture.saved.clone());
            if retention_fails {
                fixture.filesystem.files.lock().unwrap().prepare_error =
                    Some(crate::platform::secure_filesystem::SecureFilesystemError::Unavailable);
            }
            let expected = if retention_fails {
                Err(DirectError::StorageUnavailable)
            } else {
                Ok(())
            };
            assert_eq!(fixture.owner.unpair_self(&id), expected);
            let original = fixture.owner.0.state.lock().unwrap().document.pending[0].operation;

            assert_eq!(fixture.owner.cancel_join(id), expected);

            let state = fixture.owner.0.state.lock().unwrap();
            assert_eq!(state.document.pending.len(), 1);
            assert_eq!(state.document.pending[0].operation, original);
            assert!(state.document.routes.is_empty());
            assert!(state.routes.is_empty());
        }
    }

    #[test]
    fn cancel_before_enrollment_completion_keeps_only_the_original_claim_cleanup() {
        let fixture = EnrollmentFixture::new("Server 1");
        let id = fixture.saved.route.id;

        fixture.owner.cancel_join(id).unwrap();
        complete_enrollment(&fixture.owner, id, fixture.saved.clone());

        let relaunched = fixture.relaunch();
        for owner in [&fixture.owner, &relaunched] {
            assert!(owner.route(&id).is_none());
            let state = owner.0.state.lock().unwrap();
            assert_eq!(state.document.pending.len(), 1);
            let pending = &state.document.pending[0];
            assert_eq!(pending.operation, id);
            assert!(pending.cancel_requested);
            assert!(
                matches!(&pending.authority, Authority::Enrollment { claim, credential, .. }
                if *claim == [8; 32] && *credential == fixture.saved.credential)
            );
        }
        assert!(
            fixture
                .credentials
                .values
                .lock()
                .unwrap()
                .contains_key(&fixture.saved.credential)
        );
    }

    #[test]
    fn published_enrollment_with_uncertain_sync_is_never_reported_as_failed() {
        let fixture = EnrollmentFixture::new("Server 1");
        fixture.filesystem.files.lock().unwrap().commit_outcome =
            Some(SecureCommitOutcome::CommittedButUnsynced);
        let id = fixture.saved.route.id;

        complete_enrollment(&fixture.owner, id, fixture.saved.clone());
        let relaunched = fixture.relaunch();

        assert!(
            relaunched.route(&id).is_some(),
            "the filesystem really published the route"
        );
        assert!(
            matches!(fixture.owner.join_status(id), Some(JoinStatus::ApprovedUncertain(route)) if route == id)
        );
        assert!(fixture.owner.route(&id).is_some());
        assert!(fixture.owner.persistence_error());
        assert!(fixture.owner.pending_unpairs().is_empty());
        assert!(relaunched.pending_unpairs().is_empty());
        assert!(fixture.network.cancellations.lock().unwrap().is_empty());
        assert!(
            fixture
                .credentials
                .values
                .lock()
                .unwrap()
                .contains_key(&fixture.saved.credential)
        );
        fixture.filesystem.files.lock().unwrap().commit_outcome = None;
        {
            let mut state = fixture.owner.0.state.lock().unwrap();
            assert!(fixture.owner.0.save(&mut state));
        }
        assert!(
            matches!(fixture.owner.join_status(id), Some(JoinStatus::Approved(route)) if route == id)
        );
        assert!(!fixture.owner.persistence_error());
    }

    #[test]
    fn a_failed_pairing_save_keeps_restricted_cleanup_across_relaunch() {
        let fixture = EnrollmentFixture::new("Server 1");
        fixture.filesystem.files.lock().unwrap().prepare_error =
            Some(crate::platform::secure_filesystem::SecureFilesystemError::Unavailable);
        let id = fixture.saved.route.id;

        complete_enrollment(&fixture.owner, id, fixture.saved.clone());
        let relaunched = fixture.relaunch();

        assert!(matches!(
            fixture.owner.join_status(id),
            Some(JoinStatus::Failed(DirectError::StorageUnavailable))
        ));
        for owner in [&fixture.owner, &relaunched] {
            assert!(owner.route(&id).is_none());
            assert!(owner.server(&id).is_none());
            let state = owner.0.state.lock().unwrap();
            let pending = &state.document.pending[0];
            assert!(pending.cancel_requested);
            assert!(!pending.confirmed);
            assert!(matches!(&pending.authority,
                Authority::Enrollment { claim, credential, .. }
                    if *claim == [8; 32] && *credential == fixture.saved.credential));
        }
        assert!(
            fixture
                .credentials
                .values
                .lock()
                .unwrap()
                .contains_key(&fixture.saved.credential)
        );
        assert!(fixture.network.cancellations.lock().unwrap().is_empty());
    }

    #[test]
    fn indeterminate_publication_does_not_claim_failure_or_automatically_withdraw() {
        let fixture = EnrollmentFixture::new("Server 1");
        fixture.filesystem.files.lock().unwrap().commit_error =
            Some(crate::platform::secure_filesystem::SecureFilesystemError::Unavailable);
        let id = fixture.saved.route.id;

        complete_enrollment(&fixture.owner, id, fixture.saved.clone());
        work(&fixture.owner.0);

        assert!(matches!(
            fixture.owner.join_status(id),
            Some(JoinStatus::PublicationUnconfirmed)
        ));
        assert!(fixture.owner.route(&id).is_none());
        assert!(fixture.owner.pending_unpairs().is_empty());
        assert!(fixture.network.cancellations.lock().unwrap().is_empty());
        assert!(
            fixture
                .credentials
                .values
                .lock()
                .unwrap()
                .contains_key(&fixture.saved.credential)
        );

        // Removing the transient error cannot grant write authority over an unknown publication.
        fixture.filesystem.files.lock().unwrap().commit_error = None;
        fixture.credentials.locked.store(false, Ordering::Release);
        assert_eq!(
            fixture.owner.cancel_join(id),
            Err(DirectError::StorageUnavailable)
        );
        let pending = fixture.owner.pending_unpairs();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].enrollment_cleanup);
        assert!(fixture.owner.persistence_error());
        let (pending, due) = {
            let state = fixture.owner.0.state.lock().unwrap();
            (state.document.pending[0].clone(), state.pending[&id].due)
        };
        recover(&fixture.owner.0, pending, Interaction::Quiet, due);
        assert!(fixture.network.cancellations.lock().unwrap().is_empty());
    }

    #[test]
    fn uncertain_request_retention_cannot_authorize_a_network_effect() {
        let fixture = EnrollmentFixture::new("Server 1");
        fixture.credentials.locked.store(false, Ordering::Release);
        fixture.filesystem.files.lock().unwrap().commit_outcome =
            Some(SecureCommitOutcome::CommittedButUnsynced);
        let (pending, due) = {
            let mut state = fixture.owner.0.state.lock().unwrap();
            state.document.pending[0].cancel_requested = true;
            assert!(!fixture.owner.0.save(&mut state));
            (
                state.document.pending[0].clone(),
                state.pending[&fixture.saved.route.id].due,
            )
        };

        recover(&fixture.owner.0, pending, Interaction::Quiet, due);

        assert!(fixture.network.cancellations.lock().unwrap().is_empty());
        assert!(
            fixture
                .credentials
                .values
                .lock()
                .unwrap()
                .contains_key(&fixture.saved.credential)
        );
    }

    #[test]
    fn uncertain_receipt_retention_cannot_retire_the_native_credential() {
        let fixture = EnrollmentFixture::new("Server 1");
        fixture.credentials.locked.store(false, Ordering::Release);
        let (pending, due) = {
            let mut state = fixture.owner.0.state.lock().unwrap();
            state.document.pending[0].cancel_requested = true;
            assert!(fixture.owner.0.save(&mut state));
            (
                state.document.pending[0].clone(),
                state.pending[&fixture.saved.route.id].due,
            )
        };
        fixture.filesystem.files.lock().unwrap().commit_outcome =
            Some(SecureCommitOutcome::CommittedButUnsynced);

        recover(&fixture.owner.0, pending, Interaction::Quiet, due);

        assert_eq!(fixture.network.cancellations.lock().unwrap().len(), 1);
        assert!(
            fixture
                .credentials
                .values
                .lock()
                .unwrap()
                .contains_key(&fixture.saved.credential)
        );
        let (pending, due) = {
            let state = fixture.owner.0.state.lock().unwrap();
            assert!(state.document.pending[0].confirmed);
            (
                state.document.pending[0].clone(),
                state.pending[&fixture.saved.route.id].due,
            )
        };
        fixture.filesystem.files.lock().unwrap().commit_outcome = None;
        recover(&fixture.owner.0, pending, Interaction::Quiet, due);

        assert_eq!(fixture.network.cancellations.lock().unwrap().len(), 1);
        assert!(
            !fixture
                .credentials
                .values
                .lock()
                .unwrap()
                .contains_key(&fixture.saved.credential)
        );
        assert!(fixture.owner.pending_unpairs().is_empty());
    }
}
