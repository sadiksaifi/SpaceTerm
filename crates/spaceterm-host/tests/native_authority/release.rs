use sha2::{Digest, Sha256};
use spaceterm_host::{
    activation_journal::{
        ActivationJournalRecord, ActivationJournalState as Phase, NativeOperationKind,
    },
    approved_installation::ApprovedInstallationDescriptor,
    native_deployment::{
        account::{AccountBinding, NativeAccountId},
        authority::*,
        contracts::*,
        profile::*,
        publication::*,
    },
    native_registration::{RegistrationHeadV2, StartupBarrier, encode_registration_head},
    protected_directory::{NativeDirectoryProtection, ProtectedDirectoryError},
    startup::{DesiredServerState, PendingStartupContext, ProcessIdentity, StartupSnapshot},
};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    num::NonZeroU64,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[expect(
    dead_code,
    reason = "the shared archive builder also supplies the publisher fault matrix"
)]
mod archive_fixture;

fn key() -> AccountBuildKey {
    AccountBuildKey {
        account_uid: 501,
        build_identity: BuildIdentity::Release,
    }
}
fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap()
}
fn account() -> AccountBinding {
    AccountBinding::new(key(), 20, NativeAccountId::macos([1; 16]).unwrap()).unwrap()
}
fn profile() -> RegistrationProfile {
    let roots = RegisteredRoots::new(
        "/original/config".into(),
        "/original/data".into(),
        "/original/state".into(),
        "/original/cache".into(),
    )
    .unwrap();
    RegistrationProfile::new(
        key(),
        20,
        [1; 16],
        [4; 16],
        roots,
        BootCredentialBinding::macos([2; 32], [3; 32], account().sha256(), [4; 16]).unwrap(),
        StorageBinding::new(vec![5; 16]).unwrap(),
    )
    .unwrap()
}
fn descriptor() -> ApprovedInstallationDescriptor {
    ApprovedInstallationDescriptor {
        schema_version: 1,
        key: key(),
        installation: InstallationId(nz(11)),
        server_id: [4; 16],
        account_binding_sha256: account().sha256(),
        registration_profile_sha256: Sha256::digest(profile().encode().unwrap()).into(),
        publication_nonce: [13; 32],
        archive_sha256: [14; 32],
        manifest_sha256: [6; 32],
        server_artifact_sha256: [11; 32],
        execution_closure_sha256: [15; 32],
        launcher_abi: 2,
    }
}
fn head() -> RegistrationHeadV2 {
    RegistrationHeadV2 {
        key: key(),
        generation: nz(7),
        lifecycle_generation: nz(2),
        desired_state: DesiredServerState::Running,
        approved_installation: InstallationId(nz(11)),
        staged_installation: Some(InstallationId(nz(12))),
        approved_data_generation: DataGenerationId::new(1).unwrap(),
        active_request: None,
        startup_barrier: StartupBarrier::Open,
        profile_sha256: Sha256::digest(profile().encode().unwrap()).into(),
        approved_manifest_sha256: [6; 32],
        approved_descriptor_sha256: Sha256::digest(descriptor().encode().unwrap()).into(),
        staged_descriptor_sha256: Some([8; 32]),
        installation_high_water: 12,
        data_high_water: 1,
        server_id: [4; 16],
    }
}
fn journal() -> ActivationJournalRecord {
    let h = head();
    ActivationJournalRecord {
        schema_version: 1,
        request: NativeRequestId::new([1; 16]).unwrap(),
        operation: NativeOperationKind::Stop,
        key: key(),
        server_id: h.server_id,
        account_binding_sha256: account().sha256(),
        expected_registration_generation: h.generation,
        expected_lifecycle_generation: h.lifecycle_generation,
        original_installation: h.approved_installation,
        original_data_generation: h.approved_data_generation,
        candidate_input: None,
        candidate_installation: None,
        candidate_manifest_sha256: None,
        candidate_data_generation: None,
        desired_state: DesiredServerState::Stopped,
        assessment_digest: Some([9; 32]),
        state: Phase::ActivationPrepared,
        outcome: None,
    }
}
fn context() -> PendingStartupContext {
    let h = head();
    PendingStartupContext {
        key: key(),
        primary_gid: 20,
        registration_generation: h.generation,
        lifecycle_generation: h.lifecycle_generation,
        data_generation: h.approved_data_generation,
        installation: h.approved_installation,
        server_id: h.server_id,
        account_binding_sha256: account().sha256(),
        installation_binding: descriptor().installation_binding().unwrap(),
        server_artifact_sha256: [11; 32],
        manifest_sha256: h.approved_manifest_sha256,
        credential_binding_sha256: profile().boot_credential().sha256().unwrap(),
        startup_nonce: [12; 32],
        original_process: ProcessIdentity {
            pid: 101,
            start_seconds: 1,
            start_microseconds: 0,
        },
        retriever_process: ProcessIdentity {
            pid: 102,
            start_seconds: 1,
            start_microseconds: 1,
        },
        journal_request: None,
    }
}
struct Controls {
    revoked: AtomicBool,
    process_changed: AtomicBool,
    custody_changed: AtomicBool,
    storage: Mutex<Option<NativeFailure>>,
    account: Mutex<AccountBinding>,
    covered: Mutex<std::collections::BTreeSet<(u64, u64)>>,
    retrievals: AtomicUsize,
    deliveries: AtomicUsize,
}
struct Protection(Arc<Controls>);
impl NativeDirectoryProtection for Protection {
    fn verify_directory(&self, _: &File) -> Result<(), ProtectedDirectoryError> {
        Ok(())
    }
    fn verify_regular_file(&self, _: &File) -> Result<(), ProtectedDirectoryError> {
        Ok(())
    }
}
impl NativePublicationProtection for Protection {
    fn record_fresh(&self, inode: &FreshPublicationInode<'_>) -> Result<(), NativeFailure> {
        self.verify_custody(&[inode.parent()])?;
        let metadata = inode.destination().metadata().unwrap();
        self.0
            .covered
            .lock()
            .unwrap()
            .insert((metadata.dev(), metadata.ino()));
        Ok(())
    }
    fn verify_custody(&self, files: &[&File]) -> Result<(), NativeFailure> {
        if self.0.custody_changed.load(Ordering::SeqCst) {
            return Err(NativeFailure::CustodyUnavailable);
        }
        let covered = self.0.covered.lock().unwrap();
        for file in files {
            let m = file.metadata().unwrap();
            if !covered.contains(&(m.dev(), m.ino())) {
                return Err(NativeFailure::CustodyUnavailable);
            }
        }
        Ok(())
    }
    fn authenticate_execution(
        &self,
        _: &spaceterm_distribution::InstallationManifestV1,
        _: &[PublicationFile<'_>],
    ) -> Result<[u8; 32], NativeFailure> {
        Ok([15; 32])
    }
}
struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    controls: Arc<Controls>,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("locks")).unwrap();
        for name in ["publisher.lock", "release-501.lock"] {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join("locks").join(name))
                .unwrap();
        }
        let controls = Arc::new(Controls {
            revoked: AtomicBool::new(false),
            process_changed: AtomicBool::new(false),
            custody_changed: AtomicBool::new(false),
            storage: Mutex::new(None),
            account: Mutex::new(account()),
            covered: Mutex::new(Default::default()),
            retrievals: AtomicUsize::new(0),
            deliveries: AtomicUsize::new(0),
        });
        let fixture = Self {
            _directory: directory,
            root,
            controls,
        };
        fixture.write(
            "release-501.registration",
            &encode_registration_head(&head()).unwrap(),
        );
        fixture.write("release-501.journal", &journal().encode().unwrap());
        let profiles = fixture.root.join("release-501.profiles");
        std::fs::create_dir(&profiles).unwrap();
        let digest: String = head()
            .profile_sha256
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        fixture.write(
            &format!("release-501.profiles/{digest}.json"),
            &profile().encode().unwrap(),
        );
        let mut covered = fixture.controls.covered.lock().unwrap();
        for p in fixture.root.ancestors() {
            let m = File::open(p).unwrap().metadata().unwrap();
            covered.insert((m.dev(), m.ino()));
        }
        for p in [
            fixture.root.join("locks"),
            fixture.root.join("locks/publisher.lock"),
            fixture.root.join("locks/release-501.lock"),
            profiles,
        ] {
            let m = File::open(p).unwrap().metadata().unwrap();
            covered.insert((m.dev(), m.ino()));
        }
        drop(covered);
        fixture
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.root.join(name);
        let temporary = path.with_extension("fixture-pending");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        let m = file.metadata().unwrap();
        self.controls
            .covered
            .lock()
            .unwrap()
            .insert((m.dev(), m.ino()));
        drop(file);
        std::fs::rename(temporary, &path).unwrap();
        File::open(path.parent().unwrap())
            .unwrap()
            .sync_all()
            .unwrap();
    }
    fn store(&self) -> Arc<NativeRegistrationStore> {
        Arc::new(
            NativeRegistrationStore::open_native_root(
                &self.root,
                Arc::new(Protection(Arc::clone(&self.controls))),
            )
            .unwrap(),
        )
    }
    fn engine(&self, deadline: Duration) -> NativeTransactions<Peer> {
        NativeTransactions::new(
            self.store(),
            Peer {
                controls: Arc::clone(&self.controls),
                deadline,
            },
        )
    }
    fn stop(&self, lease: &NativeTransactionLease) {
        lease.revalidate().unwrap();
        let mut h = head();
        h.generation = nz(8);
        h.lifecycle_generation = nz(3);
        h.desired_state = DesiredServerState::Stopped;
        h.active_request = Some(journal().request);
        h.startup_barrier = StartupBarrier::Inhibited;
        self.write(
            "release-501.registration",
            &encode_registration_head(&h).unwrap(),
        );
        self.write(
            "release-501.journal",
            &journal()
                .transition(Phase::Quiescing)
                .unwrap()
                .encode()
                .unwrap(),
        );
        lease.revalidate().unwrap();
    }
    fn retrieve(&self, lease: &NativeReleaseLease) -> Result<(), NativeFailure> {
        lease.recheck()?;
        self.controls.retrievals.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn deliver(&self, lease: &NativeReleaseLease) -> Result<(), NativeFailure> {
        lease.recheck()?;
        self.controls.deliveries.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Authorization {
    controls: Arc<Controls>,
    journal: ActivationJournalRecord,
    deadline: Instant,
}
impl NativeJournalAuthorization for Authorization {
    fn journal(&self) -> &ActivationJournalRecord {
        &self.journal
    }
    fn deadline(&self) -> Instant {
        self.deadline
    }
    fn revalidate(&self) -> Result<(), NativeFailure> {
        if self.controls.revoked.load(Ordering::SeqCst) {
            Err(NativeFailure::AuthorizationDenied)
        } else {
            Ok(())
        }
    }
}
struct Peer {
    controls: Arc<Controls>,
    // Positive peers allow loaded-host I/O. Zero/short deadlines test expiry separately;
    // these fixtures do not credit the production exchange's native timing bound.
    deadline: Duration,
}
impl NativeTransactionPeer for Peer {
    type Request = ActivationJournalRecord;
    fn authorize(
        &self,
        request: &Self::Request,
    ) -> Result<Box<dyn NativeJournalAuthorization>, NativeFailure> {
        Ok(Box::new(Authorization {
            controls: Arc::clone(&self.controls),
            journal: request.clone(),
            deadline: Instant::now() + self.deadline,
        }))
    }
}
struct PostExec {
    controls: Arc<Controls>,
    context: PendingStartupContext,
    descriptor: ApprovedInstallationDescriptor,
    deadline: Instant,
}
impl NativeReleaseValidation for PostExec {
    fn context(&self) -> &PendingStartupContext {
        &self.context
    }
    fn descriptor(&self) -> &ApprovedInstallationDescriptor {
        &self.descriptor
    }
    fn deadline(&self) -> Instant {
        self.deadline
    }
    fn revalidate_execution(&self) -> Result<(), NativeFailure> {
        if self.controls.process_changed.load(Ordering::SeqCst) {
            Err(NativeFailure::ProcessIdentityMismatch)
        } else {
            Ok(())
        }
    }
    fn observe_account(&self) -> Result<AccountBinding, NativeFailure> {
        Ok(self.controls.account.lock().unwrap().clone())
    }
    fn check_storage_and_policy(&self, _: &StartupSnapshot) -> Result<(), NativeFailure> {
        self.controls.storage.lock().unwrap().map_or(Ok(()), Err)
    }
}
impl NativeReleasePeer for Peer {
    type Pending = PendingStartupContext;
    type Proof = ();
    fn retain_post_exec(
        &self,
        pending: &Self::Pending,
        _: &(),
    ) -> Result<Box<dyn NativeReleaseValidation>, NativeFailure> {
        Ok(Box::new(PostExec {
            controls: Arc::clone(&self.controls),
            context: pending.clone(),
            descriptor: descriptor(),
            deadline: Instant::now() + self.deadline,
        }))
    }
}

#[test]
fn stop_committed_before_release_means_zero_retrievals_and_deliveries() {
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let stop = engine.begin(&journal()).unwrap();
    f.stop(&stop);
    drop(stop);
    assert!(engine.authorize_release(&context(), &()).is_err());
    assert_eq!(f.controls.retrievals.load(Ordering::SeqCst), 0);
    assert_eq!(f.controls.deliveries.load(Ordering::SeqCst), 0);
}
#[test]
fn release_guard_serializes_stop_until_delivery_finishes() {
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let release = engine.authorize_release(&context(), &()).unwrap();
    f.retrieve(&release).unwrap();
    let contender = f.engine(Duration::from_millis(20));
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let blocked = scope.spawn(|| {
            barrier.wait();
            contender.begin(&journal()).err()
        });
        barrier.wait();
        assert_eq!(blocked.join().unwrap(), Some(NativeFailure::TimedOut));
    });
    f.deliver(&release).unwrap();
    drop(release);
    let stop = engine.begin(&journal()).unwrap();
    f.stop(&stop);
    assert_eq!(f.controls.retrievals.load(Ordering::SeqCst), 1);
    assert_eq!(f.controls.deliveries.load(Ordering::SeqCst), 1);
}
#[test]
fn native_release_guard_excludes_a_separately_opened_cross_process_lock() {
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let release = engine.authorize_release(&context(), &()).unwrap();
    let script = "import fcntl,sys\nf=open(sys.argv[1], 'r+b')\ntry: fcntl.flock(f, fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError: sys.exit(23)\nsys.exit(0)\n";
    let attempt = || {
        std::process::Command::new("/usr/bin/python3")
            .args(["-I", "-c", script])
            .arg(f.root.join("locks/release-501.lock"))
            .env_clear()
            .status()
            .unwrap()
            .code()
    };
    assert_eq!(attempt(), Some(23));
    drop(release);
    assert_eq!(attempt(), Some(0));
}
#[test]
fn release_rechecks_process_account_custody_generations_and_storage_before_effects() {
    for fault in 0..6 {
        let f = Fixture::new();
        let engine = f.engine(Duration::from_secs(30));
        let release = engine.authorize_release(&context(), &()).unwrap();
        match fault {
            0 => f.controls.process_changed.store(true, Ordering::SeqCst),
            1 => {
                *f.controls.account.lock().unwrap() =
                    AccountBinding::new(key(), 20, NativeAccountId::macos([9; 16]).unwrap())
                        .unwrap()
            }
            2 => f.controls.custody_changed.store(true, Ordering::SeqCst),
            3 => *f.controls.storage.lock().unwrap() = Some(NativeFailure::StorageLocked),
            4 => {
                let mut h = head();
                h.generation = nz(8);
                f.write(
                    "release-501.registration",
                    &encode_registration_head(&h).unwrap(),
                );
            }
            _ => {
                let mut j = journal();
                j.state = Phase::RecoveryRequired;
                f.write("release-501.journal", &j.encode().unwrap());
            }
        }
        assert!(f.retrieve(&release).is_err());
        assert!(f.deliver(&release).is_err());
        assert_eq!(f.controls.retrievals.load(Ordering::SeqCst), 0);
        assert_eq!(f.controls.deliveries.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn transaction_rechecks_native_authorization_and_rejects_unrelated_journals() {
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let lease = engine.begin(&journal()).unwrap();
    f.controls.revoked.store(true, Ordering::SeqCst);
    assert_eq!(lease.revalidate(), Err(NativeFailure::AuthorizationDenied));
    drop(lease);
    f.controls.revoked.store(false, Ordering::SeqCst);
    let mut other = journal();
    other.request = NativeRequestId::new([9; 16]).unwrap();
    assert_eq!(
        engine.begin(&other).err(),
        Some(NativeFailure::IdentityMismatch)
    );
}
#[test]
fn uncertain_journal_survives_authority_recreation_and_old_running_head_replay() {
    let f = Fixture::new();
    let mut j = journal();
    j.state = Phase::RecoveryRequired;
    f.write("release-501.journal", &j.encode().unwrap());
    for _ in 0..2 {
        let engine = f.engine(Duration::from_secs(30));
        assert_eq!(
            engine.begin(&journal()).err(),
            Some(NativeFailure::Indeterminate)
        );
        assert!(engine.authorize_release(&context(), &()).is_err());
        f.write(
            "release-501.registration",
            &encode_registration_head(&head()).unwrap(),
        );
        File::open(f.root.join("release-501.journal"))
            .unwrap()
            .sync_all()
            .unwrap();
    }
}
#[test]
fn stable_lock_and_ancestor_substitution_retires_existing_authority() {
    for replace_ancestor in [false, true] {
        let f = Fixture::new();
        let engine = f.engine(Duration::from_secs(30));
        let lease = engine.begin(&journal()).unwrap();
        if replace_ancestor {
            std::fs::rename(f.root.join("locks"), f.root.join("retained-locks")).unwrap();
            std::fs::create_dir(f.root.join("locks")).unwrap();
        } else {
            let lock = f.root.join("locks/release-501.lock");
            std::fs::rename(&lock, f.root.join("old-lock")).unwrap();
            File::create(lock).unwrap();
        }
        assert!(lease.revalidate().is_err());
    }
}
#[test]
fn fixed_record_reads_authenticate_profile_and_preserve_schema_one_refusal() {
    let f = Fixture::new();
    let store = f.store();
    let read = store.read_snapshot(key()).unwrap();
    assert!(read.profile.roots() == profile().roots());
    let mut legacy = [0; 48];
    legacy[..8].copy_from_slice(b"STRGREC\0");
    legacy[8..10].copy_from_slice(&1u16.to_le_bytes());
    legacy[10] = 1;
    legacy[11] = 1;
    legacy[12..16].copy_from_slice(&501u32.to_le_bytes());
    legacy[16..24].copy_from_slice(&7u64.to_le_bytes());
    legacy[24..32].copy_from_slice(&11u64.to_le_bytes());
    f.write("release-501.registration", &legacy);
    assert_eq!(
        store.read_snapshot(key()).err(),
        Some(NativeFailure::LegacyRegistrationRequiresMigration)
    );
    f.write(
        "release-501.registration",
        &encode_registration_head(&head()).unwrap(),
    );
    let digest: String = head()
        .profile_sha256
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    f.write(&format!("release-501.profiles/{digest}.json"), b"{}");
    assert_eq!(
        store.read_snapshot(key()).err(),
        Some(NativeFailure::InvalidRecord)
    );
}
#[test]
fn publication_uses_the_same_live_transaction_and_cannot_extend_its_deadline() {
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let lease = engine.begin(&journal()).unwrap();
    let publication: Box<dyn NativePublicationTransaction> = Box::new(lease);
    publication.revalidate().unwrap();
    let contender = f.engine(Duration::from_millis(20));
    assert_eq!(
        contender.authorize_release(&context(), &()).err(),
        Some(NativeFailure::TimedOut)
    );
    drop(publication);
    assert!(engine.authorize_release(&context(), &()).is_ok());
    let expired = f.engine(Duration::ZERO);
    assert_eq!(
        expired.begin(&journal()).err(),
        Some(NativeFailure::TimedOut)
    );
}

struct FaultSync {
    parents: AtomicUsize,
    fail_parent: usize,
}
impl NativeRecordSync for FaultSync {
    fn sync_file(&self, file: &File) -> Result<(), NativeFailure> {
        SystemRecordSync.sync_file(file)
    }
    fn sync_parent(&self, parent: &File) -> Result<(), NativeFailure> {
        SystemRecordSync.sync_parent(parent)?;
        let call = self.parents.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.fail_parent {
            Err(NativeFailure::IoUnavailable)
        } else {
            Ok(())
        }
    }
}
fn stopped_proposal() -> RegistrationHeadV2 {
    let mut h = head();
    h.generation = nz(8);
    h.lifecycle_generation = nz(3);
    h.desired_state = DesiredServerState::Stopped;
    h.active_request = Some(journal().request);
    h.startup_barrier = StartupBarrier::Inhibited;
    h
}
#[test]
fn indeterminate_head_publication_survives_writer_recreation_until_content_validated_recovery() {
    use spaceterm_host::native_registration::writer::{PublicationState, RegistrationHeadWriter};
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(
        engine.begin(&journal()).unwrap(),
        FaultSync {
            parents: AtomicUsize::new(0),
            fail_parent: 3,
        },
    );
    let writer = RegistrationHeadWriter::new(storage);
    let original_result = writer.publish_proposal(&head(), &stopped_proposal());
    assert!(matches!(
        original_result,
        PublicationDisposition::Indeterminate
    ));
    assert_eq!(writer.state(), PublicationState::RecoveryRequired);
    drop(writer);
    // Neither a repeat synchronization nor a new authority/writer can clear the write-ahead intent.
    File::open(f.root.join("release-501.registration"))
        .unwrap()
        .sync_all()
        .unwrap();
    let recreated = f.engine(Duration::from_secs(30));
    assert_eq!(
        recreated.begin(&journal()).err(),
        Some(NativeFailure::Indeterminate)
    );
    assert!(recreated.authorize_release(&context(), &()).is_err());
    let recovery = NativeHeadStorage::new(
        recreated.begin_recovery(&journal()).unwrap(),
        SystemRecordSync,
    );
    assert!(matches!(
        recovery.recover().unwrap(),
        spaceterm_host::activation_journal::RecoverySelection::InhibitedOriginal { .. }
    ));
    drop(recovery);
    assert!(matches!(
        original_result,
        PublicationDisposition::Indeterminate
    ));
    assert!(recreated.begin(&journal()).is_ok());
    assert!(recreated.authorize_release(&context(), &()).is_err());
}
#[test]
fn head_publication_crash_before_replacement_recovers_only_the_original_selection() {
    use spaceterm_host::native_registration::writer::RegistrationPublicationStorage;
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(engine.begin(&journal()).unwrap(), SystemRecordSync);
    storage
        .write_fresh(
            key(),
            &encode_registration_head(&stopped_proposal()).unwrap(),
        )
        .unwrap();
    // The staged head and durable intent are not an approval commit.
    let pending_name = pending_head_name(&f);
    drop(storage);
    let recovery =
        NativeHeadStorage::new(engine.begin_recovery(&journal()).unwrap(), SystemRecordSync);
    assert!(matches!(
        recovery.recover().unwrap(),
        spaceterm_host::activation_journal::RecoverySelection::Original { .. }
    ));
    drop(recovery);
    assert!(engine.authorize_release(&context(), &()).is_ok());
    assert!(!f.root.join(&pending_name).exists());
    let writer = spaceterm_host::native_registration::writer::RegistrationHeadWriter::new(
        NativeHeadStorage::new(engine.begin(&journal()).unwrap(), SystemRecordSync),
    );
    assert!(matches!(
        writer.publish_proposal(&head(), &stopped_proposal()),
        PublicationDisposition::Durable(())
    ));
}
#[test]
fn successful_head_publication_requires_journal_bound_recovery_not_a_new_writer() {
    use spaceterm_host::native_registration::writer::RegistrationHeadWriter;
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(engine.begin(&journal()).unwrap(), SystemRecordSync);
    let writer = RegistrationHeadWriter::new(storage);
    assert!(matches!(
        writer.publish_proposal(&head(), &stopped_proposal()),
        PublicationDisposition::Durable(())
    ));
    let storage = writer.into_storage();
    assert!(storage.recover().is_ok());
    drop(storage);
    assert!(engine.begin(&journal()).is_ok());
    assert!(engine.authorize_release(&context(), &()).is_err());
}

fn staging_journal() -> ActivationJournalRecord {
    let mut journal = journal();
    journal.operation = NativeOperationKind::Stage;
    journal.desired_state = DesiredServerState::Running;
    journal.state = Phase::Staging;
    journal.candidate_input = Some(VerifiedCandidateId::new([3; 16]).unwrap());
    journal
}
#[test]
fn installation_allocation_is_monotonic_native_random_and_bound_to_the_original_request() {
    let f = Fixture::new();
    let journal = staging_journal();
    f.write("release-501.journal", &journal.encode().unwrap());
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(engine.begin(&journal).unwrap(), SystemRecordSync);
    let allocation = match storage.reserve_installation() {
        PublicationDisposition::Durable(allocation) => allocation,
        _ => panic!("controlled native allocation failed"),
    };
    assert_eq!(allocation.installation, InstallationId(nz(13)));
    assert!(allocation.publication_nonce.iter().any(|byte| *byte != 0));
    assert_eq!(
        storage.retained_allocation().unwrap().publication_nonce,
        allocation.publication_nonce
    );
    assert!(matches!(
        storage.reserve_installation(),
        PublicationDisposition::NotApplied(NativeFailure::OutcomePending)
    ));
    drop(storage);
    let recreated = NativeHeadStorage::new(engine.begin(&journal).unwrap(), SystemRecordSync);
    assert_eq!(
        recreated.retained_allocation().unwrap().installation,
        allocation.installation
    );
    drop(recreated);
    let snapshot = f.store().read_snapshot(key()).unwrap();
    assert_eq!(snapshot.head.installation_high_water, 13);
    assert_eq!(
        snapshot.head.approved_installation,
        head().approved_installation
    );
}
#[test]
fn uncertain_allocation_cannot_reuse_its_id_or_nonce_after_authority_recreation() {
    let f = Fixture::new();
    let journal = staging_journal();
    f.write("release-501.journal", &journal.encode().unwrap());
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(
        engine.begin(&journal).unwrap(),
        FaultSync {
            parents: AtomicUsize::new(0),
            fail_parent: 4,
        },
    );
    let original_result = storage.reserve_installation();
    assert!(matches!(
        original_result,
        PublicationDisposition::Indeterminate
    ));
    drop(storage);
    assert_eq!(
        engine.begin(&journal).err(),
        Some(NativeFailure::Indeterminate)
    );
    let recovered =
        NativeHeadStorage::new(engine.begin_recovery(&journal).unwrap(), SystemRecordSync);
    recovered.recover().unwrap();
    let allocation = recovered.retained_allocation().unwrap();
    assert_eq!(allocation.installation, InstallationId(nz(13)));
    assert!(matches!(
        recovered.reserve_installation(),
        PublicationDisposition::NotApplied(NativeFailure::OutcomePending)
    ));
    assert!(matches!(
        original_result,
        PublicationDisposition::Indeterminate
    ));
}

#[test]
fn release_rejects_context_artifact_and_installation_binding_substitution() {
    for replace_artifact in [false, true] {
        let f = Fixture::new();
        let engine = f.engine(Duration::from_secs(30));
        let mut substituted = context();
        if replace_artifact {
            substituted.server_artifact_sha256 = [99; 32];
        } else {
            substituted.installation_binding = [99; 32];
        }
        assert_eq!(
            engine.authorize_release(&substituted, &()).err(),
            Some(NativeFailure::GenerationMismatch)
        );
        assert_eq!(f.controls.retrievals.load(Ordering::SeqCst), 0);
        assert_eq!(f.controls.deliveries.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn activation_and_release_share_the_stable_native_transaction_lock() {
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let release = engine.authorize_release(&context(), &()).unwrap();
    let mut activation = journal();
    activation.operation = NativeOperationKind::Activate;
    activation.desired_state = DesiredServerState::Running;
    activation.candidate_installation = Some(InstallationId(nz(12)));
    activation.candidate_manifest_sha256 = Some([8; 32]);
    activation.candidate_data_generation = Some(DataGenerationId::new(2).unwrap());
    let contender = f.engine(Duration::from_millis(20));
    assert_eq!(
        contender.begin(&activation).err(),
        Some(NativeFailure::TimedOut)
    );
    f.deliver(&release).unwrap();
    drop(release);
    // Release of the fence does not authorize an unrelated activation or replace the admitted Stop.
    assert_eq!(
        engine.begin(&activation).err(),
        Some(NativeFailure::IdentityMismatch)
    );
    assert_eq!(
        f.store().read_journal(key()).unwrap().operation,
        NativeOperationKind::Stop
    );
}

#[test]
fn native_install_head_commit_recovers_before_journal_advance_running_and_stopped() {
    use spaceterm_host::{
        activation_journal::RecoverySelection,
        native_registration::writer::RegistrationPublicationStorage,
    };
    for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
        for after_commit in [false, true] {
            let f = Fixture::new();
            let mut original = head();
            original.desired_state = desired;
            f.write(
                "release-501.registration",
                &encode_registration_head(&original).unwrap(),
            );
            let mut install = journal();
            install.operation = NativeOperationKind::Install;
            install.state = Phase::Staged;
            install.desired_state = desired;
            install.candidate_input = Some(VerifiedCandidateId::new([3; 16]).unwrap());
            install.candidate_installation = Some(InstallationId(nz(12)));
            install.candidate_manifest_sha256 = Some([8; 32]);
            install.candidate_data_generation = Some(DataGenerationId::new(2).unwrap());
            install.assessment_digest = None;
            f.write("release-501.journal", &install.encode().unwrap());
            let mut next = original.clone();
            next.generation = nz(8);
            next.active_request = Some(install.request);
            next.startup_barrier = StartupBarrier::ApprovedStartOnly;
            next.approved_installation = install.candidate_installation.unwrap();
            next.approved_data_generation = install.candidate_data_generation.unwrap();
            next.approved_manifest_sha256 = install.candidate_manifest_sha256.unwrap();
            next.approved_descriptor_sha256 = next.staged_descriptor_sha256.take().unwrap();
            next.staged_installation = None;
            next.data_high_water = 2;
            let engine = f.engine(Duration::from_secs(30));
            let storage = NativeHeadStorage::new(engine.begin(&install).unwrap(), SystemRecordSync);
            storage
                .write_fresh(key(), &encode_registration_head(&next).unwrap())
                .unwrap();
            if after_commit {
                storage.sync_fresh().unwrap();
                storage.replace_head(key()).unwrap();
                storage.sync_parent().unwrap();
            }
            drop(storage);
            assert_eq!(
                engine.begin(&install).err(),
                Some(NativeFailure::Indeterminate)
            );
            let observed = f.store().read_snapshot(key()).unwrap();
            let selection = observed
                .journal
                .unwrap()
                .recovery_selection(&observed.head, &account().sha256())
                .unwrap();
            let recovery =
                NativeHeadStorage::new(engine.begin_recovery(&install).unwrap(), SystemRecordSync);
            if after_commit {
                let expected = RecoverySelection::Committed {
                    installation: next.approved_installation,
                    data: next.approved_data_generation,
                    desired_state: desired,
                };
                assert_eq!(selection, expected);
                assert_eq!(recovery.recover(), Ok(expected));
            } else {
                assert!(matches!(selection, RecoverySelection::Original { .. }));
                // A burned candidate data ID must remain reserved during precommit repair.
                assert_eq!(recovery.recover(), Err(NativeFailure::Indeterminate));
            }
        }
    }
}

#[test]
fn terminal_stage_replay_cannot_acquire_another_mutation_lease() {
    let f = Fixture::new();
    let request = staging_journal();
    let mut staged = request.clone();
    staged.candidate_installation = head().staged_installation;
    staged.candidate_manifest_sha256 = Some([8; 32]);
    let completed = staged
        .transition(Phase::Staged)
        .unwrap()
        .transition(Phase::Completed)
        .unwrap();
    f.write("release-501.journal", &completed.encode().unwrap());
    let before = std::fs::read(f.root.join("release-501.registration")).unwrap();
    let engine = f.engine(Duration::from_secs(30));
    assert_eq!(
        engine.begin(&request).err(),
        Some(NativeFailure::OutcomePending)
    );
    assert_eq!(
        engine.resolve(&request),
        Ok(spaceterm_host::activation_journal::RetainedNativeOutcome::Staged)
    );
    let mut substituted = request.clone();
    substituted.candidate_input = Some(VerifiedCandidateId::new([99; 16]).unwrap());
    assert_eq!(
        engine.resolve(&substituted),
        Err(NativeFailure::IdentityMismatch)
    );
    assert_eq!(
        std::fs::read(f.root.join("release-501.registration")).unwrap(),
        before
    );
    assert_eq!(f.controls.retrievals.load(Ordering::SeqCst), 0);
    assert_eq!(f.controls.deliveries.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_recovery_retirement_keeps_mutation_and_running_release_closed() {
    use spaceterm_host::native_registration::writer::RegistrationHeadWriter;
    for recovery_fault in [3, 4] {
        let f = Fixture::new();
        let request = staging_journal();
        f.write("release-501.journal", &request.encode().unwrap());
        let engine = f.engine(Duration::from_secs(30));
        let mut next = head();
        next.generation = nz(8);
        let writer = RegistrationHeadWriter::new(NativeHeadStorage::new(
            engine.begin(&request).unwrap(),
            FaultSync {
                parents: AtomicUsize::new(0),
                fail_parent: 3,
            },
        ));
        assert!(matches!(
            writer.publish_proposal(&head(), &next),
            PublicationDisposition::Indeterminate
        ));
        drop(writer);
        let recovery = NativeHeadStorage::new(
            engine.begin_recovery(&request).unwrap(),
            FaultSync {
                parents: AtomicUsize::new(0),
                fail_parent: recovery_fault,
            },
        );
        assert!(recovery.recover().is_err());
        drop(recovery);
        let recreated = f.engine(Duration::from_secs(30));
        assert_eq!(
            recreated.begin(&request).err(),
            Some(NativeFailure::Indeterminate)
        );
        let mut current = context();
        current.registration_generation = next.generation;
        assert!(recreated.authorize_release(&current, &()).is_err());
        let durable_recovery = NativeHeadStorage::new(
            recreated.begin_recovery(&request).unwrap(),
            SystemRecordSync,
        );
        durable_recovery.recover().unwrap();
        drop(durable_recovery);
        assert!(recreated.begin(&request).is_ok());
        assert!(recreated.authorize_release(&current, &()).is_ok());
    }
}

fn pending_head_name(f: &Fixture) -> String {
    let bytes = std::fs::read(f.root.join("release-501.head-publication")).unwrap();
    let intent: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    intent["staging_name"].as_str().unwrap().to_owned()
}

#[test]
fn recovery_preserves_a_substituted_staging_inode() {
    use spaceterm_host::native_registration::writer::RegistrationPublicationStorage;
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(engine.begin(&journal()).unwrap(), SystemRecordSync);
    storage
        .write_fresh(
            key(),
            &encode_registration_head(&stopped_proposal()).unwrap(),
        )
        .unwrap();
    drop(storage);
    // The replacement has the same bytes and accepted injected custody, but a different inode.
    let pending_name = pending_head_name(&f);
    f.write(
        &pending_name,
        &encode_registration_head(&stopped_proposal()).unwrap(),
    );
    let replacement = std::fs::metadata(f.root.join(&pending_name)).unwrap().ino();
    let recovery =
        NativeHeadStorage::new(engine.begin_recovery(&journal()).unwrap(), SystemRecordSync);
    assert_eq!(recovery.recover(), Err(NativeFailure::UnsafeNativeState));
    assert_eq!(
        std::fs::metadata(f.root.join(&pending_name)).unwrap().ino(),
        replacement
    );
}

#[test]
fn staging_proposal_consumes_the_reserved_high_water_id_without_allocating_again() {
    let f = Fixture::new();
    let request = staging_journal();
    f.write("release-501.journal", &request.encode().unwrap());
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(engine.begin(&request).unwrap(), SystemRecordSync);
    let allocation = match storage.reserve_installation() {
        PublicationDisposition::Durable(allocation) => allocation,
        _ => panic!("controlled native allocation failed"),
    };
    let reserved = f.store().read_snapshot(key()).unwrap().head;
    let next =
        spaceterm_host::installation::propose_staging(&reserved, allocation.installation, [17; 32])
            .unwrap();
    assert_eq!(
        next.installation_high_water,
        reserved.installation_high_water
    );
    assert_eq!(next.staged_installation, Some(allocation.installation));
    assert!(
        spaceterm_host::installation::propose_staging(&reserved, InstallationId(nz(12)), [17; 32])
            .is_err()
    );
}

#[test]
fn authorized_request_durably_admits_the_missing_journal_before_returning_a_lease() {
    let f = Fixture::new();
    std::fs::remove_file(f.root.join("release-501.journal")).unwrap();
    let engine = f.engine(Duration::from_secs(30));
    let lease = engine.begin(&journal()).unwrap();
    assert_eq!(f.store().read_journal(key()).unwrap(), journal());
    lease.revalidate().unwrap();
}

struct PublicationPeer<'a> {
    fixture: &'a Fixture,
}
impl InstallationPublicationNative for PublicationPeer<'_> {
    type Request = ActivationJournalRecord;
    type Registration = spaceterm_host::installation::RetainedRegistration;
    fn begin_publication(
        &self,
        request: &Self::Request,
        _: &spaceterm_distribution::VerifiedServerArchive,
    ) -> Result<PublicationScope, NativeFailure> {
        let engine = self.fixture.engine(Duration::from_secs(30));
        NativeHeadStorage::new(engine.begin(request)?, SystemRecordSync)
            .into_publication_scope(&self.fixture.root)
    }
    fn open_approved(&self, _: &Self::Registration) -> Result<ApprovedPublication, NativeFailure> {
        Err(NativeFailure::InstallationUnavailable)
    }
    fn latch_uncertainty(&self) -> Result<(), NativeFailure> {
        panic!("the successful publication control must not invoke uncertainty")
    }
}

#[test]
fn reserve_fresh_publication_stage_and_terminal_replay_share_one_request_and_nonce() {
    use spaceterm_host::native_registration::writer::RegistrationHeadWriter;
    let f = Fixture::new();
    let request = staging_journal();
    f.write("release-501.journal", &request.encode().unwrap());
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(engine.begin(&request).unwrap(), SystemRecordSync);
    let allocation = match storage.reserve_installation() {
        PublicationDisposition::Durable(identity) => identity,
        _ => panic!("controlled native allocation failed"),
    };
    drop(storage);
    let publisher = FreshInstallationPublisher::new(PublicationPeer { fixture: &f });
    let candidate = archive_fixture::candidate(spaceterm_distribution::ServerPlatform::LinuxX86_64);
    let staged = match publisher.publish_fresh(&request, &candidate) {
        PublicationDisposition::Durable(staged) => staged,
        _ => panic!("controlled authenticated destination publication failed"),
    };
    assert_eq!(staged.descriptor().installation, allocation.installation);
    assert_eq!(
        staged.descriptor().publication_nonce,
        allocation.publication_nonce
    );
    let storage = NativeHeadStorage::new(engine.begin(&request).unwrap(), SystemRecordSync);
    assert_eq!(
        storage.record_staged(&staged),
        PublicationDisposition::Durable(())
    );
    let before = f.store().read_snapshot(key()).unwrap().head;
    let next = storage.propose_staging(&staged).unwrap();
    let writer = RegistrationHeadWriter::new(storage);
    assert_eq!(
        writer.publish_proposal(&before, &next),
        PublicationDisposition::Durable(())
    );
    let storage = writer.into_storage();
    storage.recover().unwrap();
    let completed = f
        .store()
        .read_journal(key())
        .unwrap()
        .transition(Phase::Completed)
        .unwrap();
    assert_eq!(
        storage.advance_journal(&completed),
        PublicationDisposition::Durable(())
    );
    drop(storage);
    let bytes = std::fs::read(f.root.join("release-501.registration")).unwrap();
    assert_eq!(
        engine.begin(&request).err(),
        Some(NativeFailure::OutcomePending)
    );
    assert_eq!(
        engine.resolve(&request),
        Ok(spaceterm_host::activation_journal::RetainedNativeOutcome::Staged)
    );
    assert_eq!(
        std::fs::read(f.root.join("release-501.registration")).unwrap(),
        bytes
    );
    assert_eq!(
        next.installation_high_water,
        allocation.installation.0.get()
    );
    assert_eq!(next.approved_installation, head().approved_installation);
    let mut activation = journal();
    activation.request = NativeRequestId::new([60; 16]).unwrap();
    activation.operation = NativeOperationKind::Activate;
    activation.desired_state = next.desired_state;
    activation.expected_registration_generation = next.generation;
    activation.expected_lifecycle_generation = next.lifecycle_generation;
    activation.candidate_installation = next.staged_installation;
    activation.candidate_manifest_sha256 = Some(staged.descriptor().manifest_sha256);
    drop(engine.begin(&activation).unwrap());
    assert_eq!(
        engine.resolve(&request),
        Ok(spaceterm_host::activation_journal::RetainedNativeOutcome::Staged)
    );
    assert_eq!(
        f.store().read_snapshot(key()).unwrap().head.active_request,
        Some(activation.request)
    );
}

#[test]
fn journal_admission_and_phase_sync_failures_survive_authority_recreation() {
    for admission in [true, false] {
        for fault in [1, 2, 3] {
            let f = Fixture::new();
            let engine = f.engine(Duration::from_secs(30));
            if admission {
                std::fs::remove_file(f.root.join("release-501.journal")).unwrap();
                assert!(
                    engine
                        .begin_with_sync(
                            &journal(),
                            FaultSync {
                                parents: AtomicUsize::new(0),
                                fail_parent: fault
                            }
                        )
                        .is_err()
                );
            } else {
                let storage = NativeHeadStorage::new(
                    engine.begin(&journal()).unwrap(),
                    FaultSync {
                        parents: AtomicUsize::new(0),
                        fail_parent: fault,
                    },
                );
                let next = journal().transition(Phase::Aborted).unwrap();
                assert_eq!(
                    storage.advance_journal(&next),
                    PublicationDisposition::Indeterminate
                );
                drop(storage);
            }
            let recreated = f.engine(Duration::from_secs(30));
            assert_eq!(
                recreated.begin(&journal()).err(),
                Some(NativeFailure::Indeterminate)
            );
            assert!(recreated.authorize_release(&context(), &()).is_err());
            let recovery = NativeHeadStorage::new(
                recreated.begin_recovery(&journal()).unwrap(),
                SystemRecordSync,
            );
            recovery.recover_journal().unwrap();
            drop(recovery);
            let selected = match f.store().read_journal(key()) {
                Ok(record) => record,
                Err(NativeFailure::RegistrationMissing) => {
                    assert!(recreated.begin(&journal()).is_ok());
                    f.store().read_journal(key()).unwrap()
                }
                Err(error) => panic!("unexpected journal recovery failure: {error:?}"),
            };
            if selected.state == Phase::Aborted {
                assert_eq!(
                    recreated.begin(&journal()).err(),
                    Some(NativeFailure::OutcomePending)
                );
                assert_eq!(
                    recreated.resolve(&journal()),
                    Ok(spaceterm_host::activation_journal::RetainedNativeOutcome::Aborted)
                );
            } else {
                assert!(recreated.begin(&journal()).is_ok());
            }
        }
    }
}

#[test]
fn retained_registration_rejects_head_profile_journal_and_ancestor_replacement() {
    for substitution in 0..4 {
        let f = Fixture::new();
        let store = f.store();
        let registration = store.retain_registration(key(), head().generation).unwrap();
        assert!(registration.profile().roots() == profile().roots());
        registration.revalidate().unwrap();
        match substitution {
            0 => f.write(
                "release-501.registration",
                &encode_registration_head(&head()).unwrap(),
            ),
            1 => {
                let digest: String = head()
                    .profile_sha256
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                f.write(
                    &format!("release-501.profiles/{digest}.json"),
                    &profile().encode().unwrap(),
                );
            }
            2 => f.write("release-501.journal", &journal().encode().unwrap()),
            _ => {
                std::fs::rename(
                    f.root.join("release-501.profiles"),
                    f.root.join("retired-profiles"),
                )
                .unwrap();
                std::fs::create_dir(f.root.join("release-501.profiles")).unwrap();
            }
        }
        assert!(registration.revalidate().is_err());
    }
}

#[test]
fn head_replacement_never_publishes_a_substituted_staging_inode() {
    use spaceterm_host::native_registration::writer::RegistrationPublicationStorage;
    let f = Fixture::new();
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(engine.begin(&journal()).unwrap(), SystemRecordSync);
    storage
        .write_fresh(
            key(),
            &encode_registration_head(&stopped_proposal()).unwrap(),
        )
        .unwrap();
    let pending_name = pending_head_name(&f);
    f.write(
        &pending_name,
        &encode_registration_head(&stopped_proposal()).unwrap(),
    );
    assert_eq!(
        storage.replace_head(key()),
        Err(NativeFailure::UnsafeNativeState)
    );
    assert_eq!(f.store().read_snapshot(key()).unwrap().head, head());
}

#[test]
fn completed_stop_releases_only_its_matching_head_fence_before_the_next_start() {
    use spaceterm_host::native_registration::writer::RegistrationHeadWriter;
    let f = Fixture::new();
    let request = journal();
    let engine = f.engine(Duration::from_secs(30));
    let writer = RegistrationHeadWriter::new(NativeHeadStorage::new(
        engine.begin(&request).unwrap(),
        SystemRecordSync,
    ));
    let stopped = stopped_proposal();
    assert_eq!(
        writer.publish_proposal(&head(), &stopped),
        PublicationDisposition::Durable(())
    );
    let storage = writer.into_storage();
    storage.recover().unwrap();
    let mut current = request;
    for phase in [Phase::Quiescing, Phase::Quiesced, Phase::Completed] {
        current = current.transition(phase).unwrap();
        assert_eq!(
            storage.advance_journal(&current),
            PublicationDisposition::Durable(())
        );
    }
    let mut open = stopped.clone();
    open.generation = nz(stopped.generation.get() + 1);
    open.active_request = None;
    open.startup_barrier = StartupBarrier::Open;
    drop(storage);
    assert_eq!(
        engine.finish_request(&journal(), SystemRecordSync),
        PublicationDisposition::Durable(
            spaceterm_host::activation_journal::RetainedNativeOutcome::Stopped
        )
    );
    let mut start = journal();
    start.request = NativeRequestId::new([30; 16]).unwrap();
    start.operation = NativeOperationKind::Start;
    start.desired_state = DesiredServerState::Running;
    start.expected_registration_generation = open.generation;
    start.expected_lifecycle_generation = open.lifecycle_generation;
    assert!(engine.begin(&start).is_ok());
}

#[test]
fn fresh_activate_and_collect_are_admitted_without_fixture_written_journals() {
    for operation in [NativeOperationKind::Activate, NativeOperationKind::Collect] {
        let f = Fixture::new();
        std::fs::remove_file(f.root.join("release-501.journal")).unwrap();
        let mut request = journal();
        request.operation = operation;
        request.desired_state = DesiredServerState::Running;
        if operation == NativeOperationKind::Activate {
            request.candidate_installation = head().staged_installation;
            request.candidate_manifest_sha256 = Some([8; 32]);
        }
        let engine = f.engine(Duration::from_secs(30));
        let lease = engine.begin(&request).unwrap();
        lease.revalidate().unwrap();
        let selected = f.store().read_snapshot(key()).unwrap();
        assert_eq!(selected.journal.unwrap(), request);
        assert_eq!(selected.head.startup_barrier, StartupBarrier::Inhibited);
        assert_eq!(selected.head.active_request, Some(request.request));
        assert_eq!(
            selected.head.approved_installation,
            head().approved_installation
        );
    }
}

#[test]
fn intervening_admission_preserves_terminal_outcome_and_refuses_request_id_reuse() {
    use spaceterm_host::activation_journal::RetainedNativeOutcome;
    let f = Fixture::new();
    let first = staging_journal();
    let mut completed = first.clone();
    completed.candidate_installation = head().staged_installation;
    completed.candidate_manifest_sha256 = Some([8; 32]);
    completed = completed
        .transition(Phase::Staged)
        .unwrap()
        .transition(Phase::Completed)
        .unwrap();
    f.write("release-501.journal", &completed.encode().unwrap());
    let mut second = journal();
    second.request = NativeRequestId::new([42; 16]).unwrap();
    drop(f.engine(Duration::from_secs(30)).begin(&second).unwrap());
    let recreated = f.engine(Duration::from_secs(30));
    assert_eq!(recreated.resolve(&first), Ok(RetainedNativeOutcome::Staged));
    assert_eq!(
        recreated.begin(&first).err(),
        Some(NativeFailure::OutcomePending)
    );
    let mut substituted = first.clone();
    substituted.candidate_input = Some(VerifiedCandidateId::new([43; 16]).unwrap());
    assert_eq!(
        recreated.resolve(&substituted),
        Err(NativeFailure::IdentityMismatch)
    );
    assert_eq!(
        recreated.begin(&substituted).err(),
        Some(NativeFailure::IdentityMismatch)
    );
    assert_eq!(f.store().read_journal(key()).unwrap(), second);
}

struct FileFaultSync {
    calls: AtomicUsize,
    fail_file: usize,
}
impl NativeRecordSync for FileFaultSync {
    fn sync_file(&self, file: &File) -> Result<(), NativeFailure> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_file {
            Err(NativeFailure::IoUnavailable)
        } else {
            SystemRecordSync.sync_file(file)
        }
    }
    fn sync_parent(&self, parent: &File) -> Result<(), NativeFailure> {
        SystemRecordSync.sync_parent(parent)
    }
}

#[test]
fn staging_before_identity_publication_is_preserved_and_does_not_block_recovery() {
    use spaceterm_host::native_registration::writer::RegistrationPublicationStorage;
    for fault in 1..=3 {
        let f = Fixture::new();
        let engine = f.engine(Duration::from_secs(30));
        let storage = NativeHeadStorage::new(
            engine.begin(&journal()).unwrap(),
            FileFaultSync {
                calls: AtomicUsize::new(0),
                fail_file: fault,
            },
        );
        assert_eq!(
            storage.write_fresh(
                key(),
                &encode_registration_head(&stopped_proposal()).unwrap()
            ),
            Err(NativeFailure::IoUnavailable)
        );
        drop(storage);
        let unbound = f.root.join(pending_head_name(&f));
        let identity = std::fs::metadata(&unbound)
            .ok()
            .map(|metadata| metadata.ino());
        let recovery =
            NativeHeadStorage::new(engine.begin_recovery(&journal()).unwrap(), SystemRecordSync);
        recovery.recover().unwrap();
        assert_eq!(
            std::fs::metadata(&unbound)
                .ok()
                .map(|metadata| metadata.ino()),
            identity
        );
        recovery
            .write_fresh(
                key(),
                &encode_registration_head(&stopped_proposal()).unwrap(),
            )
            .unwrap();
        recovery.replace_head(key()).unwrap();
        recovery.sync_parent().unwrap();
        recovery.recover().unwrap();
        assert_eq!(
            f.store().read_snapshot(key()).unwrap().head,
            stopped_proposal()
        );
        assert_eq!(
            std::fs::metadata(&unbound)
                .ok()
                .map(|metadata| metadata.ino()),
            identity
        );
    }
}

fn ready_future<T>(future: impl std::future::Future<Output = T>) -> T {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("the injected store responds synchronously"),
    }
}
struct DataStore {
    expected: ActivationJournalRecord,
    validated: DataGenerationId,
    compatibility: AtomicUsize,
    validations: AtomicUsize,
}
impl spaceterm_host::installation::ServerDataCompatibilityPort for DataStore {
    fn revalidate_read_compatibility(
        &self,
        request: NativeRequestId,
        source: DataGenerationId,
        candidate: InstallationId,
        assessment: [u8; 32],
    ) -> NativeManagementFuture<'_, ()> {
        assert_eq!(request, self.expected.request);
        assert_eq!(source, self.expected.original_data_generation);
        assert_eq!(Some(candidate), self.expected.candidate_installation);
        assert_eq!(Some(assessment), self.expected.assessment_digest);
        self.compatibility.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}
impl spaceterm_host::installation::ServerMigrationPort for DataStore {
    fn prepare_generation(
        &self,
        _: NativeRequestId,
        _: DataGenerationId,
        _: InstallationId,
    ) -> NativeManagementFuture<'_, spaceterm_host::installation::MigrationProgress> {
        panic!("data publication validates the owner's result; it must not repeat migration")
    }
    fn validate_generation(
        &self,
        request: NativeRequestId,
        candidate: DataGenerationId,
    ) -> NativeManagementFuture<'_, spaceterm_host::installation::MigrationProgress> {
        assert_eq!(request, self.expected.request);
        assert_eq!(candidate, DataGenerationId::new(2).unwrap());
        self.validations.fetch_add(1, Ordering::SeqCst);
        let result = self.validated;
        Box::pin(async move {
            Ok(spaceterm_host::installation::MigrationProgress::Ready(
                result,
            ))
        })
    }
}

#[test]
fn owning_store_data_results_are_persisted_before_approval() {
    for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
        let f = Fixture::new();
        let mut h = head();
        h.desired_state = desired;
        h.generation = nz(8);
        h.active_request = Some(journal().request);
        h.startup_barrier = StartupBarrier::Inhibited;
        f.write(
            "release-501.registration",
            &encode_registration_head(&h).unwrap(),
        );
        let mut request = journal();
        request.operation = NativeOperationKind::Activate;
        request.desired_state = desired;
        request.candidate_installation = head().staged_installation;
        request.candidate_manifest_sha256 = Some([8; 32]);
        request.state = if desired == DesiredServerState::Running {
            Phase::Migrating
        } else {
            Phase::Quiesced
        };
        f.write("release-501.journal", &request.encode().unwrap());
        let store = DataStore {
            expected: request.clone(),
            validated: DataGenerationId::new(2).unwrap(),
            compatibility: AtomicUsize::new(0),
            validations: AtomicUsize::new(0),
        };
        let engine = f.engine(Duration::from_secs(30));
        let storage = NativeHeadStorage::new(engine.begin(&request).unwrap(), SystemRecordSync);
        let result = if desired == DesiredServerState::Running {
            ready_future(storage.record_migrated_data(store.validated, &store))
        } else {
            ready_future(storage.record_stopped_data(&store))
        };
        assert_eq!(result, PublicationDisposition::Durable(()));
        let selected = f.store().read_snapshot(key()).unwrap();
        assert_eq!(
            selected.journal.unwrap().candidate_data_generation,
            Some(if desired == DesiredServerState::Running {
                store.validated
            } else {
                request.original_data_generation
            })
        );
        assert_eq!(
            selected.head.data_high_water,
            if desired == DesiredServerState::Running {
                2
            } else {
                1
            }
        );
        assert_eq!(
            store.validations.load(Ordering::SeqCst),
            usize::from(desired == DesiredServerState::Running)
        );
        assert_eq!(
            store.compatibility.load(Ordering::SeqCst),
            usize::from(desired == DesiredServerState::Stopped)
        );
    }
}

#[test]
fn original_running_completion_recovers_outcome_and_fence_interruption_boundaries() {
    use spaceterm_host::activation_journal::RetainedNativeOutcome;
    for fault in 0..=9 {
        let f = Fixture::new();
        let mut request = journal();
        request.operation = NativeOperationKind::Start;
        request.desired_state = DesiredServerState::Running;
        let starting = request.transition(Phase::Starting).unwrap();
        let mut h = head();
        h.generation = nz(8);
        h.lifecycle_generation = nz(3);
        h.active_request = Some(request.request);
        h.startup_barrier = StartupBarrier::ApprovedStartOnly;
        f.write(
            "release-501.registration",
            &encode_registration_head(&h).unwrap(),
        );
        f.write("release-501.journal", &starting.encode().unwrap());
        let engine = f.engine(Duration::from_secs(30));
        let result = engine.finish_request(
            &request,
            FaultSync {
                parents: AtomicUsize::new(0),
                fail_parent: fault,
            },
        );
        if fault == 0 {
            assert_eq!(
                result,
                PublicationDisposition::Durable(RetainedNativeOutcome::Running)
            );
        } else {
            assert_eq!(
                result,
                PublicationDisposition::Indeterminate,
                "sync boundary {fault}"
            );
            let recreated = f.engine(Duration::from_secs(30));
            assert!(recreated.begin(&request).is_err());
            if f.root.join("release-501.journal-publication").exists() {
                let recovery = NativeHeadStorage::new(
                    recreated.begin_recovery(&request).unwrap(),
                    SystemRecordSync,
                );
                recovery.recover_journal().unwrap();
            }
            if f.root.join("release-501.head-publication").exists()
                || f.root.join("release-501.head-recovered").exists()
            {
                let recovery = NativeHeadStorage::new(
                    recreated.begin_recovery(&request).unwrap(),
                    SystemRecordSync,
                );
                recovery.recover().unwrap();
            }
            assert_eq!(
                recreated.finish_request(&request, SystemRecordSync),
                PublicationDisposition::Durable(RetainedNativeOutcome::Running)
            );
            assert_eq!(result, PublicationDisposition::Indeterminate);
        }
        let selected = f.store().read_snapshot(key()).unwrap();
        assert_eq!(selected.head.startup_barrier, StartupBarrier::Open);
        assert_eq!(selected.head.active_request, None);
        assert_eq!(selected.head.approved_installation, h.approved_installation);
        assert_eq!(
            selected.head.approved_data_generation,
            h.approved_data_generation
        );
        assert_eq!(selected.head.lifecycle_generation, h.lifecycle_generation);
        let bytes = std::fs::read(f.root.join("release-501.registration")).unwrap();
        assert_eq!(
            engine.begin(&request).err(),
            Some(NativeFailure::OutcomePending)
        );
        assert_eq!(engine.resolve(&request), Ok(RetainedNativeOutcome::Running));
        assert_eq!(
            engine.finish_request(&request, SystemRecordSync),
            PublicationDisposition::Durable(RetainedNativeOutcome::Running)
        );
        assert_eq!(
            std::fs::read(f.root.join("release-501.registration")).unwrap(),
            bytes
        );
    }
}

#[test]
fn outcome_capacity_is_reserved_before_new_admission_and_never_evicts_reusable_ids() {
    let f = Fixture::new();
    std::fs::remove_file(f.root.join("release-501.journal")).unwrap();
    let engine = f.engine(Duration::from_secs(30));
    let mut first = journal();
    first.request = NativeRequestId::new([50; 16]).unwrap();
    for id in 50..=83 {
        let mut request = first.clone();
        request.request = NativeRequestId::new([id; 16]).unwrap();
        let before = std::fs::read(f.root.join("release-501.journal")).ok();
        if id == 83 {
            assert_eq!(
                engine.begin(&request).err(),
                Some(NativeFailure::LimitExceeded)
            );
            assert_eq!(
                std::fs::read(f.root.join("release-501.journal")).ok(),
                before
            );
            assert!(!f.root.join("release-501.journal-publication").exists());
            break;
        }
        let storage = NativeHeadStorage::new(engine.begin(&request).unwrap(), SystemRecordSync);
        assert_eq!(
            storage.advance_journal(&request.transition(Phase::Aborted).unwrap()),
            PublicationDisposition::Durable(())
        );
    }
    assert_eq!(
        engine.resolve(&first),
        Ok(spaceterm_host::activation_journal::RetainedNativeOutcome::Aborted)
    );
    assert_eq!(
        engine.begin(&first).err(),
        Some(NativeFailure::OutcomePending)
    );
}

#[test]
fn data_result_substitution_and_uncertain_publication_never_approve_unvalidated_data() {
    for fault in 0..=3 {
        let f = Fixture::new();
        let mut h = head();
        h.generation = nz(8);
        h.active_request = Some(journal().request);
        h.startup_barrier = StartupBarrier::Inhibited;
        f.write(
            "release-501.registration",
            &encode_registration_head(&h).unwrap(),
        );
        let mut request = journal();
        request.operation = NativeOperationKind::Activate;
        request.desired_state = DesiredServerState::Running;
        request.candidate_installation = head().staged_installation;
        request.candidate_manifest_sha256 = Some([8; 32]);
        request.state = Phase::Migrating;
        f.write("release-501.journal", &request.encode().unwrap());
        let store = DataStore {
            expected: request.clone(),
            validated: DataGenerationId::new(if fault == 0 { 3 } else { 2 }).unwrap(),
            compatibility: AtomicUsize::new(0),
            validations: AtomicUsize::new(0),
        };
        let engine = f.engine(Duration::from_secs(30));
        let storage = NativeHeadStorage::new(
            engine.begin(&request).unwrap(),
            FaultSync {
                parents: AtomicUsize::new(0),
                fail_parent: fault,
            },
        );
        let result =
            ready_future(storage.record_migrated_data(DataGenerationId::new(2).unwrap(), &store));
        if fault == 0 {
            assert_eq!(
                result,
                PublicationDisposition::NotApplied(NativeFailure::IdentityMismatch)
            );
            assert_eq!(f.store().read_journal(key()).unwrap(), request);
        } else {
            assert_eq!(result, PublicationDisposition::Indeterminate);
            drop(storage);
            let recreated = f.engine(Duration::from_secs(30));
            assert_eq!(
                recreated.begin(&request).err(),
                Some(NativeFailure::Indeterminate)
            );
            assert!(recreated.authorize_release(&context(), &()).is_err());
            let recovery = NativeHeadStorage::new(
                recreated.begin_recovery(&request).unwrap(),
                SystemRecordSync,
            );
            recovery.recover_journal().unwrap();
            if f.store().read_journal(key()).unwrap().state == Phase::MigrationReady {
                recovery.recover_data_result().unwrap();
                assert_eq!(
                    f.store().read_snapshot(key()).unwrap().head.data_high_water,
                    2
                );
            }
            assert_eq!(result, PublicationDisposition::Indeterminate);
        }
        assert_eq!(
            f.store()
                .read_snapshot(key())
                .unwrap()
                .head
                .approved_data_generation,
            head().approved_data_generation
        );
    }
}

#[test]
fn separately_authorized_repair_can_reconcile_a_standalone_uncertain_journal() {
    let f = Fixture::new();
    let mut interrupted = staging_journal();
    interrupted.state = Phase::RecoveryRequired;
    f.write("release-501.journal", &interrupted.encode().unwrap());
    let mut repair = journal();
    repair.request = NativeRequestId::new([90; 16]).unwrap();
    repair.operation = NativeOperationKind::Repair;
    repair.desired_state = head().desired_state;
    let engine = f.engine(Duration::from_secs(30));
    repair.assessment_digest = Some(f.store().assess_repair(key()).unwrap());
    assert_eq!(
        engine.repair(&repair, SystemRecordSync),
        PublicationDisposition::Durable(
            spaceterm_host::activation_journal::RetainedNativeOutcome::Applied
        )
    );
    assert_eq!(
        engine.resolve(&interrupted),
        Ok(spaceterm_host::activation_journal::RetainedNativeOutcome::Aborted)
    );
    assert_eq!(
        f.store().read_snapshot(key()).unwrap().head.startup_barrier,
        StartupBarrier::Open
    );
    let before = std::fs::read(f.root.join("release-501.registration")).unwrap();
    assert_eq!(
        engine.repair(&repair, SystemRecordSync),
        PublicationDisposition::Durable(
            spaceterm_host::activation_journal::RetainedNativeOutcome::Applied
        )
    );
    assert_eq!(
        std::fs::read(f.root.join("release-501.registration")).unwrap(),
        before
    );
}

fn interrupted_install(f: &Fixture, after_commit: bool) -> ActivationJournalRecord {
    use spaceterm_host::native_registration::writer::RegistrationPublicationStorage;
    let mut request = staging_journal();
    request.operation = NativeOperationKind::Install;
    request.candidate_installation = Some(InstallationId(nz(13)));
    request.candidate_manifest_sha256 = Some([8; 32]);
    request.candidate_data_generation = Some(DataGenerationId::new(2).unwrap());
    request.state = Phase::Staged;
    f.write("release-501.journal", &request.encode().unwrap());
    let mut committed = head();
    committed.generation = nz(8);
    committed.approved_installation = request.candidate_installation.unwrap();
    committed.approved_data_generation = request.candidate_data_generation.unwrap();
    committed.approved_manifest_sha256 = request.candidate_manifest_sha256.unwrap();
    committed.approved_descriptor_sha256 = [18; 32];
    committed.active_request = Some(request.request);
    committed.startup_barrier = StartupBarrier::ApprovedStartOnly;
    committed.installation_high_water = 13;
    committed.data_high_water = 2;
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(engine.begin(&request).unwrap(), SystemRecordSync);
    storage
        .write_fresh(key(), &encode_registration_head(&committed).unwrap())
        .unwrap();
    if after_commit {
        storage.replace_head(key()).unwrap();
        storage.sync_parent().unwrap();
    }
    request
}
fn repair_request(f: &Fixture) -> ActivationJournalRecord {
    let h = f.store().read_snapshot(key()).unwrap().head;
    let mut repair = journal();
    repair.request = NativeRequestId::new([91; 16]).unwrap();
    repair.operation = NativeOperationKind::Repair;
    repair.desired_state = h.desired_state;
    repair.original_installation = h.approved_installation;
    repair.original_data_generation = h.approved_data_generation;
    repair.expected_registration_generation = h.generation;
    repair.expected_lifecycle_generation = h.lifecycle_generation;
    repair.assessment_digest = Some(f.store().assess_repair(key()).unwrap());
    repair
}

#[test]
fn administrative_repair_burns_precommit_allocations_and_never_rolls_back_committed_selection() {
    use spaceterm_host::activation_journal::RetainedNativeOutcome;
    for committed in [false, true] {
        let f = Fixture::new();
        let original = interrupted_install(&f, committed);
        let repair = repair_request(&f);
        let before = f.store().read_snapshot(key()).unwrap().head;
        let engine = f.engine(Duration::from_secs(30));
        assert!(engine.begin(&repair).is_err());
        if committed {
            let mut rollback = repair.clone();
            rollback.original_installation = original.original_installation;
            rollback.original_data_generation = original.original_data_generation;
            assert_eq!(
                engine.repair(&rollback, SystemRecordSync),
                PublicationDisposition::NotApplied(NativeFailure::StaleAssessment)
            );
            assert_eq!(f.store().read_snapshot(key()).unwrap().head, before);
        }
        assert_eq!(
            engine.repair(&repair, SystemRecordSync),
            PublicationDisposition::Durable(RetainedNativeOutcome::Applied)
        );
        let selected = f.store().read_snapshot(key()).unwrap();
        assert_eq!(
            selected.head.approved_installation,
            before.approved_installation
        );
        assert_eq!(
            selected.head.approved_data_generation,
            before.approved_data_generation
        );
        assert_eq!(selected.head.installation_high_water, 13);
        assert_eq!(selected.head.data_high_water, 2);
        assert_eq!(
            selected.head.lifecycle_generation,
            before.lifecycle_generation
        );
        if committed {
            assert_eq!(selected.journal.unwrap().state, Phase::ApprovalCommitted);
            assert_eq!(selected.head.active_request, Some(original.request));
            assert_eq!(
                selected.head.startup_barrier,
                StartupBarrier::ApprovedStartOnly
            );
        } else {
            assert_eq!(
                engine.resolve(&original),
                Ok(RetainedNativeOutcome::Aborted)
            );
            assert_eq!(selected.head.active_request, None);
            assert_eq!(selected.head.startup_barrier, StartupBarrier::Open);
            let mut next = staging_journal();
            next.request = NativeRequestId::new([92; 16]).unwrap();
            next.expected_registration_generation = selected.head.generation;
            drop(engine.begin(&next).unwrap());
            assert_eq!(
                engine.resolve(&original),
                Ok(RetainedNativeOutcome::Aborted)
            );
        }
        assert_eq!(engine.resolve(&repair), Ok(RetainedNativeOutcome::Applied));
    }
}

#[test]
fn failed_administrative_repair_remains_bound_to_its_authorized_request_until_settled() {
    use spaceterm_host::activation_journal::RetainedNativeOutcome;
    for file_failure in [false, true] {
        for fault in 1..=5 {
            let f = Fixture::new();
            let original = interrupted_install(&f, false);
            let repair = repair_request(&f);
            let engine = f.engine(Duration::from_secs(30));
            let result = if file_failure {
                engine.repair(
                    &repair,
                    FileFaultSync {
                        calls: AtomicUsize::new(0),
                        fail_file: fault,
                    },
                )
            } else {
                engine.repair(
                    &repair,
                    FaultSync {
                        parents: AtomicUsize::new(0),
                        fail_parent: fault,
                    },
                )
            };
            assert_eq!(
                result,
                PublicationDisposition::Indeterminate,
                "repair sync boundary {fault}"
            );
            let recreated = f.engine(Duration::from_secs(30));
            assert_eq!(
                recreated.begin(&original).err(),
                Some(NativeFailure::Indeterminate)
            );
            assert!(recreated.authorize_release(&context(), &()).is_err());
            let mut other = repair.clone();
            other.request = NativeRequestId::new([93; 16]).unwrap();
            assert_eq!(
                recreated.repair(&other, SystemRecordSync),
                PublicationDisposition::NotApplied(NativeFailure::IdentityMismatch)
            );
            assert_eq!(
                recreated.repair(&repair, SystemRecordSync),
                PublicationDisposition::Durable(RetainedNativeOutcome::Applied)
            );
            assert_eq!(result, PublicationDisposition::Indeterminate);
            assert_eq!(
                recreated.resolve(&original),
                Ok(RetainedNativeOutcome::Aborted)
            );
            assert_eq!(
                f.store().read_snapshot(key()).unwrap().head.data_high_water,
                2
            );
        }
    }
}

#[test]
fn repair_rejects_expired_authority_changed_assessment_and_replaced_progress_objects() {
    for fault in 0..3 {
        let f = Fixture::new();
        interrupted_install(&f, false);
        let repair = repair_request(&f);
        let before = std::fs::read(f.root.join("release-501.registration")).unwrap();
        let engine = f.engine(if fault == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs(30)
        });
        if fault == 1 {
            f.controls.revoked.store(true, Ordering::SeqCst);
        } else if fault == 2 {
            let bytes = std::fs::read(f.root.join("release-501.head-publication")).unwrap();
            f.write("release-501.head-publication", &bytes);
        }
        let result = engine.repair(&repair, SystemRecordSync);
        assert_eq!(
            result,
            PublicationDisposition::NotApplied(match fault {
                0 => NativeFailure::TimedOut,
                1 => NativeFailure::AuthorizationDenied,
                _ => NativeFailure::StaleAssessment,
            })
        );
        assert_eq!(
            std::fs::read(f.root.join("release-501.registration")).unwrap(),
            before
        );
        assert!(!f.root.join("release-501.journal-publication").exists());
    }
}

#[test]
fn interrupted_replacement_admission_preserves_the_previous_terminal_request() {
    use spaceterm_host::activation_journal::RetainedNativeOutcome;
    for fault in 1..=3 {
        let f = Fixture::new();
        let first = staging_journal();
        let mut completed = first.clone();
        completed.candidate_installation = head().staged_installation;
        completed.candidate_manifest_sha256 = Some([8; 32]);
        completed = completed
            .transition(Phase::Staged)
            .unwrap()
            .transition(Phase::Completed)
            .unwrap();
        f.write("release-501.journal", &completed.encode().unwrap());
        let mut next = journal();
        next.request = NativeRequestId::new([94; 16]).unwrap();
        let engine = f.engine(Duration::from_secs(30));
        assert_eq!(
            engine
                .begin_with_sync(
                    &next,
                    FaultSync {
                        parents: AtomicUsize::new(0),
                        fail_parent: fault
                    }
                )
                .err(),
            Some(NativeFailure::Indeterminate)
        );
        let recreated = f.engine(Duration::from_secs(30));
        let recovery =
            NativeHeadStorage::new(recreated.begin_recovery(&next).unwrap(), SystemRecordSync);
        recovery.recover_journal().unwrap();
        drop(recovery);
        assert_eq!(recreated.resolve(&first), Ok(RetainedNativeOutcome::Staged));
        drop(recreated.begin(&next).unwrap());
        assert_eq!(recreated.resolve(&first), Ok(RetainedNativeOutcome::Staged));
    }
}

#[test]
fn retained_journal_storage_preserves_unsupported_schema_classification() {
    let f = Fixture::new();
    let mut record: serde_json::Value =
        serde_json::from_slice(&journal().encode().unwrap()).unwrap();
    record["schema_version"] = 2.into();
    f.write("release-501.journal", &serde_json::to_vec(&record).unwrap());
    assert_eq!(
        f.store().read_journal(key()),
        Err(NativeFailure::UnsupportedSchema)
    );
}

#[test]
fn interrupted_data_result_cannot_be_aborted_before_its_allocation_mark_is_covered() {
    let f = Fixture::new();
    let mut h = head();
    h.generation = nz(8);
    h.active_request = Some(journal().request);
    h.startup_barrier = StartupBarrier::Inhibited;
    f.write(
        "release-501.registration",
        &encode_registration_head(&h).unwrap(),
    );
    let mut request = journal();
    request.operation = NativeOperationKind::Activate;
    request.desired_state = DesiredServerState::Running;
    request.candidate_installation = head().staged_installation;
    request.candidate_manifest_sha256 = Some([8; 32]);
    request.state = Phase::Migrating;
    f.write("release-501.journal", &request.encode().unwrap());
    let owner = DataStore {
        expected: request.clone(),
        validated: DataGenerationId::new(2).unwrap(),
        compatibility: AtomicUsize::new(0),
        validations: AtomicUsize::new(0),
    };
    let engine = f.engine(Duration::from_secs(30));
    let storage = NativeHeadStorage::new(
        engine.begin(&request).unwrap(),
        FaultSync {
            parents: AtomicUsize::new(0),
            fail_parent: 2,
        },
    );
    assert_eq!(
        ready_future(storage.record_migrated_data(owner.validated, &owner)),
        PublicationDisposition::Indeterminate
    );
    drop(storage);
    let recovery =
        NativeHeadStorage::new(engine.begin_recovery(&request).unwrap(), SystemRecordSync);
    recovery.recover_journal().unwrap();
    drop(recovery);
    assert!(f.store().assess_repair(key()).is_ok());
    let recovery =
        NativeHeadStorage::new(engine.begin_recovery(&request).unwrap(), SystemRecordSync);
    let aborted = f
        .store()
        .read_journal(key())
        .unwrap()
        .transition(Phase::Aborted)
        .unwrap();
    assert_eq!(
        recovery.advance_journal(&aborted),
        PublicationDisposition::NotApplied(NativeFailure::Indeterminate)
    );
    recovery.recover_data_result().unwrap();
    assert_eq!(
        recovery.advance_journal(&aborted),
        PublicationDisposition::Durable(())
    );
    assert_eq!(
        recovery.finish_request(),
        PublicationDisposition::Durable(
            spaceterm_host::activation_journal::RetainedNativeOutcome::Aborted
        )
    );
    assert_eq!(
        f.store().read_snapshot(key()).unwrap().head.data_high_water,
        2
    );
}
