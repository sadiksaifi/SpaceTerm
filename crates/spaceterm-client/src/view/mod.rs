//! Portable client-owned view: personal Tab order, Pane arrangements, Split
//! ratios, activation, focus and zoom, explicit-use choreography and Pane
//! presentation policy.
//!
//! Shared membership, Root Tabs and Root Panes stay authoritative in the replica.
//! Nothing in this module creates shared organization. Only an explicit-use
//! intent requests control acquisition or a measured startup, as effects that
//! client-core validates and executes; reconciliation, rendering, restored
//! focus and reconnect never do.

mod arrangement;
mod persistence;
mod presentation;
mod replica_bridge;
mod startup;
mod state;
mod types;

pub use arrangement::{
    ArrangementChange, ArrangementMetrics, ArrangementNode, ArrangementRecord, CreationLayout,
    CreationNode, FocusDirection, PaneArrangement, PaneEdge, PaneNodeRef, PanePlacement, PaneSize,
    PaneTreeRef, TabOrder, TabOrderChange, TabOrderRecord, TabStep, ZoomState,
};
pub use persistence::{
    EncodedView, PersistedView, PersistedViewParts, PersistedWorkspace, StoredView,
    VIEW_FORMAT_VERSION, ViewCommit, ViewPersistence, ViewStore, ViewStoreFailure,
    ViewStoreFailureClass, ViewStoreStage,
};
pub use presentation::{
    ContentDisposition, OfflinePrior, PanePresentation, PresentationDecision,
    PresentationEligibility, PresentationFacts, PresentationState,
};
pub use replica_bridge::{ReplicaFacts, ReplicaObservation, ReplicaReadiness, ServerViewStatus};
pub use startup::{
    AuthorizedUse, ExplicitUseKind, ExplicitUseSource, IntentCancelReason, Measurement, PaneUse,
    SelectionSource, SelectionTarget, StartupEffect, StartupIntentId, StartupIntents,
    StartupReduction, StartupState, UseContext, UseFacts, UseKind,
};
pub use state::{
    ActivationCause, ArrangementEdit, AttachmentRemoval, AttachmentSuppression, ClientViewState,
    DetachPlan, InvalidationReason, ProjectionInvalidation, RestorationFilter, ViewAvailability,
    ViewChange, ViewUpdate, WorkspaceAttachment, WorkspaceView,
};
pub use types::{
    AttachmentIncarnation, LayoutRevision, MAX_TAB_PANES, MAX_VIEW_ATTACHMENTS,
    MAX_VIEW_DOCUMENT_BYTES, MAX_VIEW_PANES, MAX_VIEW_TABS, SplitId, UseTicket, ViewCounter,
    ViewError, ViewResource, ViewRevision, WorkspaceKey,
};

#[cfg(test)]
mod tests;
