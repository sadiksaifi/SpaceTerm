//! One owned native post-exec proof for both startup and release admission.

use std::fmt;

use crate::{native_deployment::contracts::NativeFailure, startup::PendingStartupContext};

/// Retained native execution authority. Comparison facts cannot construct it.
/// The native issuer keeps process and installation custody live through each recheck.
pub struct PostExecProof {
    lease: Box<dyn RetainedPostExec>,
}

pub(crate) trait RetainedPostExec: Send + Sync {
    fn recheck(&self, context: &PendingStartupContext) -> Result<(), NativeFailure>;
}

impl PostExecProof {
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "B2 composes the Darwin native lease")
    )]
    pub(crate) fn retain(lease: impl RetainedPostExec + 'static) -> Self {
        Self {
            lease: Box::new(lease),
        }
    }

    /// Revalidates the native lease for this exact pending startup before release.
    pub fn recheck(&self, context: &PendingStartupContext) -> Result<(), NativeFailure> {
        self.lease.recheck(context)
    }
}

impl fmt::Debug for PostExecProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PostExecProof(..)")
    }
}
