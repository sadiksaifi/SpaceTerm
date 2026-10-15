//! The Development Pane window driven through a scripted server connection.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Modifiers, TestAppContext, VisualTestContext, WeakEntity};

use super::window::testing::ScriptedConnection;
use super::window::{
    DevelopmentPaneWindow, END_SESSION_SELECTOR, NOTICE_SELECTOR, RETRY_SELECTOR, STATUS_INTERVAL,
    TERMINAL_TITLE_SELECTOR, WINDOW_TITLE,
};
use crate::appearance::Appearance;
use crate::local_server::ServerError;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::terminal::WorkspaceTerminalSessionFactory;
use crate::terminal::server_session::DevelopmentPaneStatus;
use crate::terminal::testing::{
    RecordedCommand, TestTerminalSessionFactory, TestTerminalSessionRecords, test_local_directory,
};
use crate::ui::appearance_runtime;
use crate::ui::pane_lifecycle::PaneConstruction;

struct OpenPane<'a> {
    /// Weak, so closing the window releases the Pane as it does in the application.
    window: WeakEntity<DevelopmentPaneWindow>,
    connection: Arc<ScriptedConnection>,
    records: TestTerminalSessionRecords,
    cx: &'a mut VisualTestContext,
}

fn open_pane(status: DevelopmentPaneStatus, cx: &mut TestAppContext) -> OpenPane<'_> {
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let records = TestTerminalSessionRecords::default();
    let session_factory = WorkspaceTerminalSessionFactory::new_server(
        Rc::new(
            TestTerminalSessionFactory::new(records.clone())
                .server_backed()
                .with_fallback_title(WINDOW_TITLE),
        ),
        test_local_directory(PathBuf::from("/tmp/spaceterm-development-pane-test")),
    );
    let connection = Arc::new(ScriptedConnection::new(status));
    let window_connection = Arc::clone(&connection);
    let (window, cx) = cx.add_window_view(|window, cx| {
        DevelopmentPaneWindow::new(
            session_factory,
            window_connection,
            &PaneConstruction::testing(),
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    OpenPane {
        window: window.downgrade(),
        connection,
        records,
        cx,
    }
}

impl OpenPane<'_> {
    fn set(&mut self, status: DevelopmentPaneStatus) {
        self.connection.set(status);
        self.cx.executor().advance_clock(STATUS_INTERVAL);
        self.cx.run_until_parked();
    }

    fn reconnect(&mut self) {
        self.connection.reconnect();
        self.cx.executor().advance_clock(STATUS_INTERVAL);
        self.cx.run_until_parked();
    }

    fn rendered(&mut self, selector: &'static str) -> bool {
        self.cx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &'static str) {
        let position = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not rendered"))
            .center();
        self.cx
            .simulate_mouse_move(position, None, Modifiers::none());
        self.cx.simulate_click(position, Modifiers::none());
        self.cx.run_until_parked();
    }

    /// Presses and releases one key, as a person does.
    fn press(&mut self, key: &str) {
        self.cx.simulate_keystrokes(key);
        self.cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).expect("keystroke"),
        });
        self.cx.run_until_parked();
    }

    fn closed(&self) -> bool {
        self.window.upgrade().is_none()
    }

    fn keys(&self) -> usize {
        self.records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedCommand::Key(_)))
            .count()
    }
}

#[gpui::test]
fn an_interruption_freezes_the_pane_and_discards_input_until_it_reconnects(
    cx: &mut TestAppContext,
) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    assert_eq!(
        pane.records.session_count(),
        1,
        "the Pane measured and started"
    );
    assert!(!pane.rendered(NOTICE_SELECTOR));
    pane.cx.simulate_keystrokes("a");
    let delivered = pane.keys();
    assert!(delivered > 0, "a connected Pane takes keys");

    pane.set(DevelopmentPaneStatus::Reconnecting);
    assert!(pane.rendered(NOTICE_SELECTOR));
    pane.cx.simulate_keystrokes("b c");
    assert_eq!(
        pane.keys(),
        delivered,
        "input during the interruption reaches nothing"
    );

    pane.click(RETRY_SELECTOR);
    assert_eq!(pane.connection.retries(), 1);

    pane.set(DevelopmentPaneStatus::Connected);
    assert_eq!(pane.keys(), delivered, "reconnection replays nothing");
    pane.cx.simulate_keystrokes("d");
    assert!(
        pane.keys() > delivered,
        "input resumes once the server answers again"
    );
    assert_eq!(
        pane.records.session_count(),
        1,
        "the same Terminal Session continues"
    );
}

#[gpui::test]
fn a_failed_connection_explains_itself_and_offers_retry(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);

    pane.set(DevelopmentPaneStatus::Failed(ServerError::NotRunning));

    assert!(pane.rendered(NOTICE_SELECTOR));
    pane.click(RETRY_SELECTOR);
    assert_eq!(pane.connection.retries(), 1);
    pane.cx.simulate_keystrokes("a");
    assert_eq!(pane.keys(), 0);
}

#[gpui::test]
fn closing_the_window_detaches_without_ending_the_session(cx: &mut TestAppContext) {
    let pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.cx.update(|window, _| window.remove_window());
    pane.cx.run_until_parked();

    assert!(pane.closed());
    assert_eq!(pane.connection.end_sessions(), 0);
    assert_eq!(
        pane.records.dropped_session_ids(),
        vec![1],
        "dropping the handle detaches the presentation"
    );
}

#[gpui::test]
fn end_session_asks_first_and_waits_for_the_server(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);

    pane.click(END_SESSION_SELECTOR);
    assert_eq!(pane.connection.end_sessions(), 0, "End Session asks first");
    pane.click("modal-action-development-pane-end-session-cancel");
    assert_eq!(pane.connection.end_sessions(), 0);

    pane.click(END_SESSION_SELECTOR);
    pane.click("modal-action-development-pane-end-session-confirm");
    assert_eq!(pane.connection.end_sessions(), 1);
    assert!(
        pane.rendered("development-pane-status"),
        "the window stays while the server ends the session"
    );

    assert!(!pane.closed());

    pane.set(DevelopmentPaneStatus::Ended);
    assert!(pane.closed(), "the window closes once the session ended");
}

#[gpui::test]
fn an_interrupted_end_session_is_offered_again_once_the_session_proves_running(
    cx: &mut TestAppContext,
) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.click(END_SESSION_SELECTOR);
    pane.click("modal-action-development-pane-end-session-confirm");
    assert_eq!(pane.connection.end_sessions(), 1);

    // Polls on the same connection keep waiting for the server's answer.
    pane.set(DevelopmentPaneStatus::Connected);
    assert!(pane.rendered("development-pane-status"));
    pane.click(END_SESSION_SELECTOR);
    assert!(!pane.rendered("modal-action-development-pane-end-session-confirm"));

    // The connection drops before the server answers, and the connection never resends Close.
    pane.set(DevelopmentPaneStatus::Reconnecting);
    assert!(pane.rendered(NOTICE_SELECTOR));
    assert!(
        !pane.rendered("development-pane-status"),
        "nothing claims the Session is still ending"
    );

    // Reconnection to a running Session settles the request without replaying it.
    pane.reconnect();
    assert!(!pane.rendered(NOTICE_SELECTOR));
    assert!(!pane.rendered("development-pane-status"));
    assert_eq!(
        pane.connection.end_sessions(),
        1,
        "reconnection replays nothing"
    );
    pane.cx.simulate_keystrokes("a");
    assert!(pane.keys() > 0, "the Pane takes input again");
    assert!(!pane.closed());

    // Ending still asks first, then reaches the server once.
    pane.click(END_SESSION_SELECTOR);
    assert!(pane.rendered("modal-action-development-pane-end-session-confirm"));
    assert_eq!(pane.connection.end_sessions(), 1, "End Session asks first");
    pane.click("modal-action-development-pane-end-session-confirm");
    assert_eq!(pane.connection.end_sessions(), 2);
    assert!(pane.rendered("development-pane-status"));

    // A busy window can miss the interruption when reconnection completes between two polls.
    pane.connection.set(DevelopmentPaneStatus::Reconnecting);
    pane.reconnect();
    assert!(
        !pane.rendered("development-pane-status"),
        "the new connection proves the Session still runs"
    );
    pane.click(END_SESSION_SELECTOR);
    assert!(pane.rendered("modal-action-development-pane-end-session-confirm"));
    assert_eq!(pane.connection.end_sessions(), 2, "End Session asks first");
    pane.click("modal-action-development-pane-end-session-confirm");
    assert_eq!(pane.connection.end_sessions(), 3);
}

#[gpui::test]
fn end_session_waits_for_a_connection(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Reconnecting, cx);

    pane.click(END_SESSION_SELECTOR);

    assert!(!pane.rendered("modal-action-development-pane-end-session-confirm"));
    assert_eq!(pane.connection.end_sessions(), 0);
}

#[gpui::test]
fn a_server_backed_pane_performs_no_local_terminal_operations(cx: &mut TestAppContext) {
    use crate::ui::{OpenTerminalFind, PasteClipboard, ScrollPageUp};

    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);

    pane.cx.dispatch_action(PasteClipboard);
    pane.cx.dispatch_action(OpenTerminalFind);
    pane.cx.dispatch_action(ScrollPageUp);
    pane.click("development-pane-terminal");
    pane.cx.run_until_parked();

    let unsupported = pane
        .records
        .commands()
        .into_iter()
        .filter(|call| {
            !matches!(
                call.command,
                RecordedCommand::Key(_)
                    | RecordedCommand::Focus(_)
                    | RecordedCommand::Resize(_)
                    | RecordedCommand::SetPresentable(_)
                    | RecordedCommand::UpdateAppearance(_)
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(unsupported, Vec::new());
    assert!(!pane.rendered("terminal-find-bar"));
}

#[gpui::test]
fn the_header_names_the_window_once_and_adds_only_a_shell_title(cx: &mut TestAppContext) {
    use crate::terminal::{ScreenSnapshot, TerminalSessionEvent};

    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    assert!(
        !pane.rendered(TERMINAL_TITLE_SELECTOR),
        "the fallback title repeats the window's name"
    );

    let screen = ScreenSnapshot::from_test_parts_at(Arc::from([]), Default::default(), "vim", 1);
    pane.records
        .event_sender(1)
        .expect("the Pane started")
        .try_send(TerminalSessionEvent::Screen(screen))
        .expect("the Pane receives its screen");
    pane.cx.run_until_parked();

    assert!(pane.rendered(TERMINAL_TITLE_SELECTOR));
}

#[gpui::test]
fn the_keyboard_reaches_retry_and_end_session(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Reconnecting, cx);

    // Paused input leaves Tab to the window, which moves focus to Retry. End Session waits for a
    // connection, so Tab passes over it.
    pane.press("tab");
    pane.press("space");
    assert_eq!(pane.connection.retries(), 1);
    assert_eq!(pane.keys(), 0, "Tab and Space reach no shell while paused");

    // Once connected, Tab belongs to the shell, and Close Pane is the keyboard's End Session.
    pane.set(DevelopmentPaneStatus::Connected);
    pane.press("tab");
    assert!(pane.keys() > 0, "the shell takes Tab once connected");
    pane.press("cmd-w");
    assert!(pane.rendered("modal-action-development-pane-end-session-confirm"));
    pane.press("escape");
    assert!(!pane.rendered("modal-action-development-pane-end-session-confirm"));
    assert_eq!(pane.connection.end_sessions(), 0, "Escape keeps the shell");
    assert!(!pane.closed());

    // The confirmation starts on Cancel, so Return keeps the shell and Tab reaches End Session.
    pane.press("cmd-w");
    pane.press("enter");
    assert_eq!(pane.connection.end_sessions(), 0, "Return keeps the shell");
    pane.press("cmd-w");
    pane.press("tab");
    pane.press("space");
    assert_eq!(pane.connection.end_sessions(), 1);
}
