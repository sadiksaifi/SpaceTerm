//! The confirmation and failure alerts, D1 to D8.
use gpui::SharedString;
use spaceterm_core::protocol::capability::Unavailable;
use spaceterm_core::update::facts::StartFailure;

use super::{Platform, list_and, this_ends};
use crate::updates::UpdateError;
use crate::updates::confirm::{Explanation, Previewed};
use crate::updates::machines::{ActionFailure, MachineAction};
use crate::updates::state::{
    AppRow, AppStage, Command, DesktopAction, DownloadCause, MachineRow, MachineStage,
    PUSH_FAILURE, STEPPING_STONE,
};

/// The alert Check for Updates shows once a release is downloaded: its accessibility label,
/// title and body for `version`. Its main button is [`super::title_bar::RELAUNCH_TO_UPDATE`].
pub(crate) fn downloaded(version: &str) -> (&'static str, String, &'static str) {
    (
        "Update downloaded",
        format!("SpaceTerm {version} Is Downloaded"),
        "Relaunching keeps Terminal Sessions running.",
    )
}

/// The destructive button of D1 and D2.
pub(crate) const RESTART_BUTTON: &str = "Restart Core";
pub(crate) const STOP_BUTTON: &str = "Stop";
/// D1's other button. It closes the alert and changes nothing.
pub(crate) const CANCEL: &str = "Cancel";
/// D1's other button at the Ask stage, where it hides that Machine's step for a day.
pub(crate) const REMIND_ME_TOMORROW: &str = "Remind Me Tomorrow";
pub(crate) const TRY_AGAIN: &str = "Try Again";
pub(crate) const CHECK_AGAIN: &str = "Check Again";

/// D1, or D2 for a same-version restart: one Machine per confirmation.
pub(crate) fn restart(described: &[Previewed]) -> (SharedString, SharedString, SharedString) {
    let title = match described {
        [one] => format!("Restart SpaceTerm Core on {}?", one.name),
        _ => "Restart SpaceTerm Core?".to_owned(),
    };
    let message = described
        .iter()
        .map(|one| {
            let mut text = match (&one.target, &one.running) {
                (Some(to), Some(running)) => {
                    format!("SpaceTerm Core restarts from {running} to {to}.")
                }
                (Some(to), None) => format!("SpaceTerm Core restarts into {to}."),
                (None, Some(running)) => {
                    format!("SpaceTerm Core {running} restarts without updating.")
                }
                (None, None) => "SpaceTerm Core restarts without updating.".to_owned(),
            };
            text.push(' ');
            text.push_str(&this_ends(one.sessions));
            text.push_str(" Workspaces, Tabs and Panes stay.");
            if one.breaking == Some(true) {
                text.push(' ');
                text.push_str(&stranded(&one.name, &one.stranded));
            }
            text
        })
        .collect::<Vec<_>>()
        .join(" ");
    (title.into(), message.into(), RESTART_BUTTON.into())
}

/// The sentence a breaking restart adds about Paired Devices on an older SpaceTerm.
pub(crate) fn stranded(machine: &str, devices: &[String]) -> String {
    match devices {
        [] => {
            format!(
                "Devices with an older SpaceTerm can\u{2019}t connect to {machine} until they update."
            )
        }
        [_] => format!(
            "SpaceTerm on {} can\u{2019}t connect to {machine} until it updates.",
            list_and(devices)
        ),
        _ => format!(
            "SpaceTerm on {} can\u{2019}t connect to {machine} until they update.",
            list_and(devices)
        ),
    }
}

/// Stop SpaceTerm Core, from Settings or the menu bar extra.
pub(crate) fn stop(described: &[Previewed]) -> (SharedString, SharedString, SharedString) {
    let name = described
        .first()
        .map_or_else(SharedString::default, |one| one.name.clone());
    let sessions = described.iter().map(|one| one.sessions).sum();
    (
        format!("Stop SpaceTerm Core on {name}?").into(),
        format!(
            "{} SpaceTerm Core stays stopped until you start it again.",
            this_ends(sessions)
        )
        .into(),
        STOP_BUTTON.into(),
    )
}

/// The alert `Update Failed` opens for SpaceTerm's own failure: D6, D7 or D8.
pub(crate) fn app_failure(app: &AppRow) -> Option<Explanation> {
    let dismiss = Some(Box::new(Command::Desktop(DesktopAction::DismissFailure)));
    let release = |version: &Option<SharedString>| {
        version
            .as_ref()
            .map_or_else(|| "SpaceTerm".to_owned(), |v| format!("SpaceTerm {v}"))
    };
    Some(match &app.stage {
        AppStage::DownloadFailed { version, error, .. } => Explanation {
            title: format!("Couldn\u{2019}t Download {}", release(version)).into(),
            message: match error {
                UpdateError::DiskFull => "Free some disk space, then try again.",
                UpdateError::Verification => {
                    "The download didn\u{2019}t pass verification. Try again."
                }
                _ => "Check your network, then try again.",
            }
            .into(),
            then: Some(Box::new(Command::Desktop(DesktopAction::Download))),
            proceed: Some(TRY_AGAIN.into()),
            dismiss,
        },
        AppStage::RelaunchFailed { version, error, .. } => {
            relaunch_failure(version.as_deref(), *error == UpdateError::SettingsUnsaved)
        }
        AppStage::CantReplace => Explanation {
            title: cant_replace_title(Platform::HOST).into(),
            message: cant_replace_message(Platform::HOST).into(),
            then: None,
            proceed: None,
            dismiss,
        },
        _ => return None,
    })
}

/// D7: the relaunch of `version` failed, or failed because Settings couldn't be saved when
/// `unsaved`. Try Again relaunches, and Cancel dismisses the failure's title-bar step.
pub(crate) fn relaunch_failure(version: Option<&str>, unsaved: bool) -> Explanation {
    let release = version.map_or_else(|| "SpaceTerm".to_owned(), |v| format!("SpaceTerm {v}"));
    Explanation {
        title: "Couldn\u{2019}t Relaunch SpaceTerm".into(),
        message: if unsaved {
            "SpaceTerm couldn\u{2019}t save Settings, so it didn\u{2019}t relaunch. Terminal Sessions keep running.".to_owned()
        } else {
            format!("{release} is downloaded. Terminal Sessions keep running.")
        }
        .into(),
        then: Some(Box::new(Command::Desktop(DesktopAction::Update))),
        proceed: Some(TRY_AGAIN.into()),
        dismiss: Some(Box::new(Command::Desktop(DesktopAction::DismissFailure))),
    }
}

/// D8's title.
pub(crate) fn cant_replace_title(platform: Platform) -> &'static str {
    match platform {
        Platform::Mac => "Move SpaceTerm to Applications to Update",
        Platform::Linux => "Reinstall SpaceTerm to Update",
    }
}

/// D8's body.
pub(crate) fn cant_replace_message(platform: Platform) -> &'static str {
    match platform {
        Platform::Mac => "SpaceTerm can\u{2019}t replace itself where it is.",
        Platform::Linux => "Run the SpaceTerm install script to update.",
    }
}

/// The alert `Update Failed` opens for a Machine's failure: D3a to D3f.
pub(crate) fn machine_failure(row: &MachineRow) -> Option<Explanation> {
    let name = &row.name;
    let id = row.id;
    let running = row
        .running
        .as_ref()
        .map_or_else(String::new, ToString::to_string);
    let dismiss = |failure| {
        Some(Box::new(Command::Machine(
            id,
            MachineAction::DismissFailure(failure),
        )))
    };
    let restart_title = format!("Couldn\u{2019}t Restart SpaceTerm Core on {name}");
    Some(match &row.stage {
        MachineStage::StartFailed { version, cause, id: failure, .. } => {
            let (message, retry) = match cause {
                StartFailure::NotReady => (format!("SpaceTerm Core {version} didn\u{2019}t start, so it went back to {running}. The Terminal Sessions that ended don\u{2019}t come back. SpaceTerm waits for the next update."), false),
                StartFailure::PortInUse => (format!("SpaceTerm Core {version} couldn\u{2019}t start because another app is using {name}\u{2019}s Network Access port, so it went back to {running}. Close that app or change the port, then try again."), true),
                StartFailure::DiskFull => (format!("SpaceTerm Core {version} couldn\u{2019}t start because {name} is out of disk space, so it went back to {running}. Free some space, then try again."), true),
                StartFailure::Interrupted => (format!("The restart into SpaceTerm Core {version} was interrupted, so it went back to {running}."), true),
            };
            Explanation {
                title: restart_title.into(),
                message: message.into(),
                then: retry.then(|| Box::new(Command::Machine(id, MachineAction::Restart))),
                proceed: retry.then(|| TRY_AGAIN.into()),
                dismiss: dismiss(*failure),
            }
        }
        MachineStage::DownloadFailed { version, cause, retries: false, .. } => {
            let title = format!("Couldn\u{2019}t Download SpaceTerm Core {version} to {name}");
            if *cause == DownloadCause::RelaunchRequired {
                return Some(Explanation {
                    title: title.into(),
                    message: "SpaceTerm was replaced on disk while it was running. Relaunch SpaceTerm, then try again.".into(),
                    then: None,
                    proceed: None,
                    dismiss: dismiss(PUSH_FAILURE),
                });
            }
            let first = match cause {
                DownloadCause::DiskFull => format!("{name} is out of disk space. "),
                DownloadCause::Unreachable => format!("{name} stopped answering. "),
                DownloadCause::Busy => format!("SpaceTerm Core on {name} is busy with another update. "),
                DownloadCause::RelaunchRequired | DownloadCause::Failed => String::new(),
            };
            Explanation {
                title: title.into(),
                message: format!("{first}Its Terminal Sessions keep running.").into(),
                then: Some(Box::new(Command::Machine(id, MachineAction::Install))),
                proceed: Some(TRY_AGAIN.into()),
                dismiss: dismiss(PUSH_FAILURE),
            }
        }
        MachineStage::NeedsFloor { version, floor, .. } => Explanation {
            title: format!("Couldn\u{2019}t Download SpaceTerm Core {version} to {name}").into(),
            message: format!("{name} needs SpaceTerm Core {floor} first and couldn\u{2019}t get it. Its Terminal Sessions keep running.").into(),
            then: Some(Box::new(Command::Machine(id, MachineAction::Install))),
            proceed: Some(TRY_AGAIN.into()),
            dismiss: dismiss(PUSH_FAILURE),
        },
        MachineStage::SteppingStoneSkipped => how_to_update(row),
        _ => return None,
    })
}

/// D3f, from the pill's `Update Failed` and the row's `How to Update…` alike. OK dismisses the
/// title-bar step; the row keeps the failure.
pub(crate) fn how_to_update(row: &MachineRow) -> Explanation {
    let name = &row.name;
    Explanation {
        dismiss: Some(Box::new(Command::Machine(
            row.id,
            MachineAction::DismissFailure(STEPPING_STONE),
        ))),
        ..Explanation::ok(
            format!("SpaceTerm Core on {name} Can\u{2019}t Update by Itself"),
            format!(
                "On {name}, install a newer compatible SpaceTerm Core archive with spaceterm update --archive, or wait for a newer release."
            ),
        )
    }
}

/// D4: a restart started here got no answer.
pub(crate) fn not_confirmed(row: &MachineRow) -> Explanation {
    let name = &row.name;
    let version = match &row.stage {
        MachineStage::NotConfirmed { version, .. } => version.to_string(),
        _ => String::new(),
    };
    Explanation {
        title: format!("SpaceTerm Can\u{2019}t Confirm the Restart on {name}").into(),
        message: format!(
            "{name} stopped responding while SpaceTerm Core restarted into {version}."
        )
        .into(),
        then: Some(Box::new(Command::Machine(
            row.id,
            MachineAction::CheckAgain,
        ))),
        proceed: Some(CHECK_AGAIN.into()),
        dismiss: None,
    }
}

/// D5 and its siblings: the alert for an action a Core refused at once. `Changed` asks again
/// instead and `LoginItemOff` opens Login Items, so neither has an alert.
pub(crate) fn action_failure(
    name: &str,
    action: &MachineAction,
    failure: ActionFailure,
) -> Option<(SharedString, SharedString)> {
    let title = match action {
        MachineAction::Install => format!("Couldn\u{2019}t Download SpaceTerm Core to {name}"),
        MachineAction::Restart => format!("Couldn\u{2019}t Restart SpaceTerm Core on {name}"),
        MachineAction::CheckAgain => format!("Couldn\u{2019}t Check for Updates on {name}"),
        MachineAction::Start => format!("Couldn\u{2019}t Start SpaceTerm Core on {name}"),
        MachineAction::Stop => format!("Couldn\u{2019}t Stop SpaceTerm Core on {name}"),
        MachineAction::DismissFailure(_) => return None,
    };
    let message = match failure {
        ActionFailure::Changed | ActionFailure::LoginItemOff => return None,
        ActionFailure::Unavailable(Unavailable::CoreTooOld) => {
            format!("SpaceTerm Core on {name} is too old for this SpaceTerm.")
        }
        ActionFailure::Unavailable(Unavailable::ClientTooOld) => {
            format!("This SpaceTerm is too old for SpaceTerm Core on {name}.")
        }
        ActionFailure::Unavailable(Unavailable::NotConnected) | ActionFailure::Unreachable => {
            format!("SpaceTerm can\u{2019}t reach {name}. Try again when it\u{2019}s connected.")
        }
        ActionFailure::Busy => format!(
            "SpaceTerm Core on {name} is busy with another update. Try again when it finishes."
        ),
        ActionFailure::DiskFull => format!("{name} is out of disk space. Free some space, then try again."),
        ActionFailure::RelaunchRequired => "SpaceTerm was replaced on disk while it was running. Relaunch SpaceTerm, then try again.".to_owned(),
        ActionFailure::ClockBehind => format!("{name}\u{2019}s clock is more than five minutes behind. Set the correct date and time on {name}, then try again."),
        ActionFailure::SteppingStoneSkipped(_) => format!("On {name}, install a newer compatible SpaceTerm Core archive with spaceterm update --archive, or wait for a newer release."),
        ActionFailure::NeedsFloor(floor) => format!("{name} needs SpaceTerm Core {floor} first and couldn\u{2019}t get it."),
        ActionFailure::Failed => format!("SpaceTerm Core on {name} didn\u{2019}t finish. Try again."),
    };
    Some((title.into(), message.into()))
}
