use super::enrollment::{enroll_for_recovery_test as enroll_using, journal_for_recovery_test};
use spaceterm_client::{
    failure::ClientFailureClass,
    pairing::credentials::{
        CredentialCreationReservation, CredentialDeletion, CredentialPurpose, CredentialScope,
        CredentialUse, NativeDeviceCredentials, PairingFuture, PreparedCredential,
    },
    ports::ClientFuture,
    testing::DeterministicKey,
};
use spaceterm_client_native::{NativeClientProvider, native::owner_channel::OwnerChannelProvider};
use spaceterm_core::{identity::ServerId, pairing::PairingFailure};
use spaceterm_host::{
    account_service::{RegisteredServer, test_support::AccountServiceFixture},
    startup::{BuildIdentity, DesiredServerState},
    transport::{MessageTransport, PeerFacts, TransportError, TransportFuture, TransportRoute},
};
use spaceterm_protocol::authentication::SigningKey;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Keys(Arc<Mutex<Vec<PreparedCredential>>>);
impl Keys {
    fn key(scope: &CredentialScope) -> Arc<DeterministicKey> {
        Arc::new(
            DeterministicKey::new(match scope.parts().purpose {
                CredentialPurpose::DeviceIdentity => 31,
                CredentialPurpose::PairingAuthentication { .. } => 32,
            })
            .unwrap(),
        )
    }
}
impl NativeDeviceCredentials for Keys {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let scope = reservation.scope().clone();
            let prepared = PreparedCredential {
                public_key: Self::key(&scope).public_key(),
                scope,
            };
            self.0.lock().unwrap().push(prepared.clone());
            Ok(prepared)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|prepared| &prepared.scope == scope)
                .cloned())
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            Ok(Self::key(&credential.scope) as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        panic!("owner recovery must not delete native credentials")
    }
}
#[derive(Default)]
struct LostReply(Mutex<Vec<Vec<u8>>>);
impl OwnerChannelProvider for Arc<LostReply> {
    fn open<'a>(&'a self, _: &'a RegisteredServer) -> ClientFuture<'a, Arc<dyn MessageTransport>> {
        Box::pin(async { Ok(self.clone() as Arc<dyn MessageTransport>) })
    }
}
impl MessageTransport for LostReply {
    fn peer_facts(&self) -> PeerFacts {
        PeerFacts {
            route: TransportRoute::PrivateIpc,
            local_uid: Some(501),
            local_pid: None,
            pinned_server_key: None,
        }
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().push(bytes.to_vec());
            Ok(())
        })
    }
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Err(TransportError::Unavailable) })
    }
    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn desktop_owner_enrollment_retries_original_native_request_after_reply_loss() {
    let root = crate::platform::local_server_client::tests::Root::new();
    let account = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let registration = account.initialize(DesiredServerState::Stopped).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let journal = runtime.block_on(journal_for_recovery_test(&root));
    let keys = Keys::default();
    let channel = Arc::new(LostReply::default());
    let native = NativeClientProvider::from_supervisor(account.reopen());
    let first = runtime.block_on(enroll_using(
        &native,
        journal.clone(),
        keys.clone(),
        Arc::new(channel.clone()),
    ));
    assert_eq!(first.unwrap_err().class(), ClientFailureClass::Unavailable);
    let route_before = journal
        .snapshot()
        .unwrap()
        .pairing()
        .slots()
        .iter()
        .find(|slot| slot.server == Some(ServerId::new(registration.server_pin().identity)))
        .unwrap()
        .route_reference;
    assert!(
        crate::platform::local_server_client::setup_offer(native, journal.clone(), None)
            .unwrap()
            .is_some()
    );
    let native = NativeClientProvider::from_supervisor(account.reopen());
    let second = runtime.block_on(enroll_using(
        &native,
        journal.clone(),
        keys.clone(),
        Arc::new(channel.clone()),
    ));
    assert_eq!(second.unwrap_err().class(), ClientFailureClass::Unavailable);
    let attempts = channel.0.lock().unwrap();
    assert_eq!(
        attempts.len(),
        2,
        "recovery must repeat the original owner exchange"
    );
    let original: serde_json::Value = serde_json::from_slice(&attempts[0]).unwrap();
    let recovered: serde_json::Value = serde_json::from_slice(&attempts[1]).unwrap();
    assert_eq!(original["request"], recovered["request"]);
    assert_eq!(original["proposal"], recovered["proposal"]);
    assert_eq!(original["device_proof"], recovered["device_proof"]);
    assert_eq!(
        original["authentication_proof"],
        recovered["authentication_proof"]
    );
    assert_eq!(
        keys.0.lock().unwrap().len(),
        2,
        "recovery must reuse both original keys"
    );
    assert_eq!(
        journal
            .snapshot()
            .unwrap()
            .pairing()
            .slots()
            .iter()
            .find(|slot| slot.server == Some(ServerId::new(registration.server_pin().identity)))
            .unwrap()
            .route_reference,
        route_before
    );
    assert!(route_before.is_some());
    assert_eq!(
        account
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
}
