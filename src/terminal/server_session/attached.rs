//! One attached lease's worker turns. Each turn sends queued input, then at most one output
//! check, then the rest of that check's cycle. The caller supplies the exchange and the waits.
use super::*;
use pane::presentation::ScreenDecoder;
use std::time::Instant;

/// What the presentation writes to and reads from, owned by the Pane's presentation.
pub(super) struct Presentation<'a> {
    pub(super) state: &'a DevelopmentPaneState,
    pub(super) input: &'a mpsc::Receiver<SubmittedInput>,
    pub(super) events: &'a async_channel::Sender<TerminalSessionEvent>,
    pub(super) semantics:
        &'a async_channel::Sender<Arc<super::super::accessibility::TerminalAccessibilityModel>>,
    pub(super) size: &'a Mutex<PaneSize>,
    pub(super) appearance: &'a Mutex<TerminalAppearanceUpdate>,
    pub(super) local_files: Option<&'a crate::platform::local_filesystem::LocalFilesystemAuthority>,
    pub(super) remote_metadata: Option<&'a super::super::metadata::RemoteTerminalMetadataContext>,
    pub(super) effects: &'a mut ClientEffects,
    /// Shared with the presentation across reconnection, so local generations only increase.
    pub(super) generation: &'a mut PresentationGeneration,
    pub(super) takeover: &'a mut bool,
    pub(super) paste_unconfirmed: &'a mut bool,
}

/// The connection's optional operations, read once per attachment.
#[derive(Clone, Copy)]
pub(super) struct Supports {
    pub(super) changes: bool,
    pub(super) batches: bool,
    pub(super) interactive: bool,
    pub(super) reading: bool,
    pub(super) effects: bool,
    pub(super) graphics: bool,
    pub(super) accessibility: bool,
}

/// What the worker does after a turn.
pub(super) enum Turn {
    /// The Pane's Terminal Session ended.
    Ended,
    /// Start the next turn now.
    Again,
    /// Wait for a wake or this deadline, then start the next turn.
    WaitUntil(Instant),
}

pub(super) struct Attached<'a> {
    presentation: Presentation<'a>,
    supports: Supports,
    epoch: u64,
    lease: Lease,
    grant_size: PaneSize,
    grant_appearance: Option<crate::appearance::AppearanceGeneration>,
    active_input: ActiveInput,
    active_size: PaneSize,
    ready: bool,
    reader: reading::Reader,
    graphics: graphics::Graphics,
    semantic: accessibility::Fetcher,
    latest_screen: Option<(Arc<ScreenSnapshot>, ScreenMapping)>,
    /// Row changes apply only to this exact lease's own previous screen.
    decoder: ScreenDecoder,
    cadence: schedule::Cadence,
    /// An output check completed and the rest of its cycle has not run yet.
    cycle_owed: bool,
}

impl<'a> Attached<'a> {
    pub(super) fn new(
        presentation: Presentation<'a>,
        supports: Supports,
        epoch: u64,
        grant: claim::Granted,
    ) -> Self {
        let visible = presentation.state.presentable.load(Ordering::Acquire);
        Self {
            presentation,
            supports,
            epoch,
            lease: grant.lease,
            grant_size: grant.size,
            grant_appearance: grant.appearance,
            active_input: ActiveInput {
                epoch,
                lease: grant.lease,
                focus_sent: None,
            },
            active_size: grant.size,
            ready: false,
            reader: reading::Reader::default(),
            graphics: graphics::Graphics::default(),
            semantic: accessibility::Fetcher::new(grant.lease),
            latest_screen: None,
            decoder: ScreenDecoder::default(),
            cadence: schedule::Cadence::new(Instant::now(), visible),
            cycle_owed: false,
        }
    }

    /// One worker turn. `current` says whether this presentation still owns the Pane.
    pub(super) fn turn(
        &mut self,
        send: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
        current: &impl Fn() -> bool,
    ) -> Result<Turn, ServerError> {
        let state = self.presentation.state;
        let lease = self.lease;
        if state.epoch.load(Ordering::Acquire) != self.epoch {
            return Err(ServerError::Protocol);
        }
        // Queued input is sent before any output check.
        let mut active_size = self.active_size;
        let echoes = self.active_input.drain(
            state,
            self.presentation.input,
            self.supports.batches,
            &mut |request| {
                let reply = send(request)?;
                if let Request::Resize { size, .. } = request {
                    active_size = *size;
                }
                Ok(reply)
            },
            self.presentation.paste_unconfirmed,
        )?;
        self.active_size = active_size;
        // A key batch's reply carries the next publication exactly as an output check would.
        for echo in echoes {
            let output = self.decoder.apply(echo)?;
            self.accept(output, send, current)?;
        }
        let now = Instant::now();
        self.cadence
            .set_visible(state.presentable.load(Ordering::Acquire), now);
        self.cadence
            .set_preparation(state.preparation.load(Ordering::Acquire));
        if self.supports.changes
            && let Some(visible) = self.cadence.take_demand()
        {
            expect_unchanged(send(&Request::PresentationDemand { lease, visible })?)?;
            // A changed demand restarts row changes from a complete screen.
            self.decoder = ScreenDecoder::default();
        }
        if self.cadence.poll_due(now) {
            if self.check(send, current)? {
                return Ok(Turn::Ended);
            }
            self.cycle_owed = true;
            // Queued input and view changes go before the rest of this cycle.
            if state.wake.notified() {
                return Ok(Turn::Again);
            }
        }
        self.finish_cycle(send)
    }

    /// One output check, which also carries native effects and reading completion. Returns
    /// whether the Pane's Terminal Session ended.
    fn check(
        &mut self,
        send: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
        current: &impl Fn() -> bool,
    ) -> Result<bool, ServerError> {
        let state = self.presentation.state;
        let lease = self.lease;
        if self.ready {
            // The check takes effects for the newest acknowledged focus generation.
            self.active_input.sync_focus(state, send)?;
        }
        let effects = self
            .active_input
            .focus_sent
            .map(|(generation, _)| generation)
            .filter(|_| self.ready && self.supports.effects);
        if let Some(generation) = effects {
            self.presentation
                .effects
                .complete(state, lease, generation, send)?;
        }
        let reading = self.ready && self.supports.reading && self.reader.awaiting();
        let begun = effects.map(|generation| ClientEffects::begin(state, generation));
        let reply = send(&Request::PollChanges {
            lease,
            base: self.decoder.baseline(),
            effects,
            reading,
        })?;
        // A reply that completed after a successor claimed the Pane is not published.
        if !current() {
            return Err(ServerError::NotRunning);
        }
        if state.epoch.load(Ordering::Acquire) != self.epoch {
            return Err(ServerError::Protocol);
        }
        let cycle = match reply {
            Reply::Cycle(cycle) => *cycle,
            Reply::Ended => {
                state.set(DevelopmentPaneStatus::Ended);
                let _ = self
                    .presentation
                    .events
                    .force_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success));
                return Ok(true);
            }
            _ => return Err(ServerError::Protocol),
        };
        if cycle.effects.is_some() != effects.is_some() || cycle.reading.is_some() != reading {
            return Err(ServerError::Protocol);
        }
        let output = self.decoder.apply(cycle.output)?;
        self.accept(output, send, current)?;
        if let (Some(begun), Some(polled)) = (begun, cycle.effects) {
            self.presentation
                .effects
                .apply(state, begun, polled, self.presentation.events)?;
        }
        if let Some(status) = cycle.reading {
            // The status follows the screen it names, which `accept` has already recorded.
            match self.reader.settle(state, status) {
                reading::Next::Refresh => self.cadence.poll_now(Instant::now()),
                reading::Next::Recheck => self.cadence.recheck(Instant::now()),
                reading::Next::Idle | reading::Next::Continue => {}
            }
        }
        Ok(false)
    }

    /// The rest of an output cycle: focus, reading, images and semantics.
    fn finish_cycle(
        &mut self,
        send: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<Turn, ServerError> {
        let state = self.presentation.state;
        let events = self.presentation.events;
        let owed = std::mem::take(&mut self.cycle_owed);
        if self.ready {
            self.active_input.sync_focus(state, send)?;
        }
        let mut next = reading::Next::Idle;
        if self.ready && self.supports.reading {
            next = self.reader.poll(state, self.lease, send)?;
            let mut reading = state.reading.lock().unwrap_or_else(|e| e.into_inner());
            reading.publish_ready(events);
        }
        // A hidden Pane transfers no images.
        if owed && self.cadence.visible() && self.ready && self.supports.graphics {
            let result = self.graphics.poll(self.lease, send)?;
            if state.epoch.load(Ordering::Acquire) != self.epoch {
                return Err(ServerError::Protocol);
            }
            let changed = match result {
                Ok(changed) => changed,
                Err(error) => self.graphics.failure(error),
            };
            let recovered = self.graphics.recovered();
            if (changed || recovered)
                && let Some((screen, mapping)) = &mut self.latest_screen
            {
                let generation = self.presentation.generation.next();
                *self.presentation.generation = generation;
                let next = Arc::make_mut(screen);
                next.generation = generation;
                next.graphics = self.graphics.snapshot();
                mapping.local = generation;
                state.connected(self.epoch, *mapping, *self.presentation.paste_unconfirmed)?;
                self.reader.screen(state, *mapping, next.selection_present);
                let _ = events.force_send(TerminalSessionEvent::Screen(Arc::clone(screen)));
                if let Some(model) = self.semantic.rebind(state, generation) {
                    let _ = self.presentation.semantics.force_send(model);
                }
            }
        }
        // Semantic transfer follows its own explicit demand, even while hidden, and a
        // partial document continues one chunk per turn.
        if (owed || self.semantic.can_continue()) && self.ready && self.supports.accessibility {
            let model = self
                .semantic
                .poll(state, *self.presentation.generation, send)?;
            if state.epoch.load(Ordering::Acquire) != self.epoch {
                return Err(ServerError::Protocol);
            }
            if let Some(model) = model {
                let _ = self.presentation.semantics.force_send(model);
            }
        }
        if self.semantic.can_continue() {
            next = next.max(reading::Next::Continue);
        }
        if owed && self.ready {
            // A complete healthy round renews recovery even when its screen is unchanged.
            state
                .connection_recovery
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .connected(Instant::now());
        }
        let now = Instant::now();
        Ok(match next {
            reading::Next::Idle => Turn::WaitUntil(self.cadence.deadline()),
            reading::Next::Recheck => {
                self.cadence.recheck(now);
                Turn::WaitUntil(self.cadence.deadline())
            }
            reading::Next::Continue => Turn::Again,
            reading::Next::Refresh => {
                self.cadence.poll_now(now);
                Turn::Again
            }
        })
    }

    /// Accepts one decoded presentation, from an output check or a key batch's reply.
    fn accept(
        &mut self,
        output: Reply,
        send: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
        current: &impl Fn() -> bool,
    ) -> Result<(), ServerError> {
        // A reply that completed after a successor claimed the Pane is not published.
        if !current() {
            return Err(ServerError::NotRunning);
        }
        if self.presentation.state.epoch.load(Ordering::Acquire) != self.epoch {
            return Err(ServerError::Protocol);
        }
        match output {
            Reply::Screen(screen) => self.present_screen(screen, send),
            Reply::Unchanged => Ok(()),
            Reply::AccessibilityRevision { revision } => {
                self.semantic.note_revision(revision);
                Ok(())
            }
            _ => Err(ServerError::Protocol),
        }
    }

    fn present_screen(
        &mut self,
        mut screen: Box<pane::Screen>,
        send: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<(), ServerError> {
        let state = self.presentation.state;
        let events = self.presentation.events;
        let epoch = self.epoch;
        self.semantic.note_revision(screen.accessibility_revision);
        let graphics_error = self
            .graphics
            .update(screen.graphics.take(), screen.alternate)
            .err();
        if let Some(error) = graphics_error {
            self.graphics.clear();
            self.graphics.failure(error);
        }
        self.graphics.recovered();
        let generation = self.presentation.generation.next();
        *self.presentation.generation = generation;
        let mapping = ScreenMapping {
            epoch,
            local: generation,
            wire: screen.generation,
            selection: screen
                .reading
                .as_ref()
                .map_or(0, |reading| reading.selection_revision),
            size: PaneSize {
                columns: screen.columns,
                rows: screen.rows.len() as u16,
                cell_width: screen
                    .cell_pixels
                    .map_or(self.active_size.cell_width, |p| p.0),
                cell_height: screen
                    .cell_pixels
                    .map_or(self.active_size.cell_height, |p| p.1),
            },
            mouse_tracking: screen.mouse_tracking,
            alternate: screen.alternate,
            alternate_scroll: screen.alternate_scroll,
        };
        let mut screen = decode_screen(
            *screen,
            generation,
            &self
                .presentation
                .appearance
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            self.presentation.local_files,
        )?;
        if let Some(metadata) = self.presentation.remote_metadata {
            Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata).context =
                super::super::metadata::TerminalMetadataContext::Remote(metadata.clone());
        }
        Arc::make_mut(&mut screen).graphics = self.graphics.snapshot();
        if !self.ready {
            // Claims apply their measured controller facts once.
            // Reconcile changes made while attachment was in flight.
            let current_size = *self
                .presentation
                .size
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let update = self
                .presentation
                .appearance
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if current_size != self.grant_size {
                expect_unchanged(send(&Request::Resize {
                    lease: self.lease,
                    size: current_size,
                })?)?;
                self.active_size = current_size;
            }
            if self.grant_appearance != Some(update.generation) {
                expect_unchanged(send(&Request::Style {
                    lease: self.lease,
                    style: encode_style(&update),
                })?)?;
            }
            state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .interactive = self.supports.interactive;
            state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reading = self.supports.reading;
            // Publish the authorized screen first. Input stays queued
            // until this worker's focus barrier is acknowledged.
            self.ready = true;
        }
        state.connected(epoch, mapping, *self.presentation.paste_unconfirmed)?;
        self.reader.screen(state, mapping, screen.selection_present);
        self.latest_screen = Some((Arc::clone(&screen), mapping));
        measurement::record(MeasurementEvent::ScreenDecoded {
            generation: generation.as_u64(),
            wire_generation: mapping.wire,
        });
        let _ = events.force_send(TerminalSessionEvent::Screen(screen));
        if let Some(model) = self.semantic.rebind(state, generation) {
            let _ = self.presentation.semantics.force_send(model);
        }
        *self.presentation.takeover = false;
        if *self.presentation.paste_unconfirmed {
            *self.presentation.paste_unconfirmed = false;
            let _ = events.force_send(TerminalSessionEvent::PasteUnconfirmed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Adapter, typed};
    use super::*;
    use crate::terminal::session::TerminalSessionHandle;

    const ALL: Supports = Supports {
        changes: true,
        batches: true,
        interactive: true,
        reading: true,
        effects: true,
        graphics: false,
        accessibility: false,
    };

    fn wire_screen(generation: u64, text: &str) -> pane::Screen {
        let cell = |text: &str| pane::Cell {
            hyperlink: None,
            text: text.into(),
            foreground: pane::Color::Default,
            background: pane::Color::Default,
            underline_color: pane::Color::Default,
            flags: 0,
            underline: 0,
        };
        pane::Screen {
            accessibility_revision: 0,
            graphics: None,
            cell_pixels: None,
            reading: None,
            hyperlinks: Vec::new(),
            generation,
            mouse_tracking: false,
            alternate_scroll: false,
            columns: 2,
            rows: vec![vec![cell(text), cell(" ")], vec![cell(" "), cell(" ")]],
            wrapped: vec![false; 2],
            foreground: [255, 255, 255],
            background: [0, 0, 0],
            palette: vec![[0, 0, 0]; 256],
            reversed: false,
            foreground_overridden: false,
            background_overridden: false,
            cursor_overridden: false,
            palette_overrides: vec![false; 256],
            cursor: None,
            cursor_visible: false,
            cursor_blinking: false,
            cursor_shape: 0,
            cursor_color: [0, 0, 0],
            cursor_text: [0, 0, 0],
            alternate: false,
        }
    }

    fn cycle(
        output: Reply,
        effects: Option<pane::effects::Poll>,
        reading: Option<pane::reading::Status>,
    ) -> Reply {
        Reply::Cycle(Box::new(pane::presentation::Cycle {
            output,
            effects,
            reading,
        }))
    }

    /// The facts an output check asked for, answered with nothing new.
    fn quiet(request: &Request) -> Reply {
        let Request::PollChanges {
            effects, reading, ..
        } = request
        else {
            return Reply::Unchanged;
        };
        cycle(
            Reply::Unchanged,
            effects.map(|_| pane::effects::Poll {
                request: None,
                hidden_input: false,
            }),
            reading.then_some(pane::reading::Status::Ready {
                generation: 1,
                selection_revision: 0,
                selection_present: false,
            }),
        )
    }

    /// Owns what a worker's presentation borrows mutably or keeps alive.
    struct Parts {
        events: async_channel::Sender<TerminalSessionEvent>,
        published: async_channel::Receiver<TerminalSessionEvent>,
        semantics: async_channel::Sender<
            Arc<super::super::super::accessibility::TerminalAccessibilityModel>,
        >,
        effects: ClientEffects,
        generation: PresentationGeneration,
        takeover: bool,
        paste_unconfirmed: bool,
    }
    fn fixture() -> (Adapter, Parts) {
        let adapter = Adapter::new();
        adapter
            .handle
            .state
            .presentable
            .store(true, Ordering::Release);
        let (events, published) = async_channel::bounded(64);
        let (semantics, _) = async_channel::bounded(1);
        let parts = Parts {
            events,
            published,
            semantics,
            effects: ClientEffects::new().0,
            generation: PresentationGeneration::default(),
            takeover: false,
            paste_unconfirmed: false,
        };
        (adapter, parts)
    }
    fn worker<'a>(adapter: &'a Adapter, parts: &'a mut Parts) -> Attached<'a> {
        let handle = &adapter.handle;
        let grant = claim::Granted {
            lease: adapter.active.lease,
            size: *handle.size.lock().unwrap(),
            appearance: Some(handle.appearance.lock().unwrap().generation),
        };
        Attached::new(
            Presentation {
                state: &handle.state,
                input: &adapter.input,
                events: &parts.events,
                semantics: &parts.semantics,
                size: &handle.size,
                appearance: &handle.appearance,
                local_files: None,
                remote_metadata: None,
                effects: &mut parts.effects,
                generation: &mut parts.generation,
                takeover: &mut parts.takeover,
                paste_unconfirmed: &mut parts.paste_unconfirmed,
            },
            ALL,
            adapter.active.epoch,
            grant,
        )
    }

    /// Runs one turn after any pending wake has been consumed, recording each exchange.
    fn turn(
        worker: &mut Attached<'_>,
        state: &DevelopmentPaneState,
        mut answer: impl FnMut(&Request) -> Reply,
    ) -> (Vec<Request>, Turn) {
        state.wake.wait_until(Instant::now());
        let mut sent = Vec::new();
        let turn = worker
            .turn(
                &mut |request| {
                    sent.push(request.clone());
                    Ok(answer(request))
                },
                &|| true,
            )
            .unwrap();
        (sent, turn)
    }

    /// Attaches: the first output check presents a screen and the focus barrier follows.
    fn attach(worker: &mut Attached<'_>, state: &DevelopmentPaneState) {
        let (sent, _) = turn(worker, state, |request| match request {
            Request::PollChanges { .. } => {
                cycle(Reply::Screen(Box::new(wire_screen(1, "a"))), None, None)
            }
            _ => Reply::Unchanged,
        });
        assert!(
            sent.iter()
                .any(|request| matches!(request, Request::Focus { .. })),
            "the first screen enables input behind the focus barrier"
        );
    }

    fn kinds(sent: &[Request]) -> Vec<&'static str> {
        sent.iter()
            .map(|request| match request {
                Request::Keys { .. } => "keys",
                Request::PollChanges { .. } => "check",
                Request::Read { .. } => "read",
                Request::Focus { .. } => "focus",
                Request::CompleteClipboard { .. } => "clipboard",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn a_visible_output_cycle_is_one_exchange() {
        let (adapter, mut parts) = fixture();
        let state = &adapter.handle.state;
        let mut worker = worker(&adapter, &mut parts);
        attach(&mut worker, state);
        // An accepted scroll leaves its completion and native effects to the output check.
        state
            .reading
            .lock()
            .unwrap()
            .enqueue(pane::reading::Operation::Scroll(
                pane::reading::Scroll::PageUp,
            ));
        let (sent, next) = turn(&mut worker, state, |request| match request {
            Request::Read { .. } => Reply::Reading { accepted: true },
            request => quiet(request),
        });
        assert_eq!(kinds(&sent), ["read"]);
        assert!(matches!(next, Turn::Again), "completion is checked at once");
        let (sent, next) = turn(&mut worker, state, quiet);
        assert_eq!(kinds(&sent), ["check"], "one exchange: {sent:?}");
        assert!(matches!(
            &sent[0],
            Request::PollChanges {
                effects: Some(_),
                reading: true,
                ..
            }
        ));
        assert!(matches!(next, Turn::WaitUntil(_)));
        // A quiet visible cycle with no reading work is the check alone.
        std::thread::sleep(Duration::from_millis(40));
        let (sent, _) = turn(&mut worker, state, quiet);
        assert_eq!(kinds(&sent), ["check"], "one exchange: {sent:?}");
        assert!(matches!(
            &sent[0],
            Request::PollChanges {
                effects: Some(_),
                reading: false,
                ..
            }
        ));
    }

    #[test]
    fn queued_keys_precede_the_rest_of_an_output_cycle() {
        let (adapter, mut parts) = fixture();
        let state = &adapter.handle.state;
        let mut worker = worker(&adapter, &mut parts);
        attach(&mut worker, state);
        state
            .reading
            .lock()
            .unwrap()
            .enqueue(pane::reading::Operation::Scroll(
                pane::reading::Scroll::PageUp,
            ));
        std::thread::sleep(Duration::from_millis(40));
        let mut sent = Vec::new();
        let mut exchange = |request: &Request| -> Result<Reply, ServerError> {
            sent.push(request.clone());
            Ok(match request {
                Request::PollChanges { .. } => {
                    // The person types while the output check is in flight.
                    for key in typed("k") {
                        adapter.handle.key(key);
                    }
                    quiet(request)
                }
                Request::Read { .. } => Reply::Reading { accepted: true },
                request => quiet(request),
            })
        };
        assert!(matches!(
            worker.turn(&mut exchange, &|| true).unwrap(),
            Turn::Again
        ));
        worker.turn(&mut exchange, &|| true).unwrap();
        assert_eq!(kinds(&sent), ["check", "keys", "read"]);
    }

    #[test]
    fn a_key_batch_reply_presents_its_echo_and_the_next_check_continues_from_it() {
        let (adapter, mut parts) = fixture();
        let published = parts.published.clone();
        let state = &adapter.handle.state;
        let mut worker = worker(&adapter, &mut parts);
        attach(&mut worker, state);
        while published.try_recv().is_ok() {}
        let mut encoder = pane::presentation::ScreenEncoder::default();
        encoder.encode(wire_screen(1, "a"), None);
        let echo = encoder.encode(wire_screen(2, "k"), Some(1));
        assert!(matches!(echo, Reply::ScreenChanges(_)));
        let mut echo = Some(echo);
        for key in typed("k") {
            adapter.handle.key(key);
        }
        let (sent, _) = turn(&mut worker, state, |request| match request {
            Request::Keys { .. } => echo.take().expect("one key batch"),
            request => quiet(request),
        });
        assert_eq!(kinds(&sent), ["keys"], "the echo needs no output check");
        let presented: Vec<_> = std::iter::from_fn(|| published.try_recv().ok())
            .filter_map(|event| match event {
                TerminalSessionEvent::Screen(screen) => Some(screen),
                _ => None,
            })
            .collect();
        assert_eq!(presented.len(), 1);
        assert_eq!(presented[0].rows[0][0].text.as_str(), "k");
        std::thread::sleep(Duration::from_millis(40));
        let (sent, _) = turn(&mut worker, state, quiet);
        assert!(
            matches!(&sent[..], [Request::PollChanges { base: Some(2), .. }]),
            "{sent:?}"
        );
    }
}
