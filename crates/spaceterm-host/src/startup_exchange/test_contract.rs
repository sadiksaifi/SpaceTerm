//! Frozen, nonauthorizing test contract used until ND520/1 is seeded.
#![allow(dead_code)]
pub mod contracts {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(u32)]
    pub enum NativeFailure {
        NativeUnavailable = 2,
        UnsafeNativeState = 8,
        InvalidRecord = 9,
        IdentityMismatch = 12,
        GenerationMismatch = 13,
        CredentialUnavailable = 21,
        ProcessIdentityMismatch = 26,
        PrivilegeDropUnproven = 27,
        ExecutionBindingUnproven = 28,
        TimedOut = 29,
        LimitExceeded = 30,
        CleanupUnresolved = 33,
        IoUnavailable = 34,
    }
}
