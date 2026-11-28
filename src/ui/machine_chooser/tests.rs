use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext, div};
use spaceterm_ui::a11y_testing::{A11yTree, perform};

use super::*;

struct Harness {
    chooser: Entity<MachineChooser>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        spaceterm_ui::ModalLayer::new(div().size_full().child(self.chooser.clone()))
    }
}

fn route(byte: u8) -> DirectRouteId {
    [byte; 32]
}

fn machine(byte: u8, name: &str, connected: bool) -> MachineChoice {
    MachineChoice {
        route: route(byte),
        name: name.to_owned().into(),
        address: None,
        connected,
    }
}

/// An open chooser listing `choices`, and every event it reported.
fn open_chooser(
    choices: MachineChoices,
    cx: &mut TestAppContext,
) -> (
    Entity<MachineChooser>,
    Rc<RefCell<Vec<MachineChooserEvent>>>,
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&events);
    let (harness, cx) = cx.add_window_view(move |window, cx| {
        let chooser = cx.new(|cx| MachineChooser::new(window, cx));
        cx.subscribe(&chooser, move |_, _, event: &MachineChooserEvent, _| {
            recorded.borrow_mut().push(event.clone());
        })
        .detach();
        Harness { chooser }
    });
    let chooser = harness.read_with(cx, |harness, _| harness.chooser.clone());
    cx.update(|window, cx| {
        window.activate_window();
        chooser.update(cx, |chooser, cx| {
            chooser.set_choices(choices, cx);
            assert!(chooser.open(window, cx));
        });
    });
    cx.run_until_parked();
    (chooser, events, cx)
}

fn press(keys: &str, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes(keys);
    cx.run_until_parked();
}

#[gpui::test]
fn return_chooses_the_first_connected_machine(cx: &mut TestAppContext) {
    let (chooser, events, cx) = open_chooser(
        MachineChoices::Read(vec![
            machine(1, "studio", false),
            machine(2, "penguin", true),
        ]),
        cx,
    );

    press("enter", cx);

    assert_eq!(*events.borrow(), [MachineChooserEvent::Chosen(route(2))]);
    assert!(!chooser.read_with(cx, |chooser, _| chooser.blocks_terminal_input()));
}

#[gpui::test]
fn a_machine_that_is_not_connected_cannot_be_chosen(cx: &mut TestAppContext) {
    let (chooser, events, cx) = open_chooser(
        MachineChoices::Read(vec![
            machine(1, "studio", false),
            machine(2, "penguin", false),
        ]),
        cx,
    );
    let palette = chooser.read_with(cx, |chooser, _| chooser.palette.clone());
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        None
    );
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.results_note().cloned()),
        Some(SharedString::from("No Machine is connected."))
    );

    let tree = A11yTree::read(cx);
    perform(cx, tree.node("studio"), gpui::accesskit::Action::Click);

    assert!(
        !events
            .borrow()
            .iter()
            .any(|event| matches!(event, MachineChooserEvent::Chosen(_)))
    );
}

#[gpui::test]
fn no_saved_machines_says_so_beside_add_machine(cx: &mut TestAppContext) {
    let (_, events, cx) = open_chooser(MachineChoices::Read(Vec::new()), cx);
    let tree = A11yTree::read(cx);
    assert!(tree.find_text(NO_MACHINES).is_some());

    perform(cx, tree.node(ADD_MACHINE), gpui::accesskit::Action::Click);

    assert_eq!(*events.borrow(), [MachineChooserEvent::AddMachine]);
}

#[gpui::test]
fn return_waits_for_the_saved_machines(cx: &mut TestAppContext) {
    let (chooser, events, cx) = open_chooser(MachineChoices::Loading, cx);
    press("enter", cx);
    assert!(events.borrow().is_empty());

    chooser.update(cx, |chooser, cx| {
        chooser.set_choices(MachineChoices::Read(vec![machine(3, "penguin", true)]), cx);
    });
    cx.run_until_parked();
    press("enter", cx);

    assert_eq!(*events.borrow(), [MachineChooserEvent::Chosen(route(3))]);
}

#[gpui::test]
fn escape_dismisses_without_a_choice(cx: &mut TestAppContext) {
    let (_, events, cx) = open_chooser(MachineChoices::Read(vec![machine(2, "penguin", true)]), cx);

    press("escape", cx);

    assert_eq!(*events.borrow(), [MachineChooserEvent::Dismissed]);
}
