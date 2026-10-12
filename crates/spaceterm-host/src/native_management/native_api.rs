//! Optional linked native boundary. Missing coordinator linkage is a typed refusal.

use crate::native_deployment::contracts::NativeFailure;
use std::ffi::CStr;

/// Resolves only a statically named function in the already-admitted helper closure.
/// This opens no library or pathname and never searches an account-provided plugin.
///
/// # Safety
/// `T` must be the exact native function-pointer ABI for `name`. The admitted native
/// object must remain loaded through every returned pointer's use.
pub(crate) unsafe fn linked_function<T: Copy>(name: &CStr) -> Result<T, NativeFailure> {
    if std::mem::size_of::<T>() != std::mem::size_of::<*mut libc::c_void>() {
        return Err(NativeFailure::NativeUnavailable);
    }
    // SAFETY: the name is a static terminated native symbol, and no library is opened.
    let function = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    if function.is_null() {
        return Err(NativeFailure::NativeUnavailable);
    }
    // SAFETY: the caller supplies the exact function ABI and the size was checked above.
    Ok(unsafe { std::mem::transmute_copy(&function) })
}
