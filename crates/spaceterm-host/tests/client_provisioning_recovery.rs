#![cfg(all(unix, feature = "test-support"))]

use spaceterm_host::{
    account_service::{ClientProvisioning, ServiceFailure, test_support},
    startup::BuildIdentity,
};

#[derive(Clone, Copy)]
enum Metadata {
    Owner(&'static str),
    Receiver(&'static str),
}
impl Metadata {
    fn read(self, admission: &ClientProvisioning) -> Option<Vec<u8>> {
        match self {
            Self::Owner(name) => admission.owner_attempt(name).unwrap(),
            Self::Receiver(name) => admission.provisioning_attempt(name).unwrap(),
        }
    }
    fn publish(
        self,
        admission: &ClientProvisioning,
        expected: Option<&[u8]>,
        bytes: &[u8],
    ) -> Result<(), ServiceFailure> {
        match self {
            Self::Owner(name) => admission.retain_owner_attempt(name, expected, bytes),
            Self::Receiver(name) => admission.retain_provisioning_attempt(name, expected, bytes),
        }
    }
}

#[test]
fn deliberate_provisioning_recovers_only_original_metadata_across_the_shared_directory() {
    let names = [
        Metadata::Owner("enrollment"),
        Metadata::Owner("desktop"),
        Metadata::Owner("recovery"),
        Metadata::Owner("enrollment-preparation"),
        Metadata::Owner("desktop-preparation"),
        Metadata::Owner("public-claims"),
        Metadata::Receiver("cli"),
        Metadata::Receiver("companion"),
        Metadata::Receiver("management"),
    ];
    let base = br#"{"original":1}"#;
    let proposed = br#"{"original":2}"#;
    for (target, metadata) in names.iter().copied().enumerate() {
        for (unsynced, replacing) in [(false, false), (false, true), (true, false), (true, true)] {
            let root = tempfile::Builder::new()
                .prefix("st-provisioning-recovery-")
                .tempdir_in("/var/tmp")
                .unwrap();
            let account =
                test_support::AccountServiceFixture::new(root.path(), BuildIdentity::Development)
                    .unwrap();
            let supervisor = account.reopen();
            let admission = supervisor.admit_client_provisioning().unwrap();
            for (index, name) in names.iter().copied().enumerate() {
                if index != target || replacing {
                    name.publish(&admission, None, base).unwrap();
                }
            }
            if unsynced {
                test_support::fail_next_publication_durability();
            } else {
                test_support::fail_next_publication_before_effect();
            }
            assert_eq!(
                metadata.publish(&admission, replacing.then_some(base.as_slice()), proposed),
                Err(ServiceFailure::Indeterminate)
            );
            drop(admission);
            // Ordinary recovery intent reads cannot repair or bypass the fence.
            assert_eq!(
                account.reopen().retained_account_recovery(),
                Err(ServiceFailure::Indeterminate)
            );
            let recovered = account.reopen().admit_client_provisioning().unwrap();
            for (index, name) in names.iter().copied().enumerate() {
                assert_eq!(
                    name.read(&recovered).unwrap(),
                    if index == target {
                        proposed.as_slice()
                    } else {
                        base.as_slice()
                    }
                );
            }
            // Recovery retained the original proposal, not a caller replacement.
            assert_eq!(
                metadata.publish(&recovered, replacing.then_some(base.as_slice()), base),
                Err(ServiceFailure::RequestConflict)
            );
            assert_eq!(metadata.read(&recovered).unwrap(), proposed);
        }
    }
}
