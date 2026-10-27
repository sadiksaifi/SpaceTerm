use super::*;
use pane::reading::{Operation, SELECTION_CHUNK_BYTES, Scroll, Status};
use std::sync::mpsc::{Receiver, TryRecvError};

type Prepared = Receiver<Result<Option<Arc<[u8]>>, crate::terminal::SelectionCopyError>>;

#[derive(Default)]
pub(super) struct Reading {
    gesture: Option<(u64, PaneSize, PresentationGeneration)>,
    resetting: bool,
    pending: Option<Receiver<()>>,
    applying: Option<Receiver<bool>>,
    refused: bool,
    baselines: std::collections::VecDeque<(u64, PresentationGeneration)>,
    preparing: Option<(u64, Prepared)>,
    prepared: Option<(u64, Arc<[u8]>)>,
    attempted: Option<u64>,
}

impl Reading {
    pub(super) fn reset(&mut self, runtime: &TerminalRuntime) {
        self.gesture = None;
        self.baselines.clear();
        self.applying = None;
        self.refused = false;
        self.invalidate();
        runtime.reset_reading();
        self.pending = Some(runtime.reading_barrier());
        self.resetting = true;
    }
    pub(super) fn reset_pending(&mut self) -> bool {
        if self.resetting && self.busy() {
            return true;
        }
        self.resetting = false;
        false
    }
    fn busy(&mut self) -> bool {
        if self
            .pending
            .as_ref()
            .is_some_and(|rx| matches!(rx.try_recv(), Err(TryRecvError::Empty)))
        {
            return true;
        }
        self.pending = None;
        if let Some(completion) = &self.applying {
            match completion.try_recv() {
                Err(TryRecvError::Empty) => return true,
                Ok(true) => {}
                Ok(false) | Err(TryRecvError::Disconnected) => {
                    self.refused = true;
                    self.gesture = None;
                }
            }
        }
        self.applying = None;
        false
    }
    pub(super) fn observe(&mut self, wire: u64, screen: &Arc<ScreenSnapshot>) {
        if self
            .baselines
            .back()
            .is_some_and(|(saved, _)| *saved == wire)
        {
            return;
        }
        self.baselines.push_back((wire, screen.generation));
        while self.baselines.len() > 32 {
            self.baselines.pop_front();
        }
    }
    fn take_refused(&mut self) -> Result<(), Refusal> {
        if std::mem::take(&mut self.refused) {
            Err(Refusal::Stale)
        } else {
            Ok(())
        }
    }
    pub(super) fn invalidate(&mut self) {
        self.preparing = None;
        self.prepared = None;
        self.attempted = None;
    }
    pub(super) fn fence(&mut self, runtime: &TerminalRuntime) {
        self.pending = Some(runtime.reading_barrier());
    }
    pub(super) fn update(&mut self, runtime: &TerminalRuntime, screen: &ScreenSnapshot) {
        let generation = screen.selection_revision;
        if self
            .prepared
            .as_ref()
            .is_some_and(|(saved, _)| *saved != generation)
        {
            self.prepared = None;
        }
        if let Some((saved, receiver)) = &self.preparing {
            match receiver.try_recv() {
                Ok(Ok(Some(bytes))) if *saved == generation => {
                    self.prepared = Some((generation, bytes))
                }
                Ok(Ok(None)) => self.attempted = None,
                Err(TryRecvError::Empty) => return,
                _ => {}
            }
            self.preparing = None;
        }
        if screen.selection_present && self.attempted != Some(generation) && !self.busy() {
            self.attempted = Some(generation);
            self.preparing = Some((generation, runtime.prepare_server_selection(generation)));
        }
    }
    pub(super) fn chunk(&self, generation: u64, offset: usize) -> Result<Reply, Refusal> {
        let Some((saved, bytes)) = &self.prepared else {
            return Ok(Reply::SelectionPending);
        };
        if *saved != generation {
            return Err(Refusal::Stale);
        }
        if offset >= bytes.len() {
            return Err(Refusal::Unsupported);
        }
        let end = offset
            .saturating_add(SELECTION_CHUNK_BYTES)
            .min(bytes.len());
        Ok(Reply::SelectionChunk {
            generation,
            offset,
            total: bytes.len(),
            bytes: bytes[offset..end].to_vec(),
        })
    }
}

impl OwnedPane {
    fn reading_generation(&self, wire: u64) -> Result<PresentationGeneration, Refusal> {
        if wire == 0 {
            return Err(Refusal::Stale);
        }
        self.reading
            .baselines
            .iter()
            .find(|(saved, _)| *saved == wire)
            .map(|(_, generation)| *generation)
            .or_else(|| {
                self.reading
                    .gesture
                    .filter(|(saved, _, _)| *saved == wire)
                    .map(|(_, _, generation)| generation)
            })
            .or_else(|| {
                (wire == self.screen_generation)
                    .then(|| self.screen.as_ref().map(|screen| screen.generation))
                    .flatten()
            })
            .ok_or(Refusal::Stale)
    }

    pub(super) fn reading_status(&mut self) -> Status {
        if self.reading.busy() {
            return Status::Pending;
        }
        if self.reading.take_refused().is_err() {
            return Status::Refused;
        }
        // A worker completion follows its presentation barrier. Drain again after observing
        // completion so this fence includes a newly published screen as well as a quiet release.
        self.drain();
        let Some(screen) = &self.screen else {
            return Status::Pending;
        };
        Status::Ready {
            generation: self.screen_generation,
            selection_revision: screen.selection_revision,
            selection_present: screen.selection_present,
        }
    }
    pub(super) fn read(&mut self, operation: Operation) -> Result<Reply, Refusal> {
        if self.ended || self.failed {
            return Err(Refusal::Missing);
        }
        if self.reading.busy() {
            return Ok(Reply::Reading { accepted: false });
        }
        self.reading.take_refused()?;
        match operation {
            Operation::Scroll(movement) => self.runtime.scroll_scrollback(match movement {
                Scroll::PageUp => ScrollbackMovement::PageUp,
                Scroll::PageDown => ScrollbackMovement::PageDown,
                Scroll::Top => ScrollbackMovement::Top,
                Scroll::Bottom => ScrollbackMovement::Bottom,
            }),
            Operation::ScrollTo { offset, screen } => {
                let generation = self
                    .screen
                    .as_ref()
                    .filter(|_| screen == self.screen_generation)
                    .ok_or(Refusal::Stale)?
                    .generation;
                self.runtime.scroll_to(offset, generation);
            }
            Operation::Query { generation, text } => {
                if text.len() > pane::reading::MAX_QUERY_BYTES {
                    return Err(Refusal::Unsupported);
                }
                self.runtime
                    .set_find_query(FindQueryGeneration::from_value(generation), text);
            }
            Operation::Navigate {
                generation,
                backwards,
            } => self.runtime.navigate_find(
                FindQueryGeneration::from_value(generation),
                if backwards {
                    FindDirection::Previous
                } else {
                    FindDirection::Next
                },
            ),
            Operation::EndFind { generation } => self
                .runtime
                .end_find(FindQueryGeneration::from_value(generation)),
            Operation::Pointer(input) => {
                let continuation = !matches!(input.phase, pane::PointerPhase::Press)
                    && self.reading.gesture.is_some();
                if !continuation
                    && self.screen.as_ref().is_some_and(|s| s.mouse_tracking)
                    && !input.shift_selection
                {
                    return Err(Refusal::Stale);
                }
                if !matches!(input.phase, pane::PointerPhase::Motion) && input.button.is_none() {
                    return Err(Refusal::Unsupported);
                }
                let width = f32::from(self.size.columns) * f32::from(self.size.cell_width);
                let height = f32::from(self.size.rows) * f32::from(self.size.cell_height);
                if !input.x.is_finite()
                    || !input.y.is_finite()
                    || input.x < -width
                    || input.x > 2.0 * width
                    || input.y < -height
                    || input.y > 2.0 * height
                {
                    return Err(Refusal::Unsupported);
                }
                if continuation {
                    let (start, size, _) = self.reading.gesture.ok_or(Refusal::Stale)?;
                    if input.screen_generation < start
                        || input.screen_generation > self.screen_generation
                        || size != self.size
                    {
                        return Err(Refusal::Stale);
                    }
                }
                let wire = input.screen_generation;
                self.pointer_generation(
                    self.screen_generation,
                    input.x.clamp(0.0, width - 1.0),
                    input.y.clamp(0.0, height - 1.0),
                    input.modifiers,
                )?;
                let generation = self.reading_generation(wire)?;
                if matches!(input.phase, pane::PointerPhase::Press) {
                    self.reading.gesture = Some((wire, self.size, generation));
                }
                if matches!(input.phase, pane::PointerPhase::Release) {
                    self.reading.gesture = None;
                }
                self.reading.applying = Some(self.runtime.reading_pointer(PointerInput {
                    generation,
                    phase: match input.phase {
                        pane::PointerPhase::Press => crate::runtime::PointerPhase::Press,
                        pane::PointerPhase::Motion => crate::runtime::PointerPhase::Motion,
                        pane::PointerPhase::Release => crate::runtime::PointerPhase::Release,
                    },
                    button: input.button.map(|b| match b {
                        pane::PointerButton::Left => crate::runtime::PointerButton::Left,
                        pane::PointerButton::Middle => crate::runtime::PointerButton::Middle,
                        pane::PointerButton::Right => crate::runtime::PointerButton::Right,
                    }),
                    position: SurfacePosition {
                        x: input.x,
                        y: input.y,
                    },
                    modifiers: decode_modifiers(input.modifiers),
                    shift_selection: ShiftSelectionPolicy::from_selection_override(
                        input.shift_selection,
                    ),
                }));
            }
            Operation::Wheel(input) => {
                if self.screen.as_ref().is_some_and(|s| {
                    s.mouse_tracking
                        || s.active_screen == ActiveScreenSnapshot::Alternate && s.alternate_scroll
                }) && !input.shift_selection
                {
                    return Err(Refusal::Stale);
                }
                self.pointer_generation(self.screen_generation, input.x, input.y, input.modifiers)?;
                let generation = self.reading_generation(input.screen_generation)?;
                if input.horizontal_steps.unsigned_abs() > 100
                    || input.vertical_steps.unsigned_abs() > 100
                {
                    return Err(Refusal::Unsupported);
                }
                self.reading.applying = Some(self.runtime.reading_wheel(WheelInput {
                    generation,
                    horizontal_steps: input.horizontal_steps,
                    vertical_steps: input.vertical_steps,
                    phase: match input.phase {
                        pane::WheelPhase::GestureStarted => {
                            crate::runtime::WheelPhase::GestureStarted
                        }
                        pane::WheelPhase::GestureChanged => {
                            crate::runtime::WheelPhase::GestureChanged
                        }
                        pane::WheelPhase::GestureEnded => crate::runtime::WheelPhase::GestureEnded,
                        pane::WheelPhase::GestureCancelled => {
                            crate::runtime::WheelPhase::GestureCancelled
                        }
                        pane::WheelPhase::MomentumStarted => {
                            crate::runtime::WheelPhase::MomentumStarted
                        }
                        pane::WheelPhase::MomentumChanged => {
                            crate::runtime::WheelPhase::MomentumChanged
                        }
                        pane::WheelPhase::MomentumEnded => {
                            crate::runtime::WheelPhase::MomentumEnded
                        }
                        pane::WheelPhase::MomentumCancelled => {
                            crate::runtime::WheelPhase::MomentumCancelled
                        }
                    },
                    position: SurfacePosition {
                        x: input.x,
                        y: input.y,
                    },
                    modifiers: decode_modifiers(input.modifiers),
                    shift_selection: ShiftSelectionPolicy::from_selection_override(
                        input.shift_selection,
                    ),
                }));
            }
        }
        self.reading.fence(&self.runtime);
        Ok(Reply::Reading { accepted: true })
    }
}
