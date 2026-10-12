//! Protect every primitive context and compiler spill, not only caller arrays.
//!
//! hkdf 0.12.4, hmac 0.12.1 and sha2 0.10.9 use fixed-size, no-heap contexts
//! for this operation. Run them on an explicitly supplied locked pthread stack.
//! No secret is passed to pthread metadata or TLS. Join reserves the stack until
//! the worker has exited, then wipe the whole stack before unlocking it.

use std::mem::{self, size_of};
use std::ptr;

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use super::unix::LockedMemory;
use super::{ACK_INFO, CONFIRM_INFO, ConfirmationMacs, KEY_INFO, NativeFailure, PREFIX_BYTES};

// The linked headless server has 256 KiB of static TLS. glibc reserves TLS
// inside a caller-supplied pthread stack, in addition to the crypto frames.
const STACK_BYTES: usize = 524_288;
const REQUEST_BYTES: usize = 512;

#[repr(C)]
struct Request {
    ready: [u8; PREFIX_BYTES],
    secret: [u8; 32],
    challenge: [u8; 32],
    output: *mut u8,
    stack_start: usize,
    stack_end: usize,
    result: u32,
}
const _: () = assert!(size_of::<Request>() <= REQUEST_BYTES);

unsafe extern "C" {
    fn pthread_attr_setguardsize(attributes: *mut libc::pthread_attr_t, length: usize) -> i32;
    fn pthread_attr_setstack(
        attributes: *mut libc::pthread_attr_t,
        stack: *mut libc::c_void,
        length: usize,
    ) -> i32;
}

pub(super) fn calculate(
    ready: &[u8; PREFIX_BYTES],
    secret: &[u8; 32],
    challenge: &[u8; 32],
) -> Result<ConfirmationMacs, NativeFailure> {
    calculate_using(
        ready,
        secret,
        challenge,
        worker,
        |thread| unsafe { libc::pthread_join(thread, ptr::null_mut()) },
        |stack, request, output| {
            mem::forget(stack);
            mem::forget(request);
            mem::forget(output);
        },
        |_, _, _| {},
    )
}

fn calculate_using(
    ready: &[u8; PREFIX_BYTES],
    secret: &[u8; 32],
    challenge: &[u8; 32],
    callback: extern "C" fn(*mut libc::c_void) -> *mut libc::c_void,
    join: impl FnOnce(libc::pthread_t) -> i32,
    unresolved: impl FnOnce(LockedMemory<STACK_BYTES>, LockedMemory<REQUEST_BYTES>, LockedMemory<64>),
    retired: impl FnOnce(&LockedMemory<STACK_BYTES>, &LockedMemory<REQUEST_BYTES>, &LockedMemory<64>),
) -> Result<ConfirmationMacs, NativeFailure> {
    let mut stack = LockedMemory::<STACK_BYTES>::allocate()?;
    let mut request = LockedMemory::<REQUEST_BYTES>::allocate()?;
    let mut output = LockedMemory::<64>::allocate()?;
    let stack_start = stack.bytes_mut().as_mut_ptr() as usize;
    let request_pointer = request.bytes_mut().as_mut_ptr().cast::<Request>();
    // SAFETY: mmap is page-aligned, zero-initialized and larger than Request.
    // Populate in place; never create a secret-bearing Request on the caller stack.
    let data = unsafe { &mut *request_pointer };
    data.ready.copy_from_slice(ready);
    data.secret.copy_from_slice(secret);
    data.challenge.copy_from_slice(challenge);
    data.output = output.bytes_mut().as_mut_ptr();
    data.stack_start = stack_start;
    data.stack_end = stack_start + STACK_BYTES;
    data.result = NativeFailure::UnsafeNativeState as u32;

    let mut attributes: libc::pthread_attr_t = unsafe { mem::zeroed() };
    if unsafe { libc::pthread_attr_init(&mut attributes) } != 0 {
        return Err(NativeFailure::NativeUnavailable);
    }
    // Caller-provided stacks do not get an implicit guard allocation. The bounded
    // fixed-size computation uses this entire supplied, locked 512 KiB mapping.
    let configured = unsafe {
        pthread_attr_setguardsize(&mut attributes, 0) == 0
            && pthread_attr_setstack(&mut attributes, stack_start as *mut _, STACK_BYTES) == 0
    };
    let mut thread: libc::pthread_t = unsafe { mem::zeroed() };
    let created = configured
        && unsafe {
            libc::pthread_create(&mut thread, &attributes, callback, request_pointer.cast()) == 0
        };
    unsafe { libc::pthread_attr_destroy(&mut attributes) };
    if !created {
        return Err(NativeFailure::NativeUnavailable);
    }
    // SAFETY: this is the sole waiter for the successfully created joinable worker.
    // No secret input remains borrowed from the caller by the worker.
    if join(thread) != 0 {
        // An unresolved worker may still use these mappings. Never unmap its
        // stack or inputs; retain locked resources and refuse exchange completion.
        unresolved(stack, request, output);
        return Err(NativeFailure::CleanupUnresolved);
    }
    let result = unsafe { (*request_pointer).result };
    stack.wipe();
    request.wipe();
    if result != 0 {
        output.wipe();
    }
    retired(&stack, &request, &output);
    if result == 0 {
        Ok(ConfirmationMacs { bytes: output })
    } else {
        Err(NativeFailure::UnsafeNativeState)
    }
}

#[cfg(test)]
pub(super) fn failed_join(
    ready: &[u8; PREFIX_BYTES],
    secret: &[u8; 32],
    challenge: &[u8; 32],
) -> Result<ConfirmationMacs, NativeFailure> {
    calculate_using(
        ready,
        secret,
        challenge,
        worker,
        |thread| {
            // Retire the real worker before injecting the boundary's failure so
            // the test can safely reclaim mappings that production must retain.
            assert_eq!(unsafe { libc::pthread_join(thread, ptr::null_mut()) }, 0);
            libc::EINVAL
        },
        |mut stack, mut request, mut output| {
            for bytes in [
                stack.bytes().as_slice(),
                request.bytes().as_slice(),
                output.bytes().as_slice(),
            ] {
                assert!(super::unix::native_memory_is_locked(bytes));
            }
            stack.wipe();
            request.wipe();
            output.wipe();
        },
        |_, _, _| panic!("failed join cannot publish MACs"),
    )
}

extern "C" fn worker(pointer: *mut libc::c_void) -> *mut libc::c_void {
    // SAFETY: the sole caller owns and retains initialized mappings until join.
    let request = unsafe { &mut *pointer.cast::<Request>() };
    let marker = 0u8;
    let address = ptr::addr_of!(marker) as usize;
    if !(request.stack_start..request.stack_end).contains(&address) {
        return ptr::null_mut();
    }
    let mut delivery = request.ready;
    delivery[5] = 2;
    delivery[8..12].copy_from_slice(&416u32.to_be_bytes());
    delivery[28..32].fill(0);
    let mut digest = Sha256::new();
    digest.update(request.ready);
    digest.update(delivery);
    digest.update(request.challenge);
    let mut transcript: [u8; 32] = digest.finalize().into();
    let mut key = [0; 32];
    let hkdf = Hkdf::<Sha256>::new(Some(&request.ready[240..272]), &request.secret);
    if hkdf.expand(KEY_INFO, &mut key).is_err() {
        return ptr::null_mut();
    }
    let Ok(mut confirm) = Hmac::<Sha256>::new_from_slice(&key) else {
        return ptr::null_mut();
    };
    // This native test observation covers the actual worker stack containing the
    // PRK, key, pads, hash contexts and intermediate arrays. No production switch.
    #[cfg(test)]
    if !super::unix::native_memory_is_locked(&key) {
        return ptr::null_mut();
    }
    confirm.update(CONFIRM_INFO);
    confirm.update(&transcript);
    let mut confirmation: [u8; 32] = confirm.finalize().into_bytes().into();
    let Ok(mut ack) = Hmac::<Sha256>::new_from_slice(&key) else {
        return ptr::null_mut();
    };
    ack.update(ACK_INFO);
    ack.update(&transcript);
    ack.update(&confirmation);
    let mut acknowledgement: [u8; 32] = ack.finalize().into_bytes().into();
    // SAFETY: output names the exclusively owned live locked 64-byte mapping.
    let output = unsafe { std::slice::from_raw_parts_mut(request.output, 64) };
    output[..32].copy_from_slice(&confirmation);
    output[32..].copy_from_slice(&acknowledgement);
    key.zeroize();
    transcript.zeroize();
    confirmation.zeroize();
    acknowledgement.zeroize();
    delivery.zeroize();
    request.result = 0;
    // All primitive contexts, including their non-zeroizing internal PRK and
    // pad arrays, are additionally wiped by the caller's complete stack wipe.
    ptr::null_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn failed_worker(pointer: *mut libc::c_void) -> *mut libc::c_void {
        let _ = worker(pointer);
        // Simulate a native failure after calculation populated every output.
        unsafe { (*pointer.cast::<Request>()).result = NativeFailure::UnsafeNativeState as u32 };
        ptr::null_mut()
    }

    #[test]
    fn failed_calculation_wipes_stack_request_and_outputs_while_locked() {
        let result = calculate_using(
            &[1; PREFIX_BYTES],
            &[7; 32],
            &[11; 32],
            failed_worker,
            |thread| unsafe { libc::pthread_join(thread, ptr::null_mut()) },
            |_, _, _| panic!("unexpected join failure"),
            |stack, request, output| {
                for bytes in [
                    stack.bytes().as_slice(),
                    request.bytes().as_slice(),
                    output.bytes().as_slice(),
                ] {
                    assert!(super::super::unix::native_memory_is_locked(bytes));
                    assert!(bytes.iter().all(|byte| *byte == 0));
                }
            },
        );
        assert!(matches!(result, Err(NativeFailure::UnsafeNativeState)));
    }

    #[test]
    fn successful_calculation_wipes_all_intermediate_context_storage_before_return() {
        let result = calculate_using(
            &[1; PREFIX_BYTES],
            &[7; 32],
            &[11; 32],
            worker,
            |thread| unsafe { libc::pthread_join(thread, ptr::null_mut()) },
            |_, _, _| panic!("unexpected join failure"),
            |stack, request, output| {
                assert!(stack.bytes().iter().all(|byte| *byte == 0));
                assert!(request.bytes().iter().all(|byte| *byte == 0));
                assert!(super::super::unix::native_memory_is_locked(output.bytes()));
                assert!(output.bytes().iter().any(|byte| *byte != 0));
            },
        );
        assert!(result.is_ok());
    }
}
