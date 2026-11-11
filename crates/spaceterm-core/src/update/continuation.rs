//! How a Pending Version finishes, read from the update facts every released Core sends.
//!
//! A Core whose service lane offers the detailed update facts of work and service minor
//! [`super::facts::MINOR_DETAILED_FACTS`] reports its engine's own [`Continuation`], which takes
//! precedence over [`continuation`].
use super::facts::{FailureKind, UpdateFacts};
use serde::{Deserialize, Serialize};

/// How a Pending Version finishes. JSON: `"soon"`, `"when_sessions_end"` and so on. A minor may
/// only append variants after the existing ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Continuation {
    /// No live Terminal Session: SpaceTerm Core restarts into it within its grace period.
    Soon,
    /// SpaceTerm Core restarts into it by itself once no Terminal Session is live.
    WhenSessionsEnd,
    /// SpaceTerm Core restarts into it by itself once no Terminal Session is live, or once no
    /// live Terminal Session has had work going on for the idle period. Only the detailed
    /// update facts report it.
    WhenNothingRuns,
    /// A work or service major changes: it restarts only when a person confirms it.
    NeedsConfirmation,
    /// A start into it failed: SpaceTerm Core does not try it again by itself in this boot.
    StartFailed,
    /// A newer install failure replaced the failure the facts would show, so they cannot tell
    /// whether a start into it failed.
    Unknown,
}

/// How the Pending Version in `facts` finishes, by the silent-restart rule of every released
/// Core. None without a Pending Version. Never returns [`Continuation::WhenNothingRuns`].
pub fn continuation(facts: &UpdateFacts) -> Option<Continuation> {
    let pending = facts.pending.as_ref()?;
    if pending.breaking {
        return Some(Continuation::NeedsConfirmation);
    }
    match &facts.last_failure {
        Some(failure)
            if matches!(failure.kind, FailureKind::Start(_))
                && failure.version == pending.version =>
        {
            return Some(Continuation::StartFailed);
        }
        Some(failure) if matches!(failure.kind, FailureKind::Install(_)) => {
            return Some(Continuation::Unknown);
        }
        _ => {}
    }
    Some(if facts.sessions == 0 {
        Continuation::Soon
    } else {
        Continuation::WhenSessionsEnd
    })
}
