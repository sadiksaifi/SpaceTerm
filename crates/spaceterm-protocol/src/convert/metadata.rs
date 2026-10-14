//! Bounded runtime display facts, independent of terminal content revisions.

use super::identity::{decode_session, encode_session, required};
use crate::{ProtocolError, wire::common};
use spaceterm_core::observations::{MonotonicTick, RuntimeObservationSequence};
use spaceterm_terminal::{
    metadata::{MetadataFreshness, ProgressMetadata, PromptZone, TitleMetadata, TitleProvenance},
    runtime_contracts::{
        RuntimeMetadataIdentity, RuntimeMetadataOrigin, RuntimeMetadataParts,
        RuntimeMetadataSnapshot,
    },
};

pub fn encode_snapshot(value: &RuntimeMetadataSnapshot) -> common::RuntimeMetadataSnapshot {
    let parts = value.parts();
    let (origin_kind, user, host) = match &parts.origin {
        RuntimeMetadataOrigin::Local { user, host } => (1, user, host.as_ref()),
        RuntimeMetadataOrigin::Remote { user, host } => (2, user, Some(host)),
    };
    let (progress_kind, progress_percent) = match parts.progress {
        ProgressMetadata::None => (0, 0),
        ProgressMetadata::Normal(value) => (1, u32::from(value)),
        ProgressMetadata::Error(value) => (2, u32::from(value)),
        ProgressMetadata::Indeterminate => (3, 0),
        ProgressMetadata::Paused(value) => (4, u32::from(value)),
    };
    common::RuntimeMetadataSnapshot {
        session: Some(encode_session(parts.session)),
        sequence: parts.sequence.get(),
        observed_at: parts.observed_at.get(),
        title: parts.title.value.to_string(),
        title_provenance: match parts.title.provenance {
            TitleProvenance::Fallback => 0,
            TitleProvenance::WorkingDirectory => 1,
            TitleProvenance::TerminalControl => 2,
        },
        origin_kind,
        user: user.as_ref().map(|value| value.as_str().to_owned()),
        host: host.map(|value| value.as_str().to_owned()),
        progress_kind,
        progress_percent,
        freshness: match parts.freshness {
            MetadataFreshness::Live => 0,
            MetadataFreshness::Stale => 1,
        },
        prompt_zone: match parts.prompt_zone {
            PromptZone::Unknown => 0,
            PromptZone::Prompt => 1,
            PromptZone::CommandInput => 2,
            PromptZone::CommandOutput => 3,
        },
        hidden_input: parts.hidden_input,
    }
}

pub fn decode_snapshot(
    value: &common::RuntimeMetadataSnapshot,
) -> Result<RuntimeMetadataSnapshot, ProtocolError> {
    let bounded = |value: &String| {
        RuntimeMetadataIdentity::new(value.clone()).map_err(|_| ProtocolError::Malformed)
    };
    let user = value.user.as_ref().map(bounded).transpose()?;
    let host = value.host.as_ref().map(bounded).transpose()?;
    let origin = match value.origin_kind {
        1 => RuntimeMetadataOrigin::Local { user, host },
        2 => RuntimeMetadataOrigin::Remote {
            user,
            host: host.ok_or(ProtocolError::Malformed)?,
        },
        _ => return Err(ProtocolError::Malformed),
    };
    let percent = u8::try_from(value.progress_percent).map_err(|_| ProtocolError::Malformed)?;
    let progress = match (value.progress_kind, percent) {
        (0, 0) => ProgressMetadata::None,
        (1, percent) => ProgressMetadata::Normal(percent),
        (2, percent) => ProgressMetadata::Error(percent),
        (3, 0) => ProgressMetadata::Indeterminate,
        (4, percent) => ProgressMetadata::Paused(percent),
        _ => return Err(ProtocolError::Malformed),
    };
    RuntimeMetadataSnapshot::new(RuntimeMetadataParts {
        session: decode_session(required(&value.session)?)?,
        sequence: RuntimeObservationSequence::new(value.sequence)
            .map_err(|_| ProtocolError::Malformed)?,
        observed_at: MonotonicTick::new(value.observed_at),
        title: TitleMetadata {
            value: value.title.as_str().into(),
            provenance: match value.title_provenance {
                0 => TitleProvenance::Fallback,
                1 => TitleProvenance::WorkingDirectory,
                2 => TitleProvenance::TerminalControl,
                _ => return Err(ProtocolError::Malformed),
            },
        },
        origin,
        progress,
        freshness: match value.freshness {
            0 => MetadataFreshness::Live,
            1 => MetadataFreshness::Stale,
            _ => return Err(ProtocolError::Malformed),
        },
        prompt_zone: match value.prompt_zone {
            0 => PromptZone::Unknown,
            1 => PromptZone::Prompt,
            2 => PromptZone::CommandInput,
            3 => PromptZone::CommandOutput,
            _ => return Err(ProtocolError::Malformed),
        },
        hidden_input: value.hidden_input,
    })
    .map_err(|_| ProtocolError::Malformed)
}

macro_rules! profile {
    ($p:ident) => {
        pub mod $p {
            use super::*;
            use crate::{
                convert::identity::{decode_fence, encode_fence},
                wire::$p as wire,
            };
            use spaceterm_core::authority::EffectFence;
            pub fn encode_publication(
                fence: EffectFence,
                request: u64,
                generation: u64,
                snapshot: &RuntimeMetadataSnapshot,
            ) -> Result<wire::MetadataPublication, ProtocolError> {
                if fence.content().is_some() {
                    return Err(ProtocolError::Malformed);
                }
                let value = wire::MetadataPublication {
                    fence: Some(encode_fence(fence)),
                    request,
                    request_generation: generation,
                    snapshot: Some(encode_snapshot(snapshot)),
                };
                decode_publication(&value)?;
                Ok(value)
            }
            pub fn decode_publication(
                value: &wire::MetadataPublication,
            ) -> Result<(EffectFence, RuntimeMetadataSnapshot), ProtocolError> {
                if value.request == 0 || value.request_generation == 0 {
                    return Err(ProtocolError::Malformed);
                }
                let fence = decode_fence(required(&value.fence)?, None)?;
                let snapshot = decode_snapshot(required(&value.snapshot)?)?;
                if fence.session() != snapshot.session() {
                    return Err(ProtocolError::Malformed);
                }
                Ok((fence, snapshot))
            }
        }
    };
}
profile!(v1);
profile!(v2);
