//! The remote maintenance presentation, driven through the owner's states alone.

use super::*;
use crate::local_server::service::ServiceStartup;
use crate::local_server::service::maintenance::RemoteActivation;
use crate::ssh::server_operations::RemoteOperationHandle;

fn remote_build(version: &str, digit: char) -> RemoteBuild {
    RemoteBuild {
        version: version.to_owned(),
        commit: std::iter::repeat_n(digit, 40).collect(),
        digest: std::iter::repeat_n(digit, 64).collect(),
    }
}

const fn status(installed: bool, running: bool, stopped: bool) -> ServiceStatus {
    ServiceStatus {
        installed,
        enabled: running,
        running,
        stopped,
    }
}

fn facts(status: ServiceStatus) -> MaintenanceFacts {
    MaintenanceFacts {
        server: [2; 32],
        account: 501,
        build: "spaceterm-development".to_owned(),
        status,
        startup: ServiceStartup::AtLogin,
        approved: Some(remote_build("1.4.0", 'a')),
        activation: None,
    }
}

fn change(running: bool, stopped: bool) -> PreparedChange {
    PreparedChange {
        confirmation: "an exact-observation confirmation".to_owned(),
        running,
        stopped,
        startup: ServiceStartup::AtLogin,
        previous: Some(remote_build("1.4.0", 'a')),
        candidate: Some(remote_build("1.5.0", 'b')),
    }
}

/// A validated result the service reported, as it survives relaunch.
fn completed(after: Option<ServiceStatus>) -> RetainedOutcome {
    RetainedOutcome::Completed {
        status: after,
        build: None,
    }
}

fn service(machine: &str, state: &RemoteOperationState) -> MaintenancePresentation {
    present(machine, state, MaintenanceRow::Service, None)
}

fn update(machine: &str, state: &RemoteOperationState) -> MaintenancePresentation {
    present(machine, state, MaintenanceRow::Update, None)
}

/// Opening the Servers section reaches no computer, so the row says what it has not read and
/// offers the one explicit read. Reading starts nothing.
#[test]
fn an_unread_server_offers_only_a_read_that_starts_nothing() {
    let unread = RemoteOperationState::Unread;

    let service = service("penguin", &unread);
    assert_eq!(service.actions, vec![RemoteAction::Check]);
    assert!(service.explanation.contains("penguin"));
    assert!(
        service.explanation.contains("starts nothing"),
        "the read must promise it starts nothing, got {:?}",
        service.explanation
    );
    assert!(!service.working);
    let update = update("penguin", &unread);
    assert_eq!(update.state, None, "the update row reports nothing yet");
    assert!(update.actions.is_empty());
}

/// Only explicit Start reverses a deliberate Stop, so a stopped server offers Start and no other
/// step that would start it. Updating it leaves it stopped.
#[test]
fn a_stopped_server_offers_start_and_no_other_step_that_starts_it() {
    let stopped = RemoteOperationState::Read(Box::new(facts(status(true, false, true))));

    let service = service("penguin", &stopped);
    assert_eq!(
        service.actions,
        vec![
            RemoteAction::Start,
            RemoteAction::Uninstall,
            RemoteAction::Check
        ]
    );
    assert_eq!(service.state, Some("Stopped"));
    assert!(service.explanation.contains("Start Server"));
    let update = update("penguin", &stopped);
    assert_eq!(update.actions, vec![RemoteAction::Update]);
    assert!(
        update.explanation.contains("keeps the server stopped"),
        "updating a stopped server must say it stays stopped, got {:?}",
        update.explanation
    );
}

/// A running server offers the steps that end its work, each of which asks first.
#[test]
fn a_running_server_offers_the_steps_that_ask_before_ending_its_work() {
    let running = RemoteOperationState::Read(Box::new(facts(status(true, true, false))));

    let service = service("penguin", &running);
    assert_eq!(
        service.actions,
        vec![
            RemoteAction::Stop,
            RemoteAction::Restart,
            RemoteAction::Uninstall,
            RemoteAction::Check
        ]
    );
    for action in [
        RemoteAction::Stop,
        RemoteAction::Restart,
        RemoteAction::Uninstall,
    ] {
        assert!(
            action.request().is_some(),
            "{action:?} must describe its change and ask first"
        );
    }
    assert!(
        RemoteAction::Start.request().is_none(),
        "Start ends no work, so it asks nothing"
    );
    assert_eq!(
        update("penguin", &running).actions,
        vec![RemoteAction::Update]
    );
}

/// Installing a first server belongs to Connect, which explains what it does on that computer.
/// Maintenance offers no step for a machine with no server, and nothing to update.
#[test]
fn a_machine_without_a_server_offers_no_maintenance_step() {
    let bare = RemoteOperationState::Read(Box::new(facts(status(false, false, false))));

    let service = service("penguin", &bare);
    assert_eq!(service.actions, vec![RemoteAction::Check]);
    assert_eq!(service.state, Some("Not Installed"));
    let update = update("penguin", &bare);
    assert!(update.actions.is_empty());
    assert!(update.explanation.contains("nothing to update"));
}

/// One operation is underway at a time, and one row reports it. The other row says an operation
/// is underway and offers nothing, so two rows never claim the same work.
#[test]
fn only_the_row_that_owns_an_operation_reports_its_progress() {
    let stopping = RemoteOperationState::Working {
        phase: MaintenancePhase::Dispatching,
        kind: Some(SubmittedKind::Service(Action::Stop)),
        cancellable: false,
    };

    let service = service("penguin", &stopping);
    assert!(service.working);
    assert_eq!(service.state, Some("Stopping"));
    assert!(service.explanation.contains("Terminal Sessions end"));
    let update = update("penguin", &stopping);
    assert!(!update.working && update.state.is_none() && update.actions.is_empty());

    // A described update is still being prepared, so its row is named by what this surface asked.
    let transferring = RemoteOperationState::Working {
        phase: MaintenancePhase::Transferring,
        kind: None,
        cancellable: true,
    };
    let asked = present(
        "penguin",
        &transferring,
        MaintenanceRow::Update,
        Some(RemoteMaintenanceRequest::Update),
    );
    assert!(asked.working && asked.actions == vec![RemoteAction::Cancel]);
    assert!(
        !present(
            "penguin",
            &transferring,
            MaintenanceRow::Service,
            Some(RemoteMaintenanceRequest::Update)
        )
        .working
    );
}

/// A submitted operation whose answer was lost stays unconfirmed. The row says so and offers to
/// ask the machine for the answer, never to repeat the operation. Asking may never produce one,
/// so it also offers to stop following it.
#[test]
fn an_unconfirmed_operation_only_asks_the_machine_again() {
    let lost = RemoteOperationState::Unconfirmed {
        kind: SubmittedKind::Service(Action::Stop),
    };

    let service = service("penguin", &lost);
    assert_eq!(service.state, Some("Unconfirmed"));
    assert_eq!(
        service.actions,
        vec![RemoteAction::CheckAgain, RemoteAction::Discard]
    );
    assert!(
        service
            .explanation
            .contains("didn\u{2019}t hear whether penguin stopped its server")
    );
    assert!(
        service
            .explanation
            .contains("isn\u{2019}t asked to do it again"),
        "the row must promise the operation is not repeated, got {:?}",
        service.explanation
    );

    let lost_update = RemoteOperationState::Unconfirmed {
        kind: SubmittedKind::Update {
            candidate: "b".repeat(64),
        },
    };
    assert_eq!(
        update("penguin", &lost_update).actions,
        vec![RemoteAction::Finish, RemoteAction::Discard]
    );
}

/// A validated result is an everyday thing to read and dismiss, not an operation to manage.
#[test]
fn a_validated_result_is_acknowledged_and_nothing_else() {
    let stopped = RemoteOperationState::Resolved {
        handle: RemoteOperationHandle(3),
        kind: SubmittedKind::Service(Action::Stop),
        outcome: completed(Some(status(true, false, true))),
    };

    let service = service("penguin", &stopped);
    assert_eq!(service.actions, vec![RemoteAction::Acknowledge]);
    assert_eq!(service.state, Some("Stopped"));
    assert!(service.explanation.contains("penguin"));
    assert!(
        service
            .explanation
            .contains("Terminal Sessions ended for every client")
    );
    assert_eq!(RemoteAction::Acknowledge.label(), "OK");
}

/// A refusal reaches the person as a refusal, with nothing changed, and never as a result.
#[test]
fn a_refusal_says_nothing_changed() {
    let refused = RemoteOperationState::Resolved {
        handle: RemoteOperationHandle(4),
        kind: SubmittedKind::Service(Action::Restart),
        outcome: RetainedOutcome::Refused(MaintenanceRefusal::Stopped),
    };

    let service = service("penguin", &refused);
    assert_eq!(service.state, Some("Refused"));
    assert_eq!(service.tone, CaptionTone::Error);
    assert!(service.explanation.contains("Nothing changed on penguin"));
    assert!(service.explanation.contains("Start Server"));
    assert_eq!(service.actions, vec![RemoteAction::Acknowledge]);
}

/// A refusal or an incompatibility that ends an attempt to resolve an operation already
/// dispatched says nothing about what that operation did, and keeps the one step that settles it.
/// Only a state with nothing submitted promises that nothing changed.
#[test]
fn a_refused_resolution_keeps_the_original_it_was_resolving() {
    for failure in [
        SshMaintenanceFailure::Refused(MaintenanceRefusal::Unsupported),
        SshMaintenanceFailure::Incompatible(IncompatibilityKind::Capability),
    ] {
        let state = |kind| RemoteOperationState::Failed { failure, kind };
        let stop = state(Some(SubmittedKind::Service(Action::Stop)));

        let submitted = service("penguin", &stop);
        assert_eq!(
            submitted.actions,
            vec![RemoteAction::CheckAgain],
            "{failure:?} must keep the step that resolves the operation it was resolving"
        );
        assert!(
            !submitted.explanation.contains("Nothing changed"),
            "{failure:?} must not assert what a dispatched operation did, got {:?}",
            submitted.explanation
        );
        assert!(
            submitted
                .explanation
                .contains("can\u{2019}t tell whether penguin stopped its server"),
            "{failure:?} must say the result is still unknown, got {:?}",
            submitted.explanation
        );
        assert!(
            submitted
                .explanation
                .contains("isn\u{2019}t asked to do it again"),
            "{failure:?} must promise the operation is not repeated, got {:?}",
            submitted.explanation
        );
        assert_eq!(
            update("penguin", &state(Some(SubmittedKind::Return))).actions,
            vec![RemoteAction::Finish],
            "{failure:?} keeps the update journey's own resolving step"
        );
        // The step resolves the original without reaching the connection, so a removed
        // connection holding this state stays listed with it.
        assert_eq!(
            removed_actions(&stop, MaintenanceRow::Service),
            vec![RemoteAction::CheckAgain],
            "{failure:?} must keep a removed connection's record reachable"
        );

        // Nothing was submitted, so the refusal is the whole story and a read is the only step.
        let bare = service("penguin", &state(None));
        assert_eq!(bare.actions, vec![RemoteAction::Check]);
        assert_ne!(
            bare.state_selector, submitted.state_selector,
            "{failure:?} with an operation submitted reads differently from one without"
        );
    }
    let bare = service(
        "penguin",
        &RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Refused(MaintenanceRefusal::Unsupported),
            kind: None,
        },
    );
    assert!(bare.explanation.contains("Nothing changed on penguin"));
}

/// A failed update offers the return only when the service proved it is safe, and never rolls
/// back on its own.
#[test]
fn a_failed_update_offers_a_return_only_when_it_is_proved_safe() {
    let failed = |can_return| {
        let mut facts = facts(status(true, false, true));
        facts.activation = Some(RemoteActivation {
            operation: [9; 32],
            previous: remote_build("1.4.0", 'a'),
            candidate: remote_build("1.5.0", 'b'),
            stopped: true,
            phase: RemoteActivationPhase::Failed { can_return },
            returned: false,
        });
        RemoteOperationState::Read(Box::new(facts))
    };

    let safe = update("penguin", &failed(true));
    assert_eq!(safe.state, Some("Update Failed"));
    assert_eq!(safe.actions, vec![RemoteAction::Return]);
    assert!(safe.explanation.contains("don\u{2019}t return"));
    let unproved = update("penguin", &failed(false));
    assert!(
        unproved.actions.is_empty(),
        "an unproved return is never offered, got {:?}",
        unproved.actions
    );
    // The service row keeps reporting the server itself, as the local pair of rows does: the
    // server is stopped, and only explicit Start reverses that.
    let service = service("penguin", &failed(true));
    assert_eq!(service.state, Some("Stopped"));
    assert_eq!(
        service.actions,
        vec![
            RemoteAction::Start,
            RemoteAction::Uninstall,
            RemoteAction::Check
        ]
    );
}

/// An unfinished update is completed, never confirmed again.
#[test]
fn an_unfinished_update_is_finished_without_asking_again() {
    let mut facts = facts(status(true, true, false));
    facts.activation = Some(RemoteActivation {
        operation: [9; 32],
        previous: remote_build("1.4.0", 'a'),
        candidate: remote_build("1.5.0", 'b'),
        stopped: false,
        phase: RemoteActivationPhase::Activating,
        returned: false,
    });
    let unfinished = RemoteOperationState::Read(Box::new(facts));

    let update = update("penguin", &unfinished);
    assert_eq!(update.state, Some("Update Unfinished"));
    assert_eq!(update.actions, vec![RemoteAction::Finish]);
    assert!(update.explanation.contains("doesn\u{2019}t ask again"));
    assert!(update.explanation.contains("1.4.0"));
    assert!(update.explanation.contains("1.5.0"));
}

/// Removing the saved connection leaves the submitted operation understandable. Nothing offered
/// there reconnects, reads the machine or starts new work.
#[test]
fn a_removed_connection_offers_only_resolving_and_acknowledging() {
    let states = [
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Return,
        },
        RemoteOperationState::Resolved {
            handle: RemoteOperationHandle(7),
            kind: SubmittedKind::Service(Action::Stop),
            outcome: completed(None),
        },
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
        RemoteOperationState::Unread,
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Refused(MaintenanceRefusal::Unsupported),
            kind: Some(SubmittedKind::Service(Action::Stop)),
        },
        RemoteOperationState::Paused {
            reason: ContinuityChange::HostChanged,
            kind: Some(SubmittedKind::Service(Action::Stop)),
        },
    ];

    for state in &states {
        for row in [MaintenanceRow::Service, MaintenanceRow::Update] {
            for action in removed_actions(state, row) {
                assert!(
                    matches!(
                        action,
                        RemoteAction::Finish
                            | RemoteAction::CheckAgain
                            | RemoteAction::Acknowledge
                            // Discarding sends nothing: it retires this client's own record.
                            | RemoteAction::Discard
                    ),
                    "{action:?} would reach a connection the person removed"
                );
            }
        }
    }
    assert_eq!(
        removed_actions(&states[0], MaintenanceRow::Service),
        vec![RemoteAction::CheckAgain, RemoteAction::Discard]
    );
    assert_eq!(
        removed_actions(&states[1], MaintenanceRow::Update),
        vec![RemoteAction::Finish, RemoteAction::Discard]
    );
    assert_eq!(
        removed_actions(&states[2], MaintenanceRow::Service),
        vec![RemoteAction::Acknowledge]
    );
    assert!(
        removed_actions(&states[3], MaintenanceRow::Service).is_empty(),
        "a removed connection offers no read and no new operation"
    );
    assert_eq!(
        removed_actions(&states[5], MaintenanceRow::Service),
        vec![RemoteAction::CheckAgain],
        "a refused resolution keeps the step that settles the operation it was resolving"
    );
    assert_eq!(
        removed_actions(&states[6], MaintenanceRow::Service),
        vec![RemoteAction::CheckAgain, RemoteAction::Discard],
        "an operation the destination can no longer answer can be settled or stopped following"
    );
}

/// Resolving a removed connection's own original is work the person asked for, so the entry
/// reports its progress rather than disappearing until it settles.
#[test]
fn a_removed_connection_reports_the_work_it_is_doing() {
    let working = RemoteOperationState::Working {
        phase: MaintenancePhase::Authenticating,
        kind: Some(SubmittedKind::Service(Action::Stop)),
        cancellable: true,
    };

    let presentation = removed_presentation("penguin", std::slice::from_ref(&working))
        .expect("a removed connection resolving its original still has something to say");
    assert!(presentation.working, "the entry reports the work underway");
    assert!(
        presentation.actions.is_empty(),
        "nothing underway offers a step that would reach the removed connection"
    );
    assert!(presentation.explanation.contains("penguin"));

    assert!(
        removed_presentation(
            "penguin",
            &[RemoteOperationState::Read(Box::new(facts(status(
                true, true, false
            ))))]
        )
        .is_none(),
        "a removed connection with nothing left to tell is not listed"
    );
}

/// The machine heading's menu carries the operations themselves. The everyday acknowledgement and
/// the cancellation belong to the row that reports the operation.
#[test]
fn the_machine_menu_carries_the_operations_without_inline_affordances() {
    let running = RemoteOperationState::Read(Box::new(facts(status(true, true, false))));

    assert_eq!(
        menu_actions_for(std::slice::from_ref(&running), None),
        vec![
            RemoteAction::Stop,
            RemoteAction::Restart,
            RemoteAction::Update,
            RemoteAction::Uninstall
        ],
        "the menu offers the operations, with removal last"
    );
    let resolved = RemoteOperationState::Resolved {
        handle: RemoteOperationHandle(8),
        kind: SubmittedKind::Service(Action::Stop),
        outcome: completed(None),
    };
    assert!(menu_actions_for(std::slice::from_ref(&resolved), None).is_empty());
    let working = RemoteOperationState::Working {
        phase: MaintenancePhase::Dispatching,
        kind: Some(SubmittedKind::Service(Action::Stop)),
        cancellable: true,
    };
    assert!(
        menu_actions_for(std::slice::from_ref(&working), None).is_empty(),
        "nothing is offered while an operation is underway"
    );
}

/// A waiting read cannot offer a new operation while Settings reports an original in progress.
#[test]
fn the_machine_menu_waits_for_the_original_settings_reports() {
    let underway = RemoteOperationState::Working {
        phase: MaintenancePhase::Dispatching,
        kind: Some(SubmittedKind::Service(Action::Stop)),
        cancellable: true,
    };
    let read = RemoteOperationState::Read(Box::new(facts(status(true, true, false))));

    assert!(
        menu_actions_for(std::slice::from_ref(&underway), None).is_empty(),
        "the first entry alone offers nothing while its operation is underway"
    );
    assert_eq!(
        menu_actions_for(std::slice::from_ref(&underway), None),
        Vec::new(),
        "one underway original still offers nothing"
    );
    assert_eq!(
        menu_actions_for(&[underway, read], None),
        Vec::new(),
        "the menu must not offer steps from the read hidden behind the original"
    );
}

/// Every caption reads as one sentence. A literal split across source lines without a
/// continuation leaves the source indentation in the middle of the sentence.
fn reads_as_one_sentence(explanation: &str) {
    assert!(
        !explanation.contains("  "),
        "a caption runs two spaces together: {explanation:?}"
    );
}

/// Every state a person can land in reads differently from every other, so no two are mistaken
/// for one another.
#[test]
fn every_state_reads_distinctly() {
    let states = [
        RemoteOperationState::Unread,
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
        RemoteOperationState::Read(Box::new(facts(status(true, false, true)))),
        RemoteOperationState::Read(Box::new(facts(status(true, false, false)))),
        RemoteOperationState::Read(Box::new(facts(status(false, false, false)))),
        RemoteOperationState::Working {
            phase: MaintenancePhase::Authenticating,
            kind: None,
            cancellable: true,
        },
        RemoteOperationState::Described {
            handle: RemoteOperationHandle(9),
            request: RemoteMaintenanceRequest::Service(Action::Stop),
            change: Box::new(change(true, false)),
        },
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
        RemoteOperationState::NeedsPerson {
            reason: InteractionReason::Authentication,
            kind: None,
        },
        RemoteOperationState::Paused {
            reason: ContinuityChange::HostChanged,
            kind: None,
        },
        // The same pause holding an operation it can no longer ask about is a different state: it
        // offers the step that settles that operation and the step that stops following it.
        RemoteOperationState::Paused {
            reason: ContinuityChange::HostChanged,
            kind: Some(SubmittedKind::Service(Action::Stop)),
        },
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Unavailable(UnavailableKind::HostUnreachable),
            kind: None,
        },
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Forgotten,
            kind: Some(SubmittedKind::Service(Action::Stop)),
        },
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Cancelled,
            kind: None,
        },
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Incompatible(IncompatibilityKind::Protocol),
            kind: None,
        },
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Refused(MaintenanceRefusal::Unsupported),
            kind: None,
        },
        // The same refusal while an operation was already dispatched is a different state: it
        // cannot promise that nothing changed, and only resolving that operation settles it.
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Refused(MaintenanceRefusal::Unsupported),
            kind: Some(SubmittedKind::Service(Action::Stop)),
        },
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::NotRetained,
            kind: None,
        },
        // A step refused because this client still holds an unresolved original is a different
        // state: it reports that original and offers the step that settles it.
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::NotRetained,
            kind: Some(SubmittedKind::Service(Action::Stop)),
        },
    ];

    let presented: Vec<_> = states
        .iter()
        .map(|state| service("penguin", state))
        .collect();
    for (index, presentation) in presented.iter().enumerate() {
        let state = presentation.state.expect("the service row reports a state");
        assert!(
            presented[..index]
                .iter()
                .all(|earlier| earlier.state != Some(state)
                    || earlier.state_selector != presentation.state_selector),
            "{state} reads the same as an earlier state"
        );
        assert!(
            presentation.explanation.contains("penguin"),
            "{state} must name the machine it reports, got {:?}",
            presentation.explanation
        );
        reads_as_one_sentence(&presentation.explanation);
        reads_as_one_sentence(&update("penguin", &states[index]).explanation);
        assert!(
            !presentation.actions.contains(&RemoteAction::Start)
                || matches!(states[index], RemoteOperationState::Read(_)),
            "{state} must not offer to start a server"
        );
    }
}

/// Each destructive confirmation names the selected machine and says that every client's Terminal
/// Sessions on that server end. An update names both exact versions.
#[test]
fn each_confirmation_names_the_machine_and_the_sessions_that_end() {
    for action in [Action::Stop, Action::Restart, Action::Uninstall] {
        let confirmation = MaintenanceConfirmation::of(
            "penguin",
            RemoteMaintenanceRequest::Service(action),
            &change(true, false),
        )
        .expect("a destructive service change describes itself");
        assert!(
            confirmation.message.contains("penguin"),
            "{action:?} must name the machine, got {:?}",
            confirmation.message
        );
        assert!(
            confirmation.message.contains("Terminal Sessions")
                || confirmation
                    .detail
                    .contains("Terminal Sessions end for every client"),
            "{action:?} must say every client's Terminal Sessions end, got {:?} {:?}",
            confirmation.message,
            confirmation.detail
        );
    }
    let update = MaintenanceConfirmation::of(
        "penguin",
        RemoteMaintenanceRequest::Update,
        &change(true, false),
    )
    .expect("an update describes itself");
    assert_eq!(update.title, "Update Server");
    assert!(update.message.contains("penguin") && update.message.contains("1.5.0"));
    assert!(update.detail.contains("1.4.0") && update.detail.contains("1.5.0"));
    assert!(
        update.detail.contains(
            "All Terminal Sessions on this server end, including those other clients use"
        )
    );
    assert!(update.detail.ends_with("Other servers aren’t affected."));
    let returning = MaintenanceConfirmation::of(
        "penguin",
        RemoteMaintenanceRequest::Return,
        &change(false, true),
    )
    .expect("a return describes itself");
    assert_eq!(returning.title, "Return to Previous Version");
    assert!(returning.message.contains("penguin") && returning.message.contains("1.4.0"));
}

/// Discarding retires the only record that could still say what a submitted operation did, so it
/// asks first, names the machine and says plainly what the person gives up. Nothing is sent.
#[test]
fn discarding_an_operation_names_the_machine_and_what_it_gives_up() {
    let confirmation =
        MaintenanceConfirmation::for_discard("penguin", &SubmittedKind::Service(Action::Stop));

    assert!(
        RemoteAction::Discard.label().ends_with('\u{2026}'),
        "the step's label promises a confirmation"
    );
    assert_eq!(confirmation.confirm, "Discard");
    assert!(
        RemoteAction::Discard
            .consequence("penguin")
            .contains("SpaceTerm asks first."),
        "assistive technology hears that the step asks first"
    );
    assert!(
        confirmation.message.contains("penguin"),
        "the question must name the machine, got {:?}",
        confirmation.message
    );
    assert!(
        confirmation.detail.contains("what the stop did on penguin"),
        "the detail must say what the person gives up, got {:?}",
        confirmation.detail
    );
    assert!(
        confirmation.detail.contains("Nothing is sent to penguin"),
        "the detail must say that nothing is sent, got {:?}",
        confirmation.detail
    );
    let update = MaintenanceConfirmation::for_discard(
        "penguin",
        &SubmittedKind::Update {
            candidate: "b".repeat(64),
        },
    );
    assert!(
        update.detail.contains("what the update did on penguin"),
        "the detail must name the operation it discards, got {:?}",
        update.detail
    );
    for named in [confirmation.message, confirmation.detail, update.detail] {
        assert!(
            !named.contains(&"b".repeat(16)),
            "the copy must carry no operation identity, got {named:?}"
        );
    }
}

/// Updating a stopped server says so instead of promising Sessions end, and leaves it stopped.
#[test]
fn updating_a_stopped_server_leaves_it_stopped() {
    let confirmation = MaintenanceConfirmation::of(
        "penguin",
        RemoteMaintenanceRequest::Update,
        &change(false, true),
    )
    .expect("an update of a stopped server describes itself");

    assert!(confirmation.message.contains("stopped"));
    assert!(
        confirmation.detail.contains("stays stopped"),
        "the detail must say the server stays stopped, got {:?}",
        confirmation.detail
    );
}

/// A connection or a Check Status has already observed whether that account's server runs, so a
/// surface offers what that observation allows before any maintenance read, and says when the
/// observation was made. Only explicit Start still reverses a deliberate Stop.
#[test]
fn an_observation_decides_what_is_offered_before_any_read() {
    use crate::ssh::server_connection::{
        RemoteConnectionStatus, ServerAvailability, SshServerFacts,
    };

    let facts = || SshServerFacts {
        platform: ("Example OS".to_owned(), "x86_64".to_owned()),
        version: Some("1.4.0".to_owned()),
        starts_automatically: true,
    };
    let connected = RemoteConnectionStatus::Connected {
        protocol_major: 1,
        facts: facts(),
    };
    let checked = |availability| RemoteConnectionStatus::Checked {
        availability,
        facts: facts(),
        at: std::time::SystemTime::UNIX_EPOCH,
    };

    let service = observed("penguin", &connected, MaintenanceRow::Service)
        .expect("a connection observes whether the server runs");
    assert_eq!(service.state, Some("Running"));
    assert_eq!(
        service.actions,
        vec![
            RemoteAction::Stop,
            RemoteAction::Restart,
            RemoteAction::Uninstall,
            RemoteAction::Check
        ]
    );
    assert!(
        service
            .explanation
            .contains("when SpaceTerm last reached it"),
        "an earlier observation says when it was made, got {:?}",
        service.explanation
    );
    assert_eq!(
        observed_menu_actions(&connected),
        vec![
            RemoteAction::Stop,
            RemoteAction::Restart,
            RemoteAction::Update,
            RemoteAction::Uninstall
        ]
    );
    let stopped = observed(
        "penguin",
        &RemoteConnectionStatus::Stopped,
        MaintenanceRow::Service,
    )
    .expect("a stopped server was observed");
    assert_eq!(stopped.state, Some("Stopped"));
    assert_eq!(
        stopped.actions,
        vec![
            RemoteAction::Start,
            RemoteAction::Uninstall,
            RemoteAction::Check
        ]
    );
    for connection in [
        connected.clone(),
        RemoteConnectionStatus::Stopped,
        checked(ServerAvailability::NotInstalled),
        checked(ServerAvailability::NotRunning),
    ] {
        for row in [MaintenanceRow::Service, MaintenanceRow::Update] {
            if let Some(presentation) = observed("penguin", &connection, row) {
                reads_as_one_sentence(&presentation.explanation);
            }
        }
    }
    let bare = observed(
        "penguin",
        &checked(ServerAvailability::NotInstalled),
        MaintenanceRow::Service,
    )
    .expect("a check observed that no server is installed");
    assert_eq!(bare.actions, vec![RemoteAction::Check]);
    assert!(observed_menu_actions(&checked(ServerAvailability::NotInstalled)).is_empty());
    assert!(
        observed(
            "penguin",
            &RemoteConnectionStatus::Inactive,
            MaintenanceRow::Service
        )
        .is_none(),
        "nothing has been observed, so nothing is offered from an observation"
    );
}

/// A removed connection's server that has no record of its original leaves nothing to resolve, so
/// the entry reaches a final word instead of disappearing without one. It names both ways that
/// can happen without choosing one, claims nothing about what the operation did, and offers the
/// one step that clears it. The selected server's own row keeps the read it can still make.
#[test]
fn a_removed_connection_whose_original_is_unknown_reaches_a_final_word() {
    let forgotten = RemoteOperationState::Failed {
        failure: SshMaintenanceFailure::Forgotten,
        kind: None,
    };

    let presentation = removed_presentation("penguin", std::slice::from_ref(&forgotten))
        .expect("a removed connection's unknown original still has a final word");
    assert_eq!(
        presentation
            .actions
            .iter()
            .map(|action| action.label())
            .collect::<Vec<_>>(),
        vec!["OK"],
        "the entry offers the one step that clears it, got {:?}",
        presentation.actions
    );
    let said = presentation.explanation.to_string();
    assert!(
        said.contains("penguin has no record of the operation SpaceTerm sent."),
        "the final word names the machine with no record, got {said:?}"
    );
    assert!(
        said.contains("may never have arrived, or that server may have dropped it"),
        "the final word names both ways this happens without choosing one, got {said:?}"
    );
    assert!(
        said.contains("can\u{2019}t tell whether it took effect"),
        "the final word claims nothing about what the operation did, got {said:?}"
    );
    assert!(
        said.contains("has stopped following it"),
        "the final word says this client is no longer following it, got {said:?}"
    );
    reads_as_one_sentence(&presentation.explanation);
    assert!(
        !presentation.working,
        "nothing is underway for an original the service has no record of"
    );
    assert_eq!(
        service("penguin", &forgotten).actions,
        vec![RemoteAction::Check],
        "the selected server's own row keeps the read it can still make"
    );
}

/// This client's private record of these operations is not the record of saved connections, so
/// its failure says only what it changes: the step the person just took leaves the operation
/// listed, and a result on screen now is gone after SpaceTerm quits. The two storage conditions
/// read differently, and neither names a file, a path or an operating-system error.
#[test]
fn an_unsaved_operation_record_says_what_it_leaves_the_person_with() {
    let unavailable = operations_persistence_warning(SshIntentStoreError::Unavailable);
    let malformed = operations_persistence_warning(SshIntentStoreError::Malformed);

    assert_ne!(
        unavailable, malformed,
        "storage SpaceTerm couldn\u{2019}t reach reads differently from a damaged record"
    );
    assert!(
        unavailable.contains("couldn\u{2019}t save what it knows about the operations"),
        "got {unavailable:?}"
    );
    assert!(
        malformed.contains("is damaged, so it left the record unchanged"),
        "got {malformed:?}"
    );
    for notice in [unavailable, malformed] {
        assert!(
            notice.contains("Choosing OK or Discard leaves the operation listed."),
            "the notice says the step the person took doesn\u{2019}t clear it, got {notice:?}"
        );
        assert!(
            notice.contains("gone after SpaceTerm quits"),
            "the notice says a result on screen now doesn\u{2019}t survive quitting, got \
             {notice:?}"
        );
        assert!(
            notice.contains("What your servers already did is unchanged."),
            "the notice says the other computers are unaffected, got {notice:?}"
        );
        for named in [".json", "/", "\\", "errno", "Error", "Os {"] {
            assert!(
                !notice.contains(named),
                "the notice carries {named:?}, which names storage the person can\u{2019}t act \
                 on: {notice:?}"
            );
        }
    }
}

/// The owner refuses a second submission while it still holds an original it has not settled,
/// rather than overwriting the one record that could still say what that original did. The
/// refused step reached no computer, and the earlier original is still outstanding, so the row
/// says both and keeps the step that settles the original. Only a refusal holding nothing
/// submitted promises the machine is unchanged.
#[test]
fn a_refused_submission_reports_the_original_that_refused_it() {
    let state = |kind| RemoteOperationState::Failed {
        failure: SshMaintenanceFailure::NotRetained,
        kind,
    };
    let holding = state(Some(SubmittedKind::Service(Action::Stop)));

    let held = service("penguin", &holding);
    assert_eq!(
        held.actions,
        vec![RemoteAction::CheckAgain],
        "a refused submission must keep the step that settles the original it refused for"
    );
    assert!(
        held.explanation
            .contains("didn\u{2019}t ask for the step you just chose"),
        "the row must say the refused step reached nothing, got {:?}",
        held.explanation
    );
    assert!(
        held.explanation
            .contains("still following the stop it already sent to penguin"),
        "the row must say which earlier original it is still following, got {:?}",
        held.explanation
    );
    assert!(
        held.explanation
            .contains("can\u{2019}t tell whether penguin stopped its server"),
        "the row must say the earlier original is unsettled, got {:?}",
        held.explanation
    );
    assert!(
        held.explanation
            .contains("isn\u{2019}t asked to do it again"),
        "the row must promise the earlier original is not repeated, got {:?}",
        held.explanation
    );
    assert!(
        !held.explanation.contains("Nothing changed on penguin"),
        "the row must not promise the machine is unchanged while an original it sent is still \
         outstanding, got {:?}",
        held.explanation
    );
    reads_as_one_sentence(&held.explanation);
    assert_eq!(
        update("penguin", &state(Some(SubmittedKind::Return))).actions,
        vec![RemoteAction::Finish],
        "the update journey keeps its own resolving step"
    );
    // The step settles the original without reaching the connection, so a removed connection
    // holding this state stays listed with it.
    assert_eq!(
        removed_actions(&holding, MaintenanceRow::Service),
        vec![RemoteAction::CheckAgain],
        "a removed connection's record must stay reachable"
    );

    // Nothing was submitted, so the refusal is the whole story and a read is the only step.
    let bare = service("penguin", &state(None));
    assert_eq!(bare.actions, vec![RemoteAction::Check]);
    assert!(bare.explanation.contains("Nothing changed on penguin"));
    assert_ne!(
        bare.state_selector, held.state_selector,
        "a refusal holding an original reads differently from one holding none"
    );
}

/// Two originals on one machine, neither a duplicate of the other, each reach the person. Each is
/// reported by the row whose journey it belongs to, so a service original and an update original
/// read in their own rows and neither row claims the other's.
#[test]
fn two_originals_on_different_journeys_each_read_in_their_own_row() {
    let states = [
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Return,
        },
    ];

    assert_eq!(
        menu_actions_for(&states, None),
        vec![
            RemoteAction::CheckAgain,
            RemoteAction::Discard,
            RemoteAction::Finish,
        ],
        "both journeys stay reachable, with one shared Discard"
    );
    let service = present_machine("penguin", &states, MaintenanceRow::Service, None);
    assert_eq!(
        service.actions,
        vec![RemoteAction::CheckAgain, RemoteAction::Discard]
    );
    assert!(
        service
            .explanation
            .contains("whether penguin stopped its server"),
        "got {:?}",
        service.explanation
    );
    assert!(
        !service.explanation.contains("You asked for"),
        "the update journey reports its own original, so the service row names none, got {:?}",
        service.explanation
    );
    let update = present_machine("penguin", &states, MaintenanceRow::Update, None);
    assert_eq!(
        update.actions,
        vec![RemoteAction::Finish, RemoteAction::Discard]
    );
    assert!(
        update
            .explanation
            .contains("returned its server to the previous version"),
        "got {:?}",
        update.explanation
    );
    reads_as_one_sentence(&service.explanation);
    reads_as_one_sentence(&update.explanation);
}

/// A row reports one original, so a machine whose originals belong to one journey reads one of
/// them and names the rest. The sentence counts them, names each, and promises each a turn.
#[test]
fn a_row_names_every_original_it_has_still_to_report() {
    let one = [
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Restart),
        },
    ];

    let service = present_machine("penguin", &one, MaintenanceRow::Service, None);
    assert_eq!(
        service.state,
        Some("Unconfirmed"),
        "the row reports the first original the owner holds"
    );
    assert!(
        service
            .explanation
            .contains("You asked for one more operation on penguin: the restart."),
        "got {:?}",
        service.explanation
    );
    assert!(
        service
            .explanation
            .contains("reports that one here once this one is settled"),
        "got {:?}",
        service.explanation
    );
    reads_as_one_sentence(&service.explanation);

    let several = [
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Restart),
        },
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Uninstall),
        },
    ];

    let service = present_machine("penguin", &several, MaintenanceRow::Service, None);
    assert!(
        service
            .explanation
            .contains("You asked for 2 more operations on penguin: the restart and the removal."),
        "got {:?}",
        service.explanation
    );
    assert!(
        service
            .explanation
            .contains("reports them here one at a time, as each one is settled"),
        "got {:?}",
        service.explanation
    );
    reads_as_one_sentence(&service.explanation);
}

/// A read is the fact a row reports when nothing is submitted, so an original the person is
/// waiting on takes the row from it instead of being hidden behind it. One state alone reads as
/// it always has.
#[test]
fn an_original_takes_the_row_from_a_read_that_asked_for_nothing() {
    let read = RemoteOperationState::Read(Box::new(facts(status(true, true, false))));
    let unconfirmed = RemoteOperationState::Unconfirmed {
        kind: SubmittedKind::Service(Action::Stop),
    };
    let states = [read.clone(), unconfirmed.clone()];

    let service = present_machine("penguin", &states, MaintenanceRow::Service, None);
    assert_eq!(service.state, Some("Unconfirmed"));
    assert_eq!(
        service.actions,
        vec![RemoteAction::CheckAgain, RemoteAction::Discard]
    );
    assert!(
        !service.explanation.contains("You asked for"),
        "a read asked for nothing, so the row names no operation waiting, got {:?}",
        service.explanation
    );

    for state in [read, unconfirmed] {
        for row in [MaintenanceRow::Service, MaintenanceRow::Update] {
            assert_eq!(
                present_machine("penguin", std::slice::from_ref(&state), row, None),
                present("penguin", &state, row, None),
                "one state alone must read as it always has"
            );
        }
    }
    assert_eq!(
        present_machine("penguin", &[], MaintenanceRow::Service, None),
        present(
            "penguin",
            &RemoteOperationState::Unread,
            MaintenanceRow::Service,
            None
        ),
        "a machine the owner holds nothing for reads as unread"
    );
}

/// A removed connection can hold two originals for the same reason a saved one can, so its entry
/// reports one of them and names the other. The step it offers settles the one it reports.
#[test]
fn a_removed_connection_names_the_second_original_it_holds() {
    let states = [
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
        RemoteOperationState::Resolved {
            handle: RemoteOperationHandle(7),
            kind: SubmittedKind::Service(Action::Restart),
            outcome: completed(Some(status(true, true, false))),
        },
    ];

    let presentation = removed_presentation("penguin", &states)
        .expect("a removed connection with two originals still has something to say");
    assert_eq!(
        presentation.actions,
        vec![RemoteAction::CheckAgain, RemoteAction::Discard],
        "the entry offers the steps that settle the original it reports"
    );
    assert!(
        presentation
            .explanation
            .contains("whether penguin stopped its server"),
        "got {:?}",
        presentation.explanation
    );
    assert!(
        presentation
            .explanation
            .contains("You asked for one more operation on penguin: the restart."),
        "the entry must name the original it has still to report, got {:?}",
        presentation.explanation
    );
    reads_as_one_sentence(&presentation.explanation);
}
