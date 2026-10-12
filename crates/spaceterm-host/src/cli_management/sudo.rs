//! Fixed system sudo route. No password, shell, argv, path, or environment input.

use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::controller::ManagementProgress;
use crate::native_management::{
    SudoMode,
    request::{ManagementRequest, REQUEST_BYTES},
};

/// Immutable invocation facts. Only the boundary supplies the admitted helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SudoInvocation {
    mode: SudoMode,
}
impl SudoInvocation {
    /// Trusted OS executable. Its complete closure must also be retained by the boundary.
    pub fn executable(self) -> &'static str {
        "/usr/bin/sudo"
    }
    /// Fixed flags before the boundary's admitted manager. No -S, -E, or AskPass.
    pub fn prefix(self) -> &'static [&'static str] {
        match self.mode {
            SudoMode::Interactive => &["-k", "--"],
            SudoMode::NonInteractive => &["-k", "-n", "--"],
        }
    }
    /// Manager request transport. No credentials are passed in arguments.
    pub fn manager_arguments(self) -> &'static [&'static str] {
        &["--request-stdin"]
    }
}

/// Native execution and retained-copy boundary, not a caller-supplied path resolver.
pub trait SudoBoundary: Send + Sync {
    /// Opaque retained protected OS sudo, manager, interpreter/dependency closure.
    type RetainedHelper;
    /// Resolve fixed registered approved code under native custody and authenticate
    /// the destination. Root ownership and source authentication alone do not pass.
    fn retain_fixed_helper(
        &self,
        request: &ManagementRequest,
    ) -> Result<Self::RetainedHelper, NativeFailure>;
    /// Prove no replacement occurred and bind the exact copy through exec. A check
    /// followed by an unbound path exec is insufficient, even if hashes matched.
    fn recheck_execution_binding(
        &self,
        helper: &mut Self::RetainedHelper,
    ) -> Result<(), NativeFailure>;
    /// Execute only the retained helper through system sudo, with private owned
    /// stdin/stdout pipes. The fixed request stdin survives sudo
    /// descriptor closure. The manager claims it with RequestStdin before any child. Sanitize injection/AskPass/signing environment, use no
    /// shell, bound replies, discard native stderr, and reap the exact child.
    /// SUDO_UID and request UID do not authenticate the invoking principal.
    fn invoke(
        &self,
        helper: Self::RetainedHelper,
        invocation: SudoInvocation,
        request: &[u8; REQUEST_BYTES],
    ) -> Result<ManagementProgress, NativeFailure>;
}

/// Sudo consumer with no fallback authority or implicit production test adapter.
pub struct SudoAdapter<B> {
    boundary: B,
}
impl<B: SudoBoundary> SudoAdapter<B> {
    /// Inject native retained execution, not a command or helper pathname.
    pub fn new(boundary: B) -> Self {
        Self { boundary }
    }
    /// Retain, recheck, and invoke the fixed manager. The manager must independently
    /// prove caller binding, confirmation, assessment freshness, and native authority.
    pub fn execute(
        &self,
        request: &ManagementRequest,
        mode: SudoMode,
    ) -> Result<ManagementProgress, NativeFailure> {
        let mut helper = self.boundary.retain_fixed_helper(request)?;
        self.boundary.recheck_execution_binding(&mut helper)?;
        let progress = self
            .boundary
            .invoke(helper, SudoInvocation { mode }, &request.encode())?;
        if progress.request != *request.request_bytes() {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(progress)
    }
}

/// Unavailable until authenticated OS caller and retained exec providers are wired.
pub struct UnavailableSudoBoundary;
/// No success-shaped retained helper exists in unavailable production composition.
pub enum UnavailableHelper {}
impl SudoBoundary for UnavailableSudoBoundary {
    type RetainedHelper = UnavailableHelper;
    fn retain_fixed_helper(
        &self,
        _: &ManagementRequest,
    ) -> Result<UnavailableHelper, NativeFailure> {
        Err(NativeFailure::AuthorizationUnavailable)
    }
    fn recheck_execution_binding(
        &self,
        helper: &mut UnavailableHelper,
    ) -> Result<(), NativeFailure> {
        match *helper {}
    }
    fn invoke(
        &self,
        helper: UnavailableHelper,
        _: SudoInvocation,
        _: &[u8; REQUEST_BYTES],
    ) -> Result<ManagementProgress, NativeFailure> {
        match helper {}
    }
}

#[path = "execution.rs"]
mod execution;
pub use execution::{NativeSudoBoundary, RetainedSudoExecution};
#[path = "reply.rs"]
pub mod reply;
