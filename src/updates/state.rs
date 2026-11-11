//! The one update view every surface renders: the SpaceTerm row, one row per Machine, and the
//! one step the title bar's split pill shows. This module decides; it holds no person-facing
//! string. [`super::copy`] turns the view into the rendered forms each surface consumes.
//!
//! [`Step`] is the minimum over every row's candidate, so no other code decides the main area,
//! and the step and the rows come from the same stages.

use std::collections::{BTreeSet, HashMap};

use gpui::{App, SharedString};
use spaceterm_core::protocol::version::Incompatible;
use spaceterm_core::update::continuation::{Continuation, continuation};
use spaceterm_core::update::facts::{
    CheckFailure, FailureKind, InstallFailure, Operation, StartFailure, UpdateCheck, Version,
};
use spaceterm_core::update::ordering::Floor;

use super::ladder::{self, Emphasis, Stage};
use super::machines::store::MachineUpdates;
use super::machines::{
    ActionFailure, Asked, Bundled, MachineAction, MachineId, MachineLink, MachineUpdate,
};
use super::policy::DAY;
use super::{UpdateError, UpdateService, UpdateState};

pub(crate) use super::confirm::{Confirmation, Explanation};

/// How long a Machine's update checks fail before its row says it can't check.
pub(crate) const CANT_CHECK_AFTER: u64 = DAY;

/// A failure ID the store keeps in [`Dismissed`] for a failed push, which has no ID of its own.
pub(crate) const PUSH_FAILURE: u64 = u64::MAX;
/// A failure ID the store keeps in [`Dismissed`] for a skipped Stepping Stone.
pub(crate) const STEPPING_STONE: u64 = u64::MAX - 1;

/// Failure steps the person dismissed on this desktop, by Machine and failure ID.
pub(crate) type Dismissed = BTreeSet<(MachineId, u64)>;

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
    /// Restart SpaceTerm Core: the package glyph.
    UpdateCore,
    WhatsNew,
    /// A failure: the circle-alert glyph.
    UpdateFailed,
    /// A restart nobody confirmed: the circle-help glyph.
    CantConfirm,
}

/// What a click does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Desktop(DesktopAction),
    /// Performs the action at once. A Restart or Stop previews and asks first wherever its
    /// Core says something ends.
    Machine(MachineId, MachineAction),
    /// Opens an alert with the full sentence.
    Explain(Explanation),
    /// Opens the split pill's list. Only the title bar performs it.
    ShowList,
}

/// SpaceTerm's own update actions. None of them downloads and then relaunches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DesktopAction {
    /// Relaunches into the downloaded release without asking.
    Update,
    /// Downloads the release. Relaunching stays a separate choice.
    Download,
    WhatsNew,
    /// Clears a failed SpaceTerm update; SpaceTerm tries again later.
    DismissFailure,
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
    pub(crate) stage: AppStage,
    /// The ladder stage of a waiting update, from `UpdateHistory.published_at`.
    pub(crate) ladder: Stage,
    /// Whole days the update has waited.
    pub(crate) waited: u64,
    /// This build carries release notes.
    pub(crate) notes: bool,
    /// A Machine's SpaceTerm Core is too new for this SpaceTerm: a hard stop.
    pub(crate) too_old_for: Option<SharedString>,
    /// The downloaded release a scheduled check replaces. While that check runs, downloads by
    /// itself or retries by itself, the main area keeps its Relaunch; the row tells the work.
    pub(crate) replacing: Option<SharedString>,
    /// The person dismissed the failure's title-bar step on this desktop. The row keeps it.
    pub(crate) dismissed: bool,
}

/// SpaceTerm's update, as the person sees it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AppStage {
    /// `whats_new`: the first run on a newer SpaceTerm, notes not opened.
    Current {
        whats_new: bool,
    },
    /// A release waits, and automatic downloads are off.
    Available {
        version: SharedString,
    },
    Downloading {
        version: SharedString,
        fraction: Option<f32>,
        /// The person started it on this desktop.
        here: bool,
    },
    Downloaded {
        version: SharedString,
    },
    Relaunching {
        version: SharedString,
    },
    /// `retries`: SpaceTerm's one-day quiet retry tries it again by itself.
    DownloadFailed {
        version: Option<SharedString>,
        error: UpdateError,
        retries: bool,
    },
    /// `held`: the updater still holds the installer, so only a relaunch installs it.
    RelaunchFailed {
        version: Option<SharedString>,
        error: UpdateError,
        held: bool,
    },
    /// The installation can't be replaced.
    CantReplace,
    NeedsNewerSystem {
        version: Option<SharedString>,
    },
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
    pub(crate) stage: MachineStage,
    pub(crate) blocked: Option<Blocked>,
    /// The ladder stage of a waiting Pending Version, from `PendingVersion.installed_at`.
    pub(crate) ladder: Option<Stage>,
    /// Whole days the Pending Version has waited.
    pub(crate) waited: u64,
    /// The failure step the person dismissed on this desktop, or None.
    pub(crate) dismissed: bool,
    /// Cancel at the Ask stage hides this Machine's step for 24 hours on this desktop.
    pub(crate) deferred: bool,
}

/// A protocol mismatch: a hard stop above the ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blocked {
    /// SpaceTerm Core is too old for this SpaceTerm.
    CoreTooOld,
    /// This SpaceTerm is too old for the Core.
    AppTooOld,
}

/// What a Machine's row says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MachineStage {
    Running,
    Connecting,
    Offline,
    Stopped,
    /// A push held until SpaceTerm updates or its startup check ends.
    WaitingForApp {
        version: Version,
    },
    Downloading {
        version: Version,
        fraction: Option<f32>,
        /// The person started it on this desktop.
        here: bool,
    },
    Downloaded {
        version: Version,
        continuation: Continuation,
        /// Paired Devices on an older SpaceTerm that a breaking restart strands.
        stranded: Vec<String>,
    },
    /// `to` is None for a same-version restart.
    Restarting {
        to: Option<Version>,
        here: bool,
    },
    /// `kept`: the version stays downloaded.
    StartFailed {
        version: Version,
        cause: StartFailure,
        kept: bool,
        id: u64,
    },
    /// `retries`: the Core tries again at its next check. A push is not retried in this run.
    DownloadFailed {
        version: Version,
        cause: DownloadCause,
        retries: bool,
        id: u64,
    },
    /// A restart got no answer for 180 seconds.
    NotConfirmed {
        version: Version,
        here: bool,
    },
    CantCheck,
    /// The Core runs below the Update Floor of this desktop's embedded build, so it must first
    /// fetch a Stepping Stone at `floor` from its own feed.
    NeedsFloor {
        /// The embedded build's version.
        version: Version,
        floor: Floor,
        /// A fetch or push this run failed, and SpaceTerm does not retry it this run.
        failed: bool,
    },
    SteppingStoneSkipped,
}

/// Why a download of SpaceTerm Core failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DownloadCause {
    DiskFull,
    Unreachable,
    Busy,
    RelaunchRequired,
    Failed,
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

/// What SpaceTerm's own update service knows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AppFacts<'a> {
    pub(crate) state: &'a UpdateState,
    /// The retained pending version, which survives a failed check or download.
    pub(crate) pending: Option<&'a str>,
    /// The running SpaceTerm version.
    pub(crate) running: &'a str,
    /// The ladder stage of the waiting update and the whole days it waited.
    pub(crate) ladder: Stage,
    pub(crate) waited: u64,
    /// The first run on a newer SpaceTerm, with its notes not opened yet.
    pub(crate) whats_new: bool,
    /// This build carries release notes.
    pub(crate) notes: bool,
    /// A failed relaunch left its installer with the updater.
    pub(crate) relaunch_held: bool,
    /// The person started the download in progress on this desktop.
    pub(crate) download_asked: bool,
    /// SpaceTerm tries the failed automatic download again by itself.
    pub(crate) retrying: bool,
    /// The downloaded release a scheduled check replaces, while it does.
    pub(crate) replacing: Option<&'a str>,
    /// The person dismissed the current failure on this desktop.
    pub(crate) failure_dismissed: bool,
}

/// What only this desktop knows about the Machines, from the shared store.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Marks<'a> {
    pub(crate) dismissed: &'a Dismissed,
    /// When the person last cancelled a Machine's restart confirmation, in this run.
    pub(crate) deferred: &'a HashMap<MachineId, u64>,
    /// The store holds automatic pushes until SpaceTerm updates or its startup check ends.
    pub(crate) pushes_held: bool,
}

impl UpdateView {
    pub(crate) fn new(
        app: AppFacts<'_>,
        machines: &[MachineUpdate],
        marks: Marks<'_>,
        now: u64,
    ) -> Self {
        let mut machines: Vec<MachineRow> = machines
            .iter()
            .map(|update| MachineRow::new(update, marks, now))
            .collect();
        // Local first, then store order.
        machines.sort_by_key(|row| !row.local);
        let too_old_for = machines
            .iter()
            .find(|row| row.blocked == Some(Blocked::AppTooOld))
            .map(|row| row.name.clone());
        let app = AppRow::new(app, too_old_for);
        let step = std::iter::once(app.step())
            .chain(machines.iter().map(MachineRow::step))
            .flatten()
            .enumerate()
            .min_by_key(|(order, step)| (step.rank(), *order))
            .map(|(_, step)| step);
        Self {
            app,
            machines,
            step,
        }
    }

    pub(crate) fn machine(&self, machine: MachineId) -> Option<&MachineRow> {
        self.machines.iter().find(|row| row.id == machine)
    }

    /// The split pill, or None when nothing is owed.
    pub(crate) fn pill(&self) -> Option<PillView> {
        super::copy::title_bar::pill(self)
    }

    pub(crate) fn app_row(&self) -> RowView {
        super::copy::rows::app_row(&self.app)
    }

    /// Local first, then store order. `machine_rows()[i]` describes `machines[i]`, and the
    /// count and order stay stable while facts change.
    pub(crate) fn machine_rows(&self) -> Vec<RowView> {
        self.machines
            .iter()
            .map(super::copy::rows::machine_row)
            .collect()
    }

    /// The row of `machine`, when it is listed.
    pub(crate) fn machine_row(&self, machine: MachineId) -> Option<RowView> {
        self.machine(machine).map(super::copy::rows::machine_row)
    }

    /// Whether any waiting update, on any row, is at the Insist stage or a hard stop, even when
    /// a step of higher priority holds the main area.
    pub(crate) fn insists(&self) -> bool {
        let insists = |step: Option<Step>| {
            matches!(
                step,
                Some(
                    Step::Act {
                        stage: Some(Stage::Insist),
                        ..
                    } | Step::Act {
                        hard_stop: true,
                        ..
                    }
                )
            )
        };
        insists(self.app.step()) || self.machines.iter().any(|row| insists(row.waiting_step()))
    }
}

impl Step {
    /// Lower is more urgent: declaration order, and action steps by stage (hard stop, Insist,
    /// Ask, Offer), then by kind.
    fn rank(self) -> u8 {
        match self {
            Self::Failed(_) => 0,
            Self::NotConfirmed(_) => 1,
            Self::Restarting(_) => 2,
            Self::Downloading(_) => 3,
            Self::Relaunching => 4,
            Self::Act {
                kind,
                stage,
                hard_stop,
            } => {
                let stage = match (hard_stop, stage) {
                    (true, _) => 0,
                    (false, Some(Stage::Insist)) => 1,
                    (false, Some(Stage::Ask)) => 2,
                    (false, Some(Stage::Offer) | None) => 3,
                };
                let kind = match kind {
                    ActKind::Relaunch => 0,
                    ActKind::RestartCore(_) => 1,
                    ActKind::Download => 2,
                };
                5 + stage * 3 + kind
            }
            Self::WhatsNew => 20,
        }
    }

    /// The main area's emphasis.
    pub(crate) fn emphasis(self) -> Emphasis {
        match self {
            Self::Failed(_) | Self::NotConfirmed(_) => Emphasis::Warning,
            Self::Restarting(_) | Self::Downloading(_) | Self::Relaunching | Self::WhatsNew => {
                Emphasis::Plain
            }
            Self::Act {
                stage, hard_stop, ..
            } => ladder::emphasis(stage.unwrap_or(Stage::Insist), hard_stop),
        }
    }
}

impl AppRow {
    fn new(facts: AppFacts<'_>, too_old_for: Option<SharedString>) -> Self {
        let pending: Option<SharedString> = facts.pending.map(|v| v.to_owned().into());
        let version = |v: &String| SharedString::from(v.clone());
        let stage = match facts.state {
            UpdateState::Idle if facts.relaunch_held => AppStage::Downloaded {
                version: pending.clone().unwrap_or_default(),
            },
            UpdateState::Ready { version: v } => AppStage::Downloaded {
                version: version(v),
            },
            UpdateState::Available { version: v } => AppStage::Available {
                version: version(v),
            },
            UpdateState::Downloading {
                version: v,
                received,
                total,
            } => AppStage::Downloading {
                version: version(v),
                fraction: (*total > 0).then(|| (*received as f64 / *total as f64) as f32),
                here: facts.download_asked,
            },
            UpdateState::Verifying { version: v } => AppStage::Downloading {
                version: version(v),
                fraction: None,
                here: facts.download_asked,
            },
            UpdateState::Installing { version: v } => AppStage::Relaunching {
                version: version(v),
            },
            UpdateState::Failed {
                error: UpdateError::SystemTooOld,
            } => AppStage::NeedsNewerSystem { version: pending },
            UpdateState::Failed {
                error: UpdateError::ReadOnly,
            } => AppStage::CantReplace,
            UpdateState::Failed {
                error:
                    error @ (UpdateError::Relaunch
                    | UpdateError::SettingsUnsaved
                    | UpdateError::Installation),
            } => AppStage::RelaunchFailed {
                version: pending,
                error: *error,
                held: facts.relaunch_held,
            },
            UpdateState::Failed {
                error:
                    error @ (UpdateError::Download | UpdateError::Verification | UpdateError::DiskFull),
            } => AppStage::DownloadFailed {
                version: pending,
                error: *error,
                retries: facts.retrying && *error == UpdateError::Download,
            },
            // SpaceTerm tries the failed download again by itself. Its check for that, or that
            // check failing, keeps the failure rather than offering the download for a moment.
            UpdateState::Checking
            | UpdateState::Failed {
                error: UpdateError::Check,
            } if facts.retrying && pending.is_some() => AppStage::DownloadFailed {
                version: pending,
                error: UpdateError::Download,
                retries: true,
            },
            // A known release stays after Stop Download, a dismissed failure, a failed check
            // and an offline launch.
            UpdateState::Idle | UpdateState::Checking | UpdateState::Failed { .. }
                if pending.is_some() =>
            {
                AppStage::Available {
                    version: pending.unwrap_or_default(),
                }
            }
            _ => AppStage::Current {
                whats_new: facts.whats_new && facts.notes,
            },
        };
        let replacing = facts
            .replacing
            .filter(|_| {
                matches!(
                    stage,
                    AppStage::Available { .. }
                        | AppStage::Downloading { here: false, .. }
                        | AppStage::DownloadFailed { retries: true, .. }
                )
            })
            .map(|version| version.to_owned().into());
        Self {
            running: facts.running.to_owned().into(),
            stage,
            ladder: facts.ladder,
            waited: facts.waited,
            notes: facts.notes,
            too_old_for,
            replacing,
            dismissed: facts.failure_dismissed,
        }
    }

    /// The step SpaceTerm's own update contributes.
    pub(crate) fn step(&self) -> Option<Step> {
        let hard_stop = self.too_old_for.is_some();
        let act = |kind| Step::Act {
            kind,
            stage: (!hard_stop).then_some(self.ladder),
            hard_stop,
        };
        if self.replacing.is_some() {
            return Some(act(ActKind::Relaunch));
        }
        match &self.stage {
            AppStage::Current { whats_new: true } => Some(Step::WhatsNew),
            AppStage::Current { whats_new: false } | AppStage::NeedsNewerSystem { .. } => None,
            AppStage::Available { .. } => Some(act(ActKind::Download)),
            AppStage::Downloading { here: true, .. } => Some(Step::Downloading(Target::App)),
            AppStage::Downloading { here: false, .. } => None,
            AppStage::Downloaded { .. } => Some(act(ActKind::Relaunch)),
            AppStage::Relaunching { .. } => Some(Step::Relaunching),
            AppStage::DownloadFailed { retries: true, .. } => None,
            AppStage::DownloadFailed { retries: false, .. }
            | AppStage::RelaunchFailed { .. }
            | AppStage::CantReplace
                if !self.dismissed =>
            {
                Some(Step::Failed(Target::App))
            }
            // A dismissal hides only the failure. A held installer still waits for Relaunch, a
            // release that never downloaded still asks for the download, and a hard stop still
            // asks for it after a failed relaunch.
            AppStage::RelaunchFailed { held: true, .. } => Some(act(ActKind::Relaunch)),
            AppStage::DownloadFailed { retries: false, .. } => Some(act(ActKind::Download)),
            AppStage::RelaunchFailed { .. } if hard_stop => Some(act(ActKind::Download)),
            AppStage::RelaunchFailed { .. } | AppStage::CantReplace => None,
        }
    }

    /// The version that waits, when one does.
    pub(crate) fn waiting(&self) -> Option<&SharedString> {
        if let Some(version) = &self.replacing {
            return Some(version);
        }
        match &self.stage {
            AppStage::Available { version } | AppStage::Downloaded { version } => Some(version),
            _ => None,
        }
    }
}

impl MachineRow {
    pub(crate) fn new(update: &MachineUpdate, marks: Marks<'_>, now: u64) -> Self {
        let facts = update
            .detailed
            .as_ref()
            .map(|detailed| &detailed.facts)
            .or(update.facts.as_ref());
        let (stage, blocked) = machine_stage(update, marks, now);
        let pending = facts.and_then(|facts| facts.pending.as_ref());
        let waiting = matches!(stage, MachineStage::Downloaded { .. });
        let ladder = pending
            .filter(|_| waiting)
            .map(|pending| ladder::stage(pending.installed_at, now));
        let waited = pending.map_or(0, |pending| now.saturating_sub(pending.installed_at) / DAY);
        let dismissed = match &stage {
            MachineStage::StartFailed { id, .. } | MachineStage::DownloadFailed { id, .. } => {
                marks.dismissed.contains(&(update.machine, *id))
            }
            MachineStage::NeedsFloor { .. } => {
                marks.dismissed.contains(&(update.machine, PUSH_FAILURE))
            }
            MachineStage::SteppingStoneSkipped => {
                marks.dismissed.contains(&(update.machine, STEPPING_STONE))
            }
            _ => false,
        };
        // A hard stop has no deferral.
        let deferred = blocked.is_none()
            && ladder.is_some_and(|stage| {
                ladder::deferral_hides(stage, marks.deferred.get(&update.machine).copied(), now)
            });
        Self {
            id: update.machine,
            name: update.name.clone(),
            local: update.local,
            running: facts.map(|facts| facts.running.clone()),
            sessions: facts.map_or(0, |facts| facts.sessions),
            stage,
            blocked,
            ladder,
            waited,
            dismissed,
            deferred,
        }
    }

    /// The step this Machine contributes. Offline or stale facts never produce one, and work
    /// shows only when the person started it on this desktop.
    pub(crate) fn step(&self) -> Option<Step> {
        match &self.stage {
            MachineStage::StartFailed { .. }
            | MachineStage::DownloadFailed { retries: false, .. }
            | MachineStage::NeedsFloor { failed: true, .. }
            | MachineStage::SteppingStoneSkipped
                if !self.dismissed =>
            {
                Some(Step::Failed(Target::Machine(self.id)))
            }
            MachineStage::NotConfirmed { here: true, .. } => Some(Step::NotConfirmed(self.id)),
            MachineStage::Restarting { here: true, .. } => Some(Step::Restarting(self.id)),
            MachineStage::Downloading { here: true, .. } => {
                Some(Step::Downloading(Target::Machine(self.id)))
            }
            _ => self.waiting_step().filter(|_| !self.deferred),
        }
    }

    /// The `Restart Core…` step a waiting Pending Version owes, before any deferral: a blocked
    /// Core at once, a breaking one from Offer, one that applies itself from Ask.
    fn waiting_step(&self) -> Option<Step> {
        let MachineStage::Downloaded { continuation, .. } = &self.stage else {
            return None;
        };
        let stage = self.ladder?;
        let hard_stop = self.blocked == Some(Blocked::CoreTooOld);
        // A Core that restarts in a moment asks nothing, at any stage.
        let owed = hard_stop
            || *continuation == Continuation::NeedsConfirmation
            || (stage >= Stage::Ask && *continuation != Continuation::Soon);
        owed.then_some(Step::Act {
            kind: ActKind::RestartCore(self.id),
            stage: (!hard_stop).then_some(stage),
            hard_stop,
        })
    }
}

/// A Machine's row stage and whether a protocol mismatch blocks its Workspaces. Only a check
/// or install a person started reads as theirs; the Core's own work is the row's alone.
fn machine_stage(
    update: &MachineUpdate,
    marks: Marks<'_>,
    now: u64,
) -> (MachineStage, Option<Blocked>) {
    let here = |asked| update.asked == Some(asked);
    let core_too_old = update.link == MachineLink::Incompatible(Incompatible::ListenerTooOld);
    let blocked = core_too_old.then_some(Blocked::CoreTooOld);
    match &update.link {
        MachineLink::Offline { .. } => return (MachineStage::Offline, None),
        MachineLink::Stopped => return (MachineStage::Stopped, None),
        MachineLink::Unconfirmed { expected, .. } => {
            return (
                MachineStage::NotConfirmed {
                    version: expected.clone(),
                    here: here(Asked::Restart),
                },
                None,
            );
        }
        MachineLink::Incompatible(Incompatible::CallerTooOld) => {
            return (MachineStage::Running, Some(Blocked::AppTooOld));
        }
        MachineLink::Unreported => return (MachineStage::CantCheck, None),
        MachineLink::Incompatible(Incompatible::ListenerTooOld) | MachineLink::Connected => {}
    }
    let detailed = update.detailed.as_ref();
    let known = detailed
        .map(|detailed| &detailed.facts)
        .or(update.facts.as_ref());
    // Newer facts settle a failed push before the store next carries it.
    let push_failed = || {
        update
            .failed_push
            .as_ref()
            .filter(|failed| !known.is_some_and(|facts| failed.settled_by(facts)))
            .map(|failed| {
                let cause = match failed.failure {
                    ActionFailure::NeedsFloor(floor) => {
                        return MachineStage::NeedsFloor {
                            version: failed.version.clone(),
                            floor,
                            failed: true,
                        };
                    }
                    ActionFailure::DiskFull => DownloadCause::DiskFull,
                    ActionFailure::Unreachable | ActionFailure::Unavailable(_) => {
                        DownloadCause::Unreachable
                    }
                    ActionFailure::Busy => DownloadCause::Busy,
                    ActionFailure::RelaunchRequired => DownloadCause::RelaunchRequired,
                    _ => DownloadCause::Failed,
                };
                MachineStage::DownloadFailed {
                    version: failed.version.clone(),
                    cause,
                    retries: false,
                    id: PUSH_FAILURE,
                }
            })
    };
    let Some(facts) = known else {
        if let Some(failed) = push_failed() {
            return (failed, blocked);
        }
        return if core_too_old {
            (MachineStage::Running, blocked)
        } else {
            (MachineStage::Connecting, None)
        };
    };
    match &facts.operation {
        Some(Operation::Installing {
            version,
            received,
            total,
        }) => {
            return (
                MachineStage::Downloading {
                    version: version.clone(),
                    fraction: (*total > 0).then(|| (*received as f64 / *total as f64) as f32),
                    here: here(Asked::Install),
                },
                blocked,
            );
        }
        Some(Operation::RestartingSameVersion) => {
            return (
                MachineStage::Restarting {
                    to: None,
                    here: here(Asked::Restart),
                },
                blocked,
            );
        }
        Some(Operation::Restarting { to }) => {
            return (
                MachineStage::Restarting {
                    to: Some(to.clone()),
                    here: here(Asked::Restart),
                },
                blocked,
            );
        }
        Some(Operation::Stopping | Operation::Uninstalling | Operation::Checking) | None => {}
    }
    if matches!(
        facts.check,
        UpdateCheck::Failing {
            reason: CheckFailure::SteppingStoneSkipped,
            ..
        }
    ) {
        return (MachineStage::SteppingStoneSkipped, blocked);
    }
    // A Pending Version settles an install failure of its own version or an older one, as the
    // Core does, and a start failure of an older version only: only a restart settles a start
    // failure of its own.
    if let Some(failure) = &facts.last_failure {
        let order = |pending: &Version| match (pending.release(), failure.version.release()) {
            (Some(pending), Some(failed)) => Some(pending.cmp(&failed)),
            _ => (*pending == failure.version).then_some(std::cmp::Ordering::Equal),
        };
        let superseded = facts.running == failure.version
            || facts
                .pending
                .as_ref()
                .is_some_and(|pending| match failure.kind {
                    FailureKind::Install(_) => matches!(
                        order(&pending.version),
                        Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                    ),
                    FailureKind::Start(_) => {
                        order(&pending.version) == Some(std::cmp::Ordering::Greater)
                    }
                });
        match failure.kind {
            FailureKind::Start(cause) if !superseded => {
                return (
                    MachineStage::StartFailed {
                        version: failure.version.clone(),
                        cause,
                        kept: cause != StartFailure::NotReady,
                        id: failure.id,
                    },
                    blocked,
                );
            }
            // A feed download that failed is tried again at the Core's next check. A Pending
            // Version beside it keeps its row, without a promise (`Continuation::Unknown`).
            FailureKind::Install(reason) if !superseded && facts.pending.is_none() => {
                return (
                    MachineStage::DownloadFailed {
                        version: failure.version.clone(),
                        cause: match reason {
                            InstallFailure::DiskFull => DownloadCause::DiskFull,
                            InstallFailure::Download | InstallFailure::Verification => {
                                DownloadCause::Failed
                            }
                        },
                        retries: true,
                        id: failure.id,
                    },
                    blocked,
                );
            }
            _ => {}
        }
    }
    if let Some(failed) = push_failed() {
        return (failed, blocked);
    }
    // A restart the person approved here reads as restarting from the approval, before the
    // Core reports the operation.
    if here(Asked::Restart) {
        return (
            MachineStage::Restarting {
                to: facts
                    .pending
                    .as_ref()
                    .map(|pending| pending.version.clone()),
                here: true,
            },
            blocked,
        );
    }
    if let Some(pending) = &facts.pending {
        let continuation = detailed
            .and_then(|detailed| detailed.continuation)
            .or_else(|| continuation(facts))
            .unwrap_or(Continuation::Unknown);
        return (
            MachineStage::Downloaded {
                version: pending.version.clone(),
                continuation,
                stranded: pending
                    .stranded
                    .iter()
                    .map(|device| device.name.clone())
                    .collect(),
            },
            blocked,
        );
    }
    if let UpdateCheck::Failing { since, reason, .. } = facts.check
        && (now.saturating_sub(since) >= CANT_CHECK_AFTER || reason == CheckFailure::ClockBehind)
    {
        return (MachineStage::CantCheck, blocked);
    }
    if let Some(Bundled {
        version,
        floor: Some(floor),
    }) = &update.bundled
    {
        return (
            MachineStage::NeedsFloor {
                version: version.clone(),
                floor: *floor,
                failed: false,
            },
            blocked,
        );
    }
    if let Some(version) = &update.push
        && marks.pushes_held
    {
        return (
            MachineStage::WaitingForApp {
                version: version.clone(),
            },
            blocked,
        );
    }
    (MachineStage::Running, blocked)
}

/// The view of the installed update services now. Without a Machine updates service no
/// Machine appears; without SpaceTerm's update service, SpaceTerm is current.
pub(crate) fn current(cx: &App) -> UpdateView {
    let unavailable = UpdateState::Unavailable;
    let running = env!("SPACETERM_VERSION");
    let now = super::now();
    let notes = super::release_notes::embedded().is_some();
    let updates = cx
        .try_global::<UpdateService>()
        .map(|service| service.0.read(cx));
    let app = match updates {
        Some(updates) => {
            // SpaceTerm's clock: the earliest publication among the versions it waited for,
            // read on the updater's clock.
            let waited = updates
                .waiting_since()
                .map_or(0, |since| updates.now().saturating_sub(since) / DAY);
            AppFacts {
                state: updates.state(),
                pending: updates.pending_version(),
                running,
                ladder: updates.ladder_stage(),
                waited,
                whats_new: updates.whats_new(),
                notes,
                relaunch_held: updates.holds_failed_relaunch(),
                download_asked: updates.download_asked(),
                retrying: updates.retries_download(),
                replacing: updates.replaces_download(),
                failure_dismissed: updates.failure_dismissed(),
            }
        }
        None => AppFacts {
            state: &unavailable,
            pending: None,
            running,
            ladder: Stage::Offer,
            waited: 0,
            whats_new: false,
            notes,
            relaunch_held: false,
            download_asked: false,
            retrying: false,
            replacing: None,
            failure_dismissed: false,
        },
    };
    let none = HashMap::new();
    let empty = Dismissed::new();
    match MachineUpdates::global(cx) {
        Some(store) => {
            let store = store.read(cx);
            let marks = Marks {
                dismissed: store.dismissed(),
                deferred: store.deferred(),
                pushes_held: store.holds_pushes(cx),
            };
            UpdateView::new(app, store.machines(), marks, now)
        }
        None => UpdateView::new(
            app,
            &[],
            Marks {
                dismissed: &empty,
                deferred: &none,
                pushes_held: false,
            },
            now,
        ),
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
pub(crate) mod tests;
