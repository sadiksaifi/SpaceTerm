//! What a Pane says about a SpaceTerm Core restart that ended its Terminal Session, and what
//! a blocked Workspace surface says. Owned by the pane slice.

use spaceterm_core::protocol::version::Incompatible;
use spaceterm_core::update::facts::{Operation, UpdateFacts, Version};

/// The title of a Pane whose Terminal Session a restart ended.
pub(crate) const SESSION_ENDED: &str = "Terminal Session Ended";

/// After a restart without updating.
pub(crate) const RESTARTED: &str = "SpaceTerm Core restarted.";

/// After a restart into `version` that a person approved, or whose cause this desktop never
/// saw announced. It claims no cause.
pub(crate) fn restarted_into(version: &Version) -> String {
    format!("SpaceTerm Core restarted into {version}.")
}

/// After the Core restarted into `version` by itself because nothing was running.
pub(crate) fn restarted_into_while_idle(version: &Version) -> String {
    format!("SpaceTerm Core restarted into {version} while nothing was running.")
}

/// A restart's detail, then what became of what the person typed as it began. `not_sent` says
/// the Core received none of it; otherwise it may have received some.
pub(crate) fn with_typed_input(detail: &str, not_sent: bool) -> String {
    let typed = if not_sent {
        TYPED_NOT_SENT
    } else {
        TYPED_UNCONFIRMED
    };
    format!("{detail} {typed}")
}

/// The Core refused what the person typed during the restart, or it never left this desktop.
pub(crate) const TYPED_NOT_SENT: &str = "What you typed during the restart wasn\u{2019}t sent.";

/// Some of what the person typed during the restart left this desktop unconfirmed.
pub(crate) const TYPED_UNCONFIRMED: &str =
    "What you typed during the restart may not have been sent.";

/// A Pane's status when its Terminal Session runs on after what the person typed was kept:
/// after an idle restart that refused it and then did not happen, or after a lost connection.
/// `not_sent` says the Core received none of it, which a lost connection never proves.
pub(crate) fn reattached_input(after_restart: bool, not_sent: bool) -> &'static str {
    match (after_restart, not_sent) {
        (true, true) => "What you typed wasn\u{2019}t sent. SpaceTerm Core was about to restart.",
        (true, false) => {
            "What you typed may not have been sent. SpaceTerm Core was about to restart."
        }
        (false, _) => {
            "What you typed may not have been sent. The connection to SpaceTerm Core was lost."
        }
    }
}

/// Copies the kept text to the clipboard. It sends nothing, because the text can hold Return.
pub(crate) const COPY_TYPED_TEXT: &str = "Copy Typed Text";
pub(crate) const COPY_TYPED_TEXT_DESCRIPTION: &str =
    "Copies what you typed to the clipboard without sending it.";

/// After `version` failed to start and the Core went back to its running version.
pub(crate) fn could_not_restart_into(version: &Version) -> String {
    format!("SpaceTerm Core couldn\u{2019}t restart into {version}.")
}

/// The launch screen's title when this SpaceTerm was replaced on disk while it ran.
pub(crate) const RELAUNCH_SPACETERM: &str = "Relaunch SpaceTerm";

/// The blocked Workspace surface's title, which names the side that is too old.
pub(crate) const fn blocked_title(incompatible: Incompatible) -> &'static str {
    match incompatible {
        Incompatible::ListenerTooOld => "SpaceTerm Core Is Too Old",
        Incompatible::CallerTooOld => "This SpaceTerm Is Too Old",
    }
}

/// The blocked detail where the Machine has no name to show.
pub(crate) const fn blocked_unnamed(incompatible: Incompatible) -> &'static str {
    match incompatible {
        Incompatible::ListenerTooOld => "This SpaceTerm Core is too old for this SpaceTerm.",
        Incompatible::CallerTooOld => "This SpaceTerm is too old for this SpaceTerm Core.",
    }
}

/// What a blocked Workspace surface offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockedAction {
    /// `asks`: the restart ends Terminal Sessions or is breaking, so a confirmation follows.
    RestartCore {
        asks: bool,
    },
    RelaunchToUpdate,
    DownloadUpdate,
}

impl BlockedAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            // A label ends in an ellipsis only when a confirmation follows.
            Self::RestartCore { asks: true } => "Restart Core\u{2026}",
            Self::RestartCore { asks: false } => "Restart Core",
            Self::RelaunchToUpdate => "Relaunch to Update",
            Self::DownloadUpdate => "Download Update",
        }
    }
}

/// What a surface says while a Machine's Workspaces stay closed because SpaceTerm and the
/// Machine's SpaceTerm Core share no protocol major (ADR 0035).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Blocked {
    pub(crate) detail: String,
    /// The download that ends the block, while it runs.
    pub(crate) progress: Option<String>,
    pub(crate) action: Option<BlockedAction>,
}

/// SpaceTerm's own update, as far as a blocked surface offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppRelease {
    /// No newer SpaceTerm is known, or it is on its way.
    None,
    Available,
    Downloaded,
}

/// The blocked surface for `machine`. `facts` are the Core's latest update facts, which arrive
/// over the service lane while the work lane refuses.
pub(crate) fn blocked(
    incompatible: Incompatible,
    machine: &str,
    facts: Option<&UpdateFacts>,
    app: AppRelease,
) -> Blocked {
    match incompatible {
        Incompatible::ListenerTooOld => {
            let mut detail = format!("SpaceTerm Core on {machine} is too old for this SpaceTerm.");
            let mut progress = None;
            let mut action = None;
            if let Some(pending) = facts.and_then(|facts| facts.pending.as_ref()) {
                let sessions = facts.map_or(0, |facts| facts.sessions);
                let version = &pending.version;
                detail.push_str(&match sessions {
                    0 => format!(
                        " SpaceTerm Core {version} is downloaded. No Terminal Session is open on \
                         {machine}."
                    ),
                    1 => format!(
                        " SpaceTerm Core {version} is downloaded. Restarting ends 1 Terminal \
                         Session on {machine}."
                    ),
                    n => format!(
                        " SpaceTerm Core {version} is downloaded. Restarting ends {n} Terminal \
                         Sessions on {machine}."
                    ),
                });
                action = Some(BlockedAction::RestartCore {
                    asks: sessions > 0 || pending.breaking,
                });
            } else if let Some(Operation::Installing { version, .. }) =
                facts.and_then(|facts| facts.operation.as_ref())
            {
                progress = Some(format!("Downloading SpaceTerm Core {version}\u{2026}"));
            }
            Blocked {
                detail,
                progress,
                action,
            }
        }
        Incompatible::CallerTooOld => Blocked {
            detail: format!("This SpaceTerm is too old for SpaceTerm Core on {machine}."),
            progress: None,
            action: match app {
                AppRelease::None => None,
                AppRelease::Available => Some(BlockedAction::DownloadUpdate),
                AppRelease::Downloaded => Some(BlockedAction::RelaunchToUpdate),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::update::facts::{Platform, UpdateCheck};

    fn facts(sessions: u32) -> UpdateFacts {
        UpdateFacts {
            revision: 1,
            platform: Platform::HOST,
            running: Version::parse("0.11.4").unwrap(),
            pending: None,
            sessions,
            operation: None,
            check: UpdateCheck::Disabled,
            skipped: None,
            last_failure: None,
        }
    }

    #[test]
    fn a_core_too_old_says_so_then_downloads_then_offers_its_restart() {
        let waiting = blocked(
            Incompatible::ListenerTooOld,
            "penguin",
            Some(&facts(3)),
            AppRelease::None,
        );
        assert_eq!(
            waiting.detail,
            "SpaceTerm Core on penguin is too old for this SpaceTerm."
        );
        assert_eq!((waiting.progress, waiting.action), (None, None));

        let mut downloading = facts(3);
        downloading.operation = Some(Operation::Installing {
            version: Version::parse("0.14.0").unwrap(),
            received: 1,
            total: 2,
        });
        let downloading = blocked(
            Incompatible::ListenerTooOld,
            "penguin",
            Some(&downloading),
            AppRelease::None,
        );
        assert_eq!(
            downloading.progress.as_deref(),
            Some("Downloading SpaceTerm Core 0.14.0\u{2026}")
        );
        assert_eq!(downloading.action, None, "nothing to click yet");

        let mut downloaded = facts(3);
        downloaded.pending = Some(spaceterm_core::update::facts::PendingVersion {
            version: Version::parse("0.14.0").unwrap(),
            protocols: spaceterm_core::update::facts::Protocols::CURRENT,
            breaking: true,
            installed_at: 0,
            overdue_since: None,
            stranded: Vec::new(),
        });
        let downloaded = blocked(
            Incompatible::ListenerTooOld,
            "penguin",
            Some(&downloaded),
            AppRelease::None,
        );
        assert_eq!(
            downloaded.detail,
            "SpaceTerm Core on penguin is too old for this SpaceTerm. SpaceTerm Core 0.14.0 is \
             downloaded. Restarting ends 3 Terminal Sessions on penguin."
        );
        assert_eq!(downloaded.progress, None);
        assert_eq!(
            downloaded.action.map(BlockedAction::label),
            Some("Restart Core\u{2026}")
        );
    }

    #[test]
    fn a_spaceterm_too_old_offers_its_own_update_when_a_release_is_known() {
        let app = |release| {
            blocked(
                Incompatible::CallerTooOld,
                "penguin",
                Some(&facts(3)),
                release,
            )
        };
        assert_eq!(
            app(AppRelease::None).detail,
            "This SpaceTerm is too old for SpaceTerm Core on penguin."
        );
        assert_eq!(app(AppRelease::None).action, None);
        assert_eq!(
            app(AppRelease::Available).action.map(BlockedAction::label),
            Some("Download Update")
        );
        assert_eq!(
            app(AppRelease::Downloaded).action.map(BlockedAction::label),
            Some("Relaunch to Update")
        );
    }
}
