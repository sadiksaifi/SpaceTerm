use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use spaceterm_directories::AppDirectoryEnvironment;
use spaceterm_host::{
    native_deployment::{
        account::{AccountBinding, NativeAccountId, NativeAccountProvider, SystemAccountProvider},
        contracts::{DataGenerationId, NativeFailure},
        profile::{BootCredentialBinding, RegisteredRoots, RegistrationProfile, StorageBinding},
    },
    native_registration::{RegistrationHeadV2, StartupBarrier},
    startup::{
        AccountBuildKey, BuildIdentity, DesiredServerState, InstallationId, StartupInspection,
        StartupPreflight, StartupSnapshot, inspect_startup,
    },
};
use std::{
    num::NonZeroU64,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

fn key() -> AccountBuildKey {
    AccountBuildKey {
        account_uid: 501,
        build_identity: BuildIdentity::Development,
    }
}
fn account() -> AccountBinding {
    AccountBinding::new(key(), 20, NativeAccountId::macos([1; 16]).unwrap()).unwrap()
}
fn profile() -> RegistrationProfile {
    let roots = RegisteredRoots::new(
        "/first/config".into(),
        "/first/data".into(),
        "/first/state".into(),
        "/first/cache".into(),
    )
    .unwrap();
    let boot = BootCredentialBinding::macos([2; 32], [3; 32], account().sha256(), [4; 16]).unwrap();
    RegistrationProfile::new(
        key(),
        20,
        [1; 16],
        [4; 16],
        roots,
        boot,
        StorageBinding::new(vec![5; 16]).unwrap(),
    )
    .unwrap()
}
fn head(profile: &RegistrationProfile, desired_state: DesiredServerState) -> RegistrationHeadV2 {
    RegistrationHeadV2 {
        key: key(),
        generation: NonZeroU64::new(7).unwrap(),
        desired_state,
        approved_installation: InstallationId(NonZeroU64::new(11).unwrap()),
        staged_installation: Some(InstallationId(NonZeroU64::new(12).unwrap())),
        approved_data_generation: DataGenerationId::new(1).unwrap(),
        active_request: None,
        startup_barrier: StartupBarrier::Open,
        profile_sha256: Sha256::digest(profile.encode().unwrap()).into(),
        approved_manifest_sha256: [6; 32],
        approved_descriptor_sha256: [7; 32],
        staged_descriptor_sha256: Some([8; 32]),
        installation_high_water: 12,
        data_high_water: 1,
        server_id: [4; 16],
        lifecycle_generation: NonZeroU64::new(2).unwrap(),
    }
}

#[test]
fn uid_reuse_cannot_inherit_native_account_binding() {
    let old = account();
    let recreated =
        AccountBinding::new(key(), 20, NativeAccountId::macos([9; 16]).unwrap()).unwrap();
    assert_eq!(
        old.compare(&recreated),
        Err(NativeFailure::IdentityMismatch)
    );
    assert_ne!(old.sha256(), recreated.sha256());
    let regrouped =
        AccountBinding::new(key(), 21, NativeAccountId::macos([1; 16]).unwrap()).unwrap();
    assert_eq!(
        old.compare(&regrouped),
        Err(NativeFailure::IdentityMismatch)
    );
    let mut canonical = b"SpaceTerm native account binding v1\0".to_vec();
    canonical.extend([1, 3]);
    canonical.extend(501u32.to_le_bytes());
    canonical.extend(20u32.to_le_bytes());
    canonical.extend(16u16.to_le_bytes());
    canonical.extend([1; 16]);
    let expected: [u8; 32] = Sha256::digest(canonical).into();
    assert_eq!(old.sha256(), expected);
}

#[test]
fn discovery_reuses_first_approved_xdg_roots() {
    let p = profile();
    let later = AppDirectoryEnvironment {
        home: Some("/other/home".into()),
        xdg_config_home: Some("/other/config".into()),
        ..Default::default()
    };
    let proposal = RegisteredRoots::resolve_initial(BuildIdentity::Development, &later).unwrap();
    assert_ne!(proposal.config(), p.roots().config());
    let retained = RegistrationProfile::decode(&p.encode().unwrap(), key()).unwrap();
    assert_eq!(
        retained.roots().config(),
        std::path::Path::new("/first/config")
    );
    assert_eq!(retained.roots(), p.roots());
}

#[test]
fn profile_rejects_unknown_duplicate_tags_and_unsafe_pathnames() {
    let bytes = profile().encode().unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    for bad in [
        text.replacen('{', "{\"schema_version\":1,", 1),
        text.replacen('{', "{\"unknown\":1,", 1),
        text.replace("\"native-volume\"", "\"exists\""),
        text.replace("\"macos\"", "\"unknown\""),
        text.replace("\"roots\":{", "\"roots\":{\"unknown\":1,"),
        text.replace("\"schema_version\":1", "\"schema_version\":2"),
    ] {
        assert!(RegistrationProfile::decode(bad.as_bytes(), key()).is_err());
    }
    for path in [
        b"relative".as_slice(),
        b"/root/../other",
        b"/root/./data",
        b"/root/\0data",
        b"/root//data",
    ] {
        let mut object: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        object["roots"]["data"] = STANDARD.encode(path).into();
        assert!(RegistrationProfile::decode(&serde_json::to_vec(&object).unwrap(), key()).is_err());
    }
    assert!(
        RegisteredRoots::new(
            PathBuf::from("/valid"),
            PathBuf::from("../invalid"),
            PathBuf::from("/valid"),
            PathBuf::from("/valid")
        )
        .is_err()
    );
    assert!(!format!("{:?}", profile()).contains("first"));
}

struct Inspection {
    profile: RegistrationProfile,
    head: RegistrationHeadV2,
    storage_calls: AtomicUsize,
    account: AccountBinding,
    storage_failure: Option<NativeFailure>,
}
impl Inspection {
    fn new(state: DesiredServerState) -> Self {
        let profile = profile();
        Self {
            head: head(&profile, state),
            profile,
            storage_calls: AtomicUsize::new(0),
            account: account(),
            storage_failure: None,
        }
    }
}
impl StartupInspection for Inspection {
    fn read_snapshot(&self, _: AccountBuildKey) -> Result<StartupSnapshot, NativeFailure> {
        Ok(StartupSnapshot {
            head: self.head.clone(),
            profile: self.profile.clone(),
            journal: None,
        })
    }
    fn observe_account(&self, _: AccountBuildKey) -> Result<AccountBinding, NativeFailure> {
        Ok(self.account.clone())
    }
    fn check_storage(&self, _: &StartupSnapshot) -> Result<(), NativeFailure> {
        self.storage_calls.fetch_add(1, Ordering::SeqCst);
        self.storage_failure.map_or(Ok(()), Err)
    }
}
#[test]
fn stopped_preflight_does_not_inspect_storage_or_select_installation() {
    let native = Inspection::new(DesiredServerState::Stopped);
    assert_eq!(
        inspect_startup(key(), native.head.generation, &native),
        Ok(StartupPreflight::Stopped)
    );
    assert_eq!(native.storage_calls.load(Ordering::SeqCst), 0);
}
#[test]
fn startup_never_selects_staged_installation() {
    let native = Inspection::new(DesiredServerState::Running);
    assert_eq!(
        inspect_startup(key(), native.head.generation, &native),
        Ok(StartupPreflight::RetainApproved(
            native.head.approved_installation
        ))
    );
    assert_ne!(
        native.head.staged_installation,
        Some(native.head.approved_installation)
    );
}
#[test]
fn locked_or_unmounted_storage_waits_without_replacement_identity() {
    for failure in [
        NativeFailure::StorageLocked,
        NativeFailure::StorageUnavailable,
    ] {
        let mut native = Inspection::new(DesiredServerState::Running);
        native.storage_failure = Some(failure);
        assert_eq!(
            inspect_startup(key(), native.head.generation, &native),
            Ok(StartupPreflight::WaitingStorage(failure))
        );
        assert_eq!(native.storage_calls.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn stopped_still_revalidates_native_account_and_registration_generation() {
    let mut native = Inspection::new(DesiredServerState::Stopped);
    native.account =
        AccountBinding::new(key(), 20, NativeAccountId::macos([9; 16]).unwrap()).unwrap();
    assert_eq!(
        inspect_startup(key(), native.head.generation, &native),
        Err(NativeFailure::IdentityMismatch)
    );
    assert_eq!(
        inspect_startup(key(), NonZeroU64::new(8).unwrap(), &native),
        Err(NativeFailure::GenerationMismatch)
    );
    assert_eq!(native.storage_calls.load(Ordering::SeqCst), 0);
}
#[test]
fn missing_native_account_is_unavailable_not_passwd_identity() {
    assert_eq!(
        SystemAccountProvider.observe(AccountBuildKey {
            account_uid: u32::MAX - 1,
            ..key()
        }),
        Err(NativeFailure::AccountIdentityUnavailable)
    );
}

#[test]
fn immutable_profile_preserves_exact_strict_json_bytes_for_head_authentication() {
    let p = profile();
    let text = String::from_utf8(p.encode().unwrap()).unwrap();
    let noncanonical = format!(" {text}\n");
    let retained = RegistrationProfile::decode(noncanonical.as_bytes(), key()).unwrap();
    assert_eq!(retained.encode().unwrap(), noncanonical.as_bytes());
    assert_eq!(retained.roots(), p.roots());
}

#[test]
fn roots_round_trip_non_utf8_unix_pathnames_without_debug_contents() {
    use std::os::unix::ffi::OsStrExt;
    let non_utf8 = PathBuf::from(std::ffi::OsStr::from_bytes(b"/first/\xff"));
    let roots = RegisteredRoots::new(
        non_utf8.clone(),
        "/first/data".into(),
        "/first/state".into(),
        "/first/cache".into(),
    )
    .unwrap();
    let boot = BootCredentialBinding::macos([2; 32], [3; 32], account().sha256(), [4; 16]).unwrap();
    let p = RegistrationProfile::new(
        key(),
        20,
        [1; 16],
        [4; 16],
        roots,
        boot,
        StorageBinding::new(vec![5; 16]).unwrap(),
    )
    .unwrap();
    let retained = RegistrationProfile::decode(&p.encode().unwrap(), key()).unwrap();
    assert_eq!(retained.roots().config(), non_utf8);
    assert_eq!(format!("{:?}", retained.roots()), "RegisteredRoots(..)");
}

#[test]
fn registered_storage_retry_preserves_identity_roots_and_data_until_unlock() {
    use spaceterm_host::startup::RegisteredStorageRetry;
    let mut native = Inspection::new(DesiredServerState::Running);
    native.storage_failure = Some(NativeFailure::StorageLocked);
    let original_profile = native.profile.encode().unwrap();
    let original_head = native.head.clone();
    let retry = RegisteredStorageRetry::begin(key(), native.head.generation, &native).unwrap();
    assert_eq!(
        retry.retry(&native),
        Ok(StartupPreflight::WaitingStorage(
            NativeFailure::StorageLocked
        ))
    );
    native.storage_failure = Some(NativeFailure::StorageUnavailable);
    assert_eq!(
        retry.retry(&native),
        Ok(StartupPreflight::WaitingStorage(
            NativeFailure::StorageUnavailable
        ))
    );
    native.storage_failure = None;
    assert_eq!(
        retry.retry(&native),
        Ok(StartupPreflight::RetainApproved(
            original_head.approved_installation
        ))
    );
    assert!(native.profile.encode().unwrap() == original_profile);
    assert_eq!(native.head, original_head);
    assert_eq!(native.storage_calls.load(Ordering::SeqCst), 4);
    assert_eq!(format!("{retry:?}"), "RegisteredStorageRetry(..)");
}
#[test]
fn storage_retry_rejects_replacement_registration_and_respects_a_new_stop() {
    use spaceterm_host::startup::RegisteredStorageRetry;
    for fault in 0..4 {
        let mut native = Inspection::new(DesiredServerState::Running);
        native.storage_failure = Some(NativeFailure::StorageLocked);
        let retry = RegisteredStorageRetry::begin(key(), native.head.generation, &native).unwrap();
        match fault {
            0 => native.head.server_id = [9; 16],
            1 => native.head.approved_data_generation = DataGenerationId::new(2).unwrap(),
            2 => {
                let roots = RegisteredRoots::new(
                    "/replacement/config".into(),
                    "/replacement/data".into(),
                    "/replacement/state".into(),
                    "/replacement/cache".into(),
                )
                .unwrap();
                native.profile = RegistrationProfile::new(
                    key(),
                    20,
                    [1; 16],
                    [4; 16],
                    roots,
                    native.profile.boot_credential().clone(),
                    native.profile.storage().clone(),
                )
                .unwrap();
                native.head.profile_sha256 =
                    Sha256::digest(native.profile.encode().unwrap()).into();
            }
            _ => {
                native.head.generation = NonZeroU64::new(8).unwrap();
                native.head.lifecycle_generation = NonZeroU64::new(3).unwrap();
                native.head.desired_state = DesiredServerState::Stopped;
            }
        }
        let outcome = retry.retry(&native);
        if fault == 3 {
            assert_eq!(outcome, Ok(StartupPreflight::Stopped));
        } else {
            assert!(outcome.is_err());
        }
        assert_eq!(native.storage_calls.load(Ordering::SeqCst), 1);
    }
}

struct WaitingAuthority(std::sync::Mutex<Inspection>);
impl spaceterm_host::startup::NativeStartupAuthority for WaitingAuthority {
    fn admit(
        &self,
        key: AccountBuildKey,
        generation: NonZeroU64,
    ) -> Result<spaceterm_host::startup::NativeStartupAdmission, NativeFailure> {
        Ok(
            spaceterm_host::startup::NativeStartupAdmission::WaitingStorage(
                spaceterm_host::startup::UnprivilegedWaitPlan::begin(
                    key,
                    generation,
                    &*self.0.lock().unwrap(),
                )?,
            ),
        )
    }
    fn authorize_release(
        &self,
        _: &spaceterm_host::startup::PendingStartup,
        _: &spaceterm_host::startup::PostExecProof,
    ) -> Result<spaceterm_host::boot_credentials::CredentialReleasePermit, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
    fn recheck_release(
        &self,
        _: &spaceterm_host::boot_credentials::CredentialReleasePermit,
    ) -> Result<(), NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
}
struct WaitingDriver<'a> {
    authority: &'a WaitingAuthority,
    case: u8,
    waits: usize,
    resumptions: usize,
}
impl spaceterm_host::startup::OneShotLauncher for WaitingDriver<'_> {
    fn wait_for_storage(
        &mut self,
        plan: spaceterm_host::startup::UnprivilegedWaitPlan,
        _: &dyn spaceterm_host::startup::NativeStartupAuthority,
        _: &dyn spaceterm_host::boot_credentials::BootCredentialAdapter,
    ) -> Result<(), NativeFailure> {
        self.waits += 1;
        let mut native = self.authority.0.lock().unwrap();
        assert_eq!(plan.profile().roots(), profile().roots());
        assert_eq!(
            plan.head().approved_data_generation,
            DataGenerationId::new(1).unwrap()
        );
        assert_eq!(
            plan.retry(&*native),
            Ok(StartupPreflight::WaitingStorage(
                NativeFailure::StorageLocked
            ))
        );
        match self.case {
            0 => native.storage_failure = None,
            1 => native.storage_failure = Some(NativeFailure::StorageUnavailable),
            2 => {
                native.head.generation = NonZeroU64::new(8).unwrap();
                native.head.lifecycle_generation = NonZeroU64::new(3).unwrap();
                native.head.desired_state = DesiredServerState::Stopped;
            }
            _ => {
                native.head.approved_data_generation = DataGenerationId::new(2).unwrap();
                native.head.data_high_water = 2;
            }
        }
        let calls = native.storage_calls.load(Ordering::SeqCst);
        match plan.retry(&*native)? {
            StartupPreflight::Stopped => {
                assert_eq!(native.storage_calls.load(Ordering::SeqCst), calls);
                Ok(())
            }
            StartupPreflight::WaitingStorage(failure) => Err(failure),
            StartupPreflight::RetainApproved(installation) => {
                assert_eq!(installation, plan.head().approved_installation);
                self.resumptions += 1;
                // This injected driver stops at the protected reentry boundary. It cannot
                // fabricate PendingStartup or claim native exec/credential evidence.
                Err(NativeFailure::NativeUnavailable)
            }
        }
    }
    fn launch(
        &mut self,
        _: spaceterm_host::startup::PendingStartup,
        _: &dyn spaceterm_host::startup::NativeStartupAuthority,
        _: &dyn spaceterm_host::boot_credentials::BootCredentialAdapter,
    ) -> Result<std::convert::Infallible, NativeFailure> {
        panic!("storage waiting cannot directly enter the credential launcher")
    }
}
#[test]
fn launcher_hands_original_storage_wait_to_the_unprivileged_driver() {
    for (case, expected) in [
        (0, Err(NativeFailure::NativeUnavailable)),
        (1, Err(NativeFailure::StorageUnavailable)),
        (2, Ok(())),
        (3, Err(NativeFailure::GenerationMismatch)),
    ] {
        let mut native = Inspection::new(DesiredServerState::Running);
        native.storage_failure = Some(NativeFailure::StorageLocked);
        let generation = native.head.generation;
        let authority = WaitingAuthority(std::sync::Mutex::new(native));
        let mut driver = WaitingDriver {
            authority: &authority,
            case,
            waits: 0,
            resumptions: 0,
        };
        let result = spaceterm_host::startup::launch_once(
            key(),
            generation,
            &authority,
            &spaceterm_host::boot_credentials::UnavailableBootCredentials,
            &mut driver,
        );
        assert_eq!(result, expected);
        assert_eq!(driver.waits, 1);
        assert_eq!(driver.resumptions, usize::from(case == 0));
    }
}
