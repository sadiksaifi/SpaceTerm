//! The one update view every surface renders: the SpaceTerm row, one row per Machine, and the
//! one step the title bar's split pill shows. This module decides; it holds no person-facing
//! string. [`super::copy`] turns the view into the rendered forms each surface consumes.
#![allow(
    dead_code,
    reason = "the title-bar, surfaces, ladder and pane slices each read part of this view"
)]

use gpui::{App, SharedString};
use spaceterm_core::update::facts::Version;

use super::ladder::{Emphasis, Stage};
use super::machines::{MachineAction, MachineId};
use super::presentation::{Confirmation, DesktopAction, Explanation};

/// One glyph per state. App-drawn surfaces map these to Lucide icons; native menus use their
/// own symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Glyph {
    /// A spinner, determinate when the item carries a fraction.
    Working,
    /// Download a desktop release that is not downloaded yet.
    Download,
    /// Relaunch the desktop.
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

/// SpaceTerm or one Machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    App,
    Machine(MachineId),
}

/// Everything the update surfaces show, at one moment.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UpdateView {
    pub(crate) app: AppRow,
    /// Local first, then store order.
    pub(crate) machines: Vec<MachineRow>,
    /// The split pill's main area. None hides the pill.
    pub(crate) step: Option<Step>,
}

/// SpaceTerm's own update.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AppRow {
    /// The running SpaceTerm version.
    pub(crate) running: SharedString,
}

/// One Machine's update.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MachineRow {
    pub(crate) id: MachineId,
    /// "This Mac", "This Computer" or the paired name.
    pub(crate) name: SharedString,
    pub(crate) local: bool,
    pub(crate) running: Option<Version>,
    pub(crate) sessions: u32,
}

/// The split pill's main area. Declaration order is priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// A failure SpaceTerm will not retry by itself, not dismissed on this desktop.
    Failed(Target),
    /// A restart started here got no answer for 180 seconds.
    NotConfirmed(MachineId),
    /// A restart the person approved on this desktop is running.
    Restarting(MachineId),
    /// A download the person started on this desktop is running.
    Downloading(Target),
    /// Relaunch chosen, SpaceTerm has not quit yet.
    Relaunching,
    /// An action step. `stage` is None for a hard stop with no clock.
    Act {
        kind: ActKind,
        stage: Option<Stage>,
        hard_stop: bool,
    },
    /// The first run on a newer SpaceTerm, notes not opened.
    WhatsNew,
}

/// Which action step. Within one stage, declaration order is priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActKind {
    Relaunch,
    RestartCore(MachineId),
    Download,
}

/// What a row's action or the pill's main area asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    WhatsNew,
    Download,
    RelaunchToUpdate,
    RestartCore(MachineId),
    TryAgain(Target),
    CheckAgain(MachineId),
    HowToUpdate(MachineId),
}

/// The split pill, rendered.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PillView {
    pub(crate) label: SharedString,
    pub(crate) glyph: Glyph,
    pub(crate) emphasis: Emphasis,
    pub(crate) tooltip: SharedString,
    /// What the main area's click does. The chevron always opens the list.
    pub(crate) main: Command,
    /// The determinate progress of a running download.
    pub(crate) progress: Option<f32>,
}

/// One row of the list, Settings > Updates, the Machine card and the menu bar extra.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowView {
    pub(crate) name: SharedString,
    /// The running version. The title-bar list shows it only when nothing waits.
    pub(crate) version: Option<SharedString>,
    pub(crate) status: Option<SharedString>,
    pub(crate) action: Option<RowAction>,
    pub(crate) description: Option<SharedString>,
    pub(crate) accessibility: SharedString,
}

/// A row's one action.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowAction {
    pub(crate) label: SharedString,
    /// The menu bar extra's subtitle: "Ends 2 Terminal Sessions".
    pub(crate) subtitle: Option<SharedString>,
    pub(crate) command: Command,
}

impl UpdateView {
    /// The split pill, or None when nothing is owed.
    pub(crate) fn pill(&self) -> Option<PillView> {
        todo!("the model slice builds the pill from the step")
    }

    pub(crate) fn app_row(&self) -> RowView {
        todo!("the model slice builds the SpaceTerm row")
    }

    /// Local first, then store order. The count and order stay stable while facts change.
    /// `machine_rows()[i]` describes `machines[i]`.
    pub(crate) fn machine_rows(&self) -> Vec<RowView> {
        todo!("the model slice builds the Machine rows")
    }

    /// The row of `machine`, when it is listed.
    pub(crate) fn machine_row(&self, machine: MachineId) -> Option<RowView> {
        let _ = machine;
        todo!("the model slice builds the Machine rows")
    }

    /// Whether any waiting update, on any row, is at the Insist stage or a hard stop, even when
    /// a step of higher priority holds the main area.
    pub(crate) fn insists(&self) -> bool {
        todo!("the model slice reads every row's stage")
    }
}

/// What the installed update services show now.
pub(crate) fn current(cx: &App) -> UpdateView {
    let _ = cx;
    todo!("the model slice reads the update services")
}
