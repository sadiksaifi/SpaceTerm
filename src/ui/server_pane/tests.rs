use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{ServerPaneConnection, ServerPaneScreen, ServerPaneView};
use crate::server_connection::ServerError;
use crate::terminal::server_session::ServerPaneStatus;
use spaceterm_server::pane::{SessionEnd, control::Controller};

/// A Pane connection whose answers a test sets and whose explicit requests it counts.
struct Connection {
    status: Mutex<ServerPaneStatus>,
    session_end: Mutex<Option<SessionEnd>>,
    start_unconfirmed: AtomicBool,
    generation: AtomicU64,
    starts: AtomicUsize,
    retries: AtomicUsize,
    take_overs: AtomicUsize,
}

impl Connection {
    fn new(status: ServerPaneStatus) -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(status),
            session_end: Mutex::default(),
            start_unconfirmed: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            starts: AtomicUsize::new(0),
            retries: AtomicUsize::new(0),
            take_overs: AtomicUsize::new(0),
        })
    }

    fn set(&self, status: ServerPaneStatus) {
        *self.status.lock().expect("status") = status;
    }

    /// A new connection reached the server, which answered with `status`.
    fn answer(&self, status: ServerPaneStatus) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.set(status);
    }

    fn set_start_unconfirmed(&self, unconfirmed: bool) {
        self.start_unconfirmed.store(unconfirmed, Ordering::SeqCst);
    }
}

impl ServerPaneConnection for Connection {
    fn status(&self) -> ServerPaneStatus {
        *self.status.lock().expect("status")
    }

    fn controller(&self) -> Option<Controller> {
        None
    }

    fn session_end(&self) -> Option<SessionEnd> {
        *self.session_end.lock().expect("session end")
    }

    fn connection_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    // These tests read the view directly.
    fn observe(&self, hint: async_channel::Sender<()>) {
        let _ = hint.try_send(());
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
    let connection = Connection::new(ServerPaneStatus::Restored);
    let mut view = ServerPaneView::new(connection.clone());
    view.start_shell();
    connection.set(ServerPaneStatus::Reconnecting);
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
    connection.answer(ServerPaneStatus::Failed(ServerError::Protocol));
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
    connection.answer(ServerPaneStatus::Failed(ServerError::Protocol));
    view.sync();
    view.retry();

    connection.answer(ServerPaneStatus::Failed(ServerError::Protocol));
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
    connection.answer(ServerPaneStatus::Restored);
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
    connection.answer(ServerPaneStatus::Connected);
    view.sync();

    assert_eq!(view.screen(), None);
    assert_eq!(notice(&view), None);
}

#[test]
fn take_over_waits_while_a_start_may_have_run() {
    let connection = Connection::new(ServerPaneStatus::Takeover);
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
    let connection = Connection::new(ServerPaneStatus::Restored);
    let mut view = ServerPaneView::new(connection.clone());
    connection.set(ServerPaneStatus::Failed(ServerError::Protocol));
    view.sync();

    assert_eq!(
        notice(&view),
        Some((
            "Connection Failed".to_owned(),
            "Choose Try Again to reconnect, then Start Session to open a shell.".to_owned()
        ))
    );
    assert!(view.notice_offers_retry());
}

#[test]
fn an_incompatible_server_is_named_without_claiming_which_side_is_older() {
    let connection = Connection::new(ServerPaneStatus::Connected);
    let mut view = ServerPaneView::new(connection.clone());
    connection.set(ServerPaneStatus::Failed(ServerError::IncompatibleProtocol));
    view.sync();

    assert_eq!(
        notice(&view),
        Some((
            "Incompatible Server".to_owned(),
            "SpaceTerm and this server are incompatible.".to_owned()
        ))
    );
}

#[test]
fn a_first_selection_whose_answer_was_lost_shows_the_unconfirmed_start() {
    let connection = Connection::new(ServerPaneStatus::Connecting);
    let mut view = ServerPaneView::new(connection.clone());
    connection.set_start_unconfirmed(true);
    view.sync();
    assert_eq!(view.screen(), None, "a start in flight replaces nothing");

    connection.set(ServerPaneStatus::Reconnecting);
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
    let connection = Connection::new(ServerPaneStatus::Restored);
    connection.set_start_unconfirmed(true);
    let view = ServerPaneView::new(connection);

    assert_eq!(
        view.screen(),
        Some(ServerPaneScreen::Restored {
            starting: true,
            unconfirmed: false,
            offered: false,
            ended: None,
        })
    );
}

#[test]
fn the_unconfirmed_start_stays_through_a_check_that_reconnects() {
    let (connection, mut view) = lost_start();
    view.retry();
    connection.set(ServerPaneStatus::Connecting);
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

#[test]
fn the_session_ended_screen_says_why_the_shell_ended() {
    for (end, detail) in [
        (
            SessionEnd::Exited { code: 1 },
            "The shell exited with code 1.",
        ),
        (SessionEnd::Signaled, "A signal ended the shell."),
        (SessionEnd::EndAllSessions, super::ENDED_DETAIL),
    ] {
        let copy = super::DormantCopy::of(Some(end));
        assert_eq!(copy.title, "Session Ended");
        assert_eq!(copy.detail.as_ref(), detail);
        assert_eq!(copy.label, "Start Session");
    }
}

#[test]
fn an_exited_shell_offers_start_session_without_starting_one() {
    let end = SessionEnd::Exited { code: 127 };
    let connection = Connection::new(ServerPaneStatus::SessionEnded(end));
    let mut view = ServerPaneView::new(connection.clone());

    assert_eq!(
        view.screen(),
        Some(ServerPaneScreen::Restored {
            starting: false,
            unconfirmed: false,
            offered: true,
            ended: Some(end),
        })
    );
    assert_eq!(connection.starts.load(Ordering::SeqCst), 0);

    view.start_shell();

    assert_eq!(connection.starts.load(Ordering::SeqCst), 1);
    assert_eq!(connection.take_overs.load(Ordering::SeqCst), 0);
}

/// The Pane's control grant belongs to another client, but nothing runs to take over, so this
/// client sees the same Session Ended screen and its Start Session takes the Pane over.
#[test]
fn another_clients_exited_shell_offers_start_session_through_take_over() {
    let end = SessionEnd::Exited { code: 1 };
    let connection = Connection::new(ServerPaneStatus::Takeover);
    *connection.session_end.lock().unwrap() = Some(end);
    let mut view = ServerPaneView::new(connection.clone());

    assert_eq!(
        view.screen(),
        Some(ServerPaneScreen::Restored {
            starting: false,
            unconfirmed: false,
            offered: true,
            ended: Some(end),
        })
    );
    assert!(view.can_start_shell());

    view.start_shell();

    assert_eq!(connection.take_overs.load(Ordering::SeqCst), 1);
    assert_eq!(connection.starts.load(Ordering::SeqCst), 0);
    connection.set(ServerPaneStatus::TakingOver);
    view.sync();
    assert!(matches!(
        view.screen(),
        Some(ServerPaneScreen::Restored {
            starting: true,
            offered: false,
            ..
        })
    ));
}
