use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{CorePaneScreen, CorePaneView, MachinePaneConnection};
use crate::machine_connection::CoreError;
use crate::terminal::core_session::CorePaneStatus;
use spaceterm_core::pane::{SessionEnd, control::Controller};

/// A Pane connection whose answers a test sets and whose explicit requests it counts.
struct Connection {
    status: Mutex<CorePaneStatus>,
    version: Mutex<Option<spaceterm_core::update::facts::Version>>,
    restart: Mutex<Option<spaceterm_core::update::facts::Version>>,
    start_failure: Mutex<Option<spaceterm_core::update::facts::Version>>,
    /// The announced restart is one the Core began because nothing was running.
    idle_restart: AtomicBool,
    now: Mutex<std::time::Instant>,
    session_end: Mutex<Option<SessionEnd>>,
    start_unconfirmed: AtomicBool,
    generation: AtomicU64,
    starts: AtomicUsize,
    retries: AtomicUsize,
    take_overs: AtomicUsize,
}

impl Connection {
    fn new(status: CorePaneStatus) -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(status),
            version: Mutex::default(),
            restart: Mutex::default(),
            start_failure: Mutex::default(),
            idle_restart: AtomicBool::new(false),
            now: Mutex::new(std::time::Instant::now()),
            session_end: Mutex::default(),
            start_unconfirmed: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            starts: AtomicUsize::new(0),
            retries: AtomicUsize::new(0),
            take_overs: AtomicUsize::new(0),
        })
    }

    fn set(&self, status: CorePaneStatus) {
        *self.status.lock().expect("status") = status;
    }

    /// A new connection reached the server, which answered with `status`.
    fn answer(&self, status: CorePaneStatus) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.set(status);
    }

    fn set_start_unconfirmed(&self, unconfirmed: bool) {
        self.start_unconfirmed.store(unconfirmed, Ordering::SeqCst);
    }

    fn set_version(&self, version: &str) {
        *self.version.lock().unwrap() = Some(version_of(version));
    }

    /// The Core announces a restart into `version`, or ends its announcement.
    fn announce(&self, version: Option<&str>) {
        *self.restart.lock().unwrap() = version.map(version_of);
    }

    fn advance(&self, by: std::time::Duration) {
        *self.now.lock().unwrap() += by;
    }
}

fn version_of(text: &str) -> spaceterm_core::update::facts::Version {
    spaceterm_core::update::facts::Version::parse(text).unwrap()
}

impl MachinePaneConnection for Connection {
    fn restart_progress(&self) -> Option<crate::terminal::core_session::restart::CoreRestart> {
        self.restart.lock().unwrap().as_ref().map(|to| {
            crate::terminal::core_session::restart::CoreRestart {
                to: Some(to.clone()),
                while_idle: self.idle_restart.load(Ordering::SeqCst),
            }
        })
    }
    fn start_failure(&self) -> Option<spaceterm_core::update::facts::Version> {
        self.start_failure.lock().unwrap().clone()
    }
    fn now(&self) -> std::time::Instant {
        *self.now.lock().unwrap()
    }
    fn running_version(&self) -> Option<spaceterm_core::update::facts::Version> {
        self.version.lock().unwrap().clone()
    }
    fn status(&self) -> CorePaneStatus {
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

fn notice(view: &CorePaneView) -> Option<(String, String)> {
    view.notice()
        .map(|(title, detail, _)| (title.to_string(), detail.to_string()))
}

/// A restored Pane whose Start New Shell was clicked and then lost its connection before the
/// server answered.
fn lost_start() -> (Arc<Connection>, CorePaneView) {
    let connection = Connection::new(CorePaneStatus::Restored);
    let mut view = CorePaneView::new(connection.clone());
    view.start_shell();
    connection.set(CorePaneStatus::Reconnecting);
    view.sync();
    (connection, view)
}

#[test]
fn a_lost_start_offers_only_a_check_while_reconnecting() {
    let (connection, view) = lost_start();

    assert_eq!(
        view.screen(),
        Some(CorePaneScreen::StartUnknown {
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
    connection.answer(CorePaneStatus::Failed(CoreError::Protocol));
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
        Some(CorePaneScreen::StartUnknown {
            checking: true,
            unanswered: false,
        })
    );
    assert!(!view.can_retry(), "one check runs at a time");
}

#[test]
fn a_check_that_settles_nothing_says_so_and_can_run_again() {
    let (connection, mut view) = lost_start();
    connection.answer(CorePaneStatus::Failed(CoreError::Protocol));
    view.sync();
    view.retry();

    connection.answer(CorePaneStatus::Failed(CoreError::Protocol));
    view.sync();

    assert_eq!(
        view.screen(),
        Some(CorePaneScreen::StartUnknown {
            checking: false,
            unanswered: true,
        })
    );
    assert!(view.can_retry());
    assert!(!view.can_start_shell());
}

#[test]
fn a_different_core_boot_offers_start_new_shell_again() {
    let (connection, mut view) = lost_start();
    connection.set_start_unconfirmed(false);
    connection.answer(CorePaneStatus::Restored);
    view.sync();

    assert!(matches!(
        view.screen(),
        Some(CorePaneScreen::Restored {
            unconfirmed: false,
            offered: true,
            ..
        })
    ));
    assert!(view.can_start_shell());
}

#[test]
fn a_live_shell_found_by_a_check_replaces_the_screen() {
    let (connection, mut view) = lost_start();
    connection.set_start_unconfirmed(false);
    connection.answer(CorePaneStatus::Connected);
    view.sync();

    assert_eq!(view.screen(), None);
    assert_eq!(notice(&view), None);
}

#[test]
fn take_over_waits_while_a_start_may_have_run() {
    let connection = Connection::new(CorePaneStatus::Takeover);
    connection.set_start_unconfirmed(true);
    let mut view = CorePaneView::new(connection.clone());

    view.take_over();

    assert!(!view.can_take_over());
    assert!(matches!(
        view.screen(),
        Some(CorePaneScreen::Takeover { offered: false, .. })
    ));
    assert_eq!(connection.take_overs.load(Ordering::SeqCst), 0);
}

#[test]
fn a_restored_pane_that_stopped_reconnecting_names_the_failure() {
    let connection = Connection::new(CorePaneStatus::Restored);
    let mut view = CorePaneView::new(connection.clone());
    connection.set(CorePaneStatus::Failed(CoreError::Protocol));
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
fn a_blocked_paired_machine_names_the_side_that_is_too_old() {
    use spaceterm_core::protocol::version::Incompatible;
    for (incompatible, detail) in [
        (
            Incompatible::ListenerTooOld,
            "SpaceTerm Core on penguin is too old for this SpaceTerm.",
        ),
        (
            Incompatible::CallerTooOld,
            "This SpaceTerm is too old for SpaceTerm Core on penguin.",
        ),
    ] {
        let connection = Connection::new(CorePaneStatus::Connected);
        let mut view = CorePaneView::new(connection.clone());
        view.set_machine_name("penguin".into());
        connection.set(CorePaneStatus::Failed(CoreError::IncompatibleProtocol(
            incompatible,
        )));
        view.sync();
        assert_eq!(
            notice(&view),
            Some(("Incompatible SpaceTerm Core".to_owned(), detail.to_owned()))
        );
    }
}

#[test]
fn a_first_selection_whose_answer_was_lost_shows_the_unconfirmed_start() {
    let connection = Connection::new(CorePaneStatus::Connecting);
    let mut view = CorePaneView::new(connection.clone());
    connection.set_start_unconfirmed(true);
    view.sync();
    assert_eq!(view.screen(), None, "a start in flight replaces nothing");

    connection.set(CorePaneStatus::Reconnecting);
    let sync = view.sync();

    assert!(sync.entering_screen && sync.clear_presentation);
    assert_eq!(
        view.screen(),
        Some(CorePaneScreen::StartUnknown {
            checking: false,
            unanswered: false,
        })
    );
    assert!(view.can_retry());
}

#[test]
fn a_restored_pane_whose_start_is_in_flight_shows_it_starting() {
    let connection = Connection::new(CorePaneStatus::Restored);
    connection.set_start_unconfirmed(true);
    let view = CorePaneView::new(connection);

    assert_eq!(
        view.screen(),
        Some(CorePaneScreen::Restored {
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
    connection.set(CorePaneStatus::Connecting);
    view.sync();

    assert_eq!(
        view.screen(),
        Some(CorePaneScreen::StartUnknown {
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
    let connection = Connection::new(CorePaneStatus::SessionEnded(end));
    let mut view = CorePaneView::new(connection.clone());

    assert_eq!(
        view.screen(),
        Some(CorePaneScreen::Restored {
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
    let connection = Connection::new(CorePaneStatus::Takeover);
    *connection.session_end.lock().unwrap() = Some(end);
    let mut view = CorePaneView::new(connection.clone());

    assert_eq!(
        view.screen(),
        Some(CorePaneScreen::Restored {
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
    connection.set(CorePaneStatus::TakingOver);
    view.sync();
    assert!(matches!(
        view.screen(),
        Some(CorePaneScreen::Restored {
            starting: true,
            offered: false,
            ..
        })
    ));
}

/// The detail of the screen a Pane shows once its Terminal Session ended, if it names a restart.
fn ended_detail(view: &CorePaneView) -> Option<String> {
    let copy = view.dormant_copy(None);
    (copy.title.as_ref() == "Terminal Session Ended").then(|| copy.detail.to_string())
}

/// A Pane whose Terminal Session runs on SpaceTerm Core 0.11.0, which then restarts and comes
/// back with the Pane retained and no Terminal Session.
fn restart_ending_a_session(
    announced: Option<&str>,
    returns_on: &str,
) -> (Arc<Connection>, CorePaneView) {
    let connection = Connection::new(CorePaneStatus::Connected);
    connection.set_version("0.11.0");
    let mut view = CorePaneView::new(connection.clone());
    connection.announce(announced);
    view.sync();
    connection.answer(CorePaneStatus::Reconnecting);
    view.sync();
    connection.announce(None);
    connection.set_version(returns_on);
    connection.answer(CorePaneStatus::Restored);
    view.sync();
    (connection, view)
}

#[test]
fn a_pane_whose_session_a_restart_ended_names_how_the_restart_went() {
    let (_, view) = restart_ending_a_session(Some("0.11.1"), "0.11.1");
    assert_eq!(
        ended_detail(&view).as_deref(),
        Some("SpaceTerm Core restarted into 0.11.1.")
    );
    assert_eq!(view.dormant_copy(None).label, "Start Session");

    let (connection, view) = restart_ending_a_session(None, "0.11.0");
    assert_eq!(
        ended_detail(&view).as_deref(),
        Some("SpaceTerm Core restarted.")
    );

    connection.answer(CorePaneStatus::Connected);
    let mut view = view;
    view.sync();
    *connection.start_failure.lock().unwrap() = Some(version_of("0.11.1"));
    connection.announce(Some("0.11.1"));
    view.sync();
    connection.answer(CorePaneStatus::Reconnecting);
    view.sync();
    connection.announce(None);
    connection.answer(CorePaneStatus::Restored);
    view.sync();
    assert_eq!(
        ended_detail(&view).as_deref(),
        Some("SpaceTerm Core couldn't restart into 0.11.1.")
    );
    assert_eq!(
        view.dormant_copy(Some(SessionEnd::Signaled))
            .detail
            .as_ref(),
        super::SIGNALED_DETAIL,
        "a Session that ended by itself keeps its own cause"
    );
}

#[test]
fn a_pane_names_an_idle_restart_the_core_reports() {
    let connection = Connection::new(CorePaneStatus::Connected);
    connection.set_version("0.11.0");
    connection.idle_restart.store(true, Ordering::SeqCst);
    let mut view = CorePaneView::new(connection.clone());
    connection.announce(Some("0.11.1"));
    view.sync();
    connection.answer(CorePaneStatus::Reconnecting);
    view.sync();
    connection.announce(None);
    connection.set_version("0.11.1");
    connection.answer(CorePaneStatus::Restored);
    view.sync();
    assert_eq!(
        ended_detail(&view).as_deref(),
        Some("SpaceTerm Core restarted into 0.11.1 while nothing was running.")
    );
}

#[test]
fn a_pane_does_not_claim_an_update_when_the_core_returns_to_its_previous_version() {
    let (_, view) = restart_ending_a_session(Some("0.11.1"), "0.11.0");
    assert_eq!(
        ended_detail(&view).as_deref(),
        Some("SpaceTerm Core restarted."),
        "without a reported start failure the Pane claims only the restart"
    );
}

#[test]
fn a_pane_that_had_no_session_keeps_its_first_use_copy() {
    let connection = Connection::new(CorePaneStatus::Restored);
    connection.set_version("0.11.0");
    let mut view = CorePaneView::new(connection.clone());
    connection.set_version("0.11.1");
    connection.answer(CorePaneStatus::Restored);
    view.sync();
    let copy = view.dormant_copy(None);
    assert_eq!(copy.title.as_ref(), super::RESTORED_TITLE);
    assert_eq!(copy.detail.as_ref(), super::RESTORED_DETAIL);
}

/// A saved version change is no evidence that a Terminal Session ran: inventory observation
/// records one for every retained Pane across a version-changing boot.
#[test]
fn a_saved_version_change_alone_keeps_the_first_use_copy() {
    use crate::terminal::core_session::restart::observe_panes;
    use crate::ui::workspaces::tests::{inventory, pane, tab};
    use spaceterm_core::hierarchy::Axis;
    let mut inventory = inventory(
        1,
        vec![tab([2; 32], vec![pane([3; 32], None, Axis::Right)])],
    );
    let mut saved = Vec::new();
    observe_panes(&mut saved, &inventory, &version_of("0.11.1"));
    inventory.close.as_mut().unwrap().boot = [10; 32];
    observe_panes(&mut saved, &inventory, &version_of("0.11.2"));
    assert_eq!(saved[0].updated, Some(version_of("0.11.2")));

    // Registering the Pane after a relaunch opens a view on its Restored connection.
    let connection = Connection::new(CorePaneStatus::Restored);
    connection.set_version("0.11.2");
    let view = CorePaneView::new(connection);
    let copy = view.dormant_copy(None);
    assert_eq!(copy.title.as_ref(), super::RESTORED_TITLE);
    assert_eq!(copy.detail.as_ref(), super::RESTORED_DETAIL);
}

#[test]
fn a_start_failure_published_after_the_pane_returned_repaints_it() {
    use crate::terminal::core_session::restart::RECONNECT_LIMIT;
    let (connection, mut view) = restart_ending_a_session(Some("0.11.1"), "0.11.0");
    connection.advance(RECONNECT_LIMIT);
    view.sync();
    assert_eq!(
        ended_detail(&view).as_deref(),
        Some("SpaceTerm Core restarted.")
    );
    *connection.start_failure.lock().unwrap() = Some(version_of("0.11.1"));
    assert!(view.sync().changed, "the late failure repaints the Pane");
    assert_eq!(
        ended_detail(&view).as_deref(),
        Some("SpaceTerm Core couldn't restart into 0.11.1.")
    );
}

#[test]
fn an_announced_restart_keeps_one_deadline_through_failed_statuses() {
    use crate::terminal::core_session::restart::RECONNECT_LIMIT;
    for failed in [
        CorePaneStatus::Failed(CoreError::Protocol),
        CorePaneStatus::TakeoverFailed(CoreError::Protocol),
    ] {
        let connection = Connection::new(CorePaneStatus::Connected);
        let mut view = CorePaneView::new(connection.clone());
        connection.announce(Some("0.11.2"));
        view.sync();
        connection.answer(CorePaneStatus::Reconnecting);
        view.sync();
        connection.advance(std::time::Duration::from_secs(60));
        connection.set(failed);
        view.sync();
        assert_eq!(
            notice(&view),
            None,
            "{failed:?} within the deadline shows nothing"
        );
        connection.advance(std::time::Duration::from_secs(60));
        connection.answer(CorePaneStatus::Reconnecting);
        view.sync();
        assert_eq!(notice(&view), None);
        connection.advance(RECONNECT_LIMIT - std::time::Duration::from_secs(120));
        view.sync();
        assert_eq!(
            notice(&view).map(|(title, _)| title),
            Some("Reconnecting\u{2026}".to_owned()),
            "the deadline counts from the first reading, not from the last failure"
        );
    }
}

#[test]
fn an_announced_restart_shows_no_notice_until_its_deadline() {
    use crate::terminal::core_session::restart::RECONNECT_LIMIT;
    let (connection, mut view) = lost_start();
    connection.announce(Some("0.11.2"));
    view.sync();
    assert_eq!(notice(&view), None, "the title bar owns restart progress");
    assert!(!matches!(
        view.screen(),
        Some(CorePaneScreen::StartUnknown { .. })
    ));
    assert!(!view.notice_offers_retry());

    connection.advance(RECONNECT_LIMIT - std::time::Duration::from_secs(1));
    view.sync();
    assert_eq!(notice(&view), None, "Reconnecting is no exception");

    connection.advance(std::time::Duration::from_secs(1));
    assert!(view.sync().changed, "the deadline repaints the Pane");
    assert_eq!(
        notice(&view).map(|(title, _)| title),
        Some("Reconnecting…".to_owned()),
        "after the deadline the restart is an ordinary connection loss"
    );
}
