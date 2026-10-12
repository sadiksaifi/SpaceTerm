//! ABI-2 startup exchange mechanics, separate from native release authority.
//!
//! A recognized transcript is comparison data. Neither declarations nor a valid
//! confirmation grant execution or credential-release authority. The retriever
//! must independently authorize and fence release through the native authority.
//! Production composition remains blocked on native execution binding, release
//! fencing and retriever lifetime.

use std::fmt;
use std::time::{Duration, Instant};

use crate::native_deployment::contracts::NativeFailure;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "startup_exchange/unix.rs"]
pub mod unix;

#[path = "startup_exchange/crypto.rs"]
mod crypto;

/// Exact ABI-2 prefix length.
pub const PREFIX_BYTES: usize = 352;
/// Exact ABI-2 delivery, confirmation and acknowledgement length.
pub const FRAME_BYTES: usize = 416;
/// One absolute exchange budget, including fragmented frames and EOF.
pub const MAX_EXCHANGE_WAIT: Duration = Duration::from_secs(2);
const KEY_INFO: &[u8] = b"SpaceTerm native boot confirmation key v2\0";
const CONFIRM_INFO: &[u8] = b"SpaceTerm native boot confirmation v2\0";
const ACK_INFO: &[u8] = b"SpaceTerm native boot acknowledgement v2\0";

/// An ABI-2 frame purpose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameKind {
    /// The server's post-exec declaration, not native privilege proof.
    Ready = 1,
    /// Credential delivery by the already authorized retriever.
    Delivery = 2,
    /// Transcript-bound proof of receipt.
    Confirmation = 3,
    /// Transcript-bound acknowledgement by the retriever.
    Acknowledgement = 4,
}

/// Canonical startup comparison bytes. This type supplies no native authority.
pub struct StartupTranscript([u8; PREFIX_BYTES]);

impl fmt::Debug for StartupTranscript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StartupTranscript([redacted])")
    }
}

impl Drop for StartupTranscript {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl StartupTranscript {
    /// Recognizes the complete ABI-2 readiness grammar with no trailing bytes.
    pub fn recognize_ready(bytes: &[u8]) -> Result<Self, NativeFailure> {
        if bytes.len() != PREFIX_BYTES {
            return Err(NativeFailure::InvalidRecord);
        }
        let mut value = [0; PREFIX_BYTES];
        value.copy_from_slice(bytes);
        let invalid = &value[..4] != b"STN2"
            || value[4] != 2
            || value[5] != FrameKind::Ready as u8
            || !is_zero(&value[6..8])
            || u32_at(&value, 8) != PREFIX_BYTES as u32
            || !is_zero(&value[12..16])
            || matches!(u32_at(&value, 16), 0 | u32::MAX)
            || u32_at(&value, 20) == u32::MAX
            || !(1..=3).contains(&u32_at(&value, 24))
            || u32_at(&value, 28) != 0x3f
            || [32, 40, 48, 56]
                .iter()
                .any(|&offset| u64_at(&value, offset) == 0)
            || is_zero(&value[64..80])
            || [80, 112, 144, 176, 208, 240]
                .iter()
                .any(|&offset| is_zero(&value[offset..offset + 32]))
            || !valid_process(&value, 272)
            || !valid_process(&value, 296)
            || u64_at(&value, 272) == u64_at(&value, 296)
            || !is_zero(&value[292..296])
            || !is_zero(&value[316..320])
            || !is_zero(&value[336..352]);
        if invalid {
            value.zeroize();
            return Err(NativeFailure::InvalidRecord);
        }
        Ok(Self(value))
    }

    /// Constructs one exact context-bound prefix. Non-Ready declarations are zero.
    pub fn prefix(&self, kind: FrameKind) -> [u8; PREFIX_BYTES] {
        let mut prefix = self.0;
        prefix[5] = kind as u8;
        let size = if kind == FrameKind::Ready {
            PREFIX_BYTES
        } else {
            FRAME_BYTES
        };
        prefix[8..12].copy_from_slice(&(size as u32).to_be_bytes());
        if kind != FrameKind::Ready {
            prefix[28..32].fill(0);
        }
        prefix
    }

    /// Requires every context byte, reserved byte, length and frame purpose to match.
    pub fn recognize_prefix(&self, kind: FrameKind, bytes: &[u8]) -> Result<(), NativeFailure> {
        let mut expected = self.prefix(kind);
        let matches = bytes == expected;
        expected.zeroize();
        if matches {
            Ok(())
        } else {
            Err(NativeFailure::InvalidRecord)
        }
    }

    /// The retriever's process comparison identity. A PID alone supplies no authority.
    pub fn retriever_identity(&self) -> ProcessIdentity {
        ProcessIdentity {
            pid: u64_at(&self.0, 296) as u32,
            start_seconds: u64_at(&self.0, 304),
            start_microseconds: u32_at(&self.0, 312),
        }
    }

    /// Calculates ABI-2 MACs on a separate, wholly locked native stack.
    /// The pinned no-heap primitives' states and spills remain on that stack;
    /// it is wiped after exact join. Inputs and outputs are owned locked mappings.
    /// No wire result or memory observation supplies native release authority.
    pub fn confirmation_macs(
        &self,
        secret: &[u8; 32],
        challenge: &[u8; 32],
    ) -> Result<ConfirmationMacs, NativeFailure> {
        crypto::calculate(&self.0, secret, challenge)
    }
}

/// The shared native process comparison identity. No comparison grants authority.
pub use crate::startup::ProcessIdentity;

/// Owned locked MAC bytes, wiped before unlock, with redacted diagnostics.
pub struct ConfirmationMacs {
    bytes: unix::LockedMemory<64>,
}

impl fmt::Debug for ConfirmationMacs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfirmationMacs([redacted])")
    }
}

impl ConfirmationMacs {
    /// The transcript-bound receipt MAC, not the boot credential.
    pub fn confirmation(&self) -> &[u8; 32] {
        &self.bytes.bytes().as_chunks::<32>().0[0]
    }
    /// The transcript-bound acknowledgement MAC.
    pub fn acknowledgement(&self) -> &[u8; 32] {
        &self.bytes.bytes().as_chunks::<32>().0[1]
    }
    /// Constant-time verification of the receipt MAC.
    pub fn verify_confirmation(&self, value: &[u8]) -> Result<(), NativeFailure> {
        verify_mac(self.confirmation(), value)
    }
    /// Constant-time verification of the acknowledgement MAC.
    pub fn verify_acknowledgement(&self, value: &[u8]) -> Result<(), NativeFailure> {
        verify_mac(self.acknowledgement(), value)
    }
}

/// One absolute monotonic deadline. No I/O operation can extend it.
#[derive(Clone, Copy, Debug)]
pub struct ExchangeDeadline(Instant);
impl ExchangeDeadline {
    /// Starts a bounded exchange. Zero and budgets over two seconds reject.
    pub fn start(budget: Duration) -> Result<Self, NativeFailure> {
        if budget.is_zero() || budget > MAX_EXCHANGE_WAIT {
            return Err(NativeFailure::LimitExceeded);
        }
        Ok(Self(
            Instant::now()
                .checked_add(budget)
                .ok_or(NativeFailure::LimitExceeded)?,
        ))
    }
    /// Remaining budget, or a typed timeout, including at exact expiry.
    pub fn remaining(&self) -> Result<Duration, NativeFailure> {
        let value = self.0.saturating_duration_since(Instant::now());
        if value.is_zero() {
            Err(NativeFailure::TimedOut)
        } else {
            Ok(value)
        }
    }
}

fn verify_mac(expected: &[u8], value: &[u8]) -> Result<(), NativeFailure> {
    if expected.len() == value.len() && bool::from(expected.ct_eq(value)) {
        Ok(())
    } else {
        Err(NativeFailure::CredentialUnavailable)
    }
}
fn u32_at(bytes: &[u8; PREFIX_BYTES], offset: usize) -> u32 {
    let mut value = [0; 4];
    value.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_be_bytes(value)
}
fn u64_at(bytes: &[u8; PREFIX_BYTES], offset: usize) -> u64 {
    let mut value = [0; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_be_bytes(value)
}
fn valid_process(value: &[u8; PREFIX_BYTES], offset: usize) -> bool {
    (1..=i32::MAX as u64).contains(&u64_at(value, offset)) && u32_at(value, offset + 16) < 1_000_000
}
fn is_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|&byte| byte == 0)
}
