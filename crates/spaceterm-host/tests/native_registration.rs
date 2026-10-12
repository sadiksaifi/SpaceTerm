use std::num::NonZeroU64;

use spaceterm_host::native_registration::{NativeRegistrationReader, decode_registration_record};
use spaceterm_host::startup::{
    AccountBuildKey, BuildIdentity, DesiredServerState, InstallationId, ProtectedRegistration,
    StartupError,
};

fn key() -> AccountBuildKey {
    AccountBuildKey {
        account_uid: 501,
        build_identity: BuildIdentity::Development,
    }
}

fn nonzero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("fixture identity must be nonzero")
}

// Explicit native schema fixture, not serialization of the Rust domain struct.
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
        generation: nonzero(7),
        desired_state: DesiredServerState::Running,
        approved_installation: InstallationId(nonzero(11)),
        staged_installation: Some(InstallationId(nonzero(99))),
    }
}

#[test]
fn decoder_binds_each_build_and_desired_state_without_promoting_staged_installation() {
    for (build_code, build_identity) in [
        (1, BuildIdentity::Release),
        (2, BuildIdentity::Preflight),
        (3, BuildIdentity::Development),
    ] {
        for (state_code, desired_state) in [
            (1, DesiredServerState::Running),
            (2, DesiredServerState::Stopped),
        ] {
            let requested = AccountBuildKey {
                build_identity,
                ..key()
            };
            let mut bytes = record_bytes();
            bytes[10] = build_code;
            bytes[11] = state_code;
            let expected = ProtectedRegistration {
                key: requested,
                desired_state,
                ..expected_record()
            };

            assert_eq!(decode_registration_record(&bytes, requested), Ok(expected));
        }
    }
}

#[test]
fn decoder_treats_zero_staged_identifier_as_absent_without_changing_approved() {
    let mut bytes = record_bytes();
    bytes[32..40].fill(0);
    let expected = ProtectedRegistration {
        staged_installation: None,
        ..expected_record()
    };

    assert_eq!(decode_registration_record(&bytes, key()), Ok(expected));
}

#[test]
fn decoder_preserves_full_width_little_endian_identities_without_signed_narrowing() {
    let requested = AccountBuildKey {
        account_uid: u32::MAX - 1,
        ..key()
    };
    let mut bytes = record_bytes();
    bytes[12..16].copy_from_slice(&requested.account_uid.to_le_bytes());
    bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    bytes[24..32].copy_from_slice(&(u64::MAX - 1).to_le_bytes());
    bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
    let expected = ProtectedRegistration {
        key: requested,
        generation: nonzero(u64::MAX),
        approved_installation: InstallationId(nonzero(u64::MAX - 1)),
        staged_installation: Some(InstallationId(nonzero(u64::MAX))),
        ..expected_record()
    };

    assert_eq!(decode_registration_record(&bytes, requested), Ok(expected));
}

#[test]
fn decoder_rejects_every_truncated_prefix_and_all_trailing_or_oversized_bytes() {
    let bytes = record_bytes();
    for length in 0..48 {
        assert_eq!(
            decode_registration_record(&bytes[..length], key()),
            Err(StartupError::UnsafeRegistration),
            "a truncated native record was admitted"
        );
    }
    for extra_bytes in [1, 8, 4096] {
        let mut extended = bytes.to_vec();
        extended.resize(48 + extra_bytes, 0);
        assert_eq!(
            decode_registration_record(&extended, key()),
            Err(StartupError::UnsafeRegistration),
            "trailing native record data was admitted"
        );
    }
}

#[test]
fn decoder_rejects_unknown_magic_schema_build_state_and_reserved_data() {
    let original = record_bytes();
    for offset in (0..8).chain(40..48) {
        let mut bytes = original;
        bytes[offset] ^= 1;
        assert_eq!(
            decode_registration_record(&bytes, key()),
            Err(StartupError::UnsafeRegistration)
        );
    }
    for schema in [0_u16, 2, u16::MAX] {
        let mut bytes = original;
        bytes[8..10].copy_from_slice(&schema.to_le_bytes());
        assert_eq!(
            decode_registration_record(&bytes, key()),
            Err(StartupError::UnsafeRegistration)
        );
    }
    for code in [0, 4, u8::MAX] {
        let mut bytes = original;
        bytes[10] = code;
        assert_eq!(
            decode_registration_record(&bytes, key()),
            Err(StartupError::UnsafeRegistration)
        );
    }
    for code in [0, 3, u8::MAX] {
        let mut bytes = original;
        bytes[11] = code;
        assert_eq!(
            decode_registration_record(&bytes, key()),
            Err(StartupError::UnsafeRegistration)
        );
    }
}

#[test]
fn decoder_rejects_zero_generation_approved_identifier_and_invalid_stored_uid() {
    for range in [16..24, 24..32] {
        let mut bytes = record_bytes();
        bytes[range].fill(0);
        assert_eq!(
            decode_registration_record(&bytes, key()),
            Err(StartupError::UnsafeRegistration)
        );
    }
    for account_uid in [0_u32, u32::MAX] {
        let mut bytes = record_bytes();
        bytes[12..16].copy_from_slice(&account_uid.to_le_bytes());
        assert_eq!(
            decode_registration_record(&bytes, key()),
            Err(StartupError::UnsafeRegistration)
        );
    }
}

#[test]
fn decoder_rejects_root_and_native_uid_sentinel_requests_before_decoding() {
    for (account_uid, expected_error) in [
        (0, StartupError::RootAccount),
        (u32::MAX, StartupError::UnsafeRegistration),
    ] {
        let requested = AccountBuildKey {
            account_uid,
            ..key()
        };
        assert_eq!(
            decode_registration_record(&[], requested),
            Err(expected_error)
        );
    }
}

#[test]
fn decoder_does_not_relabel_another_valid_account_or_build_as_the_requested_identity() {
    for (offset, replacement) in [
        (12, 502_u32.to_le_bytes().to_vec()),
        (10, vec![1]),
        (10, vec![2]),
    ] {
        let mut bytes = record_bytes();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        assert_eq!(
            decode_registration_record(&bytes, key()),
            Err(StartupError::IdentityMismatch)
        );
    }
}

#[test]
fn decoder_rejects_malformed_other_identities_before_classifying_identity_mismatch() {
    let mut other_account = record_bytes();
    other_account[12..16].copy_from_slice(&502_u32.to_le_bytes());
    let mut other_build = record_bytes();
    other_build[10] = 1;
    for other_identity in [other_account, other_build] {
        for range in [16..24, 24..32] {
            let mut bytes = other_identity;
            bytes[range].fill(0);

            assert_eq!(
                decode_registration_record(&bytes, key()),
                Err(StartupError::UnsafeRegistration)
            );
        }
    }
}

#[test]
fn fixed_reader_rejects_root_and_native_uid_sentinel_requests_on_every_host() {
    let reader = NativeRegistrationReader::new();
    for (account_uid, expected_error) in [
        (0, StartupError::RootAccount),
        (u32::MAX, StartupError::UnsafeRegistration),
    ] {
        let requested = AccountBuildKey {
            account_uid,
            ..key()
        };

        assert_eq!(
            reader.read_fixed_registration(requested),
            Err(expected_error)
        );
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[test]
fn fixed_reader_fails_closed_on_an_unsupported_host() {
    let reader = NativeRegistrationReader::new();

    assert_eq!(
        reader.read_fixed_registration(key()),
        Err(StartupError::NativeUnavailable)
    );
}

#[test]
fn decoder_errors_have_no_payload_path_native_error_or_error_source() {
    use std::error::Error;

    let mut bytes = record_bytes().to_vec();
    bytes.extend_from_slice(b"/private/credential-fixture denied-native-value");
    let result = decode_registration_record(&bytes, key());

    assert_eq!(result, Err(StartupError::UnsafeRegistration));
    let error = result.expect_err("invalid native record must fail");
    assert_eq!(format!("{error:?}"), "UnsafeRegistration");
    assert_eq!(
        error.to_string(),
        "the protected server registration is unsafe"
    );
    assert!(error.source().is_none());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod startup_composition {
    use std::cell::RefCell;
    use std::convert::Infallible;

    use spaceterm_host::startup::{NativeStartupRegistration, StartupAdmission, admit_startup};

    use super::*;

    // This fixture has candidate bytes, not root protection, authenticated code or credentials.
    struct CandidateFixture {
        bytes: [u8; 48],
        preparations: RefCell<Vec<(AccountBuildKey, NonZeroU64, InstallationId)>>,
    }

    impl NativeStartupRegistration for CandidateFixture {
        type RetainedInstallation = Infallible;

        fn read_fixed_registration(
            &self,
            key: AccountBuildKey,
        ) -> Result<ProtectedRegistration, StartupError> {
            decode_registration_record(&self.bytes, key)
        }

        fn retain_approved_installation(
            &self,
            key: AccountBuildKey,
            generation: NonZeroU64,
            installation: InstallationId,
        ) -> Result<Self::RetainedInstallation, StartupError> {
            self.preparations
                .borrow_mut()
                .push((key, generation, installation));
            Err(StartupError::InstallationUnavailable)
        }
    }

    #[test]
    fn decoded_stopped_registration_returns_before_any_approved_installation_preparation() {
        let mut bytes = record_bytes();
        bytes[11] = 2;
        let native = CandidateFixture {
            bytes,
            preparations: RefCell::new(Vec::new()),
        };

        let admission = admit_startup(key(), nonzero(7), &native);

        assert!(
            matches!(admission, Ok(StartupAdmission::Stopped)),
            "decoded deliberate Stop must precede installation preparation"
        );
        assert!(native.preparations.borrow().is_empty());
    }

    #[test]
    fn decoded_running_registration_prepares_only_approved_and_cannot_issue_a_candidate_ticket() {
        let native = CandidateFixture {
            bytes: record_bytes(),
            preparations: RefCell::new(Vec::new()),
        };

        let admission = admit_startup(key(), nonzero(7), &native);

        assert_eq!(admission.err(), Some(StartupError::InstallationUnavailable));
        assert_eq!(
            native.preparations.borrow().as_slice(),
            &[(key(), nonzero(7), InstallationId(nonzero(11)))]
        );
    }

    #[test]
    fn decoded_nonexact_generation_fails_before_installation_preparation() {
        let native = CandidateFixture {
            bytes: record_bytes(),
            preparations: RefCell::new(Vec::new()),
        };

        let admission = admit_startup(key(), nonzero(8), &native);

        assert_eq!(admission.err(), Some(StartupError::GenerationMismatch));
        assert!(native.preparations.borrow().is_empty());
    }

    #[test]
    #[ignore = "requires separate authorization and a preinstalled root-owned stopped native record"]
    fn authorized_fixed_root_record_fixture_is_read_without_installation_or_credential_access() {
        use spaceterm_host::native_registration::NativeRegistrationReader;

        // Explicit opt-in in addition to --ignored. This test never installs a fixture.
        assert_eq!(
            std::env::var("SPACETERM_NATIVE_REGISTRATION_FIXTURE")
                .ok()
                .as_deref(),
            Some("authorized-stopped-v1"),
            "root-qualified native evidence requires separate host authorization"
        );
        let expected = ProtectedRegistration {
            desired_state: DesiredServerState::Stopped,
            ..expected_record()
        };
        let reader = NativeRegistrationReader::new();

        assert_eq!(reader.read_fixed_registration(key()), Ok(expected));
    }
}
