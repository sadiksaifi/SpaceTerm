//! Invariants I1 to I7 and the ladder rules L1 to L3 of design section 11, over generated
//! inputs, and the replays R1 and R5.
use std::rc::Rc;

use spaceterm_core::protocol::capability::{Capabilities, Capability};
use spaceterm_core::update::facts::{
    DetailedUpdateFacts, PendingVersion, Platform, Protocols, StrandedDevice, UpdateCheck,
    UpdateFacts, UpdateFailure,
};

use super::*;
use crate::updates::copy::title_bar::LABELS;
use crate::updates::machines::store::Surface;
use crate::updates::machines::testing::ScriptedMachines;
use crate::updates::machines::{Bundled, FailedPush, MachinePreview, MachineUpdatesEvent};
use spaceterm_core::update::ordering::Floor;

pub(crate) const NOW: u64 = 100 * DAY;
pub(crate) const LOCAL: MachineId = [1; 32];
pub(crate) const PENGUIN: MachineId = [2; 32];
const BUILD_BOX: MachineId = [3; 32];
const NAMES: [&str; 3] = ["This Mac", "penguin", "build-box"];

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn facts(running: &str, sessions: u32) -> UpdateFacts {
    UpdateFacts {
        revision: 1,
        platform: Platform::LinuxX86_64,
        running: version(running),
        pending: None,
        sessions,
        operation: None,
        check: UpdateCheck::Succeeded { at: NOW },
        skipped: None,
        last_failure: None,
    }
}

fn pending(to: &str, breaking: bool, installed_at: u64) -> PendingVersion {
    PendingVersion {
        version: version(to),
        protocols: Protocols::CURRENT,
        breaking,
        installed_at,
        overdue_since: None,
        stranded: Vec::new(),
    }
}

pub(crate) fn machine(
    id: MachineId,
    name: &str,
    local: bool,
    facts: Option<UpdateFacts>,
) -> MachineUpdate {
    MachineUpdate {
        machine: id,
        name: name.to_owned().into(),
        local,
        link: MachineLink::Connected,
        facts,
        detailed: None,
        capabilities: Capabilities::fake(&Capability::ALL),
        push: None,
        bundled: None,
        failed_push: None,
        asked: None,
    }
}

/// A Machine waiting with `to` downloaded since `installed_at`.
pub(crate) fn waiting(
    id: MachineId,
    name: &str,
    sessions: u32,
    breaking: bool,
    installed_at: u64,
) -> MachineUpdate {
    let mut facts = facts("0.11.3", sessions);
    facts.pending = Some(pending("0.11.4", breaking, installed_at));
    machine(id, name, id == LOCAL, Some(facts))
}

pub(crate) fn app(state: &UpdateState) -> AppFacts<'_> {
    AppFacts {
        state,
        pending: None,
        running: "0.11.4",
        ladder: Stage::Offer,
        waited: 0,
        whats_new: false,
        notes: true,
        relaunch_held: false,
        download_asked: false,
        retrying: false,
        replacing: None,
        failure_dismissed: false,
    }
}

struct Desk {
    dismissed: Dismissed,
    deferred: HashMap<MachineId, u64>,
}

impl Desk {
    fn new() -> Self {
        Self {
            dismissed: Dismissed::new(),
            deferred: HashMap::new(),
        }
    }

    fn marks(&self) -> Marks<'_> {
        Marks {
            dismissed: &self.dismissed,
            deferred: &self.deferred,
            pushes_held: false,
        }
    }

    fn view(&self, app: AppFacts<'_>, machines: &[MachineUpdate], now: u64) -> UpdateView {
        UpdateView::new(app, machines, self.marks(), now)
    }
}

/// Every app state the generator covers, with the flags that change its row.
pub(crate) fn app_states() -> Vec<(UpdateState, Option<&'static str>, bool, bool, bool)> {
    let v = || "0.11.5".to_owned();
    let mut states = Vec::new();
    for asked in [false, true] {
        states.push((
            UpdateState::Downloading {
                version: v(),
                received: 1,
                total: 2,
            },
            Some("0.11.5"),
            asked,
            false,
            false,
        ));
    }
    for whats_new in [false, true] {
        states.push((UpdateState::UpToDate, None, false, false, whats_new));
    }
    for retrying in [false, true] {
        states.push((
            UpdateState::Failed {
                error: UpdateError::Download,
            },
            Some("0.11.5"),
            false,
            retrying,
            false,
        ));
    }
    for state in [
        UpdateState::Available { version: v() },
        UpdateState::Ready { version: v() },
        UpdateState::Installing { version: v() },
        UpdateState::Failed {
            error: UpdateError::Relaunch,
        },
        UpdateState::Failed {
            error: UpdateError::ReadOnly,
        },
        UpdateState::Failed {
            error: UpdateError::SystemTooOld,
        },
    ] {
        states.push((state, Some("0.11.5"), false, false, false));
    }
    states
}

/// Every Machine variant the generator covers: link states, update stages, 0, 1 or 3 sessions,
/// every ladder age, started here or elsewhere.
pub(crate) fn machine_variants(id: MachineId, name: &str) -> Vec<MachineUpdate> {
    let local = id == LOCAL;
    let mut all = Vec::new();
    let ages = [NOW, NOW - 3 * DAY, NOW - 8 * DAY];
    for sessions in [0, 1, 3] {
        for installed_at in ages {
            for breaking in [false, true] {
                for asked in [None, Some(Asked::Restart)] {
                    let mut update = waiting(id, name, sessions, breaking, installed_at);
                    update.local = local;
                    update.asked = asked;
                    all.push(update.clone());
                    let mut too_old = update.clone();
                    too_old.link = MachineLink::Incompatible(Incompatible::ListenerTooOld);
                    all.push(too_old);
                }
            }
        }
        let base = machine(id, name, local, Some(facts("0.11.4", sessions)));
        for asked in [
            None,
            Some(Asked::Restart),
            Some(Asked::Install),
            Some(Asked::Check),
        ] {
            let mut restarting = base.clone();
            restarting.asked = asked;
            restarting.facts.as_mut().unwrap().operation = Some(Operation::Restarting {
                to: version("0.11.5"),
            });
            all.push(restarting.clone());
            restarting.facts.as_mut().unwrap().operation = Some(Operation::RestartingSameVersion);
            all.push(restarting.clone());
            restarting.facts.as_mut().unwrap().operation = Some(Operation::Installing {
                version: version("0.11.5"),
                received: 1,
                total: 4,
            });
            all.push(restarting.clone());
            restarting.facts.as_mut().unwrap().operation = Some(Operation::Checking);
            all.push(restarting.clone());
            let mut unconfirmed = base.clone();
            unconfirmed.asked = asked;
            unconfirmed.link = MachineLink::Unconfirmed {
                expected: version("0.11.5"),
                since: NOW,
            };
            all.push(unconfirmed);
        }
        for cause in [
            StartFailure::NotReady,
            StartFailure::PortInUse,
            StartFailure::DiskFull,
            StartFailure::Interrupted,
        ] {
            let mut failed = waiting(id, name, sessions, false, NOW);
            let facts = failed.facts.as_mut().unwrap();
            facts.last_failure = Some(UpdateFailure {
                id: 7,
                at: NOW,
                version: version("0.11.4"),
                kind: FailureKind::Start(cause),
            });
            if cause == StartFailure::NotReady {
                facts.pending = None;
            }
            all.push(failed);
        }
        let mut feed = base.clone();
        feed.facts.as_mut().unwrap().last_failure = Some(UpdateFailure {
            id: 8,
            at: NOW,
            version: version("0.11.5"),
            kind: FailureKind::Install(InstallFailure::Download),
        });
        all.push(feed.clone());
        let mut beside = waiting(id, name, sessions, false, NOW - 3 * DAY);
        beside.facts.as_mut().unwrap().last_failure =
            feed.facts.as_ref().unwrap().last_failure.clone();
        all.push(beside);
        let mut pushed = base.clone();
        pushed.push = Some(version("0.11.5"));
        for failure in [
            ActionFailure::DiskFull,
            ActionFailure::Unreachable,
            ActionFailure::Busy,
            ActionFailure::RelaunchRequired,
            ActionFailure::Failed,
        ] {
            pushed.failed_push = Some(FailedPush {
                version: version("0.11.5"),
                failure,
            });
            all.push(pushed.clone());
        }
        let mut stone = base.clone();
        stone.facts.as_mut().unwrap().check = UpdateCheck::Failing {
            since: NOW - 2 * DAY,
            reason: CheckFailure::SteppingStoneSkipped,
            last_success: None,
        };
        all.push(stone);
        let mut below = base.clone();
        below.bundled = Some(Bundled {
            version: version("0.12.1"),
            floor: Some(Floor::parse("0.12.0").unwrap()),
        });
        all.push(below.clone());
        let mut fetch_failed = below.clone();
        fetch_failed.failed_push = Some(FailedPush {
            version: version("0.12.1"),
            failure: ActionFailure::NeedsFloor(Floor::parse("0.12.0").unwrap()),
        });
        all.push(fetch_failed);
        below.link = MachineLink::Incompatible(Incompatible::ListenerTooOld);
        all.push(below);
        all.push(base);
    }
    for link in [
        MachineLink::Offline { since: NOW },
        MachineLink::Stopped,
        MachineLink::Incompatible(Incompatible::CallerTooOld),
        MachineLink::Unreported,
    ] {
        let mut update = waiting(id, name, 2, false, NOW - 8 * DAY);
        update.link = link;
        all.push(update);
    }
    // A Core with the detailed facts reports its engine's own continuation, which can differ
    // from the one released Cores' facts imply.
    for continuation in [
        Continuation::WhenNothingRuns,
        Continuation::Soon,
        Continuation::StartFailed,
    ] {
        for installed_at in [NOW, NOW - 3 * DAY, NOW - 8 * DAY] {
            let mut update = waiting(id, name, 2, false, installed_at);
            update.detailed = Some(DetailedUpdateFacts {
                facts: update.facts.clone().unwrap(),
                continuation: Some(continuation),
                working: 0,
                restart: None,
            });
            all.push(update);
        }
    }
    all.push(machine(id, name, local, None));
    all
}

/// Every generated scene: an app state with 0 to 3 Machines, varying one Machine at a time.
fn scenes(mut each: impl FnMut(&UpdateView, &[MachineUpdate])) {
    let desk = Desk::new();
    let states = app_states();
    let fleets: Vec<Vec<MachineUpdate>> = {
        let mut fleets = vec![Vec::new()];
        for variant in machine_variants(PENGUIN, "penguin") {
            fleets.push(vec![variant]);
        }
        for variant in machine_variants(LOCAL, "This Mac") {
            fleets.push(vec![
                variant,
                waiting(PENGUIN, "penguin", 0, false, NOW),
                machine(BUILD_BOX, "build-box", false, None),
            ]);
        }
        fleets
    };
    for (state, pending, asked, retrying, whats_new) in &states {
        for ladder in [Stage::Offer, Stage::Ask, Stage::Insist] {
            let mut facts = app(state);
            facts.pending = *pending;
            facts.download_asked = *asked;
            facts.retrying = *retrying;
            facts.whats_new = *whats_new;
            facts.ladder = ladder;
            facts.waited = match ladder {
                Stage::Offer => 0,
                Stage::Ask => 3,
                Stage::Insist => 8,
            };
            for fleet in &fleets {
                each(&desk.view(facts, fleet, NOW), fleet);
            }
        }
    }
}

/// Every string a view renders, for the text checks.
fn texts(view: &UpdateView) -> Vec<String> {
    let mut out = Vec::new();
    let mut row = |row: RowView| {
        out.extend(row.status.map(|s| s.to_string()));
        out.extend(row.description.map(|s| s.to_string()));
        out.push(row.accessibility.to_string());
        if let Some(action) = row.action {
            out.push(action.label.to_string());
            out.extend(action.subtitle.map(|s| s.to_string()));
        }
    };
    row(view.app_row());
    view.machine_rows().into_iter().for_each(&mut row);
    if let Some(pill) = view.pill() {
        out.push(pill.label.to_string());
        out.push(pill.tooltip.to_string());
        if let Command::Explain(alert) = pill.main {
            out.push(alert.title.to_string());
            out.push(alert.message.to_string());
        }
    }
    out
}

/// I1: every main-area label is one of the nine, with no digit and no Machine name. `Step`
/// carries no count by type.
#[test]
fn i1_the_main_area_shows_one_of_nine_labels_with_no_count_or_place() {
    scenes(|view, _| {
        assert_eq!(view.pill().is_some(), view.step.is_some());
        if let Some(pill) = view.pill() {
            let label = pill.label.as_ref();
            assert!(LABELS.contains(&label), "{label}");
            assert!(!label.chars().any(|c| c.is_ascii_digit()), "{label}");
            assert!(NAMES.iter().all(|name| !label.contains(name)), "{label}");
        }
    });
}

/// I2: before the Ask stage, a Machine whose update applies itself contributes no step.
#[test]
fn i2_an_update_that_applies_itself_waits_for_the_ask_stage() {
    scenes(|view, _| {
        for row in &view.machines {
            if let MachineStage::Downloaded { continuation, .. } = &row.stage
                && row.ladder == Some(Stage::Offer)
                && row.blocked.is_none()
                && *continuation != Continuation::NeedsConfirmation
            {
                assert_eq!(row.step(), None, "{:?}", row.stage);
            }
        }
    });
}

/// I3: work nobody here started (a Core's check, download or push, a silent restart, another
/// device's restart, a failure the Core retries) changes nothing: with no `Asked` mark, it
/// contributes no step.
#[test]
fn i3_work_nobody_here_started_contributes_no_step() {
    scenes(|view, fleet| {
        for (row, update) in view.machines.iter().zip(sorted(fleet)) {
            let background = matches!(
                row.stage,
                MachineStage::Restarting { .. }
                    | MachineStage::Downloading { .. }
                    | MachineStage::NotConfirmed { .. }
                    | MachineStage::DownloadFailed { retries: true, .. }
                    | MachineStage::Running
                    | MachineStage::Connecting
                    | MachineStage::Offline
                    | MachineStage::Stopped
                    | MachineStage::CantCheck
            );
            if background && update.asked.is_none() {
                assert_eq!(row.step(), None, "{:?}", row.stage);
            }
        }
    });
}

fn sorted(fleet: &[MachineUpdate]) -> Vec<&MachineUpdate> {
    let mut fleet: Vec<&MachineUpdate> = fleet.iter().collect();
    fleet.sort_by_key(|update| !update.local);
    fleet
}

/// I4: the list keeps its rows' number and order while facts change: local first, then store
/// order, one row per Machine whatever its state.
#[test]
fn i4_the_list_keeps_its_rows_while_facts_change() {
    let desk = Desk::new();
    let state = UpdateState::UpToDate;
    let order = |view: &UpdateView| -> Vec<SharedString> {
        view.machine_rows()
            .into_iter()
            .map(|row| row.name)
            .collect()
    };
    let variants = machine_variants(LOCAL, "This Mac");
    let expected: Vec<SharedString> = vec!["This Mac".into(), "penguin".into(), "build-box".into()];
    for local in &variants {
        for penguin in machine_variants(PENGUIN, "penguin").into_iter().step_by(7) {
            let fleet = [
                penguin,
                machine(BUILD_BOX, "build-box", false, None),
                local.clone(),
            ];
            let view = desk.view(app(&state), &fleet, NOW);
            assert_eq!(order(&view), expected);
            assert_eq!(view.machine_rows().len(), view.machines.len());
            for (row, rendered) in view.machines.iter().zip(view.machine_rows()) {
                assert_eq!(view.machine_row(row.id), Some(rendered));
            }
        }
    }
}

/// I5: each action maps to one command on every surface. `Download` never relaunches, and a
/// label ends in an ellipsis exactly when its command can open a confirmation or more input.
#[test]
fn i5_each_action_has_one_command_and_download_never_relaunches() {
    scenes(|view, _| {
        let mut actions: Vec<RowAction> = view
            .machine_rows()
            .into_iter()
            .filter_map(|row| row.action)
            .collect();
        actions.extend(view.app_row().action);
        for action in actions {
            let label = action.label.as_ref();
            let asks = matches!(
                action.command,
                Command::Machine(_, MachineAction::Restart) | Command::Explain(_)
            );
            assert_eq!(
                label.ends_with('\u{2026}'),
                asks,
                "{label}: {:?}",
                action.command
            );
            match label {
                "Download" => assert!(matches!(
                    action.command,
                    Command::Desktop(DesktopAction::Download)
                        | Command::Machine(_, MachineAction::Install)
                )),
                "Relaunch to Update" => {
                    assert_eq!(action.command, Command::Desktop(DesktopAction::Update))
                }
                "Restart Core\u{2026}" | "Try Again\u{2026}" => {
                    assert!(matches!(
                        action.command,
                        Command::Machine(_, MachineAction::Restart)
                    ))
                }
                "What's New" => {
                    assert_eq!(action.command, Command::Desktop(DesktopAction::WhatsNew))
                }
                _ => {}
            }
        }
        if let Some(pill) = view.pill() {
            match pill.label.as_ref() {
                "Download Update" => {
                    assert_eq!(pill.main, Command::Desktop(DesktopAction::Download))
                }
                "Relaunch to Update" => {
                    assert_eq!(pill.main, Command::Desktop(DesktopAction::Update))
                }
                "Restart Core\u{2026}" => {
                    assert!(matches!(
                        pill.main,
                        Command::Machine(_, MachineAction::Restart)
                    ))
                }
                _ => {}
            }
        }
    });
}

/// I6: no consequence carries a zero count, no text says "installed", "ready" or "idle", and no
/// action names the computer.
#[test]
fn i6_no_text_counts_zero_or_uses_a_banned_word() {
    scenes(|view, _| {
        for text in texts(view) {
            assert!(!text.contains("0 Terminal Session"), "{text}");
            for banned in [
                "installed",
                "ready",
                "idle",
                "Update This",
                "Update penguin",
                "\u{2019}",
            ] {
                assert!(!text.contains(banned), "{banned}: {text}");
            }
        }
    });
}

/// I7: the pill's `Restart Core…` performs the same command as its Machine's row, and every
/// surface reads the same row.
#[test]
fn i7_the_pill_and_the_row_agree() {
    scenes(|view, _| {
        if let Some(Step::Act {
            kind: ActKind::RestartCore(id),
            ..
        }) = view.step
        {
            let row = view.machine_row(id).unwrap();
            assert_eq!(
                row.action.map(|action| action.command),
                view.pill().map(|pill| pill.main)
            );
        }
    });
}

/// I7 for SpaceTerm's own row: the menu bar extra's SpaceTerm step is the row's action, with the
/// same command, label and subtitle. Only `Download` reads `Download Update` there, the label
/// the pill gives it.
#[test]
fn i7_the_menu_bar_extra_offers_the_spaceterm_row_action() {
    use crate::status_menu::model::{Command as MenuCommand, Menu};
    use crate::updates::copy::{rows, settings};
    scenes(|view, fleet| {
        let menu = Menu::new(view, fleet);
        let action = view
            .app_row()
            .action
            .filter(|action| action.command != Command::Desktop(DesktopAction::WhatsNew));
        match (action, menu.step) {
            (None, None) => {}
            (Some(action), Some(step)) => {
                let label = if action.label.as_ref() == rows::DOWNLOAD {
                    settings::DOWNLOAD_UPDATE.into()
                } else {
                    action.label
                };
                assert_eq!(step.title, label);
                assert_eq!(step.subtitle, action.subtitle);
                assert_eq!(step.command, MenuCommand::Update(action.command));
            }
            (action, step) => panic!("row {action:?}, extra {step:?}"),
        }
    });
}

/// L1: presence, emphasis and cadence follow the stage only, and no label or row status
/// changes text between stages.
#[test]
fn l1_a_stage_changes_emphasis_never_words() {
    let desk = Desk::new();
    let state = UpdateState::UpToDate;
    for sessions in [1, 2] {
        let at = |days: u64| {
            desk.view(
                app(&state),
                &[waiting(
                    LOCAL,
                    "This Mac",
                    sessions,
                    false,
                    NOW - days * DAY,
                )],
                NOW,
            )
        };
        let offer = at(1);
        assert_eq!(
            offer.step, None,
            "a Core that applies itself is list only at Offer"
        );
        let (ask, insist) = (at(3), at(8));
        let (ask_pill, insist_pill) = (ask.pill().unwrap(), insist.pill().unwrap());
        assert_eq!(ask_pill.label, insist_pill.label);
        assert_eq!(ask_pill.tooltip, insist_pill.tooltip);
        assert_eq!(ask_pill.emphasis, Emphasis::Accent);
        assert_eq!(insist_pill.emphasis, Emphasis::Warning);
        assert_eq!(offer.machine_rows()[0].status, ask.machine_rows()[0].status);
        assert_eq!(
            ask.machine_rows()[0].status,
            insist.machine_rows()[0].status
        );
        assert!(!ask.insists() && insist.insists());
    }
}

/// L2: the Core clock is `installed_at`, which a replacing version inherits, and a restart
/// starts a fresh one.
#[test]
fn l2_the_clock_inherits_across_a_replacement_and_resets_after_a_restart() {
    let desk = Desk::new();
    let state = UpdateState::UpToDate;
    let old = waiting(LOCAL, "This Mac", 2, false, NOW - 3 * DAY);
    let mut replaced = old.clone();
    replaced
        .facts
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .version = version("0.11.5");
    let stage = |update: &MachineUpdate| {
        desk.view(app(&state), std::slice::from_ref(update), NOW)
            .machines[0]
            .ladder
    };
    assert_eq!(stage(&old), Some(Stage::Ask));
    assert_eq!(
        stage(&replaced),
        Some(Stage::Ask),
        "the newer version inherits the clock"
    );
    let mut restarted = waiting(LOCAL, "This Mac", 2, false, NOW);
    restarted.facts.as_mut().unwrap().running = version("0.11.4");
    assert_eq!(stage(&restarted), Some(Stage::Offer));
}

/// L3: Cancel hides an Ask step for exactly 24 hours, and never an Insist or hard-stop step.
#[test]
fn l3_cancel_defers_an_ask_step_for_a_day_only() {
    let state = UpdateState::UpToDate;
    let mut desk = Desk::new();
    let cancelled = NOW - ladder::ASK_DEFERRAL + 1;
    desk.deferred.insert(LOCAL, cancelled);
    let view =
        |update: MachineUpdate, desk: &Desk, now| desk.view(app(&state), &[update], now).step;
    let ask = waiting(LOCAL, "This Mac", 2, false, NOW - 3 * DAY);
    assert_eq!(view(ask.clone(), &desk, NOW), None);
    assert!(
        view(ask, &desk, NOW + 1).is_some(),
        "24 hours after Cancel it asks again"
    );
    let insist = waiting(LOCAL, "This Mac", 2, false, NOW - 8 * DAY);
    assert!(view(insist, &desk, NOW).is_some());
    let mut blocked = waiting(LOCAL, "This Mac", 2, false, NOW - 3 * DAY);
    blocked.link = MachineLink::Incompatible(Incompatible::ListenerTooOld);
    assert!(matches!(
        view(blocked, &desk, NOW),
        Some(Step::Act {
            hard_stop: true,
            stage: None,
            ..
        })
    ));
    desk.deferred.clear();
}

/// The step is the minimum over every row: failures first, then progress the person started,
/// then action steps by stage and kind, then What's New.
#[test]
fn the_step_follows_the_priority_table() {
    let desk = Desk::new();
    let ready = UpdateState::Ready {
        version: "0.11.5".into(),
    };
    let insisting = waiting(PENGUIN, "penguin", 1, false, NOW - 8 * DAY);
    let mut facts = app(&ready);
    facts.ladder = Stage::Ask;
    assert_eq!(
        desk.view(facts, std::slice::from_ref(&insisting), NOW).step,
        Some(Step::Act {
            kind: ActKind::RestartCore(PENGUIN),
            stage: Some(Stage::Insist),
            hard_stop: false
        }),
        "Insist outranks Ask"
    );
    facts.ladder = Stage::Insist;
    assert_eq!(
        desk.view(facts, std::slice::from_ref(&insisting), NOW).step,
        Some(Step::Act {
            kind: ActKind::Relaunch,
            stage: Some(Stage::Insist),
            hard_stop: false
        }),
        "within a stage, Relaunch comes first"
    );
    let mut failed = insisting;
    failed.failed_push = Some(FailedPush {
        version: version("0.11.5"),
        failure: ActionFailure::DiskFull,
    });
    failed.facts.as_mut().unwrap().pending = None;
    let view = desk.view(facts, &[failed], NOW);
    assert_eq!(view.step, Some(Step::Failed(Target::Machine(PENGUIN))));
    let pill = view.pill().unwrap();
    assert_eq!(pill.label.as_ref(), "Update Failed");
    assert_eq!(
        pill.tooltip.as_ref(),
        "Couldn't download SpaceTerm Core 0.11.5 to penguin. penguin is out of disk space."
    );
    let mut whats_new = app(&UpdateState::UpToDate);
    whats_new.whats_new = true;
    assert_eq!(desk.view(whats_new, &[], NOW).step, Some(Step::WhatsNew));
}

/// The rows of design 4.7 for the recorded journey, and the D1 confirmation.
#[test]
fn the_rows_and_the_confirmation_name_the_software_and_the_consequence() {
    let desk = Desk::new();
    let state = UpdateState::UpToDate;
    let mut this_mac = waiting(LOCAL, "This Mac", 2, false, NOW);
    let view = desk.view(app(&state), &[this_mac.clone()], NOW);
    let row = &view.machine_rows()[0];
    assert_eq!(
        row.status.as_deref(),
        Some("Restarts into 0.11.4 when 2 Terminal Sessions end")
    );
    let action = row.action.clone().unwrap();
    assert_eq!(action.label.as_ref(), "Restart Core\u{2026}");
    assert_eq!(action.subtitle.as_deref(), Some("Ends 2 Terminal Sessions"));
    assert_eq!(
        row.accessibility.as_ref(),
        "SpaceTerm Core on This Mac restarts into 0.11.4 when its 2 Terminal Sessions end. Restarting SpaceTerm Core now ends 2 Terminal Sessions."
    );

    this_mac
        .facts
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .installed_at = NOW - 2 * DAY;
    let view = desk.view(app(&state), &[this_mac.clone()], NOW);
    let pill = view.pill().unwrap();
    assert_eq!(pill.label.as_ref(), "Restart Core\u{2026}");
    assert_eq!(pill.glyph, Glyph::UpdateCore);
    assert_eq!(
        pill.tooltip.as_ref(),
        "SpaceTerm Core 0.11.4 is downloaded on This Mac. Restarting into it ends 2 Terminal Sessions.",
        "a stage changes emphasis, never the tooltip's words"
    );
    assert_eq!(
        view.machine_rows()[0].description.as_deref(),
        Some("An update has waited 2 days.")
    );

    let preview = MachinePreview {
        machine: LOCAL,
        action: MachineAction::Restart,
        sessions: 2,
        running: Some(version("0.11.3")),
        target: Some(version("0.11.4")),
        id: 1,
    };
    let confirmation = super::super::confirm::previewed(
        std::slice::from_ref(&this_mac),
        std::slice::from_ref(&preview),
        false,
        NOW,
    )
    .unwrap();
    assert_eq!(
        confirmation.title.as_ref(),
        "Restart SpaceTerm Core on This Mac?"
    );
    assert_eq!(
        confirmation.message.as_ref(),
        "SpaceTerm Core restarts from 0.11.3 to 0.11.4. This ends 2 Terminal Sessions. Workspaces, Tabs and Panes stay."
    );
    assert_eq!(confirmation.button.as_ref(), "Restart Core");
    assert_eq!(
        confirmation.cancel.as_ref(),
        "Remind Me Tomorrow",
        "at Ask the other button hides the step for a day, and says so"
    );
    assert!(confirmation.approval.holds(&[this_mac.clone()]));
    for days in [1, 8] {
        let mut staged = this_mac.clone();
        staged
            .facts
            .as_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .installed_at = NOW - days * DAY;
        let cancel = super::super::confirm::previewed(
            std::slice::from_ref(&staged),
            std::slice::from_ref(&preview),
            false,
            NOW,
        )
        .unwrap()
        .cancel;
        assert_eq!(
            cancel.as_ref(),
            "Cancel",
            "at Offer and Insist it changes nothing: {days} days"
        );
    }

    let mut breaking = this_mac.clone();
    let pending = breaking.facts.as_mut().unwrap().pending.as_mut().unwrap();
    pending.breaking = true;
    pending.stranded = vec![StrandedDevice {
        pairing: Default::default(),
        name: "iPad".into(),
    }];
    let none = MachinePreview {
        sessions: 0,
        ..preview
    };
    let confirmation = super::super::confirm::previewed(&[breaking], &[none], false, NOW).unwrap();
    assert_eq!(
        confirmation.message.as_ref(),
        "SpaceTerm Core restarts from 0.11.3 to 0.11.4. No Terminal Session is open. Workspaces, Tabs and Panes stay. SpaceTerm on iPad can't connect to This Mac until it updates."
    );
}

fn store_view(cx: &mut gpui::TestAppContext, app: &AppFacts<'_>) -> (UpdateView, usize) {
    let store = cx.update(|cx| MachineUpdates::global(cx).unwrap());
    cx.read(|cx| {
        let store = store.read(cx);
        let marks = Marks {
            dismissed: store.dismissed(),
            deferred: store.deferred(),
            pushes_held: false,
        };
        let view = UpdateView::new(*app, store.machines(), marks, NOW);
        let rows = view.machine_rows().len();
        (view, rows)
    })
}

/// R1, the recorded journey: after the relaunch to 0.11.4, This Mac waits on 2 Terminal Sessions
/// and penguin restarts itself with none. Every frame shows `What's New`, and the list keeps
/// its two Machine rows while penguin's text changes in place.
#[gpui::test]
fn r1_the_recorded_journey_shows_whats_new_for_every_frame(cx: &mut gpui::TestAppContext) {
    let adapter = Rc::new(ScriptedMachines::default());
    cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
    let state = UpdateState::UpToDate;
    let mut facts = app(&state);
    facts.whats_new = true;
    let this_mac = waiting(LOCAL, "This Mac", 2, false, NOW);
    let penguin = waiting(PENGUIN, "penguin", 0, false, NOW);
    let mut restarting = penguin.clone();
    restarting.facts.as_mut().unwrap().operation = Some(Operation::Restarting {
        to: version("0.11.4"),
    });
    let mut away = restarting.clone();
    away.link = MachineLink::Offline { since: NOW };
    let mut back = machine(PENGUIN, "penguin", false, Some(facts_running("0.11.4")));
    back.facts.as_mut().unwrap().revision = 2;
    let frames = [
        MachineUpdatesEvent::Machines(vec![this_mac.clone(), penguin.clone()]),
        MachineUpdatesEvent::Changed(Box::new(restarting)),
        MachineUpdatesEvent::Changed(Box::new(away)),
        MachineUpdatesEvent::Changed(Box::new(back)),
    ];
    let mut penguin_status = Vec::new();
    for frame in frames {
        adapter.emit(frame);
        cx.run_until_parked();
        let (view, rows) = store_view(cx, &facts);
        let pill = view.pill().expect("the pill stays visible");
        assert_eq!(pill.label.as_ref(), "What's New");
        assert_eq!(pill.tooltip.as_ref(), "See what's new in SpaceTerm 0.11.4.");
        assert_eq!(rows, 2);
        let rows = view.machine_rows();
        assert_eq!(rows[0].name.as_ref(), "This Mac");
        assert_eq!(
            rows[0].status.as_deref(),
            Some("Restarts into 0.11.4 when 2 Terminal Sessions end")
        );
        penguin_status.push(rows[1].status.clone().unwrap().to_string());
    }
    assert_eq!(
        penguin_status,
        [
            "Restarts into 0.11.4 in a moment",
            "Restarting into 0.11.4\u{2026}",
            "Offline",
            "0.11.4",
        ]
    );
}

fn facts_running(running: &str) -> UpdateFacts {
    facts(running, 0)
}

/// R5: a restart the person approved here is the title bar's step until the Core comes back;
/// the same restart started elsewhere is no step at all.
#[gpui::test]
fn r5_only_a_restart_approved_here_shows_restart_progress(cx: &mut gpui::TestAppContext) {
    let adapter = Rc::new(ScriptedMachines::default());
    cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
    let store = cx.update(|cx| MachineUpdates::global(cx).unwrap());
    let state = UpdateState::UpToDate;
    let facts = app(&state);
    let this_mac = waiting(LOCAL, "This Mac", 2, false, NOW);
    let mut restarting = this_mac.clone();
    restarting.facts.as_mut().unwrap().operation = Some(Operation::Restarting {
        to: version("0.11.4"),
    });
    let mut back = machine(LOCAL, "This Mac", true, Some(facts_running("0.11.4")));
    back.facts.as_mut().unwrap().revision = 2;

    adapter.emit(MachineUpdatesEvent::Machines(vec![this_mac.clone()]));
    adapter.emit(MachineUpdatesEvent::Changed(Box::new(restarting.clone())));
    cx.run_until_parked();
    assert_eq!(
        store_view(cx, &facts).0.step,
        None,
        "another device's restart"
    );

    adapter.emit(MachineUpdatesEvent::Changed(Box::new(this_mac)));
    cx.run_until_parked();
    let preview = MachinePreview {
        machine: LOCAL,
        action: MachineAction::Restart,
        sessions: 2,
        running: Some(version("0.11.3")),
        target: Some(version("0.11.4")),
        id: 1,
    };
    store.update(cx, |store, cx| store.execute(preview, Surface::Window, cx));
    let shown = |cx: &mut gpui::TestAppContext| {
        let (view, _) = store_view(cx, &facts);
        (view.step, view.pill().map(|pill| pill.label.to_string()))
    };
    assert_eq!(
        shown(cx),
        (
            Some(Step::Restarting(LOCAL)),
            Some("Restarting Core\u{2026}".into())
        ),
        "from the approval"
    );
    adapter.emit(MachineUpdatesEvent::Finished {
        machine: LOCAL,
        action: MachineAction::Restart,
        outcome: Ok(()),
    });
    adapter.emit(MachineUpdatesEvent::Changed(Box::new(restarting)));
    cx.run_until_parked();
    assert_eq!(
        shown(cx),
        (
            Some(Step::Restarting(LOCAL)),
            Some("Restarting Core\u{2026}".into())
        )
    );
    let pill = store_view(cx, &facts).0.pill().unwrap();
    assert_eq!(
        pill.tooltip.as_ref(),
        "Restarting SpaceTerm Core on This Mac into 0.11.4."
    );
    assert_eq!(pill.main, Command::ShowList);

    adapter.emit(MachineUpdatesEvent::Changed(Box::new(back)));
    cx.run_until_parked();
    assert_eq!(
        shown(cx),
        (None, None),
        "the pill hides once the Core is back"
    );
}

fn installed_store(
    cx: &mut gpui::TestAppContext,
) -> (Rc<ScriptedMachines>, gpui::Entity<MachineUpdates>) {
    let adapter = Rc::new(ScriptedMachines::default());
    cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
    let store = cx.update(|cx| MachineUpdates::global(cx).unwrap());
    (adapter, store)
}

fn approved_restart() -> MachinePreview {
    MachinePreview {
        machine: LOCAL,
        action: MachineAction::Restart,
        sessions: 2,
        running: Some(version("0.11.3")),
        target: Some(version("0.11.4")),
        id: 1,
    }
}

fn restarting(update: &MachineUpdate) -> MachineUpdate {
    let mut restarting = update.clone();
    restarting.facts.as_mut().unwrap().operation = Some(Operation::Restarting {
        to: version("0.11.4"),
    });
    restarting
}

fn returned() -> MachineUpdate {
    let mut back = machine(LOCAL, "This Mac", true, Some(facts("0.11.4", 0)));
    back.facts.as_mut().unwrap().revision = 2;
    back
}

fn step_now(cx: &mut gpui::TestAppContext) -> Option<Step> {
    let state = UpdateState::UpToDate;
    store_view(cx, &app(&state)).0.step
}

/// A restart approved here that never comes back is this desktop's `Restart Not Confirmed`,
/// whichever of the adapter's deadline and the lane's uncertain answer arrives first, and the
/// store's own later deadline does not end it.
#[gpui::test]
fn a_restart_approved_here_is_not_confirmed_after_the_deadline(cx: &mut gpui::TestAppContext) {
    for uncertain_first in [false, true] {
        let (adapter, store) = installed_store(cx);
        let this_mac = waiting(LOCAL, "This Mac", 2, false, NOW);
        adapter.emit(MachineUpdatesEvent::Machines(vec![this_mac.clone()]));
        cx.run_until_parked();
        store.update(cx, |store, cx| {
            store.execute(approved_restart(), Surface::Window, cx)
        });
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(restarting(
            &this_mac,
        ))));
        cx.run_until_parked();
        assert_eq!(step_now(cx), Some(Step::Restarting(LOCAL)));
        let uncertain = MachineUpdatesEvent::Finished {
            machine: LOCAL,
            action: MachineAction::Restart,
            outcome: Err(ActionFailure::Unreachable),
        };
        let unconfirmed = MachineUpdatesEvent::Changed(Box::new(MachineUpdate {
            link: MachineLink::Unconfirmed {
                expected: version("0.11.4"),
                since: NOW,
            },
            ..restarting(&this_mac)
        }));
        cx.executor()
            .advance_clock(crate::updates::machines::store::RESTART_DEADLINE);
        if uncertain_first {
            adapter.emit(uncertain);
            adapter.emit(unconfirmed);
        } else {
            adapter.emit(unconfirmed);
            adapter.emit(uncertain);
        }
        cx.run_until_parked();
        assert_eq!(step_now(cx), Some(Step::NotConfirmed(LOCAL)));
        cx.executor()
            .advance_clock(2 * crate::updates::machines::store::RESTART_DEADLINE);
        cx.run_until_parked();
        assert_eq!(
            step_now(cx),
            Some(Step::NotConfirmed(LOCAL)),
            "it stays this desktop's"
        );
        let state = UpdateState::UpToDate;
        let pill = store_view(cx, &app(&state)).0.pill().unwrap();
        assert_eq!(pill.label.as_ref(), "Restart Not Confirmed");
        let Command::Explain(alert) = pill.main else {
            panic!("D4 opens from the main area");
        };
        assert_eq!(alert.proceed.as_deref(), Some("Check Again"));

        // The Core answers again on the new version: the restart settled.
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(returned())));
        cx.run_until_parked();
        assert_eq!(step_now(cx), None);
    }
}

/// A restart settles once the Core is back without a restart operation, even when the facts
/// arrive before the request answers, and even when its restart was never observed.
#[gpui::test]
fn a_restart_settles_when_the_core_returns_whatever_the_order(cx: &mut gpui::TestAppContext) {
    for observed in [true, false] {
        let (adapter, store) = installed_store(cx);
        let this_mac = waiting(LOCAL, "This Mac", 2, false, NOW);
        adapter.emit(MachineUpdatesEvent::Machines(vec![this_mac.clone()]));
        cx.run_until_parked();
        store.update(cx, |store, cx| {
            store.execute(approved_restart(), Surface::Window, cx)
        });
        if observed {
            adapter.emit(MachineUpdatesEvent::Changed(Box::new(restarting(
                &this_mac,
            ))));
        }
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(returned())));
        cx.run_until_parked();
        assert_eq!(
            step_now(cx),
            None,
            "back before the answer, observed: {observed}"
        );
        adapter.emit(MachineUpdatesEvent::Finished {
            machine: LOCAL,
            action: MachineAction::Restart,
            outcome: Ok(()),
        });
        cx.run_until_parked();
        assert_eq!(step_now(cx), None);
        assert_eq!(
            store.read_with(cx, |store, _| store.machines()[0].asked),
            None
        );
    }
}

/// SpaceTerm's step reads the ladder's own clock: the days since the earliest publication on
/// the updater's clock, and What's New only on the first run after an update.
#[gpui::test]
fn the_installed_view_reads_the_apps_ladder_clock(cx: &mut gpui::TestAppContext) {
    use crate::updates::testing::RecordingAdapter;
    use crate::updates::{ApplicationUpdates, UpdateEvent};
    let adapter = RecordingAdapter::available();
    *adapter.clock.borrow_mut() = Some(Box::new(|| NOW));
    let adapter = Rc::new(adapter);
    cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
    let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
    service.update(cx, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::ReleaseMetadata {
        published_at: NOW - 6 * DAY,
        prepared: false,
    });
    adapter.emit(UpdateEvent::Available("99.0.0".into()));
    adapter.emit(UpdateEvent::Ready);
    cx.run_until_parked();
    let view = cx.update(|cx| current(cx));
    assert!(
        matches!(view.app.stage, AppStage::Downloaded { .. }),
        "{:?}",
        view.app.stage
    );
    assert_eq!((view.app.ladder, view.app.waited), (Stage::Ask, 6));
    assert_eq!(
        view.pill().unwrap().tooltip.as_ref(),
        "SpaceTerm 99.0.0 is downloaded. Relaunching keeps Terminal Sessions running."
    );

    let mut later = RecordingAdapter::available();
    later.history.launched("0.0.1", 0);
    later.history.launched(env!("SPACETERM_VERSION"), 1);
    let later = Rc::new(later);
    cx.update(|cx| ApplicationUpdates::install(later.clone(), cx));
    let view = cx.update(|cx| current(cx));
    assert_eq!(view.app.stage, AppStage::Current { whats_new: false });
    assert_eq!(view.step, None, "a later launch owes no What's New");
}

/// D3f dismisses the title-bar step from the row's `How to Update…` as from the pill, and the
/// row keeps the failure.
#[gpui::test]
fn failure_alerts_dismiss_the_step_and_keep_the_row(cx: &mut gpui::TestAppContext) {
    let (adapter, store) = installed_store(cx);
    let mut stone = machine(PENGUIN, "penguin", false, Some(facts("0.11.3", 1)));
    stone.facts.as_mut().unwrap().check = UpdateCheck::Failing {
        since: NOW,
        last_success: None,
        reason: CheckFailure::SteppingStoneSkipped,
    };
    adapter.emit(MachineUpdatesEvent::Machines(vec![stone]));
    cx.run_until_parked();
    assert_eq!(step_now(cx), Some(Step::Failed(Target::Machine(PENGUIN))));
    let state = UpdateState::UpToDate;
    let row = store_view(cx, &app(&state)).0.machine_row(PENGUIN).unwrap();
    let Some(RowAction {
        command: Command::Explain(alert),
        ..
    }) = row.action
    else {
        panic!("How to Update… opens D3f");
    };
    let Some(Command::Machine(id, action)) = alert.dismiss.map(|dismiss| *dismiss) else {
        panic!("OK dismisses the step");
    };
    store.update(cx, |store, cx| {
        store.perform(id, action, Surface::Window, cx)
    });
    assert_eq!(step_now(cx), None);
    let row = store_view(cx, &app(&state)).0.machine_row(PENGUIN).unwrap();
    assert_eq!(row.status.as_deref(), Some("Can't update by itself"));
    assert!(
        adapter.performed().is_empty(),
        "the Core's facts stay as they are"
    );
}

/// A connected Core below the Update Floor of this desktop's embedded build, with no push tried.
fn below_the_floor() -> MachineUpdate {
    let mut below = machine(PENGUIN, "penguin", false, Some(facts("0.11.3", 2)));
    below.bundled = Some(Bundled {
        version: version("0.14.0"),
        floor: Some(Floor::parse("0.12.0").unwrap()),
    });
    below
}

/// A Core below the Update Floor offers its Stepping Stone on its row: the floor first, with
/// Download, which asks its Core to fetch it. The title bar owes nothing until a fetch fails,
/// and the row follows the Stepping Stone once its Core fetches it.
#[test]
fn a_core_below_the_update_floor_offers_its_stepping_stone() {
    let state = UpdateState::UpToDate;
    let below = below_the_floor();
    let view = Desk::new().view(app(&state), std::slice::from_ref(&below), NOW);
    assert_eq!(view.step, None, "nothing failed");
    let row = view.machine_rows().remove(0);
    assert_eq!(row.status.as_deref(), Some("Needs 0.12.0 before 0.14.0"));
    assert_eq!(row.description, None, "the status says it whole");
    let action = row.action.expect("the row offers the Stepping Stone");
    assert_eq!(action.label.as_ref(), "Download");
    assert_eq!(
        action.command,
        Command::Machine(PENGUIN, MachineAction::Install)
    );

    let mut too_old = below.clone();
    too_old.link = MachineLink::Incompatible(Incompatible::ListenerTooOld);
    let row = Desk::new()
        .view(app(&state), &[too_old], NOW)
        .machine_rows()
        .remove(0);
    assert_eq!(
        row.status.as_deref(),
        Some("Too old for this SpaceTerm \u{b7} Needs 0.12.0 before 0.14.0")
    );
    assert_eq!(row.action.unwrap().label.as_ref(), "Download");

    let mut fetching = below.clone();
    fetching.facts.as_mut().unwrap().operation = Some(Operation::Installing {
        version: version("0.12.0"),
        received: 1,
        total: 4,
    });
    let mut fetched = below;
    fetched.facts.as_mut().unwrap().pending = Some(pending("0.12.0", false, NOW));
    let view = Desk::new().view(app(&state), &[fetching, fetched], NOW);
    assert!(matches!(
        &view.machines[0].stage,
        MachineStage::Downloading { version: v, .. } if *v == version("0.12.0")
    ));
    assert!(matches!(
        &view.machines[1].stage,
        MachineStage::Downloaded { version: v, .. } if *v == version("0.12.0")
    ));
}

/// Download on a Core below the Update Floor asks its Core to fetch from its feed. A fetch that
/// fails shows once as `Update Failed` with the floor's D3e; dismissing it keeps the row and its
/// Download, and the next failure shows again.
#[gpui::test]
fn a_failed_stepping_stone_fetch_shows_once_and_keeps_the_download(cx: &mut gpui::TestAppContext) {
    let (adapter, store) = installed_store(cx);
    adapter.emit(MachineUpdatesEvent::Machines(vec![below_the_floor()]));
    cx.run_until_parked();
    let state = UpdateState::UpToDate;
    let download = |cx: &mut gpui::TestAppContext| {
        let row = store_view(cx, &app(&state)).0.machine_row(PENGUIN).unwrap();
        assert_eq!(row.status.as_deref(), Some("Needs 0.12.0 before 0.14.0"));
        let Some(RowAction {
            label,
            command: Command::Machine(id, action),
            ..
        }) = row.action
        else {
            panic!("the row keeps its Download");
        };
        assert_eq!(label.as_ref(), "Download");
        store.update(cx, |store, cx| {
            store.perform(id, action, Surface::Window, cx)
        });
        adapter.emit(MachineUpdatesEvent::Finished {
            machine: PENGUIN,
            action: MachineAction::Install,
            outcome: Err(ActionFailure::NeedsFloor(Floor::parse("0.12.0").unwrap())),
        });
        cx.run_until_parked();
    };

    download(cx);
    assert_eq!(
        adapter.performed(),
        [(PENGUIN, MachineAction::Install)],
        "Download asks the Core to fetch from its feed"
    );
    assert_eq!(step_now(cx), Some(Step::Failed(Target::Machine(PENGUIN))));
    let view = store_view(cx, &app(&state)).0;
    let Some(Command::Explain(alert)) = view.pill().map(|pill| pill.main) else {
        panic!("D3e opens from the main area");
    };
    assert_eq!(
        alert.title.as_ref(),
        "Couldn't Download SpaceTerm Core 0.14.0 to penguin"
    );
    assert_eq!(
        alert.message.as_ref(),
        "penguin needs SpaceTerm Core 0.12.0 first and couldn't get it. Its Terminal Sessions keep running."
    );
    let Some(Command::Machine(id, dismiss)) = alert.dismiss.map(|dismiss| *dismiss) else {
        panic!("Cancel dismisses the step");
    };
    store.update(cx, |store, cx| {
        store.perform(id, dismiss, Surface::Window, cx)
    });
    assert_eq!(step_now(cx), None, "the failure shows once");

    download(cx);
    assert_eq!(
        step_now(cx),
        Some(Step::Failed(Target::Machine(PENGUIN))),
        "a new failure shows again"
    );
}

/// A failed Stepping Stone fetch lasts only until newer facts settle it: once the Core has the
/// Stepping Stone downloaded, its row offers the restart and no failure shows; once it runs the
/// Stepping Stone, the floor is behind it.
#[gpui::test]
fn a_stepping_stone_the_core_has_settles_its_failed_fetch(cx: &mut gpui::TestAppContext) {
    let (adapter, store) = installed_store(cx);
    adapter.emit(MachineUpdatesEvent::Machines(vec![below_the_floor()]));
    cx.run_until_parked();
    store.update(cx, |store, cx| {
        store.perform(PENGUIN, MachineAction::Install, Surface::Window, cx)
    });
    adapter.emit(MachineUpdatesEvent::Finished {
        machine: PENGUIN,
        action: MachineAction::Install,
        outcome: Err(ActionFailure::NeedsFloor(Floor::parse("0.12.0").unwrap())),
    });
    cx.run_until_parked();
    assert_eq!(step_now(cx), Some(Step::Failed(Target::Machine(PENGUIN))));

    // The Core's own check fetched it. The floor still applies to the running version.
    let mut fetched = below_the_floor();
    fetched.facts.as_mut().unwrap().pending = Some(pending("0.12.0", false, NOW));
    adapter.emit(MachineUpdatesEvent::Changed(Box::new(fetched.clone())));
    cx.run_until_parked();
    assert_eq!(step_now(cx), None, "no failure shows");
    let state = UpdateState::UpToDate;
    let (view, _) = store_view(cx, &app(&state));
    assert!(matches!(
        &view.machine(PENGUIN).unwrap().stage,
        MachineStage::Downloaded { version: v, .. } if *v == version("0.12.0")
    ));
    let row = view.machine_row(PENGUIN).unwrap();
    assert_eq!(row.action.unwrap().label.as_ref(), "Restart Core\u{2026}");

    // It restarted into the Stepping Stone, and the embedded build is offered.
    let mut restarted = below_the_floor();
    restarted.facts.as_mut().unwrap().running = version("0.12.0");
    restarted.bundled = Some(Bundled {
        version: version("0.14.0"),
        floor: None,
    });
    restarted.push = Some(version("0.14.0"));
    adapter.emit(MachineUpdatesEvent::Changed(Box::new(restarted)));
    cx.run_until_parked();
    let (view, _) = store_view(cx, &app(&state));
    assert_eq!(view.step, None);
    assert_eq!(view.machine(PENGUIN).unwrap().stage, MachineStage::Running);
}

/// The multi-step feed chain keeps each downloaded Stepping Stone available to restart into,
/// even when it is below the embedded floor and follows a failed fetch.
#[gpui::test]
fn intermediate_stepping_stones_retire_fetch_failures_and_offer_each_restart(
    cx: &mut gpui::TestAppContext,
) {
    let (adapter, store) = installed_store(cx);
    let floor = Floor::parse("1.2.0").unwrap();
    let mut update = machine(PENGUIN, "penguin", false, Some(facts("1.0.0", 2)));
    update.bundled = Some(Bundled {
        version: version("1.2.2"),
        floor: Some(floor),
    });
    adapter.emit(MachineUpdatesEvent::Machines(vec![update.clone()]));
    cx.run_until_parked();

    for next in ["1.1.0", "1.2.0"] {
        store.update(cx, |store, cx| {
            store.perform(PENGUIN, MachineAction::Install, Surface::Window, cx)
        });
        adapter.emit(MachineUpdatesEvent::Finished {
            machine: PENGUIN,
            action: MachineAction::Install,
            outcome: Err(ActionFailure::NeedsFloor(floor)),
        });
        cx.run_until_parked();
        assert_eq!(step_now(cx), Some(Step::Failed(Target::Machine(PENGUIN))));

        // An empty check still leaves no next step and must keep the failed fetch.
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(update.clone())));
        cx.run_until_parked();
        assert_eq!(step_now(cx), Some(Step::Failed(Target::Machine(PENGUIN))));

        update.facts.as_mut().unwrap().pending = Some(pending(next, false, NOW));
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(update.clone())));
        cx.run_until_parked();
        assert!(store.read_with(cx, |store, _| store.machines()[0].failed_push.is_none()));
        assert_eq!(step_now(cx), None);
        let (view, _) = store_view(cx, &app(&UpdateState::UpToDate));
        assert!(matches!(
            &view.machine(PENGUIN).unwrap().stage,
            MachineStage::Downloaded { version: v, .. } if *v == version(next)
        ));
        assert_eq!(
            view.machine_row(PENGUIN).unwrap().action.unwrap().command,
            Command::Machine(PENGUIN, MachineAction::Restart)
        );

        let facts = update.facts.as_mut().unwrap();
        facts.running = version(next);
        facts.pending = None;
        facts.revision += 1;
        if next == "1.2.0" {
            update.push = Some(version("1.2.2"));
        }
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(update.clone())));
        cx.run_until_parked();
    }
}

/// A Core that reports it restarts after an hour with nothing running keeps that explanation
/// and adds how long the update waited from Ask.
#[test]
fn a_reported_continuation_takes_precedence_and_keeps_the_waiting_age() {
    let state = UpdateState::UpToDate;
    let mut update = waiting(LOCAL, "This Mac", 2, false, NOW - 3 * DAY);
    update.detailed = Some(DetailedUpdateFacts {
        facts: update.facts.clone().unwrap(),
        continuation: Some(Continuation::WhenNothingRuns),
        working: 0,
        restart: None,
    });
    let row = Desk::new()
        .view(app(&state), &[update], NOW)
        .machine_rows()
        .remove(0);
    assert_eq!(
        row.status.as_deref(),
        Some("Restarts into 0.11.4 after an hour with nothing running")
    );
    assert_eq!(
        row.description.as_deref(),
        Some(
            "Nothing running means no program, no typing and no output in any Terminal Session. An update has waited 3 days."
        ),
        "a Pane on screen keeps nothing running"
    );
}

/// A confirmation describes the Core's fresh preview, not cached facts; an unobserved target
/// asks even with nothing open; and the approval covers exactly the described work.
#[test]
fn a_confirmation_follows_the_fresh_preview_and_its_approval_the_described_work() {
    use crate::updates::confirm::previewed;
    let mut cached = waiting(LOCAL, "This Mac", 0, false, NOW);
    cached.facts.as_mut().unwrap().running = version("0.11.2");
    let fresh = approved_restart();
    let confirmation = previewed(
        std::slice::from_ref(&cached),
        std::slice::from_ref(&fresh),
        false,
        NOW,
    )
    .unwrap();
    assert_eq!(
        confirmation.message.as_ref(),
        "SpaceTerm Core restarts from 0.11.3 to 0.11.4. This ends 2 Terminal Sessions. Workspaces, Tabs and Panes stay."
    );
    assert!(
        !confirmation.approval.holds(std::slice::from_ref(&cached)),
        "the cached facts differ from what the alert described"
    );

    let observed = waiting(LOCAL, "This Mac", 0, false, NOW);
    let nothing_open = MachinePreview {
        sessions: 0,
        ..fresh.clone()
    };
    assert_eq!(
        previewed(
            std::slice::from_ref(&observed),
            std::slice::from_ref(&nothing_open),
            false,
            NOW
        ),
        None,
        "nothing ends and nothing breaks: no question"
    );
    let unobserved = machine(LOCAL, "This Mac", true, Some(facts("0.11.3", 0)));
    let asked = previewed(
        std::slice::from_ref(&unobserved),
        std::slice::from_ref(&nothing_open),
        false,
        NOW,
    )
    .expect("an unobserved target may be breaking, so it asks");
    assert!(!asked.approval.holds(std::slice::from_ref(&unobserved)));

    let two = waiting(LOCAL, "This Mac", 2, false, NOW);
    let approval = previewed(
        std::slice::from_ref(&two),
        std::slice::from_ref(&fresh),
        false,
        NOW,
    )
    .unwrap()
    .approval;
    let mut unrelated = two.clone();
    unrelated.facts.as_mut().unwrap().revision = 9;
    unrelated.name = "Renamed".into();
    assert!(
        approval.holds(&[unrelated]),
        "other changes keep the approval"
    );
    let mut retargeted = two.clone();
    retargeted
        .facts
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .version = version("0.11.5");
    assert!(!approval.holds(&[retargeted]));
    assert!(!approval.holds(&[]), "a removed Machine ends the approval");
    assert!(!approval.still_approves(&MachinePreview {
        target: None,
        ..fresh.clone()
    }));
    assert!(approval.retargets(&MachinePreview {
        target: Some(version("0.11.5")),
        ..fresh
    }));
}

/// Through the store: a restart that ends sessions asks, and approving executes exactly the
/// previewed work. Cancel records a deferral only at the Ask stage: Cancel at Offer leaves the
/// first Ask step to show, and an Insist step never hides (L3).
#[gpui::test]
fn a_requested_restart_asks_then_runs_or_defers_only_at_ask(cx: &mut gpui::TestAppContext) {
    use crate::updates::machines::store::{Ask, request};
    let now = crate::updates::now();
    let cases = [
        (true, now - 3 * DAY, false),
        (false, now - ladder::ASK_AFTER + 60, false),
        (false, now - 3 * DAY, true),
        (false, now - 8 * DAY, false),
    ];
    for (approve, installed_at, defers) in cases {
        let (adapter, store) = installed_store(cx);
        adapter.emit(MachineUpdatesEvent::Machines(vec![waiting(
            LOCAL,
            "This Mac",
            2,
            false,
            installed_at,
        )]));
        cx.run_until_parked();
        let asked = Rc::new(std::cell::Cell::new(0));
        let counted = asked.clone();
        let ask: Ask = Rc::new(move |confirmation, answered, cx| {
            assert_eq!(
                confirmation.title.as_ref(),
                "Restart SpaceTerm Core on This Mac?"
            );
            counted.set(counted.get() + 1);
            answered(approve, cx);
        });
        cx.update(|cx| {
            request(
                vec![(LOCAL, MachineAction::Restart)],
                false,
                Surface::Window,
                ask,
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(asked.get(), 1);
        assert_eq!(
            adapter.executed_actions(),
            if approve {
                vec![(LOCAL, MachineAction::Restart)]
            } else {
                Vec::new()
            }
        );
        assert_eq!(
            store.read_with(cx, |store, _| store.deferred().contains_key(&LOCAL)),
            defers,
            "installed {} seconds ago",
            now - installed_at
        );
    }
}
