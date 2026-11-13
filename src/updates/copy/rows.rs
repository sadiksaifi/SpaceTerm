//! Row statuses, actions and descriptions: one row model for the title-bar list, Settings >
//! Updates, the Machine card and the menu bar extra.
use gpui::SharedString;
use spaceterm_core::update::continuation::Continuation;

use super::{Platform, dialogs, sessions, waited};
use crate::updates::ladder::Stage;
use crate::updates::machines::MachineAction;
use crate::updates::state::{
    AppRow, AppStage, AskedCheck, Blocked, Command, DesktopAction, LastCheck, MachineRow,
    MachineStage, RowAction, RowTone, RowView,
};

pub(crate) const APP_NAME: &str = "SpaceTerm";
/// The heading above the Machine rows on every surface.
pub(crate) const CORE_HEADING: &str = "SpaceTerm Core";
/// The list's last entry, which opens Settings > Updates.
pub(crate) const UPDATE_SETTINGS: &str = "Update Settings\u{2026}";
pub(crate) const DOWNLOAD: &str = "Download";
pub(crate) const RELAUNCH_TO_UPDATE: &str = "Relaunch to Update";
pub(crate) const RESTART_CORE: &str = "Restart Core\u{2026}";
/// Restart Core when it restarts at once, with no confirmation.
pub(crate) const RESTART_CORE_NOW: &str = "Restart Core";
pub(crate) const TRY_AGAIN: &str = "Try Again";
pub(crate) const TRY_AGAIN_ASKS: &str = "Try Again\u{2026}";
pub(crate) const CHECK_AGAIN: &str = "Check Again";
/// A check a person asked for, while it runs.
pub(crate) const CHECKING: &str = "Checking\u{2026}";
/// A check a person asked for that failed.
pub(crate) const COULDNT_CHECK: &str = "Couldn\u{2019}t check for updates";
pub(crate) const HOW_TO_UPDATE: &str = "How to Update\u{2026}";
pub(crate) const KEEP_RUNNING: &str = "Terminal Sessions keep running";
pub(crate) const NOTHING_RUNNING: &str =
    "Nothing running means no program, no typing and no output in any Terminal Session.";

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
        // Release notes are never the row's action: the action slot only updates.
        AppStage::Current { .. } => match app.asked_check {
            Some(AskedCheck::Running) => (
                CHECKING.to_owned(),
                None,
                None,
                format!(
                    "SpaceTerm {} is running and checking for updates.",
                    app.running
                ),
            ),
            Some(AskedCheck::Failed) => (
                COULDNT_CHECK.to_owned(),
                action(CHECK_AGAIN, None, Command::Desktop(DesktopAction::Check)),
                None,
                format!(
                    "SpaceTerm {} couldn\u{2019}t check for updates.",
                    app.running
                ),
            ),
            Some(AskedCheck::Succeeded) => {
                let checked = checked(app.last_check);
                (
                    checked.clone(),
                    None,
                    None,
                    format!("SpaceTerm {} is running. {checked}.", app.running),
                )
            }
            None => (
                app.running.to_string(),
                None,
                None,
                format!("SpaceTerm {} is running.", app.running),
            ),
        },
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
                || "Couldn\u{2019}t download the update".to_owned(),
                |v| format!("Couldn\u{2019}t download {v}"),
            );
            let sentence = version.as_ref().map_or_else(
                || "SpaceTerm couldn\u{2019}t download the update.".to_owned(),
                |v| format!("SpaceTerm couldn\u{2019}t download {v}."),
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
            "Couldn\u{2019}t relaunch".to_owned(),
            action(TRY_AGAIN, None, Command::Desktop(DesktopAction::Update)),
            None,
            "SpaceTerm couldn\u{2019}t relaunch. Terminal Sessions keep running.".to_owned(),
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
    let tone = match &app.stage {
        AppStage::DownloadFailed { .. } | AppStage::RelaunchFailed { .. } => RowTone::Failure,
        AppStage::Current { .. } if app.asked_check == Some(AskedCheck::Failed) => RowTone::Failure,
        AppStage::CantReplace | AppStage::NeedsNewerSystem { .. } => RowTone::Warning,
        AppStage::Available { .. } | AppStage::Downloaded { .. }
            if app.too_old_for.is_some() || app.ladder == Stage::Insist =>
        {
            RowTone::Warning
        }
        _ => RowTone::Plain,
    };
    RowView {
        name: APP_NAME.into(),
        version: Some(app.running.clone()),
        status: Some(status.into()),
        action: row_action,
        accessibility: sentence.into(),
        description: description.map(Into::into),
        tone,
    }
}

/// "Checked just now", "Checked 5 minutes ago": a check the person asked for that succeeded.
fn checked(last: LastCheck) -> String {
    match last {
        LastCheck::Succeeded { age } => format!("Checked {}", super::settings::ago(age)),
        LastCheck::Unknown | LastCheck::Failing { .. } => "Checked".to_owned(),
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
    // A label ends in an ellipsis only when a confirmation follows.
    let asks = row.restart_asks();
    let restart = || {
        action(
            if asks { RESTART_CORE } else { RESTART_CORE_NOW },
            ends.clone(),
            Command::Machine(id, MachineAction::Restart),
        )
    };
    let try_again = if asks { TRY_AGAIN_ASKS } else { TRY_AGAIN };
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
            (None, Some(running)) => match row.asked_check {
                Some(AskedCheck::Running) => (
                    CHECKING.to_owned(),
                    None,
                    None,
                    format!("{subject} runs {running} and is checking for updates."),
                ),
                Some(AskedCheck::Failed) => (
                    COULDNT_CHECK.to_owned(),
                    action(
                        CHECK_AGAIN,
                        None,
                        Command::Machine(id, MachineAction::CheckAgain),
                    ),
                    None,
                    format!("{subject} couldn\u{2019}t check for updates."),
                ),
                Some(AskedCheck::Succeeded) => {
                    let checked = checked(row.last_check);
                    (
                        checked.clone(),
                        None,
                        None,
                        format!("{subject} runs {running}. {checked}."),
                    )
                }
                None => (
                    running.clone(),
                    None,
                    None,
                    format!("{subject} runs {running}."),
                ),
            },
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
        // Check Again on a Machine no route reaches retries its routes for a while.
        MachineStage::Offline if row.asked_check == Some(AskedCheck::Running) => (
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
                        format!("Couldn\u{2019}t restart into {version}"),
                        action(
                            try_again,
                            None,
                            Command::Machine(id, MachineAction::Restart),
                        ),
                        None,
                        format!("{subject} couldn\u{2019}t restart into {version}."),
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
            format!("Couldn\u{2019}t restart into {version}"),
            kept.then(|| {
                action(
                    try_again,
                    None,
                    Command::Machine(id, MachineAction::Restart),
                )
            })
            .flatten(),
            (!kept).then(|| "SpaceTerm waits for the next update.".to_owned()),
            format!("{subject} couldn\u{2019}t restart into {version}."),
        ),
        MachineStage::DownloadFailed {
            version, retries, ..
        } => (
            format!("Couldn\u{2019}t download {version}"),
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
            format!("SpaceTerm couldn\u{2019}t download SpaceTerm Core {version} to {name}."),
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
        MachineStage::CantCheck if row.asked_check == Some(AskedCheck::Running) => (
            CHECKING.to_owned(),
            None,
            None,
            format!("{subject} is checking for updates."),
        ),
        MachineStage::CantCheck => (
            "Can\u{2019}t check for updates".to_owned(),
            action(
                CHECK_AGAIN,
                None,
                Command::Machine(id, MachineAction::CheckAgain),
            ),
            None,
            format!("{subject} can\u{2019}t check for updates."),
        ),
        // Download asks the Core to fetch the Stepping Stone at the floor from its own feed.
        MachineStage::NeedsFloor { version, floor, .. } => (
            blocked(format!("Needs {floor} before {version}")),
            action(DOWNLOAD, None, Command::Machine(id, MachineAction::Install)),
            None,
            if too_old {
                format!(
                    "{subject} is too old for this SpaceTerm and needs {floor} before it can update to {version}."
                )
            } else {
                format!("{subject} needs {floor} before it can update to {version}.")
            },
        ),
        MachineStage::SteppingStoneSkipped => (
            "Can\u{2019}t update by itself".to_owned(),
            action(
                HOW_TO_UPDATE,
                None,
                Command::Explain(dialogs::how_to_update(row)),
            ),
            None,
            format!("{subject} can\u{2019}t update by itself."),
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
    let tone = match &row.stage {
        MachineStage::CantCheck if row.asked_check == Some(AskedCheck::Running) => RowTone::Plain,
        MachineStage::StartFailed { .. }
        | MachineStage::DownloadFailed { .. }
        | MachineStage::CantCheck
        | MachineStage::NeedsFloor { failed: true, .. } => RowTone::Failure,
        MachineStage::Running if row.asked_check == Some(AskedCheck::Failed) => RowTone::Failure,
        MachineStage::NotConfirmed { .. } | MachineStage::SteppingStoneSkipped => RowTone::Warning,
        _ if row.blocked.is_some() || row.ladder == Some(Stage::Insist) => RowTone::Warning,
        _ => RowTone::Plain,
    };
    RowView {
        name: name.clone(),
        version: running.map(Into::into),
        status: Some(status.into()),
        action: row_action,
        description: description.map(Into::into),
        accessibility: sentence.into(),
        tone,
    }
}
