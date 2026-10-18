//! The Development Pane window driven through a scripted server connection.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Modifiers, TestAppContext, VisualTestContext, WeakEntity};

use super::window::testing::ScriptedConnection;
use super::window::{
    DevelopmentPaneWindow, END_SESSION_SELECTOR, NOTICE_SELECTOR, RESTORED_SELECTOR,
    RETRY_SELECTOR, START_SHELL_SELECTOR, START_SHELL_STATUS_SELECTOR, STATUS_INTERVAL,
    TAKE_OVER_SELECTOR, TAKE_OVER_STATUS_SELECTOR, TAKEOVER_SELECTOR, TERMINAL_SELECTOR,
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
use crate::ui::server_pane;

struct OpenPane<'a> {
    /// Weak, so closing the window releases the Pane as it does in the application.
    window: WeakEntity<DevelopmentPaneWindow>,
    connection: Arc<ScriptedConnection>,
    records: TestTerminalSessionRecords,
    screen_generation: u64,
    cx: &'a mut VisualTestContext,
}

fn open_pane(status: DevelopmentPaneStatus, cx: &mut TestAppContext) -> OpenPane<'_> {
    open_pane_with_session_options(status, cx, |sessions| sessions)
}

fn open_pane_with_session_options(
    status: DevelopmentPaneStatus,
    cx: &mut TestAppContext,
    configure: impl FnOnce(TestTerminalSessionFactory) -> TestTerminalSessionFactory,
) -> OpenPane<'_> {
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
            configure(TestTerminalSessionFactory::new(records.clone()).server_backed())
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
            &PaneConstruction::new(
                Rc::new(crate::terminal::GpuiTerminalKeyInputAdapterFactory::default()),
                Rc::new(crate::platform::accesskit_terminal_accessibility::AccessKitTerminalAccessibilityAdapterFactory),
                crate::terminal::native_services::testing::adapters(),
                crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(),
            ),
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
        screen_generation: 0,
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

    /// A new connection reaches the server, which answers with this status.
    fn reconnect_to(&mut self, status: DevelopmentPaneStatus) {
        self.connection.reconnect();
        self.connection.set(status);
        self.cx.executor().advance_clock(STATUS_INTERVAL);
        self.cx.run_until_parked();
    }

    /// Another client takes control of the Pane.
    fn lose_control(&mut self) {
        self.connection.lose_control();
        self.cx.executor().advance_clock(STATUS_INTERVAL);
        self.cx.run_until_parked();
    }

    /// The shell publishes a screen with this title, as the server does while this client
    /// controls the Pane.
    fn publish_title(&mut self, title: &str) {
        use crate::terminal::{ScreenSnapshot, TerminalSessionEvent};

        self.screen_generation += 1;
        let screen = ScreenSnapshot::from_test_parts_at(
            Arc::from([]),
            Default::default(),
            title,
            self.screen_generation,
        );
        self.records
            .event_sender(1)
            .expect("the Pane started")
            .try_send(TerminalSessionEvent::Screen(screen))
            .expect("the Pane receives its screen");
        self.cx.run_until_parked();
    }

    fn resizes(&self) -> usize {
        self.records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedCommand::Resize(_)))
            .count()
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

    /// Whether the server-backed Choose Pane Directory picker is on screen.
    fn directory_picker_open(&mut self) -> bool {
        use spaceterm_ui::a11y_testing::A11yTree;

        A11yTree::read(self.cx)
            .find("Choose Pane Directory")
            .is_some()
    }

    /// The role of whatever holds the keyboard, or the empty string when nothing does.
    fn focused_role(&mut self) -> String {
        use spaceterm_ui::a11y_testing::A11yTree;

        A11yTree::read(self.cx)
            .focused()
            .and_then(|node| node["aria"]["role"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Offers recovery, reaches the directory screen, and opens its picker while the connection
    /// is interrupted, so the picker reads nothing from the owning server.
    fn open_directory_picker(&mut self) {
        self.connection.offer_directory_recovery();
        self.set(DevelopmentPaneStatus::Failed(
            ServerError::DirectoryUnavailable,
        ));
        // The connection drops between the window's last status read and this click.
        self.connection.set(DevelopmentPaneStatus::Reconnecting);
        self.click("development-pane-choose-directory");
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
fn native_terminal_accessibility_follows_the_development_pane_presentation(
    cx: &mut TestAppContext,
) {
    use crate::terminal::{
        CellSnapshot, ScreenSnapshot, TerminalAccessibilityModel, TerminalSessionEvent,
    };
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let mut pane =
        open_pane_with_session_options(DevelopmentPaneStatus::Connected, cx, |sessions| {
            sessions.with_reading_available(true)
        });
    let publish = |pane: &mut OpenPane<'_>, text: &str| {
        pane.screen_generation += 1;
        let screen = ScreenSnapshot::from_test_parts_at(
            Arc::from([text
                .chars()
                .map(|character| CellSnapshot {
                    text: character.to_string(),
                    foreground_source: Default::default(),
                    background_source: Default::default(),
                    inverse: false,
                    bold: false,
                    faint: false,
                    italic: false,
                    blinking: false,
                    invisible: false,
                    underline: Default::default(),
                    underline_source: Default::default(),
                    strikethrough: false,
                    overline: false,
                    selected: false,
                    spacer_tail: false,
                    semantic_content: Default::default(),
                    hyperlink: None,
                })
                .collect::<Vec<_>>()
                .into()]),
            Default::default(),
            WINDOW_TITLE,
            pane.screen_generation,
        );
        pane.records
            .event_sender(1)
            .unwrap()
            .try_send(TerminalSessionEvent::Screen(Arc::clone(&screen)))
            .unwrap();
        pane.records
            .last_accessibility_sender()
            .unwrap()
            .force_send(Arc::new(TerminalAccessibilityModel::from_screen(&screen)))
            .unwrap();
        pane.cx.run_until_parked();
    };
    publish(&mut pane, "initial output");
    let tree = A11yTree::read(pane.cx);
    assert_eq!(tree.node("Terminal Pane")["aria"]["role"], "Terminal");
    assert_eq!(
        tree.with_role("TextRun")[0]["aria"]["value"],
        "initial output"
    );
    pane.cx.update(|window, cx| window.blur(cx));
    perform(pane.cx, tree.node("Terminal Pane"), Action::Focus);
    assert_eq!(pane.focused_role(), "Terminal");

    pane.set(DevelopmentPaneStatus::Reconnecting);
    pane.cx.update(|window, cx| window.blur(cx));
    let tree = A11yTree::read(pane.cx);
    assert_eq!(
        tree.with_role("TextRun")[0]["aria"]["value"],
        "initial output"
    );
    perform(pane.cx, tree.node("Terminal Pane"), Action::Focus);
    assert_ne!(
        pane.focused_role(),
        "Terminal",
        "frozen text grants no native focus"
    );
    pane.reconnect();

    for status in [
        DevelopmentPaneStatus::Takeover,
        DevelopmentPaneStatus::Restored,
        DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable),
    ] {
        pane.set(status);
        let tree = A11yTree::read(pane.cx);
        assert!(tree.with_role("Terminal").is_empty(), "{status:?}");
        assert!(tree.with_role("TextRun").is_empty(), "{status:?}");
        assert_ne!(pane.focused_role(), "Terminal");
        pane.set(DevelopmentPaneStatus::Reconnecting);
        assert!(A11yTree::read(pane.cx).with_role("Terminal").is_empty());

        pane.reconnect();
        publish(&mut pane, "fresh output");
        let tree = A11yTree::read(pane.cx);
        assert_eq!(
            tree.with_role("TextRun")[0]["aria"]["value"],
            "fresh output"
        );
        pane.cx.update(|window, cx| window.blur(cx));
        perform(pane.cx, tree.node("Terminal Pane"), Action::Focus);
        assert_eq!(pane.focused_role(), "Terminal");
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
    assert_eq!(
        pane.connection.end_session_generations(),
        vec![0],
        "the request carries the approved connection generation"
    );
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
    assert_eq!(
        pane.connection.end_session_generations(),
        vec![0, 1, 2],
        "each request carries the generation it was approved on"
    );
}

#[gpui::test]
fn an_end_session_approval_never_reaches_a_connection_that_arrives_while_it_is_sent(
    cx: &mut TestAppContext,
) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.click(END_SESSION_SELECTOR);
    let approved = pane.connection.generation();

    // A new connection reaches the Session after the window validated the approval and before
    // the request leaves it.
    pane.connection.reconnect_after_next_read();
    pane.click("modal-action-development-pane-end-session-confirm");

    assert_ne!(pane.connection.generation(), approved);
    assert_eq!(
        pane.connection.end_session_generations(),
        vec![approved],
        "the request names the approved generation, so the new connection ends nothing"
    );
}

#[gpui::test]
fn end_session_waits_for_a_connection(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Reconnecting, cx);

    pane.click(END_SESSION_SELECTOR);

    assert!(!pane.rendered("modal-action-development-pane-end-session-confirm"));
    assert_eq!(pane.connection.end_sessions(), 0);
}

#[gpui::test]
fn a_server_backed_pane_pastes_text_without_local_terminal_operations(cx: &mut TestAppContext) {
    use crate::ui::{OpenTerminalFind, PasteClipboard, ScrollPageUp};

    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);

    pane.cx
        .write_to_clipboard(gpui::ClipboardItem::new_string("echo hello".into()));
    pane.cx.dispatch_action(PasteClipboard);
    pane.cx.run_until_parked();
    let pastes = pane
        .records
        .commands()
        .into_iter()
        .filter_map(|call| match call.command {
            RecordedCommand::RequestPaste(text) => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(pastes, ["echo hello"]);
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
                    | RecordedCommand::RequestPaste(_)
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
fn unsupported_terminal_input_keeps_output_and_session_controls_available(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;
    let mut pane =
        open_pane_with_session_options(DevelopmentPaneStatus::Connected, cx, |sessions| {
            sessions.with_interactive_input_available(false)
        });
    pane.publish_title("running-build");
    assert!(pane.rendered(TERMINAL_SELECTOR));
    assert!(pane.rendered(TERMINAL_TITLE_SELECTOR));
    assert!(pane.rendered("terminal-input-unavailable"));
    assert!(!pane.rendered(RETRY_SELECTOR));
    let tree = A11yTree::read(pane.cx);
    assert_eq!(tree.node("Typing Unavailable")["aria"]["role"], "Status");
    assert_eq!(
        tree.node("Typing Unavailable")["aria"]["description"],
        "Typing Unavailable This server doesn't accept terminal input from this version of SpaceTerm. The shell keeps running on the server."
    );
    pane.press("a");
    pane.press("cmd-v");
    assert_eq!(pane.keys(), 0);
    pane.click(END_SESSION_SELECTOR);
    assert!(pane.rendered("modal-action-development-pane-end-session-confirm"));
    pane.press("escape");
    pane.lose_control();
    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(!pane.rendered("terminal-input-unavailable"));
    pane.click(TAKE_OVER_SELECTOR);
    assert_eq!(pane.connection.take_overs(), 1);
}

#[gpui::test]
fn reconnecting_notice_takes_precedence_over_unsupported_terminal_input(cx: &mut TestAppContext) {
    let mut pane =
        open_pane_with_session_options(DevelopmentPaneStatus::Connected, cx, |sessions| {
            sessions.with_interactive_input_available(false)
        });
    pane.publish_title("running-build");
    assert!(pane.rendered("terminal-input-unavailable"));
    pane.set(DevelopmentPaneStatus::Reconnecting);
    assert!(pane.rendered(NOTICE_SELECTOR));
    assert!(pane.rendered(RETRY_SELECTOR));
    assert!(!pane.rendered("terminal-input-unavailable"));
    pane.click(RETRY_SELECTOR);
    assert_eq!(pane.connection.retries(), 1);
    pane.reconnect_to(DevelopmentPaneStatus::Connected);
    assert!(pane.rendered("terminal-input-unavailable"));
    assert!(
        pane.rendered(TERMINAL_TITLE_SELECTOR),
        "the unsupported connection still presents the shell's output"
    );
}

#[gpui::test]
fn server_state_changes_cancel_paste_confirmation_before_any_later_approval(
    cx: &mut TestAppContext,
) {
    use crate::terminal::{
        PasteConfirmation, PasteConfirmationId, PasteDecision, PasteRequestOutcome, PasteRisk,
    };
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};
    let confirmation = PasteConfirmation {
        id: PasteConfirmationId::new(544),
        byte_len: 12,
        line_count: 2,
        risk: PasteRisk {
            multiline: true,
            control_bytes: false,
            closing_fence: false,
        },
    };
    let mut pane =
        open_pane_with_session_options(DevelopmentPaneStatus::Connected, cx, |sessions| {
            sessions
                .with_paste_response(Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)))
        });
    for status in [
        DevelopmentPaneStatus::Reconnecting,
        DevelopmentPaneStatus::Takeover,
        DevelopmentPaneStatus::Restored,
    ] {
        pane.cx
            .write_to_clipboard(gpui::ClipboardItem::new_string("first\nsecond".into()));
        pane.cx.dispatch_action(crate::ui::PasteClipboard);
        let tree = A11yTree::read(pane.cx);
        let confirm = tree.node("Paste").clone();
        assert_eq!(tree.with_role("AlertDialog").len(), 1);
        let before = pane.records.commands().len();
        if status == DevelopmentPaneStatus::Takeover {
            pane.lose_control();
        } else {
            pane.set(status);
        }
        assert!(
            A11yTree::read(pane.cx).with_role("AlertDialog").is_empty(),
            "{status:?}"
        );
        perform(pane.cx, &confirm, Action::Click);
        pane.press("enter");
        let resolutions = pane.records.commands()[before..]
            .iter()
            .filter_map(|call| match call.command {
                RecordedCommand::ResolvePaste(id, decision) => Some((id, decision)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            resolutions,
            [(confirmation.id, PasteDecision::Cancel)],
            "{status:?}"
        );
        pane.reconnect_to(DevelopmentPaneStatus::Connected);
    }
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
    pane.press("cmd-alt-w");
    assert!(pane.rendered("modal-action-development-pane-end-session-confirm"));
    pane.press("escape");
    assert!(!pane.rendered("modal-action-development-pane-end-session-confirm"));
    assert_eq!(pane.connection.end_sessions(), 0, "Escape keeps the shell");
    assert!(!pane.closed());

    // The confirmation starts on Cancel, so Return keeps the shell and Tab reaches End Session.
    pane.press("cmd-alt-w");
    pane.press("enter");
    assert_eq!(pane.connection.end_sessions(), 0, "Return keeps the shell");
    pane.press("cmd-alt-w");
    pane.press("tab");
    pane.press("space");
    assert_eq!(pane.connection.end_sessions(), 1);
}

#[gpui::test]
fn another_client_taking_control_replaces_the_pane_with_the_takeover_screen(
    cx: &mut TestAppContext,
) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.publish_title("secret-build");
    assert!(pane.rendered(TERMINAL_TITLE_SELECTOR));
    let delivered = pane.keys();

    pane.lose_control();

    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(pane.rendered(TAKE_OVER_SELECTOR));
    assert!(
        !pane.rendered(TERMINAL_SELECTOR),
        "no part of the Pane paints"
    );
    assert!(
        !pane.rendered(TERMINAL_TITLE_SELECTOR),
        "the shell's title belongs to the Controlling Client"
    );
    assert!(
        !pane.rendered(NOTICE_SELECTOR),
        "control moved; nothing failed"
    );
    pane.cx.simulate_keystrokes("a enter");
    assert_eq!(pane.keys(), delivered, "typing reaches no shell");
    pane.click(END_SESSION_SELECTOR);
    assert!(!pane.rendered("modal-action-development-pane-end-session-confirm"));
    assert_eq!(
        pane.connection.take_overs(),
        0,
        "nothing takes control back unasked"
    );
    assert_eq!(
        pane.records.session_count(),
        1,
        "the Terminal Session continues"
    );
    assert!(!pane.closed());
}

#[gpui::test]
fn assistive_technology_reaches_take_over_and_no_terminal_content(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    const TERMINAL_ACTIONS: &str = "Terminal context actions";

    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    // The Pane's context actions stand for everything the terminal publishes.
    assert!(A11yTree::read(pane.cx).find(TERMINAL_ACTIONS).is_some());

    pane.lose_control();

    let tree = A11yTree::read(pane.cx);
    let screen = tree.node("Controlled by Another Client");
    assert_eq!(screen["aria"]["role"], "Group");
    assert!(
        tree.node(&format!(
            "Controlled by Another Client. {}",
            server_pane::TAKEOVER_DETAIL
        ))["aria"]["value"]
            .as_str()
            .is_some_and(|description| description.contains("Take Over")),
        "the screen explains what Take Over does"
    );
    assert!(
        tree.find(TERMINAL_ACTIONS).is_none(),
        "the Pane leaves the tree"
    );
    let take_over = tree.node("Take Over");
    assert_eq!(take_over["aria"]["role"], "Button");
    assert!(tree.exposed(take_over));
    perform(pane.cx, take_over, gpui::accesskit::Action::Click);
    pane.cx.run_until_parked();
    assert_eq!(pane.connection.take_overs(), 1);

    let tree = A11yTree::read(pane.cx);
    let progress = tree.node("Taking Over…");
    assert_eq!(progress["aria"]["role"], "ProgressIndicator");
    assert_eq!(tree.node("Take Over")["aria"]["disabled"], true);
}

#[gpui::test]
fn the_keyboard_reaches_take_over_but_typing_never_presses_it(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.lose_control();

    // Someone typing into the shell when control moves keeps typing into nothing.
    pane.press("space");
    pane.press("enter");
    pane.press("x");
    assert_eq!(pane.connection.take_overs(), 0);

    // End Session waits for control, so Tab passes over it to Take Over.
    pane.press("tab");
    pane.press("space");
    assert_eq!(pane.connection.take_overs(), 1);
    assert!(
        pane.rendered(TAKE_OVER_STATUS_SELECTOR),
        "progress shows at once"
    );

    // Take Over waits for the server, so pressing again sends nothing more.
    pane.press("space");
    pane.press("enter");
    assert_eq!(pane.connection.take_overs(), 1);
}

#[gpui::test]
fn take_over_returns_control_with_a_fresh_screen_only(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.publish_title("secret-build");
    pane.lose_control();
    let resizes = pane.resizes();

    pane.click(TAKE_OVER_SELECTOR);
    assert_eq!(pane.connection.take_overs(), 1);
    assert!(pane.rendered(TAKE_OVER_STATUS_SELECTOR));
    assert!(
        !pane.rendered(TERMINAL_SELECTOR),
        "the Pane waits for the server's screen"
    );

    pane.reconnect();

    assert!(!pane.rendered(TAKEOVER_SELECTOR));
    assert!(pane.rendered(TERMINAL_SELECTOR));
    assert!(
        !pane.rendered(TERMINAL_TITLE_SELECTOR),
        "the former title never returns"
    );
    assert!(
        pane.resizes() > resizes,
        "the new Controlling Client supplies its dimensions"
    );
    let delivered = pane.keys();
    pane.cx.simulate_keystrokes("a");
    assert!(pane.keys() > delivered, "typing reaches the shell again");
    pane.publish_title("fresh-build");
    assert!(pane.rendered(TERMINAL_TITLE_SELECTOR));
    assert_eq!(
        pane.records.session_count(),
        1,
        "the same Terminal Session continues"
    );
}

#[gpui::test]
fn an_unconfirmed_take_over_says_so_and_can_be_tried_again(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.lose_control();
    pane.click(TAKE_OVER_SELECTOR);

    // The window reads another Controlling Client on the same connection, with no grant.
    pane.set(DevelopmentPaneStatus::Takeover);

    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(
        pane.rendered(TAKE_OVER_STATUS_SELECTOR),
        "the missing confirmation is explained"
    );
    pane.click(TAKE_OVER_SELECTOR);
    assert_eq!(pane.connection.take_overs(), 2);
}

#[gpui::test]
fn losing_control_again_after_a_granted_take_over_is_not_unconfirmed(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.lose_control();
    pane.click(TAKE_OVER_SELECTOR);

    // Control arrives and moves to another client again between two reads.
    pane.connection.reconnect();
    pane.lose_control();

    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(!pane.rendered(TAKE_OVER_STATUS_SELECTOR));
    assert!(!pane.rendered(TERMINAL_SELECTOR));
}

#[gpui::test]
fn the_takeover_screen_stays_through_an_interruption(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.publish_title("secret-build");
    pane.lose_control();

    pane.set(DevelopmentPaneStatus::Reconnecting);

    assert!(
        pane.rendered(TAKEOVER_SELECTOR),
        "the former screen never returns"
    );
    assert!(!pane.rendered(TERMINAL_SELECTOR));
    assert!(!pane.rendered(TERMINAL_TITLE_SELECTOR));
    assert!(pane.rendered(NOTICE_SELECTOR));
    pane.click(TAKE_OVER_SELECTOR);
    assert_eq!(
        pane.connection.take_overs(),
        0,
        "Take Over waits for the server"
    );
    pane.click(RETRY_SELECTOR);
    assert_eq!(pane.connection.retries(), 1);

    // The server still names another Controlling Client after reconnecting.
    pane.set(DevelopmentPaneStatus::Takeover);
    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(!pane.rendered(NOTICE_SELECTOR));
    pane.click(TAKE_OVER_SELECTOR);
    assert_eq!(pane.connection.take_overs(), 1);
}

#[gpui::test]
fn losing_the_server_while_another_client_controls_keeps_the_takeover_screen(
    cx: &mut TestAppContext,
) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.publish_title("secret-build");

    // The server reports another Controlling Client only as the connection fails.
    pane.connection
        .set(DevelopmentPaneStatus::TakeoverReconnecting);
    pane.cx.executor().advance_clock(STATUS_INTERVAL);
    pane.cx.run_until_parked();

    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(!pane.rendered(TERMINAL_SELECTOR));
    assert!(!pane.rendered(TERMINAL_TITLE_SELECTOR));
    assert!(pane.rendered(NOTICE_SELECTOR));
    pane.click(TAKE_OVER_SELECTOR);
    assert_eq!(
        pane.connection.take_overs(),
        0,
        "Take Over waits for the server"
    );
    pane.click(RETRY_SELECTOR);
    assert_eq!(pane.connection.retries(), 1);
}

#[gpui::test]
fn an_interrupted_take_over_says_it_was_not_confirmed(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.lose_control();
    pane.click(TAKE_OVER_SELECTOR);

    pane.set(DevelopmentPaneStatus::TakeoverReconnecting);
    assert!(
        pane.rendered(TAKE_OVER_STATUS_SELECTOR),
        "the attempt is not left spinning"
    );
    assert!(pane.rendered(NOTICE_SELECTOR));

    pane.set(DevelopmentPaneStatus::Takeover);
    assert!(pane.rendered(TAKE_OVER_STATUS_SELECTOR));
    pane.click(TAKE_OVER_SELECTOR);
    assert_eq!(
        pane.connection.take_overs(),
        2,
        "Take Over is offered again"
    );
}

#[gpui::test]
fn opening_a_pane_another_client_controls_shows_the_takeover_screen(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connecting, cx);
    assert_eq!(
        pane.records.session_count(),
        1,
        "the Pane measured and started"
    );

    pane.set(DevelopmentPaneStatus::Takeover);

    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(!pane.rendered(TERMINAL_SELECTOR));
    pane.cx.simulate_keystrokes("a");
    assert_eq!(pane.keys(), 0);
}

#[gpui::test]
fn an_end_session_approval_never_outlives_control(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.click(END_SESSION_SELECTOR);
    assert!(pane.rendered("modal-action-development-pane-end-session-confirm"));

    pane.lose_control();
    assert!(
        !pane.rendered("modal-action-development-pane-end-session-confirm"),
        "the confirmation closes with control"
    );
    pane.click(TAKE_OVER_SELECTOR);
    pane.reconnect();
    assert!(!pane.rendered("modal-action-development-pane-end-session-confirm"));
    assert_eq!(pane.connection.end_sessions(), 0);

    // Control moves away and back before the window reads it, then the person confirms.
    pane.click(END_SESSION_SELECTOR);
    pane.connection.lose_control();
    pane.connection.reconnect();
    pane.click("modal-action-development-pane-end-session-confirm");
    assert_eq!(
        pane.connection.end_sessions(),
        0,
        "approval given to the former grant never applies"
    );
    assert!(!pane.closed());

    // Once the window reads the new grant, End Session asks again.
    pane.cx.executor().advance_clock(STATUS_INTERVAL);
    pane.cx.run_until_parked();
    pane.click(END_SESSION_SELECTOR);
    pane.click("modal-action-development-pane-end-session-confirm");
    assert_eq!(
        pane.connection.end_session_generations(),
        vec![pane.connection.generation()],
        "only the current grant's approval reaches the server"
    );
}

#[gpui::test]
fn a_session_ended_while_another_client_controls_it_says_so_without_its_content(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.publish_title("secret-build");
    pane.lose_control();
    let delivered = pane.keys();

    // Another client ends the Session while this window shows the Takeover Screen.
    pane.set(DevelopmentPaneStatus::TakeoverFailed(
        ServerError::SessionUnavailable,
    ));

    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(
        !pane.rendered(TERMINAL_SELECTOR),
        "no part of the Pane paints"
    );
    assert!(!pane.rendered(TERMINAL_TITLE_SELECTOR));
    assert!(
        !pane.rendered(NOTICE_SELECTOR),
        "nothing claims SpaceTerm is reconnecting"
    );
    assert!(!pane.rendered(RETRY_SELECTOR), "retrying finds no Session");
    assert!(
        !pane.rendered(TAKE_OVER_SELECTOR),
        "nothing is left to take over"
    );
    let tree = A11yTree::read(pane.cx);
    let screen = tree.node("Session Ended");
    assert_eq!(screen["aria"]["role"], "Group");
    let description = tree.node(&format!(
        "Session Ended. {}",
        server_pane::SESSION_ENDED_DETAIL
    ))["aria"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(description.contains("no longer available"));
    assert!(
        !description.contains("keeps running"),
        "the shell no longer runs"
    );
    assert_eq!(
        tree.node("Session Ended. This Terminal Session is no longer available on the server.")["aria"]
            ["role"],
        "Status",
        "the end is announced"
    );
    assert!(tree.find("Controlled by Another Client").is_none());
    assert!(tree.find("Reconnecting…").is_none());
    assert!(tree.find("Terminal context actions").is_none());
    assert!(tree.exposed(tree.node("Close Window")));

    // Typing reaches nothing, and nothing restarts or ends a Session.
    pane.press("space");
    pane.press("enter");
    pane.press("x");
    assert_eq!(pane.keys(), delivered);
    assert!(!pane.closed(), "typing never closes the window");
    assert_eq!(pane.connection.take_overs(), 0);
    assert_eq!(pane.connection.retries(), 0);
    assert_eq!(pane.connection.end_sessions(), 0);
    assert_eq!(pane.records.session_count(), 1, "no Session starts");

    // End Session has nothing to end, so Tab passes over it to Close Window.
    pane.press("tab");
    pane.press("space");
    assert!(pane.closed());
    assert_eq!(pane.connection.end_sessions(), 0);
}

#[gpui::test]
fn close_pane_closes_the_window_once_the_session_ended(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.lose_control();
    pane.set(DevelopmentPaneStatus::TakeoverFailed(
        ServerError::SessionUnavailable,
    ));

    // Close Pane acts on key down, so the window is gone before the key comes back up.
    pane.cx.simulate_keystrokes("cmd-alt-w");
    pane.cx.run_until_parked();

    assert!(pane.closed());
    assert_eq!(pane.connection.end_sessions(), 0);
}

#[gpui::test]
fn a_permanent_failure_after_losing_control_explains_itself_without_content(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.publish_title("secret-build");
    let delivered = pane.keys();

    // The window first learns of the takeover from a permanent failure.
    pane.connection.lose_control();
    pane.set(DevelopmentPaneStatus::TakeoverFailed(
        ServerError::AuthenticationRefused,
    ));

    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(
        !pane.rendered(TERMINAL_SELECTOR),
        "no part of the Pane paints"
    );
    assert!(!pane.rendered(TERMINAL_TITLE_SELECTOR));
    let tree = A11yTree::read(pane.cx);
    assert!(tree.find("Terminal context actions").is_none());
    assert!(
        tree.find("Reconnecting…").is_none(),
        "SpaceTerm stopped trying"
    );
    let notice = tree.node("Not Connected");
    assert_eq!(notice["aria"]["role"], "Status");
    assert!(
        notice["aria"]["description"]
            .as_str()
            .is_some_and(|description| description
                .contains("did not pass the account and server identity checks")),
        "the notice names the failure"
    );
    let screen = tree.node(&format!(
        "Controlled by Another Client. {}",
        server_pane::TAKEOVER_FAILED_DETAIL
    ));
    assert!(
        !screen["aria"]["value"]
            .as_str()
            .unwrap_or_default()
            .contains("keeps running"),
        "nothing claims the shell runs while SpaceTerm cannot reach it"
    );
    assert_eq!(tree.node("Take Over")["aria"]["disabled"], true);

    pane.cx.simulate_keystrokes("a enter");
    assert_eq!(pane.keys(), delivered, "typing reaches no shell");
    pane.click(TAKE_OVER_SELECTOR);
    assert_eq!(pane.connection.take_overs(), 0);
    pane.click(RETRY_SELECTOR);
    assert_eq!(pane.connection.retries(), 1);

    // A retry that reaches the server finds another Controlling Client again.
    pane.set(DevelopmentPaneStatus::Takeover);
    assert!(!pane.rendered(NOTICE_SELECTOR));
    assert!(!pane.rendered(TERMINAL_SELECTOR));
    pane.click(TAKE_OVER_SELECTOR);
    assert_eq!(pane.connection.take_overs(), 1);
}

#[gpui::test]
fn a_restored_pane_starts_a_shell_only_when_asked(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connecting, cx);
    pane.set(DevelopmentPaneStatus::Restored);

    assert!(pane.rendered(RESTORED_SELECTOR));
    assert!(pane.rendered(START_SHELL_SELECTOR));
    assert!(!pane.rendered(TERMINAL_SELECTOR), "no former screen paints");
    assert!(
        !pane.rendered(NOTICE_SELECTOR),
        "the server answered; nothing failed"
    );
    assert!(
        !pane.rendered("development-pane-status"),
        "the screen reports its own progress"
    );

    // Typing meant for the former shell, focus, and reconnection start nothing.
    pane.press("space");
    pane.press("enter");
    pane.press("x");
    pane.cx.update(|window, _| window.activate_window());
    pane.set(DevelopmentPaneStatus::Reconnecting);
    pane.click(RETRY_SELECTOR);
    pane.set(DevelopmentPaneStatus::Restored);
    pane.click(END_SESSION_SELECTOR);
    assert!(!pane.rendered("modal-action-development-pane-end-session-confirm"));
    assert_eq!(pane.connection.restored_starts(), 0);
    assert_eq!(pane.connection.retries(), 1, "Retry only reconnects");
    assert_eq!(pane.keys(), 0, "typing reaches no shell");

    pane.click(START_SHELL_SELECTOR);
    assert_eq!(pane.connection.restored_starts(), 1);
    assert!(
        pane.rendered(START_SHELL_STATUS_SELECTOR),
        "progress shows at once"
    );
    pane.click(START_SHELL_SELECTOR);
    assert_eq!(
        pane.connection.restored_starts(),
        1,
        "Start New Shell waits for the server"
    );

    // The fresh shell's first screen returns the Pane, and typing reaches it.
    pane.reconnect();
    assert!(!pane.rendered(RESTORED_SELECTOR));
    assert!(pane.rendered(TERMINAL_SELECTOR));
    pane.cx.simulate_keystrokes("a");
    assert!(pane.keys() > 0, "typing reaches the fresh shell");
    assert_eq!(pane.connection.restored_starts(), 1);
}

#[gpui::test]
fn a_server_restart_forgets_the_former_screen(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.publish_title("secret-build");
    assert!(pane.rendered(TERMINAL_TITLE_SELECTOR));

    // The server stops while the window stays open, then returns with the Pane dormant.
    pane.set(DevelopmentPaneStatus::Reconnecting);
    pane.reconnect_to(DevelopmentPaneStatus::Restored);

    assert!(pane.rendered(RESTORED_SELECTOR));
    assert!(!pane.rendered(TERMINAL_SELECTOR));
    assert!(
        !pane.rendered(TERMINAL_TITLE_SELECTOR),
        "the ended shell's title never shows"
    );
    let tree = A11yTree::read(pane.cx);
    assert!(
        tree.find("Terminal context actions").is_none(),
        "the Pane leaves the tree"
    );
    let screen = tree.node("Server Restarted");
    assert_eq!(screen["aria"]["role"], "Group");
    let description = tree.node(&format!(
        "Server Restarted. {}",
        server_pane::RESTORED_DETAIL
    ))["aria"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(description.contains("ended"), "the former programs ended");
    assert!(
        description.contains("new shell"),
        "the screen says a new shell starts"
    );
    assert!(
        !description.contains("keeps running"),
        "nothing claims a shell runs"
    );

    pane.click(START_SHELL_SELECTOR);
    pane.reconnect();
    assert!(pane.rendered(TERMINAL_SELECTOR));
    assert!(
        !pane.rendered(TERMINAL_TITLE_SELECTOR),
        "the former title never returns"
    );
    pane.publish_title("fresh-shell");
    assert!(pane.rendered(TERMINAL_TITLE_SELECTOR));
}

#[gpui::test]
fn assistive_technology_reaches_start_new_shell(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let pane = open_pane(DevelopmentPaneStatus::Restored, cx);

    let tree = A11yTree::read(pane.cx);
    assert_eq!(
        tree.node(
            "Server Restarted. Programs that were running in this Pane ended when the server stopped. Start a new \
             shell to keep working here."
        )["aria"]["role"],
        "Status",
        "the restart is announced"
    );
    let start = tree.node("Start New Shell");
    assert_eq!(start["aria"]["role"], "Button");
    assert!(tree.exposed(start));
    perform(pane.cx, start, gpui::accesskit::Action::Click);
    pane.cx.run_until_parked();
    assert_eq!(pane.connection.restored_starts(), 1);

    let tree = A11yTree::read(pane.cx);
    assert_eq!(
        tree.node("Starting Shell…")["aria"]["role"],
        "ProgressIndicator"
    );
    assert_eq!(tree.node("Start New Shell")["aria"]["disabled"], true);
}

#[gpui::test]
fn the_keyboard_reaches_start_new_shell_but_typing_never_presses_it(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.set(DevelopmentPaneStatus::Reconnecting);
    pane.reconnect_to(DevelopmentPaneStatus::Restored);

    // Someone typing into the shell when the server restarts keeps typing into nothing.
    pane.press("space");
    pane.press("enter");
    pane.press("x");
    assert_eq!(pane.connection.restored_starts(), 0);

    // End Session has no shell to end, so Tab passes over it to Start New Shell.
    pane.press("tab");
    pane.press("space");
    assert_eq!(pane.connection.restored_starts(), 1);

    // Start New Shell waits for the server, so pressing again sends nothing more.
    pane.press("space");
    pane.press("enter");
    assert_eq!(pane.connection.restored_starts(), 1);
    assert_eq!(pane.keys(), 0);
}

#[gpui::test]
fn an_interrupted_start_says_it_was_not_confirmed_and_can_be_tried_again(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Restored, cx);
    pane.click(START_SHELL_SELECTOR);

    pane.set(DevelopmentPaneStatus::Reconnecting);
    assert!(pane.rendered(RESTORED_SELECTOR), "no former screen returns");
    assert!(pane.rendered(NOTICE_SELECTOR));
    assert!(
        pane.rendered(START_SHELL_STATUS_SELECTOR),
        "the attempt is not left spinning"
    );
    pane.click(START_SHELL_SELECTOR);
    assert_eq!(
        pane.connection.restored_starts(),
        1,
        "Start New Shell waits for the server"
    );

    pane.set(DevelopmentPaneStatus::Restored);
    assert!(pane.rendered(START_SHELL_STATUS_SELECTOR));
    pane.click(START_SHELL_SELECTOR);
    assert_eq!(
        pane.connection.restored_starts(),
        2,
        "Start New Shell is offered again"
    );
}

#[gpui::test]
fn an_interruption_leaves_the_open_recovery_picker_holding_the_keyboard(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.open_directory_picker();
    assert!(pane.directory_picker_open());
    assert_eq!(pane.focused_role(), "TextInput");

    // The window reads the interruption, then a new connection answers Directory Unavailable
    // again. The picker stays open across both, so the keyboard must stay in it.
    pane.set(DevelopmentPaneStatus::Reconnecting);
    assert!(pane.directory_picker_open());
    assert_eq!(pane.focused_role(), "TextInput");
    pane.reconnect_to(DevelopmentPaneStatus::Failed(
        ServerError::DirectoryUnavailable,
    ));
    assert!(
        pane.directory_picker_open(),
        "the reconnected screen never takes focus from the picker, which closes without it"
    );
    assert_eq!(pane.focused_role(), "TextInput");

    // A further connection answers the same status, so only the generation changed.
    pane.reconnect_to(DevelopmentPaneStatus::Failed(
        ServerError::DirectoryUnavailable,
    ));
    assert!(pane.directory_picker_open());
    assert_eq!(pane.focused_role(), "TextInput");

    // Escape reaches the picker, which dismisses and hands focus back to the window.
    pane.press("escape");
    assert!(!pane.directory_picker_open());
    assert_ne!(pane.focused_role(), "TextInput");
    assert!(pane.rendered("development-pane-directory-retry"));
}

#[gpui::test]
fn a_stale_recovery_target_closes_the_picker_and_returns_the_keyboard(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Connected, cx);
    pane.open_directory_picker();
    assert!(pane.directory_picker_open());

    // The Pane is no longer waiting on its directory, so nothing is left to repair.
    pane.set(DevelopmentPaneStatus::Restored);

    assert!(!pane.directory_picker_open());
    assert_ne!(pane.focused_role(), "TextInput");
    assert!(pane.rendered(START_SHELL_SELECTOR));
}

#[gpui::test]
fn a_start_lost_between_two_reads_is_offered_again(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Restored, cx);
    pane.click(START_SHELL_SELECTOR);

    // The connection drops and returns to the dormant Pane before the window reads it.
    pane.reconnect_to(DevelopmentPaneStatus::Restored);

    assert!(pane.rendered(START_SHELL_STATUS_SELECTOR));
    pane.click(START_SHELL_SELECTOR);
    assert_eq!(pane.connection.restored_starts(), 2);
}

#[gpui::test]
fn an_unavailable_directory_offers_retry_without_exposing_terminal_content(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;
    let mut pane = open_pane(DevelopmentPaneStatus::Restored, cx);
    pane.click(START_SHELL_SELECTOR);
    pane.set(DevelopmentPaneStatus::Failed(
        ServerError::DirectoryUnavailable,
    ));
    assert!(pane.rendered(RESTORED_SELECTOR));
    assert!(!pane.rendered(TERMINAL_SELECTOR));
    assert!(!pane.rendered(NOTICE_SELECTOR));
    let tree = A11yTree::read(pane.cx);
    assert_eq!(tree.node("Directory Unavailable")["aria"]["role"], "Group");
    assert!(tree.node("Directory Unavailable")["aria"]["description"].is_null());
    let statuses = tree.with_role("Status");
    assert_eq!(statuses.len(), 2);
    assert!(statuses.iter().any(|node| node["aria"]["value"]
        == format!(
            "Directory Unavailable. {}",
            crate::ui::server_pane::DIRECTORY_UNAVAILABLE_DETAIL
        )));
    assert!(statuses.iter().any(|node| node["aria"]["value"] == ""));
    assert!(tree.find("Retry").is_some());
    assert!(tree.find("Choose Directory…").is_some());
    assert!(tree.find("Close Window").is_none());
    assert!(tree.find("Terminal context actions").is_none());
    pane.press("space");
    pane.press("enter");
    pane.press("x");
    assert_eq!(pane.keys(), 0);
    assert_eq!(pane.connection.restored_starts(), 1);
    pane.click("development-pane-directory-retry");
    assert_eq!(pane.connection.restored_starts(), 2);
    pane.click("development-pane-directory-retry");
    assert_eq!(
        pane.connection.restored_starts(),
        2,
        "a pending Retry submits once"
    );
    assert!(!pane.closed());
}

#[gpui::test]
fn close_pane_closes_the_window_while_no_shell_runs(cx: &mut TestAppContext) {
    let pane = open_pane(DevelopmentPaneStatus::Restored, cx);

    // Close Pane acts on key down, so the window is gone before the key comes back up.
    pane.cx.simulate_keystrokes("cmd-alt-w");
    pane.cx.run_until_parked();

    assert!(pane.closed());
    assert_eq!(pane.connection.end_sessions(), 0);
    assert_eq!(pane.connection.restored_starts(), 0);
}

#[gpui::test]
fn another_client_starting_the_shell_shows_the_takeover_screen(cx: &mut TestAppContext) {
    let mut pane = open_pane(DevelopmentPaneStatus::Restored, cx);

    pane.lose_control();

    assert!(pane.rendered(TAKEOVER_SELECTOR));
    assert!(!pane.rendered(RESTORED_SELECTOR));
    assert!(!pane.rendered(TERMINAL_SELECTOR));
    assert_eq!(pane.connection.restored_starts(), 0);
}
