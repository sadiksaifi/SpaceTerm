//! The split pill: label, glyph, emphasis and tooltip. The labels are the closed set of design
//! section 4.5; none carries a count or a Machine's name.
use gpui::SharedString;

use super::{dialogs, sessions, this_ends};
use crate::updates::UpdateError;
use crate::updates::machines::MachineAction;
use crate::updates::state::{
    ActKind, AppRow, AppStage, Blocked, Command, DesktopAction, DownloadCause, Glyph, MachineRow,
    MachineStage, PillView, Step, Target, UpdateView,
};
use spaceterm_core::update::continuation::Continuation;
use spaceterm_core::update::facts::StartFailure;

pub(crate) const UPDATE_FAILED: &str = "Update Failed";
pub(crate) const RESTART_NOT_CONFIRMED: &str = "Restart Not Confirmed";
pub(crate) const RESTARTING_CORE: &str = "Restarting Core\u{2026}";
pub(crate) const DOWNLOADING_UPDATE: &str = "Downloading Update\u{2026}";
pub(crate) const RELAUNCHING: &str = "Relaunching\u{2026}";
pub(crate) const RELAUNCH_TO_UPDATE: &str = "Relaunch to Update";
pub(crate) const RESTART_CORE: &str = "Restart Core\u{2026}";
/// Restart Core when it restarts at once, with no confirmation.
pub(crate) const RESTART_CORE_NOW: &str = "Restart Core";
pub(crate) const DOWNLOAD_UPDATE: &str = "Download Update";
pub(crate) const WHATS_NEW: &str = "What\u{2019}s New";

/// Every main-area label.
#[cfg(test)]
pub(crate) const LABELS: [&str; 10] = [
    UPDATE_FAILED,
    RESTART_NOT_CONFIRMED,
    RESTARTING_CORE,
    DOWNLOADING_UPDATE,
    RELAUNCHING,
    RELAUNCH_TO_UPDATE,
    RESTART_CORE,
    RESTART_CORE_NOW,
    DOWNLOAD_UPDATE,
    WHATS_NEW,
];

/// The split pill for `view`'s step, or None when nothing is owed.
pub(crate) fn pill(view: &UpdateView) -> Option<PillView> {
    let step = view.step?;
    let machine = |id| view.machine(id);
    let (label, glyph, tooltip, main, progress): (&str, Glyph, String, Command, Option<f32>) =
        match step {
            Step::Failed(target) => {
                let (alert, tooltip) = match target {
                    Target::App => (dialogs::app_failure(&view.app)?, app_failed(&view.app)),
                    Target::Machine(id) => {
                        let row = machine(id)?;
                        (dialogs::machine_failure(row)?, machine_failed(row))
                    }
                };
                (
                    UPDATE_FAILED,
                    Glyph::UpdateFailed,
                    tooltip,
                    Command::Explain(alert),
                    None,
                )
            }
            Step::NotConfirmed(id) => {
                let row = machine(id)?;
                (
                    RESTART_NOT_CONFIRMED,
                    Glyph::CantConfirm,
                    format!(
                        "{} stopped responding while SpaceTerm Core restarted.",
                        row.name
                    ),
                    Command::Explain(dialogs::not_confirmed(row)),
                    None,
                )
            }
            Step::Restarting(id) => {
                let row = machine(id)?;
                let tooltip = match &row.stage {
                    MachineStage::Restarting { to: Some(to), .. } => {
                        format!("Restarting SpaceTerm Core on {} into {to}.", row.name)
                    }
                    _ => format!("Restarting SpaceTerm Core on {}.", row.name),
                };
                (
                    RESTARTING_CORE,
                    Glyph::Working,
                    tooltip,
                    Command::ShowList,
                    None,
                )
            }
            Step::Downloading(target) => {
                let (tooltip, progress) = match target {
                    Target::App => match &view.app.stage {
                        AppStage::Downloading {
                            version, fraction, ..
                        } => (format!("Downloading SpaceTerm {version}."), *fraction),
                        _ => return None,
                    },
                    Target::Machine(id) => {
                        let row = machine(id)?;
                        match &row.stage {
                            MachineStage::Downloading {
                                version, fraction, ..
                            } => (
                                format!("Downloading SpaceTerm Core {version} to {}.", row.name),
                                *fraction,
                            ),
                            _ => return None,
                        }
                    }
                };
                (
                    DOWNLOADING_UPDATE,
                    Glyph::Working,
                    tooltip,
                    Command::ShowList,
                    progress,
                )
            }
            Step::Relaunching => (
                RELAUNCHING,
                Glyph::Working,
                "Relaunching SpaceTerm. Terminal Sessions keep running.".to_owned(),
                Command::ShowList,
                None,
            ),
            // A stage changes emphasis and cadence, never words: every stage's tooltip names the
            // software and its version, as Offer's does.
            Step::Act { kind, .. } => {
                match kind {
                    ActKind::Relaunch => (
                        RELAUNCH_TO_UPDATE,
                        Glyph::Restart,
                        if let Some(version) = &view.app.replacing {
                            // A scheduled check downloads this release again or a newer one.
                            format!(
                                "Relaunching updates SpaceTerm to {version}. Terminal Sessions keep running."
                            )
                        } else {
                            format!(
                                "SpaceTerm {} is downloaded. Relaunching keeps Terminal Sessions running.",
                                release(&view.app)
                            )
                        },
                        Command::Desktop(DesktopAction::Update),
                        None,
                    ),
                    ActKind::RestartCore(id) => {
                        let row = machine(id)?;
                        (
                            // A label ends in an ellipsis only when a confirmation follows.
                            if row.restart_asks() {
                                RESTART_CORE
                            } else {
                                RESTART_CORE_NOW
                            },
                            Glyph::UpdateCore,
                            restart_tooltip(row),
                            Command::Machine(id, MachineAction::Restart),
                            None,
                        )
                    }
                    ActKind::Download => (
                        DOWNLOAD_UPDATE,
                        Glyph::Download,
                        match release(&view.app) {
                            version if version.is_empty() => {
                                "A SpaceTerm update is available.".to_owned()
                            }
                            version => format!("SpaceTerm {version} is available."),
                        },
                        Command::Desktop(DesktopAction::Download),
                        None,
                    ),
                }
            }
            Step::WhatsNew => (
                WHATS_NEW,
                Glyph::WhatsNew,
                format!("See what\u{2019}s new in SpaceTerm {}.", view.app.running),
                Command::Desktop(DesktopAction::WhatsNew),
                None,
            ),
        };
    Some(PillView {
        label: label.into(),
        glyph,
        emphasis: step.emphasis(),
        tooltip: tooltip.into(),
        main,
        progress,
    })
}

/// The tooltip of `Restart Core…`: blocked, then downloaded, with what breaking costs.
fn restart_tooltip(row: &MachineRow) -> String {
    let name = &row.name;
    let n = row.sessions;
    let (version, breaking, stranded) = match &row.stage {
        MachineStage::Downloaded {
            version,
            continuation,
            stranded,
        } => (
            version.to_string(),
            *continuation == Continuation::NeedsConfirmation,
            stranded.as_slice(),
        ),
        _ => (String::new(), false, &[][..]),
    };
    if row.blocked == Some(Blocked::CoreTooOld) {
        return format!(
            "{name}\u{2019}s Workspaces can\u{2019}t open until SpaceTerm Core restarts into {version}. {}",
            this_ends(n)
        );
    }
    let ends = match n {
        0 => "No Terminal Session is open.".to_owned(),
        n => format!("Restarting into it ends {}.", sessions(n)),
    };
    let mut text = format!("SpaceTerm Core {version} is downloaded on {name}. {ends}");
    if breaking {
        text.push(' ');
        text.push_str(&dialogs::stranded(name, stranded));
    }
    text
}

/// The release an action step names: the one that waits, or the one whose failure the person
/// dismissed while its step still applies.
fn release(app: &AppRow) -> SharedString {
    app.waiting()
        .cloned()
        .or_else(|| match &app.stage {
            AppStage::DownloadFailed { version, .. } | AppStage::RelaunchFailed { version, .. } => {
                version.clone()
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// The tooltip of `Update Failed` for SpaceTerm's own failure: what failed, then why, in at
/// most two short sentences.
fn app_failed(app: &AppRow) -> String {
    let release = |version: &Option<SharedString>| {
        version
            .as_ref()
            .map_or_else(|| "SpaceTerm".to_owned(), |v| format!("SpaceTerm {v}"))
    };
    match &app.stage {
        AppStage::DownloadFailed { version, error, .. } => {
            let why = match error {
                UpdateError::DiskFull => "Free some disk space, then try again.",
                UpdateError::Verification => "The download didn\u{2019}t pass verification.",
                _ => "Check your network, then try again.",
            };
            format!("Couldn\u{2019}t download {}. {why}", release(version))
        }
        AppStage::RelaunchFailed {
            error: UpdateError::SettingsUnsaved,
            ..
        } => "Couldn\u{2019}t relaunch SpaceTerm because it couldn\u{2019}t save Settings."
            .to_owned(),
        AppStage::RelaunchFailed {
            version: Some(version),
            ..
        } => format!("Couldn\u{2019}t relaunch into SpaceTerm {version}."),
        AppStage::RelaunchFailed { version: None, .. } => {
            "Couldn\u{2019}t relaunch SpaceTerm.".to_owned()
        }
        _ => "SpaceTerm can\u{2019}t update itself where it is.".to_owned(),
    }
}

/// The tooltip of `Update Failed` for a Machine's failure: what failed, then why, in at most two
/// short sentences.
fn machine_failed(row: &MachineRow) -> String {
    let name = &row.name;
    match &row.stage {
        MachineStage::StartFailed { version, cause, .. } => {
            let why = match cause {
                StartFailure::NotReady => format!("SpaceTerm Core {version} didn\u{2019}t start."),
                StartFailure::PortInUse => {
                    format!("Another app is using {name}\u{2019}s Network Access port.")
                }
                StartFailure::DiskFull => format!("{name} is out of disk space."),
                StartFailure::Interrupted => "The restart was interrupted.".to_owned(),
            };
            format!("Couldn\u{2019}t restart SpaceTerm Core on {name}. {why}")
        }
        MachineStage::DownloadFailed { version, cause, .. } => {
            let failed = format!("Couldn\u{2019}t download SpaceTerm Core {version} to {name}.");
            let why = match cause {
                DownloadCause::DiskFull => format!("{name} is out of disk space."),
                DownloadCause::Unreachable => format!("{name} stopped answering."),
                DownloadCause::Busy => {
                    format!("SpaceTerm Core on {name} is busy with another update.")
                }
                DownloadCause::RelaunchRequired => {
                    "SpaceTerm was replaced on disk while it was running.".to_owned()
                }
                DownloadCause::Failed => return failed,
            };
            format!("{failed} {why}")
        }
        MachineStage::NeedsFloor { version, floor, .. } => format!(
            "Couldn\u{2019}t download SpaceTerm Core {version} to {name}. {name} needs SpaceTerm Core \
             {floor} first."
        ),
        _ => format!("SpaceTerm Core on {name} can\u{2019}t update by itself."),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::updates::state::tests::{NOW, app};
    use crate::updates::state::{AppFacts, Dismissed, Marks, UpdateView};
    use crate::updates::{UpdateError, UpdateState};

    fn tooltip(facts: AppFacts<'_>) -> String {
        let marks = Marks {
            dismissed: &Dismissed::new(),
            deferred: &HashMap::new(),
            pushes_held: false,
        };
        let pill = UpdateView::new(facts, &[], marks, NOW)
            .pill()
            .expect("a step");
        pill.tooltip.to_string()
    }

    /// Hovering `Update Failed` tells what failed before the alert opens: SpaceTerm and the
    /// release, then why.
    #[test]
    fn the_update_failed_tooltip_names_what_failed() {
        let cases = [
            (
                UpdateError::Download,
                "Couldn\u{2019}t download SpaceTerm 0.11.5. Check your network, then try again.",
            ),
            (
                UpdateError::DiskFull,
                "Couldn\u{2019}t download SpaceTerm 0.11.5. Free some disk space, then try again.",
            ),
            (
                UpdateError::Verification,
                "Couldn\u{2019}t download SpaceTerm 0.11.5. The download didn\u{2019}t pass verification.",
            ),
            (
                UpdateError::Relaunch,
                "Couldn\u{2019}t relaunch into SpaceTerm 0.11.5.",
            ),
            (
                UpdateError::SettingsUnsaved,
                "Couldn\u{2019}t relaunch SpaceTerm because it couldn\u{2019}t save Settings.",
            ),
            (
                UpdateError::ReadOnly,
                "SpaceTerm can\u{2019}t update itself where it is.",
            ),
        ];
        for (error, expected) in cases {
            let state = UpdateState::Failed { error };
            let mut facts = app(&state);
            facts.pending = Some("0.11.5");
            assert_eq!(tooltip(facts), expected, "{error:?}");
        }
    }

    /// After a dismissed download failure the main area offers `Download Update`, whose tooltip
    /// names the release, or says only that one is available when the failure named none.
    #[test]
    fn a_dismissed_download_failure_offers_its_release() {
        let failed = UpdateState::Failed {
            error: UpdateError::Download,
        };
        for (pending, expected) in [
            (Some("0.11.5"), "SpaceTerm 0.11.5 is available."),
            (None, "A SpaceTerm update is available."),
        ] {
            let mut facts = app(&failed);
            facts.pending = pending;
            facts.failure_dismissed = true;
            let marks = Marks {
                dismissed: &Dismissed::new(),
                deferred: &HashMap::new(),
                pushes_held: false,
            };
            let pill = UpdateView::new(facts, &[], marks, NOW)
                .pill()
                .expect("the release keeps a step");
            assert_eq!(pill.label.as_ref(), super::DOWNLOAD_UPDATE);
            assert_eq!(pill.tooltip.as_ref(), expected);
        }
    }

    /// While a scheduled check downloads a newer release in place of the downloaded one, the
    /// tooltip names the release Relaunch installs and claims no finished download.
    #[test]
    fn the_relaunch_tooltip_names_the_release_relaunch_installs() {
        let downloading = UpdateState::Downloading {
            version: "0.11.6".into(),
            received: 1,
            total: 2,
        };
        let mut facts = app(&downloading);
        facts.pending = Some("0.11.6");
        facts.replacing = Some("0.11.6");
        assert_eq!(
            tooltip(facts),
            "Relaunching updates SpaceTerm to 0.11.6. Terminal Sessions keep running."
        );

        let ready = UpdateState::Ready {
            version: "0.11.6".into(),
        };
        let mut facts = app(&ready);
        facts.pending = Some("0.11.6");
        assert_eq!(
            tooltip(facts),
            "SpaceTerm 0.11.6 is downloaded. Relaunching keeps Terminal Sessions running."
        );

        // A dismissed failed relaunch whose installer the updater still holds.
        let failed = UpdateState::Failed {
            error: UpdateError::Relaunch,
        };
        let mut facts = app(&failed);
        facts.pending = Some("0.11.6");
        facts.relaunch_held = true;
        facts.failure_dismissed = true;
        assert_eq!(
            tooltip(facts),
            "SpaceTerm 0.11.6 is downloaded. Relaunching keeps Terminal Sessions running."
        );
    }
}
