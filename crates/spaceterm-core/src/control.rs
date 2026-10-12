//! Control and explicit-use intent contracts. Measurement never creates work.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::{ControlGeneration, ControlGrant, EffectFence, ServerEpoch};
use crate::geometry::{GridSize, MeasuredGrid};
use crate::identity::{ClientId, PaneId, TabId};
use crate::operations::OperationId;

/// Compare-acquire is valid only while the Pane is unclaimed at this exact stamp.
/// A losing comparison never implicitly becomes Take Over.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompareAcquireRequest {
    pub epoch: ServerEpoch,
    pub pane: PaneId,
    pub client: ClientId,
    pub expected_generation: ControlGeneration,
}
/// An explicit user action replacing control. The reducer orders it atomically.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TakeOverRequest {
    pub epoch: ServerEpoch,
    pub pane: PaneId,
    pub client: ClientId,
}
/// Detach can defer this request offline. A later grant or runtime cannot match.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReleaseControlRequest {
    pub epoch: ServerEpoch,
    pub grant: ControlGrant,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum StartupSource {
    ExplicitSelection,
    TabOpening { tab: TabId },
    StartSession,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum StartupError {
    #[error("startup intent belongs to another Client or server")]
    WrongBinding,
    #[error("startup reservation does not bind its measured explicit intent")]
    InvalidReservation,
}

/// Cancellable explicit intent retained until measured dimensions arrive.
/// Reconnect, rendering, focus restoration and Settings closure supply no intent.
/// The intent identity is the BeginStartup operation identity. Completion and
/// cancellation use new operation identities while referring to that original ID.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "IntentParts", into = "IntentParts")]
pub struct StartupIntent {
    id: OperationId,
    fence: EffectFence,
    source: StartupSource,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
struct IntentParts {
    id: OperationId,
    fence: EffectFence,
    source: StartupSource,
}
impl StartupIntent {
    pub fn new(
        id: OperationId,
        fence: EffectFence,
        source: StartupSource,
    ) -> Result<Self, StartupError> {
        if id.stream().client() != fence.grant().client()
            || id.stream().server() != fence.session().pane.server()
            || matches!(source, StartupSource::TabOpening { tab } if tab.server() != id.stream().server())
        {
            return Err(StartupError::WrongBinding);
        }
        Ok(Self { id, fence, source })
    }
    pub const fn id(self) -> OperationId {
        self.id
    }
    pub const fn fence(self) -> EffectFence {
        self.fence
    }
    pub const fn source(self) -> StartupSource {
        self.source
    }

    /// Comparison only. The server must still hold this uncancelled intent and
    /// current control, revalidate the directory and reserve aggregate grid memory.
    pub fn matches_measurement(self, measurement: &StartupMeasurement) -> bool {
        self.id == measurement.intent && self.fence.matches(&measurement.fence)
    }
}
impl TryFrom<IntentParts> for StartupIntent {
    type Error = StartupError;
    fn try_from(value: IntentParts) -> Result<Self, Self::Error> {
        Self::new(value.id, value.fence, value.source)
    }
}
impl From<StartupIntent> for IntentParts {
    fn from(value: StartupIntent) -> Self {
        Self {
            id: value.id,
            fence: value.fence,
            source: value.source,
        }
    }
}

/// Completes an existing explicit intent. There is deliberately no bare geometry
/// startup operation and no provisional 80 x 24 default.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartupMeasurement {
    pub intent: OperationId,
    pub fence: EffectFence,
    pub grid: GridSize,
}

/// Additive measured completion. Existing StartupMeasurement and its retained
/// Command representation remain unchanged.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MeasuredStartupMeasurement {
    pub intent: OperationId,
    pub fence: EffectFence,
    pub grid: MeasuredGrid,
}
impl From<StartupMeasurement> for MeasuredStartupMeasurement {
    fn from(value: StartupMeasurement) -> Self {
        Self {
            intent: value.intent,
            fence: value.fence,
            grid: value.grid.into(),
        }
    }
}

/// Comparison facts for measured explicit startup after the actor has committed
/// StartupReserved. This record is not authorization and must be accepted only
/// from the serialized startup registry with its current admitted reservation.
/// The intent/measurement fence names the old generation; session names the new
/// published Starting generation. No client may manufacture a reserved launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeasuredStartupReservation {
    intent: StartupIntent,
    measurement: MeasuredStartupMeasurement,
    session: crate::authority::SessionKey,
}
impl MeasuredStartupReservation {
    pub fn new(
        intent: StartupIntent,
        measurement: impl Into<MeasuredStartupMeasurement>,
        session: crate::authority::SessionKey,
    ) -> Result<Self, StartupError> {
        let measurement = measurement.into();
        let expected = intent.fence().session();
        if intent.id() != measurement.intent
            || !intent.fence().matches(&measurement.fence)
            || session.epoch.get() == 0
            || session.pane != expected.pane
            || session.epoch != expected.epoch
            || expected.generation.checked_successor() != Some(session.generation)
        {
            return Err(StartupError::InvalidReservation);
        }
        Ok(Self {
            intent,
            measurement,
            session,
        })
    }
    pub const fn intent(self) -> StartupIntent {
        self.intent
    }
    pub const fn measurement(self) -> MeasuredStartupMeasurement {
        self.measurement
    }
    pub const fn expected_session(self) -> crate::authority::SessionKey {
        self.intent.fence().session()
    }
    pub const fn session(self) -> crate::authority::SessionKey {
        self.session
    }
    pub const fn grid(self) -> MeasuredGrid {
        self.measurement.grid
    }
}
