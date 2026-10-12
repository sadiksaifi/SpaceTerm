//! Management uses the connection worker and its one durable operation tracker.

use std::{future::Future, pin::Pin};

use serde::Serialize;
use spaceterm_core::{
    identity::{OperationStreamId, ServerId},
    management::{ManagementProgress, ServerStatus},
    operations::{Command, OperationId},
};

use super::{
    confirmation::{
        AssessmentToken, ConfirmedManagementAction, ManagementAction, ManagementAssessment,
    },
    status::StatusSubscription,
};

pub type ManagementFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ManagementFailure>> + Send + 'a>>;

/// Content-free classifications shared by the CLI and status companion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ManagementFailure {
    #[error("invalid invocation")]
    Usage,
    #[error("exact confirmation is required")]
    ConfirmationRequired,
    #[error("assessment is stale")]
    StaleAssessment,
    #[error("pairing is required")]
    PairingRequired,
    #[error("pairing was revoked")]
    Revoked,
    #[error("authorization was denied")]
    AuthorizationDenied,
    #[error("authorization is unavailable")]
    AuthorizationUnavailable,
    #[error("host is unsupported")]
    UnsupportedHost,
    #[error("native capability is unavailable")]
    NativeUnavailable,
    #[error("credential is unavailable")]
    CredentialUnavailable,
    #[error("storage is locked")]
    StorageLocked,
    #[error("storage is unavailable")]
    StorageUnavailable,
    #[error("native or durable state is unsafe")]
    UnsafeNativeState,
    #[error("protocol is incompatible")]
    Incompatible,
    #[error("resource limit exceeded")]
    LimitExceeded,
    #[error("original outcome remains pending")]
    OutcomePending,
    #[error("durability is indeterminate")]
    Indeterminate,
    #[error("server is offline")]
    Offline,
    #[error("input or output is unavailable")]
    IoUnavailable,
}

impl ManagementFailure {
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Usage
            | Self::ConfirmationRequired
            | Self::StaleAssessment
            | Self::LimitExceeded => 2,
            Self::UnsupportedHost
            | Self::NativeUnavailable
            | Self::AuthorizationUnavailable
            | Self::Incompatible => 69,
            Self::OutcomePending
            | Self::Indeterminate
            | Self::StorageLocked
            | Self::StorageUnavailable
            | Self::CredentialUnavailable
            | Self::Offline
            | Self::IoUnavailable => 75,
            Self::PairingRequired | Self::Revoked | Self::AuthorizationDenied => 77,
            Self::UnsafeNativeState => 70,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementClientKind {
    Cli,
    StatusCompanion,
}

pub trait ProtocolManagement: Send + Sync {
    /// Read the current authenticated negotiation, never a native process observation.
    fn negotiated_major(&self) -> Result<u32, ManagementFailure>;
    fn status(&self) -> ManagementFuture<'_, ServerStatus>;
    fn assess(&self, action: ManagementAction) -> ManagementFuture<'_, ManagementAssessment>;
    /// Retrieve the original server-held assessment. Never create a replacement
    /// assessment for a supplied token, or interpret that token as authority.
    fn restore_assessment(
        &self,
        action: ManagementAction,
        token: AssessmentToken,
    ) -> ManagementFuture<'_, ManagementAssessment>;
    /// Ordinary tracked mutations only. Self-unpair uses the pairing journal's
    /// marker-first removal and purpose-restricted task, never this submission path.
    fn execute(
        &self,
        confirmed: ConfirmedManagementAction,
    ) -> ManagementFuture<'_, ManagementProgress>;
    fn resolve(&self, operation: OperationId) -> ManagementFuture<'_, ManagementProgress>;
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription>;
}

/// Inject the real client-core worker here, not another connection, journal,
/// allocator or retry loop. Ordinary readiness must be checked on every call.
pub trait ManagementBackend: Send + Sync {
    fn context(&self)
    -> Result<super::bindings::AuthenticatedManagementContext, ManagementFailure>;
    fn status(&self) -> ManagementFuture<'_, ServerStatus>;
    fn assess(&self, action: ManagementAction) -> ManagementFuture<'_, ManagementAssessment>;
    fn restore_assessment(
        &self,
        action: ManagementAction,
        token: AssessmentToken,
    ) -> ManagementFuture<'_, ManagementAssessment>;
    /// The tracker persists the canonical command and one original OperationId
    /// before sending. It records the outcome durably before acknowledgement.
    /// Loss or cancellation after submission retains that same identity.
    fn submit(&self, command: Command) -> ManagementFuture<'_, ManagementProgress>;
    fn resolve(&self, operation: OperationId) -> ManagementFuture<'_, ManagementProgress>;
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription>;
}

/// The status companion's only management surface. It has no assessment,
/// submission or resolution path, so passive status cannot mutate a server.
pub trait StatusManagement: Send + Sync {
    fn status(&self) -> ManagementFuture<'_, ServerStatus>;
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription>;
}

/// Inject the client-core worker's status queries for the companion's own
/// Client binding. Unlike [`ManagementBackend`] it cannot submit a command.
pub trait StatusBackend: Send + Sync {
    fn context(&self)
    -> Result<super::bindings::AuthenticatedManagementContext, ManagementFailure>;
    fn status(&self) -> ManagementFuture<'_, ServerStatus>;
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription>;
}

pub(crate) fn check_operation(
    operation: OperationId,
    stream: OperationStreamId,
) -> Result<(), ManagementFailure> {
    if operation.stream() != stream {
        Err(ManagementFailure::UnsafeNativeState)
    } else {
        Ok(())
    }
}
pub(crate) fn check_server(server: ServerId, expected: ServerId) -> Result<(), ManagementFailure> {
    if server.bytes() == [0; 16] || server != expected {
        Err(ManagementFailure::UnsafeNativeState)
    } else {
        Ok(())
    }
}
