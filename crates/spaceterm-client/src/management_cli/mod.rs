//! Status-only companion access and deliberate, tracked CLI management.
pub mod bindings;
pub mod confirmation;
pub mod ports;
pub mod status;

pub use bindings::{
    AuthenticatedManagementContext, StatusOnlyManagement, TrackedProtocolManagement,
};
pub use confirmation::{
    AssessmentEvidence, AssessmentToken, ConfirmationPrompt, ConfirmationReply,
    ConfirmedManagementAction, ManagementAction, ManagementAssessment, confirm_exact,
};
pub use ports::{
    ManagementBackend, ManagementClientKind, ManagementFailure, ManagementFuture,
    ProtocolManagement, StatusBackend, StatusManagement,
};
pub use spaceterm_core::management::{ManagementProgress, ServerStatus};
pub use status::StatusSubscription;
