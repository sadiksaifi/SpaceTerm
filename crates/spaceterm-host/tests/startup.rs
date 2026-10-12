#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::cell::RefCell;
use std::num::NonZeroU64;

use spaceterm_host::startup::{
    AccountBuildKey, BuildIdentity, DesiredServerState, InstallationId, NativeStartupRegistration,
    ProtectedRegistration, StartupAdmission, StartupError, admit_startup,
};

// This adapter models protected native records, not account isolation or a real launcher.
struct NativeFixture {
    registration: Result<ProtectedRegistration, StartupError>,
    retention_error: Option<StartupError>,
    reads: RefCell<Vec<AccountBuildKey>>,
    retentions: RefCell<Vec<(AccountBuildKey, NonZeroU64, InstallationId)>>,
}

// Deliberately lacks Clone and Debug. Ticket diagnostics must not inspect native authority.
struct RetainedFixtureInstallation;

impl NativeFixture {
    fn new(registration: Result<ProtectedRegistration, StartupError>) -> Self {
        Self {
            registration,
            retention_error: None,
            reads: RefCell::new(Vec::new()),
            retentions: RefCell::new(Vec::new()),
        }
    }
}

impl NativeStartupRegistration for NativeFixture {
    type RetainedInstallation = RetainedFixtureInstallation;

    fn read_fixed_registration(
        &self,
        key: AccountBuildKey,
    ) -> Result<ProtectedRegistration, StartupError> {
        self.reads.borrow_mut().push(key);
        self.registration
    }

    fn retain_approved_installation(
        &self,
        key: AccountBuildKey,
        generation: NonZeroU64,
        installation: InstallationId,
    ) -> Result<Self::RetainedInstallation, StartupError> {
        self.retentions
            .borrow_mut()
            .push((key, generation, installation));
        if let Some(error) = self.retention_error {
            return Err(error);
        }
        Ok(RetainedFixtureInstallation)
    }
}

fn generation(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("fixture generation must be nonzero")
}

fn requested_key() -> AccountBuildKey {
    AccountBuildKey {
        account_uid: 501,
        build_identity: BuildIdentity::Development,
    }
}

fn registration(
    key: AccountBuildKey,
    generation: NonZeroU64,
    desired_state: DesiredServerState,
) -> ProtectedRegistration {
    ProtectedRegistration {
        key,
        generation,
        desired_state,
        approved_installation: InstallationId(self::generation(11)),
        staged_installation: Some(InstallationId(self::generation(99))),
    }
}

#[test]
fn running_admission_reads_the_exact_key_and_retains_approved_not_newer_staged_installation() {
    let key = requested_key();
    let generation = generation(7);
    let record = registration(key, generation, DesiredServerState::Running);
    let native = NativeFixture::new(Ok(record));

    let admission = admit_startup(key, generation, &native);

    assert!(
        matches!(&admission, Ok(StartupAdmission::Ready(_))),
        "exact protected startup admission failed: {admission:?}"
    );
    assert_eq!(native.reads.borrow().as_slice(), &[key]);
    assert_eq!(
        native.retentions.borrow().as_slice(),
        &[(key, generation, record.approved_installation)]
    );
}

#[test]
fn admission_rejects_other_accounts_build_identities_and_nonexact_generations_before_retention() {
    let key = requested_key();
    let requested_generation = generation(7);
    let other_account = AccountBuildKey {
        account_uid: 502,
        ..key
    };
    let release = AccountBuildKey {
        build_identity: BuildIdentity::Release,
        ..key
    };
    let preflight = AccountBuildKey {
        build_identity: BuildIdentity::Preflight,
        ..key
    };
    let cases = [
        (
            other_account,
            generation(7),
            DesiredServerState::Running,
            StartupError::IdentityMismatch,
        ),
        (
            release,
            generation(7),
            DesiredServerState::Running,
            StartupError::IdentityMismatch,
        ),
        (
            preflight,
            generation(7),
            DesiredServerState::Running,
            StartupError::IdentityMismatch,
        ),
        (
            key,
            generation(6),
            DesiredServerState::Running,
            StartupError::GenerationMismatch,
        ),
        (
            key,
            generation(8),
            DesiredServerState::Running,
            StartupError::GenerationMismatch,
        ),
        (
            other_account,
            generation(7),
            DesiredServerState::Stopped,
            StartupError::IdentityMismatch,
        ),
        (
            key,
            generation(6),
            DesiredServerState::Stopped,
            StartupError::GenerationMismatch,
        ),
    ];

    for (registered_key, registered_generation, state, expected_error) in cases {
        let native = NativeFixture::new(Ok(registration(
            registered_key,
            registered_generation,
            state,
        )));

        let admission = admit_startup(key, requested_generation, &native);

        assert_eq!(admission.err(), Some(expected_error));
        assert_eq!(native.reads.borrow().as_slice(), &[key]);
        assert!(native.retentions.borrow().is_empty());
    }
}

#[test]
fn root_account_rejection_precedes_even_the_protected_registration_read() {
    let root_key = AccountBuildKey {
        account_uid: 0,
        ..requested_key()
    };
    let native = NativeFixture::new(Err(StartupError::RegistrationUnavailable));

    let admission = admit_startup(root_key, generation(7), &native);

    assert_eq!(admission.err(), Some(StartupError::RootAccount));
    assert!(native.reads.borrow().is_empty());
    assert!(native.retentions.borrow().is_empty());
}

#[test]
fn uid_sentinel_rejection_precedes_a_matching_running_registration_read_and_retention() {
    for build_identity in [
        BuildIdentity::Release,
        BuildIdentity::Preflight,
        BuildIdentity::Development,
    ] {
        let key = AccountBuildKey {
            account_uid: u32::MAX,
            build_identity,
        };
        let expected_generation = generation(7);
        let native = NativeFixture::new(Ok(registration(
            key,
            expected_generation,
            DesiredServerState::Running,
        )));

        let admission = admit_startup(key, expected_generation, &native);

        assert!(
            matches!(&admission, Err(StartupError::UnsafeRegistration)),
            "UID sentinel must be rejected before Running admission: {admission:?}"
        );
        assert!(native.reads.borrow().is_empty());
        assert!(native.retentions.borrow().is_empty());
    }
}

#[test]
fn uid_sentinel_rejection_precedes_a_matching_stopped_registration_read_and_retention() {
    for build_identity in [
        BuildIdentity::Release,
        BuildIdentity::Preflight,
        BuildIdentity::Development,
    ] {
        let key = AccountBuildKey {
            account_uid: u32::MAX,
            build_identity,
        };
        let expected_generation = generation(7);
        let native = NativeFixture::new(Ok(registration(
            key,
            expected_generation,
            DesiredServerState::Stopped,
        )));

        let admission = admit_startup(key, expected_generation, &native);

        assert!(
            matches!(&admission, Err(StartupError::UnsafeRegistration)),
            "UID sentinel must be rejected before Stopped admission: {admission:?}"
        );
        assert!(native.reads.borrow().is_empty());
        assert!(native.retentions.borrow().is_empty());
    }
}

#[test]
fn deliberate_stopped_admission_does_not_prepare_an_installation() {
    let key = requested_key();
    let generation = generation(7);
    let mut native = NativeFixture::new(Ok(registration(
        key,
        generation,
        DesiredServerState::Stopped,
    )));
    // An eager installation step would fail. Stopped must return before that step.
    native.retention_error = Some(StartupError::InstallationUnavailable);

    let admission = admit_startup(key, generation, &native);

    assert!(
        matches!(&admission, Ok(StartupAdmission::Stopped)),
        "deliberate Stop admission failed: {admission:?}"
    );
    assert_eq!(native.reads.borrow().as_slice(), &[key]);
    assert!(native.retentions.borrow().is_empty());
}

#[test]
fn admission_preserves_missing_unavailable_and_unsafe_registration_failures_without_retention() {
    let key = requested_key();
    for expected_error in [
        StartupError::RegistrationMissing,
        StartupError::RegistrationUnavailable,
        StartupError::UnsafeRegistration,
    ] {
        let native = NativeFixture::new(Err(expected_error));

        let admission = admit_startup(key, generation(7), &native);

        assert_eq!(admission.err(), Some(expected_error));
        assert_eq!(native.reads.borrow().as_slice(), &[key]);
        assert!(native.retentions.borrow().is_empty());
    }
}

#[test]
fn an_unavailable_approved_installation_does_not_fall_back_to_the_staged_installation() {
    let key = requested_key();
    let generation = generation(7);
    let record = registration(key, generation, DesiredServerState::Running);
    let mut native = NativeFixture::new(Ok(record));
    native.retention_error = Some(StartupError::InstallationUnavailable);

    let admission = admit_startup(key, generation, &native);

    assert_eq!(admission.err(), Some(StartupError::InstallationUnavailable));
    assert_eq!(
        native.retentions.borrow().as_slice(),
        &[(key, generation, record.approved_installation)]
    );
}

mod native_composition {
    use spaceterm_host::{
        boot_credentials::*,
        native_deployment::{contracts::NativeFailure, profile::BootCredentialBinding},
        startup::*,
    };
    use std::{
        num::NonZeroU64,
        sync::atomic::{AtomicUsize, Ordering},
    };
    struct Authority(AtomicUsize);
    impl NativeStartupAuthority for Authority {
        fn admit(
            &self,
            _: AccountBuildKey,
            _: NonZeroU64,
        ) -> Result<NativeStartupAdmission, NativeFailure> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(NativeStartupAdmission::Stopped)
        }
        fn authorize_release(
            &self,
            _: &PendingStartup,
            _: &PostExecProof,
        ) -> Result<CredentialReleasePermit, NativeFailure> {
            panic!("Stopped must not authorize credential release")
        }
        fn recheck_release(&self, _: &CredentialReleasePermit) -> Result<(), NativeFailure> {
            panic!("Stopped must not recheck credential release")
        }
    }
    struct Credentials;
    impl BootCredentialAdapter for Credentials {
        fn inspect_policy(
            &self,
            _: &BootCredentialBinding,
        ) -> Result<CredentialPolicyFacts, NativeFailure> {
            panic!("Stopped must not inspect credentials")
        }
        fn retrieve_for_startup(
            &self,
            _: &CredentialReleasePermit,
            _: &mut LockedSecret32,
        ) -> Result<(), NativeFailure> {
            panic!("Stopped must not retrieve credentials")
        }
    }
    struct Launcher;
    impl OneShotLauncher for Launcher {
        fn launch(
            &mut self,
            _: PendingStartup,
            _: &dyn NativeStartupAuthority,
            _: &dyn BootCredentialAdapter,
        ) -> Result<std::convert::Infallible, NativeFailure> {
            panic!("Stopped must not create pipes, fork, or retain an installation")
        }
    }
    #[test]
    fn stopped_precedes_credential_policy_and_installation_retention() {
        let key = AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        };
        let authority = Authority(AtomicUsize::new(0));
        assert_eq!(
            launch_once(
                key,
                NonZeroU64::new(7).unwrap(),
                &authority,
                &Credentials,
                &mut Launcher
            ),
            Ok(())
        );
        assert_eq!(authority.0.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn root_and_uid_sentinel_reject_before_native_authority_composition() {
        let authority = Authority(AtomicUsize::new(0));
        for account_uid in [0, u32::MAX] {
            let key = AccountBuildKey {
                account_uid,
                build_identity: BuildIdentity::Development,
            };
            assert_eq!(
                launch_once(
                    key,
                    NonZeroU64::new(7).unwrap(),
                    &authority,
                    &Credentials,
                    &mut Launcher
                ),
                Err(NativeFailure::InvalidRecord)
            );
        }
        assert_eq!(authority.0.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn production_launcher_fails_before_credentials_without_native_publication_authority() {
        let key = AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        };
        assert_eq!(
            launch_once(
                key,
                NonZeroU64::new(7).unwrap(),
                &SystemNativeStartupAuthority,
                &Credentials,
                &mut Launcher
            ),
            Err(NativeFailure::CustodyUnavailable)
        );
    }
}
