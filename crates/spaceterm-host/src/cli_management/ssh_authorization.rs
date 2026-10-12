//! One SSH Bootstrap sudo exchange. Responses never become reusable authorization.

use super::SudoInvocation;
use crate::native_deployment::contracts::{NativeFailure, NativeManagementFuture};
use crate::native_management::{
    SudoMode, controller::ManagementProgress, facade::ConfirmedNativeAction,
    request::ManagementRequest,
};
use std::sync::atomic::{AtomicU8, Ordering};
use zeroize::Zeroizing;

/// Existing zeroized Authentication Prompt machinery, selected by the SSH owner.
/// This port receives only the exact management request and never native output or a password cache.
pub trait SshSudoAuthenticationPrompt: Send + Sync {
    /// Requests one obscured response for the original authenticated SSH destination and account.
    /// Cancellation is a typed refusal. Responses are not cloned, logged or persisted.
    fn respond(
        &self,
        request: &ManagementRequest,
    ) -> NativeManagementFuture<'_, Zeroizing<Vec<u8>>>;
}

/// Borrowed authorization scope for one confirmed request. It has no Debug implementation,
/// exported grant, credential reference or constructor outside this exchange.
pub struct ScopedSshSudoAuthentication<'a> {
    request: &'a ManagementRequest,
    prompt: &'a dyn SshSudoAuthenticationPrompt,
    attempts: AtomicU8,
}
impl ScopedSshSudoAuthentication<'_> {
    /// Delivers a zeroized response only inside the original exchange. The SSH peer must
    /// feed it to the retained sudo terminal, never to a shell or the manager request channel.
    pub fn respond<'a>(
        &'a self,
        request: &'a ManagementRequest,
    ) -> NativeManagementFuture<'a, Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            if request != self.request {
                return Err(NativeFailure::IdentityMismatch);
            }
            self.attempts
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |attempts| {
                    (attempts < 3).then_some(attempts + 1)
                })
                .map_err(|_| NativeFailure::LimitExceeded)?;
            let response = self.prompt.respond(self.request).await?;
            if response.len() > 16 * 1024 {
                return Err(NativeFailure::LimitExceeded);
            }
            Ok(response)
        })
    }
}

/// Authenticated SSH transport to the protected native manager. OpenSSH host/account
/// trust and publication authority are prerequisites, not facts inferred from a downloaded helper.
pub trait NativeSshSudoBoundary: Send + Sync {
    /// Retained remote account, destination, broker, manager and exact child/reaper scope.
    /// Dropping a wait must not release custody while its native exchange is live.
    type Exchange: Send;
    /// Retains only the fixed native route for the original account/build and approved manager.
    fn retain(&self, request: &ManagementRequest) -> NativeManagementFuture<'_, Self::Exchange>;
    /// Rechecks host/account trust, exact request and native execution custody.
    fn recheck(
        &self,
        exchange: &Self::Exchange,
        request: &ManagementRequest,
    ) -> Result<(), NativeFailure>;
    /// Runs fixed sudo -k through the original SSH transport, using the existing obscured
    /// Authentication Prompt for sudo only. No AskPass executable, -S, shell command, credential
    /// environment or cached response is accepted. It returns the authenticated manager's reply
    /// only after exact reaping, including on denial/cancellation. No sudo timestamp is created.
    fn invoke<'a>(
        &'a self,
        exchange: &'a mut Self::Exchange,
        invocation: SudoInvocation,
        request: &'a ManagementRequest,
        authentication: &'a ScopedSshSudoAuthentication<'a>,
    ) -> NativeManagementFuture<'a, ManagementProgress>;
}

/// Scoped SSH Bootstrap sudo consumer. Native authorization remains in the remote OS broker.
pub struct SshSudoManagement<B, P> {
    boundary: B,
    prompts: P,
}
impl<B: NativeSshSudoBoundary, P: SshSudoAuthenticationPrompt> SshSudoManagement<B, P> {
    /// Injects the authenticated SSH native route and existing zeroized prompt owner.
    pub fn new(boundary: B, prompts: P) -> Self {
        Self { boundary, prompts }
    }
    /// Uses one original confirmation and discards its authorization scope on every return.
    pub fn execute(
        &self,
        confirmed: ConfirmedNativeAction,
    ) -> NativeManagementFuture<'_, ManagementProgress> {
        Box::pin(async move {
            let request = confirmed.assessment().request();
            let mut exchange = self.boundary.retain(request).await?;
            self.boundary.recheck(&exchange, request)?;
            let authentication = ScopedSshSudoAuthentication {
                request,
                prompt: &self.prompts,
                attempts: AtomicU8::new(0),
            };
            let progress = self
                .boundary
                .invoke(
                    &mut exchange,
                    SudoInvocation::for_mode(SudoMode::Interactive),
                    request,
                    &authentication,
                )
                .await?;
            self.boundary.recheck(&exchange, request)?;
            if progress.request != *request.request_bytes() {
                return Err(NativeFailure::IdentityMismatch);
            }
            Ok(progress)
        })
    }
}
