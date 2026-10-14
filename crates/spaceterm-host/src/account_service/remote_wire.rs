//! Closed, pathless SSH observations of native lifecycle requests.
//!
//! These DTOs carry comparisons, not pairing authority. Only the authenticated
//! runtime can assess and admit live work. The SSH producer resolves that already
//! accepted original; it cannot turn a reservation into admission.

use std::{io, num::NonZeroU64};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use spaceterm_protocol::authentication::ServerPin;

use super::{
    InstallationStatus, NativeLifecycleAssessmentRef, NativeLifecycleRequest, ServiceAction,
    ServiceFailure, ServiceOutcome, ServicePhase, ServiceProgress, ServiceRequestState,
    ServiceStatus,
};
use crate::{native_deployment::contracts::NativeRequestId, startup::AccountBuildKey};

/// Maximum encoded native lifecycle body, within the account envelope's 64 KiB limit.
pub const NATIVE_LIFECYCLE_WIRE_LIMIT: usize = 32 * 1024;

/// Content-free framing and comparison failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeLifecycleWireFailure {
    /// The encoded body exceeds the native lifecycle limit.
    LimitExceeded,
    /// A record is malformed, has unknown fields, or contains invalid comparisons.
    InvalidRecord,
}

/// Native status with the registration generation observed on the guest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLifecycleObservation {
    /// Native status, including retained intent and selected signed code.
    pub status: ServiceStatus,
    /// The exact nonzero registration generation, not refreshed capture authority.
    pub registration_generation: NonZeroU64,
}

/// Closed owner-side operations. Every operation binds the same account/build and pin.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeLifecycleAccountRequest {
    /// Reads native facts without installing, enrolling, or starting anything.
    Observe {
        /// Expected guest account and compiled identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity.
        #[serde(with = "pin_codec")]
        expected_pin: ServerPin,
    },
    /// Durably reserves one original identifier, without admission.
    Reserve {
        /// Expected guest account and compiled identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity.
        #[serde(with = "pin_codec")]
        expected_pin: ServerPin,
    },
    /// Captures the original exact action and native expiry before paired assessment.
    Capture {
        /// Expected guest account and compiled identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity.
        #[serde(with = "pin_codec")]
        expected_pin: ServerPin,
        /// Guest-issued original identifier; retries never allocate a replacement.
        request: NativeRequestId,
        /// Exact live action, admitted only by the paired worker, never Start or Stage.
        action: ServiceAction,
    },
    /// Restores original comparisons, including expired ones, without refreshing them.
    Restore {
        /// Expected guest account and compiled identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity.
        #[serde(with = "pin_codec")]
        expected_pin: ServerPin,
        /// Exact original capture retained by the client.
        original: NativeLifecycleRequest,
        /// Optional exact signed assessment comparison retained by the client.
        assessment: Option<NativeLifecycleAssessmentRef>,
    },
    /// Resolves only a signed runtime-origin original, never promoting Reserved.
    Resolve {
        /// Expected guest account and compiled identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity.
        #[serde(with = "pin_codec")]
        expected_pin: ServerPin,
        /// Original identifier, not a new action or runtime scope.
        request: NativeRequestId,
    },
    /// Retires only an exact immutable terminal result under the native lease.
    Acknowledge {
        /// Expected guest account and compiled identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity.
        #[serde(with = "pin_codec")]
        expected_pin: ServerPin,
        /// Complete original progress, including digest, phase, and terminal state.
        progress: ServiceProgress,
    },
}

/// Content-free native results. No result contains paths, credentials, or runtime tokens.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "result",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeLifecycleAccountReply {
    /// Current facts, not the outcome of a historical request.
    Observed(NativeLifecycleObservation),
    /// One durably reserved original identifier.
    Reserved(NativeRequestId),
    /// Original capture, unchanged after a lost reply or expiry.
    Captured(NativeLifecycleRequest),
    /// Original capture and any original signed assessment, without refreshed expiry.
    Restored {
        /// Exact retained capture.
        original: NativeLifecycleRequest,
        /// Exact original signed public reference, when assessed.
        assessment: Option<NativeLifecycleAssessmentRef>,
    },
    /// Original durable progress, possibly still Reserved.
    Progress(ServiceProgress),
    /// The exact immutable terminal progress acknowledged, including on retry.
    Acknowledged {
        /// Exact terminal progress, never a replacement request ID.
        progress: ServiceProgress,
    },
    /// A bounded classification, never native error text.
    Refused(ServiceFailure),
}

impl NativeLifecycleAccountRequest {
    /// Returns the expected native identity for every closed verb.
    pub fn expected_identity(&self) -> (AccountBuildKey, ServerPin) {
        match self {
            Self::Observe {
                expected_key,
                expected_pin,
            }
            | Self::Reserve {
                expected_key,
                expected_pin,
            }
            | Self::Capture {
                expected_key,
                expected_pin,
                ..
            }
            | Self::Restore {
                expected_key,
                expected_pin,
                ..
            }
            | Self::Resolve {
                expected_key,
                expected_pin,
                ..
            }
            | Self::Acknowledge {
                expected_key,
                expected_pin,
                ..
            } => (*expected_key, *expected_pin),
        }
    }

    /// Checks identities and comparison DTOs without importing native authority.
    pub fn validate(&self) -> Result<(), NativeLifecycleWireFailure> {
        let (key, pin) = self.expected_identity();
        validate_key(key)?;
        validate_pin(pin)?;
        match self {
            Self::Restore {
                original,
                assessment,
                ..
            } => validate_restore(*original, assessment.as_ref()),
            Self::Acknowledge { progress, .. } => validate_terminal(progress),
            Self::Observe { .. }
            | Self::Reserve { .. }
            | Self::Capture { .. }
            | Self::Resolve { .. } => Ok(()),
        }
    }

    /// Encodes one bounded native lifecycle body, rejecting invalid comparisons.
    pub fn encode(&self) -> Result<Vec<u8>, NativeLifecycleWireFailure> {
        self.validate()?;
        encode(self)
    }

    /// Decodes one bounded, closed native lifecycle body before native effects.
    pub fn decode(bytes: &[u8]) -> Result<Self, NativeLifecycleWireFailure> {
        let request: Self = decode(bytes)?;
        request.validate()?;
        Ok(request)
    }
}

impl NativeLifecycleAccountReply {
    /// Checks public native comparisons, without issuing runtime authority.
    pub fn validate(&self) -> Result<(), NativeLifecycleWireFailure> {
        match self {
            Self::Observed(observation) => validate_status(&observation.status),
            Self::Captured(original) => validate_capture(*original),
            Self::Restored {
                original,
                assessment,
            } => validate_restore(*original, assessment.as_ref()),
            Self::Progress(progress) => validate_progress(progress),
            Self::Acknowledged { progress } => validate_terminal(progress),
            Self::Reserved(_) | Self::Refused(_) => Ok(()),
        }
    }

    /// Encodes one bounded content-free reply.
    pub fn encode(&self) -> Result<Vec<u8>, NativeLifecycleWireFailure> {
        self.validate()?;
        encode(self)
    }

    /// Decodes one bounded closed reply and validates its nested comparisons.
    pub fn decode(bytes: &[u8]) -> Result<Self, NativeLifecycleWireFailure> {
        let reply: Self = decode(bytes)?;
        reply.validate()?;
        Ok(reply)
    }
}

fn validate_key(key: AccountBuildKey) -> Result<(), NativeLifecycleWireFailure> {
    if key.account_uid == 0 || key.account_uid == u32::MAX {
        return Err(NativeLifecycleWireFailure::InvalidRecord);
    }
    Ok(())
}

fn validate_pin(pin: ServerPin) -> Result<(), NativeLifecycleWireFailure> {
    if pin.identity == [0; 16] || pin.public_key == [0; 32] {
        return Err(NativeLifecycleWireFailure::InvalidRecord);
    }
    Ok(())
}

fn validate_capture(original: NativeLifecycleRequest) -> Result<(), NativeLifecycleWireFailure> {
    if original.capture_digest.bytes() == [0; 32] {
        return Err(NativeLifecycleWireFailure::InvalidRecord);
    }
    Ok(())
}

fn validate_restore(
    original: NativeLifecycleRequest,
    assessment: Option<&NativeLifecycleAssessmentRef>,
) -> Result<(), NativeLifecycleWireFailure> {
    validate_capture(original)?;
    if let Some(assessment) = assessment
        && (assessment.original != original
            || assessment.request_digest.bytes() == [0; 32]
            || assessment.proof_digest.bytes() == [0; 32]
            || assessment.epoch.get() == 0
            || assessment.affected_sessions > 64
            || assessment.expires_at.boot == [0; 32]
            || assessment.expires_at.millis == 0)
    {
        return Err(NativeLifecycleWireFailure::InvalidRecord);
    }
    Ok(())
}

fn validate_installation(
    installation: &InstallationStatus,
) -> Result<(), NativeLifecycleWireFailure> {
    if installation.artifact_sha256 == [0; 32] {
        return Err(NativeLifecycleWireFailure::InvalidRecord);
    }
    Ok(())
}

fn validate_status(status: &ServiceStatus) -> Result<(), NativeLifecycleWireFailure> {
    validate_key(status.key)?;
    if status.server.is_none_or(|server| server.bytes() == [0; 16]) || status.desired.is_none() {
        return Err(NativeLifecycleWireFailure::InvalidRecord);
    }
    for installation in status.approved.iter().chain(status.staged.iter()) {
        validate_installation(installation)?;
    }
    if status
        .approved
        .as_ref()
        .zip(status.staged.as_ref())
        .is_some_and(|(approved, staged)| approved.installation == staged.installation)
    {
        return Err(NativeLifecycleWireFailure::InvalidRecord);
    }
    Ok(())
}

fn validate_progress(progress: &ServiceProgress) -> Result<(), NativeLifecycleWireFailure> {
    if progress.request_digest.bytes() == [0; 32] {
        return Err(NativeLifecycleWireFailure::InvalidRecord);
    }
    match (&progress.phase, &progress.state) {
        (
            ServicePhase::Completed,
            ServiceRequestState::Completed(ServiceOutcome::Activated(installation)),
        ) => validate_installation(installation),
        (
            ServicePhase::Completed,
            ServiceRequestState::Completed(
                ServiceOutcome::Stopped | ServiceOutcome::Restarted | ServiceOutcome::Uninstalled,
            )
            | ServiceRequestState::NotApplied(_),
        ) => Ok(()),
        // Exact native non-admission is read-only Reserved, not an ACKable result.
        (ServicePhase::Reserved, ServiceRequestState::NotApplied(_)) => Ok(()),
        (ServicePhase::Completed, _)
        | (_, ServiceRequestState::Completed(_) | ServiceRequestState::NotApplied(_)) => {
            Err(NativeLifecycleWireFailure::InvalidRecord)
        }
        (_, ServiceRequestState::Pending | ServiceRequestState::Indeterminate) => Ok(()),
    }
}

fn validate_terminal(progress: &ServiceProgress) -> Result<(), NativeLifecycleWireFailure> {
    validate_progress(progress)?;
    if progress.phase != ServicePhase::Completed {
        return Err(NativeLifecycleWireFailure::InvalidRecord);
    }
    Ok(())
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, NativeLifecycleWireFailure> {
    if bytes.len() > NATIVE_LIFECYCLE_WIRE_LIMIT {
        return Err(NativeLifecycleWireFailure::LimitExceeded);
    }
    serde_json::from_slice(bytes).map_err(|_| NativeLifecycleWireFailure::InvalidRecord)
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, NativeLifecycleWireFailure> {
    let mut writer = BoundedWriter(Vec::new());
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| NativeLifecycleWireFailure::LimitExceeded)?;
    Ok(writer.0)
}

struct BoundedWriter(Vec<u8>);
impl io::Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > NATIVE_LIFECYCLE_WIRE_LIMIT.saturating_sub(self.0.len()) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) mod pin_codec {
    use super::*;
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Pin {
        identity: [u8; 16],
        public_key: [u8; 32],
    }
    pub(crate) fn serialize<S: serde::Serializer>(
        pin: &ServerPin,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Pin {
            identity: pin.identity,
            public_key: pin.public_key,
        }
        .serialize(serializer)
    }
    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ServerPin, D::Error> {
        let pin = Pin::deserialize(deserializer)?;
        let pin = ServerPin {
            identity: pin.identity,
            public_key: pin.public_key,
        };
        validate_pin(pin).map_err(|_| serde::de::Error::custom("invalid server pin"))?;
        Ok(pin)
    }
}
