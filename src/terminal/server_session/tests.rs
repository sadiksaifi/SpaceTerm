use super::link::{ActiveInput, present};
use super::*;

#[test]
fn paired_panes_have_no_local_file_authority_even_with_local_composition() {
    struct Route;
    impl spaceterm_server::direct::DirectConnector for Route {
        fn connect(
            &self,
        ) -> Result<spaceterm_server::direct::DirectConnection, spaceterm_server::direct::DirectError>
        {
            panic!("factory construction must not connect");
        }
    }
    let server = ServerConnection::direct(
        "spaceterm-development",
        [4; 32],
        spaceterm_server::direct::PairedDevice {
            pin: spaceterm_server::direct::ServerPin {
                build: "spaceterm-development".into(),
                server: [9; 32],
                public_key: [2; 32],
                certificate: [3; 32],
            },
            pairing: [5; 32],
            client: [6; 32],
        },
        Arc::new(Route),
    )
    .unwrap();
    let (factory, _) = DevelopmentServerSessionFactory::for_pane(
        server,
        &PaneControls::default(),
        crate::platform::runtime_local_filesystem(),
        [8; 32],
    );
    assert!(!factory.allows_local_file_insertion());
    assert!(factory.local_file_authority().is_none());
}

#[test]
fn clear_screen_and_scrollback_uses_the_current_epoch_and_exact_lease_without_focus() {
    let mut adapter = Adapter::new();
    adapter.handle.interactive_focus(false, false);
    adapter.handle.clear_screen_and_scrollback();
    let mut cleared = Vec::new();
    adapter
        .drain(|request| {
            match request {
                Request::ClearScreenAndScrollback { lease } => cleared.push(*lease),
                Request::Focus { .. } => {}
                _ => panic!("unexpected request: {request:?}"),
            }
            Ok(Reply::Unchanged)
        })
        .unwrap();
    assert_eq!(cleared, vec![adapter.active.lease]);

    adapter.handle.clear_screen_and_scrollback();
    adapter.handle.state.invalidate();
    adapter
        .drain(|_| panic!("clear from a retired epoch was replayed"))
        .unwrap();
    adapter.handle.clear_screen_and_scrollback();
    assert!(
        adapter.input.try_recv().is_err(),
        "disconnected clear was queued"
    );
}

#[test]
fn scrolling_reads_the_cache_without_input_focus_or_a_server_message() {
    let adapter = Adapter::new();
    adapter.frame(40);
    adapter.handle.interactive_focus(false, true);
    adapter.handle.scroll_scrollback(ScrollbackMovement::Top);
    assert!(adapter.input.try_recv().is_err(), "scrolling sends nothing");
    let shown = adapter.shown();
    assert_eq!(shown.viewport.offset_rows, 0);
    assert!(shown.scrollbar.total_rows > shown.scrollbar.visible_rows);
}

#[test]
fn copy_with_a_selection_is_local_and_without_one_forwards_through_the_fenced_input_path() {
    use super::super::pointer_input::*;
    let mut adapter = Adapter::new();
    let mapping = adapter.frame(0);
    let modifiers = InputModifiers {
        platform: true,
        ..Default::default()
    };
    assert!(adapter.handle.copy_or_forward(modifiers).unwrap().is_none());
    let mut forwarded = 0;
    adapter
        .drain(|request| {
            if matches!(
                request,
                Request::Input {
                    input: pane::InteractiveInput::CopyOrForward { modifiers: 8 },
                    ..
                }
            ) {
                forwarded += 1;
            }
            Ok(Reply::Unchanged)
        })
        .unwrap();
    assert_eq!(forwarded, 1, "the program decides what Copy without a Selection does");
    let pointer = |phase, x| PointerInput {
        generation: mapping.local,
        phase,
        button: Some(PointerButton::Left),
        position: SurfacePosition { x, y: 1.0 },
        modifiers: Default::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    };
    adapter.handle.pointer(pointer(PointerPhase::Press, 1.0));
    adapter.handle.pointer(pointer(PointerPhase::Motion, 45.0));
    let copy = adapter
        .handle
        .pointer_and_copy_selection(pointer(PointerPhase::Release, 45.0))
        .unwrap()
        .expect("a visible Selection copies at once");
    assert_eq!(copy.plain_text.as_ref(), "line");
    assert!(adapter.input.try_recv().is_err(), "selecting sends nothing");
    let copied = adapter.handle.copy_or_forward(modifiers).unwrap().unwrap();
    assert_eq!(copied.plain_text, copy.plain_text);
    assert!(adapter.input.try_recv().is_err(), "Copy of a Selection forwards nothing");
    // Typing clears the Selection, as in a local terminal.
    adapter.handle.key(KeyInput {
        action: KeyAction::Press,
        physical_key: PhysicalKey::A,
        native_key_code: None,
        logical_key: "a".into(),
        text: Some("a".into()),
        unshifted_codepoint: Some('a'),
        modifiers: InputModifiers::default(),
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    });
    assert!(adapter.handle.copy_selection().unwrap().is_none());
}

#[test]
fn a_wheel_measured_against_an_earlier_paint_of_the_same_grid_still_scrolls() {
    use super::super::pointer_input::*;
    let adapter = Adapter::new();
    let painted = adapter.frame(40);
    // Output arrives between the paint and the wheel.
    let current = ScreenMapping {
        local: PresentationGeneration::test(5),
        wire: 18,
        ..painted
    };
    adapter
        .handle
        .state
        .connected(adapter.active.epoch, current, false)
        .unwrap();
    adapter.handle.wheel(WheelInput {
        generation: painted.local,
        horizontal_steps: 0,
        vertical_steps: 3,
        phase: WheelPhase::GestureChanged,
        position: SurfacePosition { x: 1.0, y: 1.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    });
    let shown = adapter.shown();
    assert_eq!(
        shown.viewport.offset_rows + 3,
        shown.scrollbar.total_rows - shown.scrollbar.visible_rows
    );
    assert!(adapter.input.try_recv().is_err());
}

#[test]
fn a_copy_waiting_for_rows_wakes_the_worker_to_request_them() {
    use super::super::pointer_input::*;
    let adapter = Adapter::new();
    let mapping = adapter.frame(40);
    adapter.handle.scroll_scrollback(ScrollbackMovement::Top);
    let pointer = |phase, y| PointerInput {
        generation: mapping.local,
        phase,
        button: Some(PointerButton::Left),
        position: SurfacePosition { x: 1.0, y },
        modifiers: Default::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    };
    adapter.handle.pointer(pointer(PointerPhase::Press, 1.0));
    adapter.handle.pointer(pointer(PointerPhase::Motion, 61.0));
    adapter.handle.pointer(pointer(PointerPhase::Release, 61.0));
    // Consume the wakes the gesture caused.
    adapter
        .handle
        .state
        .wake
        .wait(Some(std::time::Instant::now()));
    assert!(adapter.handle.copy_selection().unwrap().is_none(), "no row is cached yet");
    let started = std::time::Instant::now();
    adapter
        .handle
        .state
        .wake
        .wait(Some(started + Duration::from_secs(5)));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the worker was not woken to request the selected rows"
    );
    assert!(
        !adapter
            .handle
            .state
            .reading
            .lock()
            .unwrap()
            .scrollback
            .requests()
            .is_empty()
    );
}

#[test]
fn copy_readiness_is_a_flag_the_worker_keeps_and_pages_decode_outside_the_lock() {
    use super::super::pointer_input::*;
    let adapter = Adapter::new();
    let mapping = adapter.frame(40);
    adapter.handle.scroll_scrollback(ScrollbackMovement::Top);
    let pointer = |phase, y| PointerInput {
        generation: mapping.local,
        phase,
        button: Some(PointerButton::Left),
        position: SurfacePosition { x: 1.0, y },
        modifiers: Default::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    };
    adapter.handle.pointer(pointer(PointerPhase::Press, 1.0));
    adapter.handle.pointer(pointer(PointerPhase::Motion, 61.0));
    adapter.handle.pointer(pointer(PointerPhase::Release, 61.0));
    let state = &adapter.handle.state;
    state.reading.lock().unwrap().turn(std::time::Instant::now());
    assert!(!adapter.handle.selection_copy_ready(), "the selected rows are not cached");
    // The worker answers the pages: it decodes them without the lock, then adds them.
    let requests = state.reading.lock().unwrap().scrollback.requests();
    assert!(!requests.is_empty());
    let history = mapping_history(&adapter);
    for request in requests {
        let rows: Vec<RowSnapshot> = (request.start..request.start + u64::from(request.rows))
            .filter(|line| *line < history.total)
            .map(|_| {
                let cells = vec![blank_test_cell("line"); usize::from(mapping.size.columns)];
                Arc::from(cells)
            })
            .collect();
        let page = crate::runtime::screen_encoder::encode_page(
            history.epoch,
            request.start,
            rows.iter().map(|row| (row.as_ref(), false)),
        );
        state.reading.lock().unwrap().page(request, Some(page));
    }
    let pages = state.reading.lock().unwrap().take_pages();
    let decoded = reading::decode_pages(pages, mapping.size.columns, None);
    assert!(decoded.iter().all(|(_, rows)| rows.is_some()));
    state.reading.lock().unwrap().settle_pages(decoded);
    state.reading.lock().unwrap().turn(std::time::Instant::now());
    assert!(adapter.handle.selection_copy_ready());
}

fn mapping_history(adapter: &Adapter) -> crate::terminal::emulator::HistorySnapshot {
    adapter
        .handle
        .state
        .reading
        .lock()
        .unwrap()
        .scrollback
        .present(PresentationGeneration::test(1))
        .unwrap()
        .history
}

/// A cell in the default style holding the first character of `text`.
fn blank_test_cell(text: &str) -> CellSnapshot {
    CellSnapshot {
        text: text.chars().next().unwrap().to_string(),
        foreground_source: TerminalColor::Default,
        background_source: TerminalColor::Default,
        inverse: false,
        bold: false,
        faint: false,
        italic: false,
        blinking: false,
        invisible: false,
        underline: TerminalUnderlineSnapshot::None,
        underline_source: TerminalColor::Default,
        strikethrough: false,
        overline: false,
        selected: false,
        spacer_tail: false,
        semantic_content: CellSemanticSnapshot::Output,
        hyperlink: None,
    }
}

pub(super) struct Adapter {
    pub(super) handle: PresentationHandle,
    pub(super) input: mpsc::Receiver<SubmittedInput>,
    pub(super) active: ActiveInput,
    unconfirmed: bool,
}
impl Adapter {
    pub(super) fn new() -> Self {
        Self::with_epoch(|state| Ok(state.invalidate())).unwrap()
    }
    /// A presentation attached through the client's control record for its Pane.
    fn attached(presenter: &control::Presenter) -> Result<Self, ServerError> {
        Self::with_epoch(|state| state.attach(presenter))
    }
    fn with_epoch(
        attach: impl FnOnce(&DevelopmentPaneState) -> Result<u64, ServerError>,
    ) -> Result<Self, ServerError> {
        use super::super::geometry::{BackingScale, CellGridSize, LogicalCellSize};
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(20, 6),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        );
        let state = Arc::new(DevelopmentPaneState::new());
        let epoch = attach(&state)?;
        state.set(DevelopmentPaneStatus::Connected);
        state.interaction.lock().unwrap().interactive = true;
        state.interaction.lock().unwrap().reading = true;
        let (sender, input) = mpsc::sync_channel(512);
        let handle = PresentationHandle {
            input: sender,
            state,
            alive: Arc::new(AtomicBool::new(true)),
            size: Arc::new(Mutex::new(pane_size(geometry))),
            appearance: Arc::new(Mutex::new(TerminalAppearanceUpdate::builtin().unwrap())),
            geometry: Mutex::new(geometry),
        };
        handle.interactive_focus(true, true);
        Ok(Self {
            handle,
            input,
            active: ActiveInput {
                epoch,
                lease: Lease {
                    server: [1; 32],
                    session: [2; 32],
                    control: [3; 32],
                    connection: [4; 32],
                },
                focus_sent: None,
            },
            unconfirmed: false,
        })
    }
    pub(super) fn drain(
        &mut self,
        mut exchange: impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<(), ServerError> {
        while let Ok(input) = self.input.try_recv() {
            self.active.process(
                &self.handle.state,
                input,
                &mut exchange,
                &mut self.unconfirmed,
            )?;
        }
        Ok(())
    }
    /// Sends queued input as the worker does, in `rounds` bounded drains. A drain with
    /// nothing queued sends nothing.
    pub(super) fn drain_rounds(
        &mut self,
        rounds: usize,
        batches: bool,
        mut exchange: impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<(), ServerError> {
        for _ in 0..rounds {
            self.active.drain(
                &self.handle.state,
                &self.input,
                batches,
                &mut exchange,
                &mut self.unconfirmed,
            )?;
        }
        Ok(())
    }
    fn tracking_screen(&self, local: u64, wire: u64, mouse_tracking: bool) -> ScreenMapping {
        let mapping = ScreenMapping {
            epoch: self.active.epoch,
            local: PresentationGeneration::test(local),
            wire,
            size: *self.handle.size.lock().unwrap(),
            mouse_tracking,
            alternate: false,
            alternate_scroll: false,
        };
        self.handle
            .state
            .connected(self.active.epoch, mapping, false)
            .unwrap();
        mapping
    }
    /// Applies a frame of a real terminal that printed `history + 6` numbered lines, as the
    /// worker applies frames, and returns its mapping.
    fn frame(&self, history: u64) -> ScreenMapping {
        use super::super::geometry::{BackingScale, CellGridSize, LogicalCellSize};
        let mut emulator = crate::terminal::emulator::TerminalEmulator::new(
            TerminalGeometry::from_grid(
                CellGridSize::new(20, 6),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ),
        )
        .unwrap();
        emulator.snapshot().unwrap();
        for _ in 0..history + 5 {
            emulator.feed(b"line\r\n");
        }
        emulator.feed(b"line");
        let screen = emulator.snapshot().unwrap().unwrap();
        let mapping = self.tracking_screen(4, 17, false);
        let mut reading = self.handle.state.reading.lock().unwrap();
        reading.scrollback.attach(self.active.lease.session);
        reading.scrollback.screen(screen);
        mapping
    }
    /// What the worker presents now.
    fn shown(&self) -> Arc<ScreenSnapshot> {
        self.handle
            .state
            .reading
            .lock()
            .unwrap()
            .scrollback
            .present(PresentationGeneration::test(99))
            .unwrap()
    }
    fn confirmation(&mut self) -> super::super::paste::PasteConfirmation {
        let reply = self
            .handle
            .request_paste(super::super::PastePayload::from("one\ntwo".to_owned()));
        self.drain(|request| {
            Ok(
                if matches!(
                    request,
                    Request::Input {
                        input: pane::InteractiveInput::Paste { .. },
                        ..
                    }
                ) {
                    Reply::Paste(pane::PasteOutcome::ConfirmationRequired(
                        pane::PasteConfirmation {
                            id: 17,
                            byte_len: 7,
                            line_count: 2,
                            multiline: true,
                            control_bytes: false,
                            closing_fence: false,
                        },
                    ))
                } else {
                    Reply::Unchanged
                },
            )
        })
        .unwrap();
        let Ok(Ok(super::super::PasteRequestOutcome::ConfirmationRequired(confirmation))) =
            reply.try_recv()
        else {
            panic!("expected paste confirmation")
        };
        confirmation
    }
}

fn presenter_for(control: &Arc<control::PaneControl>) -> control::Presenter {
    control.reserve().activate(&AtomicBool::new(true))
}

/// The server's focus rule for one Pane on one connection, stated independently.
#[derive(Default)]
struct FocusServer {
    generation: u64,
    target: Option<pane::FocusTarget>,
    keys: Vec<u64>,
    visible: Option<bool>,
}
impl FocusServer {
    fn handle(&mut self, request: &Request) -> Result<Reply, ServerError> {
        match request {
            Request::Focus {
                generation, target, ..
            } => {
                if *generation == self.generation && Some(*target) == self.target {
                    return Ok(Reply::Unchanged);
                }
                if *generation <= self.generation {
                    return Err(ServerError::StaleControl);
                }
                self.generation = *generation;
                self.target = Some(*target);
                Ok(Reply::Unchanged)
            }
            Request::Input {
                generation,
                input: pane::InteractiveInput::Key(_),
                ..
            } => {
                if *generation != self.generation
                    || self.target != Some(pane::FocusTarget::Terminal)
                {
                    return Err(ServerError::StaleControl);
                }
                self.keys.push(*generation);
                Ok(Reply::Unchanged)
            }
            Request::View { visible, .. } => {
                self.visible = Some(*visible);
                Ok(Reply::Unchanged)
            }
            _ => Ok(Reply::Unchanged),
        }
    }
}

#[test]
fn a_reopened_presentation_is_interactive_at_once_after_many_focus_changes() {
    let controls = PaneControls::default();
    let mut server = FocusServer::default();
    for changes in [0, 1, 15, 100] {
        let presenter = presenter_for(&controls.pane([7; 32]));
        let mut old = Adapter::attached(&presenter).unwrap();
        for change in 0..changes {
            old.handle.interactive_focus(change % 2 == 1, true);
            old.active
                .sync_focus(&old.handle.state, &mut |request| {
                    presenter.send(request, |request| server.handle(request))
                })
                .unwrap();
        }
        // Command-Q closes the window; the same client reopens a new one.
        presenter.retire(old.active.lease, |request| server.handle(request));
        // Closing every window leaves the Pane unfocused and without visual demand.
        assert_eq!(server.target, Some(pane::FocusTarget::Unfocused));
        assert_eq!(server.visible, Some(false));
        let successor = presenter_for(&controls.pane([7; 32]));
        let mut new = Adapter::attached(&successor).unwrap();
        new.handle.key(KeyInput::text_input("x"));
        new.drain(|request| successor.send(request, |request| server.handle(request)))
            .unwrap_or_else(|error| {
                panic!("after {changes} focus changes the first focus was refused: {error:?}")
            });
        assert_eq!(server.target, Some(pane::FocusTarget::Terminal));
        assert_eq!(server.keys.last(), Some(&server.generation));
    }
}

#[test]
fn a_dismissed_presentations_paused_input_cannot_follow_its_successors_focus() {
    let controls = PaneControls::default();
    let mut server = FocusServer::default();
    let presenter = presenter_for(&controls.pane([7; 32]));
    let mut old = Adapter::attached(&presenter).unwrap();
    old.handle.key(KeyInput::text_input("old"));
    let successor = presenter_for(&controls.pane([7; 32]));
    let mut new = Adapter::attached(&successor).unwrap();
    new.drain(|request| successor.send(request, |request| server.handle(request)))
        .unwrap();
    let acknowledged = (server.generation, server.target);
    // The predecessor resumes its paused admission after the successor's focus barrier.
    let mut reached = 0;
    let _ = old.drain(|request| {
        presenter.send(request, |request| {
            reached += 1;
            server.handle(request)
        })
    });
    presenter.retire(old.active.lease, |request| {
        reached += 1;
        server.handle(request)
    });
    assert_eq!(reached, 0, "a retired presenter reaches no server");
    assert!(server.keys.is_empty());
    assert_eq!((server.generation, server.target), acknowledged);
}

#[test]
fn a_dismissed_window_whose_worker_starts_late_cannot_revoke_its_reopened_successor() {
    let controls = PaneControls::default();
    let (old, _) = DevelopmentServerSessionFactory::for_pane(
        ServerConnection::for_identity("spaceterm-development").unwrap(),
        &controls,
        crate::platform::runtime_local_filesystem(),
        [7; 32],
    );
    // The old window was created first and closed before its worker ran.
    let reservation = old.control.reserve();
    let successor = presenter_for(&controls.pane([7; 32]));
    let focus = Request::Focus {
        lease: Adapter::new().active.lease,
        generation: 10,
        target: pane::FocusTarget::Terminal,
    };
    successor.send(&focus, |_| Ok(Reply::Unchanged)).unwrap();
    let (_, input) = mpsc::sync_channel(1);
    let (events, _output) = async_channel::bounded(2);
    let (semantics, _semantics) = async_channel::bounded(1);
    let (effects, _clipboard) = ClientEffects::new(Arc::new(|| {}));
    // The dismissed worker's production entry point. Its view is gone, so it connects to
    // nothing.
    present(
        old,
        (reservation, Arc::new(AtomicBool::new(false))),
        input,
        (events, semantics),
        Arc::new(Mutex::new(PaneSize {
            columns: 20,
            rows: 6,
            cell_width: 10,
            cell_height: 20,
        })),
        Arc::new(Mutex::new(TerminalAppearanceUpdate::builtin().unwrap())),
        (effects, 60),
    );
    assert!(
        successor.send(&focus, |_| Ok(Reply::Unchanged)).is_ok(),
        "the reopened window keeps its Pane"
    );
}

#[test]
fn an_exhausted_focus_sequence_fails_closed_instead_of_reusing_a_generation() {
    let controls = PaneControls::default();
    let presenter = presenter_for(&controls.pane([7; 32]));
    presenter
        .send(
            &Request::Focus {
                lease: Adapter::new().active.lease,
                generation: u64::MAX,
                target: pane::FocusTarget::Unfocused,
            },
            |_| Ok(Reply::Unchanged),
        )
        .unwrap();
    let successor = presenter_for(&controls.pane([7; 32]));
    assert!(matches!(
        Adapter::attached(&successor),
        Err(ServerError::Unsupported)
    ));
}

#[test]
fn queued_key_is_discarded_when_focus_moves_to_its_paste_confirmation() {
    let mut adapter = Adapter::new();
    let confirmation = adapter.confirmation();
    adapter.handle.key(KeyInput::text_input("queued"));
    adapter.handle.focus_paste_confirmation(confirmation.id);
    let mut written = false;
    adapter
        .drain(|request| {
            written |= matches!(
                request,
                Request::Input {
                    input: pane::InteractiveInput::Key(_),
                    ..
                }
            );
            Ok(Reply::Unchanged)
        })
        .unwrap();
    assert!(
        !written,
        "a queued key must not enter the terminal after modal focus transfer"
    );
}

#[test]
fn blur_and_refocus_discard_queued_keys_but_allow_current_input_method_text() {
    let mut adapter = Adapter::new();
    adapter.handle.key(KeyInput::text_input("stale"));
    adapter.handle.interactive_focus(false, false);
    adapter.handle.interactive_focus(false, true);
    adapter.handle.key(KeyInput::text_input("not-focused"));
    adapter.handle.key(KeyInput::input_method_commit("日本語"));
    let mut text = Vec::new();
    adapter
        .drain(|request| {
            if let Request::Input {
                input: pane::InteractiveInput::Key(key),
                ..
            } = request
            {
                text.push(key.text.clone());
            }
            Ok(Reply::Unchanged)
        })
        .unwrap();
    assert_eq!(text, vec![Some("日本語".into())]);
}

#[test]
fn an_inflight_paste_confirmation_is_cancelled_after_local_focus_loss() {
    let mut adapter = Adapter::new();
    let response = adapter
        .handle
        .request_paste(super::super::PastePayload::from("one\ntwo".to_owned()));
    let state = Arc::clone(&adapter.handle.state);
    let mut cancelled = Vec::new();
    adapter
        .drain(|request| match request {
            Request::Input {
                input: pane::InteractiveInput::Paste { .. },
                ..
            } => {
                state
                    .interaction
                    .lock()
                    .unwrap()
                    .focus(pane::FocusTarget::Unfocused);
                Ok(Reply::Paste(pane::PasteOutcome::ConfirmationRequired(
                    pane::PasteConfirmation {
                        id: 42,
                        byte_len: 7,
                        line_count: 2,
                        multiline: true,
                        control_bytes: false,
                        closing_fence: false,
                    },
                )))
            }
            Request::Input {
                input: pane::InteractiveInput::ResolvePaste { id, confirm },
                ..
            } => {
                cancelled.push((*id, *confirm));
                Ok(Reply::Paste(pane::PasteOutcome::Cancelled))
            }
            _ => Ok(Reply::Unchanged),
        })
        .unwrap();
    assert_eq!(cancelled, vec![(42, false)]);
    assert_eq!(
        response.try_recv().unwrap().unwrap(),
        super::super::PasteRequestOutcome::Rejected(
            super::super::paste::PasteRejection::TerminalUnfocused
        )
    );
    assert!(
        adapter
            .handle
            .state
            .interaction
            .lock()
            .unwrap()
            .pending
            .is_none()
    );
}

#[test]
fn a_confirmation_queued_before_reconnection_cannot_authorize_the_replacement_lease() {
    let mut adapter = Adapter::new();
    let confirmation = adapter.confirmation();
    adapter.handle.focus_paste_confirmation(confirmation.id);
    let response = adapter
        .handle
        .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
    let previous_generation = adapter.handle.input_generation();
    let epoch = adapter.handle.state.invalidate();
    adapter.handle.state.set(DevelopmentPaneStatus::Connected);
    adapter.handle.state.interaction.lock().unwrap().interactive = true;
    adapter.handle.interactive_focus(true, true);
    adapter.active.epoch = epoch;
    adapter.active.lease.connection = [9; 32];
    adapter.active.focus_sent = None;
    adapter
        .drain(|_| panic!("stale approval reached the replacement connection"))
        .unwrap();
    assert_ne!(adapter.handle.input_generation(), previous_generation);
    assert_eq!(
        response.try_recv().unwrap().unwrap(),
        super::super::PasteResolution::Stale
    );
}

#[test]
fn modal_paste_is_single_use_and_lost_confirmation_reply_is_typed_as_uncertain() {
    for acknowledgement in [
        Err(ServerError::Protocol),
        Ok(Reply::Unchanged),
        Ok(Reply::Paste(pane::PasteOutcome::Rejected(
            pane::PasteRejection::Empty,
        ))),
    ] {
        let mut adapter = Adapter::new();
        let confirmation = adapter.confirmation();
        adapter.handle.focus_paste_confirmation(confirmation.id);
        let response = adapter
            .handle
            .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
        let expected_lease = adapter.active.lease;
        let result = adapter.drain(|request| match request {
            Request::Input {
                lease,
                input:
                    pane::InteractiveInput::ResolvePaste {
                        id: 17,
                        confirm: true,
                    },
                ..
            } => {
                assert_eq!(*lease, expected_lease);
                acknowledgement.clone()
            }
            _ => Ok(Reply::Unchanged),
        });
        assert_eq!(result, Err(ServerError::Protocol));
        assert_eq!(
            response.try_recv().unwrap().unwrap(),
            super::super::PasteResolution::Unconfirmed
        );
        assert!(adapter.unconfirmed);
        let duplicate = adapter
            .handle
            .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
        assert_eq!(
            duplicate.try_recv().unwrap().unwrap(),
            super::super::PasteResolution::Stale
        );

        // Reconnection publishes the retained outcome before its replaceable
        // wakeup. Later screen frames may replace that wakeup while UI is busy.
        let epoch = adapter.handle.state.invalidate();
        let mapping = ScreenMapping {
            epoch,
            local: PresentationGeneration::test(10),
            wire: 3,
            size: *adapter.handle.size.lock().unwrap(),
            mouse_tracking: false,
            alternate: false,
            alternate_scroll: false,
        };
        adapter
            .handle
            .state
            .connected(epoch, mapping, true)
            .unwrap();
        let (events, output) = async_channel::bounded(2);
        events
            .force_send(TerminalSessionEvent::PasteUnconfirmed)
            .unwrap();
        let screen = ScreenSnapshot::empty_with_appearance(
            crate::local_path::LocalPathSemantics::Posix,
            &adapter.handle.appearance.lock().unwrap(),
        );
        for _ in 0..2 {
            events
                .force_send(TerminalSessionEvent::Screen(Arc::clone(&screen)))
                .unwrap();
        }
        while let Ok(event) = output.try_recv() {
            assert!(matches!(event, TerminalSessionEvent::Screen(_)));
        }
        assert!(
            adapter.handle.take_paste_unconfirmed(),
            "screen replacement lost the retained paste warning"
        );
        assert!(
            !adapter.handle.take_paste_unconfirmed(),
            "warning was delivered twice"
        );

        adapter
            .handle
            .state
            .connected(epoch, mapping, true)
            .unwrap();
        adapter.handle.state.invalidate();
        assert!(
            !adapter.handle.take_paste_unconfirmed(),
            "a stale warning crossed a connection epoch"
        );
    }
}

#[test]
fn incompatible_paste_request_acknowledgements_do_not_claim_that_nothing_was_written() {
    for acknowledgement in [
        Reply::Unchanged,
        Reply::Paste(pane::PasteOutcome::Cancelled),
    ] {
        let mut adapter = Adapter::new();
        let response = adapter
            .handle
            .request_paste(super::super::PastePayload::from("safe".to_owned()));
        let result = adapter.drain(|request| {
            Ok(
                if matches!(
                    request,
                    Request::Input {
                        input: pane::InteractiveInput::Paste { .. },
                        ..
                    }
                ) {
                    acknowledgement.clone()
                } else {
                    Reply::Unchanged
                },
            )
        });
        assert_eq!(result, Err(ServerError::Protocol));
        assert_eq!(
            response.try_recv().unwrap().unwrap(),
            super::super::PasteRequestOutcome::Unconfirmed
        );
        assert!(adapter.unconfirmed);
    }
}

#[test]
fn modal_dismissal_restores_focus_after_the_already_approved_paste() {
    let mut adapter = Adapter::new();
    let confirmation = adapter.confirmation();
    adapter.handle.focus_paste_confirmation(confirmation.id);
    let response = adapter
        .handle
        .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
    adapter.handle.interactive_focus(true, true);
    adapter.handle.key(KeyInput::text_input("after"));
    let mut order = Vec::new();
    adapter
        .drain(|request| match request {
            Request::Input {
                input: pane::InteractiveInput::ResolvePaste { confirm: true, .. },
                ..
            } => {
                order.push("paste");
                Ok(Reply::Paste(pane::PasteOutcome::Written))
            }
            Request::Input {
                input: pane::InteractiveInput::Key(_),
                ..
            } => {
                order.push("key");
                Ok(Reply::Unchanged)
            }
            Request::Focus {
                target: pane::FocusTarget::Terminal,
                ..
            } => {
                order.push("focus");
                Ok(Reply::Unchanged)
            }
            _ => Ok(Reply::Unchanged),
        })
        .unwrap();
    assert_eq!(order, vec!["paste", "focus", "key"]);
    assert_eq!(
        response.try_recv().unwrap().unwrap(),
        super::super::PasteResolution::Written
    );
}

#[test]
fn leaving_the_pane_cancels_an_approval_even_after_modal_focus_restoration() {
    let mut adapter = Adapter::new();
    let confirmation = adapter.confirmation();
    adapter.handle.focus_paste_confirmation(confirmation.id);
    let response = adapter
        .handle
        .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
    adapter.handle.interactive_focus(true, true);
    adapter.handle.interactive_focus(false, false);
    adapter.handle.interactive_focus(true, true);
    adapter
        .drain(|request| {
            assert!(
                !matches!(request, Request::Input { .. }),
                "an approval crossed unrelated focus loss"
            );
            Ok(Reply::Unchanged)
        })
        .unwrap();
    assert_eq!(
        response.try_recv().unwrap().unwrap(),
        super::super::PasteResolution::Stale
    );
}

#[test]
fn unsupported_interaction_disables_typing_without_starting_reconnection() {
    let mut adapter = Adapter::new();
    adapter.handle.key(KeyInput::text_input("before"));
    adapter
        .drain(|request| {
            if matches!(request, Request::Input { .. }) {
                Err(ServerError::Unsupported)
            } else {
                Ok(Reply::Unchanged)
            }
        })
        .unwrap();
    assert!(!adapter.handle.interactive_input_available());
    assert_eq!(
        adapter.handle.state.status(),
        DevelopmentPaneStatus::Connected
    );
    adapter.handle.key(KeyInput::input_method_commit("after"));
    adapter
        .drain(|_| panic!("input was sent after capability refusal"))
        .unwrap();
}

fn assert_tracked_continuation_after_output(tracking_after_output: bool) {
    use super::super::pointer_input::*;
    let mut adapter = Adapter::new();
    let press_screen = adapter.tracking_screen(8, 91, true);
    let pointer = PointerInput {
        generation: press_screen.local,
        phase: PointerPhase::Press,
        button: Some(PointerButton::Left),
        position: SurfacePosition { x: 15.0, y: 50.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    };
    let mut delivered = Vec::new();
    let lease = adapter.active.lease;
    let mut server = |request: &Request| {
        // A wheel needs only its grid, so output arriving after the paint never drops it.
        if let Request::Input {
            input: pane::InteractiveInput::Wheel(wheel),
            ..
        } = request
        {
            delivered.push(("wheel", wheel.screen_generation));
        }
        if let Request::Input {
            lease: actual,
            input: pane::InteractiveInput::Pointer(pointer),
            ..
        } = request
        {
            assert_eq!(*actual, lease);
            delivered.push((
                match pointer.phase {
                    pane::PointerPhase::Press => "press",
                    pane::PointerPhase::Motion => "motion",
                    pane::PointerPhase::Release => "release",
                },
                pointer.screen_generation,
            ));
        }
        Ok(Reply::Unchanged)
    };
    adapter.handle.pointer(pointer);
    adapter.drain(&mut server).unwrap();
    let next_screen = adapter.tracking_screen(9, 92, tracking_after_output);
    // Starts and buttonless motion must not borrow the continuation range.
    adapter.handle.pointer(pointer);
    adapter.handle.pointer(PointerInput {
        phase: PointerPhase::Motion,
        button: None,
        ..pointer
    });
    adapter.handle.wheel(WheelInput {
        generation: pointer.generation,
        horizontal_steps: 0,
        vertical_steps: 1,
        phase: WheelPhase::GestureChanged,
        position: pointer.position,
        modifiers: pointer.modifiers,
        shift_selection: pointer.shift_selection,
    });
    let continuation = PointerInput {
        generation: if tracking_after_output {
            pointer.generation
        } else {
            next_screen.local
        },
        ..pointer
    };
    adapter.handle.pointer(PointerInput {
        phase: PointerPhase::Motion,
        ..continuation
    });
    adapter.handle.pointer(PointerInput {
        phase: PointerPhase::Release,
        ..continuation
    });
    adapter.drain(&mut server).unwrap();
    let clickable = if tracking_after_output {
        next_screen
    } else {
        adapter.tracking_screen(10, 93, true)
    };
    adapter.handle.pointer(PointerInput {
        generation: clickable.local,
        ..pointer
    });
    adapter.drain(&mut server).unwrap();
    let mut expected = vec![("press", 91)];
    // The program receives the wheel only while it tracks the mouse; otherwise it scrolls this
    // client's view.
    if tracking_after_output {
        expected.push(("wheel", 92));
    }
    expected.extend([("motion", 92), ("release", 92), ("press", clickable.wire)]);
    assert_eq!(
        delivered, expected,
        "tracking after output: {tracking_after_output}"
    );
}

#[test]
fn tracked_continuations_survive_a_newer_screen() {
    assert_tracked_continuation_after_output(true);
}

#[test]
fn tracked_continuations_survive_tracking_exit() {
    assert_tracked_continuation_after_output(false);
}

#[test]
fn continuations_never_retag_a_previous_epoch_or_replaced_geometry() {
    use super::super::{
        geometry::{BackingScale, CellGridSize, LogicalCellSize},
        pointer_input::*,
    };
    for reconnect in [false, true] {
        let mut adapter = Adapter::new();
        let old_screen = adapter.tracking_screen(8, 91, true);
        let pointer = PointerInput {
            generation: old_screen.local,
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            position: SurfacePosition { x: 15.0, y: 50.0 },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::ReportToApplication,
        };
        adapter.handle.pointer(pointer);
        adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
        if reconnect {
            adapter.active.epoch = adapter.handle.state.invalidate();
            adapter.active.lease.connection = [9; 32];
            adapter.active.focus_sent = None;
            adapter.handle.state.interaction.lock().unwrap().interactive = true;
        } else {
            let original = *adapter.handle.geometry.lock().unwrap();
            adapter.handle.resize(TerminalGeometry::from_grid(
                CellGridSize::new(21, 6),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ));
            adapter.handle.pointer(PointerInput {
                phase: PointerPhase::Release,
                ..pointer
            });
            // Returning to the same size must not resurrect the old range.
            adapter.handle.resize(original);
            adapter
                .drain(|request| {
                    assert!(!matches!(request, Request::Input { .. }));
                    Ok(Reply::Unchanged)
                })
                .unwrap();
        }
        let replacement = adapter.tracking_screen(9, 92, true);
        adapter.handle.pointer(PointerInput {
            phase: PointerPhase::Release,
            ..pointer
        });
        adapter.handle.pointer(PointerInput {
            phase: PointerPhase::Motion,
            ..pointer
        });
        adapter.handle.pointer(PointerInput {
            generation: replacement.local,
            ..pointer
        });
        let mut delivered = Vec::new();
        adapter
            .drain(|request| {
                if let Request::Input {
                    input: pane::InteractiveInput::Pointer(pointer),
                    ..
                } = request
                {
                    delivered.push((
                        matches!(pointer.phase, pane::PointerPhase::Press),
                        pointer.screen_generation,
                    ));
                }
                Ok(Reply::Unchanged)
            })
            .unwrap();
        assert_eq!(delivered, vec![(true, 92)], "reconnect: {reconnect}");
    }
}

#[test]
fn same_wire_size_resize_keeps_quiet_pointer_and_wheel_input_at_the_new_fitted_cells() {
    use super::super::{
        geometry::{BackingScale, CellGridSize, LogicalCellSize, LogicalPosition, LogicalSize},
        pointer_input::*,
    };
    let mut adapter = Adapter::new();
    let geometry = |width| {
        TerminalGeometry::from_viewport(
            LogicalSize::new(width, 432.0),
            LogicalCellSize::new(8.0, 18.0),
            BackingScale::ONE,
            CellGridSize::new(2, 2),
        )
    };
    let original = geometry(640.0);
    adapter.handle.resize(original);
    adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
    adapter
        .handle
        .state
        .connected(
            adapter.active.epoch,
            ScreenMapping {
                epoch: adapter.active.epoch,
                local: PresentationGeneration::test(8),
                wire: 91,
                size: pane_size(original),
                mouse_tracking: true,
                alternate: false,
                alternate_scroll: false,
            },
            false,
        )
        .unwrap();
    let resized = geometry(641.0);
    assert_eq!(pane_size(original), pane_size(resized));
    adapter.handle.resize(resized);
    // A quiet server acknowledges this unchanged wire size without a Screen.
    adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
    let cell = resized.logical_cell_size();
    let position =
        resized.to_backing_position(LogicalPosition::new(cell.width * 10.5, cell.height * 5.5));
    let position = SurfacePosition {
        x: position.x,
        y: position.y,
    };
    adapter.handle.pointer(PointerInput {
        generation: PresentationGeneration::test(8),
        phase: PointerPhase::Press,
        button: Some(PointerButton::Left),
        position,
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    });
    adapter.handle.wheel(WheelInput {
        generation: PresentationGeneration::test(8),
        horizontal_steps: 0,
        vertical_steps: 1,
        phase: WheelPhase::GestureChanged,
        position,
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    });
    let mut delivered = Vec::new();
    let lease = adapter.active.lease;
    adapter
        .drain(|request| {
            if let Request::Input {
                lease: actual,
                input,
                ..
            } = request
            {
                assert_eq!(*actual, lease);
                match input {
                    pane::InteractiveInput::Pointer(pointer) => {
                        delivered.push(("pointer", pointer.screen_generation, pointer.x, pointer.y))
                    }
                    pane::InteractiveInput::Wheel(wheel) => {
                        delivered.push(("wheel", wheel.screen_generation, wheel.x, wheel.y))
                    }
                    _ => panic!("unexpected interactive input"),
                }
            }
            Ok(Reply::Unchanged)
        })
        .unwrap();
    assert_eq!(
        delivered.len(),
        2,
        "same-grid resize disabled quiet application mouse input"
    );
    assert_eq!((delivered[0].0, delivered[0].1), ("pointer", 91));
    assert_eq!((delivered[1].0, delivered[1].1), ("wheel", 91));
    for (_, _, x, y) in delivered {
        assert!(
            (x - 84.0).abs() < 0.001,
            "pointer mapping used the old fitted width: {x}"
        );
        assert!((y - 99.0).abs() < 0.001);
    }
}

#[test]
fn a_wire_size_change_waits_for_its_authoritative_pointer_mapping() {
    use super::super::{
        geometry::{BackingScale, CellGridSize, LogicalCellSize},
        pointer_input::*,
    };
    let mut adapter = Adapter::new();
    let original = *adapter.handle.size.lock().unwrap();
    let mapping = ScreenMapping {
        epoch: adapter.active.epoch,
        local: PresentationGeneration::test(8),
        wire: 91,
        size: original,
        mouse_tracking: true,
        alternate: false,
        alternate_scroll: false,
    };
    adapter
        .handle
        .state
        .connected(adapter.active.epoch, mapping, false)
        .unwrap();
    let resized = TerminalGeometry::from_grid(
        CellGridSize::new(21, 6),
        LogicalCellSize::new(10.0, 20.0),
        BackingScale::ONE,
    );
    adapter.handle.resize(resized);
    adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
    // Initial readiness can receive the old Screen before its resize is applied.
    adapter
        .handle
        .state
        .connected(adapter.active.epoch, mapping, false)
        .unwrap();
    let pointer = PointerInput {
        generation: mapping.local,
        phase: PointerPhase::Press,
        button: Some(PointerButton::Left),
        position: SurfacePosition { x: 15.0, y: 50.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    };
    adapter.handle.pointer(pointer);
    adapter
        .drain(|_| panic!("obsolete dimensions restored pointer mapping"))
        .unwrap();
    // The resize itself publishes this authoritative replacement, without shell output.
    let replacement = ScreenMapping {
        local: PresentationGeneration::test(9),
        wire: 92,
        size: pane_size(resized),
        ..mapping
    };
    adapter
        .handle
        .state
        .connected(adapter.active.epoch, replacement, false)
        .unwrap();
    adapter.handle.pointer(PointerInput {
        generation: replacement.local,
        ..pointer
    });
    let mut delivered = Vec::new();
    adapter
        .drain(|request| {
            if let Request::Input {
                input: pane::InteractiveInput::Pointer(pointer),
                ..
            } = request
            {
                delivered.push((pointer.screen_generation, pointer.x, pointer.y));
            }
            Ok(Reply::Unchanged)
        })
        .unwrap();
    assert_eq!(delivered, vec![(92, 15.0, 50.0)]);
}

#[test]
fn pointer_coordinates_use_fitted_cells_and_the_authoritative_screen_revision() {
    use super::super::{
        geometry::{BackingScale, CellGridSize, LogicalCellSize, LogicalPosition, LogicalSize},
        pointer_input::*,
    };
    let mut adapter = Adapter::new();
    let geometry = TerminalGeometry::from_viewport(
        LogicalSize::new(201.0, 122.0),
        LogicalCellSize::new(10.0, 20.0),
        BackingScale::new(1.5).unwrap(),
        CellGridSize::new(2, 2),
    );
    adapter.handle.resize(geometry);
    adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
    adapter.handle.state.interaction.lock().unwrap().screen = Some(ScreenMapping {
        epoch: adapter.active.epoch,
        local: PresentationGeneration::test(8),
        wire: 91,
        size: pane_size(geometry),
        mouse_tracking: true,
        alternate: false,
        alternate_scroll: false,
    });
    let cell = geometry.logical_cell_size();
    let position =
        geometry.to_backing_position(LogicalPosition::new(cell.width * 1.5, cell.height * 2.5));
    let input = PointerInput {
        generation: PresentationGeneration::test(8),
        phase: PointerPhase::Press,
        button: Some(PointerButton::Left),
        position: SurfacePosition {
            x: position.x,
            y: position.y,
        },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    };
    adapter.handle.pointer(PointerInput {
        generation: PresentationGeneration::test(7),
        ..input
    });
    adapter.handle.pointer(input);
    let mut coordinates = Vec::new();
    adapter
        .drain(|request| {
            if let Request::Input {
                input: pane::InteractiveInput::Pointer(pointer),
                ..
            } = request
            {
                coordinates.push((pointer.screen_generation, pointer.x, pointer.y));
            }
            Ok(Reply::Unchanged)
        })
        .unwrap();
    assert_eq!(
        coordinates.len(),
        1,
        "stale presentation input must be discarded"
    );
    assert_eq!(coordinates[0].0, 91);
    assert!(
        (coordinates[0].1 - 22.5).abs() < 0.001,
        "wrong fitted-cell x: {:?}",
        coordinates[0]
    );
    assert!(
        (coordinates[0].2 - 75.0).abs() < 0.001,
        "wrong fitted-cell y: {:?}",
        coordinates[0]
    );
}

#[test]
fn an_ended_session_is_a_permanent_failure_after_control_loss() {
    let state = DevelopmentPaneState::new();
    state.set(DevelopmentPaneStatus::Takeover);
    assert!(!state.connection_failed(ServerError::SessionUnavailable, true));
    assert_eq!(
        state.status(),
        DevelopmentPaneStatus::TakeoverFailed(ServerError::SessionUnavailable)
    );
}

#[test]
fn only_a_connected_answer_reads_as_refused_input() {
    use crate::terminal::InteractiveInput;
    let state = DevelopmentPaneState::new();
    assert_eq!(
        state.interactive_input(),
        InteractiveInput::Pending,
        "a Pane still attaching has no answer"
    );
    state.interaction.lock().unwrap().interactive = true;
    state.set(DevelopmentPaneStatus::Connected);
    assert_eq!(state.interactive_input(), InteractiveInput::Accepted);
    state.invalidate();
    assert_eq!(
        state.interactive_input(),
        InteractiveInput::Pending,
        "an interrupted connection is not a refusal"
    );
    state.set(DevelopmentPaneStatus::Connected);
    assert_eq!(
        state.interactive_input(),
        InteractiveInput::Refused,
        "a connection that negotiated no input refuses it"
    );
}

#[test]
fn a_resolving_update_keeps_the_pane_reconnecting_without_carrying_a_click() {
    for (takeover, reconnecting) in [
        (false, DevelopmentPaneStatus::Reconnecting),
        (true, DevelopmentPaneStatus::TakeoverReconnecting),
    ] {
        let state = DevelopmentPaneState::new();
        state.take_over.store(true, Ordering::Release);
        state.start_restored.store(true, Ordering::Release);
        assert!(state.connection_failed(ServerError::ActivationUnresolved, takeover));
        assert_eq!(state.status(), reconnecting);
        // The reconnect neither takes control nor starts a shell on the person's behalf.
        assert!(!state.take_over.load(Ordering::Acquire));
        assert!(!state.start_restored.load(Ordering::Acquire));
    }
    // A deliberate Stop and permanent refusals still wait for the person.
    for error in [
        ServerError::Stopped,
        ServerError::AuthenticationRefused,
        ServerError::MissingRequiredCapability,
    ] {
        let state = DevelopmentPaneState::new();
        assert!(!state.connection_failed(error, false), "{error:?}");
        assert_eq!(state.status(), DevelopmentPaneStatus::Failed(error));
    }
}

#[test]
fn each_take_over_offer_names_the_current_controlling_client() {
    use pane::control::Controller;
    let client = |name: &str| Controller::Client {
        name: name.into(),
        this_client: false,
    };
    let state = DevelopmentPaneState::new();
    assert_eq!(state.controller(), None);
    state.offered_take_over(client("Studio"), false);
    assert_eq!(state.status(), DevelopmentPaneStatus::Takeover);
    assert_eq!(state.controller(), Some(client("Studio")));
    state.offered_take_over(Controller::Nobody, true);
    assert_eq!(state.controller(), Some(Controller::Nobody));
    assert!(state.session_dormant());
    state.take_over();
    state.offered_take_over(client("Laptop"), false);
    assert!(!state.session_dormant());
    assert_eq!(
        state.status(),
        DevelopmentPaneStatus::TakingOver,
        "an offer that crosses Take Over leaves it waiting for its answer"
    );
    assert_eq!(state.controller(), Some(client("Laptop")));
}

#[test]
fn a_transport_outage_keeps_the_takeover_screen_reconnecting() {
    let state = DevelopmentPaneState::new();
    state.set(DevelopmentPaneStatus::Takeover);
    assert!(state.connection_failed(ServerError::NotRunning, true));
    assert_eq!(state.status(), DevelopmentPaneStatus::TakeoverReconnecting);
}

#[test]
fn persistent_protocol_failure_stops_after_three_automatic_reconnections() {
    for takeover in [false, true] {
        let state = DevelopmentPaneState::new();
        for _ in 0..3 {
            assert!(state.connection_failed(ServerError::Protocol, takeover));
        }
        assert!(!state.connection_failed(ServerError::Protocol, takeover));
        assert_eq!(
            state.status(),
            if takeover {
                DevelopmentPaneStatus::TakeoverFailed(ServerError::Protocol)
            } else {
                DevelopmentPaneStatus::Failed(ServerError::Protocol)
            }
        );
    }
}

#[test]
fn a_brief_screen_between_protocol_failures_does_not_renew_automatic_retries() {
    let adapter = Adapter::new();
    let state = &adapter.handle.state;
    for sequence in 1..=3 {
        assert!(state.connection_failed(ServerError::Protocol, false));
        adapter.tracking_screen(sequence, sequence, false);
    }
    assert!(!state.connection_failed(ServerError::Protocol, false));
    assert_eq!(
        state.status(),
        DevelopmentPaneStatus::Failed(ServerError::Protocol)
    );
    state.retry();
    assert!(state.connection_failed(ServerError::Protocol, false));
}

#[test]
fn an_outage_during_protocol_recovery_retains_the_original_failure() {
    let state = DevelopmentPaneState::new();
    assert!(state.connection_failed(ServerError::Protocol, false));
    assert!(state.connection_failed(ServerError::NotRunning, false));
    assert!(state.connection_failed(ServerError::NotRunning, false));
    assert!(!state.connection_failed(ServerError::NotRunning, false));
    assert_eq!(
        state.status(),
        DevelopmentPaneStatus::Failed(ServerError::Protocol)
    );
}
