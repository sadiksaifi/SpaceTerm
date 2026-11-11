//! The split pill: label, glyph, emphasis and tooltip. The labels are the closed set of design
//! section 4.5; none carries a count or a Machine's name.
use gpui::SharedString;

use super::{days, dialogs, sessions, this_ends};
use crate::updates::ladder::Stage;
use crate::updates::machines::MachineAction;
use crate::updates::state::{
    ActKind, AppStage, Blocked, Command, DesktopAction, Glyph, MachineRow, MachineStage,
    PillView, Step, Target, UpdateView,
};
use spaceterm_core::update::continuation::Continuation;

pub(crate) const UPDATE_FAILED: &str = "Update Failed";
pub(crate) const RESTART_NOT_CONFIRMED: &str = "Restart Not Confirmed";
pub(crate) const RESTARTING_CORE: &str = "Restarting Core\u{2026}";
pub(crate) const DOWNLOADING_UPDATE: &str = "Downloading Update\u{2026}";
pub(crate) const RELAUNCHING: &str = "Relaunching\u{2026}";
pub(crate) const RELAUNCH_TO_UPDATE: &str = "Relaunch to Update";
pub(crate) const RESTART_CORE: &str = "Restart Core\u{2026}";
pub(crate) const DOWNLOAD_UPDATE: &str = "Download Update";
pub(crate) const WHATS_NEW: &str = "What's New";

/// Every main-area label.
pub(crate) const LABELS: [&str; 9] = [
    UPDATE_FAILED,
    RESTART_NOT_CONFIRMED,
    RESTARTING_CORE,
    DOWNLOADING_UPDATE,
    RELAUNCHING,
    RELAUNCH_TO_UPDATE,
    RESTART_CORE,
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
                let alert = match target {
                    Target::App => dialogs::app_failure(&view.app)?,
                    Target::Machine(id) => dialogs::machine_failure(machine(id)?)?,
                };
                (
                    UPDATE_FAILED,
                    Glyph::UpdateFailed,
                    first_sentence(&alert.message),
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
                (RESTARTING_CORE, Glyph::Working, tooltip, Command::ShowList, None)
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
                (DOWNLOADING_UPDATE, Glyph::Working, tooltip, Command::ShowList, progress)
            }
            Step::Relaunching => (
                RELAUNCHING,
                Glyph::Working,
                "Relaunching SpaceTerm. Terminal Sessions keep running.".to_owned(),
                Command::ShowList,
                None,
            ),
            Step::Act { kind, stage, .. } => {
                let asking = matches!(stage, Some(Stage::Ask | Stage::Insist));
                match kind {
                    ActKind::Relaunch => (
                        RELAUNCH_TO_UPDATE,
                        Glyph::Restart,
                        if asking {
                            format!(
                                "An update has waited {}. Relaunching keeps Terminal Sessions running.",
                                days(view.app.waited)
                            )
                        } else {
                            format!(
                                "SpaceTerm {} is downloaded. Relaunching keeps Terminal Sessions running.",
                                view.app.waiting().cloned().unwrap_or_default()
                            )
                        },
                        Command::Desktop(DesktopAction::Update),
                        None,
                    ),
                    ActKind::RestartCore(id) => {
                        let row = machine(id)?;
                        (
                            RESTART_CORE,
                            Glyph::UpdateCore,
                            restart_tooltip(row, asking),
                            Command::Machine(id, MachineAction::Restart),
                            None,
                        )
                    }
                    ActKind::Download => (
                        DOWNLOAD_UPDATE,
                        Glyph::Download,
                        format!(
                            "SpaceTerm {} is available.",
                            view.app.waiting().cloned().unwrap_or_default()
                        ),
                        Command::Desktop(DesktopAction::Download),
                        None,
                    ),
                }
            }
            Step::WhatsNew => (
                WHATS_NEW,
                Glyph::WhatsNew,
                format!("See what's new in SpaceTerm {}.", view.app.running),
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

/// The tooltip of `Restart Core…`: blocked, then waiting from Ask, then breaking.
fn restart_tooltip(row: &MachineRow, asking: bool) -> String {
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
            "{name}'s Workspaces open after SpaceTerm Core restarts into {version}. {}",
            this_ends(n)
        );
    }
    if asking {
        let ends = match n {
            0 => "No Terminal Session is open.".to_owned(),
            n => format!("Restarting into {version} ends {}.", sessions(n)),
        };
        return format!("An update has waited {} on {name}. {ends}", days(row.waited));
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

/// The first sentence of an alert's body, for the tooltip of `Update Failed`.
fn first_sentence(text: &SharedString) -> String {
    match text.find(". ") {
        Some(end) => text[..=end].to_owned(),
        None => text.to_string(),
    }
}
