//! Bounded native evidence emission, owned by native-evidence E2.

use std::convert::Infallible;

use crate::native_deployment::contracts::NativeFailure;

/// A closed native observation record. Resource identities stay in protected receipts.
pub struct NativeEvidenceRecord {
    _unavailable: Infallible,
}

impl std::fmt::Debug for NativeEvidenceRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeEvidenceRecord(..)")
    }
}

/// Bounded JSONL output from observed native effects and cleanup, never preflight authority.
pub trait NativeEvidenceEmitter: Send + Sync {
    /// Emits the closed case's actual observations without secrets, paths, or native stderr.
    fn emit(&self, record: &NativeEvidenceRecord) -> Result<(), NativeFailure>;
}

/// Unavailable emitter until E2 supplies native observations and strict schema validation.
#[derive(Default)]
pub struct UnavailableNativeEvidenceEmitter;

impl NativeEvidenceEmitter for UnavailableNativeEvidenceEmitter {
    fn emit(&self, _record: &NativeEvidenceRecord) -> Result<(), NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
}
