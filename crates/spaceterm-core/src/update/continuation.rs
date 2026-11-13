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
    /// update facts can report it. Admission uses the revocable lease and final Stop grant of ADR 0065.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::facts::{
        InstallFailure, Platform, Protocols, StartFailure, UpdateCheck, UpdateFailure, Version,
    };

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn facts(sessions: u32, breaking: bool, failure: Option<(&str, FailureKind)>) -> UpdateFacts {
        UpdateFacts {
            revision: 1,
            platform: Platform::HOST,
            running: version("0.11.3"),
            pending: Some(super::super::facts::PendingVersion {
                version: version("0.11.4"),
                protocols: Protocols::CURRENT,
                breaking,
                installed_at: 1,
                overdue_since: None,
                stranded: Vec::new(),
            }),
            sessions,
            operation: None,
            check: UpdateCheck::Never,
            skipped: None,
            last_failure: failure.map(|(text, kind)| UpdateFailure {
                id: 1,
                at: 1,
                version: version(text),
                kind,
            }),
        }
    }

    #[test]
    fn a_waiting_version_restarts_soon_without_sessions_and_once_they_end_with_them() {
        assert_eq!(
            continuation(&facts(0, false, None)),
            Some(Continuation::Soon)
        );
        assert_eq!(
            continuation(&facts(2, false, None)),
            Some(Continuation::WhenSessionsEnd)
        );
    }

    #[test]
    fn a_breaking_version_needs_confirmation_whatever_else_holds() {
        let start = FailureKind::Start(StartFailure::PortInUse);
        assert_eq!(
            continuation(&facts(0, true, Some(("0.11.4", start)))),
            Some(Continuation::NeedsConfirmation)
        );
    }

    #[test]
    fn a_start_failure_of_the_waiting_version_is_not_retried_and_of_another_is_ignored() {
        let start = FailureKind::Start(StartFailure::DiskFull);
        assert_eq!(
            continuation(&facts(0, false, Some(("0.11.4", start)))),
            Some(Continuation::StartFailed)
        );
        assert_eq!(
            continuation(&facts(0, false, Some(("0.11.2", start)))),
            Some(Continuation::Soon)
        );
    }

    #[test]
    fn an_install_failure_hides_whether_a_start_failed() {
        let install = FailureKind::Install(InstallFailure::Download);
        assert_eq!(
            continuation(&facts(1, false, Some(("0.11.5", install)))),
            Some(Continuation::Unknown)
        );
    }

    #[test]
    fn nothing_waits_without_a_pending_version() {
        let facts = UpdateFacts {
            pending: None,
            ..facts(0, false, None)
        };
        assert_eq!(continuation(&facts), None);
    }
}
