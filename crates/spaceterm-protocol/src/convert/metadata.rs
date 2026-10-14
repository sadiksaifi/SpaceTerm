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

/// The largest valid snapshot contains three 1024-byte display fields plus
/// bounded session and scalar facts. No transfer reserves more than this cap.
pub const MAX_SNAPSHOT_BYTES: usize = 4096;

pub fn snapshot_bytes(snapshot: &RuntimeMetadataSnapshot) -> Result<Vec<u8>, ProtocolError> {
    use prost::Message;
    let encoded = encode_snapshot(snapshot).encode_to_vec();
    if encoded.len() > MAX_SNAPSHOT_BYTES {
        return Err(ProtocolError::LimitExceeded);
    }
    Ok(encoded)
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FragmentIdentity {
    fence: spaceterm_core::authority::EffectFence,
    request: u64,
    generation: u64,
    sequence: u64,
    total_bytes: usize,
}

/// Borrowed, shape-checked fragment. Only the completed snapshot is a display fact.
pub struct MetadataFragment<'a> {
    identity: FragmentIdentity,
    offset: usize,
    data: &'a [u8],
}
impl MetadataFragment<'_> {
    pub fn fence(&self) -> spaceterm_core::authority::EffectFence {
        self.identity.fence
    }
}
struct PendingMetadata {
    identity: FragmentIdentity,
    bytes: Vec<u8>,
}

/// One ordered, bounded transfer per authenticated connection. A new first
/// fragment replaces an interrupted transfer. No partial title is published.
#[derive(Default)]
pub struct MetadataAssembler {
    pending: Option<PendingMetadata>,
}
impl MetadataAssembler {
    pub fn receive(
        &mut self,
        fragment: MetadataFragment<'_>,
    ) -> Result<
        Option<(
            spaceterm_core::authority::EffectFence,
            RuntimeMetadataSnapshot,
        )>,
        ProtocolError,
    > {
        let result = self.receive_checked(fragment);
        if result.is_err() {
            self.pending = None;
        }
        result
    }
    fn receive_checked(
        &mut self,
        fragment: MetadataFragment<'_>,
    ) -> Result<
        Option<(
            spaceterm_core::authority::EffectFence,
            RuntimeMetadataSnapshot,
        )>,
        ProtocolError,
    > {
        use prost::Message;
        if fragment.offset == 0 {
            self.pending = Some(PendingMetadata {
                identity: fragment.identity,
                bytes: Vec::with_capacity(fragment.identity.total_bytes),
            });
        }
        let pending = self.pending.as_mut().ok_or(ProtocolError::Malformed)?;
        if pending.identity != fragment.identity || pending.bytes.len() != fragment.offset {
            return Err(ProtocolError::Malformed);
        }
        pending.bytes.extend_from_slice(fragment.data);
        if pending.bytes.len() != pending.identity.total_bytes {
            return Ok(None);
        }
        let pending = self.pending.take().ok_or(ProtocolError::Malformed)?;
        crate::preflight::check(
            crate::preflight::SPACETERM_COMMON_RUNTIMEMETADATASNAPSHOT,
            &pending.bytes,
            crate::limits::NegotiatedLimits::default(),
        )?;
        let snapshot = common::RuntimeMetadataSnapshot::decode(pending.bytes.as_slice())
            .map_err(|_| ProtocolError::Malformed)?;
        let snapshot = decode_snapshot(&snapshot)?;
        if snapshot.session() != pending.identity.fence.session()
            || snapshot.parts().sequence.get() != pending.identity.sequence
        {
            return Err(ProtocolError::Malformed);
        }
        Ok(Some((pending.identity.fence, snapshot)))
    }
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
            pub fn encode_fragment(
                fence: EffectFence,
                request: u64,
                generation: u64,
                snapshot: &RuntimeMetadataSnapshot,
                offset: u32,
                bytes: &[u8],
                total_bytes: u32,
            ) -> Result<wire::MetadataFragment, ProtocolError> {
                if fence.content().is_some() || snapshot.session() != fence.session() {
                    return Err(ProtocolError::Malformed);
                }
                let value = wire::MetadataFragment {
                    fence: Some(encode_fence(fence)),
                    request,
                    request_generation: generation,
                    sequence: snapshot.parts().sequence.get(),
                    offset,
                    total_bytes,
                    data: bytes.to_vec(),
                };
                decode_fragment(&value)?;
                Ok(value)
            }
            pub fn decode_fragment(
                value: &wire::MetadataFragment,
            ) -> Result<MetadataFragment<'_>, ProtocolError> {
                let fence = decode_fence(required(&value.fence)?, None)?;
                let total_bytes = value.total_bytes as usize;
                let offset = value.offset as usize;
                if value.request == 0
                    || value.request_generation == 0
                    || value.sequence == 0
                    || total_bytes == 0
                    || total_bytes > MAX_SNAPSHOT_BYTES
                    || value.data.is_empty()
                    || offset >= total_bytes
                    || value.data.len() > total_bytes - offset
                {
                    return Err(ProtocolError::Malformed);
                }
                Ok(MetadataFragment {
                    identity: FragmentIdentity {
                        fence,
                        request: value.request,
                        generation: value.request_generation,
                        sequence: value.sequence,
                        total_bytes,
                    },
                    offset,
                    data: &value.data,
                })
            }
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
