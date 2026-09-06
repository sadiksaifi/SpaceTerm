//! Narrow capability for probing a Control Connection endpoint before OpenSSH owns it.

use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("the private Control Connection endpoint is unavailable")]
pub(crate) struct ControlSocketUnavailable;

/// Verifies that one exact local endpoint can be created without retaining it.
pub(crate) trait ControlSocketProbe: Send + Sync {
    fn probe(&self, endpoint: &Path) -> Result<(), ControlSocketUnavailable>;
}
