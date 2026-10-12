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
