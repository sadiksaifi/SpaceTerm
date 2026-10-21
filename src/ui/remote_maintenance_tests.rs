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

/// A submitted operation whose answer was lost stays unconfirmed. The row says so and offers only
/// to ask the machine for the answer, never to repeat the operation.
#[test]
fn an_unconfirmed_operation_only_asks_the_machine_again() {
    let lost = RemoteOperationState::Unconfirmed {
        kind: SubmittedKind::Service(Action::Stop),
    };

    let service = service("penguin", &lost);
    assert_eq!(service.state, Some("Unconfirmed"));
    assert_eq!(service.actions, vec![RemoteAction::CheckAgain]);
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
        vec![RemoteAction::Finish]
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
    ];

    for state in &states {
        for row in [MaintenanceRow::Service, MaintenanceRow::Update] {
            for action in removed_actions(state, row) {
                assert!(
                    matches!(
                        action,
                        RemoteAction::Finish | RemoteAction::CheckAgain | RemoteAction::Acknowledge
                    ),
                    "{action:?} would reach a connection the person removed"
                );
            }
        }
    }
    assert_eq!(
        removed_actions(&states[0], MaintenanceRow::Service),
        vec![RemoteAction::CheckAgain]
    );
    assert_eq!(
        removed_actions(&states[1], MaintenanceRow::Update),
        vec![RemoteAction::Finish]
    );
    assert_eq!(
        removed_actions(&states[2], MaintenanceRow::Service),
        vec![RemoteAction::Acknowledge]
    );
    assert!(
        removed_actions(&states[3], MaintenanceRow::Service).is_empty(),
        "a removed connection offers no read and no new operation"
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

    let presentation = removed_presentation("penguin", &working)
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
            &RemoteOperationState::Read(Box::new(facts(status(true, true, false))))
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
        menu_actions(&running, None),
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
    assert!(menu_actions(&resolved, None).is_empty());
    let working = RemoteOperationState::Working {
        phase: MaintenancePhase::Dispatching,
        kind: Some(SubmittedKind::Service(Action::Stop)),
        cancellable: true,
    };
    assert!(
        menu_actions(&working, None).is_empty(),
        "nothing is offered while an operation is underway"
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
        update
            .detail
            .contains("Terminal Sessions end for every client")
    );
    let returning = MaintenanceConfirmation::of(
        "penguin",
        RemoteMaintenanceRequest::Return,
        &change(false, true),
    )
    .expect("a return describes itself");
    assert_eq!(returning.title, "Return to Previous Version");
    assert!(returning.message.contains("penguin") && returning.message.contains("1.4.0"));
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
