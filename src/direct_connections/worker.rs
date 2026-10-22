use std::sync::atomic::Ordering;
use std::time::Duration;

use super::credentials::CredentialError;
use super::records::Authority;
use super::*;

pub(super) trait DirectNetwork: Send + Sync {
    fn connect(
        &self,
        identity: &str,
        route: &DirectRoute,
        credential: DeviceCredential,
        gate: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<LocalServer, DirectError>;
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
    ) -> Result<LocalServer, DirectError> {
        let connector = Arc::new(Connector {
            endpoint: route.endpoint.clone(),
            paired: route.paired.clone(),
            credential,
            gate,
        });
        let server = LocalServer::direct(identity, route.id, route.paired.clone(), connector)?;
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
        if std::thread::Builder::new()
            .name("direct-connections".into())
            .spawn(move || {
                loop {
                    let Some(owner) = weak.upgrade() else {
                        break;
                    };
                    work(&owner);
                    drop(owner);
                    std::thread::sleep(Duration::from_millis(250));
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

fn credential_error(error: CredentialError) -> DirectError {
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
        }
    }
}
struct RouteJob {
    saved: SavedRoute,
    interaction: Interaction,
    gate: Arc<std::sync::atomic::AtomicBool>,
    server: Option<LocalServer>,
    due: Instant,
}

fn work(owner: &Arc<Inner>) {
    let (routes, pending) = {
        let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let mut routes = state
            .document
            .routes
            .iter()
            .filter_map(|saved| {
                let runtime = state.routes.get(&saved.route.id)?;
                (saved.active
                    && runtime.retry.due <= now
                    && !state.route_jobs.contains(&saved.route.id)
                    && !matches!(
                        runtime.status,
                        DirectRouteStatus::NeedsInteraction | DirectRouteStatus::Refused(_)
                    ))
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
        (routes, pending)
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
}

fn check_route(owner: &Arc<Inner>, job: RouteJob) {
    let result = match job.server {
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
            }
            runtime.status = DirectRouteStatus::Connected;
            runtime.retry.attempt = 0;
            runtime.retry.due = Instant::now() + Duration::from_secs(5);
        }
        Err(error) => {
            runtime.status = route_failure(error, runtime.server.is_some());
            runtime.retry.delay();
        }
    }
    drop(state);
    owner.notify();
}

fn connect(
    owner: &Inner,
    saved: &SavedRoute,
    interaction: Interaction,
    gate: Arc<std::sync::atomic::AtomicBool>,
) -> Result<LocalServer, DirectError> {
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
        enrollment.submit()?;
        {
            let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(join) = state.joins.get_mut(&id)
                && !join.cancelled
            {
                join.status = JoinStatus::AwaitingApproval {
                    server_name: invitation.server_name().to_owned(),
                    server_fingerprint: invitation.server_pin().fingerprint(),
                    device_fingerprint: credential.fingerprint(),
                    expires_at: invitation.expires_at_unix_seconds(),
                };
            }
        }
        owner.0.notify();
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
            if let Some(paired) = enrollment.finish()? {
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
    let committed = owner.0.save(&mut state);
    if committed {
        state.pending.remove(&id);
        state.routes.insert(
            id,
            RouteState {
                status: DirectRouteStatus::Connecting,
                server: None,
                gate: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                retry: Retry::new(Interaction::Quiet),
            },
        );
    } else {
        // Ordinary connections and presentation require a durable route. The
        // original enrollment keeps its key and resolves only cancellation.
        state.document = original;
        if let Some(pending) = state
            .document
            .pending
            .iter_mut()
            .find(|p| p.operation == id)
        {
            pending.cancel_requested = true;
        }
    }
    if let Some(join) = state.joins.get_mut(&id) {
        join.status = if committed {
            JoinStatus::Approved(id)
        } else {
            JoinStatus::Failed(DirectError::StorageUnavailable)
        };
    }
}

#[cfg(test)]
mod enrollment_tests {
    use super::*;

    #[test]
    fn a_failed_pairing_save_keeps_restricted_cleanup_and_never_presents_a_route() {
        struct UnavailableRecords;
        impl Records for UnavailableRecords {
            fn load(&self) -> Result<Document, ()> {
                Ok(Document::default())
            }
            fn save(&self, _: &Document) -> Result<(), ()> {
                Err(())
            }
        }
        let saved = super::super::tests::saved(1, 5);
        let id = saved.route.id;
        let owner = DirectConnections::new(
            "spaceterm-development",
            Arc::new(UnavailableRecords),
            credentials::native("spaceterm-development"),
            Arc::new(NativeNetwork),
        );
        {
            let mut state = owner.0.state.lock().unwrap();
            state.document.pending.push(PendingRecord::enrollment(
                id,
                saved.route.label.clone(),
                saved.route.endpoint.clone(),
                saved.route.paired.pin.clone(),
                [8; 32],
                saved.credential,
                false,
            ));
            state.pending.insert(id, Retry::new(Interaction::Quiet));
            state.joins.insert(
                id,
                JoinState {
                    status: JoinStatus::Validating,
                    cancelled: false,
                },
            );
        }
        complete_enrollment(&owner, id, saved);
        assert!(owner.route(&id).is_none());
        assert!(owner.server(&id).is_none());
        assert!(matches!(
            owner.join_status(id),
            Some(JoinStatus::Failed(DirectError::StorageUnavailable))
        ));
        let pending = owner.pending_unpairs();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].enrollment_cleanup);
        assert!(!pending[0].server_confirmed);
    }
}
