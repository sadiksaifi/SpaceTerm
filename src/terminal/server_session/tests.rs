use super::link::{ActiveInput, present};
use super::*;

#[derive(Default)]
struct ProbeGate {
    released: Mutex<bool>,
    changed: std::sync::Condvar,
}
impl ProbeGate {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
    fn wait(&self) {
        let (released, _) = self
            .changed
            .wait_timeout_while(
                self.released.lock().unwrap(),
                Duration::from_secs(10),
                |released| !*released,
            )
            .unwrap();
        assert!(*released, "the test did not release its connection probe");
    }
}
struct ReleaseProbe(Arc<ProbeGate>);
impl Drop for ReleaseProbe {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct PairedProbe {
    calls: Mutex<Vec<std::time::Instant>>,
    entered: mpsc::Sender<usize>,
    hold: usize,
    gate: Arc<ProbeGate>,
}
impl spaceterm_server::direct::DirectConnector for PairedProbe {
    fn connect(&self) -> Result<spaceterm_server::direct::DirectConnection, DirectError> {
        let count = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(std::time::Instant::now());
            calls.len()
        };
        self.entered.send(count).unwrap();
        if count == self.hold {
            self.gate.wait();
        }
        Err(DirectError::Transport)
    }
}
impl PairedProbe {
    fn new(hold: usize) -> (Arc<Self>, mpsc::Receiver<usize>) {
        let (entered, attempts) = mpsc::channel();
        (
            Arc::new(Self {
                calls: Mutex::default(),
                entered,
                hold,
                gate: Arc::default(),
            }),
            attempts,
        )
    }
}

fn paired_pane(
    server: ServerConnection,
    controls: &PaneControls,
    id: u8,
) -> (StartedTerminalSession, Arc<ServerPaneState>) {
    let (factory, state) = ServerSessionFactory::for_pane(
        server,
        controls,
        crate::platform::runtime_local_filesystem(),
        [id; 32],
    );
    // A real retry intent marks when the worker consumes its initial connection request.
    state.retry();
    let started = factory
        .start(
            *Adapter::new().handle.geometry.lock().unwrap(),
            TerminalAppearanceUpdate::builtin().unwrap(),
        )
        .unwrap();
    (started, state)
}

#[test]
fn eight_paired_panes_share_the_route_owners_reachability_probes() {
    let (probe, _entered) = PairedProbe::new(usize::MAX);
    let server = super::push_tests::paired_connection(probe.clone());
    // The owner probes once per backoff step. Panes may only follow the resulting link.
    assert_eq!(
        server.refresh(),
        Err(ServerError::Direct(DirectError::Transport))
    );
    let controls = PaneControls::default();
    let panes: Vec<_> = (1..=8)
        .map(|id| paired_pane(server.clone(), &controls, id))
        .collect();
    for _ in 0..3 {
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(
            server.refresh(),
            Err(ServerError::Direct(DirectError::Transport))
        );
    }
    let until = std::time::Instant::now() + Duration::from_secs(5);
    while panes
        .iter()
        .any(|(_, state)| !matches!(state.status(), ServerPaneStatus::Failed(_)))
    {
        assert!(
            std::time::Instant::now() < until,
            "the Pane recovery episode did not end"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(panes);
    let calls = probe.calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        4,
        "eight Panes multiplied the owner's four probes"
    );
    assert!(
        calls
            .windows(2)
            .all(|pair| pair[1].duration_since(pair[0]) >= Duration::from_secs(1))
    );
}

#[test]
fn closing_panes_cancels_recovery_while_the_route_owner_is_probing() {
    let (probe, entered) = PairedProbe::new(3);
    let release = ReleaseProbe(Arc::clone(&probe.gate));
    let server = super::push_tests::paired_connection(probe.clone());
    for _ in 0..2 {
        assert_eq!(
            server.refresh(),
            Err(ServerError::Direct(DirectError::Transport))
        );
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    let owner = std::thread::spawn({
        let server = server.clone();
        move || server.refresh()
    });
    assert_eq!(entered.recv_timeout(Duration::from_secs(5)).unwrap(), 3);
    // A slow owner holds the shared-link mutex. Pane recovery must neither wait behind it
    // nor leave another dial queued after the presentations are dropped.
    let controls = PaneControls::default();
    let panes: Vec<_> = (1..=2)
        .map(|id| paired_pane(server.clone(), &controls, id))
        .collect();
    let states: Vec<_> = panes
        .iter()
        .map(|(_, state)| Arc::downgrade(state))
        .collect();
    let started_by = std::time::Instant::now() + Duration::from_secs(5);
    while panes
        .iter()
        .any(|(_, state)| state.retry.load(Ordering::Acquire))
    {
        assert!(
            std::time::Instant::now() < started_by,
            "Pane workers did not consume their retry requests"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(panes);
    let released_by = std::time::Instant::now() + Duration::from_secs(2);
    while states.iter().any(|state| state.strong_count() > 0)
        && std::time::Instant::now() < released_by
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    let released_before_owner = states.iter().all(|state| state.strong_count() == 0);
    drop(release);
    assert_eq!(
        owner.join().unwrap(),
        Err(ServerError::Direct(DirectError::Transport))
    );
    let until = std::time::Instant::now() + Duration::from_secs(5);
    while states.iter().any(|state| state.strong_count() > 0) {
        assert!(
            std::time::Instant::now() < until,
            "closed Pane workers did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let count = probe.calls.lock().unwrap().len();
    assert!(
        released_before_owner,
        "closed Panes waited for the owner and made {count} probes"
    );
    assert_eq!(count, 3, "closed Panes started another connection");
}

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
    let (factory, _) = ServerSessionFactory::for_pane(
        server,
        &PaneControls::default(),
        crate::platform::runtime_local_filesystem(),
        [8; 32],
    );
    assert!(!factory.allows_local_file_insertion());
    assert!(factory.local_file_authority().is_none());
}

#[test]
fn a_closed_windows_view_returns_with_the_next_presentation_of_the_same_session() {
    use super::super::pointer_input::*;
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
    let controls = PaneControls::default();
    let present = || {
        ServerSessionFactory::for_pane(
            server.clone(),
            &controls,
            crate::platform::runtime_local_filesystem(),
            [8; 32],
        )
        .1
    };

    // The closing window scrolled back and selected rows of Terminal Session [2; 32].
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
    adapter.handle.pointer(pointer(PointerPhase::Motion, 41.0));
    adapter.handle.pointer(pointer(PointerPhase::Release, 41.0));
    // The closing window's connection asked for the rows above the screen.
    let asked = adapter
        .handle
        .state
        .reading
        .lock()
        .unwrap()
        .scrollback
        .requests();
    assert!(!asked.is_empty());
    let closing = present();
    std::mem::swap(
        &mut *closing.reading.lock().unwrap(),
        &mut *adapter.handle.state.reading.lock().unwrap(),
    );
    closing.retain_view();
    assert!(
        closing.reading.lock().unwrap().scrollback.following(),
        "the closing presentation keeps nothing"
    );

    let reopened = present();
    let mut reading = reopened.reading.lock().unwrap();
    reading.scrollback.attach([2; 32]);
    assert!(!reading.scrollback.following());
    assert!(reading.scrollback.selection_present());
    let span = |requests: &[super::scrollback::PageRequest]| {
        let start = requests.iter().map(|request| request.start).min();
        let end = requests
            .iter()
            .map(|request| request.start + u64::from(request.rows))
            .max();
        start.zip(end)
    };
    assert_eq!(
        span(&reading.scrollback.requests()),
        span(&asked),
        "the next connection asks again for rows the ended one never received"
    );
    drop(reading);
    // The view returns once.
    assert!(present().reading.lock().unwrap().scrollback.following());

    // After a server restart the Pane has a new Terminal Session, which starts at its screen.
    let closing = present();
    std::mem::swap(
        &mut *closing.reading.lock().unwrap(),
        &mut *reopened.reading.lock().unwrap(),
    );
    closing.retain_view();
    let restarted = present();
    let mut reading = restarted.reading.lock().unwrap();
    reading.scrollback.attach([7; 32]);
    assert!(reading.scrollback.following());
    assert!(!reading.scrollback.selection_present());
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
    assert_eq!(
        forwarded, 1,
        "the program decides what Copy without a Selection does"
    );
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
    assert!(
        adapter.input.try_recv().is_err(),
        "Copy of a Selection forwards nothing"
    );
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
    assert!(
        adapter.handle.copy_selection().unwrap().is_none(),
        "no row is cached yet"
    );
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

pub(super) fn assert_copy_readiness_and_page_decoding() {
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
    state
        .reading
        .lock()
        .unwrap()
        .turn(std::time::Instant::now());
    assert!(
        !adapter.handle.selection_copy_ready(),
        "the selected rows are not cached"
    );
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
    state
        .reading
        .lock()
        .unwrap()
        .turn(std::time::Instant::now());
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
        attach: impl FnOnce(&ServerPaneState) -> Result<u64, ServerError>,
    ) -> Result<Self, ServerError> {
        use super::super::geometry::{BackingScale, CellGridSize, LogicalCellSize};
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(20, 6),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        );
        let state = Arc::new(ServerPaneState::new());
        let epoch = attach(&state)?;
        state.set(ServerPaneStatus::Connected);
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
        mut exchange: impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<(), ServerError> {
        for _ in 0..rounds {
            self.active.drain(
                &self.handle.state,
                &self.input,
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
        let mut emulator =
            crate::terminal::emulator::TerminalEmulator::new(TerminalGeometry::from_grid(
                CellGridSize::new(20, 6),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ))
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
    let (old, _) = ServerSessionFactory::for_pane(
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
    adapter.handle.state.set(ServerPaneStatus::Connected);
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

        // Reconnection retains the outcome until the UI takes its warning once.
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
        assert!(
            adapter.handle.take_paste_unconfirmed(),
            "reconnection lost the retained paste warning"
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
fn an_unsupported_refusal_drops_only_that_message() {
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
    assert!(adapter.handle.interactive_input_available());
    assert_eq!(adapter.handle.state.status(), ServerPaneStatus::Connected);
    adapter.handle.key(KeyInput::input_method_commit("after"));
    let mut sent = 0;
    adapter
        .drain(|request| {
            sent += usize::from(matches!(request, Request::Input { .. }));
            Ok(Reply::Unchanged)
        })
        .unwrap();
    assert_eq!(sent, 1, "input after the refusal still reaches the server");
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
    let state = ServerPaneState::new();
    state.set(ServerPaneStatus::Takeover);
    assert!(!state.connection_failed(ServerError::SessionUnavailable, true));
    assert_eq!(
        state.status(),
        ServerPaneStatus::TakeoverFailed(ServerError::SessionUnavailable)
    );
}

#[test]
fn only_an_interactive_connection_accepts_input() {
    use crate::terminal::InteractiveInput;
    let state = ServerPaneState::new();
    assert_eq!(
        state.interactive_input(),
        InteractiveInput::Pending,
        "a Pane still attaching takes no input yet"
    );
    state.interaction.lock().unwrap().interactive = true;
    state.set(ServerPaneStatus::Connected);
    assert_eq!(state.interactive_input(), InteractiveInput::Accepted);
    state.invalidate();
    assert_eq!(
        state.interactive_input(),
        InteractiveInput::Pending,
        "an interrupted connection takes no input until it is interactive again"
    );
    state.set(ServerPaneStatus::Connected);
    assert_eq!(state.interactive_input(), InteractiveInput::Pending);
}

#[test]
fn a_resolving_update_keeps_the_pane_reconnecting_without_carrying_a_click() {
    for (takeover, reconnecting) in [
        (false, ServerPaneStatus::Reconnecting),
        (true, ServerPaneStatus::TakeoverReconnecting),
    ] {
        let state = ServerPaneState::new();
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
        ServerError::IncompatibleProtocol,
    ] {
        let state = ServerPaneState::new();
        assert!(!state.connection_failed(error, false), "{error:?}");
        assert_eq!(state.status(), ServerPaneStatus::Failed(error));
    }
}

#[test]
fn each_take_over_offer_names_the_current_controlling_client() {
    use pane::control::Controller;
    let client = |name: &str| Controller::Client {
        name: name.into(),
        this_client: false,
    };
    let state = ServerPaneState::new();
    assert_eq!(state.controller(), None);
    state.offered_take_over(client("Studio"), None);
    assert_eq!(state.status(), ServerPaneStatus::Takeover);
    assert_eq!(state.controller(), Some(client("Studio")));
    state.offered_take_over(Controller::Nobody, Some(pane::SessionEnd::Signaled));
    assert_eq!(state.controller(), Some(Controller::Nobody));
    assert_eq!(state.session_end(), Some(pane::SessionEnd::Signaled));
    state.take_over();
    state.offered_take_over(client("Laptop"), None);
    assert_eq!(state.session_end(), None);
    assert_eq!(
        state.status(),
        ServerPaneStatus::TakingOver,
        "an offer that crosses Take Over leaves it waiting for its answer"
    );
    assert_eq!(state.controller(), Some(client("Laptop")));
}

#[test]
fn a_transport_outage_keeps_the_takeover_screen_reconnecting() {
    let state = ServerPaneState::new();
    state.set(ServerPaneStatus::Takeover);
    assert!(state.connection_failed(ServerError::NotRunning, true));
    assert_eq!(state.status(), ServerPaneStatus::TakeoverReconnecting);
}

#[test]
fn persistent_protocol_failure_stops_after_three_automatic_reconnections() {
    for takeover in [false, true] {
        let state = ServerPaneState::new();
        for _ in 0..3 {
            assert!(state.connection_failed(ServerError::Protocol, takeover));
        }
        assert!(!state.connection_failed(ServerError::Protocol, takeover));
        assert_eq!(
            state.status(),
            if takeover {
                ServerPaneStatus::TakeoverFailed(ServerError::Protocol)
            } else {
                ServerPaneStatus::Failed(ServerError::Protocol)
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
        ServerPaneStatus::Failed(ServerError::Protocol)
    );
    state.retry();
    assert!(state.connection_failed(ServerError::Protocol, false));
}

#[test]
fn an_outage_during_protocol_recovery_retains_the_original_failure() {
    let state = ServerPaneState::new();
    assert!(state.connection_failed(ServerError::Protocol, false));
    assert!(state.connection_failed(ServerError::NotRunning, false));
    assert!(state.connection_failed(ServerError::NotRunning, false));
    assert!(!state.connection_failed(ServerError::NotRunning, false));
    assert_eq!(
        state.status(),
        ServerPaneStatus::Failed(ServerError::Protocol)
    );
}

#[test]
fn paired_reachability_does_not_renew_protocol_recovery_or_identity_refusals() {
    use spaceterm_server::direct::DirectError;
    for takeover in [false, true] {
        for refusal in [
            DirectError::PinMismatch,
            DirectError::Revoked,
            DirectError::AuthenticationRefused,
            DirectError::CredentialUnavailable,
            DirectError::InteractionRequired,
            DirectError::IncompatibleProtocol,
        ] {
            let state = ServerPaneState::new();
            assert!(state.connection_failed(ServerError::Direct(DirectError::Transport), takeover));
            assert!(!state.connection_failed(ServerError::Direct(refusal), takeover));
            state.server_reachable();
            assert!(!state.retry.load(Ordering::Acquire));
        }

        let state = ServerPaneState::new();
        assert!(state.connection_failed(ServerError::Direct(DirectError::Transport), takeover));
        assert!(state.connection_failed(ServerError::Protocol, takeover));
        for retryable in [true, false] {
            state.server_reachable();
            assert!(!state.retry.load(Ordering::Acquire));
            assert_eq!(
                state.connection_failed(ServerError::Direct(DirectError::Transport), takeover),
                retryable
            );
        }
        assert_eq!(
            state.status(),
            if takeover {
                ServerPaneStatus::TakeoverFailed(ServerError::Protocol)
            } else {
                ServerPaneStatus::Failed(ServerError::Protocol)
            }
        );
    }
}

/// Selecting a Pane whose Session ended in this server run, by focus, Tab opening or a click,
/// queues nothing: only Start Session starts its shell, whoever holds its grant.
#[test]
fn selecting_an_ended_pane_queues_no_start() {
    use pane::control::Controller;
    let ended = ServerPaneState::new();
    ended.set(ServerPaneStatus::SessionEnded(pane::SessionEnd::Exited {
        code: 1,
    }));
    ended.set_session_end(Some(pane::SessionEnd::Exited { code: 1 }));
    ended.select();
    assert!(!ended.selected.load(Ordering::Acquire));

    let elsewhere = ServerPaneState::new();
    elsewhere.offered_take_over(Controller::Nobody, Some(pane::SessionEnd::EndAllSessions));
    elsewhere.select();
    assert!(!elsewhere.selected.load(Ordering::Acquire));

    // A Pane retained across a server restart still starts on first use.
    let restored = ServerPaneState::new();
    restored.set(ServerPaneStatus::Restored);
    restored.select();
    assert!(restored.selected.load(Ordering::Acquire));
}

#[test]
fn hiding_a_pane_keeps_its_semantic_demand_and_document() {
    let adapter = Adapter::new();
    adapter
        .handle
        .accessibility_demand_sender()
        .expect("a server Pane accepts semantic demand")
        .request();
    adapter.handle.set_presentable(false);
    assert!(
        adapter
            .handle
            .state
            .accessibility_demand
            .load(Ordering::Acquire),
        "hiding a Pane must not end its semantic demand, or showing it would resend everything"
    );
    adapter.handle.set_presentable(true);
    assert!(
        adapter
            .handle
            .state
            .accessibility_demand
            .load(Ordering::Acquire)
    );
}

use spaceterm_server::connection::link::{ChannelEvent, ChannelSink as _};
use spaceterm_server::protocol::PaneEvent;

/// A Pane worker's pushed side for semantic tests: a real terminal, its server encoder, the
/// worker's inbox, its frame receiver and its semantic state, with the view's one-model channel.
struct PushedPane {
    adapter: Adapter,
    emulator: crate::terminal::emulator::TerminalEmulator,
    encoder: crate::runtime::screen_encoder::ScreenEncoder,
    inbox: Arc<super::inbox::Inbox>,
    screens: screen::ScreenReceiver<'static>,
    graphics: graphics::Graphics,
    predictor: prediction::Predictor,
    shown: Option<screen::Shown>,
    generation: PresentationGeneration,
    mapping: Option<ScreenMapping>,
    semantic: accessibility::Semantics,
    events: (
        async_channel::Sender<TerminalSessionEvent>,
        async_channel::Receiver<TerminalSessionEvent>,
    ),
    semantics: (
        async_channel::Sender<Arc<crate::terminal::accessibility::TerminalAccessibilityModel>>,
        async_channel::Receiver<Arc<crate::terminal::accessibility::TerminalAccessibilityModel>>,
    ),
    frame: u64,
    screen_generation: u64,
}

impl PushedPane {
    /// A demanded Pane whose terminal printed `history + 6` lines on a 20 by 6 grid.
    fn new(history: u64) -> Self {
        use super::super::geometry::{BackingScale, CellGridSize, LogicalCellSize};
        let adapter = Adapter::new();
        adapter
            .handle
            .state
            .reading
            .lock()
            .unwrap()
            .scrollback
            .attach(adapter.active.lease.session);
        let mut emulator =
            crate::terminal::emulator::TerminalEmulator::new(TerminalGeometry::from_grid(
                CellGridSize::new(20, 6),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ))
            .unwrap();
        for _ in 0..history + 5 {
            emulator.feed(b"line\r\n");
        }
        emulator.feed(b"line");
        let mut semantic = accessibility::Semantics::new();
        semantic.follow(true, |_| Ok(())).unwrap();
        Self {
            adapter,
            emulator,
            encoder: Default::default(),
            inbox: super::inbox::Inbox::new(Arc::new(|| {})),
            screens: screen::ScreenReceiver::new(None, None),
            graphics: Default::default(),
            predictor: Default::default(),
            shown: None,
            generation: PresentationGeneration::default(),
            mapping: None,
            semantic,
            events: async_channel::unbounded(),
            semantics: async_channel::bounded(1),
            frame: 0,
            screen_generation: 0,
        }
    }

    /// Pushes the terminal's current screen as the channel's next frame.
    fn push_frame(&mut self) {
        let screen = self.emulator.snapshot().unwrap().unwrap();
        self.screen_generation += 1;
        let capture = crate::runtime::screen_encoder::ScreenCapture {
            screen,
            generation: self.screen_generation,
            accessibility_revision: 0,
        };
        let base = (self.frame > 0).then_some(self.frame);
        self.frame += 1;
        let update = self.encoder.encode(&capture, self.frame, base).unwrap();
        self.inbox
            .deliver(ChannelEvent::Frame(Box::new(spaceterm_server::protocol::Frame {
                channel: spaceterm_server::protocol::Channel(1),
                frame: self.frame,
                base,
                input: 0,
                update,
            })));
    }

    fn push_semantics(&self, messages: Vec<pane::accessibility::Update>) {
        for message in messages {
            self.inbox
                .deliver(ChannelEvent::Accessibility(Box::new(message)));
        }
    }

    fn push(&self, event: PaneEvent) {
        self.inbox.deliver(ChannelEvent::Pane(event));
    }

    /// One worker turn over everything pushed: the drain applies frames and stages semantics,
    /// each delivered screen is shown, and the drain's semantics publish after them, as
    /// `link::present` does. Returns the drain's flow and its Pane events.
    fn turn(&mut self) -> (super::inbox::Flow, Vec<PaneEvent>) {
        let state = &self.adapter.handle.state;
        let mut delivered = Vec::new();
        let flow = self
            .inbox
            .drain(
                &mut screen::Frames {
                    state,
                    screens: &mut self.screens,
                    epoch: self.adapter.active.epoch,
                    generation: &mut self.generation,
                    active_size: *self.adapter.handle.size.lock().unwrap(),
                    graphics: &mut self.graphics,
                    appearance: &self.adapter.handle.appearance,
                    semantic: &mut self.semantic,
                    shown: &mut self.shown,
                    predictor: &mut self.predictor,
                },
                |_| Ok::<_, ServerError>(()),
                |delivery| delivered.push(delivery),
            )
            .unwrap();
        let mut events = Vec::new();
        for delivery in delivered {
            match delivery {
                super::inbox::Delivery::Screen((screen, mapping)) => {
                    self.mapping = Some(
                        super::link::show(
                            state,
                            (&self.events.0, &self.semantics.0),
                            &mut self.semantic,
                            Some(screen),
                            (self.adapter.active.epoch, self.generation, mapping),
                            false,
                        )
                        .unwrap(),
                    );
                }
                super::inbox::Delivery::Pane(event) => events.push(event),
            }
        }
        if let Some(model) = self
            .semantic
            .publish(&state.accessibility, self.generation)
        {
            self.semantics.0.force_send(model).unwrap();
        }
        (flow, events)
    }

    /// The model the view holds now: the last one the worker sent.
    fn published(&self) -> Arc<crate::terminal::accessibility::TerminalAccessibilityModel> {
        self.semantics.1.try_recv().expect("a published model")
    }
}

/// A revision of `lines` rows numbered from 0, each holding "line", that inserts the rows from
/// `from` and shows the last 6.
fn semantic_lines(
    revision: u64,
    base: Option<u64>,
    from: u32,
    lines: u32,
) -> Vec<pane::accessibility::Update> {
    use pane::accessibility::{Header, Packer, Run, RowUpdate, Screen, Tail};
    let mut packer = Packer::new(Header {
        demand_generation: 1,
        revision,
        base,
        screen: Screen::Primary,
        screen_generation: 1,
    });
    let mut messages = packer.splice(
        from,
        0,
        vec![Run {
            node_serial: 7,
            page_row: from as u16,
            count: (lines - from) as u16,
        }],
    );
    for line in from..lines {
        messages.extend(packer.row(RowUpdate::new(
            line,
            revision,
            false,
            "line".into(),
            Vec::new(),
        )));
    }
    messages.push(packer.finish(Tail {
        visible_lines: lines - 6..lines,
        cursor: None,
        selection: None,
    }));
    messages
}

#[test]
fn a_frame_then_a_semantic_tail_in_one_drain_publish_the_view_the_frame_moved() {
    let mut pane = PushedPane::new(40);
    pane.push_frame();
    pane.push_semantics(semantic_lines(1, None, 0, 46));
    pane.turn();
    pane.adapter
        .handle
        .scroll_scrollback(ScrollbackMovement::Top);
    pane.generation = pane.generation.next();
    let state = Arc::clone(&pane.adapter.handle.state);
    pane.mapping = Some(
        super::link::show(
            &state,
            (&pane.events.0, &pane.semantics.0),
            &mut pane.semantic,
            None,
            (pane.adapter.active.epoch, pane.generation, pane.mapping.unwrap()),
            false,
        )
        .unwrap(),
    );
    assert_eq!(
        pane.published().visible_lines(),
        0..6,
        "scrolled to the top of 40 history lines, the first 6 rows are shown"
    );

    // Output adds a history line while the view stays at the top, so the view's scrolled
    // offset grows from 40 to 41. Its frame and the revision that appends the line arrive
    // before the worker drains.
    pane.emulator.feed(b"\r\nline");
    pane.push_frame();
    pane.push_semantics(semantic_lines(2, Some(1), 46, 47));
    pane.turn();

    let model = pane.published();
    assert_eq!(model.rows().count(), 47, "revision 2 is published");
    assert_eq!(model.generation(), pane.generation);
    assert_eq!(
        model.visible_lines(),
        0..6,
        "the published model follows the view the frame moved, not the one before it"
    );
}

#[test]
fn a_semantic_tail_before_the_channels_end_publishes_the_ended_panes_document() {
    let mut pane = PushedPane::new(0);
    pane.push_frame();
    pane.push_semantics(semantic_lines(1, None, 0, 6));
    pane.turn();
    assert_eq!(pane.published().rows().count(), 6);

    let mut unfinished = semantic_lines(3, Some(2), 7, 8);
    unfinished.pop();
    pane.push_semantics(semantic_lines(2, Some(1), 6, 7));
    pane.push(PaneEvent::Ended);
    pane.push_semantics(unfinished);
    let (flow, events) = pane.turn();

    assert_eq!(flow, super::inbox::Flow::Ended);
    assert!(matches!(events.as_slice(), [PaneEvent::Ended]));
    let model = pane.published();
    assert_eq!(
        model.rows().count(),
        7,
        "the revision completed before the end is the final document"
    );
    assert!(pane.semantics.1.try_recv().is_err());
}
