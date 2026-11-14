//! The split pill and its list, driven through a GPUI window. The update state is scripted, so
//! each test states exactly what the state shows and then what changes.

use super::*;
use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::*;
use spaceterm_ui::a11y_testing::A11yTree;

use crate::ui::updates::control::{Shown, ViewSource};
use crate::updates::UpdateEvent;
use crate::updates::confirm::Explanation;
use crate::updates::ladder::Emphasis;
use crate::updates::machines::store::{MachineUpdatesNotice, Surface};
use crate::updates::machines::testing::ScriptedMachines;
use crate::updates::machines::{
    ActionFailure, MachineAction, MachineId, MachineUpdate, MachineUpdatesEvent,
};
use crate::updates::state::{
    Command, DesktopAction, Glyph, PillView, Primary, PrimaryKind, RowAction, RowView,
};

const THIS_MAC: MachineId = [1; 32];
const PENGUIN: MachineId = [2; 32];
const BUILD_BOX: MachineId = [3; 32];

const CONFIRM: &str = "modal-action-update-confirm-proceed";
const CANCEL: &str = "modal-action-update-confirm-cancel";

/// A scripted row: with an action, the list shows the action; otherwise it reports `status`.
fn row(name: &str, status: &str, action: Option<(&str, Command)>) -> RowView {
    let action = action.map(|(label, command)| RowAction {
        label: label.to_owned().into(),
        subtitle: None,
        command,
    });
    let primary = match &action {
        Some(action) => Primary {
            kind: PrimaryKind::Action,
            label: action.label.clone(),
            command: Some(action.command.clone()),
            description: format!("{name}: {status}").into(),
        },
        None => Primary {
            kind: PrimaryKind::Status,
            label: status.to_owned().into(),
            command: None,
            description: format!("{name}: {status}").into(),
        },
    };
    RowView {
        name: name.to_owned().into(),
        version: None,
        target: None,
        status: Some(status.to_owned().into()),
        action,
        primary,
        details: Vec::new(),
        accessibility: format!("{name}: {status}").into(),
        tone: Default::default(),
    }
}

fn pill(label: &str, glyph: Glyph, emphasis: Emphasis, main: Command) -> PillView {
    PillView {
        label: label.to_owned().into(),
        glyph,
        emphasis,
        tooltip: format!("{label} tooltip").into(),
        main,
        progress: None,
    }
}

fn whats_new() -> PillView {
    pill(
        "What\u{2019}s New",
        Glyph::WhatsNew,
        Emphasis::Plain,
        Command::Desktop(DesktopAction::WhatsNew),
    )
}

fn restarting() -> PillView {
    pill(
        "Restarting Core…",
        Glyph::Working,
        Emphasis::Plain,
        Command::ShowList,
    )
}

fn restart_core(machine: MachineId) -> Command {
    Command::Machine(machine, MachineAction::Restart)
}

/// The recording's list after the relaunch: penguin restarts by itself while This Mac waits.
fn recording() -> Shown {
    Shown {
        pill: Some(whats_new()),
        app: row(
            "SpaceTerm",
            "0.11.4",
            Some((
                "What\u{2019}s New",
                Command::Desktop(DesktopAction::WhatsNew),
            )),
        ),
        machines: vec![
            (
                THIS_MAC,
                row(
                    "This Mac",
                    "Restarts into 0.11.4 when 2 Terminal Sessions end",
                    Some(("Restart Core…", restart_core(THIS_MAC))),
                ),
            ),
            (PENGUIN, row("penguin", "Restarting into 0.11.4…", None)),
        ],
    }
}

fn with_pill(pill: PillView) -> Shown {
    Shown {
        pill: Some(pill),
        ..recording()
    }
}

/// The update state the control reads, scripted by the test.
#[derive(Clone)]
struct Script(Rc<RefCell<Shown>>);

impl Script {
    fn new(shown: Shown) -> Self {
        Self(Rc::new(RefCell::new(shown)))
    }

    fn source(&self) -> ViewSource {
        let shown = self.0.clone();
        Rc::new(move |_| shown.borrow().clone())
    }

    /// Changes what the state shows, as a background event would, and lets the control redraw.
    fn set(
        &self,
        change: impl FnOnce(&mut Shown),
        control: &Entity<UpdateControl>,
        cx: &mut gpui::VisualTestContext,
    ) {
        change(&mut self.0.borrow_mut());
        control.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
    }
}

/// A window whose root holds the title-bar control under a modal layer, as a Workspace Window
/// does, so the control's own alerts present.
struct Host {
    control: Entity<UpdateControl>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // The title bar holds the control at its trailing edge.
        spaceterm_ui::ModalLayer::new(
            gpui::div()
                .size_full()
                .flex()
                .justify_end()
                .items_start()
                .child(self.control.clone()),
        )
    }
}

fn hosted<'a>(
    script: &Script,
    cx: &'a mut gpui::TestAppContext,
) -> (Entity<UpdateControl>, &'a mut gpui::VisualTestContext) {
    let source = script.source();
    let (host, visual) = cx.add_window_view(move |window, cx| Host {
        control: cx.new(|cx| UpdateControl::with_source(source, window, cx)),
    });
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    let control = visual.update(|_, cx| host.read(cx).control.clone());
    (control, visual)
}

/// The scripted Machine updates and the update service with automatic downloads off.
fn services(
    cx: &mut gpui::TestAppContext,
) -> (
    Rc<crate::updates::testing::RecordingAdapter>,
    Entity<ApplicationUpdates>,
    Rc<ScriptedMachines>,
) {
    let machines = Rc::new(ScriptedMachines::default());
    cx.update(|cx| MachineUpdates::install(machines.clone(), cx));
    let (adapter, updates) = manual_updates(cx);
    (adapter, updates, machines)
}

fn click(selector: &'static str, cx: &mut gpui::VisualTestContext) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} renders"));
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    cx.run_until_parked();
}

fn shows(selector: &'static str, cx: &mut gpui::VisualTestContext) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// The list's entries from top to bottom, by their selectors.
fn list(cx: &mut gpui::VisualTestContext) -> Vec<&'static str> {
    let mut rows: Vec<(gpui::Pixels, &'static str)> = [
        "update-row-app",
        "update-row-0",
        "update-row-1",
        "update-row-2",
        "update-row-3",
        "update-settings",
    ]
    .into_iter()
    .filter_map(|selector| Some((cx.debug_bounds(selector)?.origin.y, selector)))
    .collect();
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("positions compare"));
    rows.into_iter().map(|(_, selector)| selector).collect()
}

fn description(name: &str, cx: &mut gpui::VisualTestContext) -> String {
    A11yTree::read(cx).node(name)["aria"]["description"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn shows_pill(control: &Entity<UpdateControl>, cx: &mut gpui::VisualTestContext) -> bool {
    cx.update(|_, cx| control.read(cx).shows_pill(cx))
}

#[gpui::test]
fn nothing_owed_hides_the_pill(cx: &mut gpui::TestAppContext) {
    let _ = services(cx);
    let script = Script::new(Shown {
        pill: None,
        ..recording()
    });
    let (control, visual) = hosted(&script, cx);
    assert!(!shows("update-pill", visual));
    assert!(!shows_pill(&control, visual));
}

/// The owner's complaint: the control changed shape by itself. Every visible state, progress
/// included, is one capsule with a main segment, a divider and a chevron segment, and the chevron
/// keeps its size and its place at the trailing edge.
#[gpui::test]
fn the_pill_keeps_two_segments_and_its_chevron_through_every_state(cx: &mut gpui::TestAppContext) {
    let _ = services(cx);
    let failed = Command::Explain(Explanation::ok(
        "Couldn\u{2019}t Restart SpaceTerm Core on penguin",
        "The restart into SpaceTerm Core 0.11.4 was interrupted, so it went back to 0.11.3.",
    ));
    let states = [
        pill(
            "Restart Core…",
            Glyph::UpdateCore,
            Emphasis::Accent,
            restart_core(THIS_MAC),
        ),
        restarting(),
        pill(
            "Update Failed",
            Glyph::UpdateFailed,
            Emphasis::Warning,
            failed,
        ),
        pill(
            "Downloading Update…",
            Glyph::Working,
            Emphasis::Plain,
            Command::ShowList,
        ),
        whats_new(),
    ];
    let script = Script::new(recording());
    let (control, visual) = hosted(&script, cx);
    let area = |cx: &mut gpui::VisualTestContext| {
        cx.debug_bounds("update-control-area")
            .expect("the control renders")
    };
    let mut chevrons = Vec::new();
    for state in states {
        let label = state.label.clone();
        script.set(|shown| shown.pill = Some(state), &control, visual);
        let main = visual
            .debug_bounds("update-pill")
            .unwrap_or_else(|| panic!("{label}: main segment"));
        let chevron = visual
            .debug_bounds("update-pill-menu")
            .unwrap_or_else(|| panic!("{label}: chevron segment"));
        assert!(
            shows("combo-button-divider", visual),
            "{label}: divider between the segments"
        );
        assert_eq!(
            main.size.height, chevron.size.height,
            "{label}: one capsule"
        );
        assert!(
            main.right() <= chevron.left() + gpui::px(1.0),
            "{label}: main, then chevron"
        );
        chevrons.push((label, chevron.size, area(visual).right() - chevron.right()));
        let tree = A11yTree::read(visual);
        tree.node(&chevrons.last().unwrap().0);
        tree.node("Updates");
    }
    let (_, size, inset) = chevrons[0].clone();
    for (label, other_size, other_inset) in &chevrons {
        assert_eq!(*other_size, size, "{label}: the chevron keeps its size");
        assert_eq!(*other_inset, inset, "{label}: the chevron keeps its place");
    }
    script.set(|shown| shown.pill = None, &control, visual);
    assert!(
        !shows("update-pill", visual),
        "settled: hidden, never half a pill"
    );
    assert!(!shows("update-pill-menu", visual));
}

#[gpui::test]
fn the_chevron_opens_spaceterm_then_every_machine_then_update_settings(
    cx: &mut gpui::TestAppContext,
) {
    let (_, _, machines) = services(cx);
    let script = Script::new(recording());
    let (_, visual) = hosted(&script, cx);
    click("update-pill-menu", visual);
    assert_eq!(
        list(visual),
        [
            "update-row-app",
            "update-row-0",
            "update-row-1",
            "update-settings"
        ]
    );
    let tree = A11yTree::read(visual);
    tree.node("Update Settings…");
    assert_eq!(
        tree.node("penguin")["aria"]["role"],
        "Label",
        "a row without an action is information"
    );
    assert!(
        machines.executed_actions().is_empty(),
        "opening performs nothing"
    );
    assert!(machines.performed().is_empty());
}

/// While open, the list grows to show a row whose text grows, so no action is cut off, and never
/// shrinks under the pointer when a row's text shrinks. It fits its rows again when it next opens.
/// The text width of `text` in the list's font.
fn text_width(text: &str, visual: &mut gpui::VisualTestContext) -> gpui::Pixels {
    visual.update(|window, _| {
        let font = spaceterm_ui::ControlTypography::default().regular().clone();
        let run = gpui::TextRun {
            len: text.len(),
            font,
            color: window.text_style().color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window
            .text_system()
            .shape_line(text.to_owned().into(), gpui::px(12.0), &[run], None)
            .width
    })
}

fn bounds(
    selector: &'static str,
    visual: &mut gpui::VisualTestContext,
) -> gpui::Bounds<gpui::Pixels> {
    visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} renders"))
}

/// The list keeps its native width while a row's state changes, and every row stays one line:
/// a longer state never widens the open list or the next one.
#[gpui::test]
fn the_list_keeps_its_width_and_one_line_rows_while_states_change(cx: &mut gpui::TestAppContext) {
    let _ = services(cx);
    let script = Script::new(recording());
    let (control, visual) = hosted(&script, cx);
    visual.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(580.0)));
    visual.run_until_parked();
    click("update-pill-menu", visual);
    let opened = bounds("update-row-app", visual);
    let rows = |visual: &mut gpui::VisualTestContext| {
        ["update-row-app", "update-row-0", "update-row-1"]
            .map(|selector| bounds(selector, visual).size.height)
    };
    let heights = rows(visual);
    script.set(
        |shown| {
            shown.machines[0].1 = row(
                "This Mac",
                "Couldn\u{2019}t restart into 0.11.4 after an hour with nothing running",
                None,
            );
        },
        &control,
        visual,
    );
    assert_eq!(
        bounds("update-row-app", visual).size.width,
        opened.size.width,
        "a longer state doesn't widen the open list"
    );
    assert_eq!(rows(visual), heights, "every row stays one line");
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    click("update-pill-menu", visual);
    assert_eq!(
        bounds("update-row-app", visual).size.width,
        opened.size.width,
        "the next opening keeps the native width"
    );
    assert_eq!(rows(visual), heights);
}

/// At the default 900 x 580 window every row shows its one presentation whole, a long Machine
/// name gives way to it, and assistive technology reads the full name and state.
#[gpui::test]
fn every_row_shows_its_presentation_whole_and_a_long_name_gives_way(cx: &mut gpui::TestAppContext) {
    const LONG: &str = "workstation-with-a-very-long-machine-name-0123456789";
    let _ = services(cx);
    let script = Script::new(Shown {
        pill: Some(pill(
            "Relaunch to Update",
            Glyph::Restart,
            Emphasis::Accent,
            Command::Desktop(DesktopAction::Update),
        )),
        app: row(
            "SpaceTerm",
            "0.11.4",
            Some((
                "Relaunch to Update",
                Command::Desktop(DesktopAction::Update),
            )),
        ),
        machines: vec![
            (
                THIS_MAC,
                row(
                    "This Mac",
                    "0.11.4",
                    Some(("Restart Core\u{2026}", restart_core(THIS_MAC))),
                ),
            ),
            (PENGUIN, row("penguin", "Restart pending", None)),
            (
                BUILD_BOX,
                row(
                    LONG,
                    "0.11.4",
                    Some(("Restart Core\u{2026}", restart_core(BUILD_BOX))),
                ),
            ),
        ],
    });
    let (_, visual) = hosted(&script, cx);
    visual.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(580.0)));
    visual.run_until_parked();
    click("update-pill-menu", visual);
    for (detail, shown) in [
        ("update-row-app-detail", "Relaunch to Update"),
        ("update-row-0-detail", "Restart Core\u{2026}"),
        ("update-row-1-detail", "Restart pending"),
        ("update-row-2-detail", "Restart Core\u{2026}"),
    ] {
        let needed = text_width(shown, visual);
        let detail = bounds(detail, visual);
        assert!(
            detail.size.width >= needed,
            "{shown} shows whole: {:?} < {needed:?}",
            detail.size.width
        );
        assert!(
            detail.right() <= gpui::px(900.0),
            "{shown} stays inside the window"
        );
    }
    let name = bounds("update-row-2-label", visual);
    assert!(
        name.size.width < text_width(LONG, visual),
        "the long name gives way"
    );
    let heights: Vec<_> = ["update-row-0", "update-row-1", "update-row-2"]
        .into_iter()
        .map(|selector| bounds(selector, visual).size.height)
        .collect();
    assert!(
        heights.windows(2).all(|pair| pair[0] == pair[1]),
        "{heights:?}"
    );
    let tree = A11yTree::read(visual);
    tree.node(LONG);
    assert_eq!(description(LONG, visual), format!("{LONG}: 0.11.4"));
    tree.node("SpaceTerm Core");
}

/// The owner's complaint: penguin's restart ended while the menu was open, and the menu turned
/// into a button. Now the list keeps its rows and the pill keeps its shape until the list closes.
#[gpui::test]
fn while_the_list_is_open_only_text_changes_and_the_pill_waits_for_it_to_close(
    cx: &mut gpui::TestAppContext,
) {
    let _ = services(cx);
    let script = Script::new(recording());
    let (control, visual) = hosted(&script, cx);
    click("update-pill-menu", visual);
    let open = list(visual);

    // penguin finishes, build-box appears, and the notes are read elsewhere: no step is owed.
    script.set(
        |shown| {
            shown.machines[1] = (PENGUIN, row("penguin", "0.11.4", None));
            shown
                .machines
                .push((BUILD_BOX, row("build-box", "Offline", None)));
            shown.pill = None;
        },
        &control,
        visual,
    );
    assert_eq!(list(visual), open, "the rows keep their number and order");
    assert_eq!(
        description("penguin", visual),
        "penguin: 0.11.4",
        "the row\u{2019}s text changes in place"
    );
    assert!(A11yTree::read(visual).find("build-box").is_none());
    assert!(
        shows("update-pill", visual),
        "the pill stays while the list is open"
    );
    assert!(shows("update-pill-menu", visual), "with its chevron");
    assert!(shows_pill(&control, visual));

    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(!shows("update-row-app", visual), "the list closed");
    assert!(!shows("update-pill", visual), "then the pill hides");
    assert!(!shows_pill(&control, visual));
}

/// The descriptions of the list's Machine rows, top to bottom.
fn machine_descriptions(cx: &mut gpui::VisualTestContext) -> Vec<String> {
    A11yTree::read(cx)
        .in_order()
        .into_iter()
        .filter_map(|node| node["aria"]["description"].as_str())
        .filter(|description| description.starts_with("penguin"))
        .map(str::to_owned)
        .collect()
}

fn check_again(machine: MachineId) -> Command {
    Command::Machine(machine, MachineAction::CheckAgain)
}

/// Rows belong to Machines, not names. Two Machines may share a name, a name may change, and a
/// Machine that leaves while another arrives keeps the count without changing whose row is whose.
#[gpui::test]
fn the_open_list_keeps_each_row_with_its_machine(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    let line = |id, name, status| {
        (
            id,
            row(name, status, Some(("Check Again", check_again(id)))),
        )
    };
    let script = Script::new(Shown {
        machines: vec![
            line(THIS_MAC, "penguin", "first"),
            line(PENGUIN, "penguin", "second"),
        ],
        ..recording()
    });
    let (control, visual) = hosted(&script, cx);
    click("update-pill-menu", visual);
    assert_eq!(
        machine_descriptions(visual),
        ["penguin: first", "penguin: second"]
    );

    // Both share a name and both change: each row takes its own Machine's text.
    script.set(
        |shown| {
            shown.machines = vec![
                line(PENGUIN, "penguin", "second, later"),
                line(THIS_MAC, "penguin", "first, later"),
            ];
        },
        &control,
        visual,
    );
    assert_eq!(
        machine_descriptions(visual),
        ["penguin: first, later", "penguin: second, later"],
        "each row keeps its Machine and its place"
    );

    // The second is renamed; the first leaves while build-box arrives, so the count holds.
    script.set(
        |shown| {
            shown.machines = vec![
                line(PENGUIN, "penguin-2", "renamed"),
                line(BUILD_BOX, "penguin-3", "arrived"),
            ];
        },
        &control,
        visual,
    );
    assert_eq!(
        machine_descriptions(visual),
        ["penguin: first, later", "penguin-2: renamed"],
        "a Machine that left keeps its last row; one that arrived waits"
    );
    click("update-row-0", visual);
    assert_eq!(
        machines.performed(),
        [(THIS_MAC, MachineAction::CheckAgain)],
        "the first row still targets its own Machine"
    );

    click("update-pill-menu", visual);
    assert_eq!(
        machine_descriptions(visual),
        ["penguin-2: renamed", "penguin-3: arrived"],
        "the next opening shows the current Machines"
    );
    click("update-row-1", visual);
    assert_eq!(
        machines.performed(),
        [
            (THIS_MAC, MachineAction::CheckAgain),
            (BUILD_BOX, MachineAction::CheckAgain)
        ]
    );
}

#[gpui::test]
fn a_new_step_while_the_list_is_open_keeps_the_list_open(cx: &mut gpui::TestAppContext) {
    let _ = services(cx);
    let script = Script::new(with_pill(restarting()));
    let (control, visual) = hosted(&script, cx);
    click("update-pill", visual);
    let open = list(visual);
    assert!(!open.is_empty(), "the main area opened the list");

    script.set(|shown| shown.pill = Some(whats_new()), &control, visual);
    assert_eq!(list(visual), open, "the list stays open with its rows");

    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    A11yTree::read(visual).node("What\u{2019}s New");
    assert!(shows("update-pill-menu", visual));
}

#[gpui::test]
fn the_main_area_performs_the_step_and_a_row_performs_its_action(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    let script = Script::new(with_pill(pill(
        "Check Again",
        Glyph::UpdateFailed,
        Emphasis::Warning,
        Command::Machine(BUILD_BOX, MachineAction::CheckAgain),
    )));
    script.0.borrow_mut().machines[1] = (
        PENGUIN,
        row(
            "penguin",
            "Can\u{2019}t check for updates",
            Some((
                "Check Again",
                Command::Machine(PENGUIN, MachineAction::CheckAgain),
            )),
        ),
    );
    let (_, visual) = hosted(&script, cx);
    click("update-pill", visual);
    assert_eq!(
        machines.performed(),
        [(BUILD_BOX, MachineAction::CheckAgain)]
    );
    assert!(
        !shows("update-row-app", visual),
        "the main area opens no list"
    );

    click("update-pill-menu", visual);
    click("update-row-1", visual);
    assert_eq!(
        machines.performed(),
        [
            (BUILD_BOX, MachineAction::CheckAgain),
            (PENGUIN, MachineAction::CheckAgain)
        ]
    );
}

#[gpui::test]
fn progress_opens_the_list_from_the_main_area(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    let script = Script::new(with_pill(restarting()));
    let (_, visual) = hosted(&script, cx);
    click("update-pill", visual);
    assert!(shows("update-row-app", visual), "the list opens");
    assert!(machines.performed().is_empty(), "nothing runs");
}

#[gpui::test]
fn a_failure_opens_its_alert_and_cancel_dismisses_its_step(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    let failed = Command::Explain(Explanation {
        title: "Couldn\u{2019}t Restart SpaceTerm Core on penguin".into(),
        message:
            "SpaceTerm Core 0.11.4 couldn\u{2019}t start because penguin is out of disk space, so \
                  it went back to 0.11.3. Free some space, then try again."
                .into(),
        then: Some(Box::new(restart_core(PENGUIN))),
        proceed: Some("Try Again".into()),
        dismiss: Some(Box::new(Command::Machine(
            PENGUIN,
            MachineAction::DismissFailure(7),
        ))),
    });
    let script = Script::new(with_pill(pill(
        "Update Failed",
        Glyph::UpdateFailed,
        Emphasis::Warning,
        failed,
    )));
    let (_, visual) = hosted(&script, cx);
    click("update-pill", visual);
    let tree = A11yTree::read(visual);
    tree.node("Try Again");
    tree.node("Cancel");
    assert!(
        machines.performed().is_empty(),
        "an open alert performs nothing"
    );

    click("modal-action-update-explain-cancel", visual);
    assert_eq!(
        machines.performed(),
        [(PENGUIN, MachineAction::DismissFailure(7))]
    );
    assert!(machines.asked().is_empty(), "Cancel restarts nothing");
}

#[gpui::test]
fn relaunch_to_update_relaunches_at_once_and_download_never_relaunches(
    cx: &mut gpui::TestAppContext,
) {
    let (adapter, updates, _) = services(cx);
    let script = Script::new(with_pill(pill(
        "Download Update",
        Glyph::Download,
        Emphasis::Accent,
        Command::Desktop(DesktopAction::Download),
    )));
    let (control, visual) = hosted(&script, cx);
    updates.update(visual, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available(version()));
    visual.run_until_parked();

    click("update-pill", visual);
    assert_eq!(adapter.downloads.get(), 1, "Download downloads");
    adapter.emit(UpdateEvent::Ready);
    visual.run_until_parked();
    assert_eq!(adapter.installations.get(), 0, "Download never relaunches");

    script.set(
        |shown| {
            shown.pill = Some(pill(
                "Relaunch to Update",
                Glyph::Restart,
                Emphasis::Accent,
                Command::Desktop(DesktopAction::Update),
            ));
        },
        &control,
        visual,
    );
    click("update-pill", visual);
    assert_eq!(
        adapter.installations.get(),
        1,
        "Relaunch to Update never asks"
    );
    assert!(!shows(CANCEL, visual));
}

/// Restart Core asks on the Core's own preview. Return never confirms it, and Cancel restarts
/// nothing.
#[gpui::test]
fn restart_core_asks_and_only_its_destructive_button_restarts(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    machines.emit(MachineUpdatesEvent::Machines(vec![waiting(PENGUIN, 3)]));
    let script = Script::new(with_pill(pill(
        "Restart Core…",
        Glyph::UpdateCore,
        Emphasis::Accent,
        restart_core(PENGUIN),
    )));
    let (_, visual) = hosted(&script, cx);
    click("update-pill", visual);
    assert!(
        shows(CONFIRM, visual),
        "the confirmation asks: asked {:?}",
        machines.asked()
    );
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert!(
        machines.executed_actions().is_empty(),
        "Return never restarts"
    );
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(!shows(CONFIRM, visual), "Escape cancels");
    assert!(
        machines.executed_actions().is_empty(),
        "Cancel restarts nothing"
    );

    click("update-pill", visual);
    click(CONFIRM, visual);
    assert_eq!(
        machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)]
    );
}

fn notice(failure: ActionFailure, refused_at_once: bool) -> MachineUpdatesNotice {
    MachineUpdatesNotice {
        machine: PENGUIN,
        name: "penguin".into(),
        local: false,
        action: MachineAction::Restart,
        surface: Surface::Window,
        outcome: Err(failure),
        refused_at_once,
    }
}

/// D5: only a restart the Core refused at its preview explains itself at once. A restart that
/// already ran reports through the title bar, whatever its failure.
#[test]
fn only_a_restart_refused_at_its_preview_has_an_alert() {
    let unreachable = commands::refusal(&notice(ActionFailure::Unreachable, true))
        .expect("an unreachable Machine explains itself");
    assert_eq!(
        unreachable.title,
        "Couldn\u{2019}t Restart SpaceTerm Core on penguin"
    );
    assert_eq!(
        unreachable.message,
        "SpaceTerm can\u{2019}t reach penguin. Try again when it\u{2019}s connected."
    );
    let busy =
        commands::refusal(&notice(ActionFailure::Busy, true)).expect("a busy Core explains itself");
    assert_eq!(
        busy.message,
        "SpaceTerm Core on penguin is busy with another update. Try again when it finishes."
    );
    for failure in [
        ActionFailure::Unreachable,
        ActionFailure::Busy,
        ActionFailure::Failed,
    ] {
        assert_eq!(
            commands::refusal(&notice(failure, false)),
            None,
            "{failure:?} after the restart ran"
        );
    }
}

/// A Workspace Window, where unasked alerts present, over the real update services.
fn workspace(cx: &mut gpui::TestAppContext) -> &mut gpui::VisualTestContext {
    let (workspace, visual) = cx.add_window_view(|window, cx| {
        crate::ui::WorkspaceManager::new_with_adapters(
            std::env::temp_dir(),
            crate::ui::workspace_manager::tests::workspace_adapters(),
            crate::settings::sidebar::SidebarState::Expanded,
            window,
            cx,
        )
    });
    visual.update(|window, cx| {
        window.activate_window();
        workspace.update(cx, |workspace, cx| workspace.focus(window, cx));
    });
    visual.run_until_parked();
    visual
}

fn alert_open(cx: &mut gpui::VisualTestContext) -> bool {
    cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx))
}

/// The restart's preview succeeds and it runs; the Core then reports that it failed. Nothing
/// interrupts: the outcome shows as the title bar's step. A refusal at the preview, while the
/// person waits, opens D5 at once.
#[gpui::test]
fn a_restart_that_ran_opens_no_alert_and_a_refused_preview_does(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    cx.update(crate::ui::updates::init);
    let visual = workspace(cx);
    machines.emit(MachineUpdatesEvent::Machines(vec![waiting(PENGUIN, 0)]));
    visual.run_until_parked();

    visual.update(|_, cx| commands::perform(restart_core(PENGUIN), commands::Presenter::Front, cx));
    visual.run_until_parked();
    assert_eq!(
        machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)]
    );
    machines.emit(MachineUpdatesEvent::Finished {
        machine: PENGUIN,
        action: MachineAction::Restart,
        outcome: Err(ActionFailure::Failed),
    });
    visual.run_until_parked();
    assert!(
        !alert_open(visual),
        "no alert opens unasked after the restart ran"
    );

    machines.hold_previews();
    visual.update(|_, cx| commands::perform(restart_core(PENGUIN), commands::Presenter::Front, cx));
    visual.run_until_parked();
    machines.answer_preview(|_, _| Err(ActionFailure::Unreachable));
    visual.run_until_parked();
    assert!(
        alert_open(visual),
        "a refused preview explains itself at once"
    );
    A11yTree::read(visual)
        .text("SpaceTerm can\u{2019}t reach penguin. Try again when it\u{2019}s connected.");
}

/// Check for Updates during a download the person already started answers as a check does:
/// the release is downloading. It never offers to stop it.
#[gpui::test]
fn check_for_updates_during_a_download_says_it_is_downloading(cx: &mut gpui::TestAppContext) {
    let (adapter, updates, _) = services(cx);
    let visual = workspace(cx);
    updates.update(visual, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available(version()));
    visual.run_until_parked();
    updates.update(visual, |updates, cx| updates.download(cx));
    adapter.emit(UpdateEvent::Downloading {
        received: 1,
        total: 10,
    });
    visual.run_until_parked();

    visual.update(|_, cx| cx.defer(crate::ui::updates::check_for_updates));
    visual.run_until_parked();
    let tree = A11yTree::read(visual);
    tree.text("SpaceTerm 0.4.2 Is Downloading");
    tree.node("Release Notes");
    assert!(tree.find("Stop Download").is_none());
    assert_eq!(adapter.cancellations.get(), 0);
}

fn relaunch_failed(updates: &Entity<ApplicationUpdates>, cx: &mut gpui::VisualTestContext) -> bool {
    cx.update(|_, cx| {
        matches!(updates.read(cx).state(), UpdateState::Failed { error } if error.relaunch_failed())
    })
}

/// A relaunch that failed, reached through Check for Updates: D7.
fn failed_relaunch(
    cx: &mut gpui::TestAppContext,
) -> (
    Rc<crate::updates::testing::RecordingAdapter>,
    Entity<ApplicationUpdates>,
    &mut gpui::VisualTestContext,
) {
    let (adapter, updates, _) = services(cx);
    let visual = workspace(cx);
    updates.update(visual, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available(version()));
    visual.run_until_parked();
    updates.update(visual, |updates, cx| updates.download(cx));
    adapter.emit(UpdateEvent::Ready);
    visual.run_until_parked();
    updates.update(visual, |updates, cx| updates.relaunch(cx));
    // The updater gives up its installer while SpaceTerm keeps running.
    adapter.emit(UpdateEvent::Finished);
    visual.run_until_parked();
    assert!(relaunch_failed(&updates, visual));
    visual.update(|_, cx| cx.defer(crate::ui::updates::check_for_updates));
    visual.run_until_parked();
    A11yTree::read(visual).text("Couldn\u{2019}t Relaunch SpaceTerm");
    (adapter, updates, visual)
}

/// SpaceTerm's own update as the title bar and every row show it: the main area's label, then
/// the row's status and action.
fn app_shown(cx: &mut gpui::VisualTestContext) -> (Option<String>, String, Option<String>) {
    cx.update(|_, cx| {
        let view = state::current(cx);
        let row = view.app_row();
        (
            view.pill().map(|pill| pill.label.to_string()),
            row.status
                .map(|status| status.to_string())
                .unwrap_or_default(),
            row.action.map(|action| action.label.to_string()),
        )
    })
}

fn failed_step(status: &str, action: Option<&str>) -> (Option<String>, String, Option<String>) {
    (
        Some(crate::updates::copy::title_bar::UPDATE_FAILED.to_owned()),
        status.to_owned(),
        action.map(str::to_owned),
    )
}

fn dismissed_step(status: &str, action: Option<&str>) -> (Option<String>, String, Option<String>) {
    (None, status.to_owned(), action.map(str::to_owned))
}

/// Cancel on D7 hides only the failure's title-bar step on this desktop. The updater keeps its
/// failure and its installer, and the row keeps the failure with its Try Again.
#[gpui::test]
fn cancel_on_a_failed_relaunch_dismisses_only_its_step(cx: &mut gpui::TestAppContext) {
    let (adapter, updates, visual) = failed_relaunch(cx);
    assert_eq!(
        app_shown(visual),
        failed_step("Relaunch failed", Some("Try Again"))
    );
    click("modal-action-update-explain-cancel", visual);
    assert_eq!(
        app_shown(visual),
        dismissed_step("Relaunch failed", Some("Try Again")),
        "the row keeps the failure"
    );
    assert!(relaunch_failed(&updates, visual), "the updater keeps it");
    assert_eq!(adapter.installations.get(), 1, "Cancel relaunches nothing");
}

#[gpui::test]
fn escape_on_a_failed_relaunch_dismisses_only_its_step(cx: &mut gpui::TestAppContext) {
    let (adapter, updates, visual) = failed_relaunch(cx);
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(!alert_open(visual));
    assert_eq!(
        app_shown(visual),
        dismissed_step("Relaunch failed", Some("Try Again"))
    );
    assert!(relaunch_failed(&updates, visual));
    assert_eq!(adapter.installations.get(), 1, "Escape relaunches nothing");
}

/// Check for Updates fails with `error` for release 0.4.2, which an earlier check found.
fn failed_check(
    error: UpdateError,
    cx: &mut gpui::TestAppContext,
) -> (
    Rc<crate::updates::testing::RecordingAdapter>,
    Entity<ApplicationUpdates>,
    &mut gpui::VisualTestContext,
) {
    failed_check_beside(error, Vec::new(), cx)
}

/// [`failed_check`] while `machines` are connected.
fn failed_check_beside(
    error: UpdateError,
    machines: Vec<MachineUpdate>,
    cx: &mut gpui::TestAppContext,
) -> (
    Rc<crate::updates::testing::RecordingAdapter>,
    Entity<ApplicationUpdates>,
    &mut gpui::VisualTestContext,
) {
    let (adapter, updates, scripted) = services(cx);
    scripted.emit(MachineUpdatesEvent::Machines(machines));
    cx.update(crate::ui::updates::init);
    let visual = workspace(cx);
    updates.update(visual, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available(version()));
    adapter.emit(UpdateEvent::Finished);
    visual.run_until_parked();
    visual.update(|_, cx| cx.defer(crate::ui::updates::check_for_updates));
    visual.run_until_parked();
    adapter.emit(UpdateEvent::Failed(error));
    adapter.emit(UpdateEvent::Finished);
    visual.run_until_parked();
    (adapter, updates, visual)
}

/// A failed Check for Updates opens the same D6 as `Update Failed`, never `Couldn't Check for
/// Updates`. Cancel then hides only the failure: the release is still not downloaded, so the
/// main area offers `Download Update` and the title bar never looks up to date.
#[gpui::test]
fn a_failed_download_from_check_for_updates_opens_d6(cx: &mut gpui::TestAppContext) {
    let (adapter, _, visual) = failed_check(UpdateError::Download, cx);
    let tree = A11yTree::read(visual);
    tree.text("Couldn\u{2019}t Download SpaceTerm 0.4.2");
    tree.text("Check your network, then try again.");
    tree.node("Try Again");
    tree.node("Cancel");
    assert!(tree.find("Couldn\u{2019}t Check for Updates").is_none());
    assert_eq!(
        app_shown(visual),
        failed_step("Download failed", Some("Try Again"))
    );

    let downloads = adapter.downloads.get();
    click("modal-action-update-explain-cancel", visual);
    assert!(!alert_open(visual));
    assert_eq!(
        app_shown(visual),
        (
            Some(crate::updates::copy::title_bar::DOWNLOAD_UPDATE.to_owned()),
            "Download failed".to_owned(),
            Some("Try Again".to_owned())
        ),
        "the release keeps its step; the row keeps the failure"
    );
    assert_eq!(
        adapter.downloads.get(),
        downloads,
        "Cancel downloads nothing"
    );

    // The row's Try Again fails again: that newer failure is the person's to see.
    let try_again = visual.update(|_, cx| {
        state::current(cx)
            .app_row()
            .action
            .expect("the row offers Try Again")
            .command
    });
    visual.update(|_, cx| commands::perform(try_again, commands::Presenter::Front, cx));
    visual.run_until_parked();
    adapter.emit(UpdateEvent::Available(version()));
    visual.run_until_parked();
    assert_eq!(
        adapter.downloads.get(),
        downloads + 1,
        "Try Again downloads"
    );
    adapter.emit(UpdateEvent::Failed(UpdateError::Download));
    adapter.emit(UpdateEvent::Finished);
    visual.run_until_parked();
    assert_eq!(
        app_shown(visual),
        failed_step("Download failed", Some("Try Again")),
        "a newer failure shows again"
    );
}

/// Check for Updates while SpaceTerm's row shows a download failure answers with that failure's
/// D6, the state the pill and the row report, instead of a check whose alert says the release is
/// downloading. Its Try Again downloads.
#[gpui::test]
fn check_for_updates_after_a_failed_download_opens_its_d6(cx: &mut gpui::TestAppContext) {
    let (adapter, _, visual) = failed_check(UpdateError::Download, cx);
    click("modal-action-update-explain-cancel", visual);
    assert!(!alert_open(visual));
    let (checks, downloads) = (adapter.checks.get(), adapter.downloads.get());

    visual.update(|_, cx| cx.defer(crate::ui::updates::check_for_updates));
    visual.run_until_parked();
    let tree = A11yTree::read(visual);
    tree.text("Couldn\u{2019}t Download SpaceTerm 0.4.2");
    tree.text("Check your network, then try again.");
    assert!(tree.find("SpaceTerm 0.4.2 Is Downloading").is_none());
    assert_eq!(adapter.checks.get(), checks, "the alert answers by itself");

    click("modal-action-update-explain-proceed", visual);
    visual.run_until_parked();
    adapter.emit(UpdateEvent::Available(version()));
    visual.run_until_parked();
    assert_eq!(
        adapter.downloads.get(),
        downloads + 1,
        "Try Again downloads"
    );
}

/// While a Core is too new for this SpaceTerm, Cancel on D6 hides only the failure: the hard
/// stop's `Download Update` stays in the main area.
#[gpui::test]
fn cancel_on_d6_under_a_hard_stop_keeps_download_update(cx: &mut gpui::TestAppContext) {
    use spaceterm_core::protocol::version::Incompatible;
    let mut too_new = waiting(PENGUIN, 2);
    too_new.link = crate::updates::machines::MachineLink::Incompatible(Incompatible::CallerTooOld);
    let (_, _, visual) = failed_check_beside(UpdateError::Download, vec![too_new], cx);
    assert_eq!(
        app_shown(visual),
        failed_step("Download failed", Some("Try Again"))
    );
    click("modal-action-update-explain-cancel", visual);
    assert_eq!(
        app_shown(visual),
        (
            Some(crate::updates::copy::title_bar::DOWNLOAD_UPDATE.to_owned()),
            "Download failed".to_owned(),
            Some("Try Again".to_owned())
        ),
        "the hard stop keeps its step; the row keeps the failure"
    );
}

/// A relaunch that ran out of time leaves its installer with the updater, and no check runs
/// while it does. Cancel on D7 hides only the failure: `Relaunch to Update` stays in the main
/// area, because only a relaunch installs that update.
#[gpui::test]
fn cancel_on_d7_with_a_held_installer_keeps_relaunch_to_update(cx: &mut gpui::TestAppContext) {
    let (adapter, updates, _) = services(cx);
    let visual = workspace(cx);
    updates.update(visual, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available(version()));
    visual.run_until_parked();
    updates.update(visual, |updates, cx| updates.download(cx));
    adapter.emit(UpdateEvent::Ready);
    visual.run_until_parked();
    updates.update(visual, |updates, cx| updates.relaunch(cx));
    visual
        .executor()
        .advance_clock(crate::updates::RELAUNCH_DEADLINE);
    visual.run_until_parked();
    assert!(visual.update(|_, cx| updates.read(cx).holds_failed_relaunch()));
    visual.update(|_, cx| cx.defer(crate::ui::updates::check_for_updates));
    visual.run_until_parked();
    A11yTree::read(visual).text("Couldn\u{2019}t Relaunch SpaceTerm");
    assert_eq!(
        app_shown(visual),
        failed_step("Relaunch failed", Some("Try Again"))
    );
    click("modal-action-update-explain-cancel", visual);
    assert_eq!(
        app_shown(visual),
        (
            Some(crate::updates::copy::title_bar::RELAUNCH_TO_UPDATE.to_owned()),
            "Relaunch failed".to_owned(),
            Some("Try Again".to_owned())
        ),
        "the held update keeps its step; the row keeps the failure"
    );
}

/// D8 from a failed Check for Updates. OK hides the title-bar step; the row keeps the failure.
#[gpui::test]
fn an_installation_that_cant_be_replaced_from_check_for_updates_opens_d8(
    cx: &mut gpui::TestAppContext,
) {
    use crate::updates::copy::Platform;
    let (_, _, visual) = failed_check(UpdateError::ReadOnly, cx);
    let tree = A11yTree::read(visual);
    tree.text(dialogs::cant_replace_title(Platform::HOST));
    tree.text(dialogs::cant_replace_message(Platform::HOST));
    tree.node("OK");
    let status = crate::updates::copy::rows::cant_replace(Platform::HOST);
    assert_eq!(app_shown(visual), failed_step(status, None));

    click("modal-action-update-explain-ok", visual);
    assert!(!alert_open(visual));
    assert_eq!(app_shown(visual), dismissed_step(status, None));
}

fn waiting(machine: MachineId, sessions: u32) -> MachineUpdate {
    use spaceterm_core::protocol::capability::{Capabilities, Capability};
    use spaceterm_core::update::facts::{
        PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
    };
    MachineUpdate {
        detailed: None,
        machine,
        name: "penguin".into(),
        local: false,
        link: crate::updates::machines::MachineLink::Connected,
        facts: Some(UpdateFacts {
            revision: 1,
            platform: Platform::LinuxX86_64,
            running: Version::parse("0.11.3").unwrap(),
            pending: Some(PendingVersion {
                version: Version::parse("0.11.4").unwrap(),
                protocols: Protocols::CURRENT,
                breaking: false,
                installed_at: crate::updates::now() - 60,
                overdue_since: None,
                stranded: Vec::new(),
            }),
            sessions,
            operation: None,
            check: UpdateCheck::Never,
            skipped: None,
            last_failure: None,
        }),
        capabilities: Capabilities::fake(&Capability::ALL),
        push: None,
        bundled: None,
        failed_push: None,
        asked: None,
    }
}

/// A reminder's click performs the step: a Core's opens its confirmation from a fresh preview
/// and restarts nothing by itself. Reminders post only once the person left SpaceTerm.
#[gpui::test]
fn a_reminder_click_performs_the_step_and_asks_first(cx: &mut gpui::TestAppContext) {
    use crate::updates::reminders::UpdateReminders;
    use crate::updates::reminders::testing::RecordingReminders;

    let machines = Rc::new(ScriptedMachines::default());
    let reminders = Rc::new(RecordingReminders::default());
    cx.update(|cx| {
        MachineUpdates::install(machines.clone(), cx);
        UpdateReminders::install(reminders.clone(), cx);
    });
    let _ = manual_updates(cx);
    cx.update(crate::ui::updates::init);
    let visual = workspace(cx);
    let mut asking = waiting(PENGUIN, 3);
    let pending = asking.facts.as_mut().unwrap().pending.as_mut().unwrap();
    pending.installed_at = crate::updates::now() - crate::updates::ladder::ASK_AFTER - 60;
    // A breaking update reminds under every cadence; the reminder's words are the ladder's.
    pending.breaking = true;
    machines.emit(MachineUpdatesEvent::Machines(vec![asking]));
    visual.run_until_parked();
    assert!(
        reminders.shown.borrow().is_empty(),
        "the pill is the reminder while the window is active"
    );
    visual.deactivate_window();
    visual.update(|_, cx| UpdateReminders::activation_changed(cx));
    visual
        .executor()
        .advance_clock(crate::updates::reminders::LEFT_SPACETERM);
    visual.run_until_parked();

    let shown = reminders.shown.borrow().clone();
    assert_eq!(shown.len(), 1, "the Ask stage reminds");
    reminders.click(&shown[0].0);
    visual.run_until_parked();
    assert_eq!(machines.asked(), [(PENGUIN, MachineAction::Restart)]);
    assert!(alert_open(visual), "the confirmation asks");
    assert!(
        machines.executed_actions().is_empty(),
        "the click restarts nothing unasked"
    );
}

/// The title bar's control over the real update state, in a host that presents its alerts.
fn live_control(cx: &mut gpui::TestAppContext) -> &mut gpui::VisualTestContext {
    let (_, visual) = cx.add_window_view(|window, cx| Host {
        control: cx.new(|cx| UpdateControl::new(window, cx)),
    });
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    visual
}

/// penguin waits at the Ask stage with 2 Terminal Sessions, so the pill offers its Restart Core…,
/// and the person checked penguin for updates from this desktop.
fn checking_beside_a_waiting_update(
    cx: &mut gpui::TestAppContext,
) -> (Rc<ScriptedMachines>, &mut gpui::VisualTestContext) {
    let (_, _, machines) = services(cx);
    let mut asking = waiting(PENGUIN, 2);
    asking
        .facts
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .installed_at = crate::updates::now() - crate::updates::ladder::ASK_AFTER - 60;
    machines.emit(MachineUpdatesEvent::Machines(vec![asking]));
    cx.run_until_parked();
    cx.update(|cx| {
        MachineUpdates::global(cx).unwrap().update(cx, |store, cx| {
            store.perform(PENGUIN, MachineAction::CheckAgain, Surface::Settings, cx)
        })
    });
    let visual = live_control(cx);
    (machines, visual)
}

/// A check the person started on a Machine whose update waits is the row's one presentation in
/// the list. The row only reports it: choosing it restarts nothing and asks nothing.
#[gpui::test]
fn a_checking_row_beside_a_waiting_update_reports_and_restarts_nothing(
    cx: &mut gpui::TestAppContext,
) {
    let (machines, visual) = checking_beside_a_waiting_update(cx);
    click("update-pill-menu", visual);
    A11yTree::read(visual).node("Checking\u{2026}");
    click("update-row-0", visual);
    assert!(machines.asked().is_empty(), "the row previews no restart");
    assert!(!shows(CONFIRM, visual), "no confirmation opens");
    assert_eq!(machines.performed(), [(PENGUIN, MachineAction::CheckAgain)]);
}

/// The same check failed. The row shows the failure alone, and choosing it opens that failure's
/// details, whose only follow-up checks again. Nothing restarts.
#[gpui::test]
fn a_failed_check_row_beside_a_waiting_update_opens_its_details(cx: &mut gpui::TestAppContext) {
    let (machines, visual) = checking_beside_a_waiting_update(cx);
    machines.emit(MachineUpdatesEvent::Finished {
        machine: PENGUIN,
        action: MachineAction::CheckAgain,
        outcome: Err(ActionFailure::Busy),
    });
    visual.run_until_parked();
    click("update-pill-menu", visual);
    A11yTree::read(visual).node("Check failed\u{2026}");
    click("update-row-0", visual);
    assert!(machines.asked().is_empty(), "the row previews no restart");
    assert!(!shows(CONFIRM, visual), "no confirmation opens");
    A11yTree::read(visual).node("Check Again");
    click("modal-action-update-explain-proceed", visual);
    assert_eq!(
        machines.performed(),
        [
            (PENGUIN, MachineAction::CheckAgain),
            (PENGUIN, MachineAction::CheckAgain)
        ]
    );
    assert!(
        machines.asked().is_empty(),
        "checking again restarts nothing"
    );
}
