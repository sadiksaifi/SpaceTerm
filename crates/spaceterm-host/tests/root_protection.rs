use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use spaceterm_host::protected_directory::{NativeDirectoryProtection, ProtectedDirectoryError};
use spaceterm_host::root_protection::NativeRootProtection;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    file: File,
}

impl Fixture {
    fn new() -> Self {
        // Temporary-directory alias resolution is fixture setup, not authority.
        let parent = fixture_io(std::env::temp_dir().canonicalize());
        let mut directory = None;
        for _ in 0..128 {
            let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                "spaceterm-root-protection-{}-{nonce}",
                std::process::id()
            ));
            let builder = DirBuilder::new();
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            match builder.create(&candidate) {
                Ok(()) => {
                    directory = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => panic!("controlled fixture directory creation failed"),
            }
        }
        let directory = directory.expect("controlled fixture directory allocation failed");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = fixture_io(options.open(directory.join("fixture")));
        Self { directory, file }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn fixture_io<T>(result: io::Result<T>) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("controlled fixture I/O failed"),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod root_ownership {
    use std::fs::File;
    use std::os::unix::fs::MetadataExt;

    use super::fixture_io;
    use super::{
        Fixture, NativeDirectoryProtection, NativeRootProtection, ProtectedDirectoryError,
    };

    #[test]
    fn nonroot_owned_private_directory_is_rejected() {
        let fixture = Fixture::new();
        let directory = fixture_io(File::open(&fixture.directory));
        let owner = fixture_io(directory.metadata()).uid();
        if owner == 0 {
            // No chown or privileged fixture creation. Pure policy tests cover UID rejection.
            return;
        }
        assert_eq!(
            NativeRootProtection.verify_directory(&directory),
            Err(ProtectedDirectoryError::Unsafe)
        );
    }

    #[test]
    fn nonroot_owned_private_regular_file_is_rejected() {
        let fixture = Fixture::new();
        let owner = fixture_io(fixture.file.metadata()).uid();
        if owner == 0 {
            return;
        }
        // Account-owned fixtures are negative ownership cases, never ACL authority proof.
        assert_eq!(
            NativeRootProtection.verify_regular_file(&fixture.file),
            Err(ProtectedDirectoryError::Unsafe)
        );
    }
}

#[cfg(target_os = "macos")]
mod macos_system_smoke {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use spaceterm_host::protected_directory::RetainedProtectedDirectory;

    use super::{NativeDirectoryProtection, NativeRootProtection};

    fn system_handle(path: &str) -> File {
        let file =
            File::open(path).unwrap_or_else(|_| panic!("safe system fixture opening failed"));
        let metadata = file
            .metadata()
            .unwrap_or_else(|_| panic!("safe system fixture metadata is unavailable"));
        assert_eq!(
            (metadata.uid(), metadata.mode() & 0o022),
            (0, 0),
            "system fixture does not have the required root mode"
        );
        file
    }

    #[test]
    fn root_owned_system_directory_passes_native_inspection() {
        let directory = system_handle("/usr/bin");
        assert_eq!(NativeRootProtection.verify_directory(&directory), Ok(()));
    }

    #[test]
    fn root_owned_system_regular_file_passes_native_inspection() {
        let file = system_handle("/usr/bin/true");
        assert_eq!(NativeRootProtection.verify_regular_file(&file), Ok(()));
    }

    #[test]
    fn retained_system_directory_can_read_a_bounded_root_owned_file() {
        // Read-only descriptor smoke. This is not a privileged ACL fixture or launch approval.
        let protection = NativeRootProtection;
        let directory = RetainedProtectedDirectory::open(Path::new("/private/etc"), &protection)
            .expect("safe system directory admission failed");
        let contents = directory
            .read_regular_file(OsStr::new("hosts"), 4096)
            .expect("bounded system fixture read failed");
        assert!(
            !contents.as_bytes().is_empty(),
            "safe system fixture is empty"
        );
    }
}

#[cfg(target_os = "linux")]
mod linux_missing_authority {
    use std::fs::File;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use spaceterm_host::protected_directory::RetainedProtectedDirectory;

    use super::{NativeDirectoryProtection, NativeRootProtection, ProtectedDirectoryError};

    fn system_handle(path: &str) -> File {
        let file =
            File::open(path).unwrap_or_else(|_| panic!("read-only system fixture opening failed"));
        let metadata = file
            .metadata()
            .unwrap_or_else(|_| panic!("read-only system fixture metadata is unavailable"));
        assert_eq!(
            (metadata.uid(), metadata.mode() & 0o022),
            (0, 0),
            "system fixture does not have the required root mode"
        );
        file
    }

    #[test]
    fn projected_root_mode_cannot_admit_a_directory_without_a_linux_authority() {
        // No issuer is installed. A normal-looking system path does not supply
        // descriptor-bound mapping or exclusion of account-writable aliases.
        let directory = system_handle("/");
        assert_eq!(
            NativeRootProtection.verify_directory(&directory),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn projected_root_mode_cannot_admit_a_regular_file_without_a_linux_authority() {
        let file = system_handle("/usr/bin/true");
        assert_eq!(
            NativeRootProtection.verify_regular_file(&file),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn retained_traversal_cannot_bootstrap_a_linux_authority_from_root_mode() {
        // Read-only denial. No marker, namespace setup or mount action is performed.
        let protection = NativeRootProtection;
        let opened = RetainedProtectedDirectory::open(Path::new("/"), &protection);
        assert_eq!(
            opened.err(),
            Some(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn a_regular_descriptor_is_unsafe_for_directory_inspection_before_authority_admission() {
        let file = system_handle("/usr/bin/true");
        assert_eq!(
            NativeRootProtection.verify_directory(&file),
            Err(ProtectedDirectoryError::Unsafe)
        );
    }

    #[test]
    fn a_directory_descriptor_is_unsafe_for_regular_file_inspection_before_authority_admission() {
        let directory = system_handle("/");
        assert_eq!(
            NativeRootProtection.verify_regular_file(&directory),
            Err(ProtectedDirectoryError::Unsafe)
        );
    }

    #[test]
    fn the_fixed_linux_reader_cannot_issue_authority_from_a_registration_or_root_mode() {
        use spaceterm_host::native_registration::NativeRegistrationReader;
        use spaceterm_host::startup::{AccountBuildKey, BuildIdentity, StartupError};

        let reader = NativeRegistrationReader::new();
        let key = AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        };
        assert_eq!(
            reader.read_fixed_registration(key),
            Err(StartupError::NativeUnavailable)
        );
    }

    #[test]
    fn fixed_reader_uid_validation_precedes_the_missing_linux_issuer() {
        use spaceterm_host::native_registration::NativeRegistrationReader;
        use spaceterm_host::startup::{AccountBuildKey, BuildIdentity, StartupError};

        let reader = NativeRegistrationReader::new();
        for (account_uid, error) in [
            (0, StartupError::RootAccount),
            (u32::MAX, StartupError::UnsafeRegistration),
        ] {
            let key = AccountBuildKey {
                account_uid,
                build_identity: BuildIdentity::Development,
            };
            assert_eq!(reader.read_fixed_registration(key), Err(error));
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[test]
fn unsupported_hosts_fail_closed_for_both_handle_operations() {
    let fixture = Fixture::new();
    // Unsupported inspection rejects before entry-type mechanics are relevant.
    assert_eq!(
        NativeRootProtection.verify_directory(&fixture.file),
        Err(ProtectedDirectoryError::NativeUnavailable)
    );
    assert_eq!(
        NativeRootProtection.verify_regular_file(&fixture.file),
        Err(ProtectedDirectoryError::NativeUnavailable)
    );
}
