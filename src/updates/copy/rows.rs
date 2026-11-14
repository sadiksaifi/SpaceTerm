//! Row statuses, actions and details: one row model for the title-bar list, Settings > Updates,
//! the Machine card and the menu bar extra. Each row is written once as a [`Shape`]: its
//! versions, a short status, its action, and the work or failure it reports. [`finish`] then
//! chooses the title-bar list's one presentation from that shape, so its label and its command
//! always come from the same state.
use gpui::SharedString;
use spaceterm_core::update::continuation::Continuation;
use spaceterm_core::update::facts::CheckFailure;

use super::{Platform, days, dialogs, sessions};
use crate::updates::confirm::Explanation;
use crate::updates::ladder::Stage;
use crate::updates::machines::MachineAction;
use crate::updates::state::{
    AppRow, AppStage, AskedCheck, Blocked, CheckFailed, Command, DesktopAction, Detail, LastCheck,
    MachineRow, MachineStage, Primary, PrimaryKind, RowAction, RowTone, RowView,
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
pub(crate) const HOW_TO_UPDATE: &str = "How to Update\u{2026}";
pub(crate) const KEEP_RUNNING: &str = "Terminal Sessions keep running";

/// A check a person asked for, while it runs.
pub(crate) const CHECKING: &str = "Checking\u{2026}";
/// A check that failed: one a person asked for, or a Core's own checks failing for a day.
pub(crate) const CHECK_FAILED: &str = "Check failed";
pub(crate) const CONNECTING: &str = "Connecting\u{2026}";
/// Connect and Check that reached no route to the Machine.
pub(crate) const CONNECTION_FAILED: &str = "Connection failed";
pub(crate) const DOWNLOADING: &str = "Downloading\u{2026}";
pub(crate) const DOWNLOAD_FAILED: &str = "Download failed";
pub(crate) const RELAUNCHING: &str = "Relaunching\u{2026}";
pub(crate) const RELAUNCH_FAILED: &str = "Relaunch failed";
/// A restart that has started. A queued automatic restart is [`RESTART_PENDING`].
pub(crate) const RESTARTING: &str = "Restarting\u{2026}";
pub(crate) const RESTART_FAILED: &str = "Restart failed";
/// The Core restarts into its Pending Version by itself in a moment.
pub(crate) const RESTART_PENDING: &str = "Restart pending";
pub(crate) const NOT_RESPONDING: &str = "Not responding";
pub(crate) const OFFLINE: &str = "Offline";
pub(crate) const STOPPED: &str = "Stopped";
/// A push held until SpaceTerm updates or its startup check ends.
pub(crate) const WAITING_FOR_APP: &str = "Waiting for SpaceTerm update";
/// SpaceTerm Core is too old for this SpaceTerm. The row sits under the SpaceTerm Core heading.
pub(crate) const CORE_UPDATE_REQUIRED: &str = "Update required";
/// This SpaceTerm is too old for the Core.
pub(crate) const APP_UPDATE_REQUIRED: &str = "SpaceTerm update required";
/// The Core couldn't start the Stepping Stone it needs, and can't update by itself.
pub(crate) const REQUIRED_UPDATE_FAILED: &str = "Required update failed";

/// What every Terminal Session must show for an hour before SpaceTerm Core restarts by itself.
pub(crate) const NOTHING_RUNNING: &str = "Nothing running means every Terminal Session waits at \
     an empty prompt with no program, background or stopped job, typing or output. Activity \
     SpaceTerm Core can\u{2019}t read counts as running.";

/// One row, before its title-bar list presentation is chosen.
struct Shape {
    status: Option<String>,
    target: Option<String>,
    action: Option<RowAction>,
    /// Work in progress the row reports.
    progress: Option<&'static str>,
    /// A failure: its short status and the details choosing the row opens.
    failure: Option<(String, Explanation)>,
    details: Vec<Detail>,
    sentence: String,
    /// What the title-bar list's description reads when the row reports work or a failure
    /// rather than its full sentence.
    reported: Option<String>,
}

impl Shape {
    fn new(sentence: String) -> Self {
        Self {
            status: None,
            target: None,
            action: None,
            progress: None,
            failure: None,
            details: Vec::new(),
            sentence,
            reported: None,
        }
    }

    fn status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    fn target(mut self, target: impl ToString) -> Self {
        self.target = Some(target.to_string());
        self
    }

    fn action(mut self, action: Option<RowAction>) -> Self {
        self.action = action;
        self
    }

    /// Work in progress, which is also the row's status.
    fn progress(mut self, label: &'static str) -> Self {
        self.progress = Some(label);
        self.status = Some(label.to_owned());
        self
    }

    /// A failure, which is also the row's status.
    fn failure(mut self, status: &str, details: Explanation) -> Self {
        self.status = Some(status.to_owned());
        self.failure = Some((status.to_owned(), details));
        self
    }

    fn detail(mut self, text: impl Into<String>) -> Self {
        self.details.push(Detail {
            text: text.into().into(),
            explanation: None,
        });
        self
    }

    fn explained(mut self, text: impl Into<String>, explanation: &str) -> Self {
        self.details.push(Detail {
            text: text.into().into(),
            explanation: Some(explanation.to_owned().into()),
        });
        self
    }
}

fn action(label: &str, subtitle: Option<String>, command: Command) -> Option<RowAction> {
    Some(RowAction {
        label: SharedString::from(label.to_owned()),
        subtitle: subtitle.map(Into::into),
        command,
    })
}

/// The row from its shape: the title-bar list shows work in progress first, then a failure,
/// then the row's action, then its status or version.
fn finish(name: &str, version: Option<String>, shape: Shape, tone: RowTone) -> RowView {
    let reported = shape.reported.clone();
    let primary = if let Some(label) = shape.progress {
        Primary {
            kind: PrimaryKind::Progress,
            label: label.into(),
            command: None,
            description: reported.unwrap_or_else(|| shape.sentence.clone()).into(),
        }
    } else if let Some((status, details)) = &shape.failure {
        Primary {
            kind: PrimaryKind::Failure,
            // The ellipsis says that choosing the row opens more.
            label: format!("{status}\u{2026}").into(),
            command: Some(Command::Explain(details.clone())),
            description: reported.unwrap_or_else(|| shape.sentence.clone()).into(),
        }
    } else if let Some(action) = &shape.action {
        Primary {
            kind: PrimaryKind::Action,
            label: action.label.clone(),
            command: Some(action.command.clone()),
            description: shape.sentence.clone().into(),
        }
    } else {
        Primary {
            kind: PrimaryKind::Status,
            label: shape
                .status
                .clone()
                .or_else(|| version.clone())
                .unwrap_or_default()
                .into(),
            command: None,
            description: shape.sentence.clone().into(),
        }
    };
    RowView {
        name: name.to_owned().into(),
        version: version.map(Into::into),
        target: shape.target.map(Into::into),
        status: shape.status.map(Into::into),
        action: shape.action,
        primary,
        details: shape.details,
        accessibility: shape.sentence.into(),
        tone,
    }
}

/// "Update waiting 3 days": how long the update has waited, on its inherited clock.
fn waiting(n: u64) -> String {
    format!("Update waiting {}", days(n))
}

/// SpaceTerm's row.
pub(crate) fn app_row(app: &AppRow) -> RowView {
    let keep = || Some(KEEP_RUNNING.to_owned());
    let running = app.running.to_string();
    let check = Command::Desktop(DesktopAction::Check);
    let check_failed = |waiting: Option<String>| {
        // SpaceTerm's own check ran and failed; its updater gives no reason.
        let failed = CheckFailed::Completed(check_reason(app.last_check));
        dialogs::check_failed(None, failed, waiting, check.clone())
    };
    let mut shape = match &app.stage {
        // Release notes are never the row's action: the action slot only updates.
        AppStage::Current { .. } => match app.asked_check {
            Some(AskedCheck::Running) => Shape::new(format!(
                "SpaceTerm {running} is running and checking for updates."
            ))
            .progress(CHECKING),
            Some(AskedCheck::Failed) => Shape::new(format!(
                "SpaceTerm {running} couldn\u{2019}t check for updates."
            ))
            .failure(CHECK_FAILED, check_failed(None))
            .action(action(CHECK_AGAIN, None, check.clone())),
            Some(AskedCheck::Succeeded) => {
                let checked = checked(app.last_check);
                Shape::new(format!("SpaceTerm {running} is running. {checked}.")).status(checked)
            }
            None => Shape::new(format!("SpaceTerm {running} is running.")),
        },
        // A check the person asked for shows beside a release that waits for its download,
        // which keeps Download.
        AppStage::Available { version } => {
            let available =
                format!("SpaceTerm {version} is available. Download gets it without relaunching.");
            let download = action(DOWNLOAD, keep(), Command::Desktop(DesktopAction::Download));
            match app.asked_check {
                Some(AskedCheck::Running) => {
                    let mut shape = Shape::new(format!(
                        "{available} SpaceTerm is checking for updates."
                    ))
                    .progress(CHECKING);
                    shape.reported = Some(format!(
                        "SpaceTerm is checking for updates. SpaceTerm {version} is available."
                    ));
                    shape
                }
                Some(AskedCheck::Failed) => {
                    let mut shape = Shape::new(format!(
                        "{available} SpaceTerm couldn\u{2019}t check for updates."
                    ))
                    .failure(
                        CHECK_FAILED,
                        check_failed(Some(format!("SpaceTerm {version} is still available."))),
                    );
                    shape.reported = Some(format!(
                        "SpaceTerm couldn\u{2019}t check for updates. SpaceTerm {version} is still available."
                    ));
                    shape
                }
                Some(AskedCheck::Succeeded) | None => Shape::new(available),
            }
            .target(version)
            .action(download)
        }
        AppStage::Downloading { version, .. } => {
            Shape::new(format!("Downloading SpaceTerm {version}."))
                .progress(DOWNLOADING)
                .target(version)
        }
        AppStage::Downloaded { version } => Shape::new(format!(
            "SpaceTerm {version} is downloaded. Relaunching keeps Terminal Sessions running."
        ))
        .target(version)
        .action(action(
            RELAUNCH_TO_UPDATE,
            keep(),
            Command::Desktop(DesktopAction::Update),
        )),
        AppStage::Relaunching { version } => {
            Shape::new("Relaunching SpaceTerm. Terminal Sessions keep running.".to_owned())
                .progress(RELAUNCHING)
                .target(version)
        }
        AppStage::DownloadFailed {
            version, retries, ..
        } => {
            let sentence = version.as_ref().map_or_else(
                || "SpaceTerm couldn\u{2019}t download the update.".to_owned(),
                |v| format!("SpaceTerm couldn\u{2019}t download {v}."),
            );
            // The failure the pill reports and dismisses, or one SpaceTerm retries by itself.
            let details = if *retries {
                dialogs::retried_download(None, version.as_deref())
            } else {
                dialogs::app_failure(app)
                    .unwrap_or_else(|| dialogs::retried_download(None, version.as_deref()))
            };
            let shape = Shape::new(sentence).failure(DOWNLOAD_FAILED, details);
            let shape = match version {
                Some(version) => shape.target(version),
                None => shape,
            };
            if *retries {
                shape.detail("SpaceTerm tries again later")
            } else {
                shape.action(action(
                    TRY_AGAIN,
                    None,
                    Command::Desktop(DesktopAction::Download),
                ))
            }
        }
        AppStage::RelaunchFailed { version, error, .. } => {
            let details = dialogs::app_failure(app).unwrap_or_else(|| {
                dialogs::relaunch_failure(
                    version.as_deref(),
                    *error == crate::updates::UpdateError::SettingsUnsaved,
                )
            });
            let shape = Shape::new(
                "SpaceTerm couldn\u{2019}t relaunch. Terminal Sessions keep running.".to_owned(),
            )
            .failure(RELAUNCH_FAILED, details)
            .action(action(
                TRY_AGAIN,
                None,
                Command::Desktop(DesktopAction::Update),
            ));
            match version {
                Some(version) => shape.target(version),
                None => shape,
            }
        }
        AppStage::CantReplace => {
            let status = cant_replace(Platform::HOST);
            let details = dialogs::app_failure(app).unwrap_or_else(|| {
                Explanation::ok(
                    dialogs::cant_replace_title(Platform::HOST),
                    dialogs::cant_replace_message(Platform::HOST),
                )
            });
            Shape::new(format!(
                "{}.",
                dialogs::cant_replace_message(Platform::HOST)
            ))
            .failure(status, details)
        }
        AppStage::NeedsNewerSystem { version } => {
            let status = format!("Requires newer {}", system(Platform::HOST));
            let shape = Shape::new(match version {
                Some(v) => format!("SpaceTerm {v} needs a newer {}.", system(Platform::HOST)),
                None => format!("SpaceTerm needs a newer {}.", system(Platform::HOST)),
            })
            .status(status);
            match version {
                Some(version) => shape.target(version),
                None => shape,
            }
        }
    };
    // Joining a scheduled retry makes its check visible without ending the quiet download
    // retry. Keep that retry stage (and its title-bar policy), but report the person's check.
    if let AppStage::DownloadFailed {
        version: Some(version),
        retries: true,
        ..
    } = &app.stage
    {
        match app.asked_check {
            Some(AskedCheck::Running) => {
                shape.failure = None;
                shape.sentence =
                    format!("SpaceTerm is checking for updates. SpaceTerm {version} is available.");
                shape = shape.progress(CHECKING);
            }
            Some(AskedCheck::Failed) => {
                shape.sentence = format!(
                    "SpaceTerm couldn\u{2019}t check for updates. SpaceTerm {version} is still available."
                );
                shape = shape.failure(
                    CHECK_FAILED,
                    check_failed(Some(format!("SpaceTerm {version} is still available."))),
                );
            }
            Some(AskedCheck::Succeeded) | None => {}
        }
    }
    if app.waiting().is_some() && app.ladder >= Stage::Ask {
        shape = shape.detail(waiting(app.waited));
    }
    let tone = match &app.stage {
        AppStage::DownloadFailed { .. } if app.asked_check == Some(AskedCheck::Running) => {
            RowTone::Plain
        }
        AppStage::DownloadFailed { .. } | AppStage::RelaunchFailed { .. } => RowTone::Failure,
        AppStage::Current { .. } | AppStage::Available { .. }
            if app.asked_check == Some(AskedCheck::Failed) =>
        {
            RowTone::Failure
        }
        AppStage::CantReplace | AppStage::NeedsNewerSystem { .. } => RowTone::Warning,
        AppStage::Available { .. } | AppStage::Downloaded { .. }
            if app.too_old_for.is_some() || app.ladder == Stage::Insist =>
        {
            RowTone::Warning
        }
        _ => RowTone::Plain,
    };
    finish(APP_NAME, Some(running), shape, tone)
}

/// "Checked just now", "Checked 5 minutes ago": a check the person asked for that succeeded.
fn checked(last: LastCheck) -> String {
    match last {
        LastCheck::Succeeded { age } => format!("Checked {}", super::settings::ago(age)),
        LastCheck::Unknown | LastCheck::Never | LastCheck::Failing { .. } => "Checked".to_owned(),
    }
}

/// Why the last check failed, when the facts tell.
fn check_reason(last: LastCheck) -> Option<CheckFailure> {
    match last {
        LastCheck::Failing { reason, .. } => reason,
        LastCheck::Unknown | LastCheck::Never | LastCheck::Succeeded { .. } => None,
    }
}

/// The SpaceTerm row's status when the installation can't be replaced: the remedy, short.
pub(crate) fn cant_replace(platform: Platform) -> &'static str {
    match platform {
        Platform::Mac => "Move to Applications",
        Platform::Linux => "Reinstall to update",
    }
}

/// The system a release can need a newer version of.
pub(crate) fn system(platform: Platform) -> &'static str {
    match platform {
        Platform::Mac => "macOS",
        Platform::Linux => "system",
    }
}

/// "Restarts when 1 Terminal Session ends", "… when 3 Terminal Sessions end".
fn when_sessions_end(n: u32) -> String {
    match n {
        1 => "Restarts when 1 Terminal Session ends".to_owned(),
        n => format!("Restarts when {n} Terminal Sessions end"),
    }
}

/// One Machine's row.
pub(crate) fn machine_row(row: &MachineRow) -> RowView {
    let name = row.name.to_string();
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
    let check_again = || {
        action(
            CHECK_AGAIN,
            None,
            Command::Machine(id, MachineAction::CheckAgain),
        )
    };
    // A check the person asked for explains itself by its own outcome; the Core's own failing
    // checks by their reason.
    let own_checks_failed = CheckFailed::Completed(check_reason(row.last_check));
    let check_failed = |failed: CheckFailed, waiting: Option<String>| {
        dialogs::check_failed(
            Some(&name),
            failed,
            waiting,
            Command::Machine(id, MachineAction::CheckAgain),
        )
    };
    let asked_failed = row.check_failed.unwrap_or(own_checks_failed);
    let too_old = row.blocked == Some(Blocked::CoreTooOld);
    let subject = format!("SpaceTerm Core on {name}");
    // The pill's failure alert when the row has one, so the list and the pill open the same
    // details.
    let failure_alert = || dialogs::machine_failure(row);
    let mut shape = match &row.stage {
        MachineStage::Running => match (row.blocked, &running) {
            (Some(Blocked::CoreTooOld), _) => {
                Shape::new(format!("{subject} is too old for this SpaceTerm."))
                    .status(CORE_UPDATE_REQUIRED)
            }
            (Some(Blocked::AppTooOld), _) => {
                Shape::new(format!("This SpaceTerm is too old for {subject}."))
                    .status(APP_UPDATE_REQUIRED)
            }
            (None, Some(running)) => match row.asked_check {
                Some(AskedCheck::Running) => Shape::new(format!(
                    "{subject} runs {running} and is checking for updates."
                ))
                .progress(CHECKING),
                Some(AskedCheck::Failed) => {
                    Shape::new(format!("{subject} couldn\u{2019}t check for updates."))
                        .failure(CHECK_FAILED, check_failed(asked_failed, None))
                        .action(check_again())
                }
                Some(AskedCheck::Succeeded) => {
                    let checked = checked(row.last_check);
                    Shape::new(format!("{subject} runs {running}. {checked}.")).status(checked)
                }
                None => Shape::new(format!("{subject} runs {running}.")),
            },
            (None, None) => Shape::new(format!("Connecting to {subject}.")).progress(CONNECTING),
        },
        MachineStage::Connecting => {
            Shape::new(format!("Connecting to {subject}.")).progress(CONNECTING)
        }
        // Check Again on a Machine no route reaches retries its routes for a while.
        MachineStage::Offline if row.asked_check == Some(AskedCheck::Running) => {
            Shape::new(format!("Connecting to {subject}.")).progress(CONNECTING)
        }
        // Connect and Check reached no route; Try Again connects and checks again.
        MachineStage::Offline if row.asked_check == Some(AskedCheck::Failed) => Shape::new(
            format!("SpaceTerm couldn\u{2019}t connect to {name} to check for updates."),
        )
        .failure(CONNECTION_FAILED, dialogs::connect_failed(&name, id))
        .action(action(
            TRY_AGAIN,
            None,
            Command::Machine(id, MachineAction::ConnectAndCheck),
        )),
        MachineStage::Offline => Shape::new(format!("{name} is offline.")).status(OFFLINE),
        MachineStage::Stopped => Shape::new(format!("{subject} is stopped.")).status(STOPPED),
        MachineStage::WaitingForApp { version } => Shape::new(format!(
            "SpaceTerm downloads SpaceTerm Core {version} to {name} after the SpaceTerm update."
        ))
        .status(WAITING_FOR_APP)
        .target(version),
        MachineStage::Downloading { version, .. } => {
            let shape = Shape::new(if too_old {
                format!(
                    "{subject} is too old for this SpaceTerm. Downloading SpaceTerm Core {version}."
                )
            } else {
                format!("Downloading SpaceTerm Core {version} to {name}.")
            })
            .progress(DOWNLOADING)
            .target(version);
            if too_old {
                shape.status(format!("{CORE_UPDATE_REQUIRED} \u{b7} {DOWNLOADING}"))
            } else {
                shape
            }
        }
        MachineStage::Downloaded {
            version,
            continuation,
            ..
        } => {
            let consequence = match row.sessions {
                0 => "No Terminal Session is open.".to_owned(),
                n => format!("Restarting SpaceTerm Core now ends {}.", sessions(n)),
            };
            if too_old {
                Shape::new(format!(
                    "{subject} is too old for this SpaceTerm and has {version} downloaded. {consequence}"
                ))
                .status(CORE_UPDATE_REQUIRED)
                .target(version)
                .action(restart())
            } else {
                match continuation {
                    Continuation::Soon => Shape::new(format!(
                        "{subject} restarts into {version} in a moment."
                    ))
                    .status(RESTART_PENDING)
                    .target(version)
                    .detail("Restarts in a moment"),
                    Continuation::WhenSessionsEnd => Shape::new(
                        format!(
                            "{subject} restarts into {version} when its {} end. {consequence}",
                            sessions(row.sessions)
                        )
                        .replace("1 Terminal Session end.", "1 Terminal Session ends."),
                    )
                    .target(version)
                    .action(restart())
                    .detail(when_sessions_end(row.sessions)),
                    Continuation::WhenNothingRuns => Shape::new(format!(
                        "{subject} restarts into {version} after an hour with nothing running. {consequence}"
                    ))
                    .target(version)
                    .action(restart())
                    .explained(
                        "Restarts after an hour with nothing running",
                        NOTHING_RUNNING,
                    ),
                    Continuation::StartFailed => Shape::new(format!(
                        "{subject} couldn\u{2019}t restart into {version}."
                    ))
                    .failure(
                        RESTART_FAILED,
                        dialogs::restart_kept(&name, id, &version.to_string()),
                    )
                    .target(version)
                    .action(action(
                        try_again,
                        None,
                        Command::Machine(id, MachineAction::Restart),
                    )),
                    Continuation::NeedsConfirmation | Continuation::Unknown => Shape::new(
                        format!("{subject} has {version} downloaded. {consequence}"),
                    )
                    .target(version)
                    .action(restart()),
                }
            }
        }
        MachineStage::Restarting { to, .. } => {
            let shape = Shape::new(match to {
                Some(to) => format!("{subject} is restarting into {to}."),
                None => format!("{subject} is restarting."),
            })
            .progress(RESTARTING);
            match to {
                Some(to) => shape.target(to),
                None => shape,
            }
        }
        MachineStage::StartFailed { version, kept, .. } => {
            let details = failure_alert()
                .unwrap_or_else(|| dialogs::restart_kept(&name, id, &version.to_string()));
            let shape = Shape::new(format!("{subject} couldn\u{2019}t restart into {version}."))
                .failure(RESTART_FAILED, details)
                .target(version);
            if *kept {
                shape.action(action(
                    try_again,
                    None,
                    Command::Machine(id, MachineAction::Restart),
                ))
            } else {
                shape.detail("Waiting for the next update")
            }
        }
        MachineStage::DownloadFailed {
            version, retries, ..
        } => {
            let version_text = version.to_string();
            let details = if *retries {
                dialogs::retried_download(Some(&name), Some(&version_text))
            } else {
                failure_alert()
                    .unwrap_or_else(|| dialogs::retried_download(Some(&name), Some(&version_text)))
            };
            let shape = Shape::new(format!(
                "SpaceTerm couldn\u{2019}t download SpaceTerm Core {version} to {name}."
            ))
            .failure(DOWNLOAD_FAILED, details)
            .target(version);
            if *retries {
                shape.detail("SpaceTerm Core tries again later")
            } else {
                shape.action(action(
                    TRY_AGAIN,
                    None,
                    Command::Machine(id, MachineAction::Install),
                ))
            }
        }
        MachineStage::NotConfirmed { version, .. } => Shape::new(format!(
            "{name} stopped responding while SpaceTerm Core restarted."
        ))
        .failure(NOT_RESPONDING, dialogs::not_confirmed(row))
        .target(version)
        .action(check_again()),
        MachineStage::CantCheck if row.asked_check == Some(AskedCheck::Running) => {
            Shape::new(format!("{subject} is checking for updates.")).progress(CHECKING)
        }
        MachineStage::CantCheck => Shape::new(format!("{subject} can\u{2019}t check for updates."))
            .failure(CHECK_FAILED, check_failed(own_checks_failed, None))
            .action(check_again()),
        // Download asks the Core to fetch the Stepping Stone at the floor from its own feed.
        // The row names the Stepping Stone, never the newest release, as what comes next.
        MachineStage::NeedsFloor {
            version,
            floor,
            failed,
        } => {
            let sentence = if too_old {
                format!(
                    "{subject} is too old for this SpaceTerm and needs {floor} before it can update to {version}."
                )
            } else {
                format!("{subject} needs {floor} before it can update to {version}.")
            };
            let download = action(DOWNLOAD, None, Command::Machine(id, MachineAction::Install));
            let shape = if *failed {
                let details =
                    failure_alert().unwrap_or_else(|| dialogs::retried_download(Some(&name), None));
                Shape::new(sentence).failure(DOWNLOAD_FAILED, details)
            } else {
                Shape::new(sentence).status(format!("{floor} required first"))
            };
            shape
                .action(download)
                .detail(format!("Updates to {version} after {floor}"))
        }
        MachineStage::SteppingStoneSkipped => {
            Shape::new(format!("{subject} can\u{2019}t update by itself."))
                .failure(REQUIRED_UPDATE_FAILED, dialogs::how_to_update(row))
                .action(action(
                    HOW_TO_UPDATE,
                    None,
                    Command::Explain(dialogs::how_to_update(row)),
                ))
        }
    };
    // A check the person asked for leads the row on every stage that doesn't already present
    // it, unless work the row reports is in progress. The row's own update or retry action and
    // its versions stay beside it in Settings; the title-bar list shows the check, and a failure
    // the check displaces stays among the details.
    let presents_check = match &row.stage {
        MachineStage::Running => row.blocked.is_none() && running.is_some(),
        MachineStage::Offline
        | MachineStage::Connecting
        | MachineStage::Downloading { .. }
        | MachineStage::Restarting { .. } => true,
        MachineStage::CantCheck => row.asked_check == Some(AskedCheck::Running),
        _ => false,
    };
    let downloaded = match &row.stage {
        MachineStage::Downloaded { version, .. } => Some(version.to_string()),
        _ => None,
    };
    let overlaid = (!presents_check).then_some(row.asked_check).flatten();
    if matches!(overlaid, Some(AskedCheck::Running | AskedCheck::Failed)) {
        let displaced = shape.failure.take().map(|(status, _)| status);
        if let Some(status) = displaced.filter(|status| status != CHECK_FAILED) {
            shape = shape.detail(status);
        }
    }
    match overlaid {
        Some(AskedCheck::Running) => {
            shape.sentence = format!("{} It is checking for updates.", shape.sentence);
            shape.reported = Some(match &downloaded {
                Some(version) => {
                    format!("{subject} is checking for updates. {version} is downloaded.")
                }
                None => format!("{subject} is checking for updates."),
            });
            shape = shape.progress(CHECKING);
        }
        Some(AskedCheck::Failed) => {
            shape.sentence = format!("{} It couldn\u{2019}t check for updates.", shape.sentence);
            shape.reported = Some(match &downloaded {
                Some(version) => format!(
                    "{subject} couldn\u{2019}t check for updates. {version} stays downloaded."
                ),
                None => format!("{subject} couldn\u{2019}t check for updates."),
            });
            let waiting = downloaded
                .as_ref()
                .map(|version| format!("SpaceTerm Core {version} stays downloaded."));
            shape = shape.failure(CHECK_FAILED, check_failed(asked_failed, waiting));
        }
        Some(AskedCheck::Succeeded) | None => {}
    }
    // From Ask, a waiting row says how long the update waited, on demand.
    if row.ladder.is_some_and(|stage| stage >= Stage::Ask) {
        shape = shape.detail(waiting(row.waited));
    }
    let tone = match &row.stage {
        _ if overlaid == Some(AskedCheck::Failed) => RowTone::Failure,
        _ if overlaid == Some(AskedCheck::Running) => {
            if row.blocked.is_some() || row.ladder == Some(Stage::Insist) {
                RowTone::Warning
            } else {
                RowTone::Plain
            }
        }
        MachineStage::CantCheck if row.asked_check == Some(AskedCheck::Running) => RowTone::Plain,
        MachineStage::StartFailed { .. }
        | MachineStage::DownloadFailed { .. }
        | MachineStage::CantCheck
        | MachineStage::NeedsFloor { failed: true, .. } => RowTone::Failure,
        MachineStage::Downloaded {
            continuation: Continuation::StartFailed,
            ..
        } if !too_old => RowTone::Failure,
        MachineStage::Running | MachineStage::Offline
            if row.asked_check == Some(AskedCheck::Failed) =>
        {
            RowTone::Failure
        }
        MachineStage::NotConfirmed { .. } | MachineStage::SteppingStoneSkipped => RowTone::Warning,
        _ if row.blocked.is_some() || row.ladder == Some(Stage::Insist) => RowTone::Warning,
        _ => RowTone::Plain,
    };
    finish(&name, running, shape, tone)
}
