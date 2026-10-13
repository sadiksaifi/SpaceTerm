//! Linger is the only privileged SSH Bootstrap operation.
//!
//! The existing OpenSSH owner retains the exact destination and child/reaper scope.
//! SpaceTerm code, installers and enrollment never execute through this sudo port.

use super::{
    probe::ReadOnlyHostProbe,
    process::{BootstrapProbeFailure, BootstrapProbeLease},
};
use spaceterm_core::bootstrap::{BootstrapLinger, BootstrapOs, BootstrapSudoAvailability};
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU8, Ordering},
};
use zeroize::Zeroizing;

/// Content-free failures from one foreground linger exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BootstrapLingerFailure {
    /// The account declined the fixed authorization.
    #[error("linger authorization denied")]
    Denied,
    /// The foreground action was cancelled.
    #[error("linger authorization cancelled")]
    Cancelled,
    /// The fixed sudo route or prompt is unavailable.
    #[error("linger authorization unavailable")]
    Unavailable,
    /// The retained SSH destination or account no longer matches.
    #[error("linger SSH route changed")]
    RouteChanged,
    /// The attempt count or response byte bound was exceeded.
    #[error("linger exchange limit exceeded")]
    LimitExceeded,
    /// The response contains terminal control bytes.
    #[error("linger response is not terminal-safe")]
    InvalidResponse,
    /// A read-only query could not certify enabled lingering.
    #[error("linger state unavailable")]
    StateUnavailable,
    /// The foreground exchange exceeded its authorization deadline.
    #[error("linger authorization timed out")]
    TimedOut,
}
/// One owned, bounded SSH authorization wait.
pub type LingerFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BootstrapLingerFailure>> + Send + 'a>>;

/// No caller-selected executable, operation, prompt text or account name is accepted.
pub struct LingerInvocation {
    uid: u32,
}
impl LingerInvocation {
    /// The system sudo binary, never downloaded SpaceTerm code.
    pub fn executable(&self) -> &'static str {
        "/usr/bin/sudo"
    }
    /// Fixed fresh authorization. There is no stdin-password or AskPass mode.
    pub fn arguments(&self) -> [String; 8] {
        [
            "-k".into(),
            "-p".into(),
            "[SpaceTerm linger password]".into(),
            "--".into(),
            "/usr/bin/loginctl".into(),
            "enable-linger".into(),
            "--".into(),
            self.uid.to_string(),
        ]
    }
    /// The retained nonroot SSH account, never a remotely supplied account name.
    pub fn account_uid(&self) -> u32 {
        self.uid
    }
}

/// Existing obscured Authentication Prompt, bound to the foreground attempt and OpenSSH route.
pub trait LingerAuthenticationPrompt: Send + Sync {
    /// Rechecks the original foreground attempt and route without presenting a prompt.
    fn revalidate(&self, uid: u32) -> Result<(), BootstrapLingerFailure>;
    /// One zeroized response, never an installer payload or reusable authorization grant.
    fn respond(&self, uid: u32) -> LingerFuture<'_, Zeroizing<Vec<u8>>>;
}

/// Password delivery is available only during one exact sudo exchange.
pub struct ScopedLingerAuthentication<'a> {
    uid: u32,
    ssh: &'a dyn BootstrapProbeLease,
    prompt: &'a dyn LingerAuthenticationPrompt,
    attempts: AtomicU8,
}
impl ScopedLingerAuthentication<'_> {
    /// The transport writes the response only to its retained remote sudo terminal.
    /// It must never write it to a shell, command arguments, environment or an artifact stream.
    pub fn respond(&self, uid: u32) -> LingerFuture<'_, Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            if uid != self.uid {
                return Err(BootstrapLingerFailure::RouteChanged);
            }
            recheck(self.ssh)?;
            self.prompt.revalidate(self.uid)?;
            self.attempts
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    (count < 3).then_some(count + 1)
                })
                .map_err(|_| BootstrapLingerFailure::LimitExceeded)?;
            let response = self.prompt.respond(self.uid).await?;
            recheck(self.ssh)?;
            self.prompt.revalidate(self.uid)?;
            if response.is_empty() || response.len() > 16 * 1024 {
                return Err(BootstrapLingerFailure::LimitExceeded);
            }
            if response.iter().any(|byte| *byte < 0x20 || *byte == 0x7f) {
                return Err(BootstrapLingerFailure::InvalidResponse);
            }
            Ok(response)
        })
    }
}

/// Existing OpenSSH process owner. Drop/cancellation retires and reaps only its exact child group.
pub trait BootstrapLingerTransport: Send + Sync {
    /// Retained remote sudo terminal and its process owner.
    type Exchange: Send;
    /// Retains the original route, never opening a replacement host/account on failure.
    fn retain<'a>(
        &'a self,
        ssh: &'a dyn BootstrapProbeLease,
        invocation: &'a LingerInvocation,
    ) -> LingerFuture<'a, Self::Exchange>;
    /// Revalidates route and process ownership before any authorization or password delivery.
    fn revalidate(&self, exchange: &Self::Exchange) -> Result<(), BootstrapLingerFailure>;
    /// Executes only the fixed invocation. Denial/cancellation includes exact process reaping.
    fn invoke<'a>(
        &'a self,
        exchange: &'a mut Self::Exchange,
        invocation: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()>;
    /// Queries linger read-only after the exchange. Exit status alone is not a linger observation.
    fn observe<'a>(
        &'a self,
        exchange: &'a Self::Exchange,
        uid: u32,
    ) -> LingerFuture<'a, BootstrapLinger>;
}

/// Enabled linger and macOS skip every authorization callback. Unavailable is not disabled.
/// A disabled Linux account receives exactly the fixed loginctl operation, not installer authority.
pub async fn enable_linger_if_needed<T: BootstrapLingerTransport>(
    transport: &T,
    ssh: &dyn BootstrapProbeLease,
    host: &ReadOnlyHostProbe,
    prompt: &dyn LingerAuthenticationPrompt,
) -> Result<(), BootstrapLingerFailure> {
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        recheck(ssh)?;
        if host.account_uid != ssh.account_uid() || matches!(host.account_uid, 0 | u32::MAX) {
            return Err(BootstrapLingerFailure::RouteChanged);
        }
        match (host.os, host.linger) {
            (BootstrapOs::Linux, BootstrapLinger::Enabled)
            | (BootstrapOs::MacOs, BootstrapLinger::NotApplicable) => return Ok(()),
            (BootstrapOs::Linux, BootstrapLinger::Disabled) => (),
            _ => return Err(BootstrapLingerFailure::StateUnavailable),
        }
        if host.sudo != BootstrapSudoAvailability::Available {
            return Err(BootstrapLingerFailure::Unavailable);
        }
        prompt.revalidate(host.account_uid)?;
        let invocation = LingerInvocation {
            uid: host.account_uid,
        };
        let mut exchange = transport.retain(ssh, &invocation).await?;
        recheck(ssh)?;
        transport.revalidate(&exchange)?;
        let authentication = ScopedLingerAuthentication {
            uid: host.account_uid,
            ssh,
            prompt,
            attempts: AtomicU8::new(0),
        };
        transport
            .invoke(&mut exchange, &invocation, &authentication)
            .await?;
        recheck(ssh)?;
        prompt.revalidate(host.account_uid)?;
        transport.revalidate(&exchange)?;
        let observed = transport.observe(&exchange, host.account_uid).await?;
        recheck(ssh)?;
        transport.revalidate(&exchange)?;
        if observed != BootstrapLinger::Enabled {
            return Err(BootstrapLingerFailure::StateUnavailable);
        }
        Ok(())
    })
    .await
    .map_err(|_| BootstrapLingerFailure::TimedOut)?
}

fn recheck(ssh: &dyn BootstrapProbeLease) -> Result<(), BootstrapLingerFailure> {
    ssh.revalidate().map_err(|failure| match failure {
        BootstrapProbeFailure::Cancelled => BootstrapLingerFailure::Cancelled,
        BootstrapProbeFailure::RouteChanged => BootstrapLingerFailure::RouteChanged,
        _ => BootstrapLingerFailure::Unavailable,
    })
}
