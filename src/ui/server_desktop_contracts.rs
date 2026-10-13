//! The desktop's one import and conversion boundary for server-backed state.
//!
//! Server-backed windows name Workspaces, Tabs, Panes and Worktrees by their server-qualified core
//! identities and never map them onto the legacy per-collection numbers. Every token and revision
//! here is local comparison data. None is a grant, a fence, or native authority.

#![allow(
    dead_code,
    reason = "frozen contracts that server-backed Panes, Tabs and Workspaces consume once their providers exist"
)]

use std::sync::Arc;

use spaceterm_client::failure::{ClientFailure, ClientFailureClass};
use spaceterm_client::view::{AttachmentIncarnation, Measurement};
use spaceterm_core::geometry::SUPPORTED_GRID_LIMITS;
use thiserror::Error;

#[allow(
    unused_imports,
    reason = "server-backed Panes, Tabs and Workspaces consume these frozen names"
)]
pub(crate) use spaceterm_core::{
    authority::{ConnectionGeneration, ContextGeneration, OrganizationRevision, ServerEpoch},
    geometry::{GridSize, MeasuredGrid},
    identity::{ClientId, PaneId, ServerId, TabId, WorkspaceId, WorktreeId},
    operations::{Command, OperationId, Outcome},
    organization::{OrganizationSnapshot, PaneRecord, TabRecord, WorkspaceRecord},
};

#[allow(
    unused_imports,
    reason = "server-backed Panes, Tabs and Workspaces consume these frozen names"
)]
pub(crate) use spaceterm_client::{
    api::ConnectionState,
    view::{
        ArrangementEdit, ClientViewState, ExplicitUseSource, LayoutRevision, PaneArrangement,
        SelectionSource, TabStep, UseTicket, WorkspaceView,
    },
};

pub(crate) use super::pane_presentation::DisplayName;
pub(crate) use crate::ui::settings_window::SelectedServerAction;

/// One local attachment of a Workspace. Reattaching allocates a new one.
pub(crate) type AttachmentGeneration = AttachmentIncarnation;

/// One coherent publication for one server: its validated organization and this device's
/// reconciled personal view, taken from the same application update.
///
/// Consumers never combine a record from one frame with an arrangement from another. A retained
/// offline frame permits navigation and personal rearrangement, never shared operations.
pub(crate) struct DesktopServerFrame {
    pub(crate) server: ServerId,
    pub(crate) connection: ConnectionState,
    pub(crate) replica_status: ReplicaStatus,
    pub(crate) organization: Arc<OrganizationSnapshot>,
    pub(crate) personal: Arc<ClientViewState>,
    pub(crate) view_revision: DesktopViewRevision,
}

/// A paired server's bounded display identity. Reachability and display names confer no authority.
#[derive(Clone, Debug)]
pub(crate) struct DesktopServerInfo {
    pub(crate) server: ServerId,
    pub(crate) name: crate::ui::pane_presentation::DisplayName,
    pub(crate) local: bool,
}

/// How current a frame's organization is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReplicaStatus {
    /// The organization is the server's current publication.
    Ready,
    /// The client is replacing a stale organization with a current one.
    Resynchronizing,
    /// The server is unreachable and the frame keeps its last organization.
    RetainedOffline,
}

/// The incarnation of one Pane's presentation that a delayed callback was issued against.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PaneViewToken {
    pub(crate) pane: PaneId,
    pub(crate) attachment_generation: AttachmentGeneration,
    pub(crate) context_generation: ContextGeneration,
}

/// A Pane's admitted grid for one measurement request.
///
/// A server-backed Pane reports it for the requested ticket even when its grid did not change. It
/// supplies size only, never begins or authorizes a use, and is never sent to a server as a
/// startup source of its own. The grid is the canonical server measurement: it keeps its cells and
/// cell metrics even when a multiplied PTY pixel extent cannot fit u16.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PaneMeasurement {
    pub(crate) token: PaneViewToken,
    pub(crate) ticket: UseTicket,
    pub(crate) layout: LayoutRevision,
    pub(crate) grid: MeasuredGrid,
}

impl PaneMeasurement {
    /// The client-view measurement for the same ticket, context, layout and grid.
    ///
    /// Client-view measurements still carry legacy `GridSize`, which refuses a PTY pixel extent
    /// past u16. Such a grid fails as [`DesktopClientError::InvalidGeometry`] rather than being
    /// truncated or replaced.
    pub(crate) fn view_measurement(self) -> Result<Measurement, DesktopClientError> {
        let grid = GridSize::new(
            u32::from(self.grid.rows()),
            u32::from(self.grid.columns()),
            self.grid.cell_pixels(),
            SUPPORTED_GRID_LIMITS,
        )
        .map_err(|_| DesktopClientError::InvalidGeometry)?;
        Ok(Measurement {
            ticket: self.ticket,
            context: self.token.context_generation,
            layout: self.layout,
            grid,
        })
    }
}

/// The tags a Pane must return with one requested measurement. They grant no authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PaneMeasurementRequest {
    pub(crate) token: PaneViewToken,
    pub(crate) ticket: UseTicket,
    pub(crate) layout: LayoutRevision,
}

/// One local UI request. Client-core allocates and persists the durable operation identity
/// separately.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct UiRequestId(u64);

impl UiRequestId {
    pub(crate) const FIRST: Self = Self(1);

    /// Exhaustion rejects the request before any effect.
    pub(crate) fn checked_successor(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// The revision of the desktop's frame publications.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct DesktopViewRevision(u64);

impl DesktopViewRevision {
    pub(crate) const INITIAL: Self = Self(0);

    /// Exhaustion rejects the publication before any effect.
    pub(crate) fn checked_successor(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Why the desktop refused or could not finish a server-backed request. Each class carries no
/// terminal or clipboard contents, paths, environment values, credentials, or native errors.
///
/// `TargetGone`, `WrongServer`, `StaleContext`, `InvalidGeometry` and `Cancelled` show no Alert,
/// because the replica or the closing window already shows the current state. Every other class
/// names a condition a person can act on.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum DesktopClientError {
    #[error("the server is offline")]
    Offline,
    #[error("the server needs an update")]
    Incompatible,
    #[error("the server connection is stopped")]
    Stopped,
    #[error("the server is reconciling")]
    Reconciling,
    #[error("the target no longer exists")]
    TargetGone,
    /// The canonical server outcome, containing only a bounded typed failure class.
    #[error("the server rejected the shared operation")]
    SharedOperationRejected(spaceterm_core::operations::OperationFailure),
    #[error("the target belongs to another server")]
    WrongServer,
    #[error("the request context is stale")]
    StaleContext,
    /// This window's own bound of pending requests is full. Earlier requests are still finishing.
    #[error("too many requests are pending")]
    AdmissionFull,
    #[error("the measured geometry is invalid")]
    InvalidGeometry,
    #[error("local storage is unavailable")]
    LocalStorageUnavailable,
    #[error("authorization is unavailable")]
    AuthorizationUnavailable,
    #[error("a local counter is exhausted")]
    CounterExhausted,
    #[error("the capability is unsupported")]
    UnsupportedCapability,
    /// The client stopped the request before it finished, for example while it shut down or
    /// replaced the connection. Client-core retains any operation it had already submitted.
    #[error("the request was cancelled")]
    Cancelled,
    /// A client or server resource limit refused the request. Waiting does not necessarily help.
    #[error("a resource limit was reached")]
    LimitReached,
    /// The server did not prove the identity this device paired with.
    #[error("the server identity could not be verified")]
    IdentityUnverified,
    /// The server or this device's retained pairing state does not allow this device's use.
    #[error("this device is not allowed to use the server")]
    AccessDenied,
}

impl std::hash::Hash for DesktopClientError {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&std::mem::discriminant(self), state);
        if let Self::SharedOperationRejected(failure) = self {
            // Core failures are bounded classifications but do not implement Hash.
            // Hash their enum cases without formatting or allocating diagnostic text.
            std::hash::Hash::hash(&std::mem::discriminant(failure), state);
            if let spaceterm_core::operations::OperationFailure::Runtime(class) = failure {
                std::hash::Hash::hash(&std::mem::discriminant(class), state);
            }
        }
    }
}

impl From<ClientFailure> for DesktopClientError {
    fn from(failure: ClientFailure) -> Self {
        match failure.class() {
            ClientFailureClass::Unavailable
            | ClientFailureClass::InvalidPeer
            | ClientFailureClass::UnsafeEndpoint => Self::Offline,
            ClientFailureClass::Cancelled => Self::Cancelled,
            ClientFailureClass::AuthorizationDenied => Self::AccessDenied,
            ClientFailureClass::PinMismatch => Self::IdentityUnverified,
            ClientFailureClass::Incompatible => Self::Incompatible,
            ClientFailureClass::MissingCapability => Self::UnsupportedCapability,
            ClientFailureClass::StaleAuthority => Self::StaleContext,
            ClientFailureClass::TargetNotFound => Self::TargetGone,
            ClientFailureClass::ResourceLimit => Self::LimitReached,
            ClientFailureClass::StorageUnavailable
            | ClientFailureClass::UnsafeStorage
            | ClientFailureClass::InvalidStorage
            | ClientFailureClass::UnknownSchema
            | ClientFailureClass::RecoveryRequired => Self::LocalStorageUnavailable,
            ClientFailureClass::CounterExhausted => Self::CounterExhausted,
        }
    }
}

/// One intentional personal-view operation, applied through the matching client-view operation
/// against the frame it was chosen in. It is not another arrangement reducer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ViewEdit {
    ActivateWorkspace(WorkspaceId),
    ActivateTab {
        workspace: WorkspaceId,
        tab: TabId,
    },
    ActivateAdjacentTab {
        workspace: WorkspaceId,
        step: TabStep,
    },
    ActivateWorktree {
        workspace: WorkspaceId,
        worktree: Option<WorktreeId>,
    },
    MoveTab {
        workspace: WorkspaceId,
        tab: TabId,
        position: usize,
    },
    StepTab {
        workspace: WorkspaceId,
        tab: TabId,
        step: TabStep,
    },
    Arrange {
        workspace: WorkspaceId,
        tab: TabId,
        edit: ArrangementEdit,
    },
}

/// One request from a server-backed window. Its `Debug` output names the request kind only, so
/// Settings drafts never reach diagnostics.
#[derive(Clone, PartialEq)]
pub(crate) enum DesktopRequest {
    /// A shared organization command. Client-core submits it under a durable operation.
    Shared {
        server: ServerId,
        command: Command,
    },
    /// A person explicitly selected a Pane. Focus restoration never sends this.
    SelectPane {
        pane: PaneId,
        source: SelectionSource,
    },
    /// A person explicitly opened a Tab. Applying a publication never sends this.
    OpenTab {
        tab: TabId,
        source: SelectionSource,
    },
    /// A person chose Start Session on a Pane with no running Session.
    StartSession {
        pane: PaneId,
        source: SelectionSource,
    },
    /// A person deliberately retried a failed startup or an unavailable directory. It starts a
    /// Session the way Start Session does.
    Retry {
        pane: PaneId,
        source: SelectionSource,
    },
    /// A person chose Take Over on a Pane another Client controls. It asks for control and begins
    /// no explicit use.
    TakeOver {
        pane: PaneId,
        source: SelectionSource,
    },
    Measured(PaneMeasurement),
    AttachWorkspace(WorkspaceId),
    DetachWorkspace(WorkspaceId),
    /// Resumes only the original local detach transaction, including its pairing, attachment,
    /// and captured grants. If that transaction cannot be recovered, refuse instead of capturing
    /// the current attachment or grants. The UI request is correlation, not release authority.
    RetryDetachWorkspace {
        workspace: WorkspaceId,
        original_request: UiRequestId,
    },
    PersonalEdit(ViewEdit),
    /// One connection attempt. It never starts a server.
    RetryConnection(ServerId),
    ServerAction(SelectedServerAction),
}

impl std::fmt::Debug for DesktopRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Shared { .. } => "DesktopRequest::Shared",
            Self::SelectPane { .. } => "DesktopRequest::SelectPane",
            Self::OpenTab { .. } => "DesktopRequest::OpenTab",
            Self::StartSession { .. } => "DesktopRequest::StartSession",
            Self::Retry { .. } => "DesktopRequest::Retry",
            Self::TakeOver { .. } => "DesktopRequest::TakeOver",
            Self::Measured(_) => "DesktopRequest::Measured",
            Self::AttachWorkspace(_) => "DesktopRequest::AttachWorkspace",
            Self::DetachWorkspace(_) => "DesktopRequest::DetachWorkspace",
            Self::RetryDetachWorkspace { .. } => "DesktopRequest::RetryDetachWorkspace",
            Self::PersonalEdit(_) => "DesktopRequest::PersonalEdit",
            Self::RetryConnection(_) => "DesktopRequest::RetryConnection",
            Self::ServerAction(_) => "DesktopRequest::ServerAction",
        })
    }
}

impl DesktopRequest {
    /// The deliberate input behind a request a person made on one Pane or Tab.
    pub(crate) fn selection_source(&self) -> Option<SelectionSource> {
        match self {
            Self::SelectPane { source, .. }
            | Self::OpenTab { source, .. }
            | Self::StartSession { source, .. }
            | Self::Retry { source, .. }
            | Self::TakeOver { source, .. } => Some(*source),
            _ => None,
        }
    }

    /// The client-view explicit use this request begins. Take Over asks for control instead, and
    /// every other request begins none.
    pub(crate) fn explicit_use(&self) -> Option<ExplicitUseSource> {
        match self {
            Self::SelectPane { source, .. } => Some(ExplicitUseSource::PaneSelection(*source)),
            Self::OpenTab { source, .. } => Some(ExplicitUseSource::TabOpening(*source)),
            Self::StartSession { source, .. } => Some(ExplicitUseSource::StartSession(*source)),
            Self::Retry { source, .. } => Some(ExplicitUseSource::Retry(*source)),
            _ => None,
        }
    }
}

/// What a person chose on a server-backed Pane's presentation.
///
/// The Pane resolves the action against the presentation that offered it. A "Retry" on an
/// unreachable server's frozen screen is a [`Self::RetryConnection`]. A "Retry" on a failed
/// startup or an unavailable directory is a [`Self::Retry`]. The Pane's owner therefore never
/// infers the meaning from a later connection state. Only the actions that begin a use or ask for
/// control carry their selection source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaneAction {
    StartSession(SelectionSource),
    Retry(SelectionSource),
    TakeOver(SelectionSource),
    /// One connection attempt to the Pane's server. It never starts or bootstraps the server.
    RetryConnection,
    ChooseDirectory,
    OpenServerSettings,
}

/// Where the Pane's owner sends one [`PaneAction`].
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "the owner matches a route where it is made and never stores one"
)]
pub(crate) enum PaneActionRoute {
    /// The runtime request the action makes.
    Request(DesktopRequest),
    /// The owning server's directory chooser. Its repair applies to this Pane only.
    ChooseDirectory(PaneId),
    /// Settings > Servers with this server selected.
    ServerSettings(ServerId),
}

impl PaneAction {
    /// The route for this action on `pane`. Server targets are always `pane`'s own server.
    pub(crate) fn route(self, pane: PaneId) -> PaneActionRoute {
        match self {
            Self::StartSession(source) => {
                PaneActionRoute::Request(DesktopRequest::StartSession { pane, source })
            }
            Self::Retry(source) => PaneActionRoute::Request(DesktopRequest::Retry { pane, source }),
            Self::TakeOver(source) => {
                PaneActionRoute::Request(DesktopRequest::TakeOver { pane, source })
            }
            Self::RetryConnection => {
                PaneActionRoute::Request(DesktopRequest::RetryConnection(pane.server()))
            }
            Self::ChooseDirectory => PaneActionRoute::ChooseDirectory(pane),
            Self::OpenServerSettings => PaneActionRoute::ServerSettings(pane.server()),
        }
    }
}
