//! Closed, bounded guest comparisons for listener-free paired maintenance.

use std::num::NonZeroU64;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use spaceterm_distribution::{
    BuildVersion, VerifiedAccountServerArtifact,
    server_feed::{MAX_ARCHIVE_BYTES, MAX_FEED_BYTES, MAX_FEED_SIGNATURE_BYTES},
};
use spaceterm_protocol::authentication::{Principal, ServerPin};

use super::super::{
    ServiceFailure, ServiceOutcome, ServicePhase, ServiceProgress, ServiceRequestState,
    supervisor::encode,
};
use super::{StoppedNativeAction, StoppedNativeAssessmentRef, StoppedNativeRequest};
use crate::{native_deployment::contracts::NativeRequestId, startup::AccountBuildKey};

/// Native JSON body bound, inside the existing 64 KiB account envelope.
pub const STOPPED_NATIVE_WIRE_LIMIT: usize = 32 * 1024;

/// Saved client metadata is comparison only. The ordinary handshake proves it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoppedNativeChannelRequest {
    /// Expected actual guest account and compiled build identity.
    pub expected_key: AccountBuildKey,
    /// Expected retained server identity and authentication key.
    #[serde(with = "super::super::remote_wire::pin_codec")]
    pub expected_pin: ServerPin,
    /// The initiating client's own saved device, pairing, and client binding.
    #[serde(with = "principal_codec")]
    pub principal: Principal,
    /// Original observed generation, checked under the native capture lease.
    pub registration_generation: NonZeroU64,
    /// Original guest-issued identifier, never allocated again by a retry.
    pub request: NativeRequestId,
    /// Exact original operation.
    pub action: StoppedNativeAction,
}

/// Exact section bounds for the existing signed STBA2 artifact grammar.
/// No path, package entry point, publisher key, or admission authority is carried.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoppedNativeArtifactTransfer {
    /// Exact original archive identity.
    pub artifact_sha256: [u8; 32],
    /// Exact verified manifest version, never a replacement candidate.
    pub version: BuildVersion,
    /// Exact signed feed bytes in the existing transfer section.
    pub feed_bytes: NonZeroU64,
    /// Exact feed signature bytes in the existing transfer section.
    pub signature_bytes: NonZeroU64,
    /// Exact archive bytes in the existing transfer section.
    pub archive_bytes: NonZeroU64,
}
impl StoppedNativeArtifactTransfer {
    /// Describes only an already authenticated artifact's exact byte sections.
    pub fn from_artifact(artifact: &VerifiedAccountServerArtifact) -> Result<Self, ServiceFailure> {
        fn length(bytes: &[u8]) -> Result<NonZeroU64, ServiceFailure> {
            NonZeroU64::new(u64::try_from(bytes.len()).map_err(|_| ServiceFailure::LimitExceeded)?)
                .ok_or(ServiceFailure::ArtifactRejected)
        }
        let transfer = Self {
            artifact_sha256: artifact.artifact_sha256(),
            version: artifact.manifest().version.clone(),
            feed_bytes: length(artifact.feed().bytes())?,
            signature_bytes: length(artifact.feed().signature())?,
            archive_bytes: length(artifact.bytes())?,
        };
        transfer.validate()?;
        Ok(transfer)
    }
    /// Bounds each independent section before archive allocation.
    pub fn validate(&self) -> Result<(), ServiceFailure> {
        if self.artifact_sha256 == [0; 32]
            || self.feed_bytes.get() > MAX_FEED_BYTES as u64
            || self.signature_bytes.get() > MAX_FEED_SIGNATURE_BYTES as u64
            || self.archive_bytes.get() > MAX_ARCHIVE_BYTES
        {
            return Err(ServiceFailure::ArtifactRejected);
        }
        self.wire_bytes().map(|_| ())
    }
    /// Exact length for a limited reader over the existing artifact receiver.
    /// Its trailing-byte check reaches this section's end, not the next handshake.
    pub fn wire_bytes(&self) -> Result<u64, ServiceFailure> {
        32_u64
            .checked_add(self.feed_bytes.get())
            .and_then(|length| length.checked_add(self.signature_bytes.get()))
            .and_then(|length| length.checked_add(self.archive_bytes.get()))
            .ok_or(ServiceFailure::LimitExceeded)
    }
    /// Rejects any replacement hash, version, or section length after verification.
    pub fn matches(&self, artifact: &VerifiedAccountServerArtifact) -> Result<(), ServiceFailure> {
        if Self::from_artifact(artifact)? != *self {
            return Err(ServiceFailure::RequestConflict);
        }
        Ok(())
    }
}

/// Closed SSH producer verbs. None can import paired admission authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoppedNativeAccountRequest {
    /// Captures the original, then enters actual pinned ordinary authentication.
    Channel {
        /// Exact observed native scope and initiating saved client comparisons.
        capture: StoppedNativeChannelRequest,
        /// Optional exact Stage section; no package is allowed for another action.
        artifact: Option<StoppedNativeArtifactTransfer>,
    },
    /// Restores original comparisons without updating issue time or authorization.
    Restore {
        /// Expected owning account and compiled build identity.
        expected_key: AccountBuildKey,
        /// Expected retained server identity and authentication key.
        #[serde(with = "super::super::remote_wire::pin_codec")]
        expected_pin: ServerPin,
        /// Original guest-issued identifier.
        request: NativeRequestId,
        /// Exact original action required by the caller's retained intent.
        action: StoppedNativeAction,
    },
    /// Resolves only the signed original, without a new ordinary handshake.
    Resolve {
        /// Expected owning account and compiled build identity.
        expected_key: AccountBuildKey,
        /// Expected retained server identity and authentication key.
        #[serde(with = "super::super::remote_wire::pin_codec")]
        expected_pin: ServerPin,
        /// Original guest-issued identifier.
        request: NativeRequestId,
        /// Exact Stage re-supply, accepted only after original paired admission.
        artifact: Option<StoppedNativeArtifactTransfer>,
    },
    /// Retires only the exact immutable terminal signed original receipt.
    Acknowledge {
        /// Expected owning account and compiled build identity.
        expected_key: AccountBuildKey,
        /// Expected retained server identity and authentication key.
        #[serde(with = "super::super::remote_wire::pin_codec")]
        expected_pin: ServerPin,
        /// Complete original terminal result.
        progress: ServiceProgress,
    },
}
impl StoppedNativeAccountRequest {
    /// Expected native identity comparisons for every closed verb.
    pub fn expected_identity(&self) -> (AccountBuildKey, ServerPin) {
        match self {
            Self::Channel { capture, .. } => (capture.expected_key, capture.expected_pin),
            Self::Restore {
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
    /// Original identifier, never a request allocation instruction.
    pub fn request(&self) -> NativeRequestId {
        match self {
            Self::Channel { capture, .. } => capture.request,
            Self::Restore { request, .. } | Self::Resolve { request, .. } => *request,
            Self::Acknowledge { progress, .. } => progress.request,
        }
    }
    /// Exact optional signed artifact section declared by this request.
    pub fn artifact(&self) -> Option<&StoppedNativeArtifactTransfer> {
        match self {
            Self::Channel { artifact, .. } | Self::Resolve { artifact, .. } => artifact.as_ref(),
            _ => None,
        }
    }
    /// Validates comparison data only, before any native effect or package input.
    pub fn validate(&self) -> Result<(), ServiceFailure> {
        let (key, pin) = self.expected_identity();
        if key.account_uid == 0
            || key.account_uid == u32::MAX
            || pin.identity == [0; 16]
            || pin.public_key == [0; 32]
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        if let Some(artifact) = self.artifact() {
            artifact.validate()?;
        }
        match self {
            Self::Channel { capture, artifact } => {
                let principal = capture.principal;
                if principal.server != pin.identity
                    || principal.device == [0; 16]
                    || principal.pairing == [0; 16]
                    || principal.client == [0; 16]
                {
                    return Err(ServiceFailure::InvalidRecord);
                }
                validate_action(capture.action)?;
                if let Some(artifact) = artifact
                    && !matches!(capture.action, StoppedNativeAction::Stage { artifact_sha256 } if artifact_sha256 == artifact.artifact_sha256)
                {
                    return Err(ServiceFailure::RequestConflict);
                }
                Ok(())
            }
            Self::Restore { action, .. } => validate_action(*action),
            Self::Acknowledge { progress, .. } => validate_progress(progress, true),
            Self::Resolve { .. } => Ok(()),
        }
    }
    /// Encodes one bounded closed body, inside the maintained account envelope.
    pub fn encode(&self) -> Result<Vec<u8>, ServiceFailure> {
        self.validate()?;
        encode(self, STOPPED_NATIVE_WIRE_LIMIT)
    }
    /// Rejects unknown verbs, unknown fields and oversized native bodies.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServiceFailure> {
        let value: Self = decode(bytes)?;
        value.validate()?;
        Ok(value)
    }
}

/// Content-free producer results. Paired authority stays in the native journal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "result",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StoppedNativeAccountReply {
    /// Original capture; the exact stream now proceeds into pinned authentication.
    Channel(StoppedNativeRequest),
    /// Exact retained capture and any unchanged original signed assessment.
    Restored {
        /// Exact original action and capture digest.
        original: StoppedNativeRequest,
        /// No assessment means that ordinary paired assessment has not completed.
        assessment: Option<StoppedNativeAssessmentRef>,
    },
    /// Original result, possibly still Reserved or Pending.
    Progress(ServiceProgress),
    /// Exact terminal result acknowledged, including after a lost ACK reply.
    Acknowledged {
        /// Immutable original terminal receipt.
        progress: ServiceProgress,
    },
    /// Fixed public classification, never raw native errors or paths.
    Refused(ServiceFailure),
}
impl StoppedNativeAccountReply {
    /// Validates public comparisons without importing admission authority.
    pub fn validate(&self) -> Result<(), ServiceFailure> {
        match self {
            Self::Channel(original) => validate_original(*original),
            Self::Restored {
                original,
                assessment,
            } => {
                validate_original(*original)?;
                if let Some(reference) = assessment
                    && (reference.original != *original
                        || reference.request_digest.bytes() == [0; 32]
                        || reference.proof_digest.bytes() == [0; 32]
                        || reference.epoch.get() == 0
                        || reference.affected_sessions != 0
                        || reference.expires_at.boot == [0; 32]
                        || reference.expires_at.millis == 0)
                {
                    return Err(ServiceFailure::InvalidRecord);
                }
                Ok(())
            }
            Self::Progress(progress) => validate_progress(progress, false),
            Self::Acknowledged { progress } => validate_progress(progress, true),
            Self::Refused(_) => Ok(()),
        }
    }
    /// Encodes one bounded content-free reply.
    pub fn encode(&self) -> Result<Vec<u8>, ServiceFailure> {
        self.validate()?;
        encode(self, STOPPED_NATIVE_WIRE_LIMIT)
    }
    /// Decodes only this closed reply grammar within the native body bound.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServiceFailure> {
        let value: Self = decode(bytes)?;
        value.validate()?;
        Ok(value)
    }
}
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ServiceFailure> {
    if bytes.len() > STOPPED_NATIVE_WIRE_LIMIT {
        return Err(ServiceFailure::LimitExceeded);
    }
    serde_json::from_slice(bytes).map_err(|_| ServiceFailure::InvalidRecord)
}
fn validate_action(action: StoppedNativeAction) -> Result<(), ServiceFailure> {
    if matches!(action, StoppedNativeAction::Stage { artifact_sha256 } if artifact_sha256 == [0; 32])
    {
        return Err(ServiceFailure::InvalidRecord);
    }
    Ok(())
}
fn validate_original(original: StoppedNativeRequest) -> Result<(), ServiceFailure> {
    validate_action(original.action)?;
    if original.capture_digest.bytes() == [0; 32] {
        return Err(ServiceFailure::InvalidRecord);
    }
    Ok(())
}
fn validate_progress(progress: &ServiceProgress, terminal: bool) -> Result<(), ServiceFailure> {
    if progress.request_digest.bytes() == [0; 32]
        || (terminal && progress.phase != ServicePhase::Completed)
    {
        return Err(ServiceFailure::InvalidRecord);
    }
    match (&progress.phase, &progress.state) {
        (
            ServicePhase::Completed,
            ServiceRequestState::Completed(ServiceOutcome::Started | ServiceOutcome::Uninstalled)
            | ServiceRequestState::NotApplied(_),
        ) => Ok(()),
        (
            ServicePhase::Completed,
            ServiceRequestState::Completed(
                ServiceOutcome::Staged(status) | ServiceOutcome::Activated(status),
            ),
        ) if status.artifact_sha256 != [0; 32] => Ok(()),
        (ServicePhase::Completed, _)
        | (_, ServiceRequestState::Completed(_) | ServiceRequestState::NotApplied(_)) => {
            Err(ServiceFailure::InvalidRecord)
        }
        (_, ServiceRequestState::Pending | ServiceRequestState::Indeterminate) => Ok(()),
    }
}
mod principal_codec {
    use super::*;
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SavedPrincipal {
        server: [u8; 16],
        device: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    }
    pub(super) fn serialize<S: serde::Serializer>(
        principal: &Principal,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        SavedPrincipal {
            server: principal.server,
            device: principal.device,
            pairing: principal.pairing,
            client: principal.client,
        }
        .serialize(serializer)
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Principal, D::Error> {
        let principal = SavedPrincipal::deserialize(deserializer)?;
        Ok(Principal {
            server: principal.server,
            device: principal.device,
            pairing: principal.pairing,
            client: principal.client,
        })
    }
}
