use std::{
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};

use super::owner_enrollment::OwnerEnrollment;
use super::{
    ConfirmedServiceAction, LingerAuthorization, LingerRequest, LingerState, RuntimeAssessment,
    ServiceAction, ServiceAssessment, ServiceClock, ServiceFuture, ServiceManager, ServiceOutcome,
    ServicePhase, ServiceProcesses, ServiceProgress, ServiceRequestState, ServiceStatus,
    ServiceTime, SupervisorState,
    processes::{NativeClock, cleanup_matches},
};
use super::{
    RegisteredServer, ServiceFailure,
    registration::{REGISTRATION_LIMIT, Registration, validate_path},
};
use crate::private_filesystem::Lease;
use crate::startup::DesiredServerState;
use crate::{
    native_deployment::contracts::NativeRequestId,
    private_filesystem::{Directory, Failure, Snapshot},
    startup::{AccountBuildKey, BuildIdentity},
};
use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    identity::ServerId,
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    operations::RequestDigest,
    retirement::SessionCleanupObservation,
};

pub(super) const JOURNAL_LIMIT: usize = 256 * 1024;
pub(super) const MAX_REQUESTS: usize = 256;
const CAS_ATTEMPTS: usize = 64;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    pub(super) schema: u16,
    pub(super) key: AccountBuildKey,
    pub(super) namespace: [u8; 8],
    pub(super) high_water: u64,
    pub(super) reservations: Vec<Reservation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reservation {
    pub(super) sequence: u64,
    pub(super) owner: Option<OwnerEnrollment>,
    pub(super) acknowledged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) service: Option<RequestRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) install: Option<super::install::InstallRecord>,
}

impl Journal {
    fn new(key: AccountBuildKey) -> Result<Self, ServiceFailure> {
        let mut namespace = [0; 8];
        getrandom::fill(&mut namespace).map_err(|_| ServiceFailure::StorageUnavailable)?;
        if namespace == [0; 8] {
            return Err(ServiceFailure::StorageUnavailable);
        }
        Ok(Self {
            schema: 1,
            key,
            namespace,
            high_water: 0,
            reservations: Vec::new(),
        })
    }
    pub(super) fn validate(&self, key: AccountBuildKey) -> Result<(), ServiceFailure> {
        if self.schema != 1 {
            return Err(ServiceFailure::UnsupportedSchema);
        }
        if self.key != key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        if self.namespace == [0; 8]
            || self.reservations.len() > MAX_REQUESTS
            || !self
                .reservations
                .windows(2)
                .all(|p| p[0].sequence < p[1].sequence)
            || self.reservations.iter().any(|r| {
                r.sequence == 0
                    || r.sequence > self.high_water
                    || (r.acknowledged && r.owner.is_none())
            })
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        if self
            .reservations
            .iter()
            .filter(|r| {
                r.service.as_ref().is_some_and(|s| s.pending())
                    || r.install.as_ref().is_some_and(|s| s.pending())
            })
            .count()
            > 1
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        for reservation in &self.reservations {
            if usize::from(reservation.owner.is_some())
                + usize::from(reservation.service.is_some())
                + usize::from(reservation.install.is_some())
                > 1
            {
                return Err(ServiceFailure::InvalidRecord);
            }
            if let Some(install) = &reservation.install {
                install.validate(key, self.request(reservation.sequence)?)?;
            }
            if let Some(service) = &reservation.service {
                service.validate(key, self.request(reservation.sequence)?)?;
            }
            if let Some(owner) = &reservation.owner {
                owner.validate(key, self.request(reservation.sequence)?)?;
            }
        }
        Ok(())
    }
    pub(super) fn request(&self, sequence: u64) -> Result<NativeRequestId, ServiceFailure> {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&self.namespace);
        bytes[8..].copy_from_slice(&sequence.to_be_bytes());
        NativeRequestId::new(bytes).map_err(|_| ServiceFailure::InvalidRecord)
    }
    pub(super) fn sequence(&self, request: NativeRequestId) -> Result<u64, ServiceFailure> {
        if request.as_bytes()[..8] != self.namespace {
            return Err(ServiceFailure::UnknownRequest);
        }
        let sequence = u64::from_be_bytes(
            request.as_bytes()[8..]
                .try_into()
                .map_err(|_| ServiceFailure::UnknownRequest)?,
        );
        if sequence == 0 || sequence > self.high_water {
            return Err(ServiceFailure::UnknownRequest);
        }
        Ok(sequence)
    }
    pub(super) fn reservation(&self, request: NativeRequestId) -> Result<usize, ServiceFailure> {
        let sequence = self.sequence(request)?;
        self.reservations
            .binary_search_by_key(&sequence, |r| r.sequence)
            .map_err(|_| ServiceFailure::RequestRetired)
    }
}

/// Current-account discovery and durable request authority for the compiled identity.
///
/// This retained provider has no public account, directory or identity override.
#[derive(Clone)]
pub struct ServiceSupervisor {
    pub(super) key: AccountBuildKey,
    pub(super) anchor: PathBuf,
    pub(super) indeterminate: Arc<AtomicBool>,
    pub(super) authorization: Option<Arc<dyn LingerAuthorization>>,
    pub(super) manager: Option<Arc<dyn ServiceManager>>,
    pub(super) processes: Option<Arc<dyn ServiceProcesses>>,
    pub(super) clock: Arc<dyn ServiceClock>,
    #[cfg(feature = "test-support")]
    pub(super) test_installation_trust: Option<spaceterm_distribution::UpdateKey>,
}

impl fmt::Debug for ServiceSupervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServiceSupervisor(..)")
    }
}

impl ServiceSupervisor {
    /// Resolves the actual nonroot account and validated compiled build identity.
    /// It does not create files or contact a service manager.
    pub fn current(
        authorization: Option<Arc<dyn LingerAuthorization>>,
    ) -> Result<Self, ServiceFailure> {
        let (uid, home) = current_account()?;
        let build = match env!("SPACETERM_HOST_BUILD") {
            "release" => BuildIdentity::Release,
            "preflight" => BuildIdentity::Preflight,
            "development" => BuildIdentity::Development,
            _ => return Err(ServiceFailure::UnsupportedHost),
        };
        let name = match build {
            BuildIdentity::Release => "release",
            BuildIdentity::Preflight => "preflight",
            BuildIdentity::Development => "development",
        };
        let installations: Arc<dyn super::ServiceInstallations> =
            Arc::new(super::NativeServiceInstallations);
        #[cfg(target_os = "macos")]
        let manager: Arc<dyn ServiceManager> = Arc::new(super::LaunchdServiceManager::current(
            Arc::clone(&installations),
        )?);
        #[cfg(target_os = "linux")]
        let manager: Arc<dyn ServiceManager> = Arc::new(super::SystemdServiceManager::current(
            Arc::clone(&installations),
        )?);
        let processes = Arc::new(
            super::NativeServiceProcesses::current(installations)?
                .with_manager(manager.clone())
                .with_runtime(Arc::new(super::runtime_inventory::RetainedRuntimeProcesses)),
        );
        Ok(Self {
            key: AccountBuildKey {
                account_uid: uid,
                build_identity: build,
            },
            anchor: home.join(".spaceterm-services").join(name),
            indeterminate: Arc::new(AtomicBool::new(false)),
            authorization,
            manager: Some(manager),
            processes: Some(processes),
            clock: Arc::new(NativeClock),
            #[cfg(feature = "test-support")]
            test_installation_trust: None,
        })
    }
    /// Composes the owning runtime's admission and descendant-custody port with
    /// actual-account native process and signed executable observations.
    /// This does not replace account, storage, manager or artifact authority.
    pub fn with_runtime_processes(
        mut self,
        runtime: Arc<dyn ServiceProcesses>,
    ) -> Result<Self, ServiceFailure> {
        let installations: Arc<dyn super::ServiceInstallations> =
            Arc::new(super::NativeServiceInstallations);
        self.processes = Some(Arc::new(
            super::NativeServiceProcesses::current(installations)?
                .with_manager(
                    self.manager
                        .as_ref()
                        .ok_or(ServiceFailure::SupervisorUnavailable)?
                        .clone(),
                )
                .with_runtime(runtime),
        ));
        Ok(self)
    }
    /// Returns the current account and compiled identity.
    pub fn key(&self) -> AccountBuildKey {
        self.key
    }
    /// Client journals remain account/build scoped even after service registration
    /// is removed or cannot be read. This path is never selected by remote data.
    pub fn client_connections_directory(&self) -> PathBuf {
        self.anchor.join("client-connections")
    }
    /// Deliberate enrollment owns the private client storage lease. This never
    /// reads or opens the server's organization database.
    pub fn admit_client_provisioning(&self) -> Result<ClientProvisioning, ServiceFailure> {
        let path = self.client_connections_directory();
        let directory = Directory::create(&path)?;
        let lease = directory.lease("provisioning-lock")?;
        Ok(ClientProvisioning {
            path,
            directory,
            lease,
        })
    }
    /// Restricted local retries may reopen only the fixed account/build runtime.
    /// Missing runtime storage gives no route authority and creates nothing.
    pub fn retained_local_endpoint(
        &self,
    ) -> Result<Option<crate::ipc::IpcEndpoint>, ServiceFailure> {
        let runtime = self.anchor.join("runtime");
        let Some(directory) = Directory::open(&runtime)? else {
            return Ok(None);
        };
        directory.verify()?;
        crate::ipc::PrivateIpcDirectory::open(&runtime)
            .and_then(|directory| directory.endpoint("server"))
            .map(Some)
            .map_err(|_| ServiceFailure::UnsafeStorage)
    }
    /// Reads an existing fixed registration and opens its existing private storage.
    /// Missing registration never causes key, directory or database creation.
    pub fn discover(&self) -> Result<Option<RegisteredServer>, ServiceFailure> {
        self.available()?;
        let Some(anchor) = Directory::open(&self.anchor)? else {
            return Ok(None);
        };
        let Some(snapshot) = anchor.read("registration", REGISTRATION_LIMIT)? else {
            return Ok(None);
        };
        let record: Registration = decode(&snapshot.bytes, REGISTRATION_LIMIT)?;
        record.validate(self.key)?;
        let directory = Directory::open(&record.data)?.ok_or(ServiceFailure::Missing)?;
        directory.verify_established()?;
        Ok(Some(RegisteredServer { record, directory }))
    }
    /// Account/build-isolated local CLI receipt storage. The path is compiled local
    /// authority, never a route or a server-supplied value. Creation is explicit.
    pub fn cli_result_directory(&self, create: bool) -> Result<Option<PathBuf>, ServiceFailure> {
        self.available()?;
        let path = self.anchor.join("cli-results");
        if create {
            Directory::create(&self.anchor)?;
            Directory::create(&path)?;
            Ok(Some(path))
        } else {
            Ok(Directory::open(&path)?.map(|_| path))
        }
    }
    /// Reserves a durable, provider-issued non-reused ID before submission.
    /// Reservation creates no identity, artifact, service or authorization effect.
    /// At capacity, the oldest never-submitted reservation can be retired. A caller
    /// retaining it must handle RequestRetired; no accepted outcome is evicted.
    pub fn allocate_request(&self) -> Result<NativeRequestId, ServiceFailure> {
        self.available()?;
        let directory = Directory::create(&self.anchor)?;
        for _ in 0..CAS_ATTEMPTS {
            let snapshot = directory.read("requests", JOURNAL_LIMIT)?;
            let mut journal = match &snapshot {
                Some(snapshot) => {
                    let journal: Journal = decode(&snapshot.bytes, JOURNAL_LIMIT)?;
                    journal.validate(self.key)?;
                    journal
                }
                None => Journal::new(self.key)?,
            };
            if journal.reservations.iter().any(|reservation| {
                reservation
                    .service
                    .as_ref()
                    .is_some_and(|service| service.phase == ServicePhase::Reserved)
            }) {
                let now = self.clock.now()?;
                // An expired, never-confirmed assessment admitted no mutation.
                // Its permanent sequence remains retired, while every accepted
                // outcome and unresolved effect keeps its reserved capacity.
                journal.reservations.retain(|reservation| !reservation.service.as_ref().is_some_and(|service| {
                    service.phase == ServicePhase::Reserved && !service.reserving && matches!(&service.binding,
                        RequestBinding::Confirmed(assessment) if assessment.check_time(now).is_err())
                }));
            }
            if journal.reservations.len() == MAX_REQUESTS {
                // Reservations have accepted no action. Under pressure retire the
                // oldest unsubmitted ID, never an unacknowledged original outcome.
                // Its sequence remains below the permanent high-water mark, so a
                // delayed submitter gets RequestRetired rather than fresh authority.
                let Some(unsubmitted) = journal
                    .reservations
                    .iter()
                    .position(|r| r.owner.is_none() && r.service.is_none() && r.install.is_none())
                else {
                    return Err(ServiceFailure::LimitExceeded);
                };
                journal.reservations.remove(unsubmitted);
            }
            journal.high_water = journal
                .high_water
                .checked_add(1)
                .ok_or(ServiceFailure::LimitExceeded)?;
            let request = journal.request(journal.high_water)?;
            journal.reservations.push(Reservation {
                sequence: journal.high_water,
                owner: None,
                acknowledged: false,
                service: None,
                install: None,
            });
            match self.publish_journal(&directory, &journal, snapshot.as_ref()) {
                Ok(()) => return Ok(request),
                Err(ServiceFailure::RequestConflict) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(ServiceFailure::Pending)
    }
    /// Reads this registered server's accepted requests and unacknowledged immutable outcomes.
    /// An unconfirmed assessment is not recovery work and is never submitted here.
    pub fn retained_service_requests(&self) -> Result<Vec<ServiceProgress>, ServiceFailure> {
        let pin = self
            .discover()?
            .ok_or(ServiceFailure::Missing)?
            .server_pin();
        let journal = match self.read_journal() {
            Ok((_, _, journal)) => journal,
            Err(ServiceFailure::Missing) => return Ok(Vec::new()),
            Err(failure) => return Err(failure),
        };
        journal
            .reservations
            .iter()
            .filter_map(|reservation| {
                let record = reservation.service.as_ref()?;
                let basis = record.binding.basis();
                if basis.server != pin.identity || basis.public_key != pin.public_key {
                    return None;
                }
                (record.phase != ServicePhase::Reserved || record.reserving).then(|| {
                    journal
                        .request(reservation.sequence)
                        .map(|request| record.progress(request))
                })
            })
            .collect()
    }

    /// Retires a completed owner result after synchronizing its separate authority record.
    /// The namespace/high-water record permanently prevents readmission of the ID.
    /// Pairing can still revalidate the original issuance without consuming an active slot.
    /// Unsubmitted reservations are not completed outcomes and cannot be acknowledged.
    pub fn acknowledge_request(&self, request: NativeRequestId) -> Result<(), ServiceFailure> {
        self.reconcile_storage(request)?;
        let (_, _, journal) = self.read_journal()?;
        let index = match journal.reservation(request) {
            Ok(index) => index,
            Err(ServiceFailure::RequestRetired) => return self.acknowledge_owner_request(request),
            Err(error) => return Err(error),
        };
        if journal.reservations[index].owner.is_some() {
            return self.acknowledge_owner_request(request);
        }
        self.mutate_request(request, |journal, index| {
            let reservation = &journal.reservations[index];
            if !reservation
                .service
                .as_ref()
                .is_some_and(|record| record.phase == ServicePhase::Completed)
                && !reservation
                    .install
                    .as_ref()
                    .is_some_and(|record| record.phase == ServicePhase::Completed)
            {
                return Err(ServiceFailure::Pending);
            }
            journal.reservations.remove(index);
            Ok(())
        })
    }
    pub(super) fn read_journal(&self) -> Result<(Directory, Snapshot, Journal), ServiceFailure> {
        self.available()?;
        let directory = Directory::open(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        let snapshot = directory
            .read("requests", JOURNAL_LIMIT)?
            .ok_or(ServiceFailure::UnknownRequest)?;
        let journal: Journal = decode(&snapshot.bytes, JOURNAL_LIMIT)?;
        journal.validate(self.key)?;
        Ok((directory, snapshot, journal))
    }
    pub(super) fn mutate_request(
        &self,
        request: NativeRequestId,
        mutation: impl Fn(&mut Journal, usize) -> Result<(), ServiceFailure>,
    ) -> Result<(), ServiceFailure> {
        for _ in 0..CAS_ATTEMPTS {
            let (directory, snapshot, mut journal) = self.read_journal()?;
            let index = journal.reservation(request)?;
            mutation(&mut journal, index)?;
            match self.publish_journal(&directory, &journal, Some(&snapshot)) {
                Ok(()) => return Ok(()),
                Err(ServiceFailure::RequestConflict) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(ServiceFailure::Pending)
    }
    fn publish_journal(
        &self,
        directory: &Directory,
        journal: &Journal,
        expected: Option<&Snapshot>,
    ) -> Result<(), ServiceFailure> {
        journal.validate(self.key)?;
        let bytes = encode(journal, JOURNAL_LIMIT)?;
        let mut reserved = bytes.len();
        for service in journal
            .reservations
            .iter()
            .filter_map(|r| r.service.as_ref())
        {
            reserved = reserved
                .checked_add(service.reserved_growth()?)
                .ok_or(ServiceFailure::LimitExceeded)?;
        }
        reserved = reserved
            .checked_add(
                journal
                    .reservations
                    .iter()
                    .filter(|r| r.install.as_ref().is_some_and(|record| record.pending()))
                    .count()
                    * 4096,
            )
            .ok_or(ServiceFailure::LimitExceeded)?;
        if reserved > JOURNAL_LIMIT {
            return Err(ServiceFailure::LimitExceeded);
        }
        if expected.is_some_and(|snapshot| snapshot.bytes.as_slice() == bytes.as_slice()) {
            return Ok(());
        }
        let result = directory.publish("requests", &bytes, expected);
        if result == Err(Failure::Indeterminate) {
            self.indeterminate.store(true, Ordering::Release);
        }
        result.map_err(Into::into)
    }
    pub(super) fn available(&self) -> Result<(), ServiceFailure> {
        if self.indeterminate.load(Ordering::Acquire) {
            Err(ServiceFailure::Indeterminate)
        } else {
            Ok(())
        }
    }
}

pub(super) fn encode<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, ServiceFailure> {
    let mut value = serde_json::to_value(value).map_err(|_| ServiceFailure::InvalidRecord)?;
    value.sort_all_objects();
    let bytes = serde_json::to_vec(&value).map_err(|_| ServiceFailure::InvalidRecord)?;
    if bytes.len() > limit {
        return Err(ServiceFailure::LimitExceeded);
    }
    Ok(bytes)
}
pub(super) fn decode<T: Serialize + serde::de::DeserializeOwned>(
    bytes: &[u8],
    limit: usize,
) -> Result<T, ServiceFailure> {
    if bytes.len() > limit {
        return Err(ServiceFailure::LimitExceeded);
    }
    let value: T = serde_json::from_slice(bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
    if encode(&value, limit)? != bytes {
        return Err(ServiceFailure::InvalidRecord);
    }
    Ok(value)
}

pub(super) fn current_account() -> Result<(u32, PathBuf), ServiceFailure> {
    let account = super::TrustedAccountFacts::current()?;
    Ok((account.uid(), account.home().to_owned()))
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RegistrationBasis {
    generation: u64,
    server: [u8; 16],
    public_key: [u8; 32],
    desired: DesiredServerState,
    approved: Option<super::InstallationStatus>,
    staged: Option<super::InstallationStatus>,
    installed: bool,
}
impl RegistrationBasis {
    pub(super) fn capture(registration: &RegisteredServer) -> Self {
        Self::capture_record(&registration.record)
    }
    fn capture_record(record: &Registration) -> Self {
        Self {
            generation: record.generation,
            server: record.server,
            public_key: record.public_key,
            desired: record.desired,
            approved: record.approved.clone(),
            staged: record.staged.clone(),
            installed: record.installed,
        }
    }
    pub(super) fn matches(&self, registration: &RegisteredServer) -> bool {
        *self == Self::capture(registration)
    }
    pub(super) fn matches_stage_publication(
        &self,
        registration: &RegisteredServer,
        status: &super::InstallationStatus,
    ) -> bool {
        let Some(generation) = self.generation.checked_add(1) else {
            return false;
        };
        let mut expected = self.clone();
        expected.generation = generation;
        expected.staged = Some(status.clone());
        expected.matches(registration)
    }
    pub(super) fn validate(&self) -> Result<(), ServiceFailure> {
        if self.generation == 0
            || self.server == [0; 16]
            || self.public_key == [0; 32]
            || self
                .approved
                .iter()
                .chain(self.staged.iter())
                .any(|s| s.artifact_sha256 == [0; 32])
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AssessmentRecord {
    pub(super) action: ServiceAction,
    basis: RegistrationBasis,
    pub(super) runtime: RuntimeAssessment,
    issued: ServiceTime,
    pub(super) token: [u8; 32],
}
impl AssessmentRecord {
    pub(super) fn check_time(&self, now: ServiceTime) -> Result<(), ServiceFailure> {
        if now.boot != self.issued.boot
            || now.millis < self.issued.millis
            || now.millis.saturating_sub(self.issued.millis) >= 30_000
        {
            return Err(ServiceFailure::StaleAssessment);
        }
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
enum RequestBinding {
    Start(RegistrationBasis),
    Confirmed(AssessmentRecord),
}
impl RequestBinding {
    fn basis(&self) -> &RegistrationBasis {
        match self {
            Self::Start(basis) => basis,
            Self::Confirmed(assessment) => &assessment.basis,
        }
    }
    fn retirement(&self) -> Option<NativeRetirementAction> {
        match self {
            Self::Start(_) => None,
            Self::Confirmed(assessment) => Some(match assessment.action {
                ServiceAction::Stop | ServiceAction::Uninstall => NativeRetirementAction::Stop,
                ServiceAction::Restart => NativeRetirementAction::RestartApproved,
                ServiceAction::Activate(_) => NativeRetirementAction::ActivateUpdate,
            }),
        }
    }
    fn digest(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<RequestDigest, ServiceFailure> {
        use sha2::{Digest as _, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"spaceterm-account-service-request-v1\0");
        digest.update(encode(&key, 1024)?);
        digest.update(request.as_bytes());
        digest.update(encode(self, JOURNAL_LIMIT)?);
        Ok(RequestDigest::from_bytes(digest.finalize().into()))
    }
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestRecord {
    binding: RequestBinding,
    digest: RequestDigest,
    phase: ServicePhase,
    state: ServiceRequestState,
    // Durable ownership before runtime reservation, including pre-acceptance crashes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reserving: bool,
    // Native-only durable non-admission. An ordinary wire token cannot issue it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    aborting: bool,
}
impl RequestRecord {
    fn pending(&self) -> bool {
        self.reserving
            || (self.phase != ServicePhase::Reserved && self.phase != ServicePhase::Completed)
    }
    fn progress(&self, request: NativeRequestId) -> ServiceProgress {
        ServiceProgress {
            request,
            request_digest: self.digest,
            phase: self.phase,
            state: self.state.clone(),
        }
    }
    fn reserved_growth(&self) -> Result<usize, ServiceFailure> {
        if self.phase == ServicePhase::Completed {
            return Ok(0);
        }
        let current = encode(self, JOURNAL_LIMIT)?.len();
        let mut largest = self.clone();
        // Combine the longest phase with every field that any successor may carry.
        // This upper bound is reserved at binding and remains unavailable to new IDs.
        largest.phase = ServicePhase::IntentCommitted;
        largest.reserving = matches!(self.binding, RequestBinding::Confirmed(_));
        largest.aborting = largest.reserving;
        largest.state = ServiceRequestState::Completed(self.binding.outcome()?);
        let completed = encode(&largest, JOURNAL_LIMIT)?.len();
        largest.state = ServiceRequestState::NotApplied(ServiceFailure::AuthorizationUnavailable);
        let not_applied = encode(&largest, JOURNAL_LIMIT)?.len();
        Ok(completed.max(not_applied).saturating_sub(current))
    }
    fn validate(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        self.binding.basis().validate()?;
        if (self.aborting && (!self.reserving || self.phase != ServicePhase::Reserved))
            || (self.reserving
                && (self.phase != ServicePhase::Reserved
                    || !matches!(self.binding, RequestBinding::Confirmed(_))))
            || self.digest != self.binding.digest(key, request)?
            || (self.phase == ServicePhase::Completed)
                == matches!(
                    self.state,
                    ServiceRequestState::Pending | ServiceRequestState::Indeterminate
                )
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        if let RequestBinding::Confirmed(assessment) = &self.binding
            && (assessment.token == [0; 32]
                || assessment.issued.boot == [0; 32]
                || assessment.runtime.server.bytes() != assessment.basis.server
                || assessment.runtime.sessions.len() > 64
                || !assessment
                    .runtime
                    .sessions
                    .windows(2)
                    .all(|pair| pair[0] < pair[1])
                || assessment.runtime.sessions.iter().any(|session| {
                    session.pane.server() != assessment.runtime.server
                        || Some(session.epoch) != assessment.runtime.epoch
                        || session.generation.get() == 0
                })
                || assessment.runtime.generation == 0
                || assessment.runtime.token == [0; 32]
                || assessment
                    .runtime
                    .epoch
                    .is_some_and(|epoch| epoch.get() == 0)
                || (assessment.action == ServiceAction::Restart
                    && assessment.basis.desired != DesiredServerState::Running)
                || matches!(assessment.action, ServiceAction::Activate(id)
                    if assessment.basis.staged.as_ref().is_none_or(|candidate| candidate.installation != id)))
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        if let ServiceRequestState::Completed(outcome) = &self.state {
            let matches = match (&self.binding, outcome) {
                (RequestBinding::Start(_), ServiceOutcome::Started) => true,
                (RequestBinding::Confirmed(assessment), outcome) => {
                    match (assessment.action, outcome) {
                        (ServiceAction::Stop, ServiceOutcome::Stopped)
                        | (ServiceAction::Restart, ServiceOutcome::Restarted)
                        | (ServiceAction::Uninstall, ServiceOutcome::Uninstalled) => true,
                        (ServiceAction::Activate(id), ServiceOutcome::Activated(status)) => {
                            status.installation == id
                                && assessment.basis.staged.as_ref() == Some(status)
                        }
                        _ => false,
                    }
                }
                _ => false,
            };
            if !matches {
                return Err(ServiceFailure::InvalidRecord);
            }
        }
        Ok(())
    }
}

impl ServiceSupervisor {
    pub(super) fn manager(&self) -> Result<&dyn ServiceManager, ServiceFailure> {
        self.manager
            .as_deref()
            .ok_or(ServiceFailure::SupervisorUnavailable)
    }
    fn processes(&self) -> Result<&dyn ServiceProcesses, ServiceFailure> {
        self.processes
            .as_deref()
            .ok_or(ServiceFailure::CleanupUnresolved)
    }
    pub(super) fn lifecycle_lease(&self) -> Result<Lease, ServiceFailure> {
        Directory::open(&self.anchor)?
            .ok_or(ServiceFailure::Missing)?
            .lease("service-lock")
            .map_err(Into::into)
    }
    fn original(&self, request: NativeRequestId) -> Result<Option<RequestRecord>, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        let index = journal.reservation(request)?;
        if journal.reservations[index].owner.is_some()
            || journal.reservations[index].install.is_some()
        {
            return Err(ServiceFailure::RequestConflict);
        }
        Ok(journal.reservations[index].service.clone())
    }
    pub(super) fn ensure_no_pending(&self, request: NativeRequestId) -> Result<(), ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        if journal.reservations.iter().any(|reservation| {
            journal.request(reservation.sequence).ok() != Some(request)
                && (reservation
                    .service
                    .as_ref()
                    .is_some_and(RequestRecord::pending)
                    || reservation
                        .install
                        .as_ref()
                        .is_some_and(|record| record.pending()))
        }) {
            return Err(ServiceFailure::Pending);
        }
        Ok(())
    }
    fn write_request(
        &self,
        request: NativeRequestId,
        record: &RequestRecord,
    ) -> Result<(), ServiceFailure> {
        self.mutate_request(request, |journal, index| {
            if journal.reservations[index].owner.is_some()
                || journal.reservations[index].install.is_some()
            {
                return Err(ServiceFailure::RequestConflict);
            }
            if let Some(original) = &journal.reservations[index].service {
                if original.binding != record.binding || original.digest != record.digest {
                    return Err(ServiceFailure::RequestConflict);
                }
                if original.phase == ServicePhase::Completed && original != record {
                    return Err(ServiceFailure::RequestConflict);
                }
            }
            journal.reservations[index].service = Some(record.clone());
            Ok(())
        })?;
        #[cfg(feature = "test-support")]
        super::test_support::after_phase(record.phase)?;
        Ok(())
    }
    fn phase(
        &self,
        request: NativeRequestId,
        record: &mut RequestRecord,
        phase: ServicePhase,
    ) -> Result<(), ServiceFailure> {
        record.phase = phase;
        self.write_request(request, record)
    }
    fn complete(
        &self,
        request: NativeRequestId,
        record: &mut RequestRecord,
        outcome: ServiceOutcome,
    ) -> Result<ServiceProgress, ServiceFailure> {
        record.phase = ServicePhase::Completed;
        record.state = ServiceRequestState::Completed(outcome);
        self.write_request(request, record)?;
        Ok(record.progress(request))
    }
    pub(super) fn read_registration(
        &self,
    ) -> Result<Option<(Directory, Snapshot, Registration)>, ServiceFailure> {
        self.available()?;
        let Some(directory) = Directory::open(&self.anchor)? else {
            return Ok(None);
        };
        let Some(snapshot) = directory.read("registration", REGISTRATION_LIMIT)? else {
            return Ok(None);
        };
        let record: Registration = decode(&snapshot.bytes, REGISTRATION_LIMIT)?;
        record.validate(self.key)?;
        Ok(Some((directory, snapshot, record)))
    }
    pub(super) fn publish_registration(
        &self,
        record: &Registration,
        expected: &Snapshot,
        directory: &Directory,
    ) -> Result<(), ServiceFailure> {
        record.validate(self.key)?;
        let result = directory.publish(
            "registration",
            &encode(record, REGISTRATION_LIMIT)?,
            Some(expected),
        );
        if result == Err(Failure::Indeterminate) {
            self.indeterminate.store(true, Ordering::Release);
        }
        result.map_err(Into::into)
    }
    fn commit_intent(
        &self,
        _request: NativeRequestId,
        record: &RequestRecord,
    ) -> Result<(), ServiceFailure> {
        let (directory, snapshot, mut registration) =
            self.read_registration()?.ok_or(ServiceFailure::Missing)?;
        let before = record.binding.basis();
        let desired = match &record.binding {
            RequestBinding::Start(_) => DesiredServerState::Running,
            RequestBinding::Confirmed(assessment) => match assessment.action {
                ServiceAction::Stop | ServiceAction::Uninstall => DesiredServerState::Stopped,
                ServiceAction::Restart | ServiceAction::Activate(_) => before.desired,
            },
        };
        let current = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if before.matches(&current) {
            if registration.desired != desired {
                registration.desired = desired;
                registration.generation = registration
                    .generation
                    .checked_add(1)
                    .ok_or(ServiceFailure::LimitExceeded)?;
                self.publish_registration(&registration, &snapshot, &directory)?;
            }
        } else {
            // Recovery can observe the original intent publication without its next
            // journal phase. Compare only its exact expected successor, not fresh state.
            let mut expected = before.clone();
            if expected.desired != desired {
                expected.desired = desired;
                expected.generation = expected
                    .generation
                    .checked_add(1)
                    .ok_or(ServiceFailure::LimitExceeded)?;
            }
            if !expected.matches(&current) {
                return Err(ServiceFailure::RequestConflict);
            }
        }
        Ok(())
    }
    /// Reads the original durable progress without resolving or executing it.
    pub fn request_progress(
        &self,
        request: NativeRequestId,
    ) -> Result<ServiceProgress, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        let record = &journal.reservations[journal.reservation(request)?];
        if let Some(install) = &record.install {
            return Ok(install.progress(request));
        }
        record
            .service
            .as_ref()
            .map(|service| service.progress(request))
            .ok_or(ServiceFailure::Pending)
    }

    /// Authenticates selected code and resources again. No directory scan,
    /// activation, initialization, process start or authorization is performed.
    pub fn verified_installation_selection(
        &self,
    ) -> Result<super::VerifiedInstallationSelection, ServiceFailure> {
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        #[cfg(feature = "test-support")]
        let store = match &self.test_installation_trust {
            Some(key) => crate::server_installation::test_support::existing_store(
                &registration,
                key.clone(),
            )?,
            None => {
                crate::server_installation::ServerInstallationStore::open_existing(&registration)?
            }
        };
        #[cfg(not(feature = "test-support"))]
        let store =
            crate::server_installation::ServerInstallationStore::open_existing(&registration)?;
        let before = store.snapshot()?;
        let approved = store
            .retain_compatible_approved(crate::organization_store::schema::SCHEMA_VERSION)?
            .status();
        let staged = store.staged_status()?;
        if store.snapshot()? != before {
            return Err(ServiceFailure::Pending);
        }
        let convert =
            |value: crate::server_installation::InstallationStatus| super::InstallationStatus {
                installation: value.installation,
                version: value.version,
                artifact_sha256: value.artifact_sha256,
            };
        let selected = super::VerifiedInstallationSelection {
            approved: convert(approved),
            staged: staged.map(convert),
        };
        let current = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if current.server_pin() != registration.server_pin()
            || current.generation() != registration.generation()
            || current.approved_installation() != Some(&selected.approved)
            || current.staged_installation() != selected.staged.as_ref()
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Ok(selected)
    }

    /// Observes current facts without keys, authorization, initialization or startup.
    pub fn status(&self) -> ServiceFuture<'_, ServiceStatus> {
        Box::pin(async move {
            let registration = self.discover()?;
            let pending = match self.read_journal() {
                Ok((_, _, journal)) => journal
                    .reservations
                    .iter()
                    .find(|r| {
                        r.service.as_ref().is_some_and(RequestRecord::pending)
                            || r.install.as_ref().is_some_and(|record| record.pending())
                    })
                    .map(|r| journal.request(r.sequence))
                    .transpose()?,
                Err(ServiceFailure::Missing | ServiceFailure::UnknownRequest) => None,
                Err(error) => return Err(error),
            };
            let manager = self.manager()?;
            Ok(ServiceStatus {
                key: self.key,
                server: registration
                    .as_ref()
                    .map(|r| ServerId::new(r.server_pin().identity)),
                desired: registration.as_ref().map(RegisteredServer::desired_state),
                approved: registration
                    .as_ref()
                    .and_then(|r| r.record.approved.clone()),
                staged: registration.as_ref().and_then(|r| r.record.staged.clone()),
                supervisor: manager.status(registration.as_ref()).await?,
                linger: manager.linger().await?,
                pending,
            })
        })
    }
    /// Reads protocol compatibility from the exact authenticated approved installation.
    /// Neither the helper's own version nor the newest staged artifact substitutes for it.
    pub fn approved_protocol_majors(&self) -> Result<Vec<u16>, ServiceFailure> {
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        let expected = registration
            .approved_installation()
            .ok_or(ServiceFailure::Missing)?;
        #[cfg(feature = "test-support")]
        let store = if let Some(key) = &self.test_installation_trust {
            crate::server_installation::test_support::existing_store(&registration, key.clone())?
        } else {
            crate::server_installation::ServerInstallationStore::open_existing(&registration)?
        };
        #[cfg(not(feature = "test-support"))]
        let store =
            crate::server_installation::ServerInstallationStore::open_existing(&registration)?;
        let executable =
            store.retain_compatible_approved(crate::organization_store::schema::SCHEMA_VERSION)?;
        let actual = executable.status();
        if actual.installation != expected.installation
            || actual.version != expected.version
            || actual.artifact_sha256 != expected.artifact_sha256
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Ok(executable.protocol_majors().to_vec())
    }
    /// Captures and persists one exact runtime scope and its original 30-second expiry.
    pub fn assess(
        &self,
        request: NativeRequestId,
        action: ServiceAction,
    ) -> ServiceFuture<'_, ServiceAssessment> {
        Box::pin(async move {
            if let Some(original) = self.original(request)? {
                return match original.binding {
                    RequestBinding::Confirmed(record) if record.action == action => {
                        self.assessment(request, record)
                    }
                    _ => Err(ServiceFailure::RequestConflict),
                };
            }
            let lease = self.lifecycle_lease()?;
            self.ensure_no_pending(request)?;
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if !registration.is_installed() {
                return Err(ServiceFailure::Missing);
            }
            match action {
                ServiceAction::Restart
                    if registration.desired_state() != DesiredServerState::Running =>
                {
                    return Err(ServiceFailure::StaleAssessment);
                }
                ServiceAction::Activate(id)
                    if registration
                        .record
                        .staged
                        .as_ref()
                        .is_none_or(|s| s.installation != id) =>
                {
                    return Err(ServiceFailure::StaleAssessment);
                }
                _ => {}
            }
            let issued = self.clock.now()?;
            let runtime = self.processes()?.assess(&registration).await?;
            runtime.validate(&registration)?;
            let mut token = [0; 32];
            getrandom::fill(&mut token).map_err(|_| ServiceFailure::StorageUnavailable)?;
            if token == [0; 32] {
                return Err(ServiceFailure::StorageUnavailable);
            }
            let assessment = AssessmentRecord {
                action,
                basis: RegistrationBasis::capture(&registration),
                runtime,
                issued,
                token,
            };
            assessment.check_time(self.clock.now()?)?;
            let binding = RequestBinding::Confirmed(assessment.clone());
            let record = RequestRecord {
                digest: binding.digest(self.key, request)?,
                binding,
                phase: ServicePhase::Reserved,
                state: ServiceRequestState::Pending,
                reserving: false,
                aborting: false,
            };
            lease.verify()?;
            self.write_request(request, &record)?;
            self.assessment(request, assessment)
        })
    }
    fn assessment(
        &self,
        request: NativeRequestId,
        record: AssessmentRecord,
    ) -> Result<ServiceAssessment, ServiceFailure> {
        record.check_time(self.clock.now()?)?;
        Ok(ServiceAssessment {
            request,
            record,
            clock: Arc::clone(&self.clock),
        })
    }
    /// Restores the exact original assessment, including its original expiry.
    pub fn restore_assessment(
        &self,
        request: NativeRequestId,
        token: [u8; 32],
    ) -> ServiceFuture<'_, ServiceAssessment> {
        Box::pin(async move {
            let original = self.original(request)?.ok_or(ServiceFailure::Missing)?;
            let RequestBinding::Confirmed(record) = original.binding else {
                return Err(ServiceFailure::RequestConflict);
            };
            if record.token != token {
                return Err(ServiceFailure::RequestConflict);
            }
            self.assessment(request, record)
        })
    }
    /// Starts only approved code under an ID retained before submission.
    pub fn start(&self, request: NativeRequestId) -> ServiceFuture<'_, ServiceProgress> {
        self.observe(request, None)
    }
    /// Durably suspends startup before requesting runtime or manager Stop.
    pub fn stop(&self, action: ConfirmedServiceAction) -> ServiceFuture<'_, ServiceProgress> {
        self.confirmed(action, ServiceAction::Stop)
    }
    /// Reconciles one Running-only approved restart, never a staged activation.
    pub fn restart(&self, action: ConfirmedServiceAction) -> ServiceFuture<'_, ServiceProgress> {
        self.confirmed(action, ServiceAction::Restart)
    }
    /// Removes verified installed code and this configuration, preserving data and linger.
    pub fn uninstall(&self, action: ConfirmedServiceAction) -> ServiceFuture<'_, ServiceProgress> {
        self.confirmed(action, ServiceAction::Uninstall)
    }
    /// Activates precisely the originally assessed staged artifact, preserving Stopped.
    pub fn activate(&self, action: ConfirmedServiceAction) -> ServiceFuture<'_, ServiceProgress> {
        let expected = action.0.record.action;
        if !matches!(expected, ServiceAction::Activate(_)) {
            return Box::pin(async { Err(ServiceFailure::RequestConflict) });
        }
        self.confirmed(action, expected)
    }
    fn confirmed(
        &self,
        action: ConfirmedServiceAction,
        expected: ServiceAction,
    ) -> ServiceFuture<'_, ServiceProgress> {
        if action.0.record.action != expected {
            return Box::pin(async { Err(ServiceFailure::RequestConflict) });
        }
        self.observe(action.0.request, Some(action.0.record))
    }
    fn observe(
        &self,
        request: NativeRequestId,
        assessment: Option<AssessmentRecord>,
    ) -> ServiceFuture<'_, ServiceProgress> {
        let supervisor = self.clone();
        Box::pin(async move {
            let handle = tokio::runtime::Handle::try_current()
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            let worker =
                handle.spawn(async move { supervisor.run(request, assessment, false).await });
            worker.await.map_err(|_| ServiceFailure::Indeterminate)?
        })
    }
    /// Resolves only the already bound original request. It never allocates or changes action.
    pub fn resolve(&self, request: NativeRequestId) -> ServiceFuture<'_, ServiceProgress> {
        let supervisor = self.clone();
        Box::pin(async move {
            supervisor.reconcile_storage(request)?;
            let (_, _, journal) = supervisor.read_journal()?;
            if journal.reservations[journal.reservation(request)?]
                .install
                .is_some()
            {
                return supervisor.resolve_install(request).await;
            }
            let original = supervisor
                .original(request)?
                .ok_or(ServiceFailure::Pending)?;
            if (original.phase == ServicePhase::Reserved && !original.reserving)
                || original.phase == ServicePhase::Completed
            {
                return Ok(original.progress(request));
            }
            let handle = tokio::runtime::Handle::try_current()
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            let worker = handle.spawn(async move { supervisor.run(request, None, true).await });
            worker.await.map_err(|_| ServiceFailure::Indeterminate)?
        })
    }
    pub(super) fn reconcile_storage(&self, request: NativeRequestId) -> Result<(), ServiceFailure> {
        Directory::reconcile_initialization(&self.anchor)?;
        match Directory::open(&self.anchor) {
            Ok(Some(directory)) => {
                // Another explicit resolver may already have published this
                // journal's synchronized original checkpoint. Validate it before
                // clearing this observer's stale in-memory uncertainty latch.
                if self.indeterminate.load(Ordering::Acquire) {
                    let snapshot = directory
                        .read("requests", JOURNAL_LIMIT)?
                        .ok_or(ServiceFailure::UnknownRequest)?;
                    let journal: Journal = decode(&snapshot.bytes, JOURNAL_LIMIT)?;
                    journal.validate(self.key)?;
                    journal.sequence(request)?;
                    self.indeterminate.store(false, Ordering::Release);
                }
                return Ok(());
            }
            Ok(None) => return Err(ServiceFailure::Missing),
            Err(Failure::Indeterminate) => {}
            Err(error) => return Err(error.into()),
        }
        let key = self.key;
        let journal_recovery =
            Directory::reconcile_publication(&self.anchor, "requests", JOURNAL_LIMIT, |bytes| {
                let journal: Journal = decode(bytes, JOURNAL_LIMIT).map_err(|_| Failure::Unsafe)?;
                journal.validate(key).map_err(|_| Failure::Unsafe)?;
                journal.sequence(request).map_err(|_| Failure::Unsafe)?;
                Ok(())
            });
        if journal_recovery.is_err() {
            let registration_recovery = Directory::reconcile_publication(
                &self.anchor,
                "registration",
                REGISTRATION_LIMIT,
                |bytes| {
                    let registration: Registration =
                        decode(bytes, REGISTRATION_LIMIT).map_err(|_| Failure::Unsafe)?;
                    registration.validate(key).map_err(|_| Failure::Unsafe)
                },
            );
            if registration_recovery.is_err() {
                self.reconcile_owner_archive(request)?;
            }
        }
        self.indeterminate.store(false, Ordering::Release);
        Ok(())
    }
    async fn run(
        &self,
        request: NativeRequestId,
        assessment: Option<AssessmentRecord>,
        resolving: bool,
    ) -> Result<ServiceProgress, ServiceFailure> {
        if let Some(original) = self.original(request)? {
            if !resolving {
                let matches = match (&original.binding, &assessment) {
                    (RequestBinding::Start(_), None) => true,
                    (RequestBinding::Confirmed(old), Some(new)) => old == new,
                    _ => false,
                };
                if !matches {
                    return Err(ServiceFailure::RequestConflict);
                }
            }
            if original.phase == ServicePhase::Completed {
                return Ok(original.progress(request));
            }
        } else if resolving {
            return Err(ServiceFailure::Pending);
        }
        let lease = match self.lifecycle_lease() {
            Ok(lease) => lease,
            Err(ServiceFailure::Pending) => {
                return self
                    .original(request)?
                    .map(|original| original.progress(request))
                    .ok_or(ServiceFailure::Pending);
            }
            Err(error) => return Err(error),
        };
        self.ensure_no_pending(request)?;
        let mut record = if let Some(original) = self.original(request)? {
            original
        } else {
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if !registration.is_installed() {
                return Err(ServiceFailure::Missing);
            }
            let binding = RequestBinding::Start(RegistrationBasis::capture(&registration));
            RequestRecord {
                digest: binding.digest(self.key, request)?,
                binding,
                phase: ServicePhase::Reserved,
                state: ServiceRequestState::Pending,
                reserving: false,
                aborting: false,
            }
        };
        if record.phase == ServicePhase::Completed {
            return Ok(record.progress(request));
        }
        if record.phase == ServicePhase::Reserved {
            if record.reserving {
                self.abort_unaccepted_reservation(request, &record, &lease)
                    .await?;
                record.reserving = false;
                record.aborting = false;
                self.write_request(request, &record)?;
            }
            if resolving {
                return Ok(record.progress(request));
            }
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if !record.binding.basis().matches(&registration) {
                return Err(ServiceFailure::StaleAssessment);
            }
            self.verify_execution_selection(&record.binding, &registration)?;
            if let RequestBinding::Confirmed(original) = &record.binding {
                if assessment.as_ref() != Some(original) {
                    return Err(ServiceFailure::RequestConflict);
                }
                original.check_time(self.clock.now()?)?;
                record.reserving = true;
                self.write_request(request, &record)?;
            }
            let admission = async {
                if let RequestBinding::Confirmed(original) = &record.binding {
                    self.processes()?
                        .revalidate(&registration, &original.runtime)
                        .await?;
                    original.check_time(self.clock.now()?)?;
                }
                lease.verify()?;
                record.reserving = false;
                record.aborting = false;
                self.phase(request, &mut record, ServicePhase::Accepted)
            }
            .await;
            if let Err(error) = admission {
                // Publication uncertainty keeps the runtime barrier. A readable original
                // Reserved record proves non-admission; no old fsync result is inferred.
                if lease.verify().is_ok()
                    && let Ok(Some(mut original)) = self.original(request)
                    && original.phase == ServicePhase::Reserved
                    && original.reserving
                {
                    self.abort_unaccepted_reservation(request, &original, &lease)
                        .await?;
                    original.reserving = false;
                    original.aborting = false;
                    self.write_request(request, &original)?;
                }
                return Err(error);
            }
        }
        self.advance(request, &mut record, &lease).await
    }
    async fn abort_unaccepted_reservation(
        &self,
        request: NativeRequestId,
        record: &RequestRecord,
        lease: &Lease,
    ) -> Result<(), ServiceFailure> {
        lease.verify()?;
        let mut original = self
            .original(request)?
            .ok_or(ServiceFailure::InvalidRecord)?;
        if original != *record || original.phase != ServicePhase::Reserved || !original.reserving {
            return Err(ServiceFailure::Indeterminate);
        }
        let RequestBinding::Confirmed(assessment) = original.binding.clone() else {
            return Err(ServiceFailure::InvalidRecord);
        };
        original.aborting = true;
        self.write_request(request, &original)?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        self.processes()?
            .abort_reservation(&registration, &assessment.runtime)
            .await?;
        lease.verify().map_err(Into::into)
    }
    fn verify_execution_selection(
        &self,
        binding: &RequestBinding,
        registration: &RegisteredServer,
    ) -> Result<(), ServiceFailure> {
        match binding {
            RequestBinding::Confirmed(assessment) => match assessment.action {
                ServiceAction::Stop | ServiceAction::Uninstall => Ok(()),
                ServiceAction::Activate(id) => {
                    let target = assessment
                        .basis
                        .staged
                        .as_ref()
                        .filter(|status| status.installation == id)
                        .ok_or(ServiceFailure::InvalidRecord)?;
                    self.verify_staged_activation(registration, target)
                }
                ServiceAction::Restart => self.manager()?.verify_installation(registration),
            },
            RequestBinding::Start(_) => self.manager()?.verify_installation(registration),
        }
    }
    async fn advance(
        &self,
        request: NativeRequestId,
        record: &mut RequestRecord,
        lease: &Lease,
    ) -> Result<ServiceProgress, ServiceFailure> {
        if record.phase == ServicePhase::Accepted {
            if matches!(record.binding, RequestBinding::Start(_))
                && let Err(failure) = self.ensure_linger().await
            {
                record.phase = ServicePhase::Completed;
                record.state = ServiceRequestState::NotApplied(failure);
                self.write_request(request, record)?;
                return Ok(record.progress(request));
            }
            lease.verify()?;
            self.commit_intent(request, record)?;
            self.phase(request, record, ServicePhase::IntentCommitted)?;
        }
        if record.phase == ServicePhase::IntentCommitted {
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if record.binding.retirement().is_some() {
                // Deliberate Stop is already durable. Manager suppression cannot
                // race ahead of its intent or turn a failed commit into success.
                if registration.desired_state() == DesiredServerState::Stopped {
                    self.manager()?.configure(&registration).await?;
                }
                self.phase(request, record, ServicePhase::Quiescing)?;
            } else {
                self.phase(request, record, ServicePhase::Publishing)?;
            }
        }
        if record.phase == ServicePhase::Quiescing {
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            let RequestBinding::Confirmed(assessment) = &record.binding else {
                return Err(ServiceFailure::InvalidRecord);
            };
            let action = record
                .binding
                .retirement()
                .ok_or(ServiceFailure::InvalidRecord)?;
            let reference = NativeManagementRequestRef::new(
                ServerId::new(registration.server_pin().identity),
                *request.as_bytes(),
                record.digest,
            )
            .map_err(|_| ServiceFailure::InvalidRecord)?;
            lease.verify()?;
            let observations = self
                .processes()?
                .quiesce(&registration, reference, action, &assessment.runtime)
                .await?;
            cleanup_matches(&assessment.runtime.sessions, &observations)?;
            self.manager()?.stop(&registration).await?;
            self.processes()?.verify_absent(&registration).await?;
            if !matches!(
                self.manager()?.status(Some(&registration)).await?,
                SupervisorState::Inactive
                    | SupervisorState::Disabled
                    | SupervisorState::NotInstalled
            ) {
                return Err(ServiceFailure::Pending);
            }
            lease.verify()?;
            self.phase(request, record, ServicePhase::Publishing)?;
        }
        if record.phase == ServicePhase::Publishing {
            let mut registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if let RequestBinding::Confirmed(assessment) = &record.binding {
                match assessment.action {
                    ServiceAction::Activate(id) => {
                        let target = assessment
                            .basis
                            .staged
                            .as_ref()
                            .filter(|candidate| candidate.installation == id)
                            .ok_or(ServiceFailure::InvalidRecord)?;
                        self.activate_installation_selection(
                            &record.progress(request),
                            &registration,
                            lease,
                            target,
                            assessment.basis.approved.as_ref(),
                        )?;
                        if registration.record.approved.as_ref() != Some(target) {
                            if registration.record.staged.as_ref() != Some(target) {
                                return Err(ServiceFailure::RequestConflict);
                            }
                            let (directory, snapshot, mut next) =
                                self.read_registration()?.ok_or(ServiceFailure::Missing)?;
                            next.approved = Some(target.clone());
                            next.staged = None;
                            next.generation = next
                                .generation
                                .checked_add(1)
                                .ok_or(ServiceFailure::LimitExceeded)?;
                            let candidate = RegisteredServer {
                                record: next.clone(),
                                directory: registration.directory.clone(),
                            };
                            self.manager()?.verify_installation(&candidate)?;
                            lease.verify()?;
                            self.publish_registration(&next, &snapshot, &directory)?;
                            registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
                        }
                    }
                    ServiceAction::Uninstall => {
                        self.manager()?.uninstall(&registration).await?;
                        if registration.record.installed {
                            let (directory, snapshot, mut next) =
                                self.read_registration()?.ok_or(ServiceFailure::Missing)?;
                            next.installed = false;
                            next.desired = assessment.basis.desired;
                            next.approved = None;
                            next.staged = None;
                            next.generation = next
                                .generation
                                .checked_add(1)
                                .ok_or(ServiceFailure::LimitExceeded)?;
                            lease.verify()?;
                            self.publish_registration(&next, &snapshot, &directory)?;
                        }
                        return self.complete(request, record, ServiceOutcome::Uninstalled);
                    }
                    _ => {}
                }
            }
            if registration.desired_state() == DesiredServerState::Running {
                self.manager()?.verify_installation(&registration)?;
            }
            self.manager()?.configure(&registration).await?;
            lease.verify()?;
            if registration.desired_state() == DesiredServerState::Running {
                self.phase(request, record, ServicePhase::Starting)?;
            } else {
                return self.complete(request, record, record.binding.outcome()?);
            }
        }
        if record.phase == ServicePhase::Starting {
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if registration.desired_state() != DesiredServerState::Running {
                return Err(ServiceFailure::RequestConflict);
            }
            self.manager()?.verify_installation(&registration)?;
            lease.verify()?;
            self.manager()?.start(&registration).await?;
            if self.manager()?.status(Some(&registration)).await? != SupervisorState::Running {
                return Ok(record.progress(request));
            }
            lease.verify()?;
            return self.complete(request, record, record.binding.outcome()?);
        }
        Ok(record.progress(request))
    }
}

impl RequestBinding {
    fn outcome(&self) -> Result<ServiceOutcome, ServiceFailure> {
        Ok(match self {
            RequestBinding::Start(_) => ServiceOutcome::Started,
            RequestBinding::Confirmed(assessment) => match assessment.action {
                ServiceAction::Stop => ServiceOutcome::Stopped,
                ServiceAction::Restart => ServiceOutcome::Restarted,
                ServiceAction::Uninstall => ServiceOutcome::Uninstalled,
                ServiceAction::Activate(id) => ServiceOutcome::Activated(
                    assessment
                        .basis
                        .staged
                        .as_ref()
                        .filter(|candidate| candidate.installation == id)
                        .ok_or(ServiceFailure::InvalidRecord)?
                        .clone(),
                ),
            },
        })
    }
}

impl ServiceSupervisor {
    pub(super) async fn ensure_linger(&self) -> Result<(), ServiceFailure> {
        match self.manager()?.linger().await? {
            None if cfg!(target_os = "linux") => Err(ServiceFailure::SupervisorUnavailable),
            None | Some(LingerState::Enabled) => Ok(()),
            Some(LingerState::Unavailable) => Err(ServiceFailure::SupervisorUnavailable),
            Some(LingerState::Disabled) => {
                let authorization = self
                    .authorization
                    .as_ref()
                    .ok_or(ServiceFailure::AuthorizationUnavailable)?;
                authorization
                    .enable_linger(&LingerRequest {
                        uid: self.key.account_uid,
                    })
                    .await?;
                if self.manager()?.linger().await? != Some(LingerState::Enabled) {
                    return Err(ServiceFailure::SupervisorUnavailable);
                }
                Ok(())
            }
        }
    }
}

pub(super) mod pin_codec {
    use serde::{Deserialize, Serialize};
    use spaceterm_protocol::authentication::ServerPin;
    pub(crate) fn serialize<S: serde::Serializer>(
        pin: &ServerPin,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        (pin.identity, pin.public_key).serialize(serializer)
    }
    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ServerPin, D::Error> {
        let (identity, public_key) = <([u8; 16], [u8; 32])>::deserialize(deserializer)?;
        Ok(ServerPin {
            identity,
            public_key,
        })
    }
}

/// Admission honors deliberate Stop before keys, store, code or listeners are opened.
pub enum AccountStartupAdmission {
    /// Automatic or accidental startup must exit without opening identity storage.
    Stopped,
    /// Established storage is unavailable; retry never initializes replacements.
    WaitingStorage(StorageWait),
    /// Owns the exact registered identity and separate instance lease.
    Ready(AccountStartup),
}
/// Retained unavailable registration identity for capped, listener-free startup retry.
pub struct StorageWait {
    key: AccountBuildKey,
    anchor: PathBuf,
    failure: ServiceFailure,
}
impl StorageWait {
    /// Returns the typed original storage failure.
    pub fn failure(&self) -> ServiceFailure {
        self.failure
    }
    /// Retries only the same fixed account/build anchor without creating storage.
    pub fn retry(
        &self,
        supervisor: &ServiceSupervisor,
    ) -> Result<AccountStartupAdmission, ServiceFailure> {
        if supervisor.key != self.key || supervisor.anchor != self.anchor {
            return Err(ServiceFailure::IdentityMismatch);
        }
        supervisor.admit_startup()
    }
}
/// Startup owns one native instance independently of the short mutation lock.
pub struct AccountStartup(Box<StartupState>);
struct StartupState {
    registration: RegisteredServer,
    identity: Arc<crate::server_keys::ServerIdentity>,
    lease: Lease,
    supervisor: ServiceSupervisor,
}
impl AccountStartup {
    /// Returns retained first-approved storage and exact public registration.
    pub fn registration(&self) -> &RegisteredServer {
        &self.0.registration
    }
    /// Returns zeroizing signing ownership with no seed accessor.
    pub fn identity(&self) -> Arc<crate::server_keys::ServerIdentity> {
        Arc::clone(&self.0.identity)
    }
    /// Revalidates deliberate intent and approval before admitting listeners or runtime work.
    pub fn revalidate(&self) -> Result<(), ServiceFailure> {
        self.0.lease.verify()?;
        let current = self
            .0
            .supervisor
            .discover()?
            .ok_or(ServiceFailure::Missing)?;
        self.0.supervisor.check_startup_barrier(&current.record)?;
        if current.desired_state() != DesiredServerState::Running
            || current.server_pin() != self.0.registration.server_pin()
            || current.record.approved != self.0.registration.record.approved
            || !current.is_installed()
        {
            return Err(ServiceFailure::Pending);
        }
        Ok(())
    }
}
impl fmt::Debug for AccountStartup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountStartup(..)")
    }
}
impl fmt::Debug for AccountStartupAdmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stopped => "AccountStartupAdmission::Stopped",
            Self::WaitingStorage(_) => "AccountStartupAdmission::WaitingStorage(..)",
            Self::Ready(_) => "AccountStartupAdmission::Ready(..)",
        })
    }
}
impl ServiceSupervisor {
    /// Checks durable Stop before key or organization access and retains one instance lease.
    pub fn admit_startup(&self) -> Result<AccountStartupAdmission, ServiceFailure> {
        match self.try_startup() {
            Err(failure @ (ServiceFailure::StorageLocked | ServiceFailure::StorageUnavailable)) => {
                Ok(AccountStartupAdmission::WaitingStorage(StorageWait {
                    key: self.key,
                    anchor: self.anchor.clone(),
                    failure,
                }))
            }
            result => result,
        }
    }
    fn try_startup(&self) -> Result<AccountStartupAdmission, ServiceFailure> {
        self.settle_accepted_stop_intent()?;
        let (_, _, record) = self.read_registration()?.ok_or(ServiceFailure::Missing)?;
        if record.desired == DesiredServerState::Stopped {
            return Ok(AccountStartupAdmission::Stopped);
        }
        self.check_startup_barrier(&record)?;
        if !record.installed || record.approved.is_none() {
            return Err(ServiceFailure::Missing);
        }
        let directory = Directory::open(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        let lease = directory.lease("instance-lock")?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.desired_state() == DesiredServerState::Stopped {
            return Ok(AccountStartupAdmission::Stopped);
        }
        self.manager()?.verify_installation(&registration)?;
        Directory::open(registration.organization_directory())?
            .ok_or(ServiceFailure::Missing)?
            .verify()?;
        Directory::open(&registration.record.runtime)?
            .ok_or(ServiceFailure::Missing)?
            .verify()?;
        let identity = crate::server_keys::ServerKeyStore::open(&registration)?
            .load_identity(registration.server_pin())?;
        let startup = AccountStartup(Box::new(StartupState {
            registration,
            identity,
            lease,
            supervisor: self.clone(),
        }));
        startup.revalidate()?;
        Ok(AccountStartupAdmission::Ready(startup))
    }
    fn check_startup_barrier(&self, registration: &Registration) -> Result<(), ServiceFailure> {
        let journal = match self.read_journal() {
            Ok((_, _, journal)) => journal,
            Err(ServiceFailure::Missing | ServiceFailure::UnknownRequest) => return Ok(()),
            Err(error) => return Err(error),
        };
        for install in journal
            .reservations
            .iter()
            .filter_map(|r| r.install.as_ref())
            .filter(|record| record.pending())
        {
            if install.phase != ServicePhase::Starting
                || install.installation.as_ref() != registration.approved.as_ref()
                || !registration.installed
            {
                return Err(ServiceFailure::Pending);
            }
        }
        for record in journal
            .reservations
            .iter()
            .filter_map(|r| r.service.as_ref())
        {
            if !record.pending() {
                continue;
            }
            // Retirement phases authorize no replacement runtime. Only the original
            // Starting phase authorizes its exact successor selection, never old code.
            if record.phase != ServicePhase::Starting {
                return Err(ServiceFailure::Pending);
            }
            let mut expected = record.binding.basis().clone();
            match &record.binding {
                RequestBinding::Start(_) => {
                    if expected.desired != DesiredServerState::Running {
                        expected.desired = DesiredServerState::Running;
                        expected.generation = expected
                            .generation
                            .checked_add(1)
                            .ok_or(ServiceFailure::LimitExceeded)?;
                    }
                }
                RequestBinding::Confirmed(assessment) => match assessment.action {
                    ServiceAction::Restart => {}
                    ServiceAction::Activate(_) => {
                        expected.approved = expected.staged.take();
                        expected.generation = expected
                            .generation
                            .checked_add(1)
                            .ok_or(ServiceFailure::LimitExceeded)?;
                    }
                    ServiceAction::Stop | ServiceAction::Uninstall => {
                        return Err(ServiceFailure::Pending);
                    }
                },
            }
            if expected.desired != DesiredServerState::Running
                || expected != RegistrationBasis::capture_record(registration)
            {
                return Err(ServiceFailure::Pending);
            }
        }
        Ok(())
    }
    fn settle_accepted_stop_intent(&self) -> Result<(), ServiceFailure> {
        let journal = match self.read_journal() {
            Ok((_, _, journal)) => journal,
            Err(ServiceFailure::Missing | ServiceFailure::UnknownRequest) => return Ok(()),
            Err(error) => return Err(error),
        };
        let Some(reservation) = journal.reservations.iter().find(|reservation| {
            reservation.service.as_ref().is_some_and(|record| {
                record.phase == ServicePhase::Accepted && matches!(&record.binding,
                    RequestBinding::Confirmed(assessment) if matches!(assessment.action, ServiceAction::Stop | ServiceAction::Uninstall))
            })
        }) else { return Ok(()); };
        let request = journal.request(reservation.sequence)?;
        let lease = self.lifecycle_lease()?;
        let mut original = self
            .original(request)?
            .ok_or(ServiceFailure::InvalidRecord)?;
        if original.phase == ServicePhase::Accepted {
            // Startup completes only the already durable original Stop intent.
            // It neither invents an assessment nor runs manager or process effects.
            lease.verify()?;
            self.commit_intent(request, &original)?;
            self.phase(request, &mut original, ServicePhase::IntentCommitted)?;
        }
        Ok(())
    }
    /// Verifies provider-owned durable non-admission before a paired local bridge
    /// may release an original runtime reservation. Comparison fields grant none.
    pub fn verify_runtime_abort(
        &self,
        assessment: &RuntimeAssessment,
    ) -> Result<(), ServiceFailure> {
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        assessment.validate(&registration)?;
        let (_, _, journal) = self.read_journal()?;
        if journal.reservations.iter().any(|reservation| reservation.service.as_ref().is_some_and(|record| {
            record.phase == ServicePhase::Reserved && record.reserving && record.aborting
                && matches!(&record.binding, RequestBinding::Confirmed(original) if original.runtime == *assessment)
        })) { Ok(()) } else { Err(ServiceFailure::RequestConflict) }
    }
    /// Reads the original all-client scope for an already authorized retirement.
    pub fn retirement_runtime_assessment(
        &self,
        server: ServerId,
        epoch: ServerEpoch,
    ) -> Result<RuntimeAssessment, ServiceFailure> {
        let (request, _) = self.verify_retirement_request(server, epoch)?;
        let original = self
            .original(
                NativeRequestId::new(request.id_bytes())
                    .map_err(|_| ServiceFailure::InvalidRecord)?,
            )?
            .ok_or(ServiceFailure::UnknownRequest)?;
        let RequestBinding::Confirmed(assessment) = original.binding else {
            return Err(ServiceFailure::RequestConflict);
        };
        Ok(assessment.runtime)
    }
    /// Validates action-specific durable intent and the exact original runtime epoch.
    /// No caller-supplied comparison value or wire field can issue this authority.
    pub fn verify_retirement_request(
        &self,
        server: ServerId,
        epoch: ServerEpoch,
    ) -> Result<(NativeManagementRequestRef, NativeRetirementAction), ServiceFailure> {
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.server_pin().identity != server.bytes() || epoch.get() == 0 {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let (_, _, journal) = self.read_journal()?;
        let reservation = journal
            .reservations
            .iter()
            .find(|reservation| {
                reservation
                    .service
                    .as_ref()
                    .is_some_and(RequestRecord::pending)
            })
            .ok_or(ServiceFailure::UnknownRequest)?;
        let record = reservation
            .service
            .as_ref()
            .ok_or(ServiceFailure::InvalidRecord)?;
        if !matches!(
            record.phase,
            ServicePhase::IntentCommitted | ServicePhase::Quiescing
        ) {
            return Err(ServiceFailure::Pending);
        }
        let RequestBinding::Confirmed(assessment) = &record.binding else {
            return Err(ServiceFailure::RequestConflict);
        };
        if assessment.runtime.epoch != Some(epoch) || assessment.runtime.server != server {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let mut expected = assessment.basis.clone();
        match assessment.action {
            ServiceAction::Stop | ServiceAction::Uninstall => {
                if expected.desired != DesiredServerState::Stopped {
                    expected.desired = DesiredServerState::Stopped;
                    expected.generation = expected
                        .generation
                        .checked_add(1)
                        .ok_or(ServiceFailure::LimitExceeded)?;
                }
            }
            ServiceAction::Restart if expected.desired != DesiredServerState::Running => {
                return Err(ServiceFailure::RequestConflict);
            }
            ServiceAction::Activate(id)
                if expected
                    .staged
                    .as_ref()
                    .is_none_or(|s| s.installation != id) =>
            {
                return Err(ServiceFailure::RequestConflict);
            }
            _ => {}
        }
        if !expected.matches(&registration) {
            return Err(ServiceFailure::RequestConflict);
        }
        let request = journal.request(reservation.sequence)?;
        let reference = NativeManagementRequestRef::new(server, *request.as_bytes(), record.digest)
            .map_err(|_| ServiceFailure::InvalidRecord)?;
        Ok((
            reference,
            record
                .binding
                .retirement()
                .ok_or(ServiceFailure::RequestConflict)?,
        ))
    }
    /// Reconciles only an already pending original ticket removal after provider
    /// reconstruction. Ordinary discovery and key reads remain closed until then.
    pub fn resolve_pairing_secret_removal(
        &self,
        ticket: spaceterm_core::pairing::EnrollmentTicketId,
    ) -> Result<(), ServiceFailure> {
        let (_, _, registration) = self.read_registration()?.ok_or(ServiceFailure::Missing)?;
        crate::server_keys::reconcile_registered_removal(
            self.key,
            &registration.data,
            spaceterm_protocol::authentication::ServerPin {
                identity: registration.server,
                public_key: registration.public_key,
            },
            ticket,
        )
        .map_err(Into::into)
    }
    /// Recovers only the original identity publication selected by retained registration.
    pub fn resolve_identity_publication(&self) -> Result<(), ServiceFailure> {
        let (_, _, registration) = self.read_registration()?.ok_or(ServiceFailure::Missing)?;
        crate::server_keys::reconcile_registered_identity_publication(
            self.key,
            &registration.data,
            spaceterm_protocol::authentication::ServerPin {
                identity: registration.server,
                public_key: registration.public_key,
            },
        )
        .map_err(Into::into)
    }
    /// Recovers the original ticket publication without issuing replacement bearer material.
    pub fn resolve_pairing_secret_publication(
        &self,
        ticket: spaceterm_core::pairing::EnrollmentTicketId,
    ) -> Result<(), ServiceFailure> {
        let (_, _, registration) = self.read_registration()?.ok_or(ServiceFailure::Missing)?;
        crate::server_keys::reconcile_registered_secret_publication(
            self.key,
            &registration.data,
            spaceterm_protocol::authentication::ServerPin {
                identity: registration.server,
                public_key: registration.public_key,
            },
            ticket,
        )
        .map_err(Into::into)
    }
    /// Reconciles the validated store's exact previous-epoch retirement scope.
    pub fn reconcile_previous_instance<'a>(
        &'a self,
        server: ServerId,
        epoch: ServerEpoch,
        expected: &'a [spaceterm_core::retirement::RetiredSession],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if registration.server_pin().identity != server.bytes() {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let sessions: Vec<_> = expected.iter().map(|value| value.session()).collect();
            super::processes::validate_previous_scope(server, epoch, &sessions)?;
            let observations = self
                .processes()?
                .reconcile_previous_instance(&registration, epoch, expected)
                .await?;
            cleanup_matches(&sessions, &observations)?;
            if !expected
                .iter()
                .zip(&observations)
                .all(|(expected, observed)| expected.first_reason() == observed.first_reason())
            {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            Ok(observations)
        })
    }

    /// Requires trusted exact native process-scope evidence even for an empty old Session list.
    pub fn previous_instance_cleanup<'a>(
        &'a self,
        server: ServerId,
        epoch: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if registration.server_pin().identity != server.bytes() {
                return Err(ServiceFailure::IdentityMismatch);
            }
            super::processes::validate_previous_scope(server, epoch, sessions)?;
            let observations = self
                .processes()?
                .previous_instance_cleanup(&registration, epoch, sessions)
                .await?;
            cleanup_matches(sessions, &observations)?;
            Ok(observations)
        })
    }

    // The artifact install owner holds this account mutation lease and durable
    // initial-install request before approving roots or issuing identity creation.
    // Existing registration is reuse, including Stopped, and never initialization.
    pub(crate) fn initialize_registration(
        &self,
        lease: &Lease,
        data: PathBuf,
        runtime: PathBuf,
        desired: DesiredServerState,
        approved: Option<super::InstallationStatus>,
    ) -> Result<RegisteredServer, ServiceFailure> {
        lease.verify()?;
        if let Some(registration) = self.discover()? {
            return Ok(registration);
        }
        validate_path(&data)?;
        validate_path(&runtime)?;
        let anchor = Directory::open(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        Directory::reconcile_initialization(&data)?;
        let directory = match Directory::create(&data) {
            Ok(directory) => directory,
            Err(Failure::Indeterminate) => {
                // Only the serialized initial-install owner may recover unpublished
                // identity material. Startup and ordinary reads never take this path.
                crate::server_keys::reconcile_initial_identity_publication(&data, self.key)?;
                Directory::create(&data)?
            }
            Err(error) => return Err(error.into()),
        };
        Directory::create(&runtime)?;
        Directory::create(&data.join("organization"))?;
        let (store, permit) = crate::server_keys::ServerKeyStore::initial(directory, self.key);
        let identity = match store.load_initial_identity() {
            Ok(identity) => identity,
            Err(crate::server_keys::KeyStoreFailure::Missing) => {
                match store.create_identity(permit) {
                    Ok(identity) => identity,
                    Err(crate::server_keys::KeyStoreFailure::AlreadyExists) => {
                        store.load_initial_identity()?
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        let pin = identity.pin();
        let registration = Registration {
            schema: 1,
            key: self.key,
            generation: 1,
            desired,
            server: pin.identity,
            public_key: pin.public_key,
            organization: data.join("organization"),
            data,
            runtime,
            approved,
            staged: None,
            installed: true,
        };
        registration.validate(self.key)?;
        super::runtime_inventory::initialize(
            &RegisteredServer {
                directory: Directory::open(&registration.data)?.ok_or(ServiceFailure::Missing)?,
                record: registration.clone(),
            },
            identity.as_ref(),
        )?;
        lease.verify()?;
        match anchor.publish(
            "registration",
            &encode(&registration, REGISTRATION_LIMIT)?,
            None,
        ) {
            Ok(()) | Err(Failure::Conflict) => {
                self.discover()?.ok_or(ServiceFailure::Indeterminate)
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// Exclusive, listener-free execution of one retained owner enrollment while
/// deliberate Stop remains in force. The instance lease also excludes normal startup.
pub struct StoppedOwnerAdmission {
    registration: RegisteredServer,
    lease: Lease,
    supervisor: ServiceSupervisor,
    request: NativeRequestId,
    digest: RequestDigest,
    claim: RequestDigest,
}
impl StoppedOwnerAdmission {
    /// Returns the original deliberate request, not a new allocation.
    pub fn request(&self) -> NativeRequestId {
        self.request
    }
    /// Returns the exact stopped registration admitted under the instance lease.
    pub fn registration(&self) -> &RegisteredServer {
        &self.registration
    }
    /// Retains cleanup custody after an explicit Start changes desired intent.
    /// This verifies only the existing instance and original owner record. It
    /// cannot authorize another owner mutation, Stop, Start, or a code selection.
    pub fn verify_instance_custody(&self) -> Result<(), ServiceFailure> {
        self.lease.verify()?;
        let current = self.supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
        if current.key() != self.registration.key()
            || current.server_pin() != self.registration.server_pin()
            || current.record.data != self.registration.record.data
            || current.record.organization != self.registration.record.organization
            || current.record.runtime != self.registration.record.runtime
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        self.supervisor.validate_owner_enrollment(
            &self.request,
            self.digest,
            spaceterm_core::identity::ServerId::new(self.registration.server_pin().identity),
            self.claim,
        )
    }

    /// Rechecks the original journal and exact registration, never Start.
    pub fn revalidate(&self) -> Result<(), ServiceFailure> {
        self.lease.verify()?;
        let current = self.supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
        self.supervisor.check_startup_barrier(&current.record)?;
        if current.desired_state() != DesiredServerState::Stopped
            || current.key() != self.registration.key()
            || current.record.data != self.registration.record.data
            || current.record.organization != self.registration.record.organization
            || current.record.runtime != self.registration.record.runtime
            || RegistrationBasis::capture_record(&current.record)
                != RegistrationBasis::capture_record(&self.registration.record)
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        self.supervisor.validate_owner_enrollment(
            &self.request,
            self.digest,
            spaceterm_core::identity::ServerId::new(self.registration.server_pin().identity),
            self.claim,
        )
    }
}
impl fmt::Debug for StoppedOwnerAdmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoppedOwnerAdmission(..)")
    }
}
impl ServiceSupervisor {
    /// The deliberate request must have been retained before any key/store access.
    /// An existing actor must receive Running requests instead of opening this path.
    pub fn admit_stopped_owner(
        &self,
        request: NativeRequestId,
        digest: RequestDigest,
        server: spaceterm_core::identity::ServerId,
        claim: RequestDigest,
    ) -> Result<StoppedOwnerAdmission, ServiceFailure> {
        let lifecycle = self.lifecycle_lease()?;
        self.validate_owner_enrollment(&request, digest, server, claim)?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.desired_state() != DesiredServerState::Stopped {
            return Err(ServiceFailure::Pending);
        }
        self.check_startup_barrier(&registration.record)?;
        let directory = Directory::open(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        let lease = directory.lease("instance-lock")?;
        lifecycle.verify()?;
        let admission = StoppedOwnerAdmission {
            registration,
            lease,
            supervisor: self.clone(),
            request,
            digest,
            claim,
        };
        admission.revalidate()?;
        Ok(admission)
    }
}

/// Account/build storage admission held for one deliberate provisioning action.
/// Consumer names are fixed; caller or remote paths grant no local authority.
pub struct ClientProvisioning {
    path: PathBuf,
    directory: Directory,
    lease: crate::private_filesystem::Lease,
}
impl ClientProvisioning {
    /// Opens or explicitly provisions one fixed dedicated consumer directory.
    /// The boolean distinguishes first creation from read-only reopening.
    pub fn journal_directory(&self, consumer: &str) -> Result<(PathBuf, bool), ServiceFailure> {
        if !matches!(consumer, "enrollment" | "cli" | "companion" | "management") {
            return Err(ServiceFailure::InvalidRecord);
        }
        self.directory.verify()?;
        self.lease.verify()?;
        let path = self.path.join(consumer);
        let create = Directory::open(&path)?.is_none();
        if create {
            Directory::create(&path)?;
        }
        Ok((path, create))
    }
}
