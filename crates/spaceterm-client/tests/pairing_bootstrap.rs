use spaceterm_client::pairing::{bootstrap::*, credentials::PairingFuture};
use spaceterm_core::pairing::PairingFailure;
use spaceterm_distribution::server_feed::BuildChannel;
use spaceterm_protocol::authentication::ServerPin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use zeroize::Zeroizing;

fn route() -> BootstrapRoute {
    BootstrapRoute::capture([1; 32], [2; 32], [3; 32], 1000, BuildChannel::Release).unwrap()
}
fn pin() -> ServerPin {
    ServerPin {
        identity: [4; 16],
        public_key: [5; 32],
    }
}
fn facts() -> BootstrapHostFacts {
    BootstrapHostFacts {
        route: route(),
        account_uid: 1000,
        os: BootstrapOs::Linux,
        architecture: BootstrapArchitecture::X86_64,
        os_version: HostVersion::new(22, 4, 0),
        libc: BootstrapLibc::Glibc(HostVersion::new(2, 35, 0)),
        kernel: HostVersion::new(5, 15, 0),
        supervision: BootstrapSupervision::SystemdUser {
            version: 249,
            reachable: true,
        },
        registration: BootstrapRegistration::Running {
            pin: pin(),
            generation: 7,
            artifact_identity: [6; 32],
            version: HostVersion::new(1, 0, 0),
            protocol_majors: [1, 2],
        },
        storage: BootstrapPrerequisite::Available,
        installation: BootstrapPrerequisite::Available,
        linger: BootstrapLinger::Enabled,
        sudo: BootstrapSudo::ResponseRequired,
        authorization_agent: true,
        background_items_allowed: true,
    }
}
#[derive(Default)]
struct Effects {
    prompt_attempts: AtomicUsize,
    lost_install_response: AtomicBool,
    lost_owner_response: AtomicBool,
    hold_resolution: AtomicBool,
    resolution_started: tokio::sync::Notify,
    resolutions: Mutex<Vec<([u8; 16], [u8; 32])>>,
    outcomes: Mutex<std::collections::HashMap<[u8; 16], BootstrapServer>>,
    local_unavailable: AtomicBool,
    hold_preflight: AtomicBool,
    preflight_started: tokio::sync::Notify,
    owner_requests: Mutex<Vec<[u8; 16]>>,
    installations: AtomicUsize,
    registry: Mutex<Option<BootstrapServer>>,
    plans: Mutex<Vec<BootstrapOperation>>,
    authorizations: AtomicUsize,
    mutations: AtomicUsize,
    enrollments: AtomicUsize,
    connections: AtomicUsize,
    cleanups: AtomicUsize,
    stale: AtomicBool,
    hold: AtomicBool,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    completed: tokio::sync::Notify,
}
struct Lease(Arc<Effects>);
impl BootstrapSshLease for Lease {
    fn revalidate(&self, _: BootstrapRoute) -> Result<(), BootstrapFailure> {
        if self.0.stale.load(Ordering::Acquire) {
            Err(BootstrapFailure::new(
                PairingFailure::RouteChanged,
                BootstrapPhase::OpenSshAuthenticated,
            ))
        } else {
            Ok(())
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.cleanups.fetch_add(1, Ordering::SeqCst);
    }
}
struct Setup {
    facts: Mutex<BootstrapHostFacts>,
    effects: Arc<Effects>,
}
struct Operation {
    plan: BootstrapPlan,
    request: [u8; 16],
    effects: Arc<Effects>,
}
impl BootstrapAuthorizedOperation for Operation {
    fn plan(&self) -> &BootstrapPlan {
        &self.plan
    }
    fn request_identity(&self) -> [u8; 16] {
        self.request
    }
    fn apply(
        self: Box<Self>,
        _: Option<VerifiedServerArtifact>,
    ) -> PairingFuture<'static, Result<BootstrapServer, BootstrapFailure>> {
        Box::pin(async move {
            self.effects.mutations.fetch_add(1, Ordering::SeqCst);
            self.effects.started.notify_one();
            if self.effects.hold.load(Ordering::Acquire) {
                self.effects.release.notified().await;
            }
            let server = BootstrapServer {
                route: self.plan.route(),
                pin: pin(),
                registration_generation: 7,
                approved_artifact_identity: self.plan.artifact_identity().unwrap(),
                protocol_majors: [1, 2],
                running: true,
            };
            let server = if self.plan.operation() == BootstrapOperation::InstallAndStart {
                let mut registry = self.effects.registry.lock().unwrap();
                *registry.get_or_insert_with(|| {
                    self.effects.installations.fetch_add(1, Ordering::SeqCst);
                    server
                })
            } else {
                server
            };
            self.effects
                .outcomes
                .lock()
                .unwrap()
                .insert(self.request, server);
            self.effects.completed.notify_one();
            let lost_response = match self.plan.operation() {
                BootstrapOperation::InstallAndStart => {
                    self.effects.lost_install_response.load(Ordering::Acquire)
                }
                BootstrapOperation::AuthorizeOwnerEnrollment => {
                    self.effects.lost_owner_response.load(Ordering::Acquire)
                }
            };
            if lost_response {
                Err(BootstrapFailure::new(
                    PairingFailure::Offline,
                    BootstrapPhase::Applied,
                ))
            } else {
                Ok(server)
            }
        })
    }
}
impl BootstrapAuthorizationBoundary for Setup {
    fn authorize<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
        prompt: &'a dyn ScopedAuthenticationPrompt,
    ) -> PairingFuture<'a, Result<Box<dyn BootstrapAuthorizedOperation>, BootstrapFailure>> {
        Box::pin(async move {
            for _ in 0..self.effects.prompt_attempts.load(Ordering::Acquire) {
                drop(prompt.respond(plan).await?);
            }
            let sequence = self.effects.authorizations.fetch_add(1, Ordering::SeqCst) + 1;
            self.effects.plans.lock().unwrap().push(plan.operation());
            Ok(Box::new(Operation {
                plan: plan.clone(),
                request: [sequence as u8; 16],
                effects: self.effects.clone(),
            }) as Box<dyn BootstrapAuthorizedOperation>)
        })
    }
}
impl NativeServerSetup for Setup {
    fn authenticate<'a>(
        &'a self,
        route: &'a BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<OpenSshAuthenticated, BootstrapFailure>> {
        Box::pin(async move {
            attempt.validate(BootstrapPhase::ExplicitIntent)?;
            OpenSshAuthenticated::retain(*route, Box::new(Lease(self.effects.clone())))
        })
    }
    fn discover<'a>(
        &'a self,
        _: &'a OpenSshAuthenticated,
    ) -> PairingFuture<'a, Result<BootstrapHostFacts, BootstrapFailure>> {
        Box::pin(async move {
            if self.effects.hold_preflight.load(Ordering::Acquire) {
                self.effects.preflight_started.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(*self.facts.lock().unwrap())
        })
    }
    fn authorize<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
        prompt: &'a dyn ScopedAuthenticationPrompt,
    ) -> PairingFuture<'a, Result<AuthorizedBootstrap, BootstrapFailure>> {
        Box::pin(async move { plan.authorize(self, prompt).await })
    }
    fn resolve(
        &self,
        request: [u8; 16],
        artifact: [u8; 32],
    ) -> PairingFuture<'_, Result<BootstrapServer, BootstrapFailure>> {
        Box::pin(async move {
            self.effects
                .resolutions
                .lock()
                .unwrap()
                .push((request, artifact));
            self.effects.resolution_started.notify_one();
            if self.effects.hold_resolution.load(Ordering::Acquire) {
                self.effects.release.notified().await;
            }
            let server = self
                .effects
                .outcomes
                .lock()
                .unwrap()
                .get(&request)
                .copied()
                .ok_or_else(|| {
                    BootstrapFailure::new(PairingFailure::RecoveryRequired, BootstrapPhase::Applied)
                })?;
            if server.approved_artifact_identity != artifact {
                return Err(BootstrapFailure::new(
                    PairingFailure::IdentityMismatch,
                    BootstrapPhase::Applied,
                ));
            }
            Ok(server)
        })
    }
}
struct Enrollment(Arc<Effects>);
impl BootstrapEnrollment for Enrollment {
    fn preflight<'a>(
        &'a self,
        _: BootstrapRoute,
        _: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        Box::pin(async move {
            if self.0.local_unavailable.load(Ordering::Acquire) {
                Err(BootstrapFailure::new(
                    PairingFailure::CredentialUnavailable,
                    BootstrapPhase::ReadOnlyPreflight,
                ))
            } else {
                Ok(())
            }
        })
    }
    fn prepare_owner<'a>(
        &'a self,
        server: &'a BootstrapServer,
        _: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<spaceterm_core::pairing::EnrollmentProposal, BootstrapFailure>>
    {
        Box::pin(async move {
            use spaceterm_core::{
                identity::{DeviceId, ServerId},
                pairing::{
                    EnrollmentProposal, EnrollmentProposalParts, PairingKeys, PairingKeysParts,
                },
            };
            Ok(EnrollmentProposal::new(EnrollmentProposalParts {
                server: ServerId::new(server.pin.identity),
                device: DeviceId::new([10; 16]),
                keys: PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: [11; 32],
                    pairing_authentication_public_key: [12; 32],
                })
                .unwrap(),
                claim_nonce: [13; 32],
            })
            .unwrap())
        })
    }
    fn enroll_owner<'a>(
        &'a self,
        _: &'a BootstrapServer,
        request: [u8; 16],
        proposal: &'a spaceterm_core::pairing::EnrollmentProposal,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        Box::pin(async move {
            self.0.owner_requests.lock().unwrap().push(request);
            assert_ne!(request, [0; 16]);
            assert_eq!(proposal.parts().server.bytes(), pin().identity);
            attempt.validate(BootstrapPhase::Enrolled)?;
            self.0.enrollments.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn connect<'a>(
        &'a self,
        _: &'a BootstrapServer,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        Box::pin(async move {
            attempt.validate(BootstrapPhase::Connected)?;
            self.0.connections.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
struct Prompt;
impl ScopedAuthenticationPrompt for Prompt {
    fn revalidate(&self, _: &BootstrapPlan) -> Result<(), BootstrapFailure> {
        Ok(())
    }
    fn respond<'a>(
        &'a self,
        _: &'a BootstrapPlan,
    ) -> PairingFuture<'a, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async { panic!("policy fake must not request an authorization response") })
    }
}
fn coordinator(
    facts: BootstrapHostFacts,
) -> (Arc<Setup>, BootstrapCoordinator<Setup, Enrollment, Prompt>) {
    let setup = Arc::new(Setup {
        facts: Mutex::new(facts),
        effects: Arc::new(Effects::default()),
    });
    (
        setup.clone(),
        BootstrapCoordinator::new(setup.clone(), Enrollment(setup.effects.clone()), Prompt),
    )
}
fn intent() -> DeliberateBootstrap {
    ForegroundBootstrap::confirm(route(), 1).unwrap()
}

#[tokio::test]
async fn bootstrap_reuses_compatible_registered_server_and_enrolls() {
    let (setup, c) = coordinator(facts());
    let outcome = c.bootstrap(intent(), None).await.unwrap();
    assert!(outcome.reused);
    assert_eq!(outcome.server.pin, pin());
    assert_eq!(outcome.phase, BootstrapPhase::Connected);
    assert_eq!(setup.effects.enrollments.load(Ordering::SeqCst), 1);
    assert_eq!(setup.effects.connections.load(Ordering::SeqCst), 1);
}

fn encoded(bytes: &[u8]) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            text.push(if i <= chunk.len() {
                ABC[((n >> (18 - i * 6)) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    text
}
fn artifact() -> VerifiedServerArtifact {
    use spaceterm_distribution::*;
    let (key, feed, signature, bytes) =
        artifact_fixture(BuildChannel::Release, ServerPlatform::LinuxX86_64);
    let verified = verify_account_server_feed(
        &key,
        BuildChannel::Release,
        ServerPlatform::LinuxX86_64,
        &feed,
        &signature,
        NOW,
    )
    .unwrap();
    ServerArchiveVerifier::new()
        .verify_account_owned(&key, verified, bytes)
        .unwrap()
}

use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::*;
const NOW: u64 = 1_800_000_000;

fn artifact_fixture(
    build: BuildChannel,
    platform: ServerPlatform,
) -> (UpdateKey, Vec<u8>, Vec<u8>, Vec<u8>) {
    let signer = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let key = UpdateKey::from_base64(&encoded(signer.public_key().as_ref())).unwrap();
    let version = if build == BuildChannel::Release {
        "1.2.3"
    } else {
        "dev.010101010101"
    };
    let name = match platform {
        ServerPlatform::LinuxX86_64 => "bin/spaceterm-server".to_owned(),
        ServerPlatform::DarwinArm64 => {
            format!("{}/Contents/MacOS/spaceterm-server", build.bundle_name())
        }
    };
    let payload = b"signed server bytes";
    let resource_root = match platform {
        ServerPlatform::LinuxX86_64 => "share/spaceterm-server".to_owned(),
        ServerPlatform::DarwinArm64 => format!("{}/Contents/Resources", build.bundle_name()),
    };
    let mut files = vec![(name.clone(), 0o755, payload.as_slice())];
    for resource in [
        "shell-integration/VERSION",
        "shell-integration/bash/spaceterm.bash",
        "shell-integration/sh/spaceterm.sh",
        "shell-integration/zsh/.zshenv",
        "shell-integration/zsh/spaceterm-integration",
        "terminfo/x/xterm-spaceterm",
        "THIRD-PARTY-NOTICES.txt",
    ] {
        files.push((
            format!("{resource_root}/{resource}"),
            0o644,
            b"bundled resource".as_slice(),
        ));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let manifest = AccountInstallationManifestV2 {
        schema_version: 2,
        component: ServerComponent::Server,
        build_identity: build,
        platform,
        version: BuildVersion::parse(version).unwrap(),
        build_commit: [1; 20],
        protocol_majors: vec![1, 2],
        storage: StorageCompatibility {
            read: vec![1],
            write: 1,
            migrate_from: vec![],
        },
        native_identifier: build.native_identifier().into(),
        entry_points: AccountEntryPoints {
            server: name.clone(),
            cli: None,
        },
        files: files
            .iter()
            .map(|(name, mode, bytes)| InstallationFile {
                name: name.clone(),
                mode: *mode,
                size: bytes.len() as u64,
                sha256: Sha256::digest(bytes).into(),
            })
            .collect(),
        dependencies: vec![],
    };
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    for (name, mode, bytes) in
        std::iter::once(("installation.json".to_owned(), 0o644, manifest.as_slice())).chain(files)
    {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("spaceterm-server/{name}"), bytes)
            .unwrap();
    }
    let tar = builder.into_inner().unwrap();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gzip, &tar).unwrap();
    let archive = gzip.finish().unwrap();
    let feed = AccountServerFeedV2 {
        schema_version: 2,
        component: ServerComponent::Server,
        build_identity: build,
        platform,
        version: BuildVersion::parse(version).unwrap(),
        build_commit: [1; 20],
        published_at: NOW,
        protocol_majors: vec![1, 2],
        storage: StorageCompatibility {
            read: vec![1],
            write: 1,
            migrate_from: vec![],
        },
        installation: AccountInstallationCompatibility {
            manifest_schema: 2,
            registration_schema: 1,
            journal_schema: 1,
        },
        archive: ServerArchiveV1 {
            name: platform.archive_name(version),
            size: archive.len() as u64,
            sha256: Sha256::digest(&archive).into(),
            signature: signer.sign(&archive).as_ref().try_into().unwrap(),
            manifest_sha256: Sha256::digest(&manifest).into(),
        },
    };
    let feed = serde_json::to_vec(&feed).unwrap();
    let signature = encoded(signer.sign(&feed).as_ref()).into_bytes();
    (key, feed, signature, archive)
}

async fn refused(f: BootstrapHostFacts, expected: BootstrapFailureClass) {
    let (s, c) = coordinator(f);
    let result = c.bootstrap(intent(), None).await;
    assert_eq!(result.err().unwrap().class, expected);
    assert_eq!(s.effects.authorizations.load(Ordering::SeqCst), 0);
    assert_eq!(s.effects.mutations.load(Ordering::SeqCst), 0);
    assert_eq!(s.effects.enrollments.load(Ordering::SeqCst), 0);
}
fn failure(class: PairingFailure) -> BootstrapFailureClass {
    BootstrapFailureClass::Pairing(class)
}
#[tokio::test]
async fn bootstrap_incompatible_server_is_not_updated_or_replaced() {
    let mut f = facts();
    if let BootstrapRegistration::Running {
        ref mut protocol_majors,
        ..
    } = f.registration
    {
        *protocol_majors = [3, 4];
    }
    refused(f, failure(PairingFailure::Incompatible)).await;
}
#[tokio::test]
async fn bootstrap_stopped_server_requires_separate_start() {
    let mut f = facts();
    f.registration = BootstrapRegistration::Stopped {
        pin: pin(),
        generation: 7,
    };
    refused(f, failure(PairingFailure::ExplicitStartRequired)).await;
}
#[tokio::test]
async fn bootstrap_locked_registration_is_not_missing() {
    for state in [
        BootstrapRegistration::Locked,
        BootstrapRegistration::Unavailable,
    ] {
        let mut f = facts();
        f.registration = state;
        let expected = if state == BootstrapRegistration::Locked {
            BootstrapFailureClass::StorageLocked
        } else {
            failure(PairingFailure::StorageUnavailable)
        };
        refused(f, expected).await;
    }
}
#[tokio::test]
async fn bootstrap_detects_arch_libc_service_manager_and_authorization_before_mutation() {
    let mut f = facts();
    f.supervision = BootstrapSupervision::Missing;
    refused(f, failure(PairingFailure::ServiceManagerUnavailable)).await;
    let mut f = facts();
    f.linger = BootstrapLinger::Disabled;
    f.sudo = BootstrapSudo::Unavailable;
    refused(f, failure(PairingFailure::AuthorizationUnavailable)).await;
    let mut f = facts();
    f.linger = BootstrapLinger::Disabled;
    f.sudo = BootstrapSudo::Denied;
    refused(f, failure(PairingFailure::AuthorizationDenied)).await;
}
#[tokio::test]
async fn bootstrap_supports_systemd_249_without_host_encrypted_unit_support() {
    let (_, c) = coordinator(facts());
    assert!(c.bootstrap(intent(), None).await.is_ok());
}
#[tokio::test]
async fn bootstrap_rejects_musl_old_glibc_and_unpublished_architecture_before_mutation() {
    for libc in [
        BootstrapLibc::Musl,
        BootstrapLibc::Glibc(HostVersion::new(2, 34, 0)),
    ] {
        let mut f = facts();
        f.libc = libc;
        refused(f, failure(PairingFailure::UnsupportedTarget)).await;
    }
    let mut f = facts();
    f.architecture = BootstrapArchitecture::Arm64;
    refused(f, failure(PairingFailure::UnsupportedTarget)).await;
    let mut f = facts();
    f.kernel = HostVersion::new(5, 14, 99);
    refused(f, failure(PairingFailure::UnsupportedTarget)).await;
}
#[tokio::test]
async fn bootstrap_missing_agent_and_background_items_fail_before_mutation() {
    let mut f = facts();
    f.linger = BootstrapLinger::Disabled;
    f.authorization_agent = false;
    refused(f, BootstrapFailureClass::MissingAuthorizationAgent).await;
    let mut f = facts();
    f.os = BootstrapOs::MacOs;
    f.architecture = BootstrapArchitecture::Arm64;
    f.os_version = HostVersion::new(26, 0, 0);
    f.libc = BootstrapLibc::NotApplicable;
    f.supervision = BootstrapSupervision::LaunchAgent { gui_domain: true };
    f.linger = BootstrapLinger::NotApplicable;
    f.background_items_allowed = false;
    refused(f, BootstrapFailureClass::BackgroundItemsSuppressed).await;
}
#[tokio::test]
async fn bootstrap_requires_verified_account_artifact() {
    let mut f = facts();
    f.registration = BootstrapRegistration::Missing;
    let (s, c) = coordinator(f);
    assert_eq!(
        c.bootstrap(intent(), None).await.err().unwrap().class,
        BootstrapFailureClass::BootstrapTrustUnavailable
    );
    assert_eq!(s.effects.authorizations.load(Ordering::SeqCst), 0);
    assert!(
        !c.bootstrap(intent(), Some(artifact()))
            .await
            .unwrap()
            .reused
    );
}
#[tokio::test]
async fn bootstrap_changed_host_or_account_fails_closed() {
    let mut f = facts();
    f.route =
        BootstrapRoute::capture([9; 32], [2; 32], [3; 32], 1000, BuildChannel::Release).unwrap();
    refused(f, failure(PairingFailure::RouteChanged)).await;
    let mut f = facts();
    f.account_uid = 1001;
    refused(f, failure(PairingFailure::RouteChanged)).await;
}
#[tokio::test]
async fn bootstrap_stale_completion_does_not_enroll() {
    let (s, c) = coordinator(facts());
    s.effects.hold.store(true, Ordering::SeqCst);
    let i = intent();
    let cancel = i.cancellation();
    let task = tokio::spawn(async move { c.bootstrap(i, None).await });
    s.effects.started.notified().await;
    cancel.cancel();
    s.effects.release.notify_one();
    assert_eq!(
        task.await.unwrap().err().unwrap().class,
        failure(PairingFailure::Cancelled)
    );
    assert_eq!(s.effects.enrollments.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn bootstrap_started_publication_outlives_client_observation() {
    let mut f = facts();
    f.registration = BootstrapRegistration::Missing;
    let (s, c) = coordinator(f);
    s.effects.hold.store(true, Ordering::SeqCst);
    let task = tokio::spawn(async move { c.bootstrap(intent(), Some(artifact())).await });
    s.effects.started.notified().await;
    task.abort();
    assert!(task.await.err().unwrap().is_cancelled());
    assert_eq!(s.effects.cleanups.load(Ordering::SeqCst), 0);
    s.effects.release.notify_one();
    s.effects.completed.notified().await;
    tokio::task::yield_now().await;
    assert_eq!(s.effects.mutations.load(Ordering::SeqCst), 1);
    assert_eq!(s.effects.cleanups.load(Ordering::SeqCst), 1);
    assert_eq!(s.effects.enrollments.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn bootstrap_concurrent_installers_converge_on_one_registration() {
    // Native serialization rechecks Missing and returns the one committed registration to both owners.
    // This fake models that external guarantee; the real native publication test is a separate gate.
    let mut f = facts();
    f.registration = BootstrapRegistration::Missing;
    let (s, c) = coordinator(f);
    let (a, b) = tokio::join!(
        c.bootstrap(intent(), Some(artifact())),
        c.bootstrap(intent(), Some(artifact()))
    );
    assert_eq!(a.unwrap().server.pin, b.unwrap().server.pin);
    assert_eq!(s.effects.installations.load(Ordering::SeqCst), 1);
}
#[test]
fn ordinary_reconnect_never_constructs_bootstrap_intent() {
    for source in [
        include_str!("../src/connection.rs"),
        include_str!("../src/pairing/revocation.rs"),
    ] {
        assert!(!source.contains("ForegroundBootstrap"));
        assert!(!source.contains("DeliberateBootstrap"));
    }
}

#[tokio::test]
async fn bootstrap_requests_only_install_and_claim_bound_owner_authority() {
    // The portable gate checks requested authority. Actual listener exposure remains a native gate.
    for (registration, expected) in [
        (
            facts().registration,
            BootstrapOperation::AuthorizeOwnerEnrollment,
        ),
        (
            BootstrapRegistration::Missing,
            BootstrapOperation::InstallAndStart,
        ),
    ] {
        let mut f = facts();
        f.registration = registration;
        let (s, c) = coordinator(f);
        c.bootstrap(intent(), Some(artifact())).await.unwrap();
        assert_eq!(
            *s.effects.plans.lock().unwrap(),
            if expected == BootstrapOperation::InstallAndStart {
                vec![expected, BootstrapOperation::AuthorizeOwnerEnrollment]
            } else {
                vec![expected]
            }
        );
    }
}

#[tokio::test]
async fn bootstrap_install_authority_never_substitutes_for_claim_bound_owner_authority() {
    let mut f = facts();
    f.registration = BootstrapRegistration::Missing;
    let (s, c) = coordinator(f);
    c.bootstrap(intent(), Some(artifact())).await.unwrap();
    assert_eq!(
        *s.effects.plans.lock().unwrap(),
        [
            BootstrapOperation::InstallAndStart,
            BootstrapOperation::AuthorizeOwnerEnrollment
        ]
    );
    assert_eq!(*s.effects.owner_requests.lock().unwrap(), vec![[2; 16]]);
}

#[tokio::test]
async fn bootstrap_cancel_handle_interrupts_preflight_and_releases_owned_ssh() {
    let (s, c) = coordinator(facts());
    s.effects.hold_preflight.store(true, Ordering::Release);
    let intent = intent();
    let cancellation = intent.cancellation();
    let task = tokio::spawn(async move { c.bootstrap(intent, None).await });
    s.effects.preflight_started.notified().await;
    cancellation.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("cancelled preflight stayed alive")
        .unwrap();
    assert_eq!(
        result.err().unwrap().class,
        failure(PairingFailure::Cancelled)
    );
    assert_eq!(s.effects.cleanups.load(Ordering::SeqCst), 1);
    assert_eq!(s.effects.authorizations.load(Ordering::SeqCst), 0);
}

struct BoundPrompt(BootstrapAttempt);
impl ScopedAuthenticationPrompt for BoundPrompt {
    fn revalidate(&self, plan: &BootstrapPlan) -> Result<(), BootstrapFailure> {
        if plan.belongs_to_attempt(&self.0) {
            Ok(())
        } else {
            Err(BootstrapFailure::new(
                PairingFailure::StaleAssessment,
                BootstrapPhase::Authorized,
            ))
        }
    }
    fn respond<'a>(
        &'a self,
        _: &'a BootstrapPlan,
    ) -> PairingFuture<'a, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async { panic!("stale prompt must not run") })
    }
}
#[tokio::test]
async fn equal_generation_from_another_foreground_attempt_cannot_reuse_a_prompt() {
    let original = intent();
    let original_attempt = original.cancellation();
    let (s, _) = coordinator(facts());
    let c = BootstrapCoordinator::new(
        s.clone(),
        Enrollment(s.effects.clone()),
        BoundPrompt(original_attempt),
    );
    let result = c.bootstrap(intent(), None).await;
    assert_eq!(
        result.err().unwrap().class,
        failure(PairingFailure::StaleAssessment)
    );
    assert_eq!(s.effects.authorizations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unavailable_local_credentials_prevent_remote_installation() {
    let mut f = facts();
    f.registration = BootstrapRegistration::Missing;
    let (s, c) = coordinator(f);
    s.effects.local_unavailable.store(true, Ordering::Release);
    let result = c.bootstrap(intent(), Some(artifact())).await;
    assert_eq!(
        result.err().unwrap().class,
        failure(PairingFailure::CredentialUnavailable)
    );
    assert_eq!(s.effects.authorizations.load(Ordering::SeqCst), 0);
    assert_eq!(s.effects.installations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn enabled_linger_needs_neither_sudo_nor_an_authorization_agent() {
    let mut f = facts();
    f.sudo = BootstrapSudo::Unavailable;
    f.authorization_agent = false;
    let (setup, coordinator) = coordinator(f);
    let outcome = coordinator.bootstrap(intent(), None).await.unwrap();
    assert!(outcome.reused);
    assert_eq!(setup.effects.installations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn macos_without_gui_domain_is_not_a_daemon_fallback() {
    let mut f = facts();
    f.os = BootstrapOs::MacOs;
    f.architecture = BootstrapArchitecture::Arm64;
    f.os_version = HostVersion::new(26, 0, 0);
    f.libc = BootstrapLibc::NotApplicable;
    f.supervision = BootstrapSupervision::LaunchAgent { gui_domain: false };
    f.linger = BootstrapLinger::NotApplicable;
    refused(f, BootstrapFailureClass::LoginSessionUnavailable).await;
}

#[tokio::test]
async fn lost_install_and_owner_responses_resolve_original_requests_without_reinstalling() {
    let mut f = facts();
    f.registration = BootstrapRegistration::Missing;
    let (setup, c) = coordinator(f);
    setup
        .effects
        .lost_install_response
        .store(true, Ordering::Release);
    setup
        .effects
        .lost_owner_response
        .store(true, Ordering::Release);
    let artifact = artifact();
    let digest = artifact.artifact_sha256();
    c.bootstrap(intent(), Some(artifact)).await.unwrap();
    assert_eq!(setup.effects.installations.load(Ordering::SeqCst), 1);
    assert_eq!(setup.effects.authorizations.load(Ordering::SeqCst), 2);
    assert_eq!(
        *setup.effects.resolutions.lock().unwrap(),
        [([1; 16], digest), ([2; 16], digest)]
    );
    assert_eq!(*setup.effects.owner_requests.lock().unwrap(), [[2; 16]]);
    assert_eq!(setup.effects.enrollments.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn initial_install_deadline_preserves_admitted_work_without_late_recovery_or_enrollment() {
    for lost_response in [false, true] {
        let mut f = facts();
        f.registration = BootstrapRegistration::Missing;
        let (setup, c) = coordinator(f);
        setup.effects.hold.store(true, Ordering::Release);
        setup
            .effects
            .lost_install_response
            .store(lost_response, Ordering::Release);
        let artifact = artifact();
        let digest = artifact.artifact_sha256();
        let started = tokio::time::Instant::now();
        let task = tokio::spawn(async move { c.bootstrap(intent(), Some(artifact)).await });
        setup.effects.started.notified().await;
        tokio::time::advance(std::time::Duration::from_secs(15)).await;
        let failure = tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("initial install observation did not expire")
            .unwrap()
            .err()
            .unwrap();
        assert_eq!(
            failure,
            BootstrapFailure::new(PairingFailure::RecoveryRequired, BootstrapPhase::Applied)
        );
        assert_eq!(
            tokio::time::Instant::now() - started,
            std::time::Duration::from_secs(15)
        );
        assert_eq!(setup.effects.cleanups.load(Ordering::SeqCst), 0);
        assert_eq!(setup.effects.installations.load(Ordering::SeqCst), 0);
        assert!(setup.effects.resolutions.lock().unwrap().is_empty());
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        setup.effects.release.notify_one();
        setup.effects.completed.notified().await;
        tokio::task::yield_now().await;
        assert_eq!(setup.effects.installations.load(Ordering::SeqCst), 1);
        assert_eq!(setup.effects.cleanups.load(Ordering::SeqCst), 1);
        assert!(setup.effects.resolutions.lock().unwrap().is_empty());
        assert_eq!(setup.effects.authorizations.load(Ordering::SeqCst), 1);
        assert_eq!(setup.effects.enrollments.load(Ordering::SeqCst), 0);
        assert_eq!(setup.effects.connections.load(Ordering::SeqCst), 0);
        let recovered = setup.resolve([1; 16], digest).await.unwrap();
        assert_eq!(recovered.pin, pin());
        assert_eq!(recovered.approved_artifact_identity, digest);
        assert_eq!(
            *setup.effects.resolutions.lock().unwrap(),
            [([1; 16], digest)]
        );
        assert_eq!(setup.effects.mutations.load(Ordering::SeqCst), 1);
        assert_eq!(setup.effects.authorizations.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn automatic_original_recovery_uses_only_the_remaining_initial_install_deadline() {
    let mut f = facts();
    f.registration = BootstrapRegistration::Missing;
    let (setup, c) = coordinator(f);
    setup.effects.hold.store(true, Ordering::Release);
    setup
        .effects
        .lost_install_response
        .store(true, Ordering::Release);
    setup.effects.hold_resolution.store(true, Ordering::Release);
    let artifact = artifact();
    let digest = artifact.artifact_sha256();
    let started = tokio::time::Instant::now();
    let task = tokio::spawn(async move { c.bootstrap(intent(), Some(artifact)).await });
    setup.effects.started.notified().await;
    tokio::time::advance(std::time::Duration::from_secs(14)).await;
    setup.effects.release.notify_one();
    setup.effects.resolution_started.notified().await;
    assert_eq!(
        *setup.effects.resolutions.lock().unwrap(),
        [([1; 16], digest)]
    );
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let failure = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("automatic recovery renewed the initial install deadline")
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(
        failure,
        BootstrapFailure::new(PairingFailure::RecoveryRequired, BootstrapPhase::Applied)
    );
    assert_eq!(
        tokio::time::Instant::now() - started,
        std::time::Duration::from_secs(15)
    );
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    setup
        .effects
        .hold_resolution
        .store(false, Ordering::Release);
    let recovered = setup.resolve([1; 16], digest).await.unwrap();
    assert_eq!(recovered.pin, pin());
    assert_eq!(recovered.approved_artifact_identity, digest);
    assert_eq!(
        *setup.effects.resolutions.lock().unwrap(),
        [([1; 16], digest), ([1; 16], digest)]
    );
    assert_eq!(setup.effects.installations.load(Ordering::SeqCst), 1);
    assert_eq!(setup.effects.mutations.load(Ordering::SeqCst), 1);
    assert_eq!(setup.effects.authorizations.load(Ordering::SeqCst), 1);
    assert_eq!(setup.effects.enrollments.load(Ordering::SeqCst), 0);
    assert_eq!(setup.effects.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn lost_owner_response_on_reuse_does_not_install_restart_or_allocate_another_owner_request() {
    let (setup, c) = coordinator(facts());
    setup
        .effects
        .lost_owner_response
        .store(true, Ordering::Release);
    let outcome = c.bootstrap(intent(), None).await.unwrap();
    assert!(outcome.reused);
    assert_eq!(setup.effects.installations.load(Ordering::SeqCst), 0);
    assert_eq!(setup.effects.authorizations.load(Ordering::SeqCst), 1);
    assert_eq!(
        *setup.effects.resolutions.lock().unwrap(),
        [([1; 16], [6; 32])]
    );
    assert_eq!(*setup.effects.owner_requests.lock().unwrap(), [[1; 16]]);
}

#[tokio::test]
async fn bootstrap_locked_storage_and_unreachable_user_manager_do_not_mutate() {
    let mut f = facts();
    f.storage = BootstrapPrerequisite::Locked;
    refused(f, BootstrapFailureClass::StorageLocked).await;
    f = facts();
    f.supervision = BootstrapSupervision::SystemdUser {
        version: 249,
        reachable: false,
    };
    refused(f, failure(PairingFailure::ServiceManagerUnavailable)).await;
    f = facts();
    f.linger = BootstrapLinger::Unavailable;
    refused(f, BootstrapFailureClass::LingerUnavailable).await;
}

struct ResponsePrompt {
    calls: Arc<AtomicUsize>,
    response: Vec<u8>,
}
impl ScopedAuthenticationPrompt for ResponsePrompt {
    fn revalidate(&self, _: &BootstrapPlan) -> Result<(), BootstrapFailure> {
        Ok(())
    }
    fn respond<'a>(
        &'a self,
        _: &'a BootstrapPlan,
    ) -> PairingFuture<'a, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Zeroizing::new(self.response.clone()))
        })
    }
}
#[tokio::test]
async fn foreground_authorization_bounds_prompt_retries_even_when_the_provider_requests_more() {
    let mut f = facts();
    f.linger = BootstrapLinger::Disabled;
    let (setup, _) = coordinator(f);
    setup.effects.prompt_attempts.store(4, Ordering::Release);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = BootstrapCoordinator::new(
        setup.clone(),
        Enrollment(setup.effects.clone()),
        ResponsePrompt {
            calls: calls.clone(),
            response: b"test-password".to_vec(),
        },
    );
    let result = c.bootstrap(intent(), None).await;
    assert_eq!(
        result.err().unwrap().class,
        failure(PairingFailure::LimitExceeded)
    );
    assert_eq!(calls.load(Ordering::Acquire), 3);
    assert_eq!(setup.effects.mutations.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn foreground_authorization_cannot_request_a_password_for_enabled_linger() {
    let (setup, _) = coordinator(facts());
    setup.effects.prompt_attempts.store(1, Ordering::Release);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = BootstrapCoordinator::new(
        setup.clone(),
        Enrollment(setup.effects.clone()),
        ResponsePrompt {
            calls: calls.clone(),
            response: b"test-password".to_vec(),
        },
    );
    assert_eq!(
        c.bootstrap(intent(), None).await.err().unwrap().class,
        failure(PairingFailure::AuthorizationDenied)
    );
    assert_eq!(calls.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn foreground_authorization_never_delivers_terminal_control_bytes() {
    let mut f = facts();
    f.linger = BootstrapLinger::Disabled;
    let (setup, _) = coordinator(f);
    setup.effects.prompt_attempts.store(1, Ordering::Release);
    let c = BootstrapCoordinator::new(
        setup.clone(),
        Enrollment(setup.effects.clone()),
        ResponsePrompt {
            calls: Arc::new(AtomicUsize::new(0)),
            response: b"password\ncommand".to_vec(),
        },
    );
    assert_eq!(
        c.bootstrap(intent(), None).await.err().unwrap().class,
        failure(PairingFailure::InvalidRequest)
    );
    assert_eq!(setup.effects.mutations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn foreground_preview_rejects_registration_and_prerequisite_substitution_before_authorization()
 {
    let original = facts();
    for changed in 0..5 {
        let mut current = original;
        match changed {
            0 => current.registration = BootstrapRegistration::Missing,
            1 => {
                current.registration = BootstrapRegistration::Stopped {
                    pin: pin(),
                    generation: 8,
                }
            }
            2 => current.storage = BootstrapPrerequisite::Unavailable,
            3 => current.linger = BootstrapLinger::Disabled,
            _ => {
                if let BootstrapRegistration::Running { ref mut pin, .. } = current.registration {
                    pin.public_key = [9; 32];
                }
            }
        }
        let (setup, coordinator) = coordinator(current);
        let intent = ForegroundBootstrap::confirm_observed(original, 1).unwrap();
        let failure = coordinator.bootstrap(intent, None).await.err().unwrap();
        assert_eq!(
            failure.class,
            BootstrapFailureClass::Pairing(PairingFailure::RouteChanged)
        );
        assert_eq!(setup.effects.authorizations.load(Ordering::SeqCst), 0);
        assert_eq!(setup.effects.mutations.load(Ordering::SeqCst), 0);
        assert_eq!(setup.effects.enrollments.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn unchanged_foreground_preview_reuses_headless_server_without_install_or_restart() {
    let original = facts();
    let (setup, coordinator) = coordinator(original);
    let intent = ForegroundBootstrap::confirm_observed(original, 1).unwrap();
    let result = coordinator.bootstrap(intent, None).await.unwrap();
    assert!(result.reused);
    assert_eq!(result.server.pin, pin());
    assert_eq!(setup.effects.installations.load(Ordering::SeqCst), 0);
    assert_eq!(setup.effects.enrollments.load(Ordering::SeqCst), 1);
}
