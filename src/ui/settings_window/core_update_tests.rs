//! The Server row's Update Server journey, driven through a scripted service.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};

use super::testing::{Outcome, ScriptedUpdate, UpdateCall, approved, available, included};
use super::{
    CoreUpdateControl, FINISH_SELECTOR, IncludedCore, UPDATE_CANCEL_SELECTOR,
    UPDATE_CHECK_SELECTOR, UPDATE_CONFIRM_SELECTOR, UPDATE_SELECTOR, UpdateAction, UpdateFacts,
    UpdatePresentation, UpdateState, UpdateStep, present_update,
};
use crate::appearance::Appearance;
use crate::local_startup::CandidateError;
use crate::machine_connection::CoreError;
use crate::machine_connection::service::activation::{ActivationPhase, ActivationStatus};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::settings_window::SettingsWindow;
use crate::ui::settings_window::machines::testing::{
    ScriptedCore, ScriptedService, connected, not_installed, stopped, uninstalled,
};
use crate::ui::settings_window::machines::{CoreServiceControl, LocalCoreControl, OfferedMachine};
use crate::ui::sidebar_window::{SidebarOwner, form::CaptionTone};

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";

struct Harness<'a> {
    window: Entity<SettingsWindow>,
    update: Arc<ScriptedUpdate>,
    service: Arc<ScriptedService>,
    core: Arc<ScriptedCore>,
    cx: &'a mut VisualTestContext,
}

/// Opens Settings on the Servers section with a service and update control reporting `facts`.
fn open(facts: UpdateFacts, cx: &mut TestAppContext) -> Harness<'_> {
    let harness = open_unread(facts, cx);
    click(NAVIGATION_SELECTOR, harness.cx);
    harness
}

/// Opens Settings on its first section, as it opens, with a service and update control reporting
/// `facts`. Nothing has read the server yet: Servers reads it once it is shown.
fn open_unread(facts: UpdateFacts, cx: &mut TestAppContext) -> Harness<'_> {
    let status = facts.status;
    let update = ScriptedUpdate::new(facts);
    let service = ScriptedService::new(status);
    let core = ScriptedCore::new(Ok(connected(1)));
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let offered = OfferedMachine {
        connection: Ok(Arc::clone(&core) as Arc<dyn LocalCoreControl>),
        service: Some(Ok(Arc::clone(&service) as Arc<dyn CoreServiceControl>)),
        update: Some(Arc::clone(&update) as Arc<dyn CoreUpdateControl>),
        command_line_tool: None,
    };
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            crate::theme_registry::testing::offline(),
            Some(offered),
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    Harness {
        window,
        update,
        service,
        core,
        cx,
    }
}

/// Clicks `selector` on the page, or in the Server row's ⋯ menu, where every step but the row's
/// one button lives.
fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let menu = super::super::machine_card::MENU_SELECTOR;
    if cx.debug_bounds(selector).is_none() && cx.debug_bounds(menu).is_some() {
        click_at(menu, cx);
    }
    click_at(selector, cx);
}

fn click_at(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

fn modal_action(selector: &str) -> &'static str {
    Box::leak(format!("modal-action-{selector}").into_boxed_str())
}

fn click_modal(selector: &str, cx: &mut VisualTestContext) {
    click(modal_action(selector), cx);
}

fn alert_open(selector: &str, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(modal_action(selector)).is_some()
}

fn rendered(selector: &'static str, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(selector).is_some()
}

impl Harness<'_> {
    fn presentation(&mut self) -> UpdatePresentation {
        self.window.read_with(self.cx, |settings, _| {
            settings
                .core_update_presentation()
                .expect("the version row should be offered")
        })
    }
}

fn activation(phase: ActivationPhase) -> ActivationStatus {
    ActivationStatus {
        operation: [7; 32],
        previous: approved(),
        candidate: included(),
        stopped: false,
        phase,
    }
}

const RETURNED: ActivationPhase = ActivationPhase::Returned {
    failure: crate::machine_connection::update::facts::StartFailure::NotReady,
    running: false,
};

fn presented(facts: UpdateFacts) -> UpdatePresentation {
    present_update(&UpdateState {
        facts: Some(facts),
        failure: None,
        working: None,
    })
}

#[gpui::test]
fn a_confirmed_update_replaces_the_running_core_once(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    let inventory_refreshes = Rc::new(std::cell::Cell::new(0));
    let (inventory, _observation) = harness.cx.update(|_, cx| {
        crate::ui::updates::machine_rows::testing::install_up_to_date(cx);
        let inventory = cx.new(crate::ui::updates::machine_rows::CoreUpdates::new);
        let refreshes = inventory_refreshes.clone();
        let observation = cx.observe(&inventory, move |_, _| {
            refreshes.set(refreshes.get() + 1);
        });
        (inventory, observation)
    });
    harness.cx.run_until_parked();
    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Update available");
    assert!(
        presentation
            .explanation
            .contains("Version 1.4.0 (commit aaaaaaa) is running."),
        "{}",
        presentation.explanation
    );
    assert!(
        presentation
            .explanation
            .contains("includes SpaceTerm Core version 1.5.0 (commit bbbbbbb)"),
        "{}",
        presentation.explanation
    );
    assert_eq!(presentation.actions, [UpdateAction::Update]);
    assert_eq!(
        spaceterm_ui::a11y_testing::A11yTree::read(harness.cx).node("Update\u{2026}")["aria"]["role"],
        "Button",
        "the row's one button reads Update…"
    );

    click(UPDATE_SELECTOR, harness.cx);
    assert!(alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);

    let before_completion = inventory_refreshes.get();
    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);
    assert_eq!(
        harness.update.changes(),
        [UpdateCall::PrepareUpdate, UpdateCall::Activate]
    );
    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Up to date");
    assert!(presentation.actions.is_empty());
    // The replaced server is read afresh by the other rows.
    assert!(harness.core.refreshes() >= 2);
    assert!(
        inventory_refreshes.get() > before_completion,
        "the Updates inventory must refresh when the admitted local update completes"
    );
    drop(inventory);
    assert!(harness.service.changes().is_empty());
}

#[gpui::test]
fn cancelling_the_confirmation_changes_nothing(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    harness.cx.update(|window, cx| {
        cx.set_global(super::super::OpenSettingsWindow(
            window.window_handle().downcast::<SettingsWindow>().unwrap(),
        ));
        crate::ui::updates::machine_rows::CoreUpdateAction::Local.activate(cx);
    });
    harness.cx.run_until_parked();
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);
    assert!(alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    click_modal(UPDATE_CANCEL_SELECTOR, harness.cx);

    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);
    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Update available");
    assert_eq!(presentation.actions, [UpdateAction::Update]);
    assert!(harness.service.changes().is_empty());
}

#[test]
fn the_confirmation_names_the_candidate_and_every_clients_sessions() {
    let prepared = |running, stopped| super::PreparedChange {
        previous: approved(),
        candidate: included(),
        running,
        stopped,
        commit: Box::new(|| Err(CoreError::Changed)),
    };
    let live = super::UpdateConfirmation::of(&prepared(true, false));
    assert_eq!(live.title, "Update SpaceTerm Core");
    let core = crate::machine_connection::service::control::this_computer();
    assert_eq!(
        live.message.as_ref(),
        format!("Update SpaceTerm Core on {core} to 1.5.0 (commit bbbbbbb)?")
    );
    assert_eq!(
        live.detail.as_ref(),
        "It changes from 1.4.0 (commit aaaaaaa) to 1.5.0 (commit bbbbbbb). All Terminal Sessions \
         on it end for every client, and Workspaces are kept."
    );
    assert_eq!(live.confirm, "Update SpaceTerm Core");

    let parked = super::UpdateConfirmation::of(&prepared(false, true));
    assert_eq!(
        parked.message.as_ref(),
        format!("Update the stopped SpaceTerm Core on {core} to 1.5.0 (commit bbbbbbb)?")
    );
    assert_eq!(
        parked.detail.as_ref(),
        "It changes from 1.4.0 (commit aaaaaaa) to 1.5.0 (commit bbbbbbb). It isn\u{2019}t \
         running, so no Terminal Sessions end, and it stays stopped until you start it."
    );
}

#[test]
fn builds_of_one_version_and_commit_are_told_apart_by_their_package() {
    use crate::machine_connection::service::control::{BuildName, build_labels};
    let name = |package| BuildName {
        version: "1.5.0",
        commit: "bbbbbbbbbb",
        package,
    };
    assert_eq!(
        build_labels(name("1111111122"), name("3333333344")),
        (
            "1.5.0 (commit bbbbbbb, package 11111111)".to_owned(),
            "1.5.0 (commit bbbbbbb, package 33333333)".to_owned()
        )
    );
    let other = BuildName {
        version: "1.4.0",
        ..name("3333333344")
    };
    assert_eq!(
        build_labels(name("1111111122"), other).1,
        "1.4.0 (commit bbbbbbb)"
    );
}

#[gpui::test]
fn updating_a_stopped_core_keeps_it_stopped(cx: &mut TestAppContext) {
    let mut facts = available();
    facts.status = Ok(stopped());
    let mut harness = open(facts, cx);
    assert!(
        harness
            .presentation()
            .explanation
            .contains("Updating keeps SpaceTerm Core stopped")
    );

    click(UPDATE_SELECTOR, harness.cx);
    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);

    assert_eq!(harness.presentation().state, "Up to date");
    assert!(
        harness
            .presentation()
            .explanation
            .contains("you stopped SpaceTerm Core")
    );
    // Nothing started the server, and the update reached no Workspace Window.
    assert!(harness.service.changes().is_empty());
}

#[gpui::test]
fn a_changed_machine_needs_a_fresh_confirmation(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    harness.update.answer(Outcome::Changed);
    click(UPDATE_SELECTOR, harness.cx);
    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);

    let presentation = harness.presentation();
    assert_eq!(presentation.tone, CaptionTone::Error);
    assert!(
        presentation
            .explanation
            .contains("SpaceTerm Core changed. Review the update again."),
        "{}",
        presentation.explanation
    );
    // The refused approval is spent. Updating again prepares and asks afresh.
    assert_eq!(presentation.actions, [UpdateAction::Update]);
    harness.update.answer(Outcome::Ready);
    click(UPDATE_SELECTOR, harness.cx);
    assert!(alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(
        harness.update.changes(),
        [
            UpdateCall::PrepareUpdate,
            UpdateCall::Activate,
            UpdateCall::PrepareUpdate
        ]
    );
}

#[gpui::test]
fn a_failed_update_reports_the_return_to_the_previous_version(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    harness.update.answer(Outcome::Returned);
    click(UPDATE_SELECTOR, harness.cx);
    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);

    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Update failed");
    assert!(
        presentation.explanation.contains(
            "didn\u{2019}t start, and Terminal Sessions ended. SpaceTerm Core returned to 1.4.0"
        ),
        "{}",
        presentation.explanation
    );
    assert_eq!(presentation.actions, [UpdateAction::Check]);
    assert_eq!(
        harness.update.changes(),
        [UpdateCall::PrepareUpdate, UpdateCall::Activate]
    );
}

#[gpui::test]
fn an_unresolved_update_finishes_without_asking_again(cx: &mut TestAppContext) {
    let mut harness = open(
        UpdateFacts {
            // The status read refuses while the update is unresolved.
            status: Err(CoreError::ActivationUnresolved),
            activation: Some(activation(ActivationPhase::Activating)),
            ..available()
        },
        cx,
    );
    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Update unfinished");
    assert_eq!(presentation.actions, [UpdateAction::Finish]);

    click(FINISH_SELECTOR, harness.cx);
    assert!(!alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(harness.update.changes(), [UpdateCall::Finish]);

    let uncertain = presented(UpdateFacts {
        status: Err(CoreError::ActivationUnresolved),
        activation: Some(activation(ActivationPhase::Uncertain)),
        ..available()
    });
    assert_eq!(uncertain.state, "Update uncertain");
    assert_eq!(uncertain.actions, [UpdateAction::Finish]);
}

#[test]
fn an_approved_or_unavailable_build_offers_no_update() {
    let mut current = available();
    if let Ok(included) = &mut current.included {
        included.approved = true;
    }
    let presentation = presented(current);
    assert_eq!(presentation.state, "Up to date");
    assert!(presentation.actions.is_empty());

    for (error, state) in [
        (CandidateError::Unavailable, "No update"),
        (CandidateError::ClientRestartRequired, "Restart SpaceTerm"),
    ] {
        let presentation = presented(UpdateFacts {
            included: Err(error),
            ..available()
        });
        assert_eq!(presentation.state, state);
        assert!(presentation.actions.is_empty(), "{error:?}");
        if error == CandidateError::ClientRestartRequired {
            assert_eq!(
                presentation.explanation.as_ref(),
                "Quit and reopen SpaceTerm to update SpaceTerm Core."
            );
        } else {
            assert!(
                presentation.explanation.contains("1.4.0"),
                "{}",
                presentation.explanation
            );
        }
    }

    // An earlier release is never offered, as activation refuses it.
    let mut earlier = available();
    if let Ok(included) = &mut earlier.included {
        included.earlier = true;
    }
    let presentation = presented(earlier);
    assert_eq!(presentation.state, "No update");
    assert!(presentation.actions.is_empty());
    assert!(
        presentation.explanation.contains("an earlier release"),
        "{}",
        presentation.explanation
    );

    let unverified = presented(UpdateFacts {
        included: Err(CandidateError::Verification(CoreError::RuntimeUnavailable)),
        ..available()
    });
    assert_eq!(unverified.state, "Not verified");
    assert_eq!(unverified.actions, [UpdateAction::Check]);

    let uninstalled = presented(UpdateFacts {
        status: Ok(not_installed()),
        approved: None,
        ..available()
    });
    assert_eq!(uninstalled.state, "Not installed");
    assert!(uninstalled.actions.is_empty());
}

#[gpui::test]
fn a_staging_refusal_explains_itself_and_asks_nothing(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    harness.update.refuse_next(CoreError::CandidateChanged);
    click(UPDATE_SELECTOR, harness.cx);

    assert!(!alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    let presentation = harness.presentation();
    assert!(
        presentation
            .explanation
            .contains("Quit and reopen SpaceTerm"),
        "{}",
        presentation.explanation
    );
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);
}

#[gpui::test]
fn another_surface_starts_the_same_journey_once_the_row_reads(cx: &mut TestAppContext) {
    let harness = open(available(), cx);
    harness.window.update(harness.cx, |settings, cx| {
        settings.request_core_update(cx);
    });
    harness.cx.run_until_parked();
    assert!(alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);

    // A row that no longer offers an update drops the request and asks nothing.
    click_modal(UPDATE_CANCEL_SELECTOR, harness.cx);
    harness.update.change(|facts| {
        if let Ok(IncludedCore { approved, .. }) = &mut facts.included {
            *approved = true;
        }
    });
    harness
        .window
        .update(harness.cx, |settings, cx| settings.check_core_update(cx));
    harness.cx.run_until_parked();
    harness.window.update(harness.cx, |settings, cx| {
        settings.request_core_update(cx);
    });
    harness.cx.run_until_parked();
    assert!(!alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);
    assert!(rendered(
        super::super::machine_card::STATUS_SELECTOR,
        harness.cx
    ));
}

#[test]
fn progress_names_whether_sessions_end() {
    let progress = |step| {
        present_update(&UpdateState {
            facts: Some(available()),
            failure: None,
            working: Some(step),
        })
    };
    assert!(
        progress(UpdateStep::Update { running: true })
            .explanation
            .contains("Terminal Sessions end")
    );
    assert!(
        !progress(UpdateStep::Update { running: false })
            .explanation
            .contains("Terminal Sessions")
    );
}

/// Runs the real preparation and confirmation path, then replaces Settings before completion.
fn confirmed_update_after_reopening(
    facts: UpdateFacts,
    outcome: Outcome,
    cx: &mut TestAppContext,
) -> (UpdatePresentation, Option<CoreError>, Vec<UpdateCall>) {
    let (update, service, core) = {
        let harness = open(facts, cx);
        harness.update.answer(outcome);
        click(UPDATE_SELECTOR, harness.cx);
        let position = harness
            .cx
            .debug_bounds(modal_action(UPDATE_CONFIRM_SELECTOR))
            .unwrap()
            .center();
        // Dispatch without parking the executor, so Settings closes before completion runs.
        harness.cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    position,
                    modifiers: Modifiers::none(),
                    button: gpui::MouseButton::Left,
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                    position,
                    modifiers: Modifiers::none(),
                    button: gpui::MouseButton::Left,
                    click_count: 1,
                }),
                cx,
            );
        });
        let released = harness.window.downgrade();
        drop(harness.window);
        harness.cx.update(|window, _| window.remove_window());
        assert!(
            released.upgrade().is_none(),
            "admitted completion must not retain Settings"
        );
        (harness.update, harness.service, harness.core)
    };
    let offered = OfferedMachine {
        connection: Ok(core as Arc<dyn LocalCoreControl>),
        service: Some(Ok(service as Arc<dyn CoreServiceControl>)),
        update: Some(Arc::clone(&update) as Arc<dyn CoreUpdateControl>),
        command_line_tool: None,
    };
    let (settings, visual) = cx.add_window_view(|window, cx| {
        let mut settings = SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            crate::theme_registry::testing::offline(),
            Some(offered),
            window,
            cx,
        );
        assert!(
            settings
                .core_update
                .as_ref()
                .unwrap()
                .state
                .working
                .is_some(),
            "reopened Settings must retain admitted progress"
        );
        // Reopening and a second request cannot prepare or dispatch another activation.
        settings.request_update_action(UpdateAction::Update, cx);
        settings
    });
    visual.run_until_parked();
    settings.read_with(visual, |settings, _| {
        let row = settings.core_update.as_ref().unwrap();
        (row.presentation(), row.state.failure, update.changes())
    })
}

#[gpui::test]
fn a_confirmed_update_finishes_after_settings_reopens(cx: &mut TestAppContext) {
    for stopped_core in [false, true] {
        let mut facts = available();
        if stopped_core {
            facts.status = Ok(stopped());
        }
        let (presentation, failure, calls) =
            confirmed_update_after_reopening(facts, Outcome::Ready, cx);
        assert_eq!(presentation.state, "Up to date");
        if stopped_core {
            assert!(presentation.explanation.contains("stopped"));
        }
        assert_eq!(failure, None);
        assert_eq!(calls, [UpdateCall::PrepareUpdate, UpdateCall::Activate]);
    }
}

#[gpui::test]
fn a_confirmed_update_keeps_its_error_after_settings_reopens(cx: &mut TestAppContext) {
    let (_, failure, calls) = confirmed_update_after_reopening(available(), Outcome::Changed, cx);
    assert_eq!(failure, Some(CoreError::Changed));
    assert_eq!(calls, [UpdateCall::PrepareUpdate, UpdateCall::Activate]);
}

#[gpui::test]
fn a_successful_update_does_not_keep_a_transient_followup_read_failure(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    click(UPDATE_SELECTOR, harness.cx);
    harness
        .update
        .refuse_next_read(CoreError::StorageUnavailable);
    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);
    assert_eq!(harness.presentation().state, "Unknown");
    harness.window.update(harness.cx, |settings, cx| {
        settings.check_core_update(cx);
    });
    harness.cx.run_until_parked();
    assert_eq!(harness.presentation().state, "Up to date");
    assert_eq!(
        harness.window.read_with(harness.cx, |settings, _| {
            settings.core_update.as_ref().unwrap().state.failure
        }),
        None,
        "a successful reread must not claim the confirmed update failed"
    );
    assert_eq!(
        harness.update.changes(),
        [UpdateCall::PrepareUpdate, UpdateCall::Activate]
    );
}

/// Check Now checks for a newer SpaceTerm first, because server builds ship inside it, and reads
/// the server only once that check settles. The server reads as checking the whole time.
#[gpui::test]
fn check_now_reads_the_core_after_spaceterm_finishes_checking(cx: &mut TestAppContext) {
    let adapter = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| crate::updates::ApplicationUpdates::install(adapter.clone(), cx));
    let mut facts = available();
    if let Ok(included) = &mut facts.included {
        included.approved = true;
    }
    let harness = open(facts, cx);
    let reads = harness.update.reads();
    let window = harness.window.clone();
    let checking = |cx: &mut VisualTestContext| {
        window.read_with(cx, |settings, _| {
            settings
                .core_state(&super::super::machine_list::MachineChoice::Local)
                .checking
        })
    };

    click(super::super::machine_card::MENU_SELECTOR, harness.cx);
    click(UPDATE_CHECK_SELECTOR, harness.cx);

    assert_eq!(
        harness.update.reads(),
        reads,
        "the server waits for SpaceTerm's own check"
    );
    assert!(checking(harness.cx));

    adapter.emit(crate::updates::UpdateEvent::UpToDate);
    adapter.emit(crate::updates::UpdateEvent::Finished);
    for _ in 0..5 {
        harness
            .cx
            .executor()
            .advance_clock(std::time::Duration::from_millis(250));
        harness.cx.run_until_parked();
    }

    assert_eq!(harness.update.reads(), reads + 1);
    assert!(!checking(harness.cx));
    assert!(
        harness.update.changes().is_empty(),
        "checking changes nothing"
    );
}

/// A failed update read before the approved build reads again still names both builds from the
/// update's own record: the subtitle says what didn't update and which version runs.
#[gpui::test]
fn a_failed_update_names_its_builds_from_its_own_record(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let harness = open(
        UpdateFacts {
            status: Err(CoreError::ActivationUnresolved),
            approved: None,
            activation: Some(activation(RETURNED)),
            ..available()
        },
        cx,
    );

    let tree = A11yTree::read(harness.cx);
    assert!(
        tree.find_text("Couldn\u{2019}t update to 1.5.0 \u{b7} Still on 1.4.0")
            .is_some(),
        "the subtitle names both builds"
    );
}

/// The first version read Servers begins answers what it saw before the status item uninstalled
/// the server. Settings sets that answer aside and reads again, so the version row says Not
/// Installed and offers no update for a server that is gone.
#[gpui::test]
fn a_first_version_read_begun_before_a_status_item_uninstall_offers_no_stale_update(
    cx: &mut TestAppContext,
) {
    use crate::ui::settings_window::SettingsSectionId;

    let mut harness = open_unread(available(), cx);
    // Showing Servers begins the first version read, which observes the installed server.
    harness.update.read_before_change();
    harness.window.update(harness.cx, |settings, cx| {
        settings.select_section(SettingsSectionId::Machines, cx);
    });
    // The status item uninstalls the server while that read is still pending.
    harness.update.change(|facts| {
        facts.status = Ok(uninstalled());
        facts.approved = None;
    });
    harness.service.report(Ok(uninstalled()));
    harness
        .cx
        .update(|_, cx| crate::status_menu::operation_succeeded(cx, uninstalled()));
    harness.cx.run_until_parked();

    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Not installed");
    assert!(
        !presentation.actions.contains(&UpdateAction::Update),
        "a removed server offers no update"
    );
}

/// The row records a confirmed restart into the Pending Version by how its lane operation ended.
#[test]
fn a_confirmed_restart_is_recorded_by_how_its_lane_operation_ended() {
    use crate::machine_connection::service::RetainedInstallation;
    use crate::machine_connection::service::lane::client::Outcome as LaneOutcome;
    use crate::machine_connection::service::lane::{LaneAction, LaneOperationState, LaneReceipt};
    use crate::machine_connection::update::facts::StartFailure;
    let installation = |version: &str| RetainedInstallation {
        directory: std::path::PathBuf::from(format!("/installations/{version}")),
        version: version.into(),
        commit: "c".repeat(40),
    };
    let settled = |state| LaneOutcome::Settled {
        receipt: LaneReceipt {
            operation: "0a".repeat(16),
            action: LaneAction::Restart,
            state,
            code: None,
            boot: "b".repeat(64),
        },
        boot: "a".repeat(64),
    };
    let phase = |outcome: &LaneOutcome| {
        super::restarted(outcome, installation("1.0.0"), installation("1.1.0"))
            .map(|status| status.phase)
    };
    assert_eq!(
        phase(&settled(LaneOperationState::Done)),
        Ok(ActivationPhase::Completed { running: true })
    );
    assert_eq!(
        phase(&settled(LaneOperationState::Failed)),
        Ok(ActivationPhase::Returned {
            failure: StartFailure::NotReady,
            running: true
        })
    );
    assert_eq!(
        phase(&settled(LaneOperationState::NotPerformed)),
        Err(CoreError::Changed)
    );
    assert_eq!(
        phase(&LaneOutcome::Unconfirmed {
            operation: "0a".repeat(16),
            boot: "a".repeat(64),
            last: None,
        }),
        Ok(ActivationPhase::Uncertain)
    );
    let status = super::restarted(
        &settled(LaneOperationState::Done),
        installation("1.0.0"),
        installation("1.1.0"),
    )
    .unwrap();
    assert_eq!(status.operation[..16], [0x0a; 16]);
    assert_eq!(status.candidate.version, "1.1.0");
    assert_eq!(status.previous.version, "1.0.0");
}

#[test]
fn lane_failures_keep_the_rows_classifications() {
    use crate::machine_connection::service::lane::client::LaneError;
    for (code, error) in [
        ("service_busy", CoreError::ServiceBusy),
        ("changed", CoreError::Changed),
        ("not_running", CoreError::NotRunning),
        ("version_skipped", CoreError::CandidateMissing),
        ("candidate_unverified", CoreError::RuntimeUnavailable),
        ("disk_full", CoreError::StorageUnavailable),
    ] {
        assert_eq!(
            super::lane_error(LaneError::Code(code.into())),
            error,
            "{code}"
        );
    }
    assert_eq!(
        super::lane_error(LaneError::Core(CoreError::Protocol)),
        CoreError::Protocol
    );
}
