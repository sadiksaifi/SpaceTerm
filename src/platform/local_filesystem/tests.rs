use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

impl LocalFilesystemAuthority {
    pub(crate) fn testing() -> Self {
        Self::new(Arc::new(FixtureIdentities))
    }

    pub(crate) fn testing_without_access() -> Self {
        struct NoAccess;
        impl LocalIdentitySource for NoAccess {
            fn identify(&self, _: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
                panic!("this context has no local filesystem authority")
            }
        }
        Self::new(Arc::new(NoAccess))
    }

    pub(crate) fn testing_with_failure(error: LocalFilesystemError) -> Self {
        struct Unavailable(LocalFilesystemError);
        impl LocalIdentitySource for Unavailable {
            fn identify(&self, _: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
                Err(self.0)
            }
        }
        Self::new(Arc::new(Unavailable(error)))
    }

    pub(crate) fn same_source(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "spaceterm-local-authority-{}-{}",
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
fn errors_and_retained_identities_do_not_disclose_content() {
    let root = Fixture::new();
    let authority = LocalFilesystemAuthority::testing();
    let directory = authority.validate_workspace_directory(&root.0).unwrap();
    assert_eq!(
        format!("{:?}", directory.identity()),
        "WorkspaceDirectoryIdentity(LocalObjectIdentity(<redacted>))"
    );
    let error = classify_io_error(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private fixture path",
    ));
    assert_eq!(format!("{error}"), "permission denied");
    assert_eq!(format!("{error:?}"), "PermissionDenied");
    let unavailable =
        ValidatedWorkspaceDirectory::new(root.0.clone(), WorkspaceDirectoryIdentity::unavailable());
    assert_eq!(
        authority.revalidate_workspace_directory(&unavailable),
        Err(LocalFilesystemError::IdentityChanged)
    );
    assert_eq!(
        WorkspaceDirectoryIdentity::for_test(31),
        WorkspaceDirectoryIdentity::for_test(31)
    );
    assert_ne!(
        directory.identity(),
        WorkspaceDirectoryIdentity::for_test(31)
    );
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
    let authority = LocalFilesystemAuthority::new(source.clone());
    assert_eq!(
        authority.validate_workspace_directory(&root.0),
        Err(LocalFilesystemError::IdentityChanged)
    );
    assert!(source.0.lock().unwrap().is_empty());
}

#[test]
fn identity_failures_remain_closed_and_do_not_fall_back_to_path_equality() {
    let root = Fixture::new();
    let authority =
        LocalFilesystemAuthority::new(Arc::new(ScriptedIdentities(Mutex::new(VecDeque::from([
            Err(LocalFilesystemError::PermissionDenied),
        ])))));
    assert_eq!(
        authority.validate_workspace_directory(&root.0),
        Err(LocalFilesystemError::PermissionDenied)
    );
}

#[test]
fn file_validation_rejects_missing_non_file_malformed_and_oversized_targets() {
    let root = Fixture::new();
    let authority = LocalFilesystemAuthority::testing();
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
    let file = LocalFilesystemAuthority::testing()
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
    let authority = LocalFilesystemAuthority::testing();
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

/// Fixture paths have logical identities only; replacement/retention is native evidence.
struct FixtureIdentities;
impl LocalIdentitySource for FixtureIdentities {
    fn identify(&self, path: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
        let canonical = path.canonicalize().map_err(classify_io_error)?;
        let metadata = fs::metadata(&canonical).map_err(classify_io_error)?;
        let kind = if metadata.is_dir() {
            LocalObjectKind::Directory
        } else if metadata.is_file() {
            LocalObjectKind::File
        } else {
            LocalObjectKind::Other
        };
        Ok(LocalIdentityObservation {
            identity: LocalObjectIdentity(IdentityValue::FixturePath(canonical), None),
            kind,
        })
    }
}
