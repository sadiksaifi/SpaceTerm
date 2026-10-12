//! Coordinator-owned source gate. Only reviewed native evidence can change this declaration.

use super::contracts::NativeFailure;

/// Production native admission is closed until the exact candidate's required evidence passes.
/// This source constant is not selected by a feature, environment variable, or runtime setting.
pub const PRODUCTION_NATIVE_ADMISSION: Result<(), NativeFailure> =
    Err(NativeFailure::CustodyUnavailable);

/// Checks the reviewed source gate before any production authority or startup effect.
pub fn require_native_evidence() -> Result<(), NativeFailure> {
    PRODUCTION_NATIVE_ADMISSION
}
