//! Linux post-exec observation and launcher composition, owned by native-handoff B1 and B2.

use crate::native_deployment::contracts::NativeFailure;
#[cfg(target_os = "linux")]
use crate::native_deployment::contracts::{CredentialPolicyFacts, CredentialReleasePermit};

#[cfg(target_os = "linux")]
pub mod credential_tool;
#[cfg(target_os = "linux")]
pub(crate) mod post_exec;
#[cfg(target_os = "linux")]
pub(crate) use post_exec::NativeLinuxPostExecLease;
#[cfg(target_os = "linux")]
#[expect(unused_imports, reason = "B2 composes native post-exec issuance")]
pub(crate) use post_exec::verify_linux_post_exec;

#[cfg(not(target_os = "linux"))]
pub(crate) enum NativeLinuxPostExecLease {}

#[cfg(not(target_os = "linux"))]
impl NativeLinuxPostExecLease {
    pub(crate) fn into_proof(self) -> crate::startup::PostExecProof {
        match self {}
    }
}

#[cfg(not(target_os = "linux"))]
#[expect(dead_code, reason = "Linux issuer is not available on other hosts")]
pub(crate) fn verify_linux_post_exec(
    _pending: &crate::startup::PendingStartup,
    _server_pidfd: std::os::fd::OwnedFd,
) -> Result<NativeLinuxPostExecLease, NativeFailure> {
    Err(NativeFailure::UnsupportedHost)
}

/// Inspects only the tool and ciphertext retained by a live release permit.
#[cfg(target_os = "linux")]
pub(crate) fn inspect_linux_credential_policy_bound(
    permit: &CredentialReleasePermit,
) -> Result<CredentialPolicyFacts, NativeFailure> {
    #[cfg(target_os = "linux")]
    {
        let execution = permit.linux_execution()?;
        credential_tool::inspect_ciphertext(
            execution.ciphertext,
            permit.credential_binding().linux_mode()?,
        )?;
        permit.recheck()?;
        Ok(CredentialPolicyFacts {
            schema: 1,
            account_and_code_bound: true,
            interaction_disabled: true,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = permit;
        Err(NativeFailure::UnsupportedHost)
    }
}
