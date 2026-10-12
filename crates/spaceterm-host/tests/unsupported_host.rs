#![cfg(not(any(target_os = "macos", target_os = "linux")))]

use std::cell::Cell;
use std::fs::File;
use std::num::NonZeroU64;
use std::path::Path;

use spaceterm_host::protected_directory::{
    NativeDirectoryProtection, ProtectedDirectoryError, RetainedProtectedDirectory,
};
use spaceterm_host::startup::{
    AccountBuildKey, BuildIdentity, InstallationId, NativeStartupRegistration,
    ProtectedRegistration, StartupError, admit_startup,
};

struct ProtectionFixture {
    inspections: Cell<usize>,
}

impl NativeDirectoryProtection for ProtectionFixture {
    fn verify_directory(&self, _directory: &File) -> Result<(), ProtectedDirectoryError> {
        self.inspections.set(self.inspections.get() + 1);
        Ok(())
    }

    fn verify_regular_file(&self, _file: &File) -> Result<(), ProtectedDirectoryError> {
        self.inspections.set(self.inspections.get() + 1);
        Ok(())
    }
}

struct RegistrationFixture {
    reads: Cell<usize>,
    retentions: Cell<usize>,
}

impl NativeStartupRegistration for RegistrationFixture {
    type RetainedInstallation = ();

    fn read_fixed_registration(
        &self,
        _key: AccountBuildKey,
    ) -> Result<ProtectedRegistration, StartupError> {
        self.reads.set(self.reads.get() + 1);
        Err(StartupError::RegistrationMissing)
    }

    fn retain_approved_installation(
        &self,
        _key: AccountBuildKey,
        _generation: NonZeroU64,
        _installation: InstallationId,
    ) -> Result<Self::RetainedInstallation, StartupError> {
        self.retentions.set(self.retentions.get() + 1);
        Ok(())
    }
}

#[test]
fn unsupported_hosts_do_not_issue_directory_authority_or_inspect_native_handles() {
    let protection = ProtectionFixture {
        inspections: Cell::new(0),
    };

    let opened = RetainedProtectedDirectory::open(Path::new("/"), &protection);

    assert_eq!(
        opened.err(),
        Some(ProtectedDirectoryError::NativeUnavailable)
    );
    assert_eq!(protection.inspections.get(), 0);
}

#[test]
fn unsupported_hosts_reject_startup_before_accessing_native_registration() {
    let native = RegistrationFixture {
        reads: Cell::new(0),
        retentions: Cell::new(0),
    };
    let key = AccountBuildKey {
        account_uid: 501,
        build_identity: BuildIdentity::Development,
    };
    let generation = NonZeroU64::new(7).expect("fixture generation must be nonzero");

    let admission = admit_startup(key, generation, &native);

    assert_eq!(admission.err(), Some(StartupError::NativeUnavailable));
    assert_eq!(native.reads.get(), 0);
    assert_eq!(native.retentions.get(), 0);
}

#[test]
fn unsupported_hosts_reject_uid_sentinel_before_accessing_native_registration() {
    for build_identity in [
        BuildIdentity::Release,
        BuildIdentity::Preflight,
        BuildIdentity::Development,
    ] {
        let native = RegistrationFixture {
            reads: Cell::new(0),
            retentions: Cell::new(0),
        };
        let key = AccountBuildKey {
            account_uid: u32::MAX,
            build_identity,
        };
        let generation = NonZeroU64::new(7).expect("fixture generation must be nonzero");

        let admission = admit_startup(key, generation, &native);

        assert_eq!(admission.err(), Some(StartupError::UnsafeRegistration));
        assert_eq!(native.reads.get(), 0);
        assert_eq!(native.retentions.get(), 0);
    }
}

#[test]
fn unsupported_hosts_reject_root_account_before_accessing_native_registration() {
    for build_identity in [
        BuildIdentity::Release,
        BuildIdentity::Preflight,
        BuildIdentity::Development,
    ] {
        let native = RegistrationFixture {
            reads: Cell::new(0),
            retentions: Cell::new(0),
        };
        let key = AccountBuildKey {
            account_uid: 0,
            build_identity,
        };
        let generation = NonZeroU64::new(7).expect("fixture generation must be nonzero");

        let admission = admit_startup(key, generation, &native);

        assert_eq!(admission.err(), Some(StartupError::RootAccount));
        assert_eq!(native.reads.get(), 0);
        assert_eq!(native.retentions.get(), 0);
    }
}
