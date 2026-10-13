//! Probe execution uses the existing OpenSSH owner. This module never launches another SSH stack.

use super::{
    command::{MAX_PROBE_BYTES, PROBE_DEADLINE, read_only_probe_script},
    probe::ReadOnlyHostProbe,
};
use std::{future::Future, io::Read, pin::Pin};

/// Bounded classifications from the unprivileged probe boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BootstrapProbeFailure {
    /// Output did not match the fixed probe grammar.
    #[error("invalid bootstrap probe")]
    InvalidProbe,
    /// Output exceeded the probe limit.
    #[error("bootstrap probe limit exceeded")]
    LimitExceeded,
    /// Effective SSH host or account changed.
    #[error("bootstrap probe route changed")]
    RouteChanged,
    /// The foreground owner cancelled the probe.
    #[error("bootstrap probe cancelled")]
    Cancelled,
    /// The owned SSH process failed.
    #[error("bootstrap probe unavailable")]
    Offline,
    /// The read-only probe exceeded its deadline.
    #[error("bootstrap probe timed out")]
    TimedOut,
}
/// Future owned by the existing SSH process boundary.
pub type BootstrapProbeFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BootstrapProbeFailure>> + Send + 'a>>;
/// Retained process owner, not caller-supplied route comparison facts.
pub trait BootstrapProbeLease: Send + Sync {
    /// Rechecks the original OpenSSH control connection and host/account bindings.
    fn revalidate(&self) -> Result<(), BootstrapProbeFailure>;
    /// Effective nonroot account established by OpenSSH.
    fn account_uid(&self) -> u32;
}
/// The existing managed-host machinery owns and reaps every process on cancellation or drop.
pub trait BootstrapProbeTransport: Send + Sync {
    /// Executes only this fixed probe under retained SSH process ownership.
    fn run_read_only<'a>(
        &'a self,
        ssh: &'a dyn BootstrapProbeLease,
        script: &'static [u8],
        maximum: usize,
    ) -> BootstrapProbeFuture<'a, Vec<u8>>;
}
/// Bounds the probe lifetime and validates the route before and after completion.
pub async fn read_only_preflight(
    transport: &dyn BootstrapProbeTransport,
    ssh: &dyn BootstrapProbeLease,
) -> Result<ReadOnlyHostProbe, BootstrapProbeFailure> {
    ssh.revalidate()?;
    let bytes = tokio::time::timeout(
        PROBE_DEADLINE,
        transport.run_read_only(ssh, read_only_probe_script(), MAX_PROBE_BYTES),
    )
    .await
    .map_err(|_| BootstrapProbeFailure::TimedOut)??;
    ssh.revalidate()?;
    ReadOnlyHostProbe::parse(&bytes, ssh.account_uid())
}
/// Bound before allocation. Never retain or return stderr or a raw process error.
pub fn read_bounded_probe(reader: impl Read) -> Result<Vec<u8>, BootstrapProbeFailure> {
    let mut bytes = Vec::with_capacity(MAX_PROBE_BYTES + 1);
    reader
        .take((MAX_PROBE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| BootstrapProbeFailure::Offline)?;
    if bytes.len() > MAX_PROBE_BYTES {
        return Err(BootstrapProbeFailure::LimitExceeded);
    }
    Ok(bytes)
}
