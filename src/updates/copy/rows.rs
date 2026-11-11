//! Row statuses, actions and descriptions: one row model for the title-bar list, Settings >
//! Updates, the Machine card and the menu bar extra.
use gpui::SharedString;
use spaceterm_core::update::continuation::Continuation;

use super::{Platform, dialogs, sessions, waited};
use crate::updates::ladder::Stage;
use crate::updates::machines::MachineAction;
use crate::updates::state::{
    AppRow, AppStage, Blocked, Command, DesktopAction, MachineRow, MachineStage, RowAction, RowView,
};

pub(crate) const APP_NAME: &str = "SpaceTerm";
/// The heading above the Machine rows on every surface.
pub(crate) const CORE_HEADING: &str = "SpaceTerm Core";
/// The list's last entry, which opens Settings > Updates.
pub(crate) const UPDATE_SETTINGS: &str = "Update Settings\u{2026}";
pub(crate) const WHATS_NEW: &str = "What's New";
pub(crate) const DOWNLOAD: &str = "Download";
pub(crate) const RELAUNCH_TO_UPDATE: &str = "Relaunch to Update";
pub(crate) const RESTART_CORE: &str = "Restart Core\u{2026}";
pub(crate) const TRY_AGAIN: &str = "Try Again";
pub(crate) const TRY_AGAIN_ASKS: &str = "Try Again\u{2026}";
pub(crate) const CHECK_AGAIN: &str = "Check Again";
pub(crate) const HOW_TO_UPDATE: &str = "How to Update\u{2026}";
pub(crate) const KEEP_RUNNING: &str = "Terminal Sessions keep running";
pub(crate) const NOTHING_RUNNING: &str =
    "Nothing running means no program in any Terminal Session and nothing typed or shown.";

fn action(label: &str, subtitle: Option<String>, command: Command) -> Option<RowAction> {
    Some(RowAction {
        label: SharedString::from(label.to_owned()),
        subtitle: subtitle.map(Into::into),
        command,
    })
}

/// SpaceTerm's row.
pub(crate) fn app_row(app: &AppRow) -> RowView {
    let keep = || Some(KEEP_RUNNING.to_owned());
    let (status, row_action, description, sentence) = match &app.stage {
        AppStage::Current { .. } => (
            app.running.to_string(),
            app.notes
                .then(|| action(WHATS_NEW, None, Command::Desktop(DesktopAction::WhatsNew)))
                .flatten(),
            None,
            format!("SpaceTerm {} is running.", app.running),
        ),
        AppStage::Available { version } => (
            format!("{version} available"),
            action(DOWNLOAD, keep(), Command::Desktop(DesktopAction::Download)),
            None,
            format!("SpaceTerm {version} is available. Download gets it without relaunching."),
        ),
        AppStage::Downloading { version, .. } => (
            format!("Downloading {version}\u{2026}"),
            None,
            None,
            format!("Downloading SpaceTerm {version}."),
        ),
        AppStage::Downloaded { version } => (
            format!("{version} downloaded"),
            action(
                RELAUNCH_TO_UPDATE,
                keep(),
                Command::Desktop(DesktopAction::Update),
            ),
            None,
            format!(
                "SpaceTerm {version} is downloaded. Relaunching keeps Terminal Sessions running."
            ),
        ),
        AppStage::Relaunching { .. } => (
            "Relaunching\u{2026}".to_owned(),
            None,
            None,
            "Relaunching SpaceTerm. Terminal Sessions keep running.".to_owned(),
        ),
        AppStage::DownloadFailed {
            version, retries, ..
        } => {
            let status = version.as_ref().map_or_else(
                || "Couldn't download the update".to_owned(),
                |v| format!("Couldn't download {v}"),
            );
            let sentence = version.as_ref().map_or_else(
                || "SpaceTerm couldn't download the update.".to_owned(),
                |v| format!("SpaceTerm couldn't download {v}."),
            );
            if *retries {
                (
                    status,
                    None,
                    Some("SpaceTerm tries again later.".to_owned()),
                    sentence,
                )
            } else {
                (
                    status,
                    action(TRY_AGAIN, None, Command::Desktop(DesktopAction::Download)),
                    None,
                    sentence,
                )
            }
        }
        AppStage::RelaunchFailed { .. } => (
            "Couldn't relaunch".to_owned(),
            action(TRY_AGAIN, None, Command::Desktop(DesktopAction::Update)),
            None,
            "SpaceTerm couldn't relaunch. Terminal Sessions keep running.".to_owned(),
        ),
        AppStage::CantReplace => {
            let status = cant_replace(Platform::HOST).to_owned();
            let sentence = format!("{status}.");
            (status, None, None, sentence)
        }
        AppStage::NeedsNewerSystem { version } => {
            let status = match version {
                Some(v) => format!("{v} needs a newer {}", system(Platform::HOST)),
                None => format!("Needs a newer {}", system(Platform::HOST)),
            };
            let sentence = format!("SpaceTerm {status}.");
            (status, None, None, sentence)
        }
    };
    let age = (app.waiting().is_some() && app.ladder >= Stage::Ask).then(|| waited(app.waited));
    let description = match (description, age) {
        (Some(text), Some(age)) => Some(format!("{text} {age}")),
        (text, age) => text.or(age),
    };
    RowView {
        name: APP_NAME.into(),
        version: Some(app.running.clone()),
        status: Some(status.into()),
        action: row_action,
        accessibility: sentence.into(),
        description: description.map(Into::into),
    }
}

/// The SpaceTerm row's status when the installation can't be replaced.
pub(crate) fn cant_replace(platform: Platform) -> &'static str {
    match platform {
        Platform::Mac => "Move SpaceTerm to Applications to update",
        Platform::Linux => "Run the SpaceTerm install script to update",
    }
}

/// The system a release can need a newer version of.
pub(crate) fn system(platform: Platform) -> &'static str {
    match platform {
        Platform::Mac => "macOS",
        Platform::Linux => "system",
    }
}

/// "Restarts into 0.11.4 when 2 Terminal Sessions end", "… when 1 Terminal Session ends".
fn when_sessions_end(version: &str, n: u32) -> String {
    match n {
        1 => format!("Restarts into {version} when 1 Terminal Session ends"),
        n => format!("Restarts into {version} when {n} Terminal Sessions end"),
    }
}

/// One Machine's row.
pub(crate) fn machine_row(row: &MachineRow) -> RowView {
    let name = &row.name;
    let id = row.id;
    let running = row.running.as_ref().map(ToString::to_string);
    let ends = match row.sessions {
        0 => None,
        n => Some(format!("Ends {}", sessions(n))),
    };
    let restart = || {
        action(
            RESTART_CORE,
            ends.clone(),
            Command::Machine(id, MachineAction::Restart),
        )
    };
    let too_old = row.blocked == Some(Blocked::CoreTooOld);
    let blocked = |text: String| {
        if too_old {
            format!("Too old for this SpaceTerm \u{b7} {text}")
        } else {
            text
        }
    };
    let subject = format!("SpaceTerm Core on {name}");
    let (status, row_action, description, sentence): (
        String,
        Option<RowAction>,
        Option<String>,
        String,
    ) = match &row.stage {
        MachineStage::Running => match (row.blocked, &running) {
            (Some(Blocked::CoreTooOld), _) => (
                "Too old for this SpaceTerm".to_owned(),
                None,
                None,
                format!("{subject} is too old for this SpaceTerm."),
            ),
            (Some(Blocked::AppTooOld), _) => (
                "Needs a newer SpaceTerm".to_owned(),
                None,
                None,
                format!("This SpaceTerm is too old for {subject}."),
            ),
            (None, Some(running)) => (
                running.clone(),
                None,
                None,
                format!("{subject} runs {running}."),
            ),
            (None, None) => (
                "Connecting\u{2026}".to_owned(),
                None,
                None,
                format!("Connecting to {subject}."),
            ),
        },
        MachineStage::Connecting => (
            "Connecting\u{2026}".to_owned(),
            None,
            None,
            format!("Connecting to {subject}."),
        ),
        MachineStage::Offline => (
            "Offline".to_owned(),
            None,
            None,
            format!("{name} is offline."),
        ),
        MachineStage::Stopped => (
            "Stopped".to_owned(),
            None,
            None,
            format!("{subject} is stopped."),
        ),
        MachineStage::WaitingForApp { version } => (
            "Waiting for the SpaceTerm update".to_owned(),
            None,
            None,
            format!(
                "SpaceTerm downloads SpaceTerm Core {version} to {name} after the SpaceTerm update."
            ),
        ),
        MachineStage::Downloading { version, .. } => (
            blocked(format!("Downloading {version}\u{2026}")),
            None,
            None,
            format!("Downloading SpaceTerm Core {version} to {name}."),
        ),
        MachineStage::Downloaded {
            version,
            continuation,
            ..
        } => {
            let version = version.to_string();
            let consequence = match row.sessions {
                0 => "No Terminal Session is open.".to_owned(),
                n => format!("Restarting SpaceTerm Core now ends {}.", sessions(n)),
            };
            if too_old {
                (
                    blocked(format!("{version} downloaded")),
                    restart(),
                    None,
                    format!(
                        "{subject} is too old for this SpaceTerm and has {version} downloaded. {consequence}"
                    ),
                )
            } else {
                match continuation {
                    Continuation::Soon => (
                        format!("Restarts into {version} in a moment"),
                        None,
                        None,
                        format!("{subject} restarts into {version} in a moment."),
                    ),
                    Continuation::WhenSessionsEnd => (
                        when_sessions_end(&version, row.sessions),
                        restart(),
                        None,
                        format!(
                            "{subject} restarts into {version} when its {} end. {consequence}",
                            sessions(row.sessions)
                        )
                        .replace("1 Terminal Session end.", "1 Terminal Session ends."),
                    ),
                    Continuation::WhenNothingRuns => (
                        format!("Restarts into {version} after an hour with nothing running"),
                        restart(),
                        Some(NOTHING_RUNNING.to_owned()),
                        format!(
                            "{subject} restarts into {version} after an hour with nothing running. {consequence}"
                        ),
                    ),
                    Continuation::StartFailed => (
                        format!("Couldn't restart into {version}"),
                        action(
                            TRY_AGAIN_ASKS,
                            None,
                            Command::Machine(id, MachineAction::Restart),
                        ),
                        None,
                        format!("{subject} couldn't restart into {version}."),
                    ),
                    Continuation::NeedsConfirmation | Continuation::Unknown => (
                        format!("{version} downloaded"),
                        restart(),
                        None,
                        format!("{subject} has {version} downloaded. {consequence}"),
                    ),
                }
            }
        }
        MachineStage::Restarting { to: Some(to), .. } => (
            format!("Restarting into {to}\u{2026}"),
            None,
            None,
            format!("{subject} is restarting into {to}."),
        ),
        MachineStage::Restarting { to: None, .. } => (
            "Restarting\u{2026}".to_owned(),
            None,
            None,
            format!("{subject} is restarting."),
        ),
        MachineStage::StartFailed { version, kept, .. } => (
            format!("Couldn't restart into {version}"),
            kept.then(|| {
                action(
                    TRY_AGAIN_ASKS,
                    None,
                    Command::Machine(id, MachineAction::Restart),
                )
            })
            .flatten(),
            (!kept).then(|| "SpaceTerm waits for the next update.".to_owned()),
            format!("{subject} couldn't restart into {version}."),
        ),
        MachineStage::DownloadFailed {
            version, retries, ..
        } => (
            format!("Couldn't download {version}"),
            (!retries)
                .then(|| {
                    action(
                        TRY_AGAIN,
                        None,
                        Command::Machine(id, MachineAction::Install),
                    )
                })
                .flatten(),
            retries.then(|| "SpaceTerm Core tries again later.".to_owned()),
            format!("SpaceTerm couldn't download SpaceTerm Core {version} to {name}."),
        ),
        MachineStage::NotConfirmed { .. } => (
            "Not responding".to_owned(),
            action(
                CHECK_AGAIN,
                None,
                Command::Machine(id, MachineAction::CheckAgain),
            ),
            None,
            format!("{name} stopped responding while SpaceTerm Core restarted."),
        ),
        MachineStage::CantCheck => (
            "Can't check for updates".to_owned(),
            action(
                CHECK_AGAIN,
                None,
                Command::Machine(id, MachineAction::CheckAgain),
            ),
            None,
            format!("{subject} can't check for updates."),
        ),
        MachineStage::NeedsFloor { floor, .. } => (
            format!("Needs {floor} first"),
            action(DOWNLOAD, None, Command::Machine(id, MachineAction::Install)),
            None,
            format!("{subject} needs {floor} first."),
        ),
        MachineStage::SteppingStoneSkipped => (
            "Can't update by itself".to_owned(),
            action(
                HOW_TO_UPDATE,
                None,
                Command::Explain(dialogs::how_to_update(row)),
            ),
            None,
            format!("{subject} can't update by itself."),
        ),
    };
    // From Ask, a waiting row adds how long the update waited, after its own description.
    let age = row
        .ladder
        .filter(|stage| *stage >= Stage::Ask)
        .map(|_| waited(row.waited));
    let description = match (description, age) {
        (Some(text), Some(age)) => Some(format!("{text} {age}")),
        (text, age) => text.or(age),
    };
    RowView {
        name: name.clone(),
        version: running.map(Into::into),
        status: Some(status.into()),
        action: row_action,
        description: description.map(Into::into),
        accessibility: sentence.into(),
    }
}
