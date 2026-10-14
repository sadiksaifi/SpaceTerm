//! Engine-free runtime commands. Terminal-runtime Job A implements admission,
//! final-effect policy, worker ownership and cancellation, not these DTOs.

use crate::appearance::{Appearance, AppearanceGeneration};
use crate::pointer_input::{PointerButton, PointerPhase};
use crate::{InputModifiers, KeyInput};
use spaceterm_core::authority::EffectFence;
use spaceterm_core::content::{CellRef, Rgb};
use spaceterm_core::content_queries::QueryStamp;
use spaceterm_core::control::MeasuredStartupReservation;
use spaceterm_core::geometry::{CellPixels, MeasuredGrid};
use spaceterm_core::native_requests::{
    NativeCompletion, NativeText, PasteApprovalId, PasteDecision,
};
use spaceterm_core::organization::{BoundedText, ServerDirectory};
pub use spaceterm_core::runtime_authority::{
    AuthorityApplied, EffectDisposition, RuntimeAuthority, RuntimeAuthorityParts,
    RuntimeAuthorityTransition, RuntimeCompletion, RuntimeLifecycle, RuntimeTerminalStatus,
    SessionTerminalResult,
};

pub type BoundedInputMethodText = BoundedText<65536>;
pub type BoundedPaste = NativeText;
/// Cell-addressed application input, never client selection or a native pixel coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationPointerInput {
    pub stamp: QueryStamp,
    pub cell: CellRef,
    pub phase: PointerPhase,
    pub button: Option<PointerButton>,
    pub modifiers: InputModifiers,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationWheelInput {
    pub stamp: QueryStamp,
    pub cell: CellRef,
    pub horizontal_steps: i32,
    pub vertical_steps: i32,
    pub modifiers: InputModifiers,
}
/// Fixed palette bound. This is runtime color policy, not a client theme/font.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeAppearance {
    pub generation: AppearanceGeneration,
    pub foreground: Rgb,
    pub background: Rgb,
    pub palette: [Rgb; 256],
    pub bold_as_bright: bool,
    pub appearance: Appearance,
}
#[derive(Clone, PartialEq)]
pub enum RuntimeCommand {
    Key(KeyInput),
    /// Encode a complete Copy chord only through enhanced keyboard reporting.
    Copy(InputModifiers),
    InputMethodCommit(BoundedInputMethodText),
    Pointer(ApplicationPointerInput),
    Wheel(ApplicationWheelInput),
    Resize(MeasuredGrid),
    Appearance(Box<RuntimeAppearance>),
    FocusReport(bool),
    CellPixelMetrics(CellPixels),
    ClearScreenAndScrollback,
    RequestPaste(BoundedPaste),
    /// Assigned by the authenticated server adapter, never inferred from a native ID.
    RequestCorrelatedPaste(u64, BoundedPaste),
    ResolvePaste(PasteApprovalId, PasteDecision),
    CompleteNativeRequest(NativeCompletion),
}
impl std::fmt::Debug for RuntimeCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // KeyInput is also a legacy local API. Its Debug includes text; the
        // server command boundary must never inherit that diagnostic payload.
        f.write_str(match self {
            Self::Key(_) => "RuntimeCommand::Key",
            Self::Copy(_) => "RuntimeCommand::Copy",
            Self::InputMethodCommit(_) => "RuntimeCommand::InputMethodCommit",
            Self::Pointer(_) => "RuntimeCommand::Pointer",
            Self::Wheel(_) => "RuntimeCommand::Wheel",
            Self::Resize(_) => "RuntimeCommand::Resize",
            Self::Appearance(_) => "RuntimeCommand::Appearance",
            Self::FocusReport(_) => "RuntimeCommand::FocusReport",
            Self::CellPixelMetrics(_) => "RuntimeCommand::CellPixelMetrics",
            Self::ClearScreenAndScrollback => "RuntimeCommand::ClearScreenAndScrollback",
            Self::RequestPaste(_) => "RuntimeCommand::RequestPaste",
            Self::RequestCorrelatedPaste(..) => "RuntimeCommand::RequestCorrelatedPaste",
            Self::ResolvePaste(..) => "RuntimeCommand::ResolvePaste",
            Self::CompleteNativeRequest(_) => "RuntimeCommand::CompleteNativeRequest",
        })
    }
}
/// The captured effect fence must be rechecked by the actor and worker. It is
/// never rewritten to current authority after queuing or native completion.
#[derive(Clone, Debug, PartialEq)]
pub struct FencedRuntimeCommand {
    pub fence: EffectFence,
    pub command: RuntimeCommand,
}
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeStartRequest {
    reservation: MeasuredStartupReservation,
    directory: ServerDirectory,
    authority: RuntimeAuthority,
    appearance: RuntimeAppearance,
}
impl RuntimeStartRequest {
    pub fn new(
        reservation: MeasuredStartupReservation,
        directory: ServerDirectory,
        authority: RuntimeAuthority,
        appearance: RuntimeAppearance,
    ) -> Result<Self, spaceterm_core::authority::FenceError> {
        if authority.session() != reservation.session()
            || directory.server() != reservation.session().pane.server()
            || authority.parts().grant != Some(reservation.intent().fence().grant())
            || authority.parts().connection != Some(reservation.intent().fence().connection())
            || authority.parts().context != reservation.intent().fence().context()
        {
            return Err(spaceterm_core::authority::FenceError::WrongBinding);
        }
        Ok(Self {
            reservation,
            directory,
            authority,
            appearance,
        })
    }
    pub const fn reservation(&self) -> MeasuredStartupReservation {
        self.reservation
    }
    pub fn directory(&self) -> &ServerDirectory {
        &self.directory
    }
    pub const fn authority(&self) -> RuntimeAuthority {
        self.authority
    }
    pub fn appearance(&self) -> &RuntimeAppearance {
        &self.appearance
    }
}

/// Bounded account/machine spelling for presentation, never native authority.
pub type RuntimeMetadataIdentity = BoundedText<1024>;
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeMetadataOrigin {
    Local {
        user: Option<RuntimeMetadataIdentity>,
        host: Option<RuntimeMetadataIdentity>,
    },
    Remote {
        user: Option<RuntimeMetadataIdentity>,
        host: RuntimeMetadataIdentity,
    },
}
impl RuntimeMetadataOrigin {
    pub fn as_origin(&self) -> crate::metadata::TerminalOrigin<'_> {
        match self {
            Self::Local { user, host } => crate::metadata::TerminalOrigin::Local {
                user: user.as_ref().map(|value| value.as_str()),
                host: host.as_ref().map(|value| value.as_str()),
            },
            Self::Remote { user, host } => crate::metadata::TerminalOrigin::Remote {
                user: user.as_ref().map(|value| value.as_str()),
                host: host.as_str(),
            },
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RuntimeMetadataError {
    #[error("metadata Session is invalid")]
    InvalidSession,
    #[error("metadata title exceeds its bound or contains control characters")]
    InvalidTitle,
    #[error("metadata progress is outside the percentage range")]
    InvalidProgress,
}
/// Untrusted presentation facts. Title/progress/freshness/prompt vocabulary is
/// the existing metadata vocabulary; Prompt Owner and command text are absent.
#[derive(Clone, Eq, PartialEq)]
pub struct RuntimeMetadataParts {
    pub session: spaceterm_core::authority::SessionKey,
    pub sequence: spaceterm_core::observations::RuntimeObservationSequence,
    pub observed_at: spaceterm_core::observations::MonotonicTick,
    pub title: crate::metadata::TitleMetadata,
    pub origin: RuntimeMetadataOrigin,
    pub progress: crate::metadata::ProgressMetadata,
    pub freshness: crate::metadata::MetadataFreshness,
    pub prompt_zone: crate::metadata::PromptZone,
    pub hidden_input: bool,
}
/// Bounded retained metadata, independent of screen and directory delivery.
#[derive(Clone, Eq, PartialEq)]
pub struct RuntimeMetadataSnapshot(RuntimeMetadataParts);
impl RuntimeMetadataSnapshot {
    pub fn new(parts: RuntimeMetadataParts) -> Result<Self, RuntimeMetadataError> {
        use crate::metadata::ProgressMetadata;
        if parts.session.epoch.get() == 0 || parts.session.generation.get() == 0 {
            return Err(RuntimeMetadataError::InvalidSession);
        }
        if parts.title.value.chars().count() > 256
            || parts.title.value.chars().any(char::is_control)
        {
            return Err(RuntimeMetadataError::InvalidTitle);
        }
        if matches!(parts.progress, ProgressMetadata::Normal(value)
            | ProgressMetadata::Error(value) | ProgressMetadata::Paused(value) if value > 100)
        {
            return Err(RuntimeMetadataError::InvalidProgress);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &RuntimeMetadataParts {
        &self.0
    }
    pub const fn session(&self) -> spaceterm_core::authority::SessionKey {
        self.0.session
    }
}
impl std::fmt::Debug for RuntimeMetadataSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeMetadataSnapshot")
            .field("session", &self.0.session)
            .field("sequence", &self.0.sequence)
            .field("observed_at", &self.0.observed_at)
            .field("title_bytes", &self.0.title.value.len())
            .field("progress", &self.0.progress)
            .finish_non_exhaustive()
    }
}
