//! Client networking and Saved Connections for paired SpaceTerm Core Machines.

pub mod connection;
pub mod direct;
mod error;
pub mod service;

pub use error::{AuthenticatedStatus, CoreError};
pub use spaceterm_protocol::{attachments, hierarchy, pane, protocol, update, worktree};

pub mod admission;

mod paired;
mod wait;
pub mod work;

pub use paired::{DeviceName, PairedMachine};
pub use wait::WaitLimit;

pub mod credentials;
pub mod enrollment;
pub mod records;
mod routes;
mod store;

pub use credentials::{CredentialError, CredentialStore, Interaction};
pub(crate) use routes::random_id;
#[cfg(any(test, feature = "test-support"))]
pub use routes::tests;
pub use routes::{
    AccountRevocation, DirectConnections, DirectRoute, DirectRouteId, DirectRouteStatus,
    EnrollmentStage, JoinId, JoinStatus, PendingUnpair, RefusalReason, RouteCleanup,
    SetupEnrollment, UnavailableReason, UnpairKey, UnpairProgress, setup,
};
pub use store::{
    ConnectionStorage, ConnectionStore, ConnectionTransaction, PendingEnrollment,
    PendingRevocation, RecoveryGuard, RemovalConfirmation, SavedConnection, SavedConnectionChange,
    StoreError, StoreKind, UnpairState, readable, valid_name,
};

pub mod workspace;
