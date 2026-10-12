//! Shared deployment transaction and release authority, owned by native-authority A1 and A3.

use std::convert::Infallible;

use super::contracts::{AuthorizedNativeRequest, NativeFailure};

/// Noncloneable lease on the installation owner's single protected transaction authority.
/// It cannot be decoded or constructed from a management request or registration facts.
pub struct NativeTransactionLease {
    _unavailable: Infallible,
}

impl std::fmt::Debug for NativeTransactionLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeTransactionLease(..)")
    }
}

/// Serialization shared by publication, lifecycle, repair, collection, and credential release.
/// Implementations retain native lock descriptors and the existing protected journal.
pub trait NativeTransactionAuthority: Send + Sync {
    /// Acquires bounded transaction authority for the OS-authenticated original request.
    fn begin(
        &self,
        request: &AuthorizedNativeRequest,
    ) -> Result<NativeTransactionLease, NativeFailure>;

    /// Revalidates retained authority without treating a later sync as proof of an earlier write.
    fn revalidate(&self, lease: &NativeTransactionLease) -> Result<(), NativeFailure>;
}

/// Unavailable serialization port until A3 supplies native locks and retained journal authority.
#[derive(Default)]
pub struct UnavailableNativeTransactionAuthority;

impl NativeTransactionAuthority for UnavailableNativeTransactionAuthority {
    fn begin(
        &self,
        _request: &AuthorizedNativeRequest,
    ) -> Result<NativeTransactionLease, NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }

    fn revalidate(&self, _lease: &NativeTransactionLease) -> Result<(), NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
}

pub(crate) fn pending_linux_custody(
    _pending: &super::contracts::PendingStartup,
) -> Result<&crate::linux_custody::NativeLinuxCustody, NativeFailure> {
    Err(NativeFailure::CustodyUnavailable)
}

pub(crate) fn pending_retained_server(
    _pending: &super::contracts::PendingStartup,
) -> Result<&std::fs::File, NativeFailure> {
    Err(NativeFailure::ExecutionBindingUnproven)
}

pub(crate) fn pending_retained_credential_tool(
    _pending: &super::contracts::PendingStartup,
) -> Result<&std::fs::File, NativeFailure> {
    Err(NativeFailure::ExecutionBindingUnproven)
}

pub(crate) fn release_linux_custody(
    _permit: &super::contracts::CredentialReleasePermit,
) -> Result<&crate::linux_custody::NativeLinuxCustody, NativeFailure> {
    Err(NativeFailure::CustodyUnavailable)
}

pub(crate) fn release_retained_credential_tool(
    _permit: &super::contracts::CredentialReleasePermit,
) -> Result<&std::fs::File, NativeFailure> {
    Err(NativeFailure::ExecutionBindingUnproven)
}
