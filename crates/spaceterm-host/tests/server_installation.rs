use spaceterm_host::{
    installation::{propose_lifecycle, propose_staging},
    native_deployment::contracts::{DataGenerationId, NativeFailure, PublicationDisposition},
    native_registration::{
        RegistrationHeadV2, StartupBarrier, encode_registration_head,
        writer::{
            PublicationState, RegistrationHeadWriter, RegistrationPublicationStorage,
            SystemRegistrationStorage,
        },
    },
    startup::{AccountBuildKey, BuildIdentity, DesiredServerState, InstallationId},
};
use std::{
    num::NonZeroU64,
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Arc, Mutex},
};
fn nz(v: u64) -> NonZeroU64 {
    NonZeroU64::new(v).unwrap()
}
fn head() -> RegistrationHeadV2 {
    RegistrationHeadV2 {
        key: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        generation: nz(7),
        lifecycle_generation: nz(2),
        desired_state: DesiredServerState::Running,
        approved_installation: InstallationId(nz(11)),
        staged_installation: None,
        approved_data_generation: DataGenerationId::new(1).unwrap(),
        active_request: None,
        startup_barrier: StartupBarrier::Open,
        profile_sha256: [1; 32],
        approved_manifest_sha256: [2; 32],
        approved_descriptor_sha256: [3; 32],
        staged_descriptor_sha256: None,
        installation_high_water: 11,
        data_high_water: 1,
        server_id: [4; 16],
    }
}
#[derive(Clone)]
struct Storage {
    inner: Arc<Mutex<Disk>>,
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}
struct Disk {
    current: [u8; 256],
    fresh: Option<[u8; 256]>,
    failure: Option<usize>,
    trace: Vec<usize>,
}
impl Storage {
    fn new(failure: Option<usize>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Disk {
                current: encode_registration_head(&head()).unwrap(),
                fresh: None,
                failure,
                trace: vec![],
            })),
            calls: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn event(&self, number: usize) -> Result<(), NativeFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut d = self.inner.lock().unwrap();
        d.trace.push(number);
        if d.failure == Some(number) {
            Err(NativeFailure::IoUnavailable)
        } else {
            Ok(())
        }
    }
}
impl RegistrationPublicationStorage for Storage {
    fn read_current(&self, _: AccountBuildKey) -> Result<[u8; 256], NativeFailure> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(2));
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.event(0)?;
        Ok(self.inner.lock().unwrap().current)
    }
    fn write_fresh(&self, _: AccountBuildKey, bytes: &[u8; 256]) -> Result<(), NativeFailure> {
        self.inner.lock().unwrap().fresh = Some(*bytes);
        self.event(1)
    }
    fn sync_fresh(&self) -> Result<(), NativeFailure> {
        self.event(2)
    }
    fn replace_head(&self, _: AccountBuildKey) -> Result<(), NativeFailure> {
        self.event(3)?;
        let mut d = self.inner.lock().unwrap();
        d.current = d.fresh.take().unwrap();
        Ok(())
    }
    fn sync_parent(&self) -> Result<(), NativeFailure> {
        self.event(4)
    }
}

#[test]
fn file_or_directory_sync_failure_is_indeterminate() {
    for fault in [2, 4] {
        let storage = Storage::new(Some(fault));
        let writer = RegistrationHeadWriter::new(storage.clone());
        let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
        assert_eq!(
            writer.publish_proposal(&head(), &next),
            PublicationDisposition::Indeterminate
        );
        assert_eq!(writer.state(), PublicationState::RecoveryRequired);
        assert_eq!(
            storage.inner.lock().unwrap().current,
            if fault == 2 {
                encode_registration_head(&head()).unwrap()
            } else {
                encode_registration_head(&next).unwrap()
            }
        );
    }
}
#[test]
fn repeated_sync_does_not_clear_uncertain_durability() {
    let storage = Storage::new(Some(4));
    let writer = RegistrationHeadWriter::new(storage.clone());
    let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::Indeterminate
    );
    storage.inner.lock().unwrap().failure = None;
    storage.sync_parent().unwrap();
    let calls = storage.calls.load(Ordering::SeqCst);
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::NotApplied(NativeFailure::Indeterminate)
    );
    assert_eq!(storage.calls.load(Ordering::SeqCst), calls);
    assert_eq!(writer.state(), PublicationState::RecoveryRequired);
}
#[test]
fn publication_serializes_competing_installers() {
    let storage = Storage::new(None);
    let writer = Arc::new(RegistrationHeadWriter::new(storage.clone()));
    let mut threads = vec![];
    for _ in 0..8 {
        let w = Arc::clone(&writer);
        threads.push(std::thread::spawn(move || {
            let original = head();
            let next = propose_staging(&original, InstallationId(nz(12)), [9; 32]).unwrap();
            w.publish_proposal(&original, &next)
        }));
    }
    let outcomes: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == PublicationDisposition::Durable(()))
            .count(),
        1
    );
    assert_eq!(outcomes.iter().filter(|o| **o == PublicationDisposition::NotApplied(NativeFailure::GenerationMismatch)).count(), 7);
    assert_eq!(storage.max_active.load(Ordering::SeqCst), 1);
}
#[test]
fn publication_orders_fresh_file_sync_atomic_replace_and_parent_sync() {
    let storage = Storage::new(None);
    let writer = RegistrationHeadWriter::new(storage.clone());
    let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::Durable(())
    );
    assert_eq!(storage.inner.lock().unwrap().trace, vec![0, 1, 2, 3, 4]);
}
#[test]
fn error_after_entering_any_mutation_callback_is_uncertain() {
    for fault in 1..=4 {
        let storage = Storage::new(Some(fault));
        let writer = RegistrationHeadWriter::new(storage);
        let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
        assert_eq!(
            writer.publish_proposal(&head(), &next),
            PublicationDisposition::Indeterminate
        );
    }
}
#[test]
fn generation_exhaustion_and_decreasing_allocations_reject_before_storage() {
    let mut original = head();
    original.generation = nz(u64::MAX);
    assert_eq!(
        propose_lifecycle(&original, DesiredServerState::Stopped),
        Err(NativeFailure::LimitExceeded)
    );
    let storage = Storage::new(None);
    let writer = RegistrationHeadWriter::new(storage.clone());
    let mut next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
    next.data_high_water = 0;
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::NotApplied(NativeFailure::InvalidRecord)
    );
    assert_eq!(storage.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn staging_never_changes_approved_selection_or_running_intent() {
    let original = head();
    let next = propose_staging(&original, InstallationId(nz(12)), [9; 32]).unwrap();
    assert_eq!(next.approved_installation, original.approved_installation);
    assert_eq!(
        next.approved_data_generation,
        original.approved_data_generation
    );
    assert_eq!(next.lifecycle_generation, original.lifecycle_generation);
    assert_eq!(next.desired_state, original.desired_state);
}
#[test]
fn production_publication_never_adopts_sampled_root_ownership_as_custody() {
    let writer = RegistrationHeadWriter::new(SystemRegistrationStorage);
    let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::NotApplied(NativeFailure::CustodyUnavailable)
    );
    assert_eq!(writer.state(), PublicationState::Open);
}

#[test]
fn lifecycle_generation_cannot_skip_a_native_intent() {
    let storage = Storage::new(None);
    let writer = RegistrationHeadWriter::new(storage.clone());
    let mut next = propose_lifecycle(&head(), DesiredServerState::Running).unwrap();
    next.lifecycle_generation = nz(4);
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::NotApplied(NativeFailure::GenerationMismatch)
    );
    assert_eq!(storage.calls.load(Ordering::SeqCst), 0);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "native_authority/publication.rs"]
mod fresh_publication;

#[cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]
mod account_installation {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    use sha2::{Digest as _, Sha256};
    use spaceterm_distribution::*;
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture,
        server_installation::test_support,
        startup::{BuildIdentity, DesiredServerState},
    };

    fn fixture(
        version: &str,
        seed: u8,
        machine: u32,
    ) -> (UpdateKey, VerifiedAccountServerArtifact) {
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
            std::iter::once(("installation.json".to_owned(), 0o644, manifest.as_slice()))
                .chain(files)
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
                name: platform.archive_name(version),
                size: archive.len() as u64,
                sha256: Sha256::digest(&archive).into(),
                signature: signer.sign(&archive).as_ref().try_into().unwrap(),
                manifest_sha256: Sha256::digest(&manifest).into(),
            },
        };
        let bytes = serde_json::to_vec(&feed).unwrap();
        let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
        let feed =
            verify_account_server_feed(&key, build, platform, &bytes, signature.as_bytes(), 1)
                .unwrap();
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

    #[test]
    fn staging_does_not_activate_or_start_a_stopped_registration() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let id = store.stage(&artifact).unwrap();
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.staged, Some(id));
        assert_eq!(snapshot.approved, None);
        assert_eq!(registered.desired_state(), DesiredServerState::Stopped);
        let versions = registered.data_directory().join("installations");
        assert_eq!(
            std::fs::metadata(&versions).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let executable = versions
            .join(format!("version-{}", id.0.get()))
            .join(&artifact.manifest().entry_points.server);
        assert_eq!(
            std::fs::metadata(executable).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn activation_and_rollback_select_exact_verified_versions_and_retain_the_previous() {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let first_id = store.stage(&first).unwrap();
        store
            .activate(first_id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let second_id = store.stage(&second).unwrap();
        assert_eq!(store.retain_approved().unwrap().installation(), first_id);
        store
            .activate(second_id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.approved, Some(second_id));
        assert_eq!(snapshot.previous, Some(first_id));
        drop(store);
        let reopened = test_support::store(&registered, key).unwrap();
        reopened.rollback(snapshot.generation, 1).unwrap();
        assert_eq!(reopened.retain_approved().unwrap().installation(), first_id);
        assert_eq!(reopened.snapshot().unwrap().previous, Some(second_id));
        assert_eq!(
            account
                .reopen()
                .discover()
                .unwrap()
                .unwrap()
                .desired_state(),
            DesiredServerState::Stopped
        );
    }

    #[test]
    fn stale_selection_and_incompatible_storage_cannot_activate_a_candidate() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Running).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let old = store.snapshot().unwrap();
        let id = store.stage(&artifact).unwrap();
        assert_eq!(
            store.activate(id, old.generation, 1),
            Err(ServiceFailure::RequestConflict)
        );
        let staged = store.snapshot().unwrap();
        assert_eq!(
            store.activate(id, staged.generation, 2),
            Err(ServiceFailure::IncompatibleStorage)
        );
        assert_eq!(store.snapshot().unwrap(), staged);
    }

    #[test]
    fn actual_wrong_architecture_is_rejected_and_the_original_preparation_can_be_discarded() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, 0);
        let store = test_support::store(&registered, key.clone()).unwrap();
        assert_eq!(
            store.stage(&artifact),
            Err(ServiceFailure::ArtifactRejected)
        );
        drop(store);
        let reopened = test_support::store(&registered, key).unwrap();
        let snapshot = reopened.snapshot().unwrap();
        assert_eq!(snapshot.approved, None);
        let preparing = snapshot.preparing.unwrap();
        assert_eq!(reopened.stage(&artifact), Err(ServiceFailure::Pending));
        reopened
            .discard_preparing(preparing, snapshot.generation)
            .unwrap();
        reopened.cleanup().unwrap();
        assert!(reopened.snapshot().unwrap().cleanup_pending.is_none());
        assert!(
            !registered
                .data_directory()
                .join("installations")
                .join(format!("version-{}", preparing.0.get()))
                .exists()
        );
    }

    #[test]
    fn an_alternate_update_key_and_other_build_identity_cannot_publish_code() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let (_, other) = fixture("1.2.3", 8, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        assert_eq!(store.stage(&other), Err(ServiceFailure::ArtifactRejected));
        assert_eq!(
            spaceterm_host::server_installation::ServerInstallationStore::open(&registered)
                .unwrap()
                .stage(&artifact),
            Err(ServiceFailure::ArtifactRejected)
        );
        for build in [BuildIdentity::Release, BuildIdentity::Preflight] {
            let account = AccountServiceFixture::new(root.path(), build).unwrap();
            let registered = account.initialize(DesiredServerState::Stopped).unwrap();
            let store = test_support::store(&registered, key.clone()).unwrap();
            assert_eq!(
                store.stage(&artifact),
                Err(ServiceFailure::ArtifactRejected)
            );
        }
    }

    #[test]
    fn startup_rejects_tampered_or_linked_installed_code() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let retained = store.retain_approved().unwrap();
        let path = retained.path().to_owned();
        let original = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"tampered").unwrap();
        assert!(store.retain_approved().is_err());
        std::fs::write(&path, original).unwrap();
        let other = path.with_extension("link-target");
        std::fs::rename(&path, &other).unwrap();
        symlink(&other, &path).unwrap();
        assert!(store.retain_approved().is_err());
    }

    #[test]
    fn cleanup_removes_only_the_superseded_version_and_preserves_private_account_data() {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let (_, third) = fixture("1.2.5", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let mut ids = Vec::new();
        for artifact in [&first, &second, &third] {
            let id = store.stage(artifact).unwrap();
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
            ids.push(id);
        }
        let versions = registered.data_directory().join("installations");
        let old = versions.join(format!("version-{}", ids[0].0.get()));
        let unlisted = old.join("unlisted");
        std::fs::write(&unlisted, b"do not delete").unwrap();
        assert!(store.cleanup().is_err());
        assert!(unlisted.exists());
        assert_eq!(store.snapshot().unwrap().cleanup_pending, Some(ids[0]));
        std::fs::remove_file(unlisted).unwrap();
        store.cleanup().unwrap();
        assert!(!old.exists());
        assert!(
            !versions
                .join(format!("metadata-{}", ids[0].0.get()))
                .exists()
        );
        assert_eq!(store.snapshot().unwrap().previous, Some(ids[1]));
        assert_eq!(store.retain_approved().unwrap().installation(), ids[2]);
        assert_eq!(
            account.reopen().discover().unwrap().unwrap().server_pin(),
            registered.server_pin()
        );
    }

    #[test]
    fn activation_sync_failure_closes_retained_and_reopened_authority_without_rollback_guessing() {
        use spaceterm_host::account_service::{
            ServiceFailure, test_support::fail_next_publication_durability,
        };
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        let generation = store.snapshot().unwrap().generation;
        fail_next_publication_durability();
        assert_eq!(
            store.activate(id, generation, 1),
            Err(ServiceFailure::Indeterminate)
        );
        assert_eq!(store.snapshot(), Err(ServiceFailure::Indeterminate));
        assert!(test_support::store(&registered, key).is_err());
        assert!(
            registered
                .data_directory()
                .join("installations")
                .join(format!("version-{}", id.0.get()))
                .exists()
        );
    }

    #[test]
    fn activation_process_fixture() {
        let Ok(root) = std::env::var("SPACETERM_INSTALLATION_CRASH_ROOT") else {
            return;
        };
        let account =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let registered = account.reopen().discover().unwrap().unwrap();
        let (key, _) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let snapshot = store.snapshot().unwrap();
        test_support::after_next_publication(|| std::process::exit(86));
        store
            .activate(snapshot.staged.unwrap(), snapshot.generation, 1)
            .unwrap();
        panic!("activation interruption did not fire");
    }

    #[test]
    fn process_exit_during_activation_keeps_every_version_and_blocks_uncheckpointed_selection() {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::activation_process_fixture",
                "--nocapture",
            ])
            .env("SPACETERM_INSTALLATION_CRASH_ROOT", root.path());
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(86));
        assert!(store.snapshot().is_err());
        assert!(test_support::store(&registered, key).is_err());
        assert!(
            registered
                .data_directory()
                .join("installations")
                .join(format!("version-{}", id.0.get()))
                .exists()
        );
    }
    #[test]
    fn unlisted_files_and_replaced_version_directories_never_become_startup_authority() {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let path = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", id.0.get()));
        let extra = path.join("unlisted");
        std::fs::write(&extra, b"unexpected executable resource").unwrap();
        for _ in 0..2 {
            assert!(store.retain_approved().is_err());
        }
        std::fs::remove_file(extra).unwrap();
        assert_eq!(
            store.retain_approved().unwrap().status().artifact_sha256,
            artifact.artifact_sha256()
        );
        let displaced = path.with_extension("displaced");
        std::fs::rename(&path, &displaced).unwrap();
        std::fs::create_dir(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(store.retain_approved().is_err());
        assert!(displaced.exists());
    }
    #[test]
    fn cleanup_never_deletes_substituted_payload_bytes() {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let (_, third) = fixture("1.2.5", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let mut first_id = None;
        for artifact in [&first, &second, &third] {
            let id = store.stage(artifact).unwrap();
            first_id.get_or_insert(id);
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
        }
        let original = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", first_id.unwrap().0.get()))
            .join(&first.manifest().entry_points.server);
        std::fs::write(&original, b"unverified replacement").unwrap();
        assert!(store.cleanup().is_err());
        assert_eq!(std::fs::read(original).unwrap(), b"unverified replacement");
        assert!(store.snapshot().unwrap().cleanup_pending.is_some());
    }
    #[test]
    fn an_unsettled_preparation_blocks_new_selection_until_original_discard() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let (_, bad) = fixture("1.2.5", 7, 0);
        let store = test_support::store(&registered, key).unwrap();
        for artifact in [&first, &second] {
            let id = store.stage(artifact).unwrap();
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
        }
        assert_eq!(store.stage(&bad), Err(ServiceFailure::ArtifactRejected));
        let pending = store.snapshot().unwrap();
        assert_eq!(
            store.rollback(pending.generation, 1),
            Err(ServiceFailure::Pending)
        );
        assert_eq!(store.snapshot().unwrap(), pending);
        store
            .discard_preparing(pending.preparing.unwrap(), pending.generation)
            .unwrap();
        store.cleanup().unwrap();
        store
            .rollback(store.snapshot().unwrap().generation, 1)
            .unwrap();
        assert_eq!(store.snapshot().unwrap().approved, pending.previous);
    }
    #[test]
    fn an_unrelated_child_cannot_retain_a_released_installer_lock() {
        use std::{
            io::Write,
            os::{fd::AsRawFd, unix::net::UnixStream},
        };
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, _) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let (mut control, parked) = UnixStream::pair().unwrap();
        let control_fd = control.as_raw_fd();
        let parked_fd = parked.as_raw_fd();
        let child = test_support::with_installation_lock(&store, || {
            // SAFETY: the child uses only async-signal-safe libc calls and never touches Rust runtime state.
            let pid = unsafe { libc::fork() };
            if pid == 0 {
                let mut byte = 0_u8;
                // SAFETY: the child owns these inherited descriptors and exits after one bounded parent-controlled read.
                unsafe {
                    libc::close(control_fd);
                    libc::read(parked_fd, (&mut byte as *mut u8).cast(), 1);
                    libc::_exit(0);
                }
            }
            pid
        })
        .unwrap();
        drop(parked);
        let snapshot = store.snapshot();
        let release = control.write_all(b"x");
        drop(control);
        let mut status = 0;
        // SAFETY: this exact child belongs to the test and releases its inherited descriptors before exiting.
        let reaped = if child > 0 {
            unsafe { libc::waitpid(child, &mut status, 0) }
        } else {
            -1
        };
        assert!(child > 0);
        assert_eq!(reaped, child);
        assert_eq!(status, 0);
        release.unwrap();
        assert!(
            snapshot.is_ok(),
            "released lock was retained by the unrelated child: {snapshot:?}"
        );
    }

    fn partial_extraction_keeps_cleanup_closed(process_exit: bool) {
        use spaceterm_host::account_service::ServiceFailure;
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        if process_exit {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "account_installation::preparation_process_fixture",
                ])
                .env("SPACETERM_PREPARATION_CRASH_ROOT", root.path())
                .env("SPACETERM_PREPARATION_CRASH_PHASE", "copy");
            let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(86));
        } else {
            test_support::after_next_partial_copy(1, || {});
            assert_eq!(
                store.stage(&artifact),
                Err(ServiceFailure::StorageUnavailable)
            );
        }
        drop(store);
        let store = test_support::store(&registered, key).unwrap();
        let snapshot = store.snapshot().unwrap();
        let id = snapshot.preparing.unwrap();
        let payload = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", id.0.get()))
            .join(&artifact.manifest().entry_points.server);
        assert_eq!(std::fs::metadata(&payload).unwrap().len(), 1);
        store.discard_preparing(id, snapshot.generation).unwrap();
        // No original-phase file-creation capability exists on this base. The
        // complete manifest must not be weakened to authorize partial copies.
        assert_eq!(store.cleanup(), Err(ServiceFailure::ArtifactRejected));
        assert_eq!(std::fs::metadata(&payload).unwrap().len(), 1);
        std::fs::write(&payload, b"unverified replacement").unwrap();
        assert_eq!(store.cleanup(), Err(ServiceFailure::ArtifactRejected));
        assert_eq!(std::fs::read(payload).unwrap(), b"unverified replacement");
        assert_eq!(
            store.stage(&artifact),
            Err(ServiceFailure::CleanupUnresolved)
        );
        assert_eq!(store.snapshot().unwrap().cleanup_pending, Some(id));
        assert!(store.snapshot().unwrap().approved.is_none());
    }

    #[test]
    fn a_failed_partial_copy_never_authorizes_deleting_unverified_contents() {
        partial_extraction_keeps_cleanup_closed(false);
    }

    #[test]
    fn a_terminated_partial_copy_never_authorizes_deleting_unverified_contents() {
        partial_extraction_keeps_cleanup_closed(true);
    }

    #[test]
    fn preparation_process_fixture() {
        let Ok(root) = std::env::var("SPACETERM_PREPARATION_CRASH_ROOT") else {
            return;
        };
        let account =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let registered = account.reopen().discover().unwrap().unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        match std::env::var("SPACETERM_PREPARATION_CRASH_PHASE")
            .unwrap()
            .as_str()
        {
            "mkdir" => test_support::after_next_version_creation(|| std::process::exit(86)),
            "metadata" => test_support::after_next_metadata_publication(|| std::process::exit(86)),
            "copy" => test_support::after_next_partial_copy(1, || std::process::exit(86)),
            _ => panic!("unknown preparation interruption"),
        }
        store.stage(&artifact).unwrap();
        panic!("preparation interruption did not fire");
    }

    fn preparation_is_retained_before_external_effect(phase: &str) {
        use spaceterm_host::account_service::ServiceFailure;
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::preparation_process_fixture",
            ])
            .env("SPACETERM_PREPARATION_CRASH_ROOT", root.path())
            .env("SPACETERM_PREPARATION_CRASH_PHASE", phase);
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(86));
        let reopened = test_support::store(&registered, key).unwrap();
        let snapshot = reopened.snapshot().unwrap();
        let id = snapshot
            .preparing
            .expect("native creation lost its durable candidate reservation");
        assert!(snapshot.approved.is_none());
        assert!(snapshot.staged.is_none());
        assert_eq!(reopened.stage(&artifact), Err(ServiceFailure::Pending));
        assert_eq!(store.snapshot().unwrap(), snapshot);
        let version = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", id.0.get()));
        assert!(version.is_dir());
        replace_from_other_process(&version, true);
        assert_eq!(
            reopened.discard_preparing(id, snapshot.generation),
            Err(ServiceFailure::CleanupUnresolved)
        );
        assert!(version.is_dir());
        assert!(version.with_extension("displaced").is_dir());
        assert_eq!(reopened.snapshot().unwrap(), snapshot);
    }

    #[test]
    fn preparation_is_durably_reserved_before_version_mkdir() {
        preparation_is_retained_before_external_effect("mkdir");
    }

    #[test]
    fn preparation_is_durably_reserved_before_metadata_publication() {
        preparation_is_retained_before_external_effect("metadata");
    }

    #[test]
    fn cleanup_replacement_process_fixture() {
        let Ok(original) = std::env::var("SPACETERM_CLEANUP_REPLACEMENT_PATH") else {
            return;
        };
        let original = std::path::PathBuf::from(original);
        let replacement = original.with_extension("replacement");
        let displaced = original.with_extension("displaced");
        std::fs::rename(&original, displaced).unwrap();
        if std::env::var_os("SPACETERM_CLEANUP_REPLACE_DIRECTORY").is_some() {
            std::fs::create_dir(&replacement).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o700)).unwrap();
        } else {
            std::fs::write(&replacement, b"unrelated replacement").unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mode = if std::env::var_os("SPACETERM_CLEANUP_REPLACE_METADATA").is_some() {
                0o600
            } else {
                0o700
            };
            std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        std::fs::rename(replacement, &original).unwrap();
    }

    fn replace_from_other_process(path: &std::path::Path, directory: bool) {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::cleanup_replacement_process_fixture",
            ])
            .env("SPACETERM_CLEANUP_REPLACEMENT_PATH", path);
        if directory {
            command.env("SPACETERM_CLEANUP_REPLACE_DIRECTORY", "1");
        }
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert!(child.wait().unwrap().success());
    }

    fn cleanup_replacement_survives(directory: bool) {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let (_, third) = fixture("1.2.5", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let mut first_id = None;
        for artifact in [&first, &second, &third] {
            let id = store.stage(artifact).unwrap();
            first_id.get_or_insert(id);
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
        }
        let version = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", first_id.unwrap().0.get()));
        let original = if directory {
            version
        } else {
            version.join(&first.manifest().entry_points.server)
        };
        let hook_path = original.clone();
        if directory {
            test_support::before_next_version_removal(move || {
                replace_from_other_process(&hook_path, true)
            });
        } else {
            test_support::before_next_file_removal(move || {
                replace_from_other_process(&hook_path, false)
            });
        }
        let result = store.cleanup();
        assert!(
            original.exists(),
            "cleanup deleted a replacement after its final identity check"
        );
        if !directory {
            assert_eq!(std::fs::read(original).unwrap(), b"unrelated replacement");
        }
        assert!(result.is_err());
        assert_eq!(store.snapshot().unwrap().cleanup_pending, first_id);
    }

    fn replace_metadata_from_other_process(path: &std::path::Path) {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::cleanup_replacement_process_fixture",
            ])
            .env("SPACETERM_CLEANUP_REPLACEMENT_PATH", path)
            .env("SPACETERM_CLEANUP_REPLACE_METADATA", "1");
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn metadata_cleanup_process_fixture() {
        let Ok(root) = std::env::var("SPACETERM_METADATA_CLEANUP_CRASH_ROOT") else {
            return;
        };
        let account =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let registered = account.reopen().discover().unwrap().unwrap();
        let (key, _) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        test_support::after_next_version_removal(|| std::process::exit(86));
        store.cleanup().unwrap();
        panic!("metadata cleanup interruption did not fire");
    }

    fn cleanup_preserves_replaced_metadata(version_already_removed: bool) {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .discard_staged(id, store.snapshot().unwrap().generation)
            .unwrap();
        let directory = registered.data_directory().join("installations");
        let metadata = directory.join(format!("metadata-{}", id.0.get()));
        let version = directory.join(format!("version-{}", id.0.get()));
        if version_already_removed {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "account_installation::metadata_cleanup_process_fixture",
                ])
                .env("SPACETERM_METADATA_CLEANUP_CRASH_ROOT", root.path());
            let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(86));
            assert!(!version.exists());
            replace_metadata_from_other_process(&metadata);
        } else {
            let path = metadata.clone();
            test_support::before_next_version_removal(move || {
                replace_metadata_from_other_process(&path)
            });
        }
        let result = store.cleanup();
        assert!(
            metadata.exists(),
            "cleanup deleted unauthenticated metadata"
        );
        assert_eq!(std::fs::read(&metadata).unwrap(), b"unrelated replacement");
        assert!(result.is_err());
        assert_eq!(store.snapshot().unwrap().cleanup_pending, Some(id));
        assert!(!version.exists());
        drop(store);
        let reopened = test_support::store(&registered, key).unwrap();
        assert!(reopened.cleanup().is_err());
        assert_eq!(std::fs::read(metadata).unwrap(), b"unrelated replacement");
        assert_eq!(reopened.snapshot().unwrap().cleanup_pending, Some(id));
    }

    #[test]
    fn cleanup_preserves_metadata_replaced_after_manifest_authentication() {
        cleanup_preserves_replaced_metadata(false);
    }

    #[test]
    fn cleanup_authenticates_metadata_after_interrupted_version_removal() {
        cleanup_preserves_replaced_metadata(true);
    }

    #[test]
    fn cleanup_preserves_a_leaf_replaced_after_the_final_identity_check() {
        cleanup_replacement_survives(false);
    }

    #[test]
    fn cleanup_preserves_a_version_replaced_after_the_final_identity_check() {
        cleanup_replacement_survives(true);
    }
}
