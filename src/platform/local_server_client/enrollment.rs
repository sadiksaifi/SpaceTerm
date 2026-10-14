//! Deliberate desktop owner enrollment delegates to the shared native owner.
//! Ordinary startup restoration never enters this foreground provisioning path.

use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::JournalController,
    pairing::credentials::NativeDeviceCredentials,
};
use spaceterm_client_native::{
    NativeClientProvider,
    credentials::adapter::DeviceCredentials,
    native::{
        consumer::{NativeConsumer, NativeConsumerKind},
        owner_channel::{NativeOwnerChannel, OwnerChannelProvider},
    },
};
use std::sync::Arc;

pub(super) async fn enroll(
    native: &NativeClientProvider,
    journal: Arc<JournalController>,
) -> Result<(), ClientFailure> {
    enroll_with_credentials(native, journal, DeviceCredentials::native()).await
}

pub(crate) async fn enroll_with_credentials<N: NativeDeviceCredentials + Clone + 'static>(
    native: &NativeClientProvider,
    journal: Arc<JournalController>,
    credentials: N,
) -> Result<(), ClientFailure> {
    enroll_using(
        native,
        journal,
        credentials,
        Arc::new(NativeOwnerChannel(native.management())),
    )
    .await
}

async fn enroll_using<N: NativeDeviceCredentials + Clone + 'static>(
    native: &NativeClientProvider,
    journal: Arc<JournalController>,
    credentials: N,
    channels: Arc<dyn OwnerChannelProvider>,
) -> Result<(), ClientFailure> {
    NativeConsumer::new(native.management(), NativeConsumerKind::Cli)
        .with_credentials(Arc::new(credentials))
        .with_owner_channels(channels)
        .enroll_journal(journal)
        .await
        .map_err(classify)
}
fn classify(error: spaceterm_client::management_cli::ManagementFailure) -> ClientFailure {
    use spaceterm_client::management_cli::ManagementFailure;
    ClientFailure::new(match error {
        ManagementFailure::CredentialUnavailable | ManagementFailure::StorageUnavailable => {
            ClientFailureClass::StorageUnavailable
        }
        ManagementFailure::AuthorizationDenied | ManagementFailure::Revoked => {
            ClientFailureClass::AuthorizationDenied
        }
        ManagementFailure::OutcomePending => ClientFailureClass::RecoveryRequired,
        ManagementFailure::UnsafeNativeState => ClientFailureClass::InvalidStorage,
        ManagementFailure::Incompatible => ClientFailureClass::Incompatible,
        ManagementFailure::LimitExceeded => ClientFailureClass::ResourceLimit,
        _ => ClientFailureClass::Unavailable,
    })
}

#[cfg(test)]
pub(super) async fn enroll_for_recovery_test<N: NativeDeviceCredentials + Clone + 'static>(
    native: &NativeClientProvider,
    journal: Arc<JournalController>,
    credentials: N,
    channels: Arc<dyn OwnerChannelProvider>,
) -> Result<(), ClientFailure> {
    enroll_using(native, journal, credentials, channels).await
}

#[cfg(test)]
pub(super) async fn journal_for_recovery_test(
    root: &crate::platform::local_server_client::tests::Root,
) -> Arc<JournalController> {
    tests::journal(root).await
}

#[cfg(test)]
#[path = "enrollment/tests.rs"]
mod tests;
