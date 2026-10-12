use std::fs::{self, DirBuilder, File, FileTimes, OpenOptions};
use std::io::{self, Seek, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use super::ProtectionInspection;
use crate::protected_directory::ProtectedDirectoryError;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    directory: File,
}

impl Fixture {
    fn new() -> Self {
        // Account-owned fixtures exercise the real native inspection interval,
        // not root admission. No metadata or native query is replaced.
        let parent = fixture_io(std::env::temp_dir().canonicalize());
        for _ in 0..128 {
            let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = parent.join(format!(
                "spaceterm-root-inspection-{}-{nonce}",
                std::process::id()
            ));
            match DirBuilder::new().mode(0o700).create(&root) {
                Ok(()) => {
                    let directory = fixture_io(File::open(&root));
                    return Self { root, directory };
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => panic!("controlled inspection fixture creation failed"),
            }
        }
        panic!("controlled inspection fixture allocation failed")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture_io<T>(result: io::Result<T>) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("controlled inspection fixture I/O failed"),
    }
}

#[test]
fn native_directory_inspection_admits_unrelated_child_creation_and_removal() {
    let fixture = Fixture::new();
    let before = fixture_io(fixture.directory.metadata());
    let inspection = ProtectionInspection::begin(&fixture.directory)
        .expect("controlled native inspection beginning failed");
    // Child mutations need not advance HFS's one-second change time.
    assert_eq!(inspection.before.changed_at, None);
    assert_eq!(inspection.inspect_acl(), Ok(()));

    let sibling_file = fixture.root.join("other-registration");
    drop(fixture_io(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&sibling_file),
    ));
    let sibling_directory = fixture.root.join("other-account");
    fixture_io(DirBuilder::new().mode(0o700).create(&sibling_directory));
    fixture_io(fs::remove_file(sibling_file));
    fixture_io(fs::remove_dir(sibling_directory));

    let after = fixture_io(fixture.directory.metadata());
    assert_eq!(
        (after.uid(), after.gid(), after.mode(), after.nlink()),
        (before.uid(), before.gid(), before.mode(), before.nlink())
    );
    assert_eq!(inspection.finish(), Ok(()));
}

#[test]
fn native_directory_inspection_rejects_changed_and_restored_modes() {
    for restore in [false, true] {
        let fixture = Fixture::new();
        let original = fixture_io(fixture.directory.metadata()).permissions();
        let inspection = ProtectionInspection::begin(&fixture.directory)
            .expect("controlled native inspection beginning failed");
        assert_eq!(inspection.inspect_acl(), Ok(()));

        fixture_io(
            fixture
                .directory
                .set_permissions(fs::Permissions::from_mode(0o720)),
        );
        if restore {
            fixture_io(fixture.directory.set_permissions(original));
        }

        assert_eq!(inspection.finish(), Err(ProtectedDirectoryError::Changed));
    }
}

#[test]
fn native_directory_inspection_rejects_restored_mutation_acls() {
    let fixture = Fixture::new();
    let inspection = ProtectionInspection::begin(&fixture.directory)
        .expect("controlled native inspection beginning failed");
    assert_eq!(inspection.inspect_acl(), Ok(()));

    let added = fixture_io(
        Command::new("/bin/chmod")
            .args(["+a", "everyone allow write"])
            .arg(&fixture.root)
            .output(),
    );
    assert!(
        added.status.success(),
        "controlled fixture ACL addition failed"
    );
    assert_eq!(
        inspection.inspect_acl(),
        Err(ProtectedDirectoryError::Unsafe)
    );
    let removed = fixture_io(
        Command::new("/bin/chmod")
            .arg("-N")
            .arg(&fixture.root)
            .output(),
    );
    assert!(
        removed.status.success(),
        "controlled fixture ACL removal failed"
    );
    assert_eq!(inspection.inspect_acl(), Ok(()));

    assert_eq!(inspection.finish(), Err(ProtectedDirectoryError::Changed));
}

#[test]
fn native_regular_file_inspection_retains_change_time_and_rejects_same_length_writes() {
    let fixture = Fixture::new();
    let mut file = fixture_io(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(fixture.root.join("registration")),
    );
    fixture_io(file.write_all(b"record"));
    let before = fixture_io(file.metadata());
    let original_modified = fixture_io(before.modified());
    let inspection =
        ProtectionInspection::begin(&file).expect("controlled native inspection beginning failed");
    assert_eq!(
        inspection.before.changed_at,
        Some((before.ctime(), before.ctime_nsec()))
    );
    assert_eq!(inspection.inspect_acl(), Ok(()));

    let mut writer = &file;
    fixture_io(writer.rewind());
    fixture_io(writer.write_all(b"edited"));
    fixture_io(file.set_times(FileTimes::new().set_modified(original_modified)));
    let after = fixture_io(file.metadata());
    assert_eq!(after.len(), before.len());
    assert_eq!(fixture_io(after.modified()), original_modified);

    assert_eq!(inspection.finish(), Err(ProtectedDirectoryError::Changed));
}
