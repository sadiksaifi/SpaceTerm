//! What every update surface shows: one state for the desktop and one per Machine, with its
//! labels, tone, glyph, full sentence and the command a click performs. The title-bar control,
//! its menu and the menu bar extra render from this module and derive no state themselves.
//!
//! Labels follow the agreed copy: control labels are at most two words in sentence case, buttons
//! and menu items use title case, and an item that asks before it acts ends in an ellipsis.
#![allow(
    dead_code,
    reason = "the title-bar control and the menu bar extra each read part of this model"
)]

use std::collections::BTreeSet;

use gpui::{App, SharedString};
use spaceterm_core::protocol::capability::Unavailable;
use spaceterm_core::protocol::version::Incompatible;
use spaceterm_core::update::facts::{
    CheckFailure, FailureKind, InstallFailure, Operation, StartFailure, UpdateCheck, Version,
};

use super::machines::store::MachineUpdates;
use super::machines::{
    ActionFailure, MachineAction, MachineId, MachineLink, MachinePreview, MachineUpdate,
};
use super::policy::{DAY, UpdateStage};
use super::{UpdateError, UpdateService, UpdateState};

/// How long a Machine's update checks fail before it shows "Can't check".
pub(crate) const CANT_CHECK_AFTER: u64 = DAY;

/// Blue for action and progress, orange for problems and overdue states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Action,
    Problem,
}

/// One glyph per state. App-drawn surfaces map these to Lucide icons; native menus use their
/// own symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Glyph {
    /// A spinner, determinate when the item carries a fraction.
    Working,
    /// Download a desktop release that is not downloaded yet.
    Download,
    /// Relaunch the desktop or restart a Core into its Pending Version.
    Restart,
    Overdue,
    /// The desktop is too old for a Machine.
    UpdateDesktop,
    /// A Machine's Core is too old for this desktop.
    UpdateCore,
    WhatsNew,
    UpdateFailed,
    InstallFailed,
    CantConfirm,
    CantCheck,
}

/// What a click does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Desktop(DesktopAction),
    /// Performs the action at once.
    Machine(MachineId, MachineAction),
    /// Asks first, then performs every action.
    Confirm(Confirmation),
    /// Opens an alert with the full sentence. OK then runs `then`.
    Explain(Explanation),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DesktopAction {
    /// Relaunches into a ready release without asking, or downloads it first.
    Update,
    WhatsNew,
    /// Clears a failed desktop install; the desktop tries again later.
    DismissFailure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Confirmation {
    pub(crate) title: SharedString,
    pub(crate) message: SharedString,
    /// The destructive button: "Restart" or "Stop".
    pub(crate) button: SharedString,
    pub(crate) actions: Vec<(MachineId, MachineAction)>,
    /// The work the alert described. Approving it authorizes exactly that work.
    pub(crate) approval: Approval,
}

/// What a confirmation described for each Machine it affects whose facts are known: the sessions
/// it closes, the Core that runs and the Pending Version it restarts into. A surface checks
/// [`Approval::holds`] against the current facts before it executes the approved previews, and
/// previews again when it does not hold. The Core refuses its own preview's token once the
/// sessions or the Pending Version changed, which also covers a Machine without facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Approval {
    machines: Vec<Described>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Described {
    machine: MachineId,
    sessions: u32,
    running: Option<Version>,
    target: Option<Version>,
    breaking: bool,
}

impl Described {
    fn of(update: &MachineUpdate) -> Self {
        let facts = update.facts.as_ref();
        let pending = facts.and_then(|facts| facts.pending.as_ref());
        Self {
            machine: update.machine,
            sessions: facts.map_or(0, |facts| facts.sessions),
            running: facts.map(|facts| facts.running.clone()),
            target: pending.map(|pending| pending.version.clone()),
            breaking: pending.is_some_and(|pending| pending.breaking),
        }
    }
}

impl Approval {
    fn of(views: &[MachineView]) -> Self {
        Self {
            machines: views
                .iter()
                // Without facts there is nothing to compare; the Core's token guards alone.
                .filter(|view| view.running.is_some())
                .map(|view| Described {
                    machine: view.machine,
                    sessions: view.sessions,
                    running: view.running.clone(),
                    target: view.state.waiting().map(|(to, _, _)| to.clone()),
                    breaking: view
                        .state
                        .waiting()
                        .is_some_and(|(_, breaking, _)| breaking),
                })
                .collect(),
        }
    }

    /// Whether the person's approval still covers a new `preview` of its work. A restart the
    /// alert described into a Pending Version is not covered once the Core previews none: that
    /// update already ran or went away.
    pub(crate) fn still_approves(&self, preview: &MachinePreview) -> bool {
        !(preview.action == MachineAction::Restart
            && preview.target.is_none()
            && self
                .target(preview.machine)
                .is_some_and(|target| target.is_some()))
    }

    /// Whether a new `preview` restarts into another Pending Version than the alert described.
    pub(crate) fn retargets(&self, preview: &MachinePreview) -> bool {
        preview.action == MachineAction::Restart
            && preview.target.is_some()
            && self
                .target(preview.machine)
                .is_some_and(|target| target != preview.target.as_ref())
    }

    /// The Pending Version described for `machine`, when the alert described it.
    fn target(&self, machine: MachineId) -> Option<Option<&Version>> {
        self.machines
            .iter()
            .find(|described| described.machine == machine)
            .map(|described| described.target.as_ref())
    }

    /// Whether `machines` still show the work the alert described: the same sessions, running
    /// Core and Pending Version on every affected Machine. Other changes keep the approval.
    pub(crate) fn holds(&self, machines: &[MachineUpdate]) -> bool {
        self.machines.iter().all(|described| {
            machines
                .iter()
                .find(|update| update.machine == described.machine)
                .is_some_and(|update| Described::of(update) == *described)
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Explanation {
    pub(crate) title: SharedString,
    pub(crate) message: SharedString,
    pub(crate) then: Option<Box<Command>>,
}

/// The desktop's own update, as the person sees it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DesktopState {
    Current,
    /// A release waits, and automatic downloads are off.
    Available {
        version: String,
    },
    Downloading {
        version: String,
        fraction: Option<f32>,
    },
    /// Downloaded and verified. Relaunching installs it.
    Ready {
        version: String,
    },
    /// One day old or more. `stage` is `Overdue` once the next launch installs it.
    Overdue {
        version: String,
        stage: UpdateStage,
    },
    /// A Machine's Core is newer than this desktop can talk to.
    TooOld {
        version: Option<String>,
        machine: SharedString,
    },
    InstallFailed {
        version: Option<String>,
        error: UpdateError,
    },
    Relaunching {
        version: String,
    },
    /// This build came by an update and its release notes are unread.
    WhatsNew {
        version: String,
    },
}

/// What the desktop's update service knows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DesktopFacts<'a> {
    pub(crate) state: &'a UpdateState,
    /// The retained pending version, which survives a failed check or download.
    pub(crate) pending: Option<&'a str>,
    pub(crate) stage: UpdateStage,
    pub(crate) notes_unread: bool,
    /// The running desktop version.
    pub(crate) running: &'a str,
}

/// One Machine's update state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MachineState {
    /// Nothing pending.
    Current,
    /// Connected, with no facts yet.
    Connecting,
    /// A Pending Version waits for a restart.
    Ready {
        to: Version,
        /// Its protocol majors differ from the running Core's, so its restart always asks.
        breaking: bool,
        stranded: Vec<String>,
    },
    /// The Pending Version has waited a day or more.
    Overdue {
        to: Version,
        days: u64,
        breaking: bool,
        stranded: Vec<String>,
    },
    /// The Core is too old for this desktop. `push` is the version this desktop can send.
    CoreTooOld {
        push: Option<Version>,
    },
    /// This desktop is too old for the Core.
    DesktopTooOld,
    Installing {
        to: Version,
        fraction: Option<f32>,
    },
    Restarting {
        to: Version,
    },
    Checking,
    Stopping,
    Uninstalling,
    /// The Pending Version did not start, and the Core went back to the running version.
    UpdateFailed {
        id: u64,
        version: Version,
    },
    InstallFailed {
        id: u64,
        version: Version,
        reason: InstallFailure,
    },
    /// A restart began, and the Core did not come back on `expected`.
    CantConfirm {
        expected: Version,
    },
    CantCheck {
        reason: CheckProblem,
        push: Option<Version>,
    },
    Stopped,
    Offline,
}

/// Why a Machine can't check for updates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckProblem {
    /// Every check has failed for [`CANT_CHECK_AFTER`] or longer.
    Failing(CheckFailure),
    /// Its SpaceTerm Core reports no update facts ([`MachineLink::Unreported`]).
    Unreported,
}

impl MachineState {
    /// Whether the title-bar control counts this state as an item.
    pub(crate) fn pending(&self) -> bool {
        !matches!(
            self,
            Self::Current
                | Self::Connecting
                | Self::DesktopTooOld
                | Self::Stopping
                | Self::Uninstalling
                | Self::Stopped
                | Self::Offline
        )
    }

    /// The waiting Pending Version, whether it is breaking, and the devices it strands.
    fn waiting(&self) -> Option<(&Version, bool, &[String])> {
        match self {
            Self::Ready {
                to,
                breaking,
                stranded,
            }
            | Self::Overdue {
                to,
                breaking,
                stranded,
                ..
            } => Some((to, *breaking, stranded)),
            _ => None,
        }
    }

    fn tone(&self) -> Tone {
        match self {
            Self::Overdue { .. }
            | Self::CoreTooOld { .. }
            | Self::DesktopTooOld
            | Self::UpdateFailed { .. }
            | Self::InstallFailed { .. }
            | Self::CantConfirm { .. }
            | Self::CantCheck { .. } => Tone::Problem,
            _ => Tone::Action,
        }
    }

    fn glyph(&self) -> Option<Glyph> {
        Some(match self {
            Self::Ready { .. } => Glyph::Restart,
            Self::Overdue { .. } => Glyph::Overdue,
            Self::CoreTooOld { .. } => Glyph::UpdateCore,
            Self::Installing { .. }
            | Self::Restarting { .. }
            | Self::Checking
            | Self::Stopping
            | Self::Uninstalling
            | Self::Connecting => Glyph::Working,
            Self::UpdateFailed { .. } => Glyph::UpdateFailed,
            Self::InstallFailed { .. } => Glyph::InstallFailed,
            Self::CantConfirm { .. } => Glyph::CantConfirm,
            Self::CantCheck { .. } => Glyph::CantCheck,
            Self::DesktopTooOld => Glyph::UpdateDesktop,
            Self::Current | Self::Stopped | Self::Offline => return None,
        })
    }

    fn fraction(&self) -> Option<f32> {
        match self {
            Self::Installing { fraction, .. } => *fraction,
            _ => None,
        }
    }

    fn working(&self) -> bool {
        self.glyph() == Some(Glyph::Working)
    }

    /// Lower is more urgent. "N updates" shows the most urgent item's glyph.
    fn urgency(&self) -> u8 {
        match self {
            Self::UpdateFailed { .. } => 0,
            Self::InstallFailed { .. } => 1,
            Self::CantConfirm { .. } => 4,
            Self::CantCheck { .. } => 5,
            Self::CoreTooOld { .. } => 6,
            Self::Overdue { .. } => 7,
            Self::Ready { .. } => 9,
            Self::Installing { .. } => 11,
            Self::Restarting { .. } => 13,
            _ => 14,
        }
    }
}

/// One Machine and its state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MachineView {
    pub(crate) machine: MachineId,
    /// "This Mac", "This Computer" or the paired name.
    pub(crate) name: SharedString,
    pub(crate) local: bool,
    /// The version the Core runs, when known.
    pub(crate) running: Option<Version>,
    pub(crate) sessions: u32,
    pub(crate) state: MachineState,
}

/// Failure notices the person dismissed on this desktop, by Machine and failure ID.
pub(crate) type Dismissed = BTreeSet<(MachineId, u64)>;

impl MachineView {
    pub(crate) fn new(update: &MachineUpdate, dismissed: &Dismissed, now: u64) -> Self {
        let facts = update.facts.as_ref();
        Self {
            machine: update.machine,
            name: update.name.clone(),
            local: update.local,
            running: facts.map(|facts| facts.running.clone()),
            sessions: facts.map_or(0, |facts| facts.sessions),
            state: machine_state(update, dismissed, now),
        }
    }

    /// The Machine's name inside a sentence: "this Mac", "this computer" or the paired name.
    fn place(&self) -> String {
        if self.local {
            LOCAL_PLACE.to_owned()
        } else {
            self.name.to_string()
        }
    }

    /// The Machine's name at the start of a sentence.
    fn subject(&self) -> String {
        self.name.to_string()
    }

    /// Where the sessions run, after "your N running sessions": empty for the local Machine.
    fn there(&self) -> &'static str {
        if self.local { "" } else { " there" }
    }

    /// Whether restarting asks first: it closes sessions or crosses a breaking change.
    fn restart_asks(&self) -> bool {
        self.sessions > 0
            || self
                .state
                .waiting()
                .is_some_and(|(_, breaking, _)| breaking)
    }

    /// Restarts at once, or asks first when sessions close or other devices stop working.
    pub(crate) fn restart_command(&self) -> Command {
        if self.restart_asks() {
            Command::Confirm(restart_confirmation(std::slice::from_ref(self)))
        } else {
            Command::Machine(self.machine, MachineAction::Restart)
        }
    }

    /// Stops at once with no sessions open, otherwise asks first.
    pub(crate) fn stop_command(&self) -> Command {
        if self.sessions == 0 {
            return Command::Machine(self.machine, MachineAction::Stop);
        }
        Command::Confirm(self.stop_confirmation())
    }

    fn stop_confirmation(&self) -> Confirmation {
        let stays = "SpaceTerm Core stays stopped until you start it again.";
        let message = match self.sessions {
            0 => stays.to_owned(),
            n => format!("This will close your {}. {stays}", running_sessions(n)),
        };
        Confirmation {
            title: format!("Stop SpaceTerm Core on {}?", self.place()).into(),
            message: message.into(),
            button: "Stop".into(),
            actions: vec![(self.machine, MachineAction::Stop)],
            approval: Approval::of(std::slice::from_ref(self)),
        }
    }

    /// The full sentence: the tooltip, the accessibility description and the alert text.
    pub(crate) fn sentence(&self) -> SharedString {
        let place = self.place();
        let name = self.subject();
        let running = self.running_text();
        match &self.state {
            MachineState::Current => match self.sessions {
                0 => format!("SpaceTerm Core {running} on {place} is up to date."),
                n => format!(
                    "SpaceTerm Core {running} on {place} is up to date and runs {}.",
                    sessions(n)
                ),
            },
            MachineState::Connecting => format!("Connecting to SpaceTerm Core on {place}."),
            MachineState::Ready { .. } | MachineState::Overdue { .. } => self.restart_sentence(),
            MachineState::CoreTooOld { push } => match push {
                Some(to) => format!(
                    "SpaceTerm Core on {place} is too old for this SpaceTerm. Updating installs \
                     SpaceTerm Core {to} and keeps {place}'s sessions open."
                ),
                None => format!("SpaceTerm Core on {place} is too old for this SpaceTerm."),
            },
            MachineState::DesktopTooOld => format!(
                "Update SpaceTerm to open the Workspaces on {place}. Its sessions keep running."
            ),
            MachineState::Installing { to, .. } => {
                format!("Installing SpaceTerm Core {to} on {place}.")
            }
            MachineState::Restarting { .. } => format!("Restarting SpaceTerm Core on {place}."),
            MachineState::Checking => format!("Checking SpaceTerm Core on {place}."),
            MachineState::Stopping => format!("Stopping SpaceTerm Core on {place}."),
            MachineState::Uninstalling => format!("Uninstalling SpaceTerm Core on {place}."),
            MachineState::UpdateFailed { version, .. } => format!(
                "SpaceTerm Core {version} could not start, so SpaceTerm went back to {running}. \
                 Your Workspaces are restored."
            ),
            MachineState::InstallFailed {
                version, reason, ..
            } => install_failure(&name, &place, version, *reason),
            MachineState::CantConfirm { expected } => {
                format!(
                    "{name} stopped answering during its restart, so SpaceTerm can't confirm \
                     that SpaceTerm Core {expected} started."
                )
            }
            MachineState::CantCheck { reason, push } => {
                let cause = check_failure(&name, *reason);
                match push {
                    Some(to) => format!(
                        "{cause}, so it can't check for updates. SpaceTerm can send SpaceTerm \
                         Core {to} from {LOCAL_PLACE}."
                    ),
                    None => format!(
                        "{cause}, so it can't check for updates. {}",
                        check_remedy(*reason)
                    ),
                }
            }
            MachineState::Stopped => {
                format!("SpaceTerm Core on {place} is stopped. Its Workspaces open when it starts.")
            }
            MachineState::Offline => format!(
                "{name} is offline. It was last seen with SpaceTerm Core {running}, so it \
                 updates when it is back."
            ),
        }
        .into()
    }

    fn running_text(&self) -> String {
        self.running
            .as_ref()
            .map_or_else(String::new, Version::to_string)
    }

    fn restart_sentence(&self) -> String {
        let place = self.place();
        let mut text = format!("Restart SpaceTerm Core on {place} to finish the update.");
        if self.sessions > 0 {
            text.push_str(&format!(
                " This will close your {}{}.",
                running_sessions(self.sessions),
                self.there()
            ));
        }
        if let MachineState::Overdue { days: waited, .. } = &self.state {
            text.push_str(&format!(
                " It has waited {}. SpaceTerm never restarts it while sessions are open.",
                days(*waited)
            ));
        }
        if let Some((_, _, stranded)) = self.state.waiting()
            && !stranded.is_empty()
        {
            text.push(' ');
            text.push_str(&stranded_sentence(&place, stranded));
        }
        text
    }

    /// The title-bar label: at most two words.
    fn control_label(&self) -> &'static str {
        match &self.state {
            // The ellipsis says the button asks first.
            MachineState::Ready { .. } if self.restart_asks() => "Restart…",
            MachineState::Ready { .. } => "Restart",
            MachineState::Overdue { .. } if self.restart_asks() => "Restart overdue…",
            MachineState::Overdue { .. } => "Restart overdue",
            MachineState::CoreTooOld { .. } => "Update needed",
            MachineState::Installing { .. } => "Installing…",
            MachineState::Restarting { .. } => "Restarting…",
            MachineState::Checking | MachineState::Connecting => "Checking…",
            MachineState::Stopping => "Stopping…",
            MachineState::Uninstalling => "Uninstalling…",
            MachineState::UpdateFailed { .. } => "Update failed",
            MachineState::InstallFailed { .. } => "Install failed",
            MachineState::CantConfirm { .. } => "Can't confirm",
            MachineState::CantCheck { .. } => "Can't check",
            MachineState::DesktopTooOld => "Update SpaceTerm",
            MachineState::Current => "Current",
            MachineState::Stopped => "Stopped",
            MachineState::Offline => "Offline",
        }
    }

    /// What the single title-bar button does, or None while the item is in progress.
    fn control_command(&self) -> Option<Command> {
        let machine = self.machine;
        Some(match &self.state {
            MachineState::Ready { .. } | MachineState::Overdue { .. } => self.restart_command(),
            MachineState::CoreTooOld { .. } => Command::Machine(machine, MachineAction::Install),
            MachineState::CantConfirm { .. } => {
                Command::Machine(machine, MachineAction::CheckAgain)
            }
            MachineState::UpdateFailed { id, version } => Command::Explain(Explanation {
                title: format!(
                    "SpaceTerm Core {version} couldn't start on {}",
                    self.place()
                )
                .into(),
                message: self.sentence(),
                then: Some(Box::new(Command::Machine(
                    machine,
                    MachineAction::DismissFailure(*id),
                ))),
            }),
            MachineState::InstallFailed { id, version, .. } => Command::Explain(Explanation {
                title: format!(
                    "Couldn't install SpaceTerm Core {version} on {}",
                    self.place()
                )
                .into(),
                message: format!(
                    "{} SpaceTerm tries the install again later.",
                    self.sentence()
                )
                .into(),
                then: Some(Box::new(Command::Machine(
                    machine,
                    MachineAction::DismissFailure(*id),
                ))),
            }),
            MachineState::CantCheck { push: Some(_), .. } => {
                Command::Machine(machine, MachineAction::Install)
            }
            MachineState::CantCheck { reason, push: None } => {
                let name = self.subject();
                Command::Explain(Explanation {
                    title: format!("{name} can't check for updates").into(),
                    message: format!(
                        "{}, and {LOCAL_PLACE} doesn't have a newer SpaceTerm Core for {name}'s \
                         platform. {}",
                        check_failure(&name, *reason),
                        check_remedy(*reason)
                    )
                    .into(),
                    then: None,
                })
            }
            MachineState::Stopped => Command::Machine(machine, MachineAction::Start),
            _ => return None,
        })
    }

    /// The in-window menu row: a short status and at most one button.
    fn menu_row(&self) -> Option<MenuRow> {
        let (status, button): (String, Option<(&str, Command)>) = match &self.state {
            MachineState::Ready { stranded, .. } => {
                let status = if self.sessions > 0 {
                    format!("Closes {}", sessions(self.sessions))
                } else if !stranded.is_empty() {
                    format!("Stops {}", count(stranded.len(), "app"))
                } else {
                    "Ready".to_owned()
                };
                let label = if self.restart_asks() {
                    "Restart…"
                } else {
                    "Restart"
                };
                (status, Some((label, self.restart_command())))
            }
            MachineState::Overdue { .. } => (
                format!("Overdue · {}", sessions(self.sessions)),
                Some((
                    if self.restart_asks() {
                        "Restart…"
                    } else {
                        "Restart"
                    },
                    self.restart_command(),
                )),
            ),
            MachineState::CoreTooOld { .. } => (
                "Too old".to_owned(),
                Some((
                    "Update",
                    Command::Machine(self.machine, MachineAction::Install),
                )),
            ),
            MachineState::Installing { .. }
            | MachineState::Restarting { .. }
            | MachineState::Checking => (self.control_label().to_owned(), None),
            MachineState::UpdateFailed { id, .. } => (
                format!("Back on {}", self.running_text()),
                Some((
                    "Dismiss Notice",
                    Command::Machine(self.machine, MachineAction::DismissFailure(*id)),
                )),
            ),
            MachineState::InstallFailed { .. } => (
                "Install failed".to_owned(),
                Some((
                    "Try Again",
                    Command::Machine(self.machine, MachineAction::Install),
                )),
            ),
            MachineState::CantConfirm { .. } => (
                "Can't confirm".to_owned(),
                Some((
                    "Check Again",
                    Command::Machine(self.machine, MachineAction::CheckAgain),
                )),
            ),
            MachineState::CantCheck { push, .. } => (
                "Can't check".to_owned(),
                push.as_ref().map(|_| {
                    (
                        UPDATE_FROM_LOCAL,
                        Command::Machine(self.machine, MachineAction::Install),
                    )
                }),
            ),
            MachineState::Offline => ("Offline".to_owned(), None),
            _ => return None,
        };
        Some(MenuRow {
            key: ItemKey::Machine(self.machine),
            name: self.name.clone(),
            status: status.into(),
            tone: if self.state == MachineState::Offline {
                None
            } else {
                Some(self.state.tone())
            },
            glyph: self.state.glyph(),
            fraction: self.state.fraction(),
            sentence: self.sentence(),
            button: button.map(|(label, command)| RowButton {
                label: label.into(),
                command,
            }),
        })
    }

    /// The menu bar extra's state after "Version X · ".
    fn extra_state(&self) -> (String, Tone, ExtraTile) {
        let quiet = |text: String| (text, Tone::Action, ExtraTile::Connected);
        match &self.state {
            MachineState::Current => quiet(if self.sessions == 0 {
                "No sessions".to_owned()
            } else {
                sessions(self.sessions)
            }),
            MachineState::Connecting => ("Connecting…".to_owned(), Tone::Action, ExtraTile::Busy),
            MachineState::Ready { .. } => quiet("Update ready".to_owned()),
            MachineState::Overdue { .. } => (
                "Restart overdue".to_owned(),
                Tone::Problem,
                ExtraTile::Connected,
            ),
            MachineState::CoreTooOld { .. } => {
                ("Too old".to_owned(), Tone::Problem, ExtraTile::Connected)
            }
            MachineState::DesktopTooOld => (
                "Needs newer SpaceTerm".to_owned(),
                Tone::Problem,
                ExtraTile::Connected,
            ),
            MachineState::Installing { to, .. } => {
                (format!("Installing {to}…"), Tone::Action, ExtraTile::Busy)
            }
            MachineState::Restarting { .. } => {
                ("Restarting…".to_owned(), Tone::Action, ExtraTile::Busy)
            }
            MachineState::Checking => ("Checking…".to_owned(), Tone::Action, ExtraTile::Busy),
            MachineState::Stopping => ("Stopping…".to_owned(), Tone::Action, ExtraTile::Busy),
            MachineState::Uninstalling => {
                ("Uninstalling…".to_owned(), Tone::Action, ExtraTile::Busy)
            }
            MachineState::UpdateFailed { .. } => (
                "Update failed".to_owned(),
                Tone::Problem,
                ExtraTile::Connected,
            ),
            MachineState::InstallFailed { .. } => (
                "Install failed".to_owned(),
                Tone::Problem,
                ExtraTile::Connected,
            ),
            MachineState::CantConfirm { .. } => (
                "Can't confirm".to_owned(),
                Tone::Problem,
                ExtraTile::Connected,
            ),
            MachineState::CantCheck { .. } => (
                "Can't check for updates".to_owned(),
                Tone::Problem,
                ExtraTile::Connected,
            ),
            MachineState::Stopped => ("Stopped".to_owned(), Tone::Action, ExtraTile::Off),
            MachineState::Offline => ("Offline".to_owned(), Tone::Action, ExtraTile::Offline),
        }
    }

    /// The menu bar extra's row for this Machine.
    pub(crate) fn extra_row(&self) -> ExtraRow {
        let (state, tone, tile) = self.extra_state();
        let restart = ExtraItem::Action {
            title: if self.sessions > 0 {
                "Restart SpaceTerm Core…".into()
            } else {
                "Restart SpaceTerm Core".into()
            },
            subtitle: Some(closes(self.sessions).into()),
            command: self.restart_command(),
        };
        let machine = self.machine;
        let action =
            |title: &str, subtitle: Option<&str>, action: MachineAction| ExtraItem::Action {
                title: title.to_owned().into(),
                subtitle: subtitle.map(|text| text.to_owned().into()),
                command: Command::Machine(machine, action),
            };
        let info = |text: String| Some(SharedString::from(text));
        let (info, items): (Option<SharedString>, Option<Vec<ExtraItem>>) = match &self.state {
            MachineState::Current | MachineState::DesktopTooOld => (
                matches!(self.state, MachineState::DesktopTooOld).then(|| self.sentence()),
                Some(vec![restart]),
            ),
            MachineState::Ready { stranded, .. } | MachineState::Overdue { stranded, .. } => {
                let title = if self.restart_asks() {
                    "Restart to Update…"
                } else {
                    "Restart to Update"
                };
                let subtitle = match (self.sessions, stranded.len()) {
                    (0, 0) => closes(0),
                    (n, 0) => closes(n),
                    (0, apps) => format!("Stops {}", count(apps, "app")),
                    (n, apps) => format!("{}, stops {}", closes(n), count(apps, "app")),
                };
                let info = match &self.state {
                    MachineState::Overdue {
                        to, days: waited, ..
                    } => info(format!(
                        "SpaceTerm Core {to} has waited {}. SpaceTerm never restarts it while \
                         sessions are open.",
                        days(*waited)
                    )),
                    _ if !stranded.is_empty() => info(stranded_sentence(&self.place(), stranded)),
                    _ => None,
                };
                (
                    info,
                    Some(vec![ExtraItem::Action {
                        title: title.into(),
                        subtitle: Some(subtitle.into()),
                        command: self.restart_command(),
                    }]),
                )
            }
            MachineState::CoreTooOld { push } => (
                info(match push {
                    Some(to) => format!("This SpaceTerm needs SpaceTerm Core {to}."),
                    None => "This SpaceTerm needs a newer SpaceTerm Core.".to_owned(),
                }),
                Some(vec![action(
                    "Install Update",
                    Some("Sessions stay open"),
                    MachineAction::Install,
                )]),
            ),
            MachineState::UpdateFailed { id, .. } => (
                Some(self.sentence()),
                Some(vec![
                    action("Dismiss Notice", None, MachineAction::DismissFailure(*id)),
                    ExtraItem::Separator,
                    restart,
                ]),
            ),
            MachineState::InstallFailed { .. } => (
                Some(self.sentence()),
                Some(vec![
                    action("Try Again", None, MachineAction::Install),
                    ExtraItem::Separator,
                    restart,
                ]),
            ),
            MachineState::CantConfirm { .. } => (
                Some(self.sentence()),
                Some(vec![action("Check Again", None, MachineAction::CheckAgain)]),
            ),
            MachineState::CantCheck { push, .. } => (
                Some(self.sentence()),
                Some(match push {
                    Some(_) => vec![
                        action(
                            UPDATE_FROM_LOCAL,
                            Some("Sessions stay open"),
                            MachineAction::Install,
                        ),
                        ExtraItem::Separator,
                        restart,
                    ],
                    None => vec![restart],
                }),
            ),
            MachineState::Stopped => (
                None,
                Some(vec![action(
                    "Start SpaceTerm Core",
                    Some("Opens its Workspaces"),
                    MachineAction::Start,
                )]),
            ),
            MachineState::Connecting
            | MachineState::Installing { .. }
            | MachineState::Restarting { .. }
            | MachineState::Checking
            | MachineState::Stopping
            | MachineState::Uninstalling
            | MachineState::Offline => (None, None),
        };
        let items = items.map(|mut items| {
            if self.local && self.state != MachineState::Stopped {
                items.push(ExtraItem::Separator);
                items.push(ExtraItem::Action {
                    title: if self.sessions > 0 {
                        "Stop SpaceTerm Core…".into()
                    } else {
                        "Stop SpaceTerm Core".into()
                    },
                    subtitle: Some(closes(self.sessions).into()),
                    command: self.stop_command(),
                });
            }
            items
        });
        ExtraRow {
            machine: self.machine,
            name: self.name.clone(),
            local: self.local,
            line: version_line(self.running.as_ref().map(Version::as_str), Some(&state)),
            state: state.into(),
            tone,
            tile,
            sentence: self.sentence(),
            info,
            items,
        }
    }
}

#[cfg(target_os = "macos")]
const LOCAL_PLACE: &str = "this Mac";
#[cfg(not(target_os = "macos"))]
const LOCAL_PLACE: &str = "this computer";
#[cfg(target_os = "macos")]
const UPDATE_FROM_LOCAL: &str = "Update from This Mac";
#[cfg(not(target_os = "macos"))]
const UPDATE_FROM_LOCAL: &str = "Update from This Computer";

fn machine_state(update: &MachineUpdate, dismissed: &Dismissed, now: u64) -> MachineState {
    let facts = update.facts.as_ref();
    let checking = facts.is_some_and(|facts| facts.operation == Some(Operation::Checking));
    let state = base_state(update, dismissed, now);
    if checking && state.pending() && !state.working() {
        MachineState::Checking
    } else {
        state
    }
}

fn base_state(update: &MachineUpdate, dismissed: &Dismissed, now: u64) -> MachineState {
    match &update.link {
        MachineLink::Offline { .. } => return MachineState::Offline,
        MachineLink::Stopped => return MachineState::Stopped,
        MachineLink::Unconfirmed { expected, .. } => {
            return MachineState::CantConfirm {
                expected: expected.clone(),
            };
        }
        MachineLink::Incompatible(Incompatible::CallerTooOld) => {
            return MachineState::DesktopTooOld;
        }
        MachineLink::Unreported => {
            return MachineState::CantCheck {
                reason: CheckProblem::Unreported,
                push: update.push.clone(),
            };
        }
        MachineLink::Incompatible(Incompatible::ListenerTooOld) => {}
        MachineLink::Connected => {}
    }
    let core_too_old = matches!(
        update.link,
        MachineLink::Incompatible(Incompatible::ListenerTooOld)
    );
    let Some(facts) = &update.facts else {
        return if core_too_old {
            MachineState::CoreTooOld {
                push: update.push.clone(),
            }
        } else {
            MachineState::Connecting
        };
    };
    match &facts.operation {
        Some(Operation::Installing {
            version,
            received,
            total,
        }) => {
            return MachineState::Installing {
                to: version.clone(),
                fraction: (*total > 0).then(|| (*received as f64 / *total as f64) as f32),
            };
        }
        Some(Operation::Restarting { to }) => {
            return MachineState::Restarting { to: to.clone() };
        }
        Some(Operation::Stopping) => return MachineState::Stopping,
        Some(Operation::Uninstalling) => return MachineState::Uninstalling,
        Some(Operation::Checking) | None => {}
    }
    if core_too_old && facts.pending.is_none() {
        return MachineState::CoreTooOld {
            push: update.push.clone(),
        };
    }
    if let Some(failure) = &facts.last_failure
        && !dismissed.contains(&(update.machine, failure.id))
    {
        let superseded = facts.running == failure.version
            || facts
                .pending
                .as_ref()
                .is_some_and(|pending| pending.installed_at > failure.at);
        if !superseded {
            match failure.kind {
                FailureKind::Start(StartFailure::NotReady) => {
                    return MachineState::UpdateFailed {
                        id: failure.id,
                        version: failure.version.clone(),
                    };
                }
                FailureKind::Install(reason) => {
                    return MachineState::InstallFailed {
                        id: failure.id,
                        version: failure.version.clone(),
                        reason,
                    };
                }
                // Transient: the version stays pending, and the next restart tries again.
                FailureKind::Start(StartFailure::PortInUse | StartFailure::DiskFull) => {}
            }
        }
    }
    if let Some(pending) = &facts.pending {
        let stranded = pending
            .stranded
            .iter()
            .map(|device| device.name.clone())
            .collect();
        return match pending.overdue_since {
            Some(_) => MachineState::Overdue {
                to: pending.version.clone(),
                days: (now.saturating_sub(pending.installed_at) / DAY).max(1),
                breaking: pending.breaking,
                stranded,
            },
            None => MachineState::Ready {
                to: pending.version.clone(),
                breaking: pending.breaking,
                stranded,
            },
        };
    }
    if let UpdateCheck::Failing { since, reason, .. } = facts.check
        && now.saturating_sub(since) >= CANT_CHECK_AFTER
    {
        return MachineState::CantCheck {
            reason: CheckProblem::Failing(reason),
            push: update.push.clone(),
        };
    }
    MachineState::Current
}

impl DesktopState {
    /// `too_old_for` names a Machine whose Core is too new for this desktop.
    pub(crate) fn new(facts: DesktopFacts<'_>, too_old_for: Option<SharedString>) -> Self {
        let pending = facts.pending.map(str::to_owned);
        let overdue = facts.stage != UpdateStage::Optional;
        let state = match facts.state {
            UpdateState::Ready { version } if overdue => Self::Overdue {
                version: version.clone(),
                stage: facts.stage,
            },
            UpdateState::Ready { version } => Self::Ready {
                version: version.clone(),
            },
            UpdateState::Available { version } if overdue => Self::Overdue {
                version: version.clone(),
                stage: facts.stage,
            },
            UpdateState::Available { version } => Self::Available {
                version: version.clone(),
            },
            UpdateState::Downloading {
                version,
                received,
                total,
            } => Self::Downloading {
                version: version.clone(),
                fraction: (*total > 0).then(|| (*received as f64 / *total as f64) as f32),
            },
            UpdateState::Verifying { version } => Self::Downloading {
                version: version.clone(),
                fraction: None,
            },
            UpdateState::Installing { version } => Self::Relaunching {
                version: version.clone(),
            },
            UpdateState::Failed {
                error:
                    error @ (UpdateError::Download
                    | UpdateError::Verification
                    | UpdateError::Installation
                    | UpdateError::ReadOnly),
            } => Self::InstallFailed {
                version: pending,
                error: *error,
            },
            UpdateState::Idle | UpdateState::Checking | UpdateState::Failed { .. }
                if overdue && pending.is_some() =>
            {
                Self::Overdue {
                    version: pending.unwrap_or_default(),
                    stage: facts.stage,
                }
            }
            _ => Self::Current,
        };
        match (state, too_old_for) {
            (
                Self::Current
                | Self::Available { .. }
                | Self::Downloading { .. }
                | Self::Ready { .. }
                | Self::Overdue { .. },
                Some(machine),
            ) => Self::TooOld {
                version: facts.pending.map(str::to_owned),
                machine,
            },
            (Self::Current, None) if facts.notes_unread => Self::WhatsNew {
                version: facts.running.to_owned(),
            },
            (state, _) => state,
        }
    }

    pub(crate) fn pending(&self) -> bool {
        !matches!(self, Self::Current | Self::Downloading { .. })
    }

    fn tone(&self) -> Tone {
        match self {
            Self::Overdue { .. } | Self::TooOld { .. } | Self::InstallFailed { .. } => {
                Tone::Problem
            }
            _ => Tone::Action,
        }
    }

    fn glyph(&self) -> Option<Glyph> {
        Some(match self {
            Self::Current => return None,
            Self::Available { .. } => Glyph::Download,
            Self::Downloading { .. } | Self::Relaunching { .. } => Glyph::Working,
            Self::Ready { .. } => Glyph::Restart,
            Self::Overdue { .. } => Glyph::Overdue,
            Self::TooOld { .. } => Glyph::UpdateDesktop,
            Self::InstallFailed { .. } => Glyph::InstallFailed,
            Self::WhatsNew { .. } => Glyph::WhatsNew,
        })
    }

    fn urgency(&self) -> u8 {
        match self {
            Self::InstallFailed { .. } => 2,
            Self::TooOld { .. } => 3,
            Self::Overdue { .. } => 8,
            Self::Ready { .. } | Self::Available { .. } => 10,
            Self::WhatsNew { .. } => 12,
            _ => 14,
        }
    }

    fn control_label(&self) -> &'static str {
        match self {
            Self::Current => "Current",
            Self::Available { .. } => "Download",
            Self::Downloading { .. } => "Downloading…",
            Self::Ready { .. } => "Relaunch",
            Self::Overdue { .. } => "Update overdue",
            Self::TooOld { .. } => "Update SpaceTerm",
            Self::InstallFailed { .. } => "Install failed",
            Self::Relaunching { .. } => "Relaunching…",
            Self::WhatsNew { .. } => "What's new",
        }
    }

    pub(crate) fn sentence(&self) -> SharedString {
        let version = |version: &Option<String>| {
            version
                .as_ref()
                .map_or_else(|| "SpaceTerm".to_owned(), |v| format!("SpaceTerm {v}"))
        };
        match self {
            Self::Current => "SpaceTerm is up to date.".to_owned(),
            Self::Available { version } => format!(
                "SpaceTerm {version} is available. Downloading it keeps every session running."
            ),
            Self::Downloading { version, .. } => format!("Downloading SpaceTerm {version}."),
            Self::Ready { version } => format!(
                "SpaceTerm {version} is ready. Relaunching SpaceTerm keeps every session running."
            ),
            Self::Overdue { version, stage } => match stage {
                UpdateStage::Overdue => format!(
                    "SpaceTerm {version} has waited 2 days. SpaceTerm installs it the next time \
                     it opens. Relaunching now keeps every session running."
                ),
                _ => format!(
                    "SpaceTerm {version} has waited 1 day. Relaunching keeps every session \
                     running. If you don't, SpaceTerm installs it the next time it opens."
                ),
            },
            Self::TooOld {
                version: v,
                machine,
            } => format!(
                "This SpaceTerm is too old for SpaceTerm Core on {machine}. Relaunching installs \
                 {} and keeps every session running.",
                match v {
                    Some(_) => version(v),
                    None => "a newer SpaceTerm".to_owned(),
                }
            ),
            Self::InstallFailed { version: v, error } => desktop_failure(&version(v), *error),
            Self::Relaunching { .. } => "SpaceTerm is relaunching. No sessions close.".to_owned(),
            Self::WhatsNew { version } => format!("SpaceTerm updated to {version}."),
        }
        .into()
    }

    fn control_command(&self) -> Option<Command> {
        Some(match self {
            Self::Available { .. }
            | Self::Ready { .. }
            | Self::Overdue { .. }
            | Self::TooOld { .. } => Command::Desktop(DesktopAction::Update),
            Self::WhatsNew { .. } => Command::Desktop(DesktopAction::WhatsNew),
            Self::InstallFailed { version, .. } => Command::Explain(Explanation {
                title: match version {
                    Some(version) => format!("Couldn't install SpaceTerm {version}"),
                    None => "Couldn't install the SpaceTerm update".to_owned(),
                }
                .into(),
                message: format!("{} SpaceTerm tries again later.", self.sentence()).into(),
                then: Some(Box::new(Command::Desktop(DesktopAction::DismissFailure))),
            }),
            Self::Current | Self::Downloading { .. } | Self::Relaunching { .. } => return None,
        })
    }

    fn menu_row(&self) -> Option<MenuRow> {
        let update = || Some(("Relaunch", Command::Desktop(DesktopAction::Update)));
        let (status, button) = match self {
            Self::Current => return None,
            Self::Available { .. } => (
                "Not downloaded".to_owned(),
                Some(("Download", Command::Desktop(DesktopAction::Update))),
            ),
            Self::Downloading { .. } => ("Downloading…".to_owned(), None),
            Self::Ready { .. } => ("Keeps sessions".to_owned(), update()),
            Self::Overdue { .. } => ("Overdue".to_owned(), update()),
            Self::TooOld { .. } => ("Too old".to_owned(), update()),
            Self::InstallFailed { .. } => (
                "Install failed".to_owned(),
                Some(("Try Again", Command::Desktop(DesktopAction::Update))),
            ),
            Self::Relaunching { .. } => ("Relaunching…".to_owned(), None),
            Self::WhatsNew { version } => (
                format!("Updated to {version}"),
                Some(("What's New", Command::Desktop(DesktopAction::WhatsNew))),
            ),
        };
        Some(MenuRow {
            key: ItemKey::Desktop,
            name: "SpaceTerm".into(),
            status: status.into(),
            tone: Some(self.tone()),
            glyph: self.glyph(),
            fraction: match self {
                Self::Downloading { fraction, .. } => *fraction,
                _ => None,
            },
            sentence: self.sentence(),
            button: button.map(|(label, command)| RowButton {
                label: label.into(),
                command,
            }),
        })
    }
}

/// Which pending item a row or button belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ItemKey {
    Desktop,
    Machine(MachineId),
}

/// One pending item as the title-bar control shows it alone.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub(crate) key: ItemKey,
    pub(crate) label: SharedString,
    pub(crate) tone: Tone,
    pub(crate) glyph: Glyph,
    /// The determinate progress of a working item.
    pub(crate) fraction: Option<f32>,
    pub(crate) sentence: SharedString,
    /// None while the item is in progress: the control is then a status, not a button.
    pub(crate) command: Option<Command>,
    urgency: u8,
}

/// The title-bar control.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Control {
    /// Nothing needs doing or reading.
    Hidden,
    /// One item: a plain button that performs its command, or a status while it works.
    Single(Item),
    /// Two or more items: a compact menu.
    Menu {
        label: SharedString,
        tone: Tone,
        glyph: Glyph,
        sentence: SharedString,
    },
}

/// A row of the in-window update menu.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MenuRow {
    pub(crate) key: ItemKey,
    pub(crate) name: SharedString,
    pub(crate) status: SharedString,
    /// None draws the status in the secondary color.
    pub(crate) tone: Option<Tone>,
    pub(crate) glyph: Option<Glyph>,
    pub(crate) fraction: Option<f32>,
    pub(crate) sentence: SharedString,
    pub(crate) button: Option<RowButton>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowButton {
    pub(crate) label: SharedString,
    pub(crate) command: Command,
}

/// "Restart All…": every Machine whose Pending Version waits.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RestartAll {
    pub(crate) machines: usize,
    pub(crate) sessions: u32,
    /// "Restart All…".
    pub(crate) label: SharedString,
    /// The in-window menu's trailing text: "7 sessions".
    pub(crate) trailing: SharedString,
    /// The menu bar extra's subtitle: "Updates 3 machines, closes 7 sessions".
    pub(crate) subtitle: SharedString,
    pub(crate) sentence: SharedString,
    pub(crate) command: Command,
}

/// The menu bar extra's tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExtraTile {
    Connected,
    /// A spinner and no submenu.
    Busy,
    /// Stopped on purpose: a gray tile.
    Off,
    /// A gray tile and no submenu.
    Offline,
}

/// The menu bar extra's header.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExtraHeader {
    /// "Version 0.8 · Update ready", or "Version 0.9" with nothing pending.
    pub(crate) line: SharedString,
    pub(crate) state: Option<SharedString>,
    pub(crate) tone: Tone,
}

/// One Machine row of the menu bar extra.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExtraRow {
    pub(crate) machine: MachineId,
    pub(crate) name: SharedString,
    pub(crate) local: bool,
    /// "Version {running} · {state}".
    pub(crate) line: SharedString,
    /// Only this part is orange for a problem.
    pub(crate) state: SharedString,
    pub(crate) tone: Tone,
    pub(crate) tile: ExtraTile,
    pub(crate) sentence: SharedString,
    /// The submenu's information line.
    pub(crate) info: Option<SharedString>,
    /// The submenu, or None for a row with no chevron.
    pub(crate) items: Option<Vec<ExtraItem>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ExtraItem {
    Action {
        title: SharedString,
        subtitle: Option<SharedString>,
        command: Command,
    },
    Separator,
}

/// A daily notification about an overdue update.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reminder {
    pub(crate) key: ItemKey,
    pub(crate) title: SharedString,
    pub(crate) body: SharedString,
}

/// Everything the update surfaces show, at one moment.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Presentation {
    pub(crate) desktop: DesktopState,
    pub(crate) machines: Vec<MachineView>,
}

impl Presentation {
    pub(crate) fn new(
        desktop: DesktopFacts<'_>,
        machines: &[MachineUpdate],
        dismissed: &Dismissed,
        now: u64,
    ) -> Self {
        let machines: Vec<MachineView> = machines
            .iter()
            .map(|update| MachineView::new(update, dismissed, now))
            .collect();
        let too_old_for = machines
            .iter()
            .find(|view| view.state == MachineState::DesktopTooOld)
            .map(|view| view.name.clone());
        Self {
            desktop: DesktopState::new(desktop, too_old_for),
            machines,
        }
    }

    pub(crate) fn machine(&self, machine: MachineId) -> Option<&MachineView> {
        self.machines.iter().find(|view| view.machine == machine)
    }

    /// Every pending item, the desktop first.
    pub(crate) fn items(&self) -> Vec<Item> {
        let desktop = self.desktop.pending().then(|| Item {
            key: ItemKey::Desktop,
            label: self.desktop.control_label().into(),
            tone: self.desktop.tone(),
            glyph: self.desktop.glyph().unwrap_or(Glyph::Restart),
            fraction: None,
            sentence: self.desktop.sentence(),
            command: self.desktop.control_command(),
            urgency: self.desktop.urgency(),
        });
        let machines = self
            .machines
            .iter()
            .filter(|view| view.state.pending())
            .map(|view| Item {
                key: ItemKey::Machine(view.machine),
                label: view.control_label().into(),
                tone: view.state.tone(),
                glyph: view.state.glyph().unwrap_or(Glyph::Restart),
                fraction: view.state.fraction(),
                sentence: view.sentence(),
                command: view.control_command(),
                urgency: view.state.urgency(),
            });
        desktop.into_iter().chain(machines).collect()
    }

    pub(crate) fn control(&self) -> Control {
        let mut items = self.items();
        match items.len() {
            0 => Control::Hidden,
            1 => Control::Single(items.remove(0)),
            n => {
                let urgent = items
                    .iter()
                    .min_by_key(|item| item.urgency)
                    .expect("two or more items");
                Control::Menu {
                    label: format!("{n} updates").into(),
                    tone: if items.iter().any(|item| item.tone == Tone::Problem) {
                        Tone::Problem
                    } else {
                        Tone::Action
                    },
                    glyph: urgent.glyph,
                    sentence: format!("{n} updates need you. Open the menu to see each machine.")
                        .into(),
                }
            }
        }
    }

    /// The desktop's row, shown above the "SpaceTerm Core" heading.
    pub(crate) fn desktop_row(&self) -> Option<MenuRow> {
        self.desktop.menu_row()
    }

    /// The rows under the "SpaceTerm Core" heading.
    pub(crate) fn machine_rows(&self) -> Vec<MenuRow> {
        self.machines
            .iter()
            .filter_map(MachineView::menu_row)
            .collect()
    }

    /// "Restart All…", when two or more Pending Versions wait.
    pub(crate) fn restart_all(&self) -> Option<RestartAll> {
        let waiting: Vec<MachineView> = self
            .machines
            .iter()
            .filter(|view| view.state.waiting().is_some())
            .cloned()
            .collect();
        if waiting.len() < 2 {
            return None;
        }
        let total: u32 = waiting.iter().map(|view| view.sessions).sum();
        let confirmation = restart_confirmation(&waiting);
        Some(RestartAll {
            machines: waiting.len(),
            sessions: total,
            label: "Restart All…".into(),
            trailing: sessions(total).into(),
            subtitle: format!(
                "Updates {} machines, closes {}",
                waiting.len(),
                sessions(total)
            )
            .into(),
            sentence: format!(
                "Restart SpaceTerm Core on {} machines. This will close {}.",
                waiting.len(),
                running_sessions(total)
            )
            .into(),
            command: Command::Confirm(confirmation),
        })
    }

    /// The confirmation for the Restarts or the Stop `previews` describe, with the sessions each
    /// Core itself will end. None when nothing asks: no session closes and no breaking change
    /// waits, unless `always`, as after a Core refused a preview the person never saw. A
    /// Machine no longer listed is left out.
    pub(crate) fn previewed(
        &self,
        previews: &[MachinePreview],
        always: bool,
    ) -> Option<Confirmation> {
        let views: Vec<MachineView> = previews
            .iter()
            .filter_map(|preview| {
                let mut view = self.machine(preview.machine)?.clone();
                view.sessions = preview.sessions;
                Some(view)
            })
            .collect();
        match previews.first().map(|preview| &preview.action) {
            Some(MachineAction::Stop) => {
                let [view] = views.as_slice() else {
                    return None;
                };
                (always || view.sessions > 0).then(|| view.stop_confirmation())
            }
            Some(MachineAction::Restart) if !views.is_empty() => (always
                || views.iter().any(MachineView::restart_asks))
            .then(|| restart_confirmation(&views)),
            _ => None,
        }
    }

    pub(crate) fn extra_header(&self) -> ExtraHeader {
        let running = env!("SPACETERM_VERSION");
        let (version, state): (&str, Option<String>) = match &self.desktop {
            DesktopState::Current => (running, None),
            DesktopState::Available { .. } => (running, Some("Update available".to_owned())),
            DesktopState::Downloading { version, .. } => {
                (running, Some(format!("Downloading {version}…")))
            }
            DesktopState::Ready { .. } => (running, Some("Update ready".to_owned())),
            DesktopState::Overdue { .. } => (running, Some("Update overdue".to_owned())),
            DesktopState::TooOld { machine, .. } => {
                (running, Some(format!("Too old for {machine}")))
            }
            DesktopState::InstallFailed { .. } => (running, Some("Install failed".to_owned())),
            DesktopState::Relaunching { .. } => (running, Some("Relaunching…".to_owned())),
            DesktopState::WhatsNew { .. } => (running, Some("Updated".to_owned())),
        };
        ExtraHeader {
            line: version_line(Some(version), state.as_deref()),
            state: state.map(Into::into),
            tone: self.desktop.tone(),
        }
    }

    pub(crate) fn extra_rows(&self) -> Vec<ExtraRow> {
        self.machines.iter().map(MachineView::extra_row).collect()
    }

    /// The menu bar extra's actions group, shown only when it applies.
    pub(crate) fn extra_actions(&self) -> Vec<ExtraItem> {
        let mut actions = Vec::new();
        match &self.desktop {
            DesktopState::Available { .. }
            | DesktopState::Ready { .. }
            | DesktopState::Overdue { .. }
            | DesktopState::TooOld { .. } => actions.push(ExtraItem::Action {
                title: "Update SpaceTerm and Relaunch".into(),
                subtitle: Some("Keeps all sessions open".into()),
                command: Command::Desktop(DesktopAction::Update),
            }),
            DesktopState::InstallFailed { .. } => actions.push(ExtraItem::Action {
                title: "Retry SpaceTerm Update".into(),
                subtitle: Some(self.desktop.sentence()),
                command: Command::Desktop(DesktopAction::Update),
            }),
            DesktopState::WhatsNew { version } => actions.push(ExtraItem::Action {
                title: super::release_notes::embedded()
                    .map_or_else(
                        || format!("What's New in SpaceTerm {version}"),
                        super::release_notes::ReleaseNotes::menu_title,
                    )
                    .into(),
                subtitle: None,
                command: Command::Desktop(DesktopAction::WhatsNew),
            }),
            DesktopState::Current
            | DesktopState::Downloading { .. }
            | DesktopState::Relaunching { .. } => {}
        }
        if let Some(all) = self.restart_all() {
            actions.push(ExtraItem::Action {
                title: all.label,
                subtitle: Some(all.subtitle),
                command: all.command,
            });
        }
        actions
    }

    /// The daily notifications due for overdue items.
    pub(crate) fn reminders(&self) -> Vec<Reminder> {
        let desktop = match &self.desktop {
            DesktopState::Overdue { version, stage } => Some(Reminder {
                key: ItemKey::Desktop,
                title: "SpaceTerm update overdue".into(),
                body: match stage {
                    UpdateStage::Overdue => format!(
                        "SpaceTerm installs {version} the next time it opens. Relaunch now to \
                         choose when."
                    ),
                    _ => {
                        format!("Relaunch to install SpaceTerm {version}. Your sessions stay open.")
                    }
                }
                .into(),
            }),
            _ => None,
        };
        let machines = self.machines.iter().filter_map(|view| match &view.state {
            MachineState::Overdue {
                to, days: waited, ..
            } => Some(Reminder {
                key: ItemKey::Machine(view.machine),
                title: format!("Restart overdue on {}", view.place()).into(),
                body: match view.sessions {
                    0 => format!("SpaceTerm Core {to} has waited {}.", days(*waited)),
                    n => format!(
                        "SpaceTerm Core {to} has waited {}. Restarting closes {}.",
                        days(*waited),
                        running_sessions(n)
                    ),
                }
                .into(),
            }),
            _ => None,
        });
        desktop.into_iter().chain(machines).collect()
    }
}

/// What the installed update services show now. Without a Machine updates service, no Machine
/// appears; without a desktop update service, the desktop is current.
pub(crate) fn current(cx: &App) -> Presentation {
    let unavailable = UpdateState::Unavailable;
    let running = env!("SPACETERM_VERSION");
    let desktop = cx
        .try_global::<UpdateService>()
        .map(|service| service.0.read(cx));
    let facts = match desktop {
        Some(updates) => DesktopFacts {
            state: updates.state(),
            pending: updates.pending_version(),
            stage: updates.stage(),
            // A build without release notes has nothing to read.
            notes_unread: super::release_notes::embedded().is_some()
                && updates.release_notes_unread(),
            running,
        },
        None => DesktopFacts {
            state: &unavailable,
            pending: None,
            stage: UpdateStage::Optional,
            notes_unread: false,
            running,
        },
    };
    let now = super::now();
    match MachineUpdates::global(cx) {
        Some(store) => {
            let store = store.read(cx);
            Presentation::new(facts, store.machines(), store.dismissed(), now)
        }
        None => Presentation::new(facts, &[], &Dismissed::new(), now),
    }
}

/// The alert for an action on a Machine that did not succeed: its title and full sentence.
/// `Changed` asks again instead, so it has no alert.
pub(crate) fn action_failure(
    name: &str,
    local: bool,
    action: &MachineAction,
    failure: ActionFailure,
) -> Option<(SharedString, SharedString)> {
    let place = if local {
        LOCAL_PLACE.to_owned()
    } else {
        name.to_owned()
    };
    let verb = match action {
        MachineAction::Install => "update",
        MachineAction::Restart => "restart",
        MachineAction::CheckAgain => "check",
        MachineAction::Start => "start",
        MachineAction::Stop => "stop",
        MachineAction::DismissFailure(_) => return None,
    };
    let message = match failure {
        ActionFailure::Changed => return None,
        ActionFailure::Unavailable(Unavailable::CoreTooOld) => format!(
            "SpaceTerm Core on {place} is too old to {verb} from this SpaceTerm. Update it on              {place} first."
        ),
        ActionFailure::Unavailable(Unavailable::ClientTooOld) => format!(
            "This SpaceTerm is too old for SpaceTerm Core on {place}. Update SpaceTerm, then try              again."
        ),
        ActionFailure::Unavailable(Unavailable::NotConnected) | ActionFailure::Unreachable => {
            format!("SpaceTerm can't reach {place}. Try again when it is connected.")
        }
        ActionFailure::Busy => format!(
            "SpaceTerm Core on {place} is busy with another update. Try again when it finishes."
        ),
        ActionFailure::Failed => format!("SpaceTerm Core on {place} couldn't {verb}."),
    };
    Some((
        format!("Couldn't {verb} SpaceTerm Core on {place}").into(),
        message.into(),
    ))
}

/// The confirmation for restarting one or more Machines into their Pending Versions.
fn restart_confirmation(views: &[MachineView]) -> Confirmation {
    let total: u32 = views.iter().map(|view| view.sessions).sum();
    let (title, message) = match views {
        [view] => (
            format!("Restart SpaceTerm Core on {}?", view.place()),
            (view.sessions > 0)
                .then(|| format!("This will close your {}.", running_sessions(view.sessions))),
        ),
        _ => (
            format!("Restart SpaceTerm Core on {} machines?", views.len()),
            (total > 0).then(|| format!("This will close {}.", running_sessions(total))),
        ),
    };
    let breaking = views.iter().filter_map(|view| {
        let (to, breaking, stranded) = view.state.waiting()?;
        if !stranded.is_empty() {
            Some(stranded_sentence(&view.place(), stranded))
        } else {
            breaking.then(|| {
                format!(
                    "SpaceTerm Core {to} is a breaking update for {}, so it restarts only when \
                     you ask.",
                    view.place()
                )
            })
        }
    });
    let message: Vec<String> = message.into_iter().chain(breaking).collect();
    Confirmation {
        title: title.into(),
        message: message.join(" ").into(),
        button: "Restart".into(),
        actions: views
            .iter()
            .map(|view| (view.machine, MachineAction::Restart))
            .collect(),
        approval: Approval::of(views),
    }
}

fn stranded_sentence(place: &str, stranded: &[String]) -> String {
    let who = if stranded.len() == 1 {
        "it updates"
    } else {
        "they update"
    };
    format!(
        "SpaceTerm on {} will stop working with {place} until {who}.",
        list_and(stranded)
    )
}

fn install_failure(name: &str, place: &str, version: &Version, reason: InstallFailure) -> String {
    match reason {
        InstallFailure::DiskFull => {
            format!("{name} is out of disk space for SpaceTerm Core {version}.")
        }
        InstallFailure::Download => {
            format!("SpaceTerm Core {version} didn't finish downloading on {place}.")
        }
        InstallFailure::Verification => format!(
            "SpaceTerm Core {version} failed signature verification on {place}, so SpaceTerm \
             didn't install it."
        ),
    }
}

fn desktop_failure(release: &str, error: UpdateError) -> String {
    match error {
        UpdateError::Download => format!("{release} didn't finish downloading."),
        UpdateError::Verification => {
            format!("{release} failed signature verification, so SpaceTerm didn't install it.")
        }
        UpdateError::ReadOnly => {
            format!("SpaceTerm can't replace its installation to install {release}.")
        }
        UpdateError::Installation | UpdateError::Check | UpdateError::Unavailable => {
            format!("{release} didn't install.")
        }
    }
}

/// Why a Machine's checks fail, as the start of a sentence.
fn check_failure(name: &str, reason: CheckProblem) -> String {
    match reason {
        CheckProblem::Failing(CheckFailure::Network) => {
            format!("{name} can't reach SpaceTerm's update feed")
        }
        CheckProblem::Failing(CheckFailure::Proxy) => {
            format!("{name}'s proxy blocks SpaceTerm's update feed")
        }
        CheckProblem::Failing(CheckFailure::Verification) => {
            format!("{name} received an update feed that failed verification")
        }
        CheckProblem::Failing(CheckFailure::SystemTooOld) => {
            format!("The newest SpaceTerm Core needs a newer system than {name} runs")
        }
        CheckProblem::Unreported => format!("SpaceTerm Core on {name} doesn't report its updates"),
    }
}

fn check_remedy(reason: CheckProblem) -> &'static str {
    match reason {
        CheckProblem::Failing(CheckFailure::Network) => "Check its network or proxy settings.",
        CheckProblem::Failing(CheckFailure::Proxy) => "Check its proxy settings.",
        CheckProblem::Failing(CheckFailure::Verification) => "SpaceTerm tries again later.",
        CheckProblem::Failing(CheckFailure::SystemTooOld) => "Update its operating system.",
        CheckProblem::Unreported => "Run spaceterm update on it.",
    }
}

/// "Version 0.9 · 3 sessions", "Version 0.9", or the state alone before the version is known.
fn version_line(version: Option<&str>, state: Option<&str>) -> SharedString {
    match (version, state) {
        (Some(version), Some(state)) => format!("Version {version} · {state}"),
        (Some(version), None) => format!("Version {version}"),
        (None, Some(state)) => state.to_owned(),
        (None, None) => String::new(),
    }
    .into()
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// "1 session", "3 sessions".
pub(crate) fn sessions(n: u32) -> String {
    count(n as usize, "session")
}

/// "1 running session", "3 running sessions".
pub(crate) fn running_sessions(n: u32) -> String {
    count(n as usize, "running session")
}

/// "Closes 3 sessions", or "No sessions open".
fn closes(n: u32) -> String {
    if n == 0 {
        "No sessions open".to_owned()
    } else {
        format!("Closes {}", sessions(n))
    }
}

fn days(n: u64) -> String {
    count(n as usize, "day")
}

fn list_and(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;
