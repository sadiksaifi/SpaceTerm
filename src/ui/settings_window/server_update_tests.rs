//! The Server version row and its Update Server journey, driven through a scripted service.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use super::testing::{Outcome, ScriptedUpdate, UpdateCall, approved, available, included};
use super::{
    FINISH_SELECTOR, IncludedServer, RETURN_CANCEL_SELECTOR, RETURN_CONFIRM_SELECTOR,
    RETURN_SELECTOR, ServerUpdateControl, UPDATE_CANCEL_SELECTOR, UPDATE_CONFIRM_SELECTOR,
    UPDATE_SELECTOR, UpdateAction, UpdateFacts, UpdatePresentation, UpdateState, UpdateStep,
    present_update,
};
use crate::appearance::Appearance;
use crate::local_server::ServerError;
use crate::local_server::service::activation::{ActivationPhase, ActivationStatus};
use crate::local_startup::CandidateError;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::settings_window::SettingsWindow;
use crate::ui::settings_window::servers::testing::{
    ScriptedServer, ScriptedService, connected, not_installed, stopped,
};
use crate::ui::settings_window::servers::{
    LocalServerControl, OfferedServer, ServerServiceControl,
};
use crate::ui::sidebar_window::form::CaptionTone;

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
    click(NAVIGATION_SELECTOR, cx);
    Harness {
        window,
        update,
        service,
        server,
        cx,
    }
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
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
    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Update Available");
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

    click(UPDATE_SELECTOR, harness.cx);
    assert!(alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);

    click_modal(UPDATE_CONFIRM_SELECTOR, harness.cx);
    assert_eq!(
        harness.update.changes(),
        [UpdateCall::PrepareUpdate, UpdateCall::Activate]
    );
    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Up to Date");
    assert!(presentation.actions.is_empty());
    // The replaced server is read afresh by the other rows.
    assert!(harness.server.refreshes() >= 2);
    assert!(harness.service.changes().is_empty());
}

#[gpui::test]
fn cancelling_the_confirmation_changes_nothing(cx: &mut TestAppContext) {
    let mut harness = open(available(), cx);
    click(UPDATE_SELECTOR, harness.cx);
    click_modal(UPDATE_CANCEL_SELECTOR, harness.cx);

    assert_eq!(harness.update.changes(), [UpdateCall::PrepareUpdate]);
    let presentation = harness.presentation();
    assert_eq!(presentation.state, "Update Available");
    assert_eq!(presentation.actions, [UpdateAction::Update]);
}

#[test]
fn the_confirmation_names_both_builds_and_every_clients_sessions() {
    let prepared = |running, stopped| super::PreparedChange {
        change: crate::local_server::service::control::UpdateChange::Update,
        previous: approved(),
        candidate: included(),
        running,
        stopped,
        commit: Box::new(|| Err(ServerError::Changed)),
    };
    let live = super::UpdateConfirmation::of(&prepared(true, false));
    assert_eq!(live.title, "Update Server");
    assert!(live.message.contains("version 1.5.0 (commit bbbbbbb)"));
    assert!(live.message.contains("end all of its Terminal Sessions"));
    assert!(live.detail.contains("from version 1.4.0 (commit aaaaaaa)"));
    assert!(
        live.detail.starts_with(&format!(
            "This is the {} server for this account on this computer. ",
            crate::application_identity::ApplicationIdentity::current().display_name()
        )),
        "{}",
        live.detail
    );
    assert!(live.detail.contains("including those other clients use"));
    assert!(
        live.detail
            .ends_with("Other servers aren\u{2019}t affected.")
    );

    let parked = super::UpdateConfirmation::of(&prepared(false, true));
    assert!(parked.message.contains("stopped"), "{}", parked.message);
    assert!(!parked.message.contains("end all"), "{}", parked.message);
    assert!(
        parked
            .detail
            .contains("stays stopped until you choose Start Server"),
        "{}",
        parked.detail
    );
}

#[test]
fn builds_of_one_version_and_commit_are_told_apart_by_their_package() {
    use crate::local_server::service::control::{BuildName, build_labels};
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

    assert_eq!(harness.presentation().state, "Up to Date");
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
        presentation.explanation.contains("confirm again"),
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
    assert_eq!(presentation.state, "Update Failed");
    assert!(
        presentation.explanation.contains("never became ready"),
        "{}",
        presentation.explanation
    );
    assert_eq!(
        presentation.actions,
        [UpdateAction::Return, UpdateAction::Check]
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
    assert!(unproven.explanation.contains("can\u{2019}t return"));
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
    assert_eq!(presentation.state, "Update Unfinished");
    assert_eq!(presentation.actions, [UpdateAction::Finish]);

    click(FINISH_SELECTOR, harness.cx);
    assert!(!alert_open(UPDATE_CONFIRM_SELECTOR, harness.cx));
    assert_eq!(harness.update.changes(), [UpdateCall::Finish]);

    let uncertain = presented(UpdateFacts {
        status: Err(ServerError::ActivationUnresolved),
        activation: Some(activation(ActivationPhase::Uncertain, false)),
        ..available()
    });
    assert_eq!(uncertain.state, "Update Uncertain");
    assert_eq!(uncertain.actions, [UpdateAction::Finish]);
}

#[test]
fn an_approved_or_unavailable_build_offers_no_update() {
    let mut current = available();
    if let Ok(included) = &mut current.included {
        included.approved = true;
    }
    let presentation = presented(current);
    assert_eq!(presentation.state, "Up to Date");
    assert!(presentation.actions.is_empty());

    for (error, state) in [
        (CandidateError::Unavailable, "No Update"),
        (CandidateError::ClientRestartRequired, "Restart SpaceTerm"),
    ] {
        let presentation = presented(UpdateFacts {
            included: Err(error),
            ..available()
        });
        assert_eq!(presentation.state, state);
        assert!(presentation.actions.is_empty(), "{error:?}");
        assert!(
            presentation.explanation.contains("1.4.0"),
            "{}",
            presentation.explanation
        );
    }

    // An earlier release is never offered, as activation refuses it.
    let mut earlier = available();
    if let Ok(included) = &mut earlier.included {
        included.earlier = true;
    }
    let presentation = presented(earlier);
    assert_eq!(presentation.state, "No Update");
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
    assert_eq!(unverified.state, "Not Verified");
    assert_eq!(unverified.actions, [UpdateAction::Check]);

    let uninstalled = presented(UpdateFacts {
        status: Ok(not_installed()),
        approved: None,
        ..available()
    });
    assert_eq!(uninstalled.state, "Not Installed");
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
    assert!(rendered(super::UPDATE_CONTROL_SELECTOR, harness.cx));
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
    assert_eq!(presentation.state, "Up to Date");
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
fn updating_a_stopped_server_after_settings_reopens_keeps_it_stopped(cx: &mut TestAppContext) {
    let mut facts = available();
    facts.status = Ok(stopped());
    let (presentation, failure, calls) =
        confirmed_update_after_reopening(facts, Outcome::Ready, cx);
    assert_eq!(presentation.state, "Up to Date");
    assert!(presentation.explanation.contains("stopped"));
    assert_eq!(failure, None);
    assert_eq!(calls, [UpdateCall::PrepareUpdate, UpdateCall::Activate]);
}
