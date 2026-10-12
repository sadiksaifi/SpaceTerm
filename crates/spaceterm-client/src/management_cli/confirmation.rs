//! Confirmation binds comparison facts. The server still revalidates them.

use std::fmt;

use spaceterm_core::{
    authority::{FieldVersion, ServerEpoch},
    identity::ServerId,
    observations::MonotonicTick,
    operations::{AssessmentTarget, CloseAssessment, Command},
    pairing::{EnrollmentProposal, PairingCommand, PairingTarget, UnpairAssessment},
    remote_access::RemoteAccessPolicy,
};

use super::ports::ManagementFailure;

pub const MAX_ASSESSMENT_BYTES: usize = 8192;
pub const ASSESSMENT_LIFETIME_MILLIS: u64 = 30_000;
pub const MAX_CONFIRMATION_LINE_BYTES: usize = 256;

/// An opaque, non-secret lookup token. Parsing grants no authority. Providers
/// retain its exact scope, original expiry and authenticated principal.
#[derive(Clone, Eq, PartialEq)]
pub struct AssessmentToken(String);
impl AssessmentToken {
    pub fn new(value: String) -> Result<Self, ManagementFailure> {
        if value.len() > MAX_ASSESSMENT_BYTES {
            return Err(ManagementFailure::LimitExceeded);
        }
        if value.is_empty()
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(ManagementFailure::Usage);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for AssessmentToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AssessmentToken(..)")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManagementAction {
    EndAllSessions { server: ServerId },
    Unpair { target: PairingTarget },
    RemoteAccess { policy: RemoteAccessPolicy },
    AuthorizeEnrollment { proposal: EnrollmentProposal },
}
impl ManagementAction {
    pub fn server(&self) -> ServerId {
        match self {
            Self::EndAllSessions { server } => *server,
            Self::Unpair { target } => target.parts().server,
            Self::RemoteAccess { policy } => policy.server(),
            Self::AuthorizeEnrollment { proposal } => proposal.parts().server,
        }
    }
    pub const fn name(&self) -> &'static str {
        match self {
            Self::EndAllSessions { .. } => "end_all_sessions",
            Self::Unpair { .. } => "unpair",
            Self::RemoteAccess { .. } => "remote_access",
            Self::AuthorizeEnrollment { .. } => "authorize_enrollment",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssessmentEvidence {
    EndAllSessions(CloseAssessment),
    Unpair(UnpairAssessment),
    RemoteAccess { current_version: FieldVersion },
    Enrollment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagementAssessment {
    token: AssessmentToken,
    action: ManagementAction,
    evidence: AssessmentEvidence,
    issued: MonotonicTick,
    expires: MonotonicTick,
}
impl ManagementAssessment {
    /// Structural validation only. Exact hierarchy remains in the owning
    /// server's assessment registry, not in a client-created destructive record.
    pub fn new(
        token: AssessmentToken,
        action: ManagementAction,
        evidence: AssessmentEvidence,
        issued: MonotonicTick,
        expires: MonotonicTick,
    ) -> Result<Self, ManagementFailure> {
        let valid = match (&action, &evidence) {
            (
                ManagementAction::EndAllSessions { server },
                AssessmentEvidence::EndAllSessions(value),
            ) => value.summary().target() == AssessmentTarget::EndAllSessions(*server),
            (ManagementAction::Unpair { target }, AssessmentEvidence::Unpair(value)) => {
                &value.parts().target == target
            }
            (
                ManagementAction::RemoteAccess { .. },
                AssessmentEvidence::RemoteAccess { current_version },
            ) => current_version.get() != 0,
            (
                ManagementAction::AuthorizeEnrollment { proposal },
                AssessmentEvidence::Enrollment,
            ) => proposal.validate().is_ok(),
            _ => false,
        };
        if !valid || action.server().bytes() == [0; 16] {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        if expires.get() <= issued.get()
            || expires.get() - issued.get() > ASSESSMENT_LIFETIME_MILLIS
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        Ok(Self {
            token,
            action,
            evidence,
            issued,
            expires,
        })
    }
    pub fn token(&self) -> &AssessmentToken {
        &self.token
    }
    pub fn action(&self) -> &ManagementAction {
        &self.action
    }
    pub fn evidence(&self) -> &AssessmentEvidence {
        &self.evidence
    }
    pub fn epoch(&self) -> Option<ServerEpoch> {
        match self.evidence {
            AssessmentEvidence::EndAllSessions(value) => Some(value.epoch()),
            _ => None,
        }
    }
    pub fn validate_at(&self, now: MonotonicTick) -> Result<(), ManagementFailure> {
        if now.get() < self.issued.get() || now.get() >= self.expires.get() {
            Err(ManagementFailure::StaleAssessment)
        } else {
            Ok(())
        }
    }
    pub fn command(&self) -> Command {
        match (&self.action, &self.evidence) {
            (
                ManagementAction::EndAllSessions { server },
                AssessmentEvidence::EndAllSessions(value),
            ) => Command::EndAllSessions {
                server: *server,
                assessment: value.reference(),
            },
            (ManagementAction::Unpair { .. }, AssessmentEvidence::Unpair(value)) => {
                Command::Pairing(PairingCommand::UnpairConfirmed {
                    assessment: value.clone(),
                })
            }
            (
                ManagementAction::RemoteAccess { policy },
                AssessmentEvidence::RemoteAccess { current_version },
            ) => Command::SetRemoteAccess {
                server: policy.server(),
                expected_version: *current_version,
                value: policy.clone(),
            },
            (
                ManagementAction::AuthorizeEnrollment { proposal },
                AssessmentEvidence::Enrollment,
            ) => Command::Pairing(PairingCommand::IssueDirectAuthorization {
                proposal: proposal.clone(),
            }),
            _ => unreachable!("checked assessment action/evidence pairing"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmationPrompt {
    assessment: ManagementAssessment,
}
impl ConfirmationPrompt {
    pub fn new(assessment: ManagementAssessment) -> Self {
        Self { assessment }
    }
    pub fn assessment(&self) -> &ManagementAssessment {
        &self.assessment
    }
    /// A controlling-terminal reply confirms only this displayed assessment.
    pub fn reply(&self, line: &[u8]) -> Result<ConfirmationReply, ManagementFailure> {
        if line.len() > MAX_CONFIRMATION_LINE_BYTES {
            return Err(ManagementFailure::LimitExceeded);
        }
        if line != b"confirm" {
            return Err(ManagementFailure::ConfirmationRequired);
        }
        Ok(ConfirmationReply::new(self.assessment.token.clone()))
    }
}
#[derive(Debug)]
pub struct ConfirmationReply {
    token: AssessmentToken,
}
impl ConfirmationReply {
    pub fn new(token: AssessmentToken) -> Self {
        Self { token }
    }
}
/// Noncloneable deliberate confirmation, not server or native authority.
#[derive(Debug)]
pub struct ConfirmedManagementAction {
    assessment: ManagementAssessment,
}
impl ConfirmedManagementAction {
    pub fn assessment(&self) -> &ManagementAssessment {
        &self.assessment
    }
}
pub fn confirm_exact(
    assessment: ManagementAssessment,
    reply: ConfirmationReply,
) -> Result<ConfirmedManagementAction, ManagementFailure> {
    if assessment.token != reply.token {
        return Err(ManagementFailure::ConfirmationRequired);
    }
    Ok(ConfirmedManagementAction { assessment })
}
