//! Linux post-exec observation and launcher composition, owned by native-handoff B1 and B2.

use std::convert::Infallible;
use std::os::fd::OwnedFd;

use crate::native_deployment::contracts::{
    CredentialPolicyFacts, CredentialReleasePermit, NativeFailure, PendingStartup, PostExecProof,
};

/// Retained pidfd and native post-exec observations, never a PID or readiness declaration.
#[expect(dead_code, reason = "native-handoff B1 replaces the unavailable seed")]
pub(crate) struct NativeLinuxPostExecLease {
    unavailable: Infallible,
}

impl NativeLinuxPostExecLease {
    pub(crate) fn into_proof(self) -> PostExecProof {
        match self.unavailable {}
    }
}

/// Performs independent retained native post-exec checks.
#[expect(dead_code, reason = "native-handoff B1 replaces the unavailable seed")]
pub(crate) fn verify_linux_post_exec(
    _pending: &PendingStartup,
    _server_pidfd: OwnedFd,
) -> Result<NativeLinuxPostExecLease, NativeFailure> {
    Err(NativeFailure::ExecutionBindingUnproven)
}

/// Inspects only the tool and ciphertext retained by a live release permit.
#[expect(dead_code, reason = "native-handoff B2 replaces the unavailable seed")]
pub(crate) fn inspect_linux_credential_policy_bound(
    _permit: &CredentialReleasePermit,
) -> Result<CredentialPolicyFacts, NativeFailure> {
    Err(NativeFailure::NativeUnavailable)
}
