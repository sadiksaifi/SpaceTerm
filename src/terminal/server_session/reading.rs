//! Keeps transport work off native Copy and Services callbacks.
use super::*;
use pane::reading::{MAX_SELECTION_BYTES, Operation, SELECTION_CHUNK_BYTES};
use std::collections::VecDeque;

/// One queued reading operation and the identity its answer settles.
struct Queued {
    id: u64,
    operation: Operation,
}

#[derive(Default)]
pub(super) struct ReadingState {
    revision: u64,
    pub(super) selection_present: bool,
    pub(super) pending_copy: Option<(u64, u64, InputModifiers)>,
    queue: VecDeque<Queued>,
    next_id: u64,
    /// The queued operation the worker is sending. Nothing changes it in place while it is
    /// sent; removing it is allowed, and its answer then removes nothing else.
    sending: Option<u64>,
    prepared: Option<(u64, ScreenMapping, super::super::SelectionCopy)>,
    pointer_copy: Option<u64>,
    notify: bool,
    /// The server refused this gesture's Press; its continuation is dropped until the next one.
    abandoned_gesture: bool,
}
impl ReadingState {
    pub(super) fn publish_ready(&mut self, events: &async_channel::Sender<TerminalSessionEvent>) {
        if self.notify
            && events
                .try_send(TerminalSessionEvent::SelectionCopyReady)
                .is_ok()
        {
            self.notify = false;
        }
    }
    pub(super) fn invalidate(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.prepared = None;
        self.pointer_copy = None;
        self.notify = false;
    }
    pub(super) fn disconnect(&mut self) {
        self.invalidate();
        self.queue.clear();
        self.abandoned_gesture = false;
        self.selection_present = false;
        self.pending_copy = None;
    }
    /// Drops the refused gesture's queued continuation. Nothing is replayed, and a later
    /// Motion never starts a gesture of its own.
    pub(super) fn abandon_gesture(&mut self) {
        let press = |queued: &Queued| matches!(&queued.operation, Operation::Pointer(p) if matches!(p.phase, pane::PointerPhase::Press));
        while let Some(index) = self
            .queue
            .iter()
            .take_while(|queued| !press(queued))
            .position(|queued| continues_gesture(&queued.operation))
        {
            self.queue.remove(index);
        }
        // Once a later Press is queued, new continuation belongs to that gesture.
        self.abandoned_gesture = !self.queue.iter().any(press);
    }
    pub(super) fn enqueue(&mut self, operation: Operation) -> bool {
        if let Operation::Pointer(pointer) = &operation {
            if matches!(pointer.phase, pane::PointerPhase::Press) {
                self.abandoned_gesture = false;
            } else if self.abandoned_gesture && continues_gesture(&operation) {
                return false;
            }
        }
        // Only an operation that is not being sent can absorb or be replaced by a newer one.
        let sending = self.sending;
        let unsent_back = self
            .queue
            .back_mut()
            .filter(|queued| Some(queued.id) != sending);
        if let Some(back) = unsent_back {
            if let Operation::Wheel(wheel) = &operation
                && let Operation::Wheel(queued) = &mut back.operation
                && let Some(merged) = merge_wheels(queued, wheel)
            {
                *queued = merged;
                return true;
            }
            if matches!(&operation, Operation::Pointer(p) if matches!(p.phase, pane::PointerPhase::Motion))
                && matches!(&back.operation, Operation::Pointer(p) if matches!(p.phase, pane::PointerPhase::Motion))
            {
                self.queue.pop_back();
            }
        }
        // The newest query and scrollbar target supersede queued targets of the same kind.
        if matches!(
            operation,
            Operation::Query { .. } | Operation::EndFind { .. }
        ) {
            self.queue.retain(|queued| {
                !matches!(
                    queued.operation,
                    Operation::Query { .. }
                        | Operation::Navigate { .. }
                        | Operation::EndFind { .. }
                )
            });
        } else if matches!(operation, Operation::ScrollTo { .. }) {
            self.queue
                .retain(|queued| !matches!(queued.operation, Operation::ScrollTo { .. }));
        }
        let release = matches!(&operation, Operation::Pointer(p) if matches!(p.phase, pane::PointerPhase::Release));
        // Preserve the final edge of an accepted gesture even when local reading is saturated.
        if self.queue.len() < 128 || release && self.queue.len() == 128 {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            self.queue.push_back(Queued { id, operation });
            true
        } else {
            false
        }
    }
    /// The first queued operation, which is being sent until `settle` answers it.
    fn send_front(&mut self) -> Option<(u64, Operation)> {
        let front = self.queue.front()?;
        self.sending = Some(front.id);
        Some((front.id, front.operation.clone()))
    }
    /// Ends sending `id`. An answered operation leaves the queue; one a hide, a newer query, or
    /// a disconnect removed meanwhile leaves no other operation behind in its place.
    fn settle(&mut self, id: u64, answered: bool) {
        self.sending = None;
        if answered && let Some(index) = self.queue.iter().position(|queued| queued.id == id) {
            self.queue.remove(index);
        }
    }
}

fn continues_gesture(operation: &Operation) -> bool {
    matches!(operation, Operation::Pointer(p)
        if matches!(p.phase, pane::PointerPhase::Release)
            || matches!(p.phase, pane::PointerPhase::Motion) && p.button.is_some())
}

/// The server accepts at most this many steps per axis in one wheel operation.
pub(super) const MAX_WHEEL_STEPS: i32 = 100;

/// Sums an unsent continuation of the same native scroll into its predecessor. Gesture and
/// momentum edges, modifier changes and over-limit sums stay separate operations.
fn merge_wheels(queued: &pane::Wheel, next: &pane::Wheel) -> Option<pane::Wheel> {
    use pane::WheelPhase::{GestureChanged, MomentumChanged};
    let same_phase = matches!(
        (queued.phase, next.phase),
        (GestureChanged, GestureChanged) | (MomentumChanged, MomentumChanged)
    );
    // A direction change is an edge: at a viewport limit, +1 then -1 still moves by one.
    let compatible = |a: i32, b: i32| a == 0 || b == 0 || a.signum() == b.signum();
    if !compatible(queued.horizontal_steps, next.horizontal_steps)
        || !compatible(queued.vertical_steps, next.vertical_steps)
    {
        return None;
    }
    let horizontal = queued.horizontal_steps.checked_add(next.horizontal_steps)?;
    let vertical = queued.vertical_steps.checked_add(next.vertical_steps)?;
    // Only scrolling of one displayed screen merges; a later screen may differ in geometry
    // or mode, and its relative movement must not be applied to an earlier view.
    (same_phase
        && queued.screen_generation == next.screen_generation
        && queued.modifiers == next.modifiers
        && queued.shift_selection == next.shift_selection
        && horizontal.unsigned_abs() <= MAX_WHEEL_STEPS.unsigned_abs()
        && vertical.unsigned_abs() <= MAX_WHEEL_STEPS.unsigned_abs())
    .then_some(pane::Wheel {
        horizontal_steps: horizontal,
        vertical_steps: vertical,
        ..*next
    })
}

/// One native event moves at most this many server operations' worth of steps. A larger delta
/// is not a plausible device event; it is bounded rather than queued as a long tail.
const MAX_WHEEL_PARTS: i32 = 16;

/// Divides one native wheel event into operations the server accepts, in order, keeping its
/// start edge on the first part and its end edge on the last.
pub(super) fn split_wheel(mut wheel: pane::Wheel) -> Vec<pane::Wheel> {
    use pane::WheelPhase::*;
    let bound = MAX_WHEEL_STEPS * MAX_WHEEL_PARTS;
    wheel.horizontal_steps = wheel.horizontal_steps.clamp(-bound, bound);
    wheel.vertical_steps = wheel.vertical_steps.clamp(-bound, bound);
    let parts = [wheel.horizontal_steps, wheel.vertical_steps]
        .iter()
        .map(|steps| steps.unsigned_abs().div_ceil(MAX_WHEEL_STEPS as u32))
        .max()
        .unwrap_or(0)
        .max(1);
    let changed = match wheel.phase {
        GestureStarted | GestureChanged | GestureEnded | GestureCancelled => GestureChanged,
        MomentumStarted | MomentumChanged | MomentumEnded | MomentumCancelled => MomentumChanged,
    };
    let mut remaining = (wheel.horizontal_steps, wheel.vertical_steps);
    (0..parts)
        .map(|part| {
            let take = |left: &mut i32| {
                let step = (*left).clamp(-MAX_WHEEL_STEPS, MAX_WHEEL_STEPS);
                *left -= step;
                step
            };
            let first = part == 0;
            let last = part + 1 == parts;
            pane::Wheel {
                horizontal_steps: take(&mut remaining.0),
                vertical_steps: take(&mut remaining.1),
                phase: match wheel.phase {
                    GestureStarted | MomentumStarted if first => wheel.phase,
                    GestureEnded | GestureCancelled | MomentumEnded | MomentumCancelled if last => {
                        wheel.phase
                    }
                    _ if parts == 1 => wheel.phase,
                    _ => changed,
                },
                ..wheel
            }
        })
        .collect()
}

/// When a worker's reading continues. Later variants are more urgent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Next {
    /// Nothing until the next output check.
    Idle,
    /// The server is still applying reading work; recheck before the next output check.
    Recheck,
    /// More ready work follows at once.
    Continue,
    /// Reading completed against a newer screen; check output now.
    Refresh,
}

#[derive(Default)]
pub(super) struct Reader {
    latest: Option<ScreenMapping>,
    awaiting_read: bool,
    /// The awaited operation began or continued a pointer gesture.
    awaiting_gesture: bool,
    target: Option<(ScreenMapping, u64)>,
    bytes: Vec<u8>,
    total: Option<usize>,
}
impl Reader {
    pub(super) fn screen(
        &mut self,
        state: &DevelopmentPaneState,
        mapping: ScreenMapping,
        selection: bool,
    ) {
        let mut reading = state.reading.lock().unwrap_or_else(|e| e.into_inner());
        self.latest = Some(mapping);
        reading.selection_present = selection;
        let current_revision = reading.revision;
        if let Some((revision, saved, _)) = &mut reading.prepared
            && selection
            && *revision == current_revision
            && saved.epoch == mapping.epoch
            && saved.selection == mapping.selection
        {
            *saved = mapping;
            return;
        }
        reading.prepared = None;
        if let Some((saved, revision)) = &mut self.target
            && selection
            && *revision == current_revision
            && saved.epoch == mapping.epoch
            && saved.selection == mapping.selection
        {
            *saved = mapping;
            return;
        }
        self.target = selection.then_some((mapping, reading.revision));
        self.bytes.clear();
        self.total = None;
    }

    /// Advances reading by at most one exchange of each kind and says when to continue.
    pub(super) fn poll(
        &mut self,
        state: &DevelopmentPaneState,
        lease: Lease,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<Next, ServerError> {
        let mut next = Next::Idle;
        if self.awaiting_read {
            match exchange(&Request::ReadingStatus { lease }) {
                Ok(Reply::SelectionPending) => return Ok(Next::Recheck),
                // The worker refused the admitted operation. It is not retried.
                Err(ServerError::StaleControl) => {
                    self.awaiting_read = false;
                    if std::mem::take(&mut self.awaiting_gesture) {
                        state
                            .reading
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .abandon_gesture();
                    }
                    return Ok(Next::Continue);
                }
                Err(error) => return Err(error),
                Ok(reply) => match reply {
                    Reply::ReadingReady {
                        generation,
                        selection_revision,
                        selection_present,
                    } => {
                        let Some(mapping) = self
                            .latest
                            .filter(|mapping| mapping.selection == selection_revision)
                        else {
                            // Its barrier names a screen this worker has not received yet.
                            return Ok(Next::Refresh);
                        };
                        self.awaiting_read = false;
                        self.awaiting_gesture = false;
                        let revision = {
                            let mut reading =
                                state.reading.lock().unwrap_or_else(|e| e.into_inner());
                            reading.selection_present = selection_present;
                            reading.revision
                        };
                        self.target = selection_present.then_some((mapping, revision));
                        self.bytes.clear();
                        self.total = None;
                        // The completed operation's screen follows without awaiting an output check.
                        if generation != mapping.wire {
                            next = Next::Refresh;
                        }
                    }
                    _ => return Err(ServerError::Protocol),
                },
            }
        }
        let operation = state
            .reading
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send_front();
        if let Some((id, operation)) = operation {
            let gesture = matches!(&operation, Operation::Pointer(p) if matches!(p.phase, pane::PointerPhase::Press))
                || continues_gesture(&operation);
            let reply = exchange(&Request::Read { lease, operation });
            // The answer settles only the operation it answers. Another thread may have removed
            // that operation during the exchange; then no other queued operation is removed.
            let answered = matches!(
                reply,
                Ok(Reply::Reading { accepted: true }) | Err(ServerError::StaleControl)
            );
            {
                let mut reading = state.reading.lock().unwrap_or_else(|e| e.into_inner());
                reading.settle(id, answered);
                // A refused gesture's queued continuation goes with it, in the same step.
                if gesture && matches!(reply, Err(ServerError::StaleControl)) {
                    reading.abandon_gesture();
                }
            }
            match reply {
                // The server's single reading slot is busy; this operation stays first.
                Ok(Reply::Reading { accepted: false }) => return Ok(next.max(Next::Recheck)),
                Ok(Reply::Reading { accepted: true }) => {
                    self.target = None;
                    self.bytes.clear();
                    self.total = None;
                    self.awaiting_read = true;
                    self.awaiting_gesture = gesture;
                    return Ok(next.max(Next::Continue));
                }
                Err(ServerError::StaleControl) => {
                    self.target = None;
                    self.bytes.clear();
                    self.total = None;
                    return Ok(next.max(Next::Continue));
                }
                Err(error) => return Err(error),
                _ => return Err(ServerError::Protocol),
            }
        }
        let pending_copy = {
            let mut reading = state.reading.lock().unwrap_or_else(|e| e.into_inner());
            reading
                .pending_copy
                .take()
                .filter(|_| !reading.selection_present)
        };
        if let Some((epoch, generation, modifiers)) = pending_copy {
            let valid = {
                let interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
                state.epoch.load(Ordering::Acquire) == epoch
                    && interaction.interactive
                    && interaction.focus_generation == generation
                    && interaction.target == pane::FocusTarget::Terminal
            };
            if valid {
                match exchange(&Request::Input {
                    lease,
                    generation,
                    input: pane::InteractiveInput::CopyOrForward {
                        modifiers: encode_modifiers(modifiers),
                    },
                }) {
                    Ok(Reply::Unchanged) | Err(ServerError::StaleControl) => {}
                    Err(error) => return Err(error),
                    _ => return Err(ServerError::Protocol),
                }
            }
        }
        let Some((mapping, revision)) = self.target else {
            return Ok(next);
        };
        if state
            .reading
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revision
            != revision
        {
            self.target = None;
            self.bytes.clear();
            return Ok(next);
        }
        let reply = exchange(&Request::SelectionChunk {
            lease,
            generation: mapping.selection,
            offset: self.bytes.len(),
        });
        match reply {
            Ok(Reply::SelectionPending) => return Ok(next.max(Next::Recheck)),
            Err(ServerError::StaleControl) => {
                self.target = None;
                self.bytes.clear();
                return Ok(next);
            }
            Err(error) => return Err(error),
            Ok(Reply::SelectionChunk {
                generation,
                offset,
                total,
                bytes,
            }) => {
                if generation != mapping.selection
                    || offset != self.bytes.len()
                    || total == 0
                    || total > MAX_SELECTION_BYTES
                    || self.total.is_some_and(|old| old != total)
                    || bytes.is_empty()
                    || bytes.len() > SELECTION_CHUNK_BYTES
                    || bytes.len() > total.saturating_sub(offset)
                {
                    return Err(ServerError::Protocol);
                }
                self.total = Some(total);
                self.bytes.extend_from_slice(&bytes);
                if self.bytes.len() == total {
                    let selection: pane::reading::Selection =
                        serde_json::from_slice(&self.bytes).map_err(|_| ServerError::Protocol)?;
                    let mut reading = state.reading.lock().unwrap_or_else(|e| e.into_inner());
                    if reading.revision == revision
                        && state.epoch.load(Ordering::Acquire) == mapping.epoch
                    {
                        reading.prepared = Some((
                            revision,
                            mapping,
                            super::super::SelectionCopy {
                                plain_text: selection.plain_text.into(),
                                html: selection.html.map(Into::into),
                            },
                        ));
                        reading.notify = reading.pointer_copy == Some(revision);
                    }
                    self.target = None;
                    self.bytes.clear();
                    return Ok(next);
                }
            }
            _ => return Err(ServerError::Protocol),
        }
        // A prepared selection continues with its next chunk without awaiting an output check.
        Ok(next.max(Next::Continue))
    }
}

impl PresentationHandle {
    pub(super) fn take_pointer_copy(&self) -> Option<super::super::SelectionCopy> {
        if self.state.status() != DevelopmentPaneStatus::Connected {
            return None;
        }
        let mut reading = self.state.reading.lock().unwrap_or_else(|e| e.into_inner());
        if reading.pointer_copy != Some(reading.revision) {
            return None;
        }
        let (revision, mapping, copy) = reading.prepared.as_ref()?;
        if *revision != reading.revision
            || mapping.epoch != self.state.epoch.load(Ordering::Acquire)
        {
            return None;
        }
        let copy = copy.clone();
        reading.pointer_copy = None;
        Some(copy)
    }
    pub(super) fn request_pointer_copy(&self) {
        let mut reading = self.state.reading.lock().unwrap_or_else(|e| e.into_inner());
        reading.pointer_copy = Some(reading.revision);
    }
    pub(super) fn read(&self, operation: Operation) {
        if self.state.status() != DevelopmentPaneStatus::Connected {
            return;
        }
        self.state
            .reading
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .invalidate();
        self.send(Input::Read(operation));
    }
    pub(super) fn prepared_selection(
        &self,
        generation: Option<PresentationGeneration>,
    ) -> Option<super::super::SelectionCopy> {
        if self.state.status() != DevelopmentPaneStatus::Connected {
            return None;
        }
        let reading = self.state.reading.lock().unwrap_or_else(|e| e.into_inner());
        let (revision, mapping, copy) = reading.prepared.as_ref()?;
        if *revision != reading.revision
            || mapping.epoch != self.state.epoch.load(Ordering::Acquire)
            || generation.is_some_and(|value| value != mapping.local)
        {
            return None;
        }
        Some(copy.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Adapter;
    use super::*;

    fn mapping(adapter: &Adapter) -> ScreenMapping {
        ScreenMapping {
            epoch: adapter.active.epoch,
            local: PresentationGeneration::test(4),
            wire: 17,
            selection: 17,
            size: *adapter.handle.size.lock().unwrap(),
            mouse_tracking: false,
            alternate: false,
            alternate_scroll: false,
        }
    }
    fn selection_bytes() -> Vec<u8> {
        serde_json::to_vec(&pane::reading::Selection {
            plain_text: "older retained row\n".repeat(5000),
            html: Some("<pre>complete</pre>".into()),
        })
        .unwrap()
    }
    fn deliver(reader: &mut Reader, adapter: &Adapter, bytes: &[u8]) {
        reader
            .poll(
                &adapter.handle.state,
                adapter.active.lease,
                &mut |request| {
                    let Request::SelectionChunk {
                        generation, offset, ..
                    } = request
                    else {
                        panic!("expected selection transfer")
                    };
                    let end = (*offset + SELECTION_CHUNK_BYTES).min(bytes.len());
                    Ok(Reply::SelectionChunk {
                        generation: *generation,
                        offset: *offset,
                        total: bytes.len(),
                        bytes: bytes[*offset..end].to_vec(),
                    })
                },
            )
            .unwrap();
    }
    #[test]
    fn unrelated_output_does_not_restart_a_complete_selection_transfer() {
        let adapter = Adapter::new();
        let mut current = mapping(&adapter);
        let bytes = selection_bytes();
        let mut reader = Reader::default();
        reader.screen(&adapter.handle.state, current, true);
        for _ in 0..bytes.len().div_ceil(SELECTION_CHUNK_BYTES) + 2 {
            if reader.target.is_some() {
                deliver(&mut reader, &adapter, &bytes);
            }
            current.wire += 1;
            current.local = PresentationGeneration::test(current.wire);
            reader.screen(&adapter.handle.state, current, true);
        }
        assert_eq!(
            adapter
                .handle
                .copy_selection_at(current.local)
                .unwrap()
                .unwrap()
                .plain_text
                .as_ref(),
            "older retained row\n".repeat(5000)
        );
    }

    #[test]
    fn native_copy_publishes_only_a_complete_current_selection_and_shares_prepared_text() {
        let adapter = Adapter::new();
        let mapping = mapping(&adapter);
        let bytes = selection_bytes();
        let mut reader = Reader::default();
        reader.screen(&adapter.handle.state, mapping, true);
        adapter.handle.request_pointer_copy();
        deliver(&mut reader, &adapter, &bytes);
        assert!(adapter.handle.copy_selection().unwrap().is_none());
        assert!(adapter.handle.take_prepared_pointer_copy().is_none());
        while reader.target.is_some() {
            deliver(&mut reader, &adapter, &bytes);
        }
        let copy = adapter
            .handle
            .copy_selection_at(mapping.local)
            .unwrap()
            .unwrap();
        assert_eq!(
            copy.plain_text.as_ref(),
            "older retained row\n".repeat(5000)
        );
        let another = adapter.handle.copy_selection().unwrap().unwrap();
        assert!(Arc::ptr_eq(&copy.plain_text, &another.plain_text));
        assert!(
            adapter
                .handle
                .copy_selection_at(PresentationGeneration::test(3))
                .unwrap()
                .is_none()
        );
        assert!(adapter.handle.take_prepared_pointer_copy().is_some());
        assert!(adapter.handle.take_prepared_pointer_copy().is_none());
        adapter.handle.scroll_scrollback(ScrollbackMovement::Top);
        assert!(adapter.handle.copy_selection().unwrap().is_none());
        adapter.handle.state.invalidate();
        assert!(adapter.handle.take_prepared_pointer_copy().is_none());
    }
    #[test]
    fn a_partial_transfer_cannot_publish_after_disconnect_or_a_new_reading_request() {
        for disconnect in [false, true] {
            let adapter = Adapter::new();
            let bytes = selection_bytes();
            let mut reader = Reader::default();
            reader.screen(&adapter.handle.state, mapping(&adapter), true);
            deliver(&mut reader, &adapter, &bytes);
            if disconnect {
                adapter.handle.state.invalidate();
            } else {
                adapter
                    .handle
                    .end_find(super::super::super::FindQueryGeneration::test(8));
            }
            reader
                .poll(&adapter.handle.state, adapter.active.lease, &mut |_| {
                    panic!("stale transfer must be discarded before transport")
                })
                .unwrap();
            assert!(adapter.handle.copy_selection().unwrap().is_none());
        }
    }
    #[test]
    fn malformed_selection_chunks_never_become_native_copy() {
        let adapter = Adapter::new();
        let mut reader = Reader::default();
        reader.screen(&adapter.handle.state, mapping(&adapter), true);
        let result = reader.poll(&adapter.handle.state, adapter.active.lease, &mut |_| {
            Ok(Reply::SelectionChunk {
                generation: 17,
                offset: 0,
                total: 1,
                bytes: vec![0; SELECTION_CHUNK_BYTES + 1],
            })
        });
        assert_eq!(result, Err(ServerError::Protocol));
        assert!(adapter.handle.copy_selection().unwrap().is_none());
    }
    #[test]
    fn one_clients_reading_never_changes_another_clients_prepared_selection() {
        let first = Adapter::new();
        let second = Adapter::new();
        let bytes = selection_bytes();
        let mut reader = Reader::default();
        reader.screen(&second.handle.state, mapping(&second), true);
        while reader.target.is_some() {
            deliver(&mut reader, &second, &bytes);
        }
        let original = second.handle.copy_selection().unwrap().unwrap();
        first.handle.scroll_scrollback(ScrollbackMovement::Top);
        first.handle.set_find_query(
            super::super::super::FindQueryGeneration::test(9),
            "other".into(),
        );
        let unchanged = second.handle.copy_selection().unwrap().unwrap();
        assert!(Arc::ptr_eq(&original.plain_text, &unchanged.plain_text));
    }
    #[test]
    fn typing_a_new_query_coalesces_queued_search_work() {
        let adapter = Adapter::new();
        let state = &adapter.handle.state;
        for generation in 1..=1000 {
            state.reading.lock().unwrap().enqueue(Operation::Query {
                generation,
                text: format!("query-{generation}"),
            });
        }
        let mut reader = Reader::default();
        let mut queries = vec![];
        reader
            .poll(state, adapter.active.lease, &mut |request| {
                let Request::Read {
                    operation: Operation::Query { generation, text },
                    ..
                } = request
                else {
                    panic!("expected query")
                };
                queries.push((*generation, text.clone()));
                Ok(Reply::Reading { accepted: true })
            })
            .unwrap();
        assert_eq!(queries, vec![(1000, "query-1000".into())]);
    }

    #[test]
    fn a_quiet_release_resumes_preparing_the_last_authoritative_selection() {
        let adapter = Adapter::new();
        let mapping = mapping(&adapter);
        let bytes = selection_bytes();
        let mut reader = Reader::default();
        reader.screen(&adapter.handle.state, mapping, true);
        adapter
            .handle
            .state
            .reading
            .lock()
            .unwrap()
            .enqueue(Operation::Pointer(pane::Pointer {
                screen_generation: mapping.wire,
                phase: pane::PointerPhase::Release,
                button: Some(pane::PointerButton::Left),
                x: 1.0,
                y: 1.0,
                modifiers: 0,
                shift_selection: false,
            }));
        adapter.handle.request_pointer_copy();
        for _ in 0..16 {
            reader
                .poll(
                    &adapter.handle.state,
                    adapter.active.lease,
                    &mut |request| match request {
                        Request::Read { .. } => Ok(Reply::Reading { accepted: true }),
                        Request::ReadingStatus { .. } => Ok(Reply::ReadingReady {
                            generation: mapping.wire,
                            selection_revision: mapping.selection,
                            selection_present: true,
                        }),
                        Request::SelectionChunk {
                            generation, offset, ..
                        } => Ok(Reply::SelectionChunk {
                            generation: *generation,
                            offset: *offset,
                            total: bytes.len(),
                            bytes: bytes
                                [*offset..(*offset + SELECTION_CHUNK_BYTES).min(bytes.len())]
                                .to_vec(),
                        }),
                        _ => Ok(Reply::Unchanged),
                    },
                )
                .unwrap();
        }
        assert!(
            adapter.handle.take_prepared_pointer_copy().is_some(),
            "a release with no new screen must still complete Copy"
        );
    }

    fn pointer(phase: pane::PointerPhase, x: f32) -> Operation {
        Operation::Pointer(pane::Pointer {
            screen_generation: 17,
            phase,
            button: Some(pane::PointerButton::Left),
            x,
            y: 1.0,
            modifiers: 0,
            shift_selection: false,
        })
    }
    fn phases(delivered: &[Operation]) -> Vec<(&'static str, f32)> {
        delivered
            .iter()
            .map(|operation| match operation {
                Operation::Pointer(p) => (
                    match p.phase {
                        pane::PointerPhase::Press => "press",
                        pane::PointerPhase::Motion => "motion",
                        pane::PointerPhase::Release => "release",
                    },
                    p.x,
                ),
                _ => panic!("expected pointer"),
            })
            .collect()
    }

    #[test]
    fn a_refused_press_never_turns_its_later_motion_into_a_new_gesture() {
        use pane::PointerPhase::{Motion, Press, Release};
        // Refused at admission, then refused by the terminal worker after admission.
        for refused_after_admission in [false, true] {
            let adapter = Adapter::new();
            let state = &adapter.handle.state;
            for operation in [pointer(Press, 1.0), pointer(Motion, 2.0)] {
                state.reading.lock().unwrap().enqueue(operation);
            }
            let mut reader = Reader::default();
            reader.screen(state, mapping(&adapter), false);
            let mut delivered = vec![];
            let poll = |reader: &mut Reader, delivered: &mut Vec<Operation>| {
                reader
                    .poll(state, adapter.active.lease, &mut |request| match request {
                        Request::Read { operation, .. } => {
                            delivered.push(operation.clone());
                            let press = matches!(operation, Operation::Pointer(p) if matches!(p.phase, Press));
                            if press && !refused_after_admission && delivered.len() == 1 {
                                Err(ServerError::StaleControl)
                            } else {
                                Ok(Reply::Reading { accepted: true })
                            }
                        }
                        Request::ReadingStatus { .. } if refused_after_admission && delivered.len() == 1 => {
                            Err(ServerError::StaleControl)
                        }
                        Request::ReadingStatus { .. } => Ok(Reply::ReadingReady {
                            generation: 17,
                            selection_revision: 17,
                            selection_present: false,
                        }),
                        _ => panic!("unexpected reading request"),
                    })
                    .unwrap()
            };
            for _ in 0..4 {
                poll(&mut reader, &mut delivered);
            }
            // Motion and Release arriving after the refusal belong to the abandoned gesture.
            for operation in [
                pointer(Motion, 3.0),
                pointer(Release, 3.0),
                pointer(Press, 9.0),
            ] {
                state.reading.lock().unwrap().enqueue(operation);
            }
            for _ in 0..4 {
                poll(&mut reader, &mut delivered);
            }
            assert_eq!(
                phases(&delivered),
                vec![("press", 1.0), ("press", 9.0)],
                "refused after admission: {refused_after_admission}"
            );
            assert_eq!(state.epoch.load(Ordering::Acquire), adapter.active.epoch);
        }
    }

    #[test]
    fn a_late_refusal_keeps_the_continuation_of_a_gesture_queued_after_it() {
        use pane::PointerPhase::{Motion, Press, Release};
        let adapter = Adapter::new();
        let state = &adapter.handle.state;
        let mut reader = Reader::default();
        reader.screen(state, mapping(&adapter), false);
        state.reading.lock().unwrap().enqueue(pointer(Press, 1.0));
        reader
            .poll(state, adapter.active.lease, &mut |_| {
                Ok(Reply::Reading { accepted: true })
            })
            .unwrap();
        // The next gesture starts while the worker still applies the first Press.
        for operation in [pointer(Release, 1.0), pointer(Press, 9.0)] {
            state.reading.lock().unwrap().enqueue(operation);
        }
        reader
            .poll(state, adapter.active.lease, &mut |_| {
                Err(ServerError::StaleControl)
            })
            .unwrap();
        for operation in [pointer(Motion, 10.0), pointer(Release, 10.0)] {
            state.reading.lock().unwrap().enqueue(operation);
        }
        let queued: Vec<_> = state
            .reading
            .lock()
            .unwrap()
            .queue
            .iter()
            .map(|queued| queued.operation.clone())
            .collect();
        assert_eq!(
            phases(&queued),
            vec![("press", 9.0), ("motion", 10.0), ("release", 10.0)],
            "only the refused gesture's continuation is dropped"
        );
    }

    fn wheel(phase: pane::WheelPhase, vertical_steps: i32) -> pane::Wheel {
        pane::Wheel {
            screen_generation: 17,
            horizontal_steps: 0,
            vertical_steps,
            phase,
            x: 1.0,
            y: 1.0,
            modifiers: 0,
            shift_selection: false,
        }
    }

    #[test]
    fn queued_native_scrolling_coalesces_within_server_limits_and_keeps_its_edges() {
        use pane::WheelPhase::*;
        let adapter = Adapter::new();
        let state = &adapter.handle.state;
        let events = [(GestureStarted, 3)]
            .into_iter()
            .chain(std::iter::repeat_n((GestureChanged, 30), 5))
            .chain([
                (GestureEnded, 0),
                (MomentumChanged, 4),
                (MomentumChanged, 4),
                // Reversing at a limit must still move back, so it is never summed away.
                (MomentumChanged, -8),
            ]);
        for (phase, steps) in events {
            state
                .reading
                .lock()
                .unwrap()
                .enqueue(Operation::Wheel(wheel(phase, steps)));
        }
        let queued: Vec<_> = state
            .reading
            .lock()
            .unwrap()
            .queue
            .iter()
            .map(|queued| match &queued.operation {
                Operation::Wheel(w) => (format!("{:?}", w.phase), w.vertical_steps),
                _ => panic!("expected wheel"),
            })
            .collect();
        let expected: Vec<_> = [
            ("GestureStarted", 3),
            ("GestureChanged", 90),
            ("GestureChanged", 60),
            ("GestureEnded", 0),
            ("MomentumChanged", 8),
            ("MomentumChanged", -8),
        ]
        .into_iter()
        .map(|(phase, steps)| (phase.to_owned(), steps))
        .collect();
        assert_eq!(queued, expected);
    }

    #[test]
    fn a_large_native_delta_is_divided_into_accepted_operations_without_losing_steps() {
        use pane::WheelPhase::*;
        let parts = |phase, steps| {
            split_wheel(wheel(phase, steps))
                .into_iter()
                .map(|w| (format!("{:?}", w.phase), w.vertical_steps))
                .collect::<Vec<_>>()
        };
        let owned = |parts: &[(&str, i32)]| {
            parts
                .iter()
                .map(|(phase, steps)| ((*phase).to_owned(), *steps))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parts(GestureStarted, -250),
            owned(&[
                ("GestureStarted", -100),
                ("GestureChanged", -100),
                ("GestureChanged", -50)
            ])
        );
        assert_eq!(
            parts(MomentumEnded, 101),
            owned(&[("MomentumChanged", 100), ("MomentumEnded", 1)])
        );
        // An implausible device delta is bounded, keeps its edge and leaves no long tail.
        for (phase, steps) in [(GestureEnded, i32::MIN), (MomentumCancelled, i32::MAX)] {
            let parts = split_wheel(wheel(phase, steps));
            assert_eq!(parts.len(), MAX_WHEEL_PARTS as usize);
            assert_eq!(
                parts
                    .iter()
                    .map(|w| i64::from(w.vertical_steps))
                    .sum::<i64>(),
                i64::from(MAX_WHEEL_STEPS * MAX_WHEEL_PARTS) * i64::from(steps.signum())
            );
            assert_eq!(
                format!("{:?}", parts.last().unwrap().phase),
                format!("{phase:?}")
            );
        }
        assert_eq!(
            parts(GestureChanged, 100),
            owned(&[("GestureChanged", 100)])
        );
        assert_eq!(parts(GestureEnded, 0), owned(&[("GestureEnded", 0)]));
    }

    #[test]
    fn a_prepared_selection_transfers_every_chunk_without_waiting_for_an_output_check() {
        let adapter = Adapter::new();
        let bytes = serde_json::to_vec(&pane::reading::Selection {
            plain_text: "x".repeat(800 * 1024),
            html: None,
        })
        .unwrap();
        let chunks = bytes.len().div_ceil(SELECTION_CHUNK_BYTES);
        let mut reader = Reader::default();
        reader.screen(&adapter.handle.state, mapping(&adapter), true);
        let mut polls = 0;
        loop {
            polls += 1;
            let more = reader
                .poll(
                    &adapter.handle.state,
                    adapter.active.lease,
                    &mut |request| {
                        let Request::SelectionChunk {
                            generation, offset, ..
                        } = request
                        else {
                            panic!("expected selection transfer")
                        };
                        let end = (*offset + SELECTION_CHUNK_BYTES).min(bytes.len());
                        Ok(Reply::SelectionChunk {
                            generation: *generation,
                            offset: *offset,
                            total: bytes.len(),
                            bytes: bytes[*offset..end].to_vec(),
                        })
                    },
                )
                .unwrap();
            if more != Next::Continue {
                break;
            }
        }
        assert_eq!(polls, chunks, "each ready chunk continues at once");
        assert!(adapter.handle.copy_selection().unwrap().is_some());
    }

    #[test]
    fn queued_scrolling_settles_without_awaiting_output_checks() {
        use pane::WheelPhase::*;
        let adapter = Adapter::new();
        let state = &adapter.handle.state;
        let mut current = mapping(&adapter);
        let mut reader = Reader::default();
        reader.screen(state, current, false);
        // Each event names a different displayed screen, so none merge.
        for generation in 0..128 {
            state
                .reading
                .lock()
                .unwrap()
                .enqueue(Operation::Wheel(pane::Wheel {
                    screen_generation: 17 + generation,
                    ..wheel(GestureChanged, -1)
                }));
        }
        let (mut barrier_checks, mut applied) = (0, 0);
        let mut turns = vec![];
        while !state.reading.lock().unwrap().queue.is_empty() || reader.awaiting_read {
            let next = reader
                .poll(state, adapter.active.lease, &mut |request| match request {
                    Request::Read { .. } => Ok(Reply::Reading { accepted: true }),
                    // The terminal worker presents the scrolled view at its second check.
                    Request::ReadingStatus { .. } => {
                        barrier_checks += 1;
                        if barrier_checks % 2 == 1 {
                            Ok(Reply::SelectionPending)
                        } else {
                            applied += 1;
                            Ok(Reply::ReadingReady {
                                generation: current.wire + 1,
                                selection_revision: current.selection,
                                selection_present: false,
                            })
                        }
                    }
                    _ => panic!("unexpected reading request"),
                })
                .unwrap();
            if next == Next::Refresh {
                current.wire += 1;
                current.local = PresentationGeneration::test(current.wire);
                reader.screen(state, current, false);
            }
            turns.push(next);
            assert!(turns.len() < 1000, "reading must settle");
        }
        assert_eq!(applied, 128);
        assert!(
            !turns.contains(&Next::Idle),
            "pending reading never waits for the output check"
        );
        assert_eq!(
            turns.iter().filter(|next| **next == Next::Recheck).count(),
            128,
            "each barrier wait is one bounded recheck"
        );
        assert_eq!(
            turns.iter().filter(|next| **next == Next::Refresh).count(),
            128,
            "each applied operation's screen is fetched at once"
        );
    }

    /// Polls `reader` until its queue is empty and nothing is awaited, answering each Read with
    /// `answer` and each status with a ready screen.
    fn drain(
        reader: &mut Reader,
        adapter: &Adapter,
        mut answer: impl FnMut(&Operation) -> Result<Reply, ServerError>,
    ) -> Vec<Operation> {
        let state = &adapter.handle.state;
        let mut delivered = vec![];
        for _ in 0..16 {
            if state.reading.lock().unwrap().queue.is_empty() && !reader.awaiting_read {
                return delivered;
            }
            reader
                .poll(state, adapter.active.lease, &mut |request| match request {
                    Request::Read { operation, .. } => {
                        delivered.push(operation.clone());
                        answer(operation)
                    }
                    Request::ReadingStatus { .. } => Ok(Reply::ReadingReady {
                        generation: 17,
                        selection_revision: 17,
                        selection_present: false,
                    }),
                    _ => panic!("unexpected reading request"),
                })
                .unwrap();
        }
        panic!("reading must settle");
    }

    #[test]
    fn hiding_a_pane_while_its_release_is_sent_keeps_the_scroll_queued_after_it() {
        use crate::terminal::TerminalSessionHandle;
        use pane::PointerPhase::{Press, Release};
        // The server accepts the Release, or refuses it after the hide.
        for refused in [false, true] {
            let adapter = Adapter::new();
            let state = &adapter.handle.state;
            let mut reader = Reader::default();
            reader.screen(state, mapping(&adapter), false);
            for operation in [
                pointer(Press, 1.0),
                pointer(Release, 1.0),
                Operation::Scroll(pane::reading::Scroll::PageUp),
            ] {
                state.reading.lock().unwrap().enqueue(operation);
            }
            let delivered = drain(&mut reader, &adapter, |operation| {
                if matches!(operation, Operation::Pointer(p) if matches!(p.phase, Release)) {
                    // The person switches Tabs while the worker sends the Release.
                    adapter.handle.set_presentable(false);
                    if refused {
                        return Err(ServerError::StaleControl);
                    }
                }
                Ok(Reply::Reading { accepted: true })
            });
            assert!(
                matches!(
                    delivered.last(),
                    Some(Operation::Scroll(pane::reading::Scroll::PageUp))
                ),
                "refused: {refused}; delivered: {delivered:?}"
            );
        }
    }

    #[test]
    fn work_queued_while_an_operation_is_sent_never_changes_or_replaces_it() {
        use pane::PointerPhase::Motion;
        use pane::WheelPhase::GestureChanged;
        let query = |generation, text: &str| Operation::Query {
            generation,
            text: text.to_owned(),
        };
        // Compatible scrolling merges, a newer Motion replaces, and a newer query supersedes.
        for (sent, later) in [
            (
                Operation::Wheel(wheel(GestureChanged, 3)),
                Operation::Wheel(wheel(GestureChanged, 4)),
            ),
            (pointer(Motion, 2.0), pointer(Motion, 3.0)),
            (query(1, "a"), query(2, "ab")),
        ] {
            let adapter = Adapter::new();
            let state = &adapter.handle.state;
            let mut reader = Reader::default();
            reader.screen(state, mapping(&adapter), false);
            state.reading.lock().unwrap().enqueue(sent.clone());
            let mut arriving = Some(later.clone());
            let delivered = drain(&mut reader, &adapter, |_| {
                if let Some(operation) = arriving.take() {
                    state.reading.lock().unwrap().enqueue(operation);
                }
                Ok(Reply::Reading { accepted: true })
            });
            assert_eq!(
                format!("{delivered:?}"),
                format!("{:?}", [sent, later]),
                "each operation is sent once, as queued"
            );
        }
    }
}
