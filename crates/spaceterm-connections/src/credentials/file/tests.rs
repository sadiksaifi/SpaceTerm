use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::{Arc, Barrier};

struct Fixture(PrivateFiles);
impl Fixture {
    fn new() -> Self {
        let mut id = [0; 32];
        getrandom::fill(&mut id).unwrap();
        Self(PrivateFiles(
            std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("spaceterm-credentials-{}", account(&id))),
        ))
    }
    fn path(&self, key: &[u8; 32]) -> PathBuf {
        self.0.0.join(account(key))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0.0);
    }
}
const KEY: [u8; 32] = [1; 32];

#[test]
fn creates_private_directory_and_file_and_removes_only_the_requested_secret() {
    let fixture = Fixture::new();
    let store = &fixture.0;
    assert_eq!(store.read(&KEY, Interaction::Quiet).unwrap(), None);
    store.remove(&KEY, Interaction::Quiet).unwrap();
    assert!(!store.0.exists());
    store.insert(&KEY, b"secret").unwrap();
    store.insert(&[2; 32], b"other").unwrap();
    assert_eq!(std::fs::metadata(&store.0).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        std::fs::metadata(fixture.path(&KEY)).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(
        store
            .read(&KEY, Interaction::Quiet)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"secret"
    );
    assert_eq!(
        store.insert(&KEY, b"replacement"),
        Err(CredentialError::Unavailable)
    );
    assert_eq!(
        store
            .read(&KEY, Interaction::Quiet)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"secret"
    );
    store.remove(&KEY, Interaction::Quiet).unwrap();
    store.remove(&KEY, Interaction::Quiet).unwrap();
    assert_eq!(store.read(&KEY, Interaction::Quiet).unwrap(), None);
    assert_eq!(
        store
            .read(&[2; 32], Interaction::Quiet)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"other"
    );
    assert_eq!(std::fs::read_dir(&store.0).unwrap().count(), 1);
}

#[test]
fn concurrent_insert_publishes_one_complete_immutable_secret() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|id| {
                let barrier = &barrier;
                let store = &fixture.0;
                scope.spawn(move || {
                    barrier.wait();
                    let secret = Zeroizing::new(vec![id; 32768]);
                    store.insert(&KEY, &secret)
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(Result::is_ok)
                .count(),
            1
        );
    });
    let bytes = fixture.0.read(&KEY, Interaction::Quiet).unwrap().unwrap();
    assert_eq!(bytes.len(), 32768);
    assert!(bytes.iter().all(|byte| *byte == bytes[0]));
    assert_eq!(std::fs::read_dir(&fixture.0.0).unwrap().count(), 1);
}

#[test]
fn refuses_loose_file_or_directory_permissions_for_every_operation() {
    for directory in [false, true] {
        let fixture = Fixture::new();
        fixture.0.insert(&KEY, b"secret").unwrap();
        let path = if directory {
            fixture.0.0.clone()
        } else {
            fixture.path(&KEY)
        };
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(if directory { 0o755 } else { 0o644 }),
        )
        .unwrap();
        assert_eq!(
            fixture.0.read(&KEY, Interaction::Quiet),
            Err(CredentialError::Unavailable)
        );
        assert_eq!(
            fixture.0.remove(&KEY, Interaction::Quiet),
            Err(CredentialError::Unavailable)
        );
        assert_eq!(
            fixture.0.insert(&KEY, b"replacement"),
            Err(CredentialError::Unavailable)
        );
        assert!(fixture.path(&KEY).exists());
    }
}

#[test]
fn refuses_file_directory_and_ancestor_symlinks() {
    let target = Fixture::new();
    target.0.insert(&KEY, b"secret").unwrap();
    let linked = Fixture::new();
    symlink(&target.0.0, &linked.0.0).unwrap();
    let ancestor = PrivateFiles(linked.0.0.join("nested"));
    for store in [&linked.0, &ancestor] {
        assert_eq!(
            store.read(&KEY, Interaction::Quiet),
            Err(CredentialError::Unavailable)
        );
        assert_eq!(
            store.insert(&KEY, b"replacement"),
            Err(CredentialError::Unavailable)
        );
        assert_eq!(
            store.remove(&KEY, Interaction::Quiet),
            Err(CredentialError::Unavailable)
        );
    }
    std::fs::remove_file(&linked.0.0).unwrap();
    linked.0.insert(&[2; 32], b"other").unwrap();
    symlink(target.path(&KEY), linked.path(&KEY)).unwrap();
    assert_eq!(
        linked.0.read(&KEY, Interaction::Quiet),
        Err(CredentialError::Unavailable)
    );
    assert_eq!(
        linked.0.remove(&KEY, Interaction::Quiet),
        Err(CredentialError::Unavailable)
    );
    assert_eq!(
        linked.0.insert(&KEY, b"replacement"),
        Err(CredentialError::Unavailable)
    );
    assert_eq!(
        target
            .0
            .read(&KEY, Interaction::Quiet)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"secret"
    );
}

#[test]
fn cli_falls_back_only_when_native_is_unavailable() {
    let fixture = Fixture::new();
    let fallback = || Ok(Arc::new(PrivateFiles(fixture.0.0.clone())) as Arc<dyn CredentialStore>);
    let store = crate::credentials::cli_store(Err(CredentialError::Unavailable), fallback).unwrap();
    assert_eq!(store.kind(), StoreKind::PrivateFile);
    assert_eq!(
        crate::credentials::cli_store(Err(CredentialError::InteractionRequired), || panic!(
            "must not bypass native interaction"
        ))
        .err(),
        Some(CredentialError::InteractionRequired)
    );
    let native: Arc<dyn CredentialStore> = Arc::new(crate::credentials::Unavailable);
    let selected =
        crate::credentials::cli_store(Ok(native.clone()), || panic!("native exists")).unwrap();
    assert!(Arc::ptr_eq(&native, &selected));
    assert_eq!(
        selected.read(&KEY, Interaction::Quiet),
        Err(CredentialError::Unavailable)
    );
    assert!(!fixture.0.0.exists());
}

#[test]
fn refuses_nonregular_entries_without_blocking_or_deleting_them() {
    let fixture = Fixture::new();
    fixture.0.insert(&[2; 32], b"other").unwrap();
    let path = fixture.path(&KEY);
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the terminated path is writable in this test's own temporary directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert_eq!(
        fixture.0.read(&KEY, Interaction::Quiet),
        Err(CredentialError::Unavailable)
    );
    assert_eq!(
        fixture.0.remove(&KEY, Interaction::Quiet),
        Err(CredentialError::Unavailable)
    );
    assert!(path.exists());
}
