//! Production native desktop owners. All routes use the pinned authenticated actor.
//! This factory reads retained bindings. It cannot install, Start, enroll or replace keys.

pub(super) mod protocol;

use super::*;
use crate::platform::{app_paths::AppPaths, client_view_store::NativeViewStore};
use spaceterm_client::{
    api::ConnectionTarget,
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalLimits, ReceiptVerifier},
    pairing::{
        credentials::NativeDeviceCredentials,
        journal::{JournalPairingStore, LocalPairingStore},
    },
    ports::{ClientFuture, ConnectionPurpose, CredentialSource, SigningAccess},
    projection::ProjectionBudgets,
};
use spaceterm_client_native::{NativeClientProvider, credentials::adapter::DeviceCredentials};
use spaceterm_core::pairing::{PairingFailure, SignedPairingReceipt};
use spaceterm_protocol::{authentication::ServerPin, pairing_adapter::ReceiptCodec};
use std::sync::atomic::AtomicBool;

pub(crate) fn production(
    paths: Arc<AppPaths>,
) -> Result<Rc<dyn DesktopClientStartup>, crate::desktop_profile::DesktopProfileError> {
    let executor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| crate::desktop_profile::DesktopProfileError::InvalidCombination)?;
    Ok(Rc::new(NativeDesktopStartup {
        owners: Arc::new(tokio::sync::Mutex::new(None)),
        factory: Some(Arc::new(Factory {
            paths: paths.clone(),
        })),
        store: SharedViewStore(Arc::new(std::sync::Mutex::new(NativeViewStore::new(paths)))),
        consumed: Arc::new(AtomicBool::new(false)),
        executor: Arc::new(executor),
    }))
}

struct Factory {
    paths: Arc<AppPaths>,
}
impl NativeOwnerFactory for Factory {
    fn recover(&self) -> ClientFuture<'_, NativeDesktopOwners> {
        Box::pin(async {
            use crate::platform::{
                app_directories::AppDirectoryRoot,
                client_journal::{self, JournalOpenMode},
            };
            let paths = self.paths.clone();
            let inspect = paths.clone();
            let existing = tokio::task::spawn_blocking(move || {
                inspect
                    .filesystem()
                    .open_private_directory(
                        &inspect
                            .directories()
                            .root(AppDirectoryRoot::State)
                            .join("client-connections")
                            .join("desktop"),
                    )
                    .map(|directory| directory.is_some())
                    .map_err(|_| fail(ClientFailureClass::StorageUnavailable))
            })
            .await
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))??;
            let journal = client_journal::open(
                &paths,
                if existing {
                    JournalOpenMode::Existing
                } else {
                    JournalOpenMode::Initialize
                },
                Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await?;
            let native = tokio::task::spawn_blocking(NativeClientProvider::current)
                .await
                .map_err(|_| fail(ClientFailureClass::Unavailable))?
                .map_err(|_| fail(ClientFailureClass::Unavailable))?;
            let provider = native.clone();
            let discovery_journal = journal.clone();
            let local = tokio::task::spawn_blocking(move || {
                crate::platform::local_server_client::discover(&provider, &discovery_journal)
            })
            .await
            .map_err(|_| fail(ClientFailureClass::Unavailable))??;
            let local_server = local.as_ref().map(|local| {
                spaceterm_core::identity::ServerId::new(local.registration.server_pin().identity)
            });
            let routes = crate::platform::server_client_route::connector(
                local.and_then(|local| local.route).into_iter().collect(),
                None,
            )?;
            let tracker = OperationTracker::new(journal.clone());
            let protocol = Arc::new(protocol::NativeProtocol::new(
                journal.clone(),
                tracker.clone(),
            ));
            let channels = Arc::new(PaneChannels::new(
                protocol.clone(),
                ProjectionBudgets::supported()?,
            )?);
            protocol.set_channels(channels.clone())?;
            Ok(NativeDesktopOwners {
                services: ClientServices {
                    routes,
                    credentials: Arc::new(Credentials {
                        store: JournalPairingStore::new(journal.clone()),
                        credentials: DeviceCredentials::native(),
                    }),
                    journal: journal.clone(),
                    organization: Arc::new(protocol::Decoder),
                    delegate: protocol.clone(),
                    discovery: Some(crate::platform::local_server_client::trusted_discovery(
                        native,
                    )),
                    limits: spaceterm_client::limits::ClientLimits::default(),
                    additional_capabilities: vec![
                        spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
                        spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
                    ],
                },
                journal,
                channels,
                tracker: Some(tracker),
                operations: protocol.clone(),
                invalidator: protocol.clone(),
                local_server,
                registry_binding: Some(protocol),
            })
        })
    }
}

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        let claims = ReceiptCodec::new(pin)
            .verify(receipt)
            .map_err(|_| fail(ClientFailureClass::InvalidStorage))?;
        if claims != receipt.parts().claims {
            return Err(fail(ClientFailureClass::InvalidStorage));
        }
        Ok(())
    }
}

struct Credentials {
    store: JournalPairingStore<JournalController>,
    credentials: DeviceCredentials,
}
impl CredentialSource for Credentials {
    fn signing_access<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess> {
        Box::pin(async move {
            if purpose != ConnectionPurpose::Ordinary {
                return Err(fail(ClientFailureClass::AuthorizationDenied));
            }
            let key = target.local_pairing()?;
            let snapshot = self.store.recover().await.map_err(pairing_failure)?;
            let saved = snapshot
                .active(&key)
                .ok_or_else(|| fail(ClientFailureClass::AuthorizationDenied))?;
            if saved.pin() != target.pin() || saved.client().parts().client.id != target.client() {
                return Err(fail(ClientFailureClass::PinMismatch));
            }
            let authority = self
                .store
                .ordinary_authority(&key)
                .await
                .map_err(pairing_failure)?;
            let signer = self
                .credentials
                .signing_key(saved.credential(), &authority.lease())
                .await
                .map_err(pairing_failure)?;
            Ok(SigningAccess(signer))
        })
    }
}
fn fail(class: ClientFailureClass) -> ClientFailure {
    ClientFailure::new(class)
}
fn pairing_failure(failure: PairingFailure) -> ClientFailure {
    fail(match failure {
        PairingFailure::CredentialUnavailable => ClientFailureClass::StorageUnavailable,
        PairingFailure::PairingRevoked => ClientFailureClass::AuthorizationDenied,
        PairingFailure::AuthorizationDenied => ClientFailureClass::AuthorizationDenied,
        _ => ClientFailureClass::InvalidStorage,
    })
}
