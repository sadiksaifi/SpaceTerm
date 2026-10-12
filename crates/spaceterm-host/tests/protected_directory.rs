#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::{CString, OsStr};
use std::fs::{self, DirBuilder, File, FileTimes, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use spaceterm_host::protected_directory::{
    NativeDirectoryProtection, ProtectedDirectoryError, RetainedProtectedDirectory,
};

const RECORD_NAME: &str = "registration";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
type EntryIdentity = (u64, u64);

// Only these account-owned fixtures are inspected. This is not root or ACL evidence.
struct FixtureProtection {
    controlled_directories: HashSet<EntryIdentity>,
    owner_uid: u32,
    inspected_directories: RefCell<HashSet<EntryIdentity>>,
    inspected_files: Cell<usize>,
}

impl NativeDirectoryProtection for FixtureProtection {
    fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
        let metadata = directory
            .metadata()
            .map_err(|_| ProtectedDirectoryError::Unavailable)?;
        let identity = (metadata.dev(), metadata.ino());
        if self.controlled_directories.contains(&identity)
            && (metadata.uid() != self.owner_uid || metadata.mode() & 0o777 != 0o700)
        {
            return Err(ProtectedDirectoryError::Unsafe);
        }
        self.inspected_directories.borrow_mut().insert(identity);
        Ok(())
    }

    fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
        let metadata = file
            .metadata()
            .map_err(|_| ProtectedDirectoryError::Unavailable)?;
        if metadata.uid() != self.owner_uid || metadata.mode() & 0o777 != 0o600 {
            return Err(ProtectedDirectoryError::Unsafe);
        }
        self.inspected_files.set(self.inspected_files.get() + 1);
        Ok(())
    }
}

type FileCheck<'fixture> = dyn FnOnce(&File) -> Result<(), ProtectedDirectoryError> + 'fixture;

// Changes occur at the injected native inspection boundary, not inside owned I/O.
struct FileCheckProtection<'fixture> {
    base: &'fixture FixtureProtection,
    trigger: usize,
    checks: Cell<usize>,
    action: RefCell<Option<Box<FileCheck<'fixture>>>>,
}

impl<'fixture> FileCheckProtection<'fixture> {
    fn new(
        base: &'fixture FixtureProtection,
        trigger: usize,
        action: impl FnOnce(&File) -> Result<(), ProtectedDirectoryError> + 'fixture,
    ) -> Self {
        Self {
            base,
            trigger,
            checks: Cell::new(0),
            action: RefCell::new(Some(Box::new(action))),
        }
    }
}

impl NativeDirectoryProtection for FileCheckProtection<'_> {
    fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
        self.base.verify_directory(directory)
    }

    fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
        self.base.verify_regular_file(file)?;
        let checks = self.checks.get() + 1;
        self.checks.set(checks);
        if checks == self.trigger
            && let Some(action) = self.action.borrow_mut().take()
        {
            action(file)?;
        }
        Ok(())
    }
}

struct Fixture {
    root: PathBuf,
    ancestor: PathBuf,
    directory: PathBuf,
    protection: FixtureProtection,
}

impl Fixture {
    fn new() -> Self {
        // Native traversal must not normalize paths. Only fixture setup resolves the
        // OS temporary-directory alias, such as /var on macOS.
        let parent = fixture_io(std::env::temp_dir().canonicalize());
        let mut root = None;
        for _ in 0..128 {
            let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!("spaceterm-host-{}-{nonce}", std::process::id()));
            match DirBuilder::new().mode(0o700).create(&candidate) {
                Ok(()) => {
                    root = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => panic!("controlled fixture directory creation failed"),
            }
        }
        let root = root.expect("controlled fixture directory allocation failed");
        let ancestor = root.join("ancestor");
        let directory = ancestor.join("registry");
        fixture_io(DirBuilder::new().mode(0o700).create(&ancestor));
        fixture_io(DirBuilder::new().mode(0o700).create(&directory));
        let metadata = fixture_io(fs::metadata(&root));
        let controlled_directories = [&root, &ancestor, &directory]
            .into_iter()
            .map(|path| entry_identity(path))
            .collect();
        Self {
            root,
            ancestor,
            directory,
            protection: FixtureProtection {
                controlled_directories,
                owner_uid: metadata.uid(),
                inspected_directories: RefCell::new(HashSet::new()),
                inspected_files: Cell::new(0),
            },
        }
    }

    fn write_record(&self, bytes: &[u8]) {
        write_private_file(&self.directory.join(RECORD_NAME), bytes);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture_io<T>(result: io::Result<T>) -> T {
    result
        .map_err(|_| ())
        .expect("controlled fixture I/O failed")
}

fn entry_identity(path: &Path) -> EntryIdentity {
    let metadata = fixture_io(fs::metadata(path));
    (metadata.dev(), metadata.ino())
}

fn write_private_file(path: &Path, bytes: &[u8]) {
    let mut file = fixture_io(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path),
    );
    fixture_io(file.write_all(bytes));
}

#[test]
fn read_accepts_the_exact_byte_bound_and_inspects_every_opened_ancestor_handle() {
    let fixture = Fixture::new();
    fixture.write_record(b"bounded record");
    let expected_ancestors: HashSet<_> =
        fixture.directory.ancestors().map(entry_identity).collect();

    let opened = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection);
    assert!(
        opened.is_ok(),
        "protected directory opening failed: {opened:?}"
    );
    let directory = opened.unwrap();
    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 14);
    assert!(read.is_ok(), "bounded protected reading failed: {read:?}");
    let contents = read.unwrap();

    assert_eq!(contents.as_bytes(), b"bounded record");
    assert!(
        expected_ancestors.is_subset(&fixture.protection.inspected_directories.borrow()),
        "the native module skipped an opened ancestor's protection inspector"
    );
    assert!(fixture.protection.inspected_files.get() > 0);
}

#[test]
fn unrelated_child_creation_between_successful_reads_does_not_invalidate_the_directory() {
    let fixture = Fixture::new();
    fixture.write_record(b"bounded record");
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection)
        .expect("controlled protected directory opening failed");
    let first = directory
        .read_regular_file(OsStr::new(RECORD_NAME), 14)
        .expect("first protected reading failed");
    assert_eq!(first.as_bytes(), b"bounded record");

    write_private_file(&fixture.directory.join("unrelated-child"), b"other");

    let second = directory
        .read_regular_file(OsStr::new(RECORD_NAME), 14)
        .expect("unrelated child creation invalidated protected reading");
    assert_eq!(second.as_bytes(), b"bounded record");
}

#[test]
fn unrelated_child_changes_during_either_file_inspection_remain_admissible() {
    for trigger in [1, 2] {
        let fixture = Fixture::new();
        fixture.write_record(b"bounded record");
        let protection = FileCheckProtection::new(&fixture.protection, trigger, |_| {
            for parent in [&fixture.root, &fixture.ancestor, &fixture.directory] {
                let sibling = parent.join("unrelated-directory");
                fixture_io(DirBuilder::new().mode(0o700).create(&sibling));
                let entry = parent.join("unrelated-file");
                write_private_file(&entry, b"other");
                fixture_io(fs::set_permissions(
                    &entry,
                    fs::Permissions::from_mode(0o644),
                ));
                let mut writer = fixture_io(OpenOptions::new().write(true).open(&entry));
                fixture_io(writer.write_all(b"edited"));
                let moved = parent.join("moved-unrelated-file");
                fixture_io(fs::rename(&entry, &moved));
                fixture_io(fs::remove_file(&moved));
                fixture_io(fs::remove_dir(&sibling));
            }
            Ok(())
        });
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");

        let read = directory
            .read_regular_file(OsStr::new(RECORD_NAME), 14)
            .expect("unrelated child changes invalidated protected reading");

        assert_eq!(read.as_bytes(), b"bounded record");
        assert!(
            protection.action.borrow().is_none(),
            "the child changes did not run"
        );
    }
}

#[test]
fn an_ancestor_replacement_invalidates_verification_and_reading_of_an_unchanged_leaf() {
    let fixture = Fixture::new();
    fixture.write_record(b"original");
    let opened = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection);
    assert!(
        opened.is_ok(),
        "protected directory opening failed: {opened:?}"
    );
    let directory = opened.unwrap();

    // The old registry and its immediate parent remain intact through their handles.
    fixture_io(fs::rename(
        &fixture.ancestor,
        fixture.root.join("displaced"),
    ));
    fixture_io(DirBuilder::new().mode(0o700).create(&fixture.ancestor));
    fixture_io(DirBuilder::new().mode(0o700).create(&fixture.directory));
    write_private_file(&fixture.directory.join(RECORD_NAME), b"replacement");

    assert_eq!(directory.verify(), Err(ProtectedDirectoryError::Changed));
    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);
    assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
}

#[test]
fn opening_rejects_an_ancestor_protection_change_restored_by_the_leaf_inspector() {
    struct RestoringProtection<'fixture> {
        fixture: &'fixture Fixture,
        leaf_identity: EntryIdentity,
        armed: Cell<bool>,
    }

    impl NativeDirectoryProtection for RestoringProtection<'_> {
        fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
            self.fixture.protection.verify_directory(directory)?;
            let metadata = directory
                .metadata()
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            if (metadata.dev(), metadata.ino()) == self.leaf_identity && self.armed.replace(false) {
                // Complete both mutations before returning. Rejection must not
                // depend on a timestamp delay. This is not production ACL evidence.
                fixture_io(fs::set_permissions(
                    &self.fixture.ancestor,
                    fs::Permissions::from_mode(0o777),
                ));
                fixture_io(fs::set_permissions(
                    &self.fixture.ancestor,
                    fs::Permissions::from_mode(0o700),
                ));
            }
            Ok(())
        }

        fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
            self.fixture.protection.verify_regular_file(file)
        }
    }

    let fixture = Fixture::new();
    let protection = RestoringProtection {
        fixture: &fixture,
        leaf_identity: entry_identity(&fixture.directory),
        armed: Cell::new(true),
    };

    let opened = RetainedProtectedDirectory::open(&fixture.directory, &protection);

    assert_eq!(opened.err(), Some(ProtectedDirectoryError::Changed));
    assert!(!protection.armed.get(), "the leaf inspector did not run");
}

#[test]
fn opening_rejects_an_ancestor_symlink_instead_of_canonicalizing_it() {
    let fixture = Fixture::new();
    let alias = fixture.root.join("alias");
    fixture_io(symlink(&fixture.ancestor, &alias));

    let opened = RetainedProtectedDirectory::open(&alias.join("registry"), &fixture.protection);

    assert_eq!(opened.err(), Some(ProtectedDirectoryError::Unsafe));
}

#[test]
fn opening_rejects_relative_paths_nul_and_traversal_components() {
    let fixture = Fixture::new();
    let invalid_paths = [
        PathBuf::from("registry"),
        fixture.directory.join("../registry"),
        fixture.ancestor.join("./registry"),
        fixture.directory.join("."),
        PathBuf::from("/./"),
        PathBuf::from("/../"),
        PathBuf::from(OsStr::from_bytes(b"/registry\0suffix")),
    ];

    for path in invalid_paths {
        let opened = RetainedProtectedDirectory::open(&path, &fixture.protection);
        assert_eq!(opened.err(), Some(ProtectedDirectoryError::InvalidPath));
    }
}

#[test]
fn opening_bounds_retained_ancestors_before_inspection_or_native_retention() {
    let fixture = Fixture::new();
    // The documented limit includes root. No entries need to exist for a path
    // exceeding the operation's descriptor/watch budget to be rejected.
    let path = PathBuf::from(format!("/{}", "component/".repeat(255)));

    let opened = RetainedProtectedDirectory::open(&path, &fixture.protection);

    assert_eq!(opened.err(), Some(ProtectedDirectoryError::Unavailable));
    assert!(fixture.protection.inspected_directories.borrow().is_empty());
}

#[test]
fn reading_rejects_a_symlink_to_an_otherwise_private_regular_file() {
    let fixture = Fixture::new();
    let target = fixture.root.join("target");
    write_private_file(&target, b"must not follow");
    fixture_io(symlink(&target, fixture.directory.join(RECORD_NAME)));
    let opened = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection);
    assert!(
        opened.is_ok(),
        "protected directory opening failed: {opened:?}"
    );
    let directory = opened.unwrap();

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Unsafe));
}

#[test]
fn reading_rejects_non_regular_entries() {
    let fixture = Fixture::new();
    fixture_io(
        DirBuilder::new()
            .mode(0o700)
            .create(fixture.directory.join(RECORD_NAME)),
    );
    let opened = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection);
    assert!(
        opened.is_ok(),
        "protected directory opening failed: {opened:?}"
    );
    let directory = opened.unwrap();

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Unsafe));
}

#[test]
fn reading_rejects_oversized_data_instead_of_returning_a_truncated_record() {
    let fixture = Fixture::new();
    fixture.write_record(&vec![b'x'; 64 * 1024]);
    let opened = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection);
    assert!(
        opened.is_ok(),
        "protected directory opening failed: {opened:?}"
    );
    let directory = opened.unwrap();

    for maximum_bytes in [0, 32] {
        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), maximum_bytes);
        assert_eq!(read.err(), Some(ProtectedDirectoryError::TooLarge));
    }
}

#[test]
fn reading_rechecks_protection_on_retained_ancestor_handles() {
    let fixture = Fixture::new();
    fixture.write_record(b"record");
    let opened = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection);
    assert!(
        opened.is_ok(),
        "protected directory opening failed: {opened:?}"
    );
    let directory = opened.unwrap();
    fixture_io(fs::set_permissions(
        &fixture.ancestor,
        fs::Permissions::from_mode(0o777),
    ));

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Unsafe));
}

#[test]
fn reading_rejects_entry_names_that_could_escape_the_retained_directory() {
    let fixture = Fixture::new();
    fixture.write_record(b"record");
    let opened = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection);
    assert!(
        opened.is_ok(),
        "protected directory opening failed: {opened:?}"
    );
    let directory = opened.unwrap();
    let invalid_names = [
        OsStr::new(""),
        OsStr::new("."),
        OsStr::new(".."),
        OsStr::new("../registration"),
        OsStr::new("registry/registration"),
        OsStr::new("/registration"),
        OsStr::new("registration/"),
        OsStr::from_bytes(b"registration\0suffix"),
    ];

    for name in invalid_names {
        let read = directory.read_regular_file(name, 32);
        assert_eq!(read.err(), Some(ProtectedDirectoryError::InvalidPath));
    }
}

#[test]
fn reading_rejects_hard_linked_files_even_when_the_native_inspector_accepts_their_mode() {
    let fixture = Fixture::new();
    fixture.write_record(b"record");
    fixture_io(fs::hard_link(
        fixture.directory.join(RECORD_NAME),
        fixture.root.join("other-link"),
    ));
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Unsafe));
}

#[test]
fn reading_rejects_a_fifo_without_waiting_for_a_writer() {
    let fixture = Fixture::new();
    let path = fixture.directory.join(RECORD_NAME);
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| ())
        .expect("controlled fixture name is invalid");
    // SAFETY: the controlled fixture name is NUL-terminated. This creates only a
    // test filesystem entry; no launcher pipe or writer is opened.
    let created = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
    assert_eq!(created, 0, "controlled fixture FIFO creation failed");
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Unsafe));
}

#[test]
fn an_empty_file_is_admitted_with_a_zero_byte_bound() {
    let fixture = Fixture::new();
    fixture.write_record(b"");
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 0);

    assert_eq!(
        read.expect("zero-bound protected reading failed")
            .as_bytes(),
        b""
    );
}

#[test]
fn an_overflowing_read_bound_fails_before_file_inspection_or_allocation() {
    let fixture = Fixture::new();
    fixture.write_record(b"record");
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture.protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), usize::MAX);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::TooLarge));
    assert_eq!(fixture.protection.inspected_files.get(), 0);
}

#[test]
fn reading_rechecks_file_protection_after_consuming_contents() {
    let fixture = Fixture::new();
    fixture.write_record(b"record");
    let protection = FileCheckProtection::new(&fixture.protection, 2, |_| {
        Err(ProtectedDirectoryError::Unsafe)
    });
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Unsafe));
}

#[test]
fn a_hard_link_added_after_reading_prevents_contents_from_being_admitted() {
    let fixture = Fixture::new();
    fixture.write_record(b"record");
    let protection = FileCheckProtection::new(&fixture.protection, 2, |_| {
        fixture_io(fs::hard_link(
            fixture.directory.join(RECORD_NAME),
            fixture.root.join("other-link"),
        ));
        Ok(())
    });
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Unsafe));
}

#[test]
fn replacement_of_the_read_file_prevents_contents_from_being_admitted() {
    let fixture = Fixture::new();
    fixture.write_record(b"record");
    let protection = FileCheckProtection::new(&fixture.protection, 2, |_| {
        fixture_io(fs::rename(
            fixture.directory.join(RECORD_NAME),
            fixture.root.join("displaced-record"),
        ));
        fixture.write_record(b"record");
        Ok(())
    });
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
}

#[test]
fn a_same_length_in_place_edit_after_reading_prevents_contents_from_being_admitted() {
    let fixture = Fixture::new();
    fixture.write_record(b"record");
    let protection = FileCheckProtection::new(&fixture.protection, 2, |_| {
        let path = fixture.directory.join(RECORD_NAME);
        let mut writer = fixture_io(OpenOptions::new().write(true).open(&path));
        let before = fixture_io(writer.metadata());
        let original_modified = fixture_io(before.modified());
        fixture_io(writer.write_all(b"edited"));
        fixture_io(writer.set_times(FileTimes::new().set_modified(original_modified)));
        let after = fixture_io(writer.metadata());
        assert_eq!(
            after.len(),
            before.len(),
            "the edit changed the fixture length"
        );
        assert_eq!(
            fixture_io(after.modified()),
            original_modified,
            "the fixture did not restore the original modification time"
        );
        // Actual ctime equality is not asserted here. The private unit-test seam
        // separately exercises equal compared timestamps without claiming native evidence.
        Ok(())
    });
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
}

#[test]
fn reading_rejects_ancestor_permissions_changed_by_the_final_file_inspector() {
    let fixture = Fixture::new();
    fixture.write_record(b"bounded record");
    let protection = FileCheckProtection::new(&fixture.protection, 2, |_| {
        fixture_io(fs::set_permissions(
            &fixture.ancestor,
            fs::Permissions::from_mode(0o777),
        ));
        Ok(())
    });
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 14);

    assert!(
        matches!(
            &read,
            Err(ProtectedDirectoryError::Unsafe | ProtectedDirectoryError::Changed)
        ),
        "protected reading accepted changed ancestor protection: {read:?}"
    );
}

#[test]
fn reading_rejects_an_ancestor_protection_change_restored_by_the_final_file_inspector() {
    let fixture = Fixture::new();
    fixture.write_record(b"bounded record");
    let protection = FileCheckProtection::new(&fixture.protection, 2, |_| {
        // Both mutations complete before the final inspector returns. No sleep
        // establishes timestamp uniqueness or native equal-ctime evidence.
        fixture_io(fs::set_permissions(
            &fixture.ancestor,
            fs::Permissions::from_mode(0o777),
        ));
        fixture_io(fs::set_permissions(
            &fixture.ancestor,
            fs::Permissions::from_mode(0o700),
        ));
        Ok(())
    });
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 14);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
    assert_eq!(protection.checks.get(), 2);
}

#[test]
fn ancestor_replacement_during_file_inspection_prevents_contents_from_being_admitted() {
    let fixture = Fixture::new();
    fixture.write_record(b"record");
    let protection = FileCheckProtection::new(&fixture.protection, 2, |_| {
        fixture_io(fs::rename(
            &fixture.ancestor,
            fixture.root.join("displaced-ancestor"),
        ));
        fixture_io(DirBuilder::new().mode(0o700).create(&fixture.ancestor));
        Ok(())
    });
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
        .expect("controlled protected directory opening failed");

    let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 32);

    assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
}

#[test]
fn verification_rechecks_ancestor_links_after_inspecting_the_leaf_directory() {
    struct ReplacingProtection<'fixture> {
        fixture: &'fixture Fixture,
        leaf_identity: EntryIdentity,
        armed: Cell<bool>,
    }

    impl NativeDirectoryProtection for ReplacingProtection<'_> {
        fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
            self.fixture.protection.verify_directory(directory)?;
            let metadata = directory
                .metadata()
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            if self.armed.get() && (metadata.dev(), metadata.ino()) == self.leaf_identity {
                self.armed.set(false);
                fixture_io(fs::rename(
                    &self.fixture.ancestor,
                    self.fixture.root.join("displaced-ancestor"),
                ));
                fixture_io(DirBuilder::new().mode(0o700).create(&self.fixture.ancestor));
            }
            Ok(())
        }

        fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
            self.fixture.protection.verify_regular_file(file)
        }
    }

    let fixture = Fixture::new();
    let protection = ReplacingProtection {
        fixture: &fixture,
        leaf_identity: entry_identity(&fixture.directory),
        armed: Cell::new(false),
    };
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
        .expect("controlled protected directory opening failed");
    protection.armed.set(true);

    let verification = directory.verify();

    assert_eq!(verification, Err(ProtectedDirectoryError::Changed));
}

#[test]
fn verification_rechecks_ancestor_protection_after_inspecting_the_leaf_directory() {
    struct WeakeningProtection<'fixture> {
        fixture: &'fixture Fixture,
        leaf_identity: EntryIdentity,
        armed: Cell<bool>,
    }

    impl NativeDirectoryProtection for WeakeningProtection<'_> {
        fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
            self.fixture.protection.verify_directory(directory)?;
            let metadata = directory
                .metadata()
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            if self.armed.get() && (metadata.dev(), metadata.ino()) == self.leaf_identity {
                self.armed.set(false);
                fixture_io(fs::set_permissions(
                    &self.fixture.ancestor,
                    fs::Permissions::from_mode(0o777),
                ));
            }
            Ok(())
        }

        fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
            self.fixture.protection.verify_regular_file(file)
        }
    }

    let fixture = Fixture::new();
    let protection = WeakeningProtection {
        fixture: &fixture,
        leaf_identity: entry_identity(&fixture.directory),
        armed: Cell::new(false),
    };
    let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
        .expect("controlled protected directory opening failed");
    protection.armed.set(true);

    let verification = directory.verify();

    assert!(
        matches!(
            verification,
            Err(ProtectedDirectoryError::Unsafe | ProtectedDirectoryError::Changed)
        ),
        "protected verification accepted changed ancestor protection: {verification:?}"
    );
}
