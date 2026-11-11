//! Invariants I1 to I7 and the ladder rules L1 to L3 of design section 11, over generated
//! inputs, and the replays R1 and R5.
use std::rc::Rc;

use spaceterm_core::protocol::capability::{Capabilities, Capability};
use spaceterm_core::update::facts::{
    PendingVersion, Platform, Protocols, StrandedDevice, UpdateCheck, UpdateFacts, UpdateFailure,
};

use super::*;
use crate::updates::copy::title_bar::LABELS;
use crate::updates::machines::store::Surface;
use crate::updates::machines::testing::ScriptedMachines;
use crate::updates::machines::{FailedPush, MachinePreview, MachineUpdatesEvent};

const NOW: u64 = 100 * DAY;
const LOCAL: MachineId = [1; 32];
const PENGUIN: MachineId = [2; 32];
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

fn machine(id: MachineId, name: &str, local: bool, facts: Option<UpdateFacts>) -> MachineUpdate {
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
fn waiting(id: MachineId, name: &str, sessions: u32, breaking: bool, installed_at: u64) -> MachineUpdate {
    let mut facts = facts("0.11.3", sessions);
    facts.pending = Some(pending("0.11.4", breaking, installed_at));
    machine(id, name, id == LOCAL, Some(facts))
}

fn app(state: &UpdateState) -> AppFacts<'_> {
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
fn app_states() -> Vec<(UpdateState, Option<&'static str>, bool, bool, bool)> {
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
fn machine_variants(id: MachineId, name: &str) -> Vec<MachineUpdate> {
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
        for asked in [None, Some(Asked::Restart), Some(Asked::Install), Some(Asked::Check)] {
            let mut restarting = base.clone();
            restarting.asked = asked;
            restarting.facts.as_mut().unwrap().operation =
                Some(Operation::Restarting { to: version("0.11.5") });
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
        beside.facts.as_mut().unwrap().last_failure = feed.facts.as_ref().unwrap().last_failure.clone();
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
        view.machine_rows().into_iter().map(|row| row.name).collect()
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
        let mut actions: Vec<RowAction> = view.machine_rows().into_iter().filter_map(|row| row.action).collect();
        actions.extend(view.app_row().action);
        for action in actions {
            let label = action.label.as_ref();
            let asks = matches!(
                action.command,
                Command::Machine(_, MachineAction::Restart) | Command::Explain(_)
            );
            assert_eq!(label.ends_with('\u{2026}'), asks, "{label}: {:?}", action.command);
            match label {
                "Download" => assert!(matches!(
                    action.command,
                    Command::Desktop(DesktopAction::Download) | Command::Machine(_, MachineAction::Install)
                )),
                "Relaunch to Update" => {
                    assert_eq!(action.command, Command::Desktop(DesktopAction::Update))
                }
                "Restart Core\u{2026}" | "Try Again\u{2026}" => {
                    assert!(matches!(action.command, Command::Machine(_, MachineAction::Restart)))
                }
                "What's New" => assert_eq!(action.command, Command::Desktop(DesktopAction::WhatsNew)),
                _ => {}
            }
        }
        if let Some(pill) = view.pill() {
            match pill.label.as_ref() {
                "Download Update" => assert_eq!(pill.main, Command::Desktop(DesktopAction::Download)),
                "Relaunch to Update" => assert_eq!(pill.main, Command::Desktop(DesktopAction::Update)),
                "Restart Core\u{2026}" => {
                    assert!(matches!(pill.main, Command::Machine(_, MachineAction::Restart)))
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
            for banned in ["installed", "ready", "idle", "Update This", "Update penguin", "\u{2019}"] {
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

/// L1: presence, emphasis and cadence follow the stage only, and no label or row status
/// changes text between stages.
#[test]
fn l1_a_stage_changes_emphasis_never_words() {
    let desk = Desk::new();
    let state = UpdateState::UpToDate;
    for sessions in [1, 2] {
        let at = |days: u64| {
            desk.view(app(&state), &[waiting(LOCAL, "This Mac", sessions, false, NOW - days * DAY)], NOW)
        };
        let offer = at(1);
        assert_eq!(offer.step, None, "a Core that applies itself is list only at Offer");
        let (ask, insist) = (at(3), at(8));
        let (ask_pill, insist_pill) = (ask.pill().unwrap(), insist.pill().unwrap());
        assert_eq!(ask_pill.label, insist_pill.label);
        assert_eq!(ask_pill.emphasis, Emphasis::Accent);
        assert_eq!(insist_pill.emphasis, Emphasis::Warning);
        assert_eq!(offer.machine_rows()[0].status, ask.machine_rows()[0].status);
        assert_eq!(ask.machine_rows()[0].status, insist.machine_rows()[0].status);
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
    replaced.facts.as_mut().unwrap().pending.as_mut().unwrap().version = version("0.11.5");
    let stage = |update: &MachineUpdate| desk.view(app(&state), std::slice::from_ref(update), NOW).machines[0].ladder;
    assert_eq!(stage(&old), Some(Stage::Ask));
    assert_eq!(stage(&replaced), Some(Stage::Ask), "the newer version inherits the clock");
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
    let view = |update: MachineUpdate, desk: &Desk, now| desk.view(app(&state), &[update], now).step;
    let ask = waiting(LOCAL, "This Mac", 2, false, NOW - 3 * DAY);
    assert_eq!(view(ask.clone(), &desk, NOW), None);
    assert!(view(ask, &desk, NOW + 1).is_some(), "24 hours after Cancel it asks again");
    let insist = waiting(LOCAL, "This Mac", 2, false, NOW - 8 * DAY);
    assert!(view(insist, &desk, NOW).is_some());
    let mut blocked = waiting(LOCAL, "This Mac", 2, false, NOW - 3 * DAY);
    blocked.link = MachineLink::Incompatible(Incompatible::ListenerTooOld);
    assert!(matches!(
        view(blocked, &desk, NOW),
        Some(Step::Act { hard_stop: true, stage: None, .. })
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
        desk.view(facts, &[insisting.clone()], NOW).step,
        Some(Step::Act {
            kind: ActKind::RestartCore(PENGUIN),
            stage: Some(Stage::Insist),
            hard_stop: false
        }),
        "Insist outranks Ask"
    );
    facts.ladder = Stage::Insist;
    assert_eq!(
        desk.view(facts, &[insisting.clone()], NOW).step,
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
    assert_eq!(pill.tooltip.as_ref(), "penguin is out of disk space.");
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
    assert_eq!(row.status.as_deref(), Some("Restarts into 0.11.4 when 2 Terminal Sessions end"));
    let action = row.action.clone().unwrap();
    assert_eq!(action.label.as_ref(), "Restart Core\u{2026}");
    assert_eq!(action.subtitle.as_deref(), Some("Ends 2 Terminal Sessions"));
    assert_eq!(
        row.accessibility.as_ref(),
        "SpaceTerm Core on This Mac restarts into 0.11.4 when its 2 Terminal Sessions end. Restart Core ends 2 Terminal Sessions now."
    );

    this_mac.facts.as_mut().unwrap().pending.as_mut().unwrap().installed_at = NOW - 2 * DAY;
    let view = desk.view(app(&state), &[this_mac.clone()], NOW);
    let pill = view.pill().unwrap();
    assert_eq!(pill.label.as_ref(), "Restart Core\u{2026}");
    assert_eq!(pill.glyph, Glyph::UpdateCore);
    assert_eq!(
        pill.tooltip.as_ref(),
        "An update has waited 2 days on This Mac. Restarting into 0.11.4 ends 2 Terminal Sessions."
    );
    assert_eq!(view.machine_rows()[0].description.as_deref(), Some("An update has waited 2 days."));

    let preview = MachinePreview {
        machine: LOCAL,
        action: MachineAction::Restart,
        sessions: 2,
        running: Some(version("0.11.3")),
        target: Some(version("0.11.4")),
        id: 1,
    };
    let confirmation = super::super::confirm::previewed(&[this_mac.clone()], &[preview.clone()], false).unwrap();
    assert_eq!(confirmation.title.as_ref(), "Restart SpaceTerm Core on This Mac?");
    assert_eq!(
        confirmation.message.as_ref(),
        "SpaceTerm Core restarts from 0.11.3 to 0.11.4. This ends 2 Terminal Sessions. Workspaces, Tabs and Panes stay."
    );
    assert_eq!(confirmation.button.as_ref(), "Restart Core");
    assert!(confirmation.approval.holds(&[this_mac.clone()]));

    let mut breaking = this_mac.clone();
    let pending = breaking.facts.as_mut().unwrap().pending.as_mut().unwrap();
    pending.breaking = true;
    pending.stranded = vec![StrandedDevice {
        pairing: Default::default(),
        name: "iPad".into(),
    }];
    let none = MachinePreview { sessions: 0, ..preview };
    let confirmation = super::super::confirm::previewed(&[breaking], &[none], false).unwrap();
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
    restarting.facts.as_mut().unwrap().operation = Some(Operation::Restarting { to: version("0.11.4") });
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
    restarting.facts.as_mut().unwrap().operation = Some(Operation::Restarting { to: version("0.11.4") });
    let mut back = machine(LOCAL, "This Mac", true, Some(facts_running("0.11.4")));
    back.facts.as_mut().unwrap().revision = 2;

    adapter.emit(MachineUpdatesEvent::Machines(vec![this_mac.clone()]));
    adapter.emit(MachineUpdatesEvent::Changed(Box::new(restarting.clone())));
    cx.run_until_parked();
    assert_eq!(store_view(cx, &facts).0.step, None, "another device's restart");

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
        (Some(Step::Restarting(LOCAL)), Some("Restarting Core\u{2026}".into())),
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
        (Some(Step::Restarting(LOCAL)), Some("Restarting Core\u{2026}".into()))
    );
    let pill = store_view(cx, &facts).0.pill().unwrap();
    assert_eq!(pill.tooltip.as_ref(), "Restarting SpaceTerm Core on This Mac into 0.11.4.");
    assert_eq!(pill.main, Command::ShowList);

    adapter.emit(MachineUpdatesEvent::Changed(Box::new(back)));
    cx.run_until_parked();
    assert_eq!(shown(cx), (None, None), "the pill hides once the Core is back");
}
