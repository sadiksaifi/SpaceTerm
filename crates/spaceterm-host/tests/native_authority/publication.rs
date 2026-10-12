use sha2::{Digest as _, Sha256};
use spaceterm_distribution::*;
use spaceterm_host::{
    native_deployment::{contracts::*, publication::*},
    protected_directory::{NativeDirectoryProtection, ProtectedDirectoryError},
};
use std::{
    fs::File,
    io::Write,
    num::NonZeroU64,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

mod archive_fixture;
use archive_fixture::*;

mod allocation_probe {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    thread_local! {
        static LARGEST: Cell<Option<usize>> = const { Cell::new(None) };
    }

    struct Allocator;
    #[global_allocator]
    static ALLOCATOR: Allocator = Allocator;

    fn record(size: usize) {
        // Per-thread measurement excludes concurrent fixtures and records no payload bytes.
        let _ = LARGEST.try_with(|largest| {
            if let Some(previous) = largest.get() {
                largest.set(Some(previous.max(size)));
            }
        });
    }

    // SAFETY: every allocation operation forwards the unchanged contract to System.
    unsafe impl GlobalAlloc for Allocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            // SAFETY: the caller supplies a valid allocation layout.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            // SAFETY: the caller supplies a valid allocation layout.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            // SAFETY: System allocated this pointer with the supplied layout.
            unsafe { System.dealloc(pointer, layout) }
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            record(size);
            // SAFETY: System allocated this pointer; the caller supplies a valid new size.
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    pub fn largest_during<T>(operation: impl FnOnce() -> T) -> (T, usize) {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                LARGEST.with(|largest| largest.set(None));
            }
        }
        LARGEST.with(|largest| {
            assert!(largest.replace(Some(0)).is_none());
        });
        let _reset = Reset;
        let result = operation();
        let largest = LARGEST.with(|largest| largest.get().unwrap());
        (result, largest)
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    Alias,
    FileProtection,
    ReplaceRestore,
    Mutate,
}
struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    selected: Mutex<Option<(PublicationIdentity, [u8; 32])>>,
    uncertain: Mutex<bool>,
    fault: Mutex<Fault>,
    transactions: AtomicUsize,
    native_closure: Mutex<[u8; 32]>,
    covered: Mutex<std::collections::BTreeSet<(u64, u64)>>,
    fresh_directories: Mutex<Vec<File>>,
}
impl Fixture {
    fn new() -> Arc<Self> {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        use std::os::unix::fs::MetadataExt;
        let covered = root
            .ancestors()
            .map(|p| {
                let m = File::open(p).unwrap().metadata().unwrap();
                (m.dev(), m.ino())
            })
            .collect();
        Arc::new(Self {
            _directory: directory,
            root,
            selected: Mutex::new(None),
            uncertain: Mutex::new(false),
            fault: Mutex::new(Fault::None),
            transactions: AtomicUsize::new(0),
            native_closure: Mutex::new([8; 32]),
            covered: Mutex::new(covered),
            fresh_directories: Mutex::new(Vec::new()),
        })
    }
    fn identity() -> PublicationIdentity {
        PublicationIdentity {
            key: AccountBuildKey {
                account_uid: 501,
                build_identity: BuildIdentity::Release,
            },
            installation: InstallationId(NonZeroU64::new(12).unwrap()),
            server_id: [4; 16],
            account_binding_sha256: [1; 32],
            registration_profile_sha256: [2; 32],
            publication_nonce: [3; 32],
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // Restore only our retained fresh fixture directories, so TempDir can unlink the
        // sealed tree. No pathname-selected object or host ancestor is changed.
        if let Ok(directories) = self.fresh_directories.get_mut() {
            for directory in directories {
                // SAFETY: these descriptors retain only directories this fixture created.
                unsafe {
                    libc::fchmod(directory.as_raw_fd(), 0o700);
                }
            }
        }
    }
}
impl NativeDirectoryProtection for Fixture {
    fn verify_directory(&self, _: &File) -> Result<(), ProtectedDirectoryError> {
        Ok(())
    }
    fn verify_regular_file(&self, _: &File) -> Result<(), ProtectedDirectoryError> {
        if *self.fault.lock().unwrap() == Fault::FileProtection {
            return Err(ProtectedDirectoryError::Unsafe);
        }
        Ok(())
    }
}
impl NativePublicationProtection for Fixture {
    fn record_fresh(&self, inode: &FreshPublicationInode<'_>) -> Result<(), NativeFailure> {
        use std::os::unix::fs::MetadataExt;
        let parent = inode.parent().metadata().unwrap();
        let mut covered = self.covered.lock().unwrap();
        if !covered.contains(&(parent.dev(), parent.ino())) {
            return Err(NativeFailure::LinuxPublicationUnavailable);
        }
        let m = inode.destination().metadata().unwrap();
        covered.insert((m.dev(), m.ino()));
        if m.is_dir() {
            self.fresh_directories
                .lock()
                .unwrap()
                .push(inode.destination().try_clone().unwrap());
        }
        Ok(())
    }
    fn verify_custody(&self, handles: &[&File]) -> Result<(), NativeFailure> {
        if *self.fault.lock().unwrap() == Fault::Alias {
            return Err(NativeFailure::LinuxPublicationUnavailable);
        }
        use std::os::unix::fs::MetadataExt;
        let covered = self.covered.lock().unwrap();
        for file in handles {
            let m = file.metadata().unwrap();
            if !covered.contains(&(m.dev(), m.ino())) {
                return Err(NativeFailure::LinuxPublicationUnavailable);
            }
        }
        Ok(())
    }
    fn authenticate_execution(
        &self,
        _: &InstallationManifestV1,
        files: &[PublicationFile<'_>],
    ) -> Result<[u8; 32], NativeFailure> {
        use std::os::fd::AsRawFd;
        for entry in files {
            // SAFETY: F_GETFL only observes this retained fixture descriptor.
            let flags = unsafe { libc::fcntl(entry.file().as_raw_fd(), libc::F_GETFL) };
            assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        }
        let mut fault = self.fault.lock().unwrap();
        let resource = self.root.join("12/share/sealed.conf");
        if *fault == Fault::ReplaceRestore {
            let original = resource.with_extension("retained");
            std::fs::rename(&resource, &original).unwrap();
            std::fs::write(&resource, CONTENT).unwrap();
            std::fs::remove_file(&resource).unwrap();
            std::fs::rename(original, &resource).unwrap();
            *fault = Fault::None;
        } else if *fault == Fault::Mutate {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&resource, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::write(&resource, vec![b'x'; CONTENT.len()]).unwrap();
            *fault = Fault::None;
        }
        Ok(*self.native_closure.lock().unwrap())
    }
}
struct Transaction(Arc<Fixture>);
impl Transaction {
    fn new(fixture: &Arc<Fixture>) -> Box<Self> {
        fixture.transactions.fetch_add(1, Ordering::SeqCst);
        Box::new(Self(fixture.clone()))
    }
}
impl Drop for Transaction {
    fn drop(&mut self) {
        self.0.transactions.fetch_sub(1, Ordering::SeqCst);
    }
}
impl NativePublicationTransaction for Transaction {
    fn revalidate(&self) -> Result<(), NativeFailure> {
        Ok(())
    }
}
struct Peer(Arc<Fixture>);
impl InstallationPublicationNative for Peer {
    type Request = ();
    type Registration = ();
    fn begin_publication(
        &self,
        _: &(),
        _: &VerifiedServerArchive,
    ) -> Result<PublicationScope, NativeFailure> {
        if *self.0.uncertain.lock().unwrap() {
            return Err(NativeFailure::Indeterminate);
        }
        PublicationScope::open(
            &self.0.root,
            Fixture::identity(),
            self.0.clone(),
            Transaction::new(&self.0),
        )
    }
    fn open_approved(&self, _: &()) -> Result<ApprovedPublication, NativeFailure> {
        let (identity, digest) = self
            .0
            .selected
            .lock()
            .unwrap()
            .clone()
            .ok_or(NativeFailure::RegistrationMissing)?;
        ApprovedPublication::new(
            PublicationScope::open(
                &self.0.root,
                identity,
                self.0.clone(),
                Transaction::new(&self.0),
            )?,
            digest,
        )
    }
    fn latch_uncertainty(&self) -> Result<(), NativeFailure> {
        assert_eq!(
            self.0.transactions.load(Ordering::SeqCst),
            1,
            "uncertainty must be latched before releasing native serialization"
        );
        *self.0.uncertain.lock().unwrap() = true;
        Ok(())
    }
}

#[test]
fn fresh_copy_does_not_adopt_account_writable_inodes() {
    use std::os::unix::fs::MetadataExt;
    for platform in [ServerPlatform::LinuxX86_64, ServerPlatform::DarwinArm64] {
        let fixture = Fixture::new();
        let source = fixture.root.join("account-source");
        let mut writer = File::create(&source).unwrap();
        writer.write_all(CONTENT).unwrap();
        let original = writer.metadata().unwrap();
        let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
        let source_bytes = std::fs::read(&source).unwrap();
        let verified = candidate_contents(platform, ArchiveFault::None, &source_bytes);
        let result = publisher.publish_fresh(&(), &verified);
        let PublicationDisposition::Durable(staged) = result else {
            panic!("fresh destination was not published: {result:?}")
        };
        let relative = match platform {
            ServerPlatform::LinuxX86_64 => "bin/spaceterm-server",
            ServerPlatform::DarwinArm64 => "SpaceTerm Server.app/Contents/MacOS/spaceterm-server",
        };
        let destination = fixture.root.join("12").join(relative);
        assert_ne!(
            std::fs::metadata(&destination).unwrap().ino(),
            original.ino()
        );
        writer.write_all(b"source-only mutation").unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), CONTENT);
        assert_eq!(
            staged.descriptor().server_artifact_sha256,
            Sha256::digest(CONTENT).as_slice()
        );
    }
}

fn publish(
    fixture: &Arc<Fixture>,
    platform: ServerPlatform,
) -> (FreshInstallationPublisher<Peer>, StagedInstallation) {
    let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
    let result = publisher.publish_fresh(&(), &candidate(platform));
    let PublicationDisposition::Durable(staged) = result else {
        panic!("publication did not complete: {result:?}")
    };
    (publisher, staged)
}
fn approve(fixture: &Fixture, staged: &StagedInstallation) {
    *fixture.selected.lock().unwrap() = Some((Fixture::identity(), *staged.descriptor_sha256()));
}

#[test]
fn destination_authentication_rejects_resource_substitution_during_native_inspection() {
    for fault in [Fault::ReplaceRestore, Fault::Mutate] {
        let fixture = Fixture::new();
        *fixture.fault.lock().unwrap() = fault;
        let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
        assert!(matches!(
            publisher.publish_fresh(&(), &candidate(ServerPlatform::LinuxX86_64)),
            PublicationDisposition::Indeterminate
        ));
        assert!(*fixture.uncertain.lock().unwrap());
        assert_eq!(fixture.transactions.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn approved_retention_authenticates_destination_and_binds_every_closure_file() {
    let resources = [
        "bin/spaceterm-server",
        "libexec/spaceterm-server-launcher",
        "libexec/spaceterm-server-manager",
        "libexec/credential-tools/systemd-creds",
        "lib/interpreter",
        "lib/libfixture.so",
        "providers/fixture.so",
        "share/sealed.conf",
        "share/native-evidence/fixture-target",
        "installation.json",
        "descriptor.json",
    ];
    for resource in resources {
        let fixture = Fixture::new();
        let (publisher, staged) = publish(&fixture, ServerPlatform::LinuxX86_64);
        approve(&fixture, &staged);
        let approved = publisher.retain_approved(&()).unwrap();
        approved.revalidate().unwrap();
        assert_eq!(
            approved.installation_binding(),
            &staged.descriptor().installation_binding().unwrap()
        );
        let path = fixture.root.join("12").join(resource);
        let replacement = path.with_extension("substitution");
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let original_bytes = std::fs::read(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
        let parent = path.parent().unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&replacement, original_bytes).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(mode)).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(approved.revalidate().is_err());
        assert!(approved.retained_server().is_err());
        assert!(publisher.retain_approved(&()).is_err());
    }
}

#[test]
fn retention_keeps_ancestors_and_never_reopens_a_replaced_generation() {
    let fixture = Fixture::new();
    let (publisher, staged) = publish(&fixture, ServerPlatform::DarwinArm64);
    approve(&fixture, &staged);
    let approved = publisher.retain_approved(&()).unwrap();
    std::fs::rename(fixture.root.join("12"), fixture.root.join("retired")).unwrap();
    std::fs::create_dir(fixture.root.join("12")).unwrap();
    assert!(approved.revalidate().is_err());
    assert!(staged.revalidate().is_err());
}

#[test]
fn native_alias_refusal_precedes_any_destination_effect() {
    let fixture = Fixture::new();
    *fixture.fault.lock().unwrap() = Fault::Alias;
    let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
    assert!(matches!(
        publisher.publish_fresh(&(), &candidate(ServerPlatform::LinuxX86_64)),
        PublicationDisposition::NotApplied(NativeFailure::LinuxPublicationUnavailable)
    ));
    assert!(!fixture.root.join("12").exists());
    assert!(!*fixture.uncertain.lock().unwrap());
}

#[test]
fn retained_policy_and_native_execution_binding_are_revalidated_through_use() {
    let fixture = Fixture::new();
    let (publisher, staged) = publish(&fixture, ServerPlatform::LinuxX86_64);
    approve(&fixture, &staged);
    let approved = publisher.retain_approved(&()).unwrap();
    *fixture.fault.lock().unwrap() = Fault::Alias;
    assert_eq!(
        approved.revalidate(),
        Err(NativeFailure::LinuxPublicationUnavailable)
    );
    *fixture.fault.lock().unwrap() = Fault::None;
    *fixture.native_closure.lock().unwrap() = [9; 32];
    assert_eq!(approved.revalidate(), Err(NativeFailure::ArtifactRejected));
}

#[test]
fn partial_publication_preserves_its_tree_and_latches_the_existing_native_journal() {
    let fixture = Fixture::new();
    *fixture.fault.lock().unwrap() = Fault::FileProtection;
    let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
    assert!(matches!(
        publisher.publish_fresh(&(), &candidate(ServerPlatform::LinuxX86_64)),
        PublicationDisposition::Indeterminate
    ));
    assert!(fixture.root.join("12").exists());
    assert_eq!(
        publisher.state(),
        spaceterm_host::native_registration::writer::PublicationState::RecoveryRequired
    );
    *fixture.fault.lock().unwrap() = Fault::None;
    assert!(matches!(
        publisher.publish_fresh(&(), &candidate(ServerPlatform::LinuxX86_64)),
        PublicationDisposition::NotApplied(NativeFailure::Indeterminate)
    ));
    let recreated = FreshInstallationPublisher::new(Peer(fixture.clone()));
    assert!(matches!(
        recreated.publish_fresh(&(), &candidate(ServerPlatform::LinuxX86_64)),
        PublicationDisposition::NotApplied(NativeFailure::Indeterminate)
    ));
}

#[test]
fn archive_links_special_files_paths_modes_and_bounds_reject_before_native_effects() {
    for fault in [
        ArchiveFault::Link,
        ArchiveFault::HardLink,
        ArchiveFault::Fifo,
        ArchiveFault::Traversal,
        ArchiveFault::Absolute,
        ArchiveFault::Unexpected,
        ArchiveFault::Mode,
        ArchiveFault::OversizeManifest,
        ArchiveFault::Missing,
    ] {
        let fixture = Fixture::new();
        let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
        assert!(matches!(
            publisher.publish_fresh(&(), &candidate_fault(ServerPlatform::LinuxX86_64, fault)),
            PublicationDisposition::NotApplied(_)
        ));
        assert!(!fixture.root.join("12").exists());
        assert_eq!(fixture.transactions.load(Ordering::SeqCst), 0);
        assert!(!*fixture.uncertain.lock().unwrap());
    }
}

#[test]
fn archive_extension_payloads_are_rejected_before_metadata_allocation() {
    let mut allocations = Vec::new();
    for kind in [
        tar::EntryType::GNULongName,
        tar::EntryType::GNULongLink,
        tar::EntryType::XHeader,
        tar::EntryType::XGlobalHeader,
    ] {
        let fixture = Fixture::new();
        // Authenticate compressed input before measuring the privileged publication read.
        // A compressible 4 MiB fixture exposes payload-sized allocations without an OOM test.
        let candidate = candidate_fault(
            ServerPlatform::LinuxX86_64,
            ArchiveFault::Extension(kind, 4 * 1024 * 1024),
        );
        let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
        let (result, largest) =
            allocation_probe::largest_during(|| publisher.publish_fresh(&(), &candidate));
        assert!(matches!(
            result,
            PublicationDisposition::NotApplied(NativeFailure::ArtifactRejected)
        ));
        allocations.push((kind, largest));
        assert!(!fixture.root.join("12").exists());
        assert_eq!(fixture.transactions.load(Ordering::SeqCst), 0);
        assert!(!*fixture.uncertain.lock().unwrap());
    }
    assert!(
        allocations.iter().all(|(_, largest)| *largest < 128 * 1024),
        "extension rejection allocated payload-sized buffers: {allocations:?}"
    );
}

#[test]
fn bounded_gnu_manifest_name_is_supported_but_oversized_names_reject() {
    let fixture = Fixture::new();
    let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
    assert!(matches!(
        publisher.publish_fresh(
            &(),
            &candidate_fault(ServerPlatform::LinuxX86_64, ArchiveFault::ManifestLongName)
        ),
        PublicationDisposition::Durable(_)
    ));
    let fixture = Fixture::new();
    let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
    assert!(matches!(
        publisher.publish_fresh(
            &(),
            &candidate_fault(
                ServerPlatform::LinuxX86_64,
                ArchiveFault::Extension(tar::EntryType::GNULongName, 1044)
            )
        ),
        PublicationDisposition::NotApplied(NativeFailure::ArtifactRejected)
    ));
    assert!(!fixture.root.join("12").exists());
}

#[test]
fn old_header_manifest_override_rejects_before_generation_creation() {
    use spaceterm_host::native_registration::writer::PublicationState;

    for platform in [ServerPlatform::LinuxX86_64, ServerPlatform::DarwinArm64] {
        let fixture = Fixture::new();
        let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
        // The archive and feed are signature-valid. The old/V7 type-L header
        // is not recognized as an extension by the ordinary copy parser.
        let candidate = candidate_fault(platform, ArchiveFault::ManifestOldLongName);
        let result = publisher.publish_fresh(&(), &candidate);
        assert!(
            matches!(
                result,
                PublicationDisposition::NotApplied(NativeFailure::ArtifactRejected)
            ),
            "unrecognized extension reached publication: result={result:?}, generation_created={}, uncertain={}, state={:?}",
            fixture.root.join("12").exists(),
            *fixture.uncertain.lock().unwrap(),
            publisher.state(),
        );
        assert!(!fixture.root.join("12").exists());
        assert!(fixture.fresh_directories.lock().unwrap().is_empty());
        assert_eq!(fixture.transactions.load(Ordering::SeqCst), 0);
        assert!(!*fixture.uncertain.lock().unwrap());
        assert_eq!(publisher.state(), PublicationState::Open);
    }
}

#[test]
fn publishing_never_adopts_or_changes_an_existing_generation() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root.join("12")).unwrap();
    std::fs::write(fixture.root.join("12/account-created"), CONTENT).unwrap();
    let publisher = FreshInstallationPublisher::new(Peer(fixture.clone()));
    assert!(matches!(
        publisher.publish_fresh(&(), &candidate(ServerPlatform::LinuxX86_64)),
        PublicationDisposition::NotApplied(NativeFailure::UnsafeNativeState)
    ));
    assert_eq!(
        std::fs::read(fixture.root.join("12/account-created")).unwrap(),
        CONTENT
    );
    assert!(!*fixture.uncertain.lock().unwrap());
}

#[test]
fn custody_outlives_the_operation_lock_and_staging_is_not_approval() {
    let fixture = Fixture::new();
    let (publisher, staged) = publish(&fixture, ServerPlatform::LinuxX86_64);
    assert_eq!(fixture.transactions.load(Ordering::SeqCst), 0);
    assert!(matches!(
        publisher.retain_approved(&()),
        Err(NativeFailure::RegistrationMissing)
    ));
    staged.revalidate().unwrap();
    approve(&fixture, &staged);
    let retained = publisher.retain_approved(&()).unwrap();
    assert_eq!(fixture.transactions.load(Ordering::SeqCst), 0);
    retained.revalidate().unwrap();
}

#[test]
fn approved_retention_requires_the_exact_native_head_descriptor_digest() {
    let fixture = Fixture::new();
    let (publisher, staged) = publish(&fixture, ServerPlatform::LinuxX86_64);
    approve(&fixture, &staged);
    fixture.selected.lock().unwrap().as_mut().unwrap().1[0] ^= 1;
    assert!(matches!(
        publisher.retain_approved(&()),
        Err(NativeFailure::ArtifactRejected)
    ));
}

#[test]
fn retained_inode_alias_rejects_even_when_bytes_and_modes_match() {
    let fixture = Fixture::new();
    let (publisher, staged) = publish(&fixture, ServerPlatform::LinuxX86_64);
    approve(&fixture, &staged);
    let retained = publisher.retain_approved(&()).unwrap();
    std::fs::hard_link(
        fixture.root.join("12/share/sealed.conf"),
        fixture.root.join("account-alias"),
    )
    .unwrap();
    assert!(retained.revalidate().is_err());
    assert!(staged.revalidate().is_err());
}
