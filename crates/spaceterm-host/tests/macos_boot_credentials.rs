//! Private composition for the adapter source while ND520/1's common seed is
//! absent at b107b1d. These types are test-only. They do not add fact constructors,
//! credentials, or permit fabrication to the production host crate. Integration
//! must replace this composition with the seeded common types and real handles.

mod native_deployment {
    pub mod contracts {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum NativeFailure {
            NativeUnavailable,
            CredentialPolicyRejected,
        }
        pub struct BootCredentialBinding;
        pub struct CredentialReleasePermit;
    }
}
mod boot_credentials {
    use super::native_deployment::contracts::{
        BootCredentialBinding, CredentialReleasePermit, NativeFailure,
    };

    pub struct CredentialPolicyFacts;
    pub struct LockedSecret32(pub [u8; 32]);
    impl LockedSecret32 {
        pub fn wipe(&mut self) {
            self.0.fill(0);
        }
    }
    pub trait BootCredentialAdapter: Send + Sync {
        fn inspect_policy(
            &self,
            binding: &BootCredentialBinding,
        ) -> Result<CredentialPolicyFacts, NativeFailure>;
        fn retrieve_for_startup(
            &self,
            permit: &CredentialReleasePermit,
            destination: &mut LockedSecret32,
        ) -> Result<(), NativeFailure>;
    }
}
#[path = "../src/boot_credentials/macos.rs"]
mod macos;

use boot_credentials::{BootCredentialAdapter, LockedSecret32};
use macos::{KeychainPolicyObservation, MacosBootCredentialAdapter};
use native_deployment::contracts::{BootCredentialBinding, CredentialReleasePermit, NativeFailure};

fn complete_observation() -> KeychainPolicyObservation {
    KeychainPolicyObservation {
        schema: 1,
        policy_class: 1,
        interaction_class: 1,
        account_binding_matches: 1,
        code_binding_matches: 1,
        auxiliary_acl_class: 1,
    }
}

#[test]
fn complete_foreign_policy_observation_is_comparison_data_only() {
    assert_eq!(complete_observation().validate(), Ok(()));
    let adapter = MacosBootCredentialAdapter;
    assert!(matches!(
        adapter.inspect_policy(&BootCredentialBinding),
        Err(NativeFailure::NativeUnavailable)
    ));
}

#[test]
fn unknown_or_weakened_foreign_policy_fields_are_rejected() {
    for value in [0, 2, u32::MAX] {
        for field in 0..6 {
            let mut observation = complete_observation();
            match field {
                0 => observation.schema = value,
                1 => observation.policy_class = value,
                2 => observation.interaction_class = value,
                3 => observation.account_binding_matches = value,
                4 => observation.code_binding_matches = value,
                5 => observation.auxiliary_acl_class = value,
                _ => unreachable!(),
            }
            assert_eq!(
                observation.validate(),
                Err(NativeFailure::CredentialPolicyRejected)
            );
        }
    }
}

#[test]
fn missing_native_handle_issuer_never_opens_system_keychain() {
    let adapter = MacosBootCredentialAdapter;
    assert!(matches!(
        adapter.inspect_policy(&BootCredentialBinding),
        Err(NativeFailure::NativeUnavailable)
    ));
}

#[test]
fn unavailable_retrieval_wipes_the_owned_destination() {
    let adapter = MacosBootCredentialAdapter;
    let mut destination = LockedSecret32([0xa7; 32]);
    assert_eq!(
        adapter.retrieve_for_startup(&CredentialReleasePermit, &mut destination),
        Err(NativeFailure::NativeUnavailable)
    );
    assert_eq!(destination.0, [0; 32]);
}
