//! Fresh publication and retained execution, owned by native-authority A2.

use std::convert::Infallible;

use super::contracts::{AuthorizedNativeRequest, NativeFailure, PublicationDisposition};
use crate::{
    approved_installation::RetainedApprovedInstallation, installation::RetainedRegistration,
};
use spaceterm_distribution::VerifiedServerArchive;

/// An unpublished candidate retained under native custody. Comparison facts cannot construct it.
pub struct StagedInstallation {
    _unavailable: Infallible,
}

impl std::fmt::Debug for StagedInstallation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StagedInstallation(..)")
    }
}

/// Fresh destination publication and head-selected execution retention.
pub trait InstallationPublisher: Send + Sync {
    /// Publishes fresh protected inodes, authenticating destination bytes before any execution.
    fn publish_fresh(
        &self,
        request: &AuthorizedNativeRequest,
        candidate: &VerifiedServerArchive,
    ) -> PublicationDisposition<StagedInstallation>;

    /// Retains the exact approved executable and its authenticated execution closure.
    fn retain_approved(
        &self,
        registration: &RetainedRegistration,
    ) -> Result<RetainedApprovedInstallation, NativeFailure>;
}

/// Unavailable publisher until A2 supplies fresh-copy custody and destination authentication.
#[derive(Default)]
pub struct UnavailableInstallationPublisher;

impl InstallationPublisher for UnavailableInstallationPublisher {
    fn publish_fresh(
        &self,
        _request: &AuthorizedNativeRequest,
        _candidate: &VerifiedServerArchive,
    ) -> PublicationDisposition<StagedInstallation> {
        PublicationDisposition::NotApplied(NativeFailure::CustodyUnavailable)
    }

    fn retain_approved(
        &self,
        _registration: &RetainedRegistration,
    ) -> Result<RetainedApprovedInstallation, NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
}
