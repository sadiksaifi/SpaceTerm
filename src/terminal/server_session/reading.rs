//! Keeps transport work off native Copy and Services callbacks.
use super::*;
use pane::reading::{MAX_SELECTION_BYTES, Operation, SELECTION_CHUNK_BYTES};
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct ReadingState {
    revision: u64,
    queue: VecDeque<Operation>,
    prepared: Option<(u64, ScreenMapping, super::super::SelectionCopy)>,
    pointer_copy: Option<u64>,
    notify: bool,
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
    }
    pub(super) fn enqueue(&mut self, operation: Operation) -> bool {
        if matches!(&operation, Operation::Pointer(p) if matches!(p.phase, pane::PointerPhase::Motion))
            && matches!(self.queue.back(), Some(Operation::Pointer(p)) if matches!(p.phase, pane::PointerPhase::Motion))
        {
            self.queue.pop_back();
        }
        // The newest query and scrollbar target supersede queued targets of the same kind.
        if matches!(
            operation,
            Operation::Query { .. } | Operation::EndFind { .. }
        ) {
            self.queue.retain(|op| {
                !matches!(
                    op,
                    Operation::Query { .. }
                        | Operation::Navigate { .. }
                        | Operation::EndFind { .. }
                )
            });
        } else if matches!(operation, Operation::ScrollTo { .. }) {
            self.queue
                .retain(|op| !matches!(op, Operation::ScrollTo { .. }));
        }
        let release = matches!(&operation, Operation::Pointer(p) if matches!(p.phase, pane::PointerPhase::Release));
        // Preserve the final edge of an accepted gesture even when local reading is saturated.
        if self.queue.len() < 128 || release && self.queue.len() == 128 {
            self.queue.push_back(operation);
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
pub(super) struct Reader {
    latest: Option<ScreenMapping>,
    awaiting_read: bool,
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
        reading.prepared = None;
        self.target = selection.then_some((mapping, reading.revision));
        self.bytes.clear();
        self.total = None;
    }
    pub(super) fn poll(
        &mut self,
        state: &DevelopmentPaneState,
        lease: Lease,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<(), ServerError> {
        if self.awaiting_read {
            match exchange(&Request::ReadingStatus { lease })? {
                Reply::SelectionPending => return Ok(()),
                Reply::ReadingReady {
                    generation,
                    selection_present,
                } => {
                    let Some(mapping) = self.latest.filter(|mapping| mapping.wire == generation)
                    else {
                        return Ok(());
                    };
                    self.awaiting_read = false;
                    let revision = state
                        .reading
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .revision;
                    self.target = selection_present.then_some((mapping, revision));
                    self.bytes.clear();
                    self.total = None;
                }
                _ => return Err(ServerError::Protocol),
            }
        }
        let operation = state
            .reading
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .queue
            .front()
            .cloned();
        if let Some(operation) = operation {
            match exchange(&Request::Read { lease, operation }) {
                Ok(Reply::Reading { accepted: false }) => return Ok(()),
                Ok(Reply::Reading { accepted: true }) => {
                    state
                        .reading
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .queue
                        .pop_front();
                    self.target = None;
                    self.bytes.clear();
                    self.total = None;
                    self.awaiting_read = true;
                    return Ok(());
                }
                Err(ServerError::StaleControl) => {
                    state
                        .reading
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .queue
                        .pop_front();
                    self.target = None;
                    self.bytes.clear();
                    self.total = None;
                    return Ok(());
                }
                Err(error) => return Err(error),
                _ => return Err(ServerError::Protocol),
            }
        }
        let Some((mapping, revision)) = self.target else {
            return Ok(());
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
            return Ok(());
        }
        let reply = exchange(&Request::SelectionChunk {
            lease,
            generation: mapping.wire,
            offset: self.bytes.len(),
        });
        match reply {
            Ok(Reply::SelectionPending) => {}
            Err(ServerError::StaleControl) => {
                self.target = None;
                self.bytes.clear();
            }
            Err(error) => return Err(error),
            Ok(Reply::SelectionChunk {
                generation,
                offset,
                total,
                bytes,
            }) => {
                if generation != mapping.wire
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
                }
            }
            _ => return Err(ServerError::Protocol),
        }
        Ok(())
    }
}

impl PresentationHandle {
    pub(super) fn take_pointer_copy(&self) -> Option<super::super::SelectionCopy> {
        if self.state.status() != DevelopmentPaneStatus::Connected {
            return None;
        }
        let mut reading = self.state.reading.lock().unwrap_or_else(|e| e.into_inner());
        if reading.pointer_copy.take() != Some(reading.revision) {
            return None;
        }
        let (revision, mapping, copy) = reading.prepared.as_ref()?;
        if *revision != reading.revision
            || mapping.epoch != self.state.epoch.load(Ordering::Acquire)
        {
            return None;
        }
        Some(copy.clone())
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
    fn native_copy_publishes_only_a_complete_current_selection_and_shares_prepared_text() {
        let adapter = Adapter::new();
        let mapping = mapping(&adapter);
        let bytes = selection_bytes();
        let mut reader = Reader::default();
        reader.screen(&adapter.handle.state, mapping, true);
        adapter.handle.request_pointer_copy();
        deliver(&mut reader, &adapter, &bytes);
        assert!(adapter.handle.copy_selection().unwrap().is_none());
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
}
