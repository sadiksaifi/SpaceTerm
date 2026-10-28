//! The Server row's Update Server journey, driven through a scripted service.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};

use super::testing::{Outcome, ScriptedUpdate, UpdateCall, approved, available, included};
use super::{
    FINISH_SELECTOR, IncludedServer, RETURN_CANCEL_SELECTOR, RETURN_CONFIRM_SELECTOR,
    RETURN_SELECTOR, ServerUpdateControl, UPDATE_CANCEL_SELECTOR, UPDATE_CHECK_SELECTOR,
    UPDATE_CONFIRM_SELECTOR, UPDATE_SELECTOR, UpdateAction, UpdateFacts, UpdatePresentation,
    UpdateState, UpdateStep, present_update,
};
use crate::appearance::Appearance;
use crate::local_startup::CandidateError;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::server_connection::ServerError;
use crate::server_connection::service::activation::{ActivationPhase, ActivationStatus};
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::settings_window::SettingsWindow;
use crate::ui::settings_window::servers::testing::{
    ScriptedServer, ScriptedService, connected, not_installed, stopped, uninstalled,
};
use crate::ui::settings_window::servers::{
    LocalServerControl, OfferedServer, ServerServiceControl,
};
use crate::ui::sidebar_window::{SidebarOwner, form::CaptionTone};

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";

struct Harness<'a> {
    window: Entity<SettingsWindow>,
    update: Arc<ScriptedUpdate>,
    service: Arc<ScriptedService>,
    server: Arc<ScriptedServer>,
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
    let server = ScriptedServer::new(Ok(connected(1)));
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let offered = OfferedServer {
        connection: Ok(Arc::clone(&server) as Arc<dyn LocalServerControl>),
        service: Some(Ok(Arc::clone(&service) as Arc<dyn ServerServiceControl>)),
        update: Some(Arc::clone(&update) as Arc<dyn ServerUpdateControl>),
    };
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            None,
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
        server,
        cx,
    }
}

/// Clicks `selector` on the page, or in the Server row's ⋯ menu, where every step but the row's
/// one button lives.
fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let menu = super::super::server_card::MENU_SELECTOR;
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
                .server_update_presentation()
                .expect("the version row should be offered")
        })
    }
}

fn activation(phase: ActivationPhase, returned: bool) -> ActivationStatus {
    ActivationStatus {
        operation: [7; 32],
        previous: approved(),
        candidate: included(),
        stopped: false,
        phase,
        returned,
    }
}

fn presented(facts: UpdateFacts) -> UpdatePresentation {
    present_update(&UpdateState {
        facts: Some(facts),
        failure: None,
        working: None,
    })
}

#[gpui::test]
fn dropdown_local_update_opens_the_existing_confirmation_without_activation(
    cx: &mut TestAppContext,
) {
    let harness = open(available(), cx);
    harness.cx.update(|window, cx| {
        cx.set_global(super::super::OpenSettingsWindow(
            window.window_handle().downcast::<SettingsWindow>().unwrap(),
        ));
        crate::ui::updates::server_rows::ServerUpdateAction::Local.activate(cx);
    });
    harness.cx.run_until_parked();
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);
    assert!(alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    click_modal(UPDATE_CANCEL_SELECTOR, harness.cx);
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);
    assert!(harness.service.changes().is_empty());
}

#[gpui::test]
fn a_confirmed_update_replaces_the_running_server_once(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    let inventory_refreshes = Rc::new(std::cell::Cell::new(0));
    let (inventory, _observation) = harness.cx.update(|_, cx| {
        crate::ui::updates::server_rows::testing::install_up_to_date(cx);
        let inventory = cx.new(crate::ui::updates::server_rows::ServerUpdates::new);
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
            .contains("includes server version 1.5.0 (commit bbbbbbb)"),
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
    assert!(harness.server.refreshes() >= 2);
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
    click(UPDATE_SELECTOR, harness.cx);
    click_modal(UPDATE_CANCEL_SELECTOR, harness.cx);

    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);
    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Update available");
    assert_eq!(presentation.actions, [UpdateAction::Update]);
}

#[test]
fn the_confirmation_names_the_candidate_and_every_clients_sessions() {
    let prepared = |running, stopped| super::PreparedChange {
        change: crate::server_connection::service::control::UpdateChange::Update,
        previous: approved(),
        candidate: included(),
        running,
        stopped,
        commit: Box::new(|| Err(ServerError::Changed)),
    };
    let live = super::UpdateConfirmation::of(&prepared(true, false));
    assert_eq!(live.title, "Update Server");
    let server = crate::application_identity::ApplicationIdentity::current().display_name();
    assert_eq!(
        live.message.as_ref(),
        format!("Update the {server} server to 1.5.0 (commit bbbbbbb)?")
    );
    assert_eq!(
        live.detail.as_ref(),
        "All Terminal Sessions on it end for every client."
    );
    assert_eq!(live.confirm, "Update Server");

    let parked = super::UpdateConfirmation::of(&prepared(false, true));
    assert_eq!(
        parked.message.as_ref(),
        format!("Update the stopped {server} server to 1.5.0 (commit bbbbbbb)?")
    );
    assert!(parked.detail.is_empty());
}

#[test]
fn builds_of_one_version_and_commit_are_told_apart_by_their_package() {
    use crate::server_connection::service::control::{BuildName, build_labels};
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
fn updating_a_stopped_server_keeps_it_stopped(cx: &mut TestAppContext) {
    let mut facts = available();
    facts.status = Ok(stopped());
    let mut harness = open(facts, cx);
    assert!(
        harness
            .presentation()
            .explanation
            .contains("Updating keeps the server stopped")
    );

    click(UPDATE_SELECTOR, harness.cx);
    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);

    assert_eq!(harness.presentation().state, "Up to date");
    assert!(
        harness
            .presentation()
            .explanation
            .contains("you stopped the server")
    );
    // Nothing started the server, and the update reached no Workspace Window.
    assert!(harness.service.changes().is_empty());
}

#[gpui::test]
fn a_changed_server_needs_a_fresh_confirmation(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    harness.update.answer(Outcome::Changed);
    click(UPDATE_SELECTOR, harness.cx);
    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);

    let presentation = harness.presentation();
    assert_eq!(presentation.tone, CaptionTone::Error);
    assert!(
        presentation
            .explanation
            .contains("The server changed. Review the update again."),
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
fn a_failed_update_offers_return_only_when_the_service_proves_it_safe(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    harness.update.answer(Outcome::Failed { can_return: true });
    click(UPDATE_SELECTOR, harness.cx);
    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);

    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Update failed");
    assert!(
        presentation.explanation.contains("didn\u{2019}t start"),
        "{}",
        presentation.explanation
    );
    assert_eq!(
        presentation.actions,
        [UpdateAction::Return, UpdateAction::Check]
    );
    assert_eq!(
        spaceterm_ui::a11y_testing::A11yTree::read(harness.cx).node("Restore 1.4.0\u{2026}")["aria"]
            ["role"],
        "Button",
        "the safe return is the row's one button and names the version it returns to"
    );

    click(RETURN_SELECTOR, harness.cx);
    click_modal(RETURN_CANCEL_SELECTOR, harness.cx);
    assert!(!harness.update.changes().contains(&UpdateCall::Return));

    click(RETURN_SELECTOR, harness.cx);
    click_modal(RETURN_CONFIRM_SELECTOR, harness.cx);
    assert_eq!(
        harness.update.changes(),
        [
            UpdateCall::PrepareUpdate,
            UpdateCall::Activate,
            UpdateCall::PrepareReturn,
            UpdateCall::PrepareReturn,
            UpdateCall::Return
        ]
    );
    let presentation = harness.presentation();
    assert!(
        presentation
            .explanation
            .contains("You returned from version"),
        "{}",
        presentation.explanation
    );
    // Returning leaves the server stopped; only an explicit Start runs it again.
    assert!(harness.service.changes().is_empty());

    let unproven = presented(UpdateFacts {
        status: Ok(stopped()),
        activation: Some(activation(
            ActivationPhase::Failed { can_return: false },
            false,
        )),
        ..available()
    });
    assert_eq!(unproven.actions, [UpdateAction::Check]);
    assert!(
        unproven
            .explanation
            .contains("SpaceTerm can\u{2019}t safely restore 1.4.0 (commit aaaaaaa).")
    );
}

#[gpui::test]
fn an_unresolved_update_finishes_without_asking_again(cx: &mut TestAppContext) {
    let mut harness = open(
        UpdateFacts {
            // The status read refuses while the update is unresolved.
            status: Err(ServerError::ActivationUnresolved),
            activation: Some(activation(ActivationPhase::Activating, false)),
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
        status: Err(ServerError::ActivationUnresolved),
        activation: Some(activation(ActivationPhase::Uncertain, false)),
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
                "Quit and reopen SpaceTerm to update the server."
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
        included: Err(CandidateError::Verification(
            ServerError::RuntimeUnavailable,
        )),
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
    harness.update.refuse_next(ServerError::CandidateChanged);
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
        settings.request_server_update(cx);
    });
    harness.cx.run_until_parked();
    assert!(alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);

    // A row that no longer offers an update drops the request and asks nothing.
    click_modal(UPDATE_CANCEL_SELECTOR, harness.cx);
    harness.update.change(|facts| {
        if let Ok(IncludedServer { approved, .. }) = &mut facts.included {
            *approved = true;
        }
    });
    harness
        .window
        .update(harness.cx, |settings, cx| settings.check_server_update(cx));
    harness.cx.run_until_parked();
    harness.window.update(harness.cx, |settings, cx| {
        settings.request_server_update(cx);
    });
    harness.cx.run_until_parked();
    assert!(!alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);
    assert!(rendered(
        super::super::server_card::STATUS_SELECTOR,
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
) -> (UpdatePresentation, Option<ServerError>, Vec<UpdateCall>) {
    let (update, service, server) = {
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
        (harness.update, harness.service, harness.server)
    };
    let offered = OfferedServer {
        connection: Ok(server as Arc<dyn LocalServerControl>),
        service: Some(Ok(service as Arc<dyn ServerServiceControl>)),
        update: Some(Arc::clone(&update) as Arc<dyn ServerUpdateControl>),
    };
    let (settings, visual) = cx.add_window_view(|window, cx| {
        let mut settings = SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            None,
            Some(offered),
            window,
            cx,
        );
        assert!(
            settings
                .server_update
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
        let row = settings.server_update.as_ref().unwrap();
        (row.presentation(), row.state.failure, update.changes())
    })
}

#[gpui::test]
fn a_confirmed_update_finishes_after_settings_reopens(cx: &mut TestAppContext) {
    let (presentation, failure, calls) =
        confirmed_update_after_reopening(available(), Outcome::Ready, cx);
    assert_eq!(presentation.state, "Up to date");
    assert_eq!(failure, None);
    assert_eq!(calls, [UpdateCall::PrepareUpdate, UpdateCall::Activate]);
}

#[gpui::test]
fn a_confirmed_update_keeps_its_error_after_settings_reopens(cx: &mut TestAppContext) {
    let (_, failure, calls) = confirmed_update_after_reopening(available(), Outcome::Changed, cx);
    assert_eq!(failure, Some(ServerError::Changed));
    assert_eq!(calls, [UpdateCall::PrepareUpdate, UpdateCall::Activate]);
}

#[gpui::test]
fn a_successful_update_does_not_keep_a_transient_followup_read_failure(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    click(UPDATE_SELECTOR, harness.cx);
    harness
        .update
        .refuse_next_read(ServerError::StorageUnavailable);
    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);
    assert_eq!(harness.presentation().state, "Unknown");
    harness.window.update(harness.cx, |settings, cx| {
        settings.check_server_update(cx);
    });
    harness.cx.run_until_parked();
    assert_eq!(harness.presentation().state, "Up to date");
    assert_eq!(
        harness.window.read_with(harness.cx, |settings, _| {
            settings.server_update.as_ref().unwrap().state.failure
        }),
        None,
        "a successful reread must not claim the confirmed update failed"
    );
    assert_eq!(
        harness.update.changes(),
        [UpdateCall::PrepareUpdate, UpdateCall::Activate]
    );
}

#[gpui::test]
fn updating_a_stopped_server_after_settings_reopens_keeps_it_stopped(cx: &mut TestAppContext) {
    let mut facts = available();
    facts.status = Ok(stopped());
    let (presentation, failure, calls) =
        confirmed_update_after_reopening(facts, Outcome::Ready, cx);
    assert_eq!(presentation.state, "Up to date");
    assert!(presentation.explanation.contains("stopped"));
    assert_eq!(failure, None);
    assert_eq!(calls, [UpdateCall::PrepareUpdate, UpdateCall::Activate]);
}

/// Check Now checks for a newer SpaceTerm first, because server builds ship inside it, and reads
/// the server only once that check settles. The server reads as checking the whole time.
#[gpui::test]
fn check_now_reads_the_server_after_spaceterm_finishes_checking(cx: &mut TestAppContext) {
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
                .server_state(&super::super::server_list::ServerChoice::Local)
                .checking
        })
    };

    click(super::super::server_card::MENU_SELECTOR, harness.cx);
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
/// update's own record: the subtitle says what didn't update, and the safe return names the
/// version it returns to.
#[gpui::test]
fn a_failed_update_names_its_builds_from_its_own_record(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let harness = open(
        UpdateFacts {
            status: Err(ServerError::ActivationUnresolved),
            approved: None,
            activation: Some(activation(
                ActivationPhase::Failed { can_return: true },
                false,
            )),
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
    assert_eq!(tree.node("Restore 1.4.0\u{2026}")["aria"]["role"], "Button");
    assert!(rendered(RETURN_SELECTOR, harness.cx));
}

/// Returning to Servers reads the update again. While that read runs, the failed update's
/// Restore is unavailable rather than taking a click it would drop. Once the read finishes, it
/// asks to restore.
#[gpui::test]
fn restore_waits_for_the_update_read_after_returning_to_servers(cx: &mut TestAppContext) {
    let harness = open(
        UpdateFacts {
            status: Ok(stopped()),
            activation: Some(activation(
                ActivationPhase::Failed { can_return: true },
                false,
            )),
            ..available()
        },
        cx,
    );
    // Reads the published tree as the last frame drew it, without running background work.
    let restore_disabled = |cx: &mut VisualTestContext| {
        let tree: serde_json::Value = cx.update(|window, _| {
            serde_json::from_str(&window.debug_a11y_tree_json().expect("a published tree"))
                .expect("a valid tree")
        });
        tree["nodes"]
            .as_object()
            .expect("published nodes")
            .values()
            .find(|node| node["aria"]["label"] == "Restore 1.4.0\u{2026}")
            .expect("Restore is the row's button")["aria"]["disabled"]
            == true
    };
    spaceterm_ui::a11y_testing::A11yTree::read(harness.cx);
    assert!(
        !restore_disabled(harness.cx),
        "a settled read offers Restore"
    );

    harness.window.update(harness.cx, |settings, cx| {
        settings.select_section(super::super::SettingsSectionId::Interface, cx);
        settings.select_section(super::super::SettingsSectionId::Servers, cx);
    });
    assert_eq!(
        harness.window.read_with(harness.cx, |settings, _| {
            settings.server_update.as_ref().unwrap().state.working
        }),
        Some(UpdateStep::Check),
        "the update read is held"
    );
    assert!(restore_disabled(harness.cx), "Restore waits for the read");

    harness.cx.run_until_parked();
    assert!(
        !restore_disabled(harness.cx),
        "the finished read offers Restore"
    );
    click(RETURN_SELECTOR, harness.cx);
    assert!(alert_open(RETURN_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareReturn]);
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
        settings.select_section(SettingsSectionId::Servers, cx);
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
