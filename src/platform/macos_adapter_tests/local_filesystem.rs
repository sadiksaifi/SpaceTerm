//! Native retained identity, permissions, and filesystem integration evidence.
use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "spaceterm-native-local-authority-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn directory_identity_preserves_selected_spelling_and_equivalent_paths() {
    let root = Fixture::new();
    let target = root.0.join("target");
    let selected = root.0.join("selected");
    fs::create_dir(&target).unwrap();
    symlink(&target, &selected).unwrap();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    let directory = authority.validate_directory(&selected).unwrap();
    let equivalent = authority.validate_directory(&target).unwrap();
    assert_eq!(directory.path(), selected);
    assert_eq!(directory.identity(), equivalent.identity());
    let dotted = selected.join(".");
    let exact = authority.validate_directory(&dotted).unwrap();
    assert_eq!(exact.path().as_os_str(), dotted.as_os_str());
    assert_eq!(exact.identity(), directory.identity());
}

#[test]
fn directory_revalidation_rejects_retarget_replacement_removal_and_file() {
    let root = Fixture::new();
    let target = root.0.join("target");
    let other = root.0.join("other");
    let selected = root.0.join("selected");
    fs::create_dir(&target).unwrap();
    fs::create_dir(&other).unwrap();
    symlink(&target, &selected).unwrap();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    let directory = authority.validate_directory(&selected).unwrap();
    fs::remove_file(&selected).unwrap();
    symlink(&other, &selected).unwrap();
    assert_eq!(
        authority.revalidate_directory(&directory),
        Err(LocalFilesystemError::IdentityChanged)
    );
    fs::remove_file(&selected).unwrap();
    symlink(&target, &selected).unwrap();
    assert!(authority.revalidate_directory(&directory).is_ok());
    fs::remove_dir(&target).unwrap();
    assert_eq!(
        authority.revalidate_directory(&directory),
        Err(LocalFilesystemError::Missing)
    );
    fs::create_dir(&target).unwrap();
    assert_eq!(
        authority.revalidate_directory(&directory),
        Err(LocalFilesystemError::IdentityChanged)
    );
    fs::remove_dir(&target).unwrap();
    fs::write(&target, b"fixture").unwrap();
    assert_eq!(
        authority.revalidate_directory(&directory),
        Err(LocalFilesystemError::NotDirectory)
    );
}

#[test]
fn directory_validation_rejects_relative_malformed_and_unreadable_paths() {
    let root = Fixture::new();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    assert_eq!(
        authority.validate_directory(Path::new("relative")),
        Err(LocalFilesystemError::NotAbsolute)
    );
    assert_eq!(
        authority.validate_directory(&root.0.join("invalid\0")),
        Err(LocalFilesystemError::Malformed)
    );
    let unreadable = root.0.join("unreadable");
    fs::create_dir(&unreadable).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let result = authority.validate_directory(&unreadable);
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result, Err(LocalFilesystemError::PermissionDenied));
}

#[test]
fn errors_and_retained_identities_do_not_disclose_content() {
    let root = Fixture::new();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    let directory = authority.validate_directory(&root.0).unwrap();
    assert_eq!(
        format!("{:?}", directory.identity()),
        "LocalDirectoryIdentity(LocalObjectIdentity(<redacted>))"
    );
    let error = classify_io_error(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private fixture path",
    ));
    assert_eq!(format!("{error}"), "permission denied");
    assert_eq!(format!("{error:?}"), "PermissionDenied");
    let unavailable =
        ValidatedLocalDirectory::new(root.0.clone(), LocalDirectoryIdentity::unavailable());
    assert_eq!(
        authority.revalidate_directory(&unavailable),
        Err(LocalFilesystemError::IdentityChanged)
    );
    assert_eq!(
        LocalDirectoryIdentity::for_test(31),
        LocalDirectoryIdentity::for_test(31)
    );
    assert_ne!(directory.identity(), LocalDirectoryIdentity::for_test(31));
}

struct ScriptedIdentities(Mutex<VecDeque<Result<LocalIdentityObservation, LocalFilesystemError>>>);

impl LocalIdentitySource for ScriptedIdentities {
    fn identify(&self, _: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected identity request")
    }
}

fn observation(
    label: u64,
    kind: LocalObjectKind,
) -> Result<LocalIdentityObservation, LocalFilesystemError> {
    Ok(LocalIdentityObservation {
        identity: LocalObjectIdentity(IdentityValue::Fixture(label), None),
        kind,
    })
}

#[test]
fn directory_validation_rejects_a_replacement_during_readability_check() {
    let root = Fixture::new();
    let source = Arc::new(ScriptedIdentities(Mutex::new(VecDeque::from([
        observation(1, LocalObjectKind::Directory),
        observation(2, LocalObjectKind::Directory),
    ]))));
    let authority =
        LocalFilesystemAuthority::new(crate::local_path::LocalPathSemantics::Posix, source.clone());
    assert_eq!(
        authority.validate_directory(&root.0),
        Err(LocalFilesystemError::IdentityChanged)
    );
    assert!(source.0.lock().unwrap().is_empty());
}

#[test]
fn identity_failures_remain_closed_and_do_not_fall_back_to_path_equality() {
    let root = Fixture::new();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(ScriptedIdentities(Mutex::new(VecDeque::from([Err(
            LocalFilesystemError::PermissionDenied,
        )])))),
    );
    assert_eq!(
        authority.validate_directory(&root.0),
        Err(LocalFilesystemError::PermissionDenied)
    );
}

#[test]
fn local_file_authority_rejects_symlink_retargeting_and_successor_objects() {
    let root = Fixture::new();
    let target = root.0.join("target");
    let other = root.0.join("other");
    let selected = root.0.join("selected");
    fs::write(&target, b"first").unwrap();
    fs::write(&other, b"other").unwrap();
    symlink(&target, &selected).unwrap();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    let file = authority.local_file("selected", &root.0).unwrap();
    assert_eq!(
        file.revalidated_path(),
        Some(target.canonicalize().unwrap())
    );
    fs::remove_file(&selected).unwrap();
    symlink(&other, &selected).unwrap();
    assert!(file.revalidated_path().is_none());
    fs::remove_file(&selected).unwrap();
    symlink(&target, &selected).unwrap();
    fs::remove_file(&target).unwrap();
    fs::write(&target, b"replacement").unwrap();
    assert!(file.revalidated_path().is_none());
}

#[test]
fn file_identity_fails_closed_when_host_refuses_retention() {
    let root = Fixture::new();
    let path = root.0.join("file");
    fs::write(&path, b"private").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    assert!(authority.local_file("file", &root.0).is_none());
}

#[test]
fn file_validation_rejects_missing_non_file_malformed_and_oversized_targets() {
    let root = Fixture::new();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    for value in ["missing", ".", "bad\0", "bad\n", ""] {
        assert!(authority.local_file(value, &root.0).is_none());
    }
    assert!(
        authority
            .local_file(&"x".repeat(MAX_LOCAL_FILE_PATH_BYTES + 1), &root.0)
            .is_none()
    );
    assert!(
        authority
            .local_file("file", Path::new("relative"))
            .is_none()
    );
}

#[test]
fn emission_metadata_is_content_private_scoped_versioned_and_bounded() {
    let root = Fixture::new();
    let path = root.0.join("private-name");
    fs::write(&path, b"contents").unwrap();
    let file = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    )
    .local_file("private-name", &root.0)
    .unwrap();
    let mut registry = LocalFileEmissionRegistry::default();
    let token = registry.emit(&file).unwrap();
    assert_eq!(token.len(), 24);
    assert_eq!(registry.restore(&token), Some(file.clone()));
    assert!(
        LocalFileEmissionRegistry::default()
            .restore(&token)
            .is_none()
    );
    let mut malformed = token.clone();
    malformed[7] = 1;
    assert!(registry.restore(&malformed).is_none());
    assert!(registry.restore(&[0; 4097]).is_none());
    assert!(registry.restore(&token[..23]).is_none());
    fs::remove_file(path).unwrap();
    assert!(
        registry
            .restore(&token)
            .unwrap()
            .revalidated_path()
            .is_none()
    );
}

#[test]
fn emission_eviction_revokes_old_metadata_without_reusing_its_authority() {
    let root = Fixture::new();
    let path = root.0.join("file");
    fs::write(&path, b"fixture").unwrap();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    let mut registry = LocalFileEmissionRegistry::default();
    let first_file = authority.local_file("file", &root.0).unwrap();
    let first = registry.emit(&first_file).unwrap();
    for label in 0..MAX_EMITTED_LOCAL_FILES {
        let file = ValidatedLocalFile(Arc::new(LocalFileState {
            selected: path.clone(),
            canonical: path.clone(),
            identity: LocalObjectIdentity(IdentityValue::Fixture(label as u64), None),
            authority: authority.clone(),
            _permit: authority.files.reserve(MAX_LOCAL_FILE_LEASES).unwrap(),
        }));
        registry.emit(&file).unwrap();
    }
    assert_eq!(registry.files.len(), MAX_EMITTED_LOCAL_FILES);
    assert!(registry.restore(&first).is_none());
    // A snapshot already holding the original typed target still retains its own exact lease.
    assert!(first_file.revalidated_path().is_some());
    let next = registry.emit(&first_file).unwrap();
    assert_ne!(first, next);
    assert!(registry.restore(&first).is_none());
}

#[test]
fn local_file_leases_preserve_descriptor_headroom_across_emulators_and_snapshots() {
    const CHILD: &str = "SPACETERM_TEST_LOCAL_FILESYSTEM_LIMIT";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::local_filesystem::macos_adapter_tests::local_file_leases_preserve_descriptor_headroom_across_emulators_and_snapshots",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // SAFETY: only this isolated test subprocess changes its own descriptor limit.
    let limit = libc::rlimit {
        rlim_cur: 256,
        rlim_max: 256,
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let root = Fixture::new();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    let mut registries: Vec<_> = (0..4)
        .map(|_| LocalFileEmissionRegistry::default())
        .collect();
    let mut snapshots = Vec::new();
    for index in 0..MAX_LOCAL_FILE_LEASES {
        let name = format!("file-{index}");
        fs::write(root.0.join(&name), b"fixture").unwrap();
        let file = authority.clone().local_file(&name, &root.0).unwrap();
        let registry = &mut registries[index % 4];
        registry.prepare_resolution();
        let token = registry.emit(&file).unwrap();
        snapshots.push(registry.restore(&token).unwrap());
    }
    for _ in 0..1024 {
        assert!(authority.local_file("file-0", &root.0).is_none());
    }
    // Output cannot consume the descriptors reserved for PTYs, sockets and directory work.
    let infrastructure: Vec<_> = (0..128).map(|_| fs::File::open(&root.0).unwrap()).collect();
    let directory = authority.validate_directory(&root.0).unwrap();
    assert!(authority.revalidate_directory(&directory).is_ok());
    assert!(snapshots[0].revalidated_path().is_some());
    drop(infrastructure);
    drop(registries);
    assert!(authority.local_file("file-0", &root.0).is_none());
    snapshots.pop();
    assert!(authority.local_file("file-0", &root.0).is_some());
    drop(snapshots);
    assert!(authority.local_file("file-0", &root.0).is_some());
}

#[test]
fn registry_eviction_releases_real_handles_before_resolving_more_output() {
    let root = Fixture::new();
    let authority = LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        Arc::new(crate::platform::macos_local_identity::MacosLocalIdentity),
    );
    let mut registry = LocalFileEmissionRegistry::default();
    let mut first = None;
    for index in 0..256 {
        let name = format!("file-{index}");
        fs::write(root.0.join(&name), b"fixture").unwrap();
        registry.prepare_resolution();
        let file = authority.local_file(&name, &root.0).unwrap();
        let token = registry.emit(&file).unwrap();
        first.get_or_insert(token);
    }
    assert!(registry.restore(&first.unwrap()).is_none());
    assert_eq!(registry.files.len(), MAX_EMITTED_LOCAL_FILES);
    drop(registry);
    assert_eq!(authority.handles.0.load(Ordering::Acquire), 0);
    assert_eq!(authority.files.0.load(Ordering::Acquire), 0);
}
