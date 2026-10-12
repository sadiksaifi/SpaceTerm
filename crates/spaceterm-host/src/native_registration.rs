//! Fixed native account/build registration reading and strict candidate record decoding.
//!
//! Reads bounded candidate facts without installing records, preparing installations,
//! loading credentials, authenticating code or executing processes.
//!
//! # Unshipped native record format
//!
//! Schema 1 is exactly 48 bytes. Integers are unsigned little-endian. This is a private
//! native startup record, not the Settings Document or a SpaceTerm Protocol message.
//! Rust struct layout and serialization are not its format.
//!
//! | Byte range | Field | Admitted values |
//! | --- | --- | --- |
//! | 0..8 | Magic | `STRGREC` followed by one NUL byte |
//! | 8..10 | Schema, u16 | 1 |
//! | 10 | Build, u8 | 1 Release, 2 Preflight, 3 Development |
//! | 11 | Desired state, u8 | 1 Running, 2 Stopped |
//! | 12..16 | Account UID, u32 | 1 through u32::MAX - 1, exactly the requested UID |
//! | 16..24 | Generation, u64 | Nonzero |
//! | 24..32 | Approved installation ID, u64 | Nonzero |
//! | 32..40 | Staged installation ID, u64 | Zero means absent; otherwise nonzero |
//! | 40..48 | Reserved | All zero |
//!
//! Unknown magic, schema, enum or reserved values, invalid stored identities, truncation
//! and trailing bytes are unsafe. A structurally valid other account/build is an identity
//! mismatch. No field contains a path, executable, native credential reference or secret.
//!
//! # Native reading contract
//!
//! macOS uses `/Library/Application Support/SpaceTerm/ServerRegistrations`. Linux uses
//! `/var/lib/spaceterm/server-registrations`. The only entry selected is
//! `<build>-<uid>.registration`, with `release`, `preflight` or `development` and the
//! canonical decimal UID. The host adapter fixes both the directory and naming rule.
//! Clients, CLI arguments, environment variables, XDG roots and installation paths cannot
//! select another privileged root. Missing storage is never created or repaired by reads.
//!
//! Admitted reads use [`crate::protected_directory::RetainedProtectedDirectory`],
//! retaining the no-follow ancestor chain through a bounded directory-relative read.
//! macOS uses [`crate::root_protection::NativeRootProtection`]. Linux requires the
//! private protection lease with both descriptor-bound non-idmapped evidence and
//! independently trusted publication coverage for every ancestor and file. No
//! production issuer is implemented, so the fixed Linux reader returns
//! [`StartupError::NativeUnavailable`] without inspecting native storage. It cannot
//! bootstrap authority from registration bytes, root mode or namespace ownership.
//! The byte bound is 48, with only the retained-directory module's one-byte oversize
//! probe. No live SQLite handle is reopened.
//!
//! Retained handles, sampled protection and covered events establish neither authentication,
//! historical write exclusion nor atomic contents. The decoder returns candidate facts,
//! not executable or credential authority. Authenticated exact copies, immutable approved
//! installations and resources, serialized global native mutation, final execution binding
//! and exact-startup credential handoff remain required above this module. Approved native
//! helpers themselves must be authenticated and protected before privileged execution.

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
use std::ffi::OsStr;
use std::fmt;
use std::num::NonZeroU64;
#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
use std::path::Path;

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
use crate::protected_directory::{
    NativeDirectoryProtection, ProtectedDirectoryError, RetainedProtectedDirectory,
};
#[cfg(target_os = "macos")]
use crate::root_protection::NativeRootProtection;
use crate::startup::{
    AccountBuildKey, BuildIdentity, DesiredServerState, InstallationId, ProtectedRegistration,
    StartupError, validate_requested_key,
};

const REGISTRATION_RECORD_BYTES: usize = 48;

#[cfg(target_os = "macos")]
const FIXED_REGISTRATION_DIRECTORY: &str =
    "/Library/Application Support/SpaceTerm/ServerRegistrations";

/// Decodes one exact bounded native startup record for the requested account/build.
///
/// Returned [`ProtectedRegistration`] fields are unauthenticated facts. The existing
/// type does not confer installation, execution, credential or native protection authority.
/// Only the trusted composition root may supply the native startup adapter. Generation
/// equality and the Stopped guard remain owned by [`crate::startup::admit_startup`].
///
/// # Errors
/// Rejects a root request before inspecting bytes with [`StartupError::RootAccount`].
/// The native UID sentinel u32::MAX, malformed records and any length other than 48
/// return [`StartupError::UnsafeRegistration`]. Structurally valid stored identities
/// differing from the requested account/build return [`StartupError::IdentityMismatch`].
/// Failures contain no payload or raw native errors.
pub fn decode_registration_record(
    bytes: &[u8],
    key: AccountBuildKey,
) -> Result<ProtectedRegistration, StartupError> {
    validate_requested_key(key)?;
    let bytes: &[u8; REGISTRATION_RECORD_BYTES] = bytes
        .try_into()
        .map_err(|_| StartupError::UnsafeRegistration)?;
    if &bytes[..8] != b"STRGREC\0"
        || u16::from_le_bytes([bytes[8], bytes[9]]) != 1
        || bytes[40..48].iter().any(|byte| *byte != 0)
    {
        return Err(StartupError::UnsafeRegistration);
    }
    let build_identity = match bytes[10] {
        1 => BuildIdentity::Release,
        2 => BuildIdentity::Preflight,
        3 => BuildIdentity::Development,
        _ => return Err(StartupError::UnsafeRegistration),
    };
    let desired_state = match bytes[11] {
        1 => DesiredServerState::Running,
        2 => DesiredServerState::Stopped,
        _ => return Err(StartupError::UnsafeRegistration),
    };
    let account_uid = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    if account_uid == 0 || account_uid == u32::MAX {
        return Err(StartupError::UnsafeRegistration);
    }
    let generation = NonZeroU64::new(u64::from_le_bytes(
        bytes[16..24]
            .try_into()
            .map_err(|_| StartupError::UnsafeRegistration)?,
    ))
    .ok_or(StartupError::UnsafeRegistration)?;
    let approved_installation = InstallationId(
        NonZeroU64::new(u64::from_le_bytes(
            bytes[24..32]
                .try_into()
                .map_err(|_| StartupError::UnsafeRegistration)?,
        ))
        .ok_or(StartupError::UnsafeRegistration)?,
    );
    let staged_installation = NonZeroU64::new(u64::from_le_bytes(
        bytes[32..40]
            .try_into()
            .map_err(|_| StartupError::UnsafeRegistration)?,
    ))
    .map(InstallationId);
    let stored_key = AccountBuildKey {
        account_uid,
        build_identity,
    };
    // IdentityMismatch is reserved for a fully well-formed record for another key.
    if stored_key != key {
        return Err(StartupError::IdentityMismatch);
    }
    Ok(ProtectedRegistration {
        key: stored_key,
        generation,
        desired_state,
        approved_installation,
        staged_installation,
    })
}

/// Read-only native registration adapter with fixed host locations and requested UID validation.
///
/// The production constructor accepts no root, protection inspector or factory. This
/// adapter does not implement installation retention and cannot issue an admission ticket.
/// Stopped admission and Running's approved-only selection are composed by the existing
/// [`crate::startup::NativeStartupRegistration`] interface, not by granting candidate bytes
/// authority. Reads never create or repair native storage.
#[derive(Default)]
pub struct NativeRegistrationReader {
    _private: (),
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    fixture: Option<FixtureLocation>,
}

impl NativeRegistrationReader {
    /// Creates the fixed host adapter without reading, creating or changing native storage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads only the fixed registration entry for this exact account/build.
    ///
    /// # Errors
    /// Rejects root and UID-sentinel requests before native inspection.
    /// Missing directories or entries return [`StartupError::RegistrationMissing`].
    /// Storage classified as unavailable returns [`StartupError::RegistrationUnavailable`].
    /// Unsafe types, links, protection, covered changes, oversize data or malformed records
    /// return [`StartupError::UnsafeRegistration`]. Structurally valid other identities
    /// return [`StartupError::IdentityMismatch`]. Unsupported native capabilities return
    /// [`StartupError::NativeUnavailable`]. Linux currently returns that failure
    /// without native storage access because no independent protection issuer exists.
    /// Requested UID validation still runs first. Failures never create records or identities.
    pub fn read_fixed_registration(
        &self,
        key: AccountBuildKey,
    ) -> Result<ProtectedRegistration, StartupError> {
        validate_requested_key(key)?;
        #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
        if let Some(fixture) = &self.fixture {
            return read_registration_at(&fixture.directory, fixture.protection.as_ref(), key);
        }
        #[cfg(target_os = "macos")]
        {
            let protection = NativeRootProtection;
            read_registration_at(Path::new(FIXED_REGISTRATION_DIRECTORY), &protection, key)
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Linux has no independently established protection issuer. Do not
            // consult a registration or inspect UID-zero storage to invent one.
            let _ = (self, key);
            Err(StartupError::NativeUnavailable)
        }
    }
}

impl fmt::Debug for NativeRegistrationReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NativeRegistrationReader(..)")
    }
}

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
fn read_registration_at(
    directory: &Path,
    protection: &dyn NativeDirectoryProtection,
    key: AccountBuildKey,
) -> Result<ProtectedRegistration, StartupError> {
    let build = match key.build_identity {
        BuildIdentity::Release => "release",
        BuildIdentity::Preflight => "preflight",
        BuildIdentity::Development => "development",
    };
    let name = format!("{build}-{}.registration", key.account_uid);
    let directory = RetainedProtectedDirectory::open(directory, protection)
        .map_err(classify_directory_error)?;
    let contents = directory
        .read_regular_file(OsStr::new(&name), REGISTRATION_RECORD_BYTES)
        .map_err(classify_directory_error)?;
    decode_registration_record(contents.as_bytes(), key)
}

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
fn classify_directory_error(error: ProtectedDirectoryError) -> StartupError {
    match error {
        ProtectedDirectoryError::Missing => StartupError::RegistrationMissing,
        ProtectedDirectoryError::Unavailable => StartupError::RegistrationUnavailable,
        ProtectedDirectoryError::NativeUnavailable => StartupError::NativeUnavailable,
        ProtectedDirectoryError::Unsafe
        | ProtectedDirectoryError::Changed
        | ProtectedDirectoryError::TooLarge
        | ProtectedDirectoryError::InvalidPath => StartupError::UnsafeRegistration,
    }
}

// The arbitrary directory and inspector exist only in the private unit-test build.
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
struct FixtureLocation {
    directory: std::path::PathBuf,
    protection: Box<dyn NativeDirectoryProtection>,
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::ffi::OsString;
    use std::fs::{self, DirBuilder, File, OpenOptions};
    use std::io::{self, Write};
    use std::num::NonZeroU64;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, symlink};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::MutexGuard;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::protected_directory::ProtectedDirectoryError;
    use crate::startup::{BuildIdentity, DesiredServerState, InstallationId};
    use crate::test_support::FIXTURE_NAMESPACE_LOCK;

    use super::*;

    const RECORD_NAME: &str = "development-501.registration";
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct InspectionControls {
        directory_error: Cell<Option<ProtectedDirectoryError>>,
        file_error: Cell<Option<ProtectedDirectoryError>>,
        inspections: Cell<usize>,
        on_file: RefCell<Option<Box<dyn FnOnce()>>>,
    }

    // This inspector admits account-owned fixtures and unrelated temporary ancestors.
    // It proves only controlled traversal/reading and classification, never root protection.
    struct FixtureProtection {
        directories: Vec<(u64, u64)>,
        owner_uid: u32,
        controls: Rc<InspectionControls>,
    }

    impl NativeDirectoryProtection for FixtureProtection {
        fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
            self.controls
                .inspections
                .set(self.controls.inspections.get() + 1);
            if let Some(error) = self.controls.directory_error.get() {
                return Err(error);
            }
            let metadata = directory
                .metadata()
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            if self.directories.contains(&(metadata.dev(), metadata.ino()))
                && (metadata.uid() != self.owner_uid || metadata.mode() & 0o777 != 0o700)
            {
                return Err(ProtectedDirectoryError::Unsafe);
            }
            Ok(())
        }

        fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
            self.controls
                .inspections
                .set(self.controls.inspections.get() + 1);
            if let Some(error) = self.controls.file_error.get() {
                return Err(error);
            }
            let metadata = file
                .metadata()
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            if metadata.uid() != self.owner_uid || metadata.mode() & 0o777 != 0o600 {
                return Err(ProtectedDirectoryError::Unsafe);
            }
            if let Some(action) = self.controls.on_file.borrow_mut().take() {
                action();
            }
            Ok(())
        }
    }

    impl NativeRegistrationReader {
        fn for_fixture(directory: PathBuf, protection: FixtureProtection) -> Self {
            Self {
                _private: (),
                fixture: Some(FixtureLocation {
                    directory,
                    protection: Box::new(protection),
                }),
            }
        }
    }

    struct Fixture {
        root: PathBuf,
        ancestor: PathBuf,
        directory: PathBuf,
        reader: NativeRegistrationReader,
        controls: Rc<InspectionControls>,
        _guard: MutexGuard<'static, ()>,
    }

    impl Fixture {
        fn new() -> Self {
            let guard = FIXTURE_NAMESPACE_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Only fixture setup resolves the temporary-directory alias, never production.
            let parent = fixture_io(std::env::temp_dir().canonicalize());
            let mut root = None;
            for _ in 0..128 {
                let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
                let candidate = parent.join(format!(
                    "spaceterm-native-registration-{}-{nonce}",
                    std::process::id()
                ));
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
            let directories = [&root, &ancestor, &directory]
                .into_iter()
                .map(|path| {
                    let metadata = fixture_io(fs::metadata(path));
                    (metadata.dev(), metadata.ino())
                })
                .collect();
            let controls = Rc::new(InspectionControls::default());
            let reader = NativeRegistrationReader::for_fixture(
                directory.clone(),
                FixtureProtection {
                    directories,
                    owner_uid: metadata.uid(),
                    controls: Rc::clone(&controls),
                },
            );
            Self {
                root,
                ancestor,
                directory,
                reader,
                controls,
                _guard: guard,
            }
        }

        fn write_entry(&self, name: &str, bytes: &[u8]) {
            let mut file = fixture_io(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(self.directory.join(name)),
            );
            fixture_io(file.write_all(bytes));
        }

        fn entry_names(&self) -> Vec<OsString> {
            let mut names: Vec<_> = fixture_io(fs::read_dir(&self.directory))
                .map(|entry| fixture_io(entry).file_name())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fixture_io<T>(result: io::Result<T>) -> T {
        result.unwrap_or_else(|_| panic!("controlled registration fixture I/O failed"))
    }

    fn key() -> AccountBuildKey {
        AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        }
    }

    fn record_bytes() -> [u8; 48] {
        let mut bytes = [0; 48];
        bytes[..8].copy_from_slice(b"STRGREC\0");
        bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
        bytes[10] = 3;
        bytes[11] = 1;
        bytes[12..16].copy_from_slice(&501_u32.to_le_bytes());
        bytes[16..24].copy_from_slice(&7_u64.to_le_bytes());
        bytes[24..32].copy_from_slice(&11_u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&99_u64.to_le_bytes());
        bytes
    }

    fn expected_record() -> ProtectedRegistration {
        ProtectedRegistration {
            key: key(),
            generation: NonZeroU64::new(7).expect("nonzero fixture generation"),
            desired_state: DesiredServerState::Running,
            approved_installation: InstallationId(NonZeroU64::new(11).expect("nonzero fixture ID")),
            staged_installation: Some(InstallationId(
                NonZeroU64::new(99).expect("nonzero fixture ID"),
            )),
        }
    }

    #[test]
    fn reader_selects_only_the_fixed_keyed_entry_with_real_bounded_fixture_io() {
        let fixture = Fixture::new();
        fixture.write_entry(RECORD_NAME, &record_bytes());
        fixture.write_entry("release-501.registration", &[]);
        fixture.write_entry("development-502.registration", &[]);
        fixture.write_entry("registration", &[]);
        let before = fixture.entry_names();

        assert_eq!(
            fixture.reader.read_fixed_registration(key()),
            Ok(expected_record())
        );
        assert!(fixture.controls.inspections.get() > 0);
        assert_eq!(fixture.entry_names(), before);
    }

    #[test]
    fn reader_selects_canonical_decimal_entries_for_each_build_and_uid_boundary() {
        let fixture = Fixture::new();
        for (build_code, build_identity, account_uid, name) in [
            (1, BuildIdentity::Release, 1, "release-1.registration"),
            (
                1,
                BuildIdentity::Release,
                u32::MAX - 1,
                "release-4294967294.registration",
            ),
            (2, BuildIdentity::Preflight, 1, "preflight-1.registration"),
            (
                2,
                BuildIdentity::Preflight,
                u32::MAX - 1,
                "preflight-4294967294.registration",
            ),
            (
                3,
                BuildIdentity::Development,
                1,
                "development-1.registration",
            ),
            (
                3,
                BuildIdentity::Development,
                u32::MAX - 1,
                "development-4294967294.registration",
            ),
        ] {
            let requested = AccountBuildKey {
                account_uid,
                build_identity,
            };
            let mut bytes = record_bytes();
            bytes[10] = build_code;
            bytes[12..16].copy_from_slice(&account_uid.to_le_bytes());
            fixture.write_entry(name, &bytes);
            let expected = ProtectedRegistration {
                key: requested,
                ..expected_record()
            };

            assert_eq!(
                fixture.reader.read_fixed_registration(requested),
                Ok(expected)
            );
        }
    }

    #[test]
    fn reader_preserves_missing_directory_and_missing_exact_entry_without_creation_or_scan() {
        for missing_directory in [false, true] {
            let fixture = Fixture::new();
            if missing_directory {
                fixture_io(fs::remove_dir(&fixture.directory));
            } else {
                fixture.write_entry("release-501.registration", &record_bytes());
                fixture.write_entry("development-502.registration", &record_bytes());
                fixture.write_entry("development-0501.registration", &record_bytes());
            }

            assert_eq!(
                fixture.reader.read_fixed_registration(key()),
                Err(StartupError::RegistrationMissing)
            );
            if missing_directory {
                assert!(!fixture.directory.exists());
            } else {
                assert_eq!(
                    fixture.entry_names(),
                    [
                        OsString::from("development-0501.registration"),
                        OsString::from("development-502.registration"),
                        OsString::from("release-501.registration"),
                    ]
                );
            }
        }
    }

    #[test]
    fn reader_preserves_unavailable_file_storage_without_replacing_registration_or_identity() {
        let fixture = Fixture::new();
        fixture.write_entry(RECORD_NAME, &record_bytes());
        fixture
            .controls
            .file_error
            .set(Some(ProtectedDirectoryError::Unavailable));
        let before = fixture.entry_names();
        let record_path = fixture.directory.join(RECORD_NAME);
        let before_metadata = fixture_io(fs::metadata(&record_path));

        assert_eq!(
            fixture.reader.read_fixed_registration(key()),
            Err(StartupError::RegistrationUnavailable)
        );
        assert_eq!(fixture.entry_names(), before);
        let after_metadata = fixture_io(fs::metadata(&record_path));
        assert_eq!(
            (
                after_metadata.dev(),
                after_metadata.ino(),
                after_metadata.ctime(),
                after_metadata.ctime_nsec()
            ),
            (
                before_metadata.dev(),
                before_metadata.ino(),
                before_metadata.ctime(),
                before_metadata.ctime_nsec()
            )
        );
        assert_eq!(fixture_io(fs::read(&record_path)), record_bytes());
    }

    #[test]
    fn reader_maps_native_protection_failures_without_losing_missing_unavailable_or_unsafe() {
        for (native_error, expected_error) in [
            (
                ProtectedDirectoryError::Missing,
                StartupError::RegistrationMissing,
            ),
            (
                ProtectedDirectoryError::Unavailable,
                StartupError::RegistrationUnavailable,
            ),
            (
                ProtectedDirectoryError::Unsafe,
                StartupError::UnsafeRegistration,
            ),
            (
                ProtectedDirectoryError::Changed,
                StartupError::UnsafeRegistration,
            ),
            (
                ProtectedDirectoryError::TooLarge,
                StartupError::UnsafeRegistration,
            ),
            (
                ProtectedDirectoryError::InvalidPath,
                StartupError::UnsafeRegistration,
            ),
            (
                ProtectedDirectoryError::NativeUnavailable,
                StartupError::NativeUnavailable,
            ),
        ] {
            for fail_file_inspection in [false, true] {
                let fixture = Fixture::new();
                fixture.write_entry(RECORD_NAME, &record_bytes());
                if fail_file_inspection {
                    fixture.controls.file_error.set(Some(native_error));
                } else {
                    fixture.controls.directory_error.set(Some(native_error));
                }

                assert_eq!(
                    fixture.reader.read_fixed_registration(key()),
                    Err(expected_error)
                );
            }
        }
    }

    #[test]
    fn reader_rejects_invalid_requested_uids_before_any_native_inspection() {
        let fixture = Fixture::new();
        fixture
            .controls
            .directory_error
            .set(Some(ProtectedDirectoryError::Unavailable));
        for (account_uid, expected_error) in [
            (0, StartupError::RootAccount),
            (u32::MAX, StartupError::UnsafeRegistration),
        ] {
            let requested = AccountBuildKey {
                account_uid,
                ..key()
            };

            assert_eq!(
                fixture.reader.read_fixed_registration(requested),
                Err(expected_error)
            );
            assert_eq!(fixture.controls.inspections.get(), 0);
        }
    }

    #[test]
    fn reader_does_not_follow_a_symlink_in_the_fixed_ancestor_chain() {
        let fixture = Fixture::new();
        fixture.write_entry(RECORD_NAME, &record_bytes());
        let moved = fixture.root.join("moved");
        fixture_io(fs::rename(&fixture.ancestor, &moved));
        fixture_io(symlink(&moved, &fixture.ancestor));

        assert_eq!(
            fixture.reader.read_fixed_registration(key()),
            Err(StartupError::UnsafeRegistration)
        );
    }

    #[test]
    fn reader_does_not_follow_a_symlink_at_the_fixed_registration_entry() {
        let fixture = Fixture::new();
        fixture.write_entry("target", &record_bytes());
        fixture_io(symlink("target", fixture.directory.join(RECORD_NAME)));

        assert_eq!(
            fixture.reader.read_fixed_registration(key()),
            Err(StartupError::UnsafeRegistration)
        );
    }

    #[test]
    fn reader_rejects_a_hardlinked_registration_entry() {
        let fixture = Fixture::new();
        fixture.write_entry("target", &record_bytes());
        fixture_io(fs::hard_link(
            fixture.directory.join("target"),
            fixture.directory.join(RECORD_NAME),
        ));

        assert_eq!(
            fixture.reader.read_fixed_registration(key()),
            Err(StartupError::UnsafeRegistration)
        );
    }

    #[test]
    fn reader_rejects_a_directory_at_the_fixed_registration_entry() {
        let fixture = Fixture::new();
        fixture_io(
            DirBuilder::new()
                .mode(0o700)
                .create(fixture.directory.join(RECORD_NAME)),
        );

        assert_eq!(
            fixture.reader.read_fixed_registration(key()),
            Err(StartupError::UnsafeRegistration)
        );
    }

    #[test]
    fn reader_rejects_native_record_bytes_over_the_exact_read_bound() {
        let fixture = Fixture::new();
        let mut bytes = record_bytes().to_vec();
        bytes.push(0);
        fixture.write_entry(RECORD_NAME, &bytes);

        assert_eq!(
            fixture.reader.read_fixed_registration(key()),
            Err(StartupError::UnsafeRegistration)
        );
    }

    #[test]
    fn reader_rejects_truncated_and_malformed_records_after_bounded_read() {
        let mut malformed = record_bytes();
        malformed[40] = 1;
        for bytes in [
            Vec::new(),
            record_bytes()[..47].to_vec(),
            malformed.to_vec(),
        ] {
            let fixture = Fixture::new();
            fixture.write_entry(RECORD_NAME, &bytes);

            assert_eq!(
                fixture.reader.read_fixed_registration(key()),
                Err(StartupError::UnsafeRegistration)
            );
            assert!(fixture.controls.inspections.get() > 0);
        }
    }

    #[test]
    fn reader_rejects_a_valid_other_identity_under_the_requested_fixed_entry_name() {
        let fixture = Fixture::new();
        let mut bytes = record_bytes();
        bytes[12..16].copy_from_slice(&502_u32.to_le_bytes());
        fixture.write_entry(RECORD_NAME, &bytes);

        assert_eq!(
            fixture.reader.read_fixed_registration(key()),
            Err(StartupError::IdentityMismatch)
        );
    }

    #[test]
    fn reader_rejects_observed_ancestor_replacement_instead_of_accepting_reopened_path_bytes() {
        let fixture = Fixture::new();
        fixture.write_entry(RECORD_NAME, &record_bytes());
        let ancestor = fixture.ancestor.clone();
        let moved = fixture.root.join("retained-original");
        *fixture.controls.on_file.borrow_mut() = Some(Box::new(move || {
            fixture_io(fs::rename(&ancestor, &moved));
            fixture_io(DirBuilder::new().mode(0o700).create(&ancestor));
        }));

        assert_eq!(
            fixture.reader.read_fixed_registration(key()),
            Err(StartupError::UnsafeRegistration)
        );
    }
}

/// A schema-2 startup barrier. The head remains the only approved selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum StartupBarrier {
    /// No active native transaction.
    Open = 0,
    /// An admitted transaction inhibits startup.
    Inhibited = 1,
    /// Validated repair is required before startup.
    RecoveryRequired = 2,
    /// Only the journal's committed approved selection may start.
    ApprovedStartOnly = 3,
}

/// Comparison facts decoded from the exact 256-byte schema-2 head.
///
/// `server_id` holds canonical core identity bytes, not a second identity type.
/// Neither construction nor decoding grants publication or credential authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrationHeadV2 {
    /// Exact account/build key.
    pub key: AccountBuildKey,
    /// Incremented at every head publication.
    pub generation: NonZeroU64,
    /// Durable lifecycle intent.
    pub desired_state: DesiredServerState,
    /// The sole runtime selected for startup.
    pub approved_installation: InstallationId,
    /// An authenticated candidate, never selected implicitly.
    pub staged_installation: Option<InstallationId>,
    /// Data generation selected atomically with the approved installation.
    pub approved_data_generation: crate::native_deployment::contracts::DataGenerationId,
    /// Active transaction identifier, if startup is restricted.
    pub active_request: Option<crate::native_deployment::contracts::NativeRequestId>,
    /// Startup restriction associated with a validated journal.
    pub startup_barrier: StartupBarrier,
    /// Digest of the immutable protected profile.
    pub profile_sha256: [u8; 32],
    /// Digest of the approved installation manifest.
    pub approved_manifest_sha256: [u8; 32],
    /// Digest of the exact approved descriptor bytes.
    pub approved_descriptor_sha256: [u8; 32],
    /// Digest of staged descriptor, present exactly when its ID is present.
    pub staged_descriptor_sha256: Option<[u8; 32]>,
    /// Protected installation allocation high-water mark.
    pub installation_high_water: u64,
    /// Protected data allocation high-water mark.
    pub data_high_water: u64,
    /// Canonical 16-byte core ServerId representation.
    pub server_id: [u8; 16],
    /// Advanced by explicit lifecycle requests.
    pub lifecycle_generation: NonZeroU64,
}

/// Decodes schema 2 separately from the retained exact schema-1 reader.
/// Identity mismatch is reported only after every structural invariant is checked.
pub fn decode_registration_head(
    bytes: &[u8],
    key: AccountBuildKey,
) -> Result<RegistrationHeadV2, crate::native_deployment::contracts::NativeFailure> {
    use crate::native_deployment::contracts::{
        DataGenerationId, NativeFailure, NativeRequestId, validate_key,
    };
    validate_key(key)?;
    if bytes.len() == 48 && bytes.get(..10) == Some(b"STRGREC\0\x01\x00") {
        return match decode_registration_record(bytes, key) {
            Ok(_) => Err(NativeFailure::LegacyRegistrationRequiresMigration),
            Err(StartupError::IdentityMismatch) => Err(NativeFailure::IdentityMismatch),
            Err(_) => Err(NativeFailure::InvalidRecord),
        };
    }
    let b: &[u8; 256] = bytes.try_into().map_err(|_| NativeFailure::InvalidRecord)?;
    if &b[..8] != b"STRGREC\0" {
        return Err(NativeFailure::InvalidRecord);
    }
    if u16::from_le_bytes([b[8], b[9]]) != 2 {
        return Err(NativeFailure::UnsupportedSchema);
    }
    if b[65..72].iter().chain(b[240..].iter()).any(|v| *v != 0) {
        return Err(NativeFailure::InvalidRecord);
    }
    let build_identity = match b[10] {
        1 => BuildIdentity::Release,
        2 => BuildIdentity::Preflight,
        3 => BuildIdentity::Development,
        _ => return Err(NativeFailure::InvalidRecord),
    };
    let desired_state = match b[11] {
        1 => DesiredServerState::Running,
        2 => DesiredServerState::Stopped,
        _ => return Err(NativeFailure::InvalidRecord),
    };
    let stored_key = AccountBuildKey {
        account_uid: u32::from_le_bytes(
            b[12..16]
                .try_into()
                .map_err(|_| NativeFailure::InvalidRecord)?,
        ),
        build_identity,
    };
    let number = |start: usize| -> Result<u64, NativeFailure> {
        Ok(u64::from_le_bytes(
            b[start..start + 8]
                .try_into()
                .map_err(|_| NativeFailure::InvalidRecord)?,
        ))
    };
    let nz = |start| NonZeroU64::new(number(start)?).ok_or(NativeFailure::InvalidRecord);
    let request: [u8; 16] = b[48..64]
        .try_into()
        .map_err(|_| NativeFailure::InvalidRecord)?;
    let digest = |start: usize| {
        b[start..start + 32]
            .try_into()
            .map_err(|_| NativeFailure::InvalidRecord)
    };
    let staged_installation = NonZeroU64::new(number(32)?).map(InstallationId);
    let staged_digest = digest(168)?;
    let head = RegistrationHeadV2 {
        key: stored_key,
        generation: nz(16)?,
        desired_state,
        approved_installation: InstallationId(nz(24)?),
        staged_installation,
        approved_data_generation: DataGenerationId::new(number(40)?)?,
        active_request: if request == [0; 16] {
            None
        } else {
            Some(NativeRequestId::new(request)?)
        },
        startup_barrier: match b[64] {
            0 => StartupBarrier::Open,
            1 => StartupBarrier::Inhibited,
            2 => StartupBarrier::RecoveryRequired,
            3 => StartupBarrier::ApprovedStartOnly,
            _ => return Err(NativeFailure::InvalidRecord),
        },
        profile_sha256: digest(72)?,
        approved_manifest_sha256: digest(104)?,
        approved_descriptor_sha256: digest(136)?,
        staged_descriptor_sha256: if staged_digest == [0; 32] {
            None
        } else {
            Some(staged_digest)
        },
        installation_high_water: number(200)?,
        data_high_water: number(208)?,
        server_id: b[216..232]
            .try_into()
            .map_err(|_| NativeFailure::InvalidRecord)?,
        lifecycle_generation: nz(232)?,
    };
    validate_registration_head(&head)?;
    if head.key != key {
        return Err(NativeFailure::IdentityMismatch);
    }
    Ok(head)
}

/// Checks local head invariants. Matching journal and historical marks require retained authority.
pub fn validate_registration_head(
    head: &RegistrationHeadV2,
) -> Result<(), crate::native_deployment::contracts::NativeFailure> {
    use crate::native_deployment::contracts::{NativeFailure, nonzero, validate_key};
    validate_key(head.key)?;
    nonzero(&head.server_id)?;
    for digest in [
        &head.profile_sha256,
        &head.approved_manifest_sha256,
        &head.approved_descriptor_sha256,
    ] {
        nonzero(digest)?;
    }
    if head.staged_installation.is_some() != head.staged_descriptor_sha256.is_some()
        || head.staged_installation == Some(head.approved_installation)
        || (head.startup_barrier == StartupBarrier::Open) != head.active_request.is_none()
        || head.installation_high_water < head.approved_installation.0.get()
        || head
            .staged_installation
            .is_some_and(|id| id.0.get() > head.installation_high_water)
        || head.data_high_water < head.approved_data_generation.get()
    {
        return Err(NativeFailure::InvalidRecord);
    }
    if let Some(digest) = head.staged_descriptor_sha256 {
        nonzero(&digest)?;
    }
    Ok(())
}

/// Encodes exactly 256 bytes, without reinterpreting the legacy record's reserved fields.
pub fn encode_registration_head(
    head: &RegistrationHeadV2,
) -> Result<[u8; 256], crate::native_deployment::contracts::NativeFailure> {
    use crate::native_deployment::contracts::build_code;
    validate_registration_head(head)?;
    let mut b = [0; 256];
    b[..8].copy_from_slice(b"STRGREC\0");
    b[8..10].copy_from_slice(&2u16.to_le_bytes());
    b[10] = build_code(head.key.build_identity);
    b[11] = match head.desired_state {
        DesiredServerState::Running => 1,
        DesiredServerState::Stopped => 2,
    };
    b[12..16].copy_from_slice(&head.key.account_uid.to_le_bytes());
    for (start, value) in [
        (16, head.generation.get()),
        (24, head.approved_installation.0.get()),
        (32, head.staged_installation.map_or(0, |id| id.0.get())),
        (40, head.approved_data_generation.get()),
        (200, head.installation_high_water),
        (208, head.data_high_water),
        (232, head.lifecycle_generation.get()),
    ] {
        b[start..start + 8].copy_from_slice(&value.to_le_bytes());
    }
    if let Some(request) = head.active_request {
        b[48..64].copy_from_slice(request.as_bytes());
    }
    b[64] = head.startup_barrier as u8;
    b[72..104].copy_from_slice(&head.profile_sha256);
    b[104..136].copy_from_slice(&head.approved_manifest_sha256);
    b[136..168].copy_from_slice(&head.approved_descriptor_sha256);
    if let Some(digest) = head.staged_descriptor_sha256 {
        b[168..200].copy_from_slice(&digest);
    }
    b[216..232].copy_from_slice(&head.server_id);
    Ok(b)
}

pub mod writer;
