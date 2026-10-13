use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spaceterm_core::operations::RequestDigest;
use std::path::PathBuf;

use super::{
    InstallationStatus, RegisteredServer, ServiceFailure, ServiceFuture, ServiceOutcome,
    ServicePhase, ServiceProgress, ServiceRequestState, ServiceSupervisor, SupervisorState,
};
use crate::{
    native_deployment::contracts::NativeRequestId,
    private_filesystem::Lease,
    server_installation::{
        InstallationAdmission, InstallationOperation, InstallationScope, ServerInstallationStore,
        VerifiedServerArtifact,
    },
    startup::{AccountBuildKey, DesiredServerState},
};

/// Locally prepared initial-install confirmation. Preparation authenticates the
/// artifact but never initializes identity, publishes configuration or starts code.
pub struct ServiceInstallAssessment {
    artifact: VerifiedServerArtifact,
    roots: InstallRoots,
    desktop_seed: Option<DesktopSeed>,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRoots {
    data: PathBuf,
    runtime: PathBuf,
}
impl InstallRoots {
    fn validate(&self) -> Result<(), ServiceFailure> {
        super::registration::validate_path(&self.data)?;
        super::registration::validate_path(&self.runtime)?;
        if self.data == self.runtime {
            return Err(ServiceFailure::InvalidRecord);
        }
        Ok(())
    }
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopSeed {
    legacy_worktree_location: Option<String>,
}
impl DesktopSeed {
    fn origin(
        &self,
    ) -> Result<crate::organization_store::bootstrap::NewServerOrigin, ServiceFailure> {
        use crate::organization_store::bootstrap::{LegacyWorktreeLocation, NewServerOrigin};
        Ok(NewServerOrigin::LocalDesktop {
            legacy_worktree_location: self
                .legacy_worktree_location
                .as_ref()
                .map(|template| LegacyWorktreeLocation::parse(template.clone()))
                .transpose()
                .map_err(|_| ServiceFailure::InvalidRecord)?,
        })
    }
}
impl ServiceInstallAssessment {
    /// Captures the legacy client preference only for explicitly confirmed local first creation.
    /// Established organization is always opened unchanged, including a headless installation.
    pub fn with_local_desktop_seed(
        mut self,
        legacy: Option<crate::organization_store::bootstrap::LegacyWorktreeLocation>,
    ) -> Self {
        self.desktop_seed = Some(DesktopSeed {
            legacy_worktree_location: legacy.map(|value| value.template().as_str().to_owned()),
        });
        self
    }
    /// Selects explicit local first-install roots. Confirmation covers these exact paths.
    /// Existing registration always keeps its previously approved roots.
    pub fn with_roots(mut self, data: PathBuf, runtime: PathBuf) -> Result<Self, ServiceFailure> {
        let roots = InstallRoots { data, runtime };
        roots.validate()?;
        self.roots = roots;
        Ok(self)
    }
    /// Explicitly confirms installation of the exact authenticated artifact.
    pub fn confirm(self) -> ConfirmedServiceInstall {
        ConfirmedServiceInstall {
            artifact: self.artifact,
            roots: self.roots,
            desktop_seed: self.desktop_seed,
        }
    }
}
/// Noncloneable local confirmation, separate from artifact verification and request allocation.
pub struct ConfirmedServiceInstall {
    artifact: VerifiedServerArtifact,
    roots: InstallRoots,
    desktop_seed: Option<DesktopSeed>,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallRecord {
    artifact_sha256: [u8; 32],
    version: spaceterm_distribution::BuildVersion,
    digest: RequestDigest,
    pub(super) phase: ServicePhase,
    state: ServiceRequestState,
    reused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    roots: Option<InstallRoots>,
    pub(super) installation: Option<InstallationStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desktop_seed: Option<DesktopSeed>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    new_organization: bool,
}
impl InstallRecord {
    pub(super) fn pending(&self) -> bool {
        self.phase != ServicePhase::Completed
    }
    pub(super) fn validate(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        if let Some(roots) = &self.roots {
            roots.validate()?;
        }
        if let Some(seed) = &self.desktop_seed {
            seed.origin()?;
        }
        if self.artifact_sha256 == [0; 32]
            || self.digest
                != install_digest(
                    key,
                    request,
                    self.artifact_sha256,
                    &self.version,
                    self.roots.as_ref(),
                    self.desktop_seed.as_ref(),
                    self.new_organization,
                )?
            || !matches!(
                self.phase,
                ServicePhase::Accepted
                    | ServicePhase::IntentCommitted
                    | ServicePhase::Publishing
                    | ServicePhase::Starting
                    | ServicePhase::Completed
            )
            || (self.phase == ServicePhase::Completed)
                == matches!(
                    self.state,
                    ServiceRequestState::Pending | ServiceRequestState::Indeterminate
                )
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        if self.installation.as_ref().is_some_and(|status| {
            status.artifact_sha256 == [0; 32]
                || (!self.reused
                    && (status.artifact_sha256 != self.artifact_sha256
                        || status.version != self.version))
        }) || (self.phase == ServicePhase::Starting && self.installation.is_none())
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        if let ServiceRequestState::Completed(ServiceOutcome::Installed {
            installation,
            reused,
            ..
        }) = &self.state
        {
            if self.installation.as_ref() != Some(installation) || *reused != self.reused {
                return Err(ServiceFailure::InvalidRecord);
            }
        } else if matches!(self.state, ServiceRequestState::Completed(_)) {
            return Err(ServiceFailure::InvalidRecord);
        }
        Ok(())
    }
    fn progress(&self, request: NativeRequestId) -> ServiceProgress {
        ServiceProgress {
            request,
            request_digest: self.digest,
            phase: self.phase,
            state: self.state.clone(),
        }
    }
}
fn install_digest(
    key: AccountBuildKey,
    request: NativeRequestId,
    digest: [u8; 32],
    version: &spaceterm_distribution::BuildVersion,
    roots: Option<&InstallRoots>,
    desktop_seed: Option<&DesktopSeed>,
    new_organization: bool,
) -> Result<RequestDigest, ServiceFailure> {
    let mut hash = Sha256::new();
    hash.update(b"spaceterm-account-install-v1\0");
    hash.update(serde_json::to_vec(&key).map_err(|_| ServiceFailure::InvalidRecord)?);
    hash.update(request.as_bytes());
    hash.update(digest);
    hash.update(serde_json::to_vec(version).map_err(|_| ServiceFailure::InvalidRecord)?);
    if let Some(roots) = roots {
        hash.update(b"first-approved-roots-v1\0");
        hash.update(serde_json::to_vec(roots).map_err(|_| ServiceFailure::InvalidRecord)?);
    }
    if let Some(seed) = desktop_seed {
        hash.update(b"first-desktop-settings-v1\0");
        hash.update(serde_json::to_vec(seed).map_err(|_| ServiceFailure::InvalidRecord)?);
    }
    if new_organization {
        hash.update(b"new-organization-v1\0");
    }
    Ok(RequestDigest::from_bytes(hash.finalize().into()))
}
impl ServiceSupervisor {
    /// Authenticates an account server artifact against the compiled production trust root.
    /// Explicit confirmation and a separately retained NativeRequestId are still required.
    pub fn assess_install(
        &self,
        artifact: VerifiedServerArtifact,
    ) -> Result<ServiceInstallAssessment, ServiceFailure> {
        let artifact = crate::server_installation::verify_server_artifact(artifact)?;
        let account = super::TrustedAccountFacts::current()?;
        let mut environment = spaceterm_directories::AppDirectoryEnvironment::capture();
        environment.home = Some(account.home().as_os_str().to_owned());
        let name = match self.key.build_identity {
            crate::startup::BuildIdentity::Release => "spaceterm",
            crate::startup::BuildIdentity::Preflight => "spaceterm-preflight",
            crate::startup::BuildIdentity::Development => "spaceterm-development",
        };
        let directories =
            spaceterm_directories::AppDirectories::resolve_xdg(name, &environment, None)
                .map_err(|_| ServiceFailure::InvalidRecord)?;
        let roots = InstallRoots {
            data: directories.data.join("server"),
            // Native endpoint registration is independent of ephemeral login XDG_RUNTIME_DIR.
            // Keep the same account-owned route across logout, reboot and client environments.
            runtime: self.anchor.join("runtime"),
        };
        roots.validate()?;
        Ok(ServiceInstallAssessment {
            artifact,
            roots,
            desktop_seed: None,
        })
    }
    /// Installs the original confirmed artifact, or reuses an established service without starting it.
    /// The retained request owns effects even if its observer is dropped.
    pub fn install(
        &self,
        request: NativeRequestId,
        confirmation: ConfirmedServiceInstall,
    ) -> ServiceFuture<'_, ServiceProgress> {
        let supervisor = self.clone();
        Box::pin(async move {
            let handle = tokio::runtime::Handle::try_current()
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            handle
                .spawn(async move { supervisor.run_install(request, Some(confirmation)).await })
                .await
                .map_err(|_| ServiceFailure::Indeterminate)?
        })
    }
    /// Resumes an already accepted install. If archive staging never completed,
    /// resubmit the same verified artifact with install; no replacement ID is allocated.
    pub fn resolve_install(&self, request: NativeRequestId) -> ServiceFuture<'_, ServiceProgress> {
        let supervisor = self.clone();
        Box::pin(async move {
            let handle = tokio::runtime::Handle::try_current()
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            handle
                .spawn(async move { supervisor.run_install(request, None).await })
                .await
                .map_err(|_| ServiceFailure::Indeterminate)?
        })
    }
    pub(super) fn original_install(
        &self,
        request: NativeRequestId,
    ) -> Result<Option<InstallRecord>, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        let index = journal.reservation(request)?;
        if journal.reservations[index].owner.is_some()
            || journal.reservations[index].service.is_some()
        {
            return Err(ServiceFailure::RequestConflict);
        }
        Ok(journal.reservations[index].install.clone())
    }
    fn write_install(
        &self,
        request: NativeRequestId,
        record: &InstallRecord,
    ) -> Result<(), ServiceFailure> {
        self.mutate_request(request, |journal, index| {
            let slot = &mut journal.reservations[index];
            if slot.owner.is_some() || slot.service.is_some() {
                return Err(ServiceFailure::RequestConflict);
            }
            if let Some(original) = &slot.install
                && (original.digest != record.digest
                    || (original.phase == ServicePhase::Completed && original != record))
            {
                return Err(ServiceFailure::RequestConflict);
            }
            slot.install = Some(record.clone());
            Ok(())
        })?;
        #[cfg(feature = "test-support")]
        super::test_support::after_phase(record.phase)?;
        Ok(())
    }
    fn install_phase(
        &self,
        request: NativeRequestId,
        record: &mut InstallRecord,
        phase: ServicePhase,
    ) -> Result<(), ServiceFailure> {
        record.phase = phase;
        self.write_install(request, record)
    }
    fn initialize_organization(
        &self,
        registration: &RegisteredServer,
        record: &InstallRecord,
    ) -> Result<(), ServiceFailure> {
        use crate::organization_store::{
            SqliteOrganizationStore, StoreIdentity, StoreLimits, StoreServices, bootstrap::*,
            migrations, receipts::SignedReceiptReader, schema,
        };
        let account = super::TrustedAccountFacts::current()?;
        if account.uid() != self.key.account_uid {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let server = spaceterm_core::identity::ServerId::new(registration.server_pin().identity);
        let identity = StoreIdentity {
            server,
            account: self.key,
            server_public_key: registration.server_pin().public_key,
        };
        let home = spaceterm_core::organization::ServerDirectory::new(
            server,
            account.home().as_os_str().as_encoded_bytes().to_vec(),
        )
        .map_err(|_| ServiceFailure::InvalidRecord)?;
        let services = StoreServices::production(
            home,
            std::sync::Arc::new(
                SignedReceiptReader::new(registration.server_pin()).map_err(classify_store)?,
            ),
        );
        let database = if record.new_organization {
            crate::storage::Database::open_for_application(
                registration.organization_directory(),
                migrations(),
                schema::APPLICATION_ID,
            )
        } else {
            crate::storage::Database::open_existing_for_application(
                registration.organization_directory(),
                migrations(),
                schema::APPLICATION_ID,
            )
        }
        .map_err(classify_storage)?;
        if database.is_new_storage().map_err(classify_storage)? {
            if !record.new_organization {
                return Err(ServiceFailure::ConfirmationRequired);
            }
            let origin = record
                .desktop_seed
                .as_ref()
                .map(DesktopSeed::origin)
                .transpose()?
                .unwrap_or(NewServerOrigin::LocalHeadless);
            match SqliteOrganizationStore::initialize_new(
                database,
                NewStoreSeed { identity, origin },
                StoreLimits::production(),
                services,
            ) {
                InitializationDisposition::Durable { store, .. } => {
                    drop(store);
                    Ok(())
                }
                InitializationDisposition::NotApplied(failure) => Err(classify_store(failure)),
                InitializationDisposition::Indeterminate => Err(ServiceFailure::Indeterminate),
            }
        } else {
            // An existing empty, damaged or locked database never grants first-creation authority.
            SqliteOrganizationStore::open_existing(
                database,
                identity,
                StoreLimits::production(),
                services,
            )
            .map(drop)
            .map_err(classify_store)
        }
    }

    fn install_store(
        &self,
        scope: &InstallationScope<'_>,
    ) -> Result<ServerInstallationStore, ServiceFailure> {
        #[cfg(feature = "test-support")]
        if let Some(key) = &self.test_installation_trust {
            return crate::server_installation::test_support::store_with_scope(scope, key.clone());
        }
        ServerInstallationStore::open(scope)
    }
    pub(super) async fn run_install(
        &self,
        request: NativeRequestId,
        confirmation: Option<ConfirmedServiceInstall>,
    ) -> Result<ServiceProgress, ServiceFailure> {
        let artifact = confirmation.as_ref().map(|value| &value.artifact);
        let proposed_roots = confirmation.as_ref().map(|value| &value.roots);
        let proposed_seed = confirmation
            .as_ref()
            .and_then(|value| value.desktop_seed.as_ref());
        self.reconcile_storage(request)?;
        if let Some(original) = self.original_install(request)?
            && original.phase == ServicePhase::Completed
        {
            if let Some(artifact) = artifact
                && (original.artifact_sha256 != artifact.artifact_sha256()
                    || original.version != artifact.manifest().version
                    || original.desktop_seed.as_ref() != proposed_seed
                    || original
                        .roots
                        .as_ref()
                        .is_some_and(|roots| Some(roots) != proposed_roots))
            {
                return Err(ServiceFailure::RequestConflict);
            }
            return Ok(original.progress(request));
        }
        let lease = self.lifecycle_lease()?;
        self.ensure_no_pending(request)?;
        let mut record = match self.original_install(request)? {
            Some(record) => {
                if let Some(artifact) = artifact
                    && (record.artifact_sha256 != artifact.artifact_sha256()
                        || record.version != artifact.manifest().version
                        || record.desktop_seed.as_ref() != proposed_seed
                        || record
                            .roots
                            .as_ref()
                            .is_some_and(|roots| Some(roots) != proposed_roots))
                {
                    return Err(ServiceFailure::RequestConflict);
                }
                record
            }
            None => {
                let artifact = artifact.ok_or(ServiceFailure::UnknownRequest)?;
                let new_organization = self.discover()?.is_none();
                let record = InstallRecord {
                    desktop_seed: proposed_seed.cloned(),
                    new_organization,
                    artifact_sha256: artifact.artifact_sha256(),
                    version: artifact.manifest().version.clone(),
                    digest: install_digest(
                        self.key,
                        request,
                        artifact.artifact_sha256(),
                        &artifact.manifest().version,
                        proposed_roots,
                        proposed_seed,
                        new_organization,
                    )?,
                    phase: ServicePhase::Accepted,
                    state: ServiceRequestState::Pending,
                    reused: self
                        .discover()?
                        .is_some_and(|registered| registered.is_installed()),
                    installation: None,
                    roots: proposed_roots.cloned(),
                };
                self.write_install(request, &record)?;
                record
            }
        };
        if record.phase == ServicePhase::Completed {
            return Ok(record.progress(request));
        }
        if record.reused {
            let registered = self.discover()?.ok_or(ServiceFailure::Missing)?;
            record.installation = Some(
                registered
                    .approved_installation()
                    .ok_or(ServiceFailure::Missing)?
                    .clone(),
            );
            return self.finish_install(request, record, &registered);
        }
        if record.phase == ServicePhase::Accepted {
            let running = self
                .discover()?
                .is_none_or(|registered| registered.desired_state() == DesiredServerState::Running);
            if running && let Err(failure) = self.ensure_linger().await {
                record.phase = ServicePhase::Completed;
                record.state = ServiceRequestState::NotApplied(failure);
                self.write_install(request, &record)?;
                return Ok(record.progress(request));
            }
            let registered = self.initialize_registration(
                &lease,
                record
                    .roots
                    .as_ref()
                    .map_or_else(|| self.anchor.join("data"), |roots| roots.data.clone()),
                record.roots.as_ref().map_or_else(
                    || self.anchor.join("runtime"),
                    |roots| roots.runtime.clone(),
                ),
                DesiredServerState::Running,
                None,
            )?;
            if registered.approved_installation().is_some() {
                return Err(ServiceFailure::RequestConflict);
            }
            self.initialize_organization(&registered, &record)?;
            self.install_phase(request, &mut record, ServicePhase::IntentCommitted)?;
        }
        if record.phase == ServicePhase::IntentCommitted || record.phase == ServicePhase::Publishing
        {
            let registered = self.discover()?.ok_or(ServiceFailure::Missing)?;
            self.initialize_organization(&registered, &record)?;
            let admission = InstallAdmission {
                supervisor: self,
                request,
                lease: &lease,
                digest: record.digest,
            };
            let scope = InstallationScope::new(&registered, &admission)?;
            let store = self.install_store(&scope)?;
            // A lost archive-copy response retains this request's exact native
            // candidate. Reuse its inventory cleanup before accepting the same
            // authenticated archive again; never adopt or scan another tree.
            if let Some(cleanup) = store.cleanup_status()? {
                if cleanup.artifact_sha256 != record.artifact_sha256 {
                    return Err(ServiceFailure::RequestConflict);
                }
                record.installation = Some(InstallationStatus {
                    installation: cleanup.installation,
                    version: record.version.clone(),
                    artifact_sha256: cleanup.artifact_sha256,
                });
                self.write_install(request, &record)?;
                store.cleanup(&scope)?;
                record.installation = None;
                self.write_install(request, &record)?;
            }
            if let Some(preparing) = store.preparing_status()? {
                if preparing.artifact_sha256 != record.artifact_sha256 {
                    return Err(ServiceFailure::RequestConflict);
                }
                record.installation = Some(InstallationStatus {
                    installation: preparing.installation,
                    version: record.version.clone(),
                    artifact_sha256: preparing.artifact_sha256,
                });
                self.write_install(request, &record)?;
                store.discard_preparing(
                    &scope,
                    preparing.installation,
                    store.snapshot()?.generation,
                )?;
                store.cleanup(&scope)?;
                record.installation = None;
                self.write_install(request, &record)?;
            }
            let snapshot = store.snapshot()?;
            let status = if let Some(approved) = snapshot.approved {
                let status = store.retain_approved()?.status();
                if status.installation != approved
                    || status.artifact_sha256 != record.artifact_sha256
                    || status.version != record.version
                {
                    return Err(ServiceFailure::RequestConflict);
                }
                InstallationStatus {
                    installation: status.installation,
                    version: status.version,
                    artifact_sha256: status.artifact_sha256,
                }
            } else {
                let status = if snapshot.staged.is_some() {
                    store.staged_status()?.ok_or(ServiceFailure::Missing)?
                } else {
                    let Some(artifact) = artifact else {
                        return Ok(record.progress(request));
                    };
                    let id = store.stage(&scope, artifact)?;
                    crate::server_installation::InstallationStatus {
                        installation: id,
                        version: record.version.clone(),
                        artifact_sha256: record.artifact_sha256,
                    }
                };
                if status.artifact_sha256 != record.artifact_sha256
                    || status.version != record.version
                {
                    return Err(ServiceFailure::RequestConflict);
                }
                record.installation = Some(InstallationStatus {
                    installation: status.installation,
                    version: status.version.clone(),
                    artifact_sha256: status.artifact_sha256,
                });
                self.install_phase(request, &mut record, ServicePhase::Publishing)?;
                store.activate(
                    &scope,
                    status.installation,
                    store.snapshot()?.generation,
                    crate::organization_store::schema::SCHEMA_VERSION,
                )?;
                record
                    .installation
                    .clone()
                    .ok_or(ServiceFailure::InvalidRecord)?
            };
            record.installation = Some(status.clone());
            self.install_phase(request, &mut record, ServicePhase::Publishing)?;
            let (directory, snapshot, mut next) =
                self.read_registration()?.ok_or(ServiceFailure::Missing)?;
            if next.approved.as_ref() != Some(&status) || !next.installed {
                next.approved = Some(status);
                next.installed = true;
                next.generation = next
                    .generation
                    .checked_add(1)
                    .ok_or(ServiceFailure::LimitExceeded)?;
                lease.verify()?;
                self.publish_registration(&next, &snapshot, &directory)?;
            }
            self.install_phase(request, &mut record, ServicePhase::Starting)?;
        }
        let registered = self.discover()?.ok_or(ServiceFailure::Missing)?;
        lease.verify()?;
        self.manager()?.verify_installation(&registered)?;
        self.manager()?.configure(&registered).await?;
        if registered.desired_state() == DesiredServerState::Running {
            self.manager()?.start(&registered).await?;
            if self.manager()?.status(Some(&registered)).await? != SupervisorState::Running {
                return Ok(record.progress(request));
            }
        }
        self.finish_install(request, record, &registered)
    }
    fn finish_install(
        &self,
        request: NativeRequestId,
        mut record: InstallRecord,
        registered: &RegisteredServer,
    ) -> Result<ServiceProgress, ServiceFailure> {
        let installation = record
            .installation
            .clone()
            .ok_or(ServiceFailure::InvalidRecord)?;
        record.phase = ServicePhase::Completed;
        record.state = ServiceRequestState::Completed(ServiceOutcome::Installed {
            server: registered.server_pin(),
            installation,
            reused: record.reused,
        });
        self.write_install(request, &record)?;
        Ok(record.progress(request))
    }
}
struct InstallAdmission<'a> {
    supervisor: &'a ServiceSupervisor,
    request: NativeRequestId,
    lease: &'a Lease,
    digest: RequestDigest,
}
impl InstallationAdmission for InstallAdmission<'_> {
    fn verify(
        &self,
        registered: &RegisteredServer,
        operation: InstallationOperation,
    ) -> Result<(), ServiceFailure> {
        self.lease.verify()?;
        let original = self
            .supervisor
            .original_install(self.request)?
            .ok_or(ServiceFailure::UnknownRequest)?;
        if !original.pending()
            || original.reused
            || original.digest != self.digest
            || registered.key() != self.supervisor.key
        {
            return Err(ServiceFailure::RequestConflict);
        }
        let current = self.supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
        if current.generation() != registered.generation()
            || current.server_pin() != registered.server_pin()
        {
            return Err(ServiceFailure::RequestConflict);
        }
        match operation {
            InstallationOperation::Open => Ok(()),
            InstallationOperation::Stage { artifact_sha256 }
                if artifact_sha256 == original.artifact_sha256 =>
            {
                Ok(())
            }
            InstallationOperation::Discard { installation, .. }
                if original
                    .installation
                    .as_ref()
                    .is_some_and(|status| status.installation == installation) =>
            {
                Ok(())
            }
            InstallationOperation::Cleanup if original.installation.is_some() => Ok(()),
            InstallationOperation::Activate {
                installation,
                storage_schema,
                ..
            } if original
                .installation
                .as_ref()
                .is_some_and(|status| status.installation == installation)
                && storage_schema == crate::organization_store::schema::SCHEMA_VERSION =>
            {
                Ok(())
            }
            _ => Err(ServiceFailure::RequestConflict),
        }
    }
}
#[cfg(feature = "test-support")]
pub(super) fn test_assessment(
    artifact: VerifiedServerArtifact,
    data: PathBuf,
    runtime: PathBuf,
) -> ServiceInstallAssessment {
    ServiceInstallAssessment {
        artifact,
        roots: InstallRoots { data, runtime },
        desktop_seed: None,
    }
}

fn classify_storage(failure: crate::storage::StorageFailure) -> ServiceFailure {
    use crate::storage::StorageFailure;
    match failure {
        StorageFailure::Unavailable | StorageFailure::Io => ServiceFailure::StorageUnavailable,
        StorageFailure::UnsafeStorage => ServiceFailure::UnsafeStorage,
        StorageFailure::UnknownSchema { .. } => ServiceFailure::UnsupportedSchema,
        StorageFailure::InvalidState => ServiceFailure::InvalidRecord,
        StorageFailure::RecoveryRequired => ServiceFailure::Indeterminate,
    }
}
fn classify_store(failure: spaceterm_core::storage::StoreFailure) -> ServiceFailure {
    use spaceterm_core::storage::StoreFailure;
    match failure {
        StoreFailure::Locked => ServiceFailure::StorageLocked,
        StoreFailure::Unavailable | StoreFailure::Io => ServiceFailure::StorageUnavailable,
        StoreFailure::UnsafeStorage => ServiceFailure::UnsafeStorage,
        StoreFailure::UnknownSchema => ServiceFailure::UnsupportedSchema,
        StoreFailure::IdentityMismatch => ServiceFailure::IdentityMismatch,
        StoreFailure::RecoveryRequired => ServiceFailure::Indeterminate,
        _ => ServiceFailure::InvalidRecord,
    }
}
