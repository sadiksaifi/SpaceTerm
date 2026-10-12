//! Native-provider request comparisons. These values carry neither native
//! authorization nor pairing identity. The provider owns request admission and
//! its management journal; OrganizationStore owns accepted Session retirement.

use crate::identity::ServerId;
use crate::operations::RequestDigest;
use crate::retirement::RetirementRecordError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeManagementRequestParts {
    pub server: ServerId,
    /// Exact provider NativeRequestId::as_bytes(), never a surrogate UUID.
    pub id: [u8; 16],
    pub digest: RequestDigest,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "NativeManagementRequestParts",
    into = "NativeManagementRequestParts"
)]
pub struct NativeManagementRequestRef(NativeManagementRequestParts);
impl NativeManagementRequestRef {
    pub fn new(
        server: ServerId,
        id: [u8; 16],
        digest: RequestDigest,
    ) -> Result<Self, RetirementRecordError> {
        if id == [0; 16] {
            return Err(RetirementRecordError::InvalidBinding);
        }
        Ok(Self(NativeManagementRequestParts { server, id, digest }))
    }
    pub const fn server(self) -> ServerId {
        self.0.server
    }
    pub const fn id_bytes(self) -> [u8; 16] {
        self.0.id
    }
    pub const fn digest(self) -> RequestDigest {
        self.0.digest
    }
}
impl TryFrom<NativeManagementRequestParts> for NativeManagementRequestRef {
    type Error = RetirementRecordError;
    fn try_from(parts: NativeManagementRequestParts) -> Result<Self, Self::Error> {
        Self::new(parts.server, parts.id, parts.digest)
    }
}
impl From<NativeManagementRequestRef> for NativeManagementRequestParts {
    fn from(value: NativeManagementRequestRef) -> Self {
        value.0
    }
}
/// Restart of the approved installation drains Sessions with Stop semantics.
/// Only staged-version activation carries the ServerUpdate retirement reason.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NativeRetirementAction {
    Stop,
    RestartApproved,
    ActivateUpdate,
}
impl NativeRetirementAction {
    pub const fn reason(self) -> crate::organization::RetirementReason {
        match self {
            Self::Stop | Self::RestartApproved => crate::organization::RetirementReason::Stop,
            Self::ActivateUpdate => crate::organization::RetirementReason::ServerUpdate,
        }
    }
}
