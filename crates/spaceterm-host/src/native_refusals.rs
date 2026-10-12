//! Fixed refusal remedies, owned by native-artifacts D2. No native text or private data is presented.

use crate::native_deployment::contracts::NativeFailure;

/// A fixed administrator remedy classification, never an override or automatic policy change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRemedy {
    /// Ask an administrator to require authentication for this account's sudo commands.
    RequireSudoAuthentication,
    /// Use an account outside the docker group, or use rootless containers.
    AccountOutsideDocker,
    /// Use an account outside the lxd group.
    AccountOutsideLxd,
    /// Remove this account's access to the rootful container service.
    RemoveContainerBrokerAccess,
    /// Remove this account's raw-device permissions or use another account.
    RemoveRawDeviceAccess,
    /// Remove the delegated file or administrator bypass authority.
    RemoveDacBypass,
    /// Remove delegated tracing access for this account.
    RemoveTracingAccess,
    /// Remove delegated profiling access for this account.
    RemoveProfilingAccess,
    /// Ask an administrator to restrict system-wide profiling.
    RestrictSystemWideProfiling,
    /// Use a supported sudo and polkit policy.
    SupportedAuthorizationPolicy,
    /// Ask an administrator to configure the supported broker profile.
    SupportedBrokerProfile,
    /// Restore the required native policy checks, then retry.
    RestorePolicyChecks,
    /// Reboot after removing the bypass rights, then retry.
    RebootAfterRightsRemoval,
    /// Repair the protected server installation through native management.
    RepairProtectedInstallation,
    /// Move the protected installation to supported host storage.
    SupportedHostStorage,
    /// Restore a supported dumpability configuration and unreadable server executable.
    SupportedDumpability,
    /// Ask an administrator to install the authenticated native publisher.
    InstallAuthenticatedPublisher,
}

impl NativeRemedy {
    /// Returns only frozen public instructions, without native stderr or dynamic metadata.
    pub const fn text(self) -> &'static str {
        match self {
            Self::RequireSudoAuthentication => {
                "Ask an administrator to require authentication for this account's sudo commands."
            }
            Self::AccountOutsideDocker => {
                "Use an account outside the docker group, or use rootless containers."
            }
            Self::AccountOutsideLxd => "Use an account outside the lxd group.",
            Self::RemoveContainerBrokerAccess => {
                "Remove this account's access to the rootful container service."
            }
            Self::RemoveRawDeviceAccess => {
                "Remove this account's raw-device permissions or use another account."
            }
            Self::RemoveDacBypass => "Remove the delegated file or administrator bypass authority.",
            Self::RemoveTracingAccess => "Remove delegated tracing access for this account.",
            Self::RemoveProfilingAccess => "Remove delegated profiling access for this account.",
            Self::RestrictSystemWideProfiling => {
                "Ask an administrator to restrict system-wide profiling."
            }
            Self::SupportedAuthorizationPolicy => "Use a supported sudo and polkit policy.",
            Self::SupportedBrokerProfile => {
                "Ask an administrator to configure the supported broker profile."
            }
            Self::RestorePolicyChecks => "Restore the required native policy checks, then retry.",
            Self::RebootAfterRightsRemoval => {
                "Reboot after removing the bypass rights, then retry."
            }
            Self::RepairProtectedInstallation => {
                "Repair the protected server installation through native management."
            }
            Self::SupportedHostStorage => {
                "Move the protected installation to supported host storage."
            }
            Self::SupportedDumpability => {
                "Restore a supported dumpability configuration and unreadable server executable."
            }
            Self::InstallAuthenticatedPublisher => {
                "Ask an administrator to install the authenticated native publisher."
            }
        }
    }
}

/// Classifies Linux custody refusals. Other native failures have their own presentation policy.
pub const fn remedy_for(failure: NativeFailure) -> Option<NativeRemedy> {
    match failure {
        NativeFailure::LinuxPasswordlessSudo => Some(NativeRemedy::RequireSudoAuthentication),
        NativeFailure::LinuxDockerMembership => Some(NativeRemedy::AccountOutsideDocker),
        NativeFailure::LinuxLxdMembership => Some(NativeRemedy::AccountOutsideLxd),
        NativeFailure::LinuxContainerBrokerAccess => {
            Some(NativeRemedy::RemoveContainerBrokerAccess)
        }
        NativeFailure::LinuxRawDiskAccess => Some(NativeRemedy::RemoveRawDeviceAccess),
        NativeFailure::LinuxDelegatedDacAuthority => Some(NativeRemedy::RemoveDacBypass),
        NativeFailure::LinuxDelegatedTracing => Some(NativeRemedy::RemoveTracingAccess),
        NativeFailure::LinuxDelegatedProfiling => Some(NativeRemedy::RemoveProfilingAccess),
        NativeFailure::LinuxSystemWideProfiling => Some(NativeRemedy::RestrictSystemWideProfiling),
        NativeFailure::LinuxUnsupportedAuthorizationPolicy => {
            Some(NativeRemedy::SupportedAuthorizationPolicy)
        }
        NativeFailure::LinuxUnsupportedPrivilegedBroker => {
            Some(NativeRemedy::SupportedBrokerProfile)
        }
        NativeFailure::LinuxPolicyCheckUnavailable => Some(NativeRemedy::RestorePolicyChecks),
        NativeFailure::LinuxPriorBypassHistoryUnproven => {
            Some(NativeRemedy::RebootAfterRightsRemoval)
        }
        NativeFailure::LinuxPublicationUnavailable => {
            Some(NativeRemedy::RepairProtectedInstallation)
        }
        NativeFailure::LinuxMountWitnessUnavailable => Some(NativeRemedy::SupportedHostStorage),
        NativeFailure::LinuxUnsafeDumpableTransition => Some(NativeRemedy::SupportedDumpability),
        NativeFailure::BootstrapTrustUnavailable => {
            Some(NativeRemedy::InstallAuthenticatedPublisher)
        }
        _ => None,
    }
}
