//! Strict content-free manager replies. Native stderr and broker exit are never an outcome.

use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::controller::{ManagementPhase, ManagementProgress, ProgressState};

/// One bounded result line, with no paths, credentials, or native error strings.
pub const REPLY_LIMIT: usize = 96;

/// Encodes only public request correlation and fixed native phase/state values.
pub fn encode(progress: ManagementProgress) -> String {
    let mut request = String::with_capacity(32);
    use std::fmt::Write;
    for byte in progress.request {
        let _ = write!(request, "{byte:02x}");
    }
    let phase = match progress.phase {
        ManagementPhase::Admitted => 1,
        ManagementPhase::IntentCommitted => 2,
        ManagementPhase::StartupSuspended => 3,
        ManagementPhase::Quiescing => 4,
        ManagementPhase::Quiesced => 5,
        ManagementPhase::SupervisionRemoved => 6,
        ManagementPhase::Completed => 7,
    };
    let state = match progress.state {
        ProgressState::Pending => 1,
        ProgressState::Complete => 2,
        ProgressState::Indeterminate => 3,
        ProgressState::CleanupUnresolved => 4,
    };
    format!("progress={request},{phase},{state}\n")
}

/// Decodes one exact result, rejecting unknown codes, extra records, and native text.
/// The consumer independently verifies request correlation and the exact child's exit.
pub fn decode(bytes: &[u8]) -> Result<Result<ManagementProgress, NativeFailure>, NativeFailure> {
    if bytes.is_empty() || bytes.len() > REPLY_LIMIT {
        return Err(NativeFailure::InvalidRecord);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| NativeFailure::InvalidRecord)?;
    let line = text
        .strip_suffix('\n')
        .ok_or(NativeFailure::InvalidRecord)?;
    if let Some(code) = line.strip_prefix("classification=") {
        if code.is_empty()
            || code.len() > 2
            || !code.bytes().all(|c| c.is_ascii_digit())
            || code.starts_with('0')
        {
            return Err(NativeFailure::InvalidRecord);
        }
        let code: u32 = code.parse().map_err(|_| NativeFailure::InvalidRecord)?;
        return Ok(Err(failure(code)?));
    }
    let value = line
        .strip_prefix("progress=")
        .ok_or(NativeFailure::InvalidRecord)?;
    let fields: Vec<_> = value.split(',').collect();
    let [request, phase, state] = fields.as_slice() else {
        return Err(NativeFailure::InvalidRecord);
    };
    if request.len() != 32
        || !request
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(NativeFailure::InvalidRecord);
    }
    let mut identity = [0; 16];
    for (offset, byte) in identity.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&request[offset * 2..offset * 2 + 2], 16)
            .map_err(|_| NativeFailure::InvalidRecord)?;
    }
    if identity == [0; 16] {
        return Err(NativeFailure::InvalidRecord);
    }
    let phase = match *phase {
        "1" => ManagementPhase::Admitted,
        "2" => ManagementPhase::IntentCommitted,
        "3" => ManagementPhase::StartupSuspended,
        "4" => ManagementPhase::Quiescing,
        "5" => ManagementPhase::Quiesced,
        "6" => ManagementPhase::SupervisionRemoved,
        "7" => ManagementPhase::Completed,
        _ => return Err(NativeFailure::InvalidRecord),
    };
    let state = match *state {
        "1" => ProgressState::Pending,
        "2" if phase == ManagementPhase::Completed => ProgressState::Complete,
        "3" => ProgressState::Indeterminate,
        "4" => ProgressState::CleanupUnresolved,
        _ => return Err(NativeFailure::InvalidRecord),
    };
    Ok(Ok(ManagementProgress {
        request: identity,
        phase,
        state,
    }))
}
fn failure(code: u32) -> Result<NativeFailure, NativeFailure> {
    Ok(match code {
        1 => NativeFailure::UnsupportedHost,
        2 => NativeFailure::NativeUnavailable,
        3 => NativeFailure::CustodyUnavailable,
        4 => NativeFailure::AccountIdentityUnavailable,
        5 => NativeFailure::RegistrationMissing,
        6 => NativeFailure::StorageLocked,
        7 => NativeFailure::StorageUnavailable,
        8 => NativeFailure::UnsafeNativeState,
        9 => NativeFailure::InvalidRecord,
        10 => NativeFailure::UnsupportedSchema,
        11 => NativeFailure::LegacyRegistrationRequiresMigration,
        12 => NativeFailure::IdentityMismatch,
        13 => NativeFailure::GenerationMismatch,
        14 => NativeFailure::StaleAssessment,
        15 => NativeFailure::ConfirmationRequired,
        16 => NativeFailure::AuthorizationDenied,
        17 => NativeFailure::AuthorizationCancelled,
        18 => NativeFailure::AuthorizationUnavailable,
        19 => NativeFailure::InstallationUnavailable,
        20 => NativeFailure::ArtifactRejected,
        21 => NativeFailure::CredentialUnavailable,
        22 => NativeFailure::CredentialPolicyRejected,
        23 => NativeFailure::InteractionRequired,
        24 => NativeFailure::SupervisorUnavailable,
        25 => NativeFailure::BackgroundItemsSuppressed,
        26 => NativeFailure::ProcessIdentityMismatch,
        27 => NativeFailure::PrivilegeDropUnproven,
        28 => NativeFailure::ExecutionBindingUnproven,
        29 => NativeFailure::TimedOut,
        30 => NativeFailure::LimitExceeded,
        31 => NativeFailure::OutcomePending,
        32 => NativeFailure::Indeterminate,
        33 => NativeFailure::CleanupUnresolved,
        34 => NativeFailure::IoUnavailable,
        35 => NativeFailure::LinuxPasswordlessSudo,
        36 => NativeFailure::LinuxDockerMembership,
        37 => NativeFailure::LinuxLxdMembership,
        38 => NativeFailure::LinuxContainerBrokerAccess,
        39 => NativeFailure::LinuxRawDiskAccess,
        40 => NativeFailure::LinuxDelegatedDacAuthority,
        41 => NativeFailure::LinuxDelegatedTracing,
        42 => NativeFailure::LinuxDelegatedProfiling,
        43 => NativeFailure::LinuxSystemWideProfiling,
        44 => NativeFailure::LinuxUnsupportedAuthorizationPolicy,
        45 => NativeFailure::LinuxUnsupportedPrivilegedBroker,
        46 => NativeFailure::LinuxPolicyCheckUnavailable,
        48 => NativeFailure::LinuxPriorBypassHistoryUnproven,
        49 => NativeFailure::LinuxPublicationUnavailable,
        50 => NativeFailure::LinuxMountWitnessUnavailable,
        51 => NativeFailure::LinuxUnsafeDumpableTransition,
        52 => NativeFailure::BootstrapTrustUnavailable,
        _ => return Err(NativeFailure::InvalidRecord),
    })
}
