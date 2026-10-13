// Reuse the signed native artifact shape from the account installation suite.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::*;
use spaceterm_host::{
    account_service::{test_support::AccountServiceFixture, *},
    startup::{BuildIdentity, DesiredServerState},
};
use spaceterm_server::deployment::setup::{SetupRequest, account_setup, prepare_confirmed_setup};
use std::sync::Arc;

fn fixture(version: &str, seed: u8, machine: u32) -> (UpdateKey, VerifiedAccountServerArtifact) {
    let build = BuildChannel::Development;
    let platform = if cfg!(target_os = "macos") {
        ServerPlatform::DarwinArm64
    } else {
        ServerPlatform::LinuxX86_64
    };
    let signer = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap();
    let key = UpdateKey::from_base64(&STANDARD.encode(signer.public_key().as_ref())).unwrap();
    let mut payload = vec![0; 64];
    if platform == ServerPlatform::DarwinArm64 {
        payload[..4].copy_from_slice(&0xfeedfacf_u32.to_le_bytes());
        payload[4..8].copy_from_slice(&machine.to_le_bytes());
        payload[12..16].copy_from_slice(&2_u32.to_le_bytes());
    } else {
        payload[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        payload[16..18].copy_from_slice(&2_u16.to_le_bytes());
        payload[18..20].copy_from_slice(&(machine as u16).to_le_bytes());
    }
    payload[63] = version.as_bytes()[version.len() - 1];
    let name = match platform {
        ServerPlatform::LinuxX86_64 => "bin/spaceterm-server".to_owned(),
        ServerPlatform::DarwinArm64 => {
            format!("{}/Contents/MacOS/spaceterm-server", build.bundle_name())
        }
    };
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
    let mut tar = tar::Builder::new(Vec::new());
    for (name, mode, bytes) in
        std::iter::once(("installation.json".to_owned(), 0o644, manifest.as_slice())).chain(files)
    {
        let mut header = tar::Header::new_ustar();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append_data(&mut header, format!("spaceterm-server/{name}"), bytes)
            .unwrap();
    }
    let tar = tar.into_inner().unwrap();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gzip, &tar).unwrap();
    let archive = gzip.finish().unwrap();
    let feed = AccountServerFeedV2 {
        schema_version: 2,
        component: ServerComponent::Server,
        build_identity: build,
        platform,
        version: BuildVersion::parse(version).unwrap(),
        published_at: 1,
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
            name: platform.account_archive_name(build, version),
            size: archive.len() as u64,
            sha256: Sha256::digest(&archive).into(),
            signature: signer.sign(&archive).as_ref().try_into().unwrap(),
            manifest_sha256: Sha256::digest(&manifest).into(),
        },
    };
    let bytes = serde_json::to_vec(&feed).unwrap();
    let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
    let feed =
        verify_account_server_feed(&key, build, platform, &bytes, signature.as_bytes(), 1).unwrap();
    let artifact = ServerArchiveVerifier::new()
        .verify_account_owned(&key, feed, archive)
        .unwrap();
    (key, artifact)
}
fn native_machine() -> u32 {
    if cfg!(target_os = "macos") {
        0x0100000c
    } else {
        62
    }
}

#[tokio::test]
async fn account_setup_requires_verified_artifact_and_retained_native_request() {
    use spaceterm_host::native_deployment::contracts::NativeRequestId;
    let root = tempfile::tempdir().unwrap();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let supervisor = account.reopen();
    let request = supervisor.allocate_request().unwrap();
    let (_key, artifact) = fixture("1.2.3", 91, native_machine());
    let before = std::fs::read(account.anchor().join("requests")).unwrap();
    // A test-authenticated artifact cannot cross the production trust boundary.
    let feed = root.path().join("feed");
    let signature = root.path().join("signature");
    let archive = root.path().join("archive");
    std::fs::write(&feed, artifact.feed().bytes()).unwrap();
    std::fs::write(&signature, artifact.feed().signature()).unwrap();
    std::fs::write(&archive, artifact.bytes()).unwrap();
    assert_eq!(
        prepare_confirmed_setup(
            &supervisor,
            &SetupRequest {
                request,
                feed,
                signature,
                archive
            }
        )
        .err(),
        Some(ServiceFailure::ArtifactRejected)
    );
    assert!(supervisor.discover().unwrap().is_none());
    assert!(!account.data_directory().exists());
    assert_eq!(
        std::fs::read(account.anchor().join("requests")).unwrap(),
        before
    );
    let unreserved = NativeRequestId::new([99; 16]).unwrap();
    assert_eq!(
        account_setup(&supervisor, unreserved, account.confirm_install(artifact))
            .await
            .err(),
        Some(ServiceFailure::UnknownRequest)
    );
    assert!(!account.data_directory().exists());
    assert_eq!(
        std::fs::read(account.anchor().join("requests")).unwrap(),
        before
    );

    // Isolated test composition supplies the real signed artifact and a retained
    // installed account. Reuse must not invoke a native manager or change Stop.
    let (_key, artifact) = fixture("1.2.3", 91, native_machine());
    let installation = InstallationStatus {
        installation: spaceterm_host::startup::InstallationId(
            std::num::NonZeroU64::new(1).unwrap(),
        ),
        version: artifact.manifest().version.clone(),
        artifact_sha256: artifact.artifact_sha256(),
    };
    let registration = account
        .initialize_with_installation(DesiredServerState::Stopped, Some(installation.clone()))
        .unwrap();
    let pin = registration.server_pin();
    let generation = registration.generation();
    let result = account_setup(&supervisor, request, account.confirm_install(artifact))
        .await
        .unwrap();
    assert_eq!(result.request, request);
    assert_eq!(
        result.state,
        ServiceRequestState::Completed(ServiceOutcome::Installed {
            server: pin,
            installation,
            reused: true
        })
    );
    let (_, artifact) = fixture("1.2.3", 91, native_machine());
    let replay = account_setup(
        &Arc::new(account.reopen()),
        request,
        account.confirm_install(artifact),
    )
    .await
    .unwrap();
    assert_eq!(replay, result);
    let current = supervisor.discover().unwrap().unwrap();
    assert_eq!(current.desired_state(), DesiredServerState::Stopped);
    assert_eq!(current.server_pin(), pin);
    assert_eq!(current.generation(), generation);
    let (_, changed) = fixture("1.2.4", 92, native_machine());
    assert_eq!(
        account_setup(&supervisor, request, account.confirm_install(changed))
            .await
            .err(),
        Some(ServiceFailure::RequestConflict)
    );
}

#[tokio::test]
async fn account_native_status_authenticates_live_approved_and_staged_code_without_mutation() {
    use spaceterm_core::{identity::ServerId, management::*};
    use spaceterm_server::{
        account_startup::test_support,
        listeners::{AccountNativeStatusObserver, NativeStatusObserver},
    };
    let root = tempfile::tempdir().unwrap();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let (key, artifact) = fixture("1.2.3", 91, native_machine());
    let approved = InstallationStatus {
        installation: spaceterm_host::startup::InstallationId(std::num::NonZeroU64::MIN),
        version: artifact.manifest().version.clone(),
        artifact_sha256: artifact.artifact_sha256(),
    };
    let registration = account
        .initialize_with_installation(DesiredServerState::Stopped, Some(approved.clone()))
        .unwrap();
    let store =
        spaceterm_host::server_installation::test_support::store(&registration, key.clone())
            .unwrap();
    let id = store.stage(&artifact).unwrap();
    assert_eq!(id, approved.installation);
    store
        .activate(
            id,
            store.snapshot().unwrap().generation,
            spaceterm_host::organization_store::schema::SCHEMA_VERSION,
        )
        .unwrap();
    let (_, staged) = fixture("1.2.4", 91, native_machine());
    let staged_id = store.stage(&staged).unwrap();
    account
        .stage_installation(InstallationStatus {
            installation: staged_id,
            version: staged.manifest().version.clone(),
            artifact_sha256: staged.artifact_sha256(),
        })
        .unwrap();
    let supervisor =
        test_support::supervisor(root.path(), Arc::new(test_support::StartupAbsence)).unwrap();
    let supervisor = Arc::new(account.with_installation_trust((*supervisor).clone(), key));
    let observer = AccountNativeStatusObserver::new(supervisor.clone());
    let before = std::fs::read(account.anchor().join("registration")).unwrap();
    let status = observer
        .observe(ServerId::new(registration.server_pin().identity))
        .await
        .unwrap();
    assert_eq!(status.verification, NativeVerification::Verified);
    assert_eq!(status.approved.as_ref().unwrap().as_str(), "1.2.3");
    assert_eq!(status.staged.as_ref().unwrap().as_str(), "1.2.4");
    assert!(
        status
            .capabilities
            .iter()
            .all(|value| value.availability == CapabilityAvailability::Available)
    );
    assert!(std::fs::read(account.anchor().join("registration")).unwrap() == before);
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
    assert!(!registration.endpoint().unwrap().path().exists());
    let request = supervisor.allocate_request().unwrap();
    spaceterm_host::account_service::test_support::interrupt_after_phase(ServicePhase::Accepted);
    assert_eq!(
        supervisor.start(request).await.err(),
        Some(ServiceFailure::SupervisorUnavailable)
    );
    let journal_before = std::fs::read(account.anchor().join("requests")).unwrap();
    let pending = observer
        .observe(ServerId::new(registration.server_pin().identity))
        .await
        .unwrap();
    assert_eq!(
        pending.progress[0].request,
        ManagementRequestReference::Native(*request.as_bytes())
    );
    assert_eq!(pending.progress[0].state, ManagementProgressState::Pending);
    assert!(std::fs::read(account.anchor().join("requests")).unwrap() == journal_before);
    assert!(std::fs::read(account.anchor().join("registration")).unwrap() == before);
    let executable = store.retain_approved().unwrap();
    std::fs::write(executable.path(), b"corrupted approved artifact").unwrap();
    let failed = observer
        .observe(ServerId::new(registration.server_pin().identity))
        .await
        .unwrap();
    assert_eq!(failed.verification, NativeVerification::Unverified);
    assert!(failed.approved.is_none());
    assert!(failed.capabilities.iter().any(|value| value.capability
        == CAPABILITY_SIGNED_SERVER_INSTALLATION
        && value.availability == CapabilityAvailability::Unavailable));
    assert!(std::fs::read(account.anchor().join("registration")).unwrap() == before);
}
