use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{ServerPaneConnection, ServerPaneScreen, ServerPaneView};
use crate::server_connection::ServerError;
use crate::terminal::server_session::DevelopmentPaneStatus;
use spaceterm_server::pane::control::Controller;

/// A Pane connection whose answers a test sets and whose explicit requests it counts.
struct Connection {
    status: Mutex<DevelopmentPaneStatus>,
    start_unconfirmed: AtomicBool,
    generation: AtomicU64,
    starts: AtomicUsize,
    retries: AtomicUsize,
    take_overs: AtomicUsize,
}

impl Connection {
    fn new(status: DevelopmentPaneStatus) -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(status),
            start_unconfirmed: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            starts: AtomicUsize::new(0),
            retries: AtomicUsize::new(0),
            take_overs: AtomicUsize::new(0),
        })
    }

    fn set(&self, status: DevelopmentPaneStatus) {
        *self.status.lock().expect("status") = status;
    }

    /// A new connection reached the server, which answered with `status`.
    fn answer(&self, status: DevelopmentPaneStatus) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.set(status);
    }

    fn set_start_unconfirmed(&self, unconfirmed: bool) {
        self.start_unconfirmed.store(unconfirmed, Ordering::SeqCst);
    }
}

impl ServerPaneConnection for Connection {
    fn status(&self) -> DevelopmentPaneStatus {
        *self.status.lock().expect("status")
    }

    fn controller(&self) -> Option<Controller> {
        None
    }

    fn connection_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn select(&self) {}

    fn retry(&self) {
        self.retries.fetch_add(1, Ordering::SeqCst);
    }

    fn take_over(&self) {
        self.take_overs.fetch_add(1, Ordering::SeqCst);
    }

    fn start_restored(&self) {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.start_unconfirmed.store(true, Ordering::SeqCst);
    }

    fn start_unconfirmed(&self) -> bool {
        self.start_unconfirmed.load(Ordering::SeqCst)
    }
}

fn notice(view: &ServerPaneView) -> Option<(String, String)> {
    view.notice()
        .map(|(title, detail, _)| (title.to_string(), detail.to_string()))
}

/// A restored Pane whose Start New Shell was clicked and then lost its connection before the
/// server answered.
fn lost_start() -> (Arc<Connection>, ServerPaneView) {
    let connection = Connection::new(DevelopmentPaneStatus::Restored);
    let mut view = ServerPaneView::new(connection.clone());
    view.start_shell();
    connection.set(DevelopmentPaneStatus::Reconnecting);
    view.sync();
    (connection, view)
}

#[test]
fn a_lost_start_offers_only_a_check_while_reconnecting() {
    let (connection, view) = lost_start();

    assert_eq!(
        view.screen(),
        Some(ServerPaneScreen::StartUnknown {
            checking: false,
            unanswered: false,
        })
    );
    assert!(!view.can_start_shell());
    assert_eq!(
        notice(&view),
        Some((
            "Reconnecting…".to_owned(),
            "SpaceTerm checks whether the shell started after reconnecting.".to_owned()
        ))
    );
    assert!(
        view.can_retry() && !view.notice_offers_retry(),
        "the screen offers the one Retry"
    );
    assert_eq!(connection.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn exhausted_recovery_after_a_lost_start_names_the_cause_and_never_starts_again() {
    let (connection, mut view) = lost_start();
    connection.answer(DevelopmentPaneStatus::Failed(ServerError::Protocol));
    view.sync();

    assert_eq!(
        notice(&view),
        Some((
            "Connection Failed".to_owned(),
            "SpaceTerm stopped reconnecting.".to_owned()
        ))
    );
    assert!(view.can_retry() && !view.notice_offers_retry());

    view.start_shell();
    view.retry();

    assert_eq!(connection.starts.load(Ordering::SeqCst), 1);
    assert_eq!(connection.retries.load(Ordering::SeqCst), 1);
    assert_eq!(
        view.screen(),
        Some(ServerPaneScreen::StartUnknown {
            checking: true,
            unanswered: false,
        })
    );
    assert!(!view.can_retry(), "one check runs at a time");
}

#[test]
fn a_check_that_settles_nothing_says_so_and_can_run_again() {
    let (connection, mut view) = lost_start();
    connection.answer(DevelopmentPaneStatus::Failed(ServerError::Protocol));
    view.sync();
    view.retry();

    connection.answer(DevelopmentPaneStatus::Failed(ServerError::Protocol));
    view.sync();

    assert_eq!(
        view.screen(),
        Some(ServerPaneScreen::StartUnknown {
            checking: false,
            unanswered: true,
        })
    );
    assert!(view.can_retry());
    assert!(!view.can_start_shell());
}

#[test]
fn a_different_server_boot_offers_start_new_shell_again() {
    let (connection, mut view) = lost_start();
    connection.set_start_unconfirmed(false);
    connection.answer(DevelopmentPaneStatus::Restored);
    view.sync();

    assert!(matches!(
        view.screen(),
        Some(ServerPaneScreen::Restored { offered: true, .. })
    ));
    assert!(view.can_start_shell());
}

#[test]
fn a_live_shell_found_by_a_check_replaces_the_screen() {
    let (connection, mut view) = lost_start();
    connection.set_start_unconfirmed(false);
    connection.answer(DevelopmentPaneStatus::Connected);
    view.sync();

    assert_eq!(view.screen(), None);
    assert_eq!(notice(&view), None);
}

#[test]
fn take_over_waits_while_a_start_may_have_run() {
    let connection = Connection::new(DevelopmentPaneStatus::Takeover);
    connection.set_start_unconfirmed(true);
    let mut view = ServerPaneView::new(connection.clone());

    view.take_over();

    assert!(!view.can_take_over());
    assert!(matches!(
        view.screen(),
        Some(ServerPaneScreen::Takeover { offered: false, .. })
    ));
    assert_eq!(connection.take_overs.load(Ordering::SeqCst), 0);
}

#[test]
fn a_restored_pane_that_stopped_reconnecting_names_the_failure() {
    let connection = Connection::new(DevelopmentPaneStatus::Restored);
    let mut view = ServerPaneView::new(connection.clone());
    connection.set(DevelopmentPaneStatus::Failed(ServerError::Protocol));
    view.sync();

    assert_eq!(
        notice(&view),
        Some((
            "Connection Failed".to_owned(),
            "Choose Retry to reconnect, then Start Session to open a shell.".to_owned()
        ))
    );
    assert!(view.notice_offers_retry());
}

#[test]
fn an_incompatible_server_is_named_without_claiming_which_side_is_older() {
    for (error, cause) in [
        (
            ServerError::IncompatibleProtocol,
            "SpaceTerm and this server are incompatible.",
        ),
        (
            ServerError::MissingRequiredCapability,
            "This server needs an update.",
        ),
    ] {
        let connection = Connection::new(DevelopmentPaneStatus::Connected);
        let mut view = ServerPaneView::new(connection.clone());
        connection.set(DevelopmentPaneStatus::Failed(error));
        view.sync();

        assert_eq!(
            notice(&view),
            Some(("Incompatible Server".to_owned(), cause.to_owned())),
            "{error:?}"
        );
    }
}

#[test]
fn a_first_selection_whose_answer_was_lost_shows_the_unconfirmed_start() {
    let connection = Connection::new(DevelopmentPaneStatus::Connecting);
    let mut view = ServerPaneView::new(connection.clone());
    connection.set_start_unconfirmed(true);
    view.sync();
    assert_eq!(view.screen(), None, "a start in flight replaces nothing");

    connection.set(DevelopmentPaneStatus::Reconnecting);
    let sync = view.sync();

    assert!(sync.entering_screen && sync.clear_presentation);
    assert_eq!(
        view.screen(),
        Some(ServerPaneScreen::StartUnknown {
            checking: false,
            unanswered: false,
        })
    );
    assert!(view.can_retry());
}

#[test]
fn a_restored_pane_whose_start_is_in_flight_shows_it_starting() {
    let connection = Connection::new(DevelopmentPaneStatus::Restored);
    connection.set_start_unconfirmed(true);
    let view = ServerPaneView::new(connection);

    assert_eq!(
        view.screen(),
        Some(ServerPaneScreen::Restored {
            starting: true,
            unconfirmed: false,
            offered: false,
            ended: false,
        })
    );
}

#[test]
fn the_unconfirmed_start_stays_through_a_check_that_reconnects() {
    let (connection, mut view) = lost_start();
    view.retry();
    connection.set(DevelopmentPaneStatus::Connecting);
    view.sync();

    assert_eq!(
        view.screen(),
        Some(ServerPaneScreen::StartUnknown {
            checking: true,
            unanswered: false,
        })
    );
    assert!(!view.can_start_shell());
}
