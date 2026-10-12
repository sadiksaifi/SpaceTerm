//! Native failure classification and decoding, owned by native-authority A1 and A3.

use super::contracts::NativeFailure;

impl TryFrom<u32> for NativeFailure {
    type Error = NativeFailure;

    fn try_from(code: u32) -> Result<Self, Self::Error> {
        match code {
            1 => Ok(Self::UnsupportedHost),
            2 => Ok(Self::NativeUnavailable),
            3 => Ok(Self::CustodyUnavailable),
            4 => Ok(Self::AccountIdentityUnavailable),
            5 => Ok(Self::RegistrationMissing),
            6 => Ok(Self::StorageLocked),
            7 => Ok(Self::StorageUnavailable),
            8 => Ok(Self::UnsafeNativeState),
            9 => Ok(Self::InvalidRecord),
            10 => Ok(Self::UnsupportedSchema),
            11 => Ok(Self::LegacyRegistrationRequiresMigration),
            12 => Ok(Self::IdentityMismatch),
            13 => Ok(Self::GenerationMismatch),
            14 => Ok(Self::StaleAssessment),
            15 => Ok(Self::ConfirmationRequired),
            16 => Ok(Self::AuthorizationDenied),
            17 => Ok(Self::AuthorizationCancelled),
            18 => Ok(Self::AuthorizationUnavailable),
            19 => Ok(Self::InstallationUnavailable),
            20 => Ok(Self::ArtifactRejected),
            21 => Ok(Self::CredentialUnavailable),
            22 => Ok(Self::CredentialPolicyRejected),
            23 => Ok(Self::InteractionRequired),
            24 => Ok(Self::SupervisorUnavailable),
            25 => Ok(Self::BackgroundItemsSuppressed),
            26 => Ok(Self::ProcessIdentityMismatch),
            27 => Ok(Self::PrivilegeDropUnproven),
            28 => Ok(Self::ExecutionBindingUnproven),
            29 => Ok(Self::TimedOut),
            30 => Ok(Self::LimitExceeded),
            31 => Ok(Self::OutcomePending),
            32 => Ok(Self::Indeterminate),
            33 => Ok(Self::CleanupUnresolved),
            34 => Ok(Self::IoUnavailable),
            35 => Ok(Self::LinuxPasswordlessSudo),
            36 => Ok(Self::LinuxDockerMembership),
            37 => Ok(Self::LinuxLxdMembership),
            38 => Ok(Self::LinuxContainerBrokerAccess),
            39 => Ok(Self::LinuxRawDiskAccess),
            40 => Ok(Self::LinuxDelegatedDacAuthority),
            41 => Ok(Self::LinuxDelegatedTracing),
            42 => Ok(Self::LinuxDelegatedProfiling),
            43 => Ok(Self::LinuxSystemWideProfiling),
            44 => Ok(Self::LinuxUnsupportedAuthorizationPolicy),
            45 => Ok(Self::LinuxUnsupportedPrivilegedBroker),
            46 => Ok(Self::LinuxPolicyCheckUnavailable),
            48 => Ok(Self::LinuxPriorBypassHistoryUnproven),
            49 => Ok(Self::LinuxPublicationUnavailable),
            50 => Ok(Self::LinuxMountWitnessUnavailable),
            51 => Ok(Self::LinuxUnsafeDumpableTransition),
            52 => Ok(Self::BootstrapTrustUnavailable),
            // Reserved 47 and every unknown code reject rather than create a refusal or authority.
            _ => Err(Self::InvalidRecord),
        }
    }
}
