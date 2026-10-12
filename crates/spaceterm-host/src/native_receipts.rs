//! Protected schema-2 receipt storage and retained cleanup bindings, owned by native-evidence E2.

use std::convert::Infallible;

use crate::native_deployment::contracts::{NativeFailure, NativeRequestId, PublicationDisposition};

/// Retained protected receipt authority, not JSON or resource-name comparison facts.
pub struct NativeReceipt {
    _unavailable: Infallible,
}

impl std::fmt::Debug for NativeReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeReceipt(..)")
    }
}

/// Exact native effect identity retained through durable identity publication.
pub struct NativeEffectBinding {
    _unavailable: Infallible,
}

impl std::fmt::Debug for NativeEffectBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeEffectBinding(..)")
    }
}

/// Exact native object and prior-process scope retained through deletion.
pub struct NativeCleanupBinding {
    _unavailable: Infallible,
}

impl std::fmt::Debug for NativeCleanupBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeCleanupBinding(..)")
    }
}

/// One evidence receipt authority shared by the administrator, emitters, and recovery.
pub trait NativeReceiptStore: Send + Sync {
    /// Durably publishes the protected receipt before exposing any fixture resource.
    fn publish(&self, request: NativeRequestId) -> PublicationDisposition<NativeReceipt>;
    /// Loads the exact retained receipt without accepting a caller-selected path.
    fn retain(&self, request: NativeRequestId) -> Result<NativeReceipt, NativeFailure>;
    /// Durably records the retained native identity before the next installation effect.
    fn record_identity(
        &self,
        receipt: &mut NativeReceipt,
        identity: NativeEffectBinding,
    ) -> PublicationDisposition<()>;
    /// Retains exact objects and the prior process scope for receipt-bound cleanup.
    fn retain_cleanup(
        &self,
        receipt: &NativeReceipt,
    ) -> Result<NativeCleanupBinding, NativeFailure>;
}

/// Unavailable receipt store until E2 supplies protected durable native identity retention.
#[derive(Default)]
pub struct UnavailableNativeReceiptStore;

impl NativeReceiptStore for UnavailableNativeReceiptStore {
    fn publish(&self, _request: NativeRequestId) -> PublicationDisposition<NativeReceipt> {
        PublicationDisposition::NotApplied(NativeFailure::NativeUnavailable)
    }
    fn retain(&self, _request: NativeRequestId) -> Result<NativeReceipt, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
    fn record_identity(
        &self,
        _receipt: &mut NativeReceipt,
        _identity: NativeEffectBinding,
    ) -> PublicationDisposition<()> {
        PublicationDisposition::NotApplied(NativeFailure::NativeUnavailable)
    }
    fn retain_cleanup(
        &self,
        _receipt: &NativeReceipt,
    ) -> Result<NativeCleanupBinding, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
}
