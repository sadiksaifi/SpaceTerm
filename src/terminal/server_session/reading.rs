//! This client's reading of one Pane: its Scrollback Cache, view, Selection, Find and copy, and
//! where wheel and pointer input go.
//!
//! The UI thread changes the reading directly, without a round trip, and wakes the Pane worker to
//! present the result. The worker applies frames, decodes Scrollback pages and asks for the rows
//! the reading still needs.
use super::scrollback::{
    CachedRow, CopyOutcome, Grid, PageRequest, PointerEvent, Scrollback,
    route::{self, Modes, PointerRoute, PointerRouter, WheelRoute},
};
use super::*;
use crate::terminal::selection::SelectionCopyOptions;
use spaceterm_server::protocol::Page;

/// How often a selection drag past an edge scrolls one row, as Ghostty does.
pub(super) const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(15);

#[derive(Default)]
pub(super) struct Reading {
    pub(super) scrollback: Scrollback,
    router: PointerRouter,
    /// The view changed since the worker last presented it.
    changed: bool,
    /// A copy waits for Scrollback pages; `SelectionCopyReady` reports when it completes.
    deferred_copy: bool,
    /// Replies to page requests, waiting for the worker to decode them.
    pages: Vec<(PageRequest, Option<Page>)>,
    /// The grid of the latest presented screen, for autoscroll.
    grid: Option<Grid>,
    autoscroll_due: Option<std::time::Instant>,
}

/// What one worker turn of reading asks for.
pub(super) struct Turn {
    /// The view changed and must be presented again.
    pub(super) present: bool,
    /// A deferred copy completed.
    pub(super) copy_ready: bool,
    /// When the next autoscroll step is due.
    pub(super) deadline: Option<std::time::Instant>,
}

impl Reading {
    /// The connection ended. Cached rows stay for a reconnect to the same Terminal Session; the
    /// gesture, pending pages and any deferred copy do not.
    pub(super) fn disconnect(&mut self) {
        self.router.cancel();
        self.scrollback.pointer_cancel();
        self.deferred_copy = false;
        // Replies to pages of the ended connection still settle them, so they stop counting
        // toward the in-flight limit.
        self.autoscroll_due = None;
    }

    pub(super) fn page(&mut self, request: PageRequest, page: Option<Page>) {
        self.pages.push((request, page));
    }

    /// Decodes arrived pages and runs a due autoscroll step. `columns` is the screen's width,
    /// which every row of the current history epoch shares.
    pub(super) fn turn(
        &mut self,
        now: std::time::Instant,
        columns: u16,
        local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
    ) -> Turn {
        for (request, page) in std::mem::take(&mut self.pages) {
            let rows = page
                .filter(|page| {
                    page.epoch == request.epoch
                        && page.start >= request.start
                        && page.rows.len() <= usize::from(request.rows)
                })
                .and_then(|page| {
                    let rows = super::screen_decoder::decode_page(&page, columns, local_files)
                        .ok()?
                        .iter()
                        .map(|(cells, wrapped)| CachedRow::new(cells, *wrapped))
                        .collect::<Vec<_>>();
                    Some((page.epoch, page.start, rows))
                });
            self.scrollback.page(request, rows);
            // A page can fill rows the view, Find or the Selection shows.
            self.changed = true;
        }
        if self.scrollback.autoscrolling() {
            let due = *self.autoscroll_due.get_or_insert(now + AUTOSCROLL_INTERVAL);
            if now >= due {
                if let Some(grid) = self.grid {
                    self.changed |= self.scrollback.autoscroll(grid);
                }
                self.autoscroll_due = Some(now + AUTOSCROLL_INTERVAL);
            }
        } else {
            self.autoscroll_due = None;
        }
        let copy_ready = self.deferred_copy && self.scrollback.copy_ready();
        if copy_ready {
            self.deferred_copy = false;
        }
        Turn {
            present: std::mem::take(&mut self.changed),
            copy_ready,
            deadline: self.autoscroll_due,
        }
    }

    /// The Selection's text now, or `None`. A copy whose rows are missing waits, and the
    /// returned flag asks the worker to request their pages.
    fn copy(&mut self) -> (Option<super::super::SelectionCopy>, bool) {
        match self.scrollback.copy(SelectionCopyOptions::default()) {
            CopyOutcome::Ready(copy) => (Some(copy), false),
            CopyOutcome::Pending => {
                self.deferred_copy = true;
                (None, true)
            }
            CopyOutcome::Nothing => (None, false),
        }
    }
}

impl PresentationHandle {
    /// Changes the reading on the calling thread and has the worker present a changed view.
    pub(super) fn local<T>(&self, change: impl FnOnce(&mut Reading) -> (T, bool)) -> T {
        let (value, changed) = {
            let mut reading = self.state.reading.lock().unwrap_or_else(|e| e.into_inner());
            let (value, changed) = change(&mut reading);
            reading.changed |= changed;
            (value, changed)
        };
        if changed {
            self.state.wake.notify();
        }
        value
    }

    fn reading_mapping(&self) -> Option<ScreenMapping> {
        if self.state.status() != DevelopmentPaneStatus::Connected {
            return None;
        }
        let interaction = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        interaction.screen.filter(|_| interaction.reading)
    }

    /// Routes a pointer event to the local Selection or to the program. A press decides the
    /// route of its whole gesture.
    pub(super) fn route_pointer(&self, input: PointerInput) -> Option<super::super::SelectionCopy> {
        use super::super::pointer_input::{PointerButton, PointerPhase, ShiftSelectionPolicy};
        let continuation = input.phase == PointerPhase::Release
            || (input.phase == PointerPhase::Motion && input.button.is_some());
        let (mapping, x, y) =
            self.pointer_position(input.generation, input.position, continuation)?;
        let shift_selection =
            input.shift_selection == ShiftSelectionPolicy::OverrideApplicationMouse;
        let modes = modes(mapping, shift_selection && input.modifiers.shift);
        let phase = match input.phase {
            PointerPhase::Press => route::PointerPhase::Press,
            PointerPhase::Motion => route::PointerPhase::Motion,
            PointerPhase::Release => route::PointerPhase::Release,
        };
        let button = input.button.map(|button| match button {
            PointerButton::Left => route::PointerButton::Left,
            PointerButton::Middle | PointerButton::Right => route::PointerButton::Other,
        });
        let grid = grid(mapping);
        let width = f32::from(mapping.size.columns) * f32::from(mapping.size.cell_width);
        let height = f32::from(mapping.size.rows) * f32::from(mapping.size.cell_height);
        let (route, copy) = self.local(|reading| {
            let route = reading.router.route(phase, button, modes);
            if route != PointerRoute::Selection {
                return ((route, None), false);
            }
            reading.grid = Some(grid);
            let event = match phase {
                route::PointerPhase::Press => PointerEvent::Press { x, y },
                route::PointerPhase::Motion => PointerEvent::Drag { x, y },
                route::PointerPhase::Release => PointerEvent::Release,
            };
            let changed = reading
                .scrollback
                .pointer(event, grid, std::time::Instant::now());
            let (copy, pending) = if phase == route::PointerPhase::Release
                && reading.scrollback.selection_present()
            {
                reading.copy()
            } else {
                (None, false)
            };
            // A drag past an edge starts autoscroll on the worker.
            (
                (route, copy),
                changed || pending || reading.scrollback.autoscrolling(),
            )
        });
        if route == PointerRoute::Program {
            self.send(Input::Pointer(
                mapping.epoch,
                pane::Pointer {
                    screen_generation: mapping.wire,
                    phase: match input.phase {
                        PointerPhase::Press => pane::PointerPhase::Press,
                        PointerPhase::Motion => pane::PointerPhase::Motion,
                        PointerPhase::Release => pane::PointerPhase::Release,
                    },
                    button: input.button.map(|button| match button {
                        PointerButton::Left => pane::PointerButton::Left,
                        PointerButton::Middle => pane::PointerButton::Middle,
                        PointerButton::Right => pane::PointerButton::Right,
                    }),
                    x: x.clamp(0.0, width - 1.0),
                    y: y.clamp(0.0, height - 1.0),
                    modifiers: encode_modifiers(input.modifiers),
                    shift_selection,
                },
            ));
        }
        copy
    }

    /// Scrolls this client's view, or sends the wheel to the program. The wheel needs only the
    /// grid it was measured against, not the exact presentation, so output arriving between a
    /// paint and the wheel never drops it.
    pub(super) fn route_wheel(&self, input: WheelInput) {
        use super::super::pointer_input::{ShiftSelectionPolicy, WheelPhase};
        let Some((mapping, x, y)) = self.pointer_position(input.generation, input.position, true)
        else {
            return;
        };
        let shift_selection =
            input.shift_selection == ShiftSelectionPolicy::OverrideApplicationMouse;
        match route::wheel_route(modes(mapping, shift_selection && input.modifiers.shift)) {
            WheelRoute::Ignore => {}
            WheelRoute::Scrollback => {
                let steps = input
                    .vertical_steps
                    .clamp(-MAX_WHEEL_LINES, MAX_WHEEL_LINES);
                self.local(|reading| ((), reading.scrollback.wheel(steps)));
            }
            WheelRoute::Program => {
                let wheel = pane::Wheel {
                    screen_generation: mapping.wire,
                    horizontal_steps: input.horizontal_steps,
                    vertical_steps: input.vertical_steps,
                    phase: match input.phase {
                        WheelPhase::GestureStarted => pane::WheelPhase::GestureStarted,
                        WheelPhase::GestureChanged => pane::WheelPhase::GestureChanged,
                        WheelPhase::GestureEnded => pane::WheelPhase::GestureEnded,
                        WheelPhase::GestureCancelled => pane::WheelPhase::GestureCancelled,
                        WheelPhase::MomentumStarted => pane::WheelPhase::MomentumStarted,
                        WheelPhase::MomentumChanged => pane::WheelPhase::MomentumChanged,
                        WheelPhase::MomentumEnded => pane::WheelPhase::MomentumEnded,
                        WheelPhase::MomentumCancelled => pane::WheelPhase::MomentumCancelled,
                    },
                    x: x.clamp(
                        0.0,
                        f32::from(mapping.size.columns) * f32::from(mapping.size.cell_width) - 1.0,
                    ),
                    y: y.clamp(
                        0.0,
                        f32::from(mapping.size.rows) * f32::from(mapping.size.cell_height) - 1.0,
                    ),
                    modifiers: encode_modifiers(input.modifiers),
                    shift_selection,
                };
                // A large native delta becomes several bounded messages, never a refused one.
                for wheel in split_wheel(wheel) {
                    self.send(Input::Wheel(mapping.epoch, wheel));
                }
            }
        }
    }

    /// Runs a reading change while the Pane presents a screen.
    pub(super) fn read(&self, change: impl FnOnce(&mut Scrollback) -> bool) {
        if self.reading_mapping().is_some() {
            self.local(|reading| ((), change(&mut reading.scrollback)));
        }
    }

    /// The Selection's text, or `None` while it has none or its rows are still arriving. A copy
    /// that waits for rows reports `SelectionCopyReady` once it completes.
    pub(super) fn copy(&self) -> Option<super::super::SelectionCopy> {
        self.reading_mapping()?;
        self.local(|reading| reading.copy())
    }

    /// The copy a pointer release or an earlier request left waiting for rows.
    pub(super) fn take_deferred_copy(&self) -> Option<super::super::SelectionCopy> {
        self.reading_mapping()?;
        self.local(|reading| {
            let copy = match reading.scrollback.copy(SelectionCopyOptions::default()) {
                CopyOutcome::Ready(copy) => Some(copy),
                _ => None,
            };
            (copy, false)
        })
    }

    /// Typing and pasting return the view to the screen and clear the Selection, as they do in a
    /// local terminal.
    pub(super) fn typed(&self) {
        self.local(|reading| ((), reading.scrollback.input()));
    }

    pub(super) fn selection_present(&self) -> bool {
        self.local(|reading| (reading.scrollback.selection_present(), false))
    }

    pub(super) fn cancel_gesture(&self) {
        self.local(|reading| {
            reading.router.cancel();
            reading.scrollback.pointer_cancel();
            ((), false)
        });
    }
}

/// Presents this client's view as `generation`: of `frame` when a new frame arrived, otherwise of
/// the last frame after the reading changed. Returns the frame's mapping to that presentation.
pub(super) fn show(
    state: &DevelopmentPaneState,
    events: &async_channel::Sender<TerminalSessionEvent>,
    frame: Option<Arc<ScreenSnapshot>>,
    (epoch, generation, mut mapping): (u64, PresentationGeneration, ScreenMapping),
    paste_unconfirmed: bool,
) -> Result<ScreenMapping, ServerError> {
    let presented = {
        let mut reading = state.reading.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(frame) = frame {
            reading.scrollback.screen(frame);
        }
        reading.grid = Some(grid(mapping));
        (
            reading.scrollback.present(generation),
            reading.scrollback.accessibility_view(),
        )
    };
    let (presented, view) = presented;
    // Assistive technology follows this client's scrolled view and Selection.
    state
        .accessibility
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .set_view(view);
    mapping.local = generation;
    state.connected(epoch, mapping, paste_unconfirmed)?;
    if let Some(presented) = presented {
        let _ = events.force_send(TerminalSessionEvent::Screen(presented));
    }
    Ok(mapping)
}

fn modes(mapping: ScreenMapping, shift_override: bool) -> Modes {
    Modes {
        mouse_tracking: mapping.mouse_tracking,
        alternate_screen: mapping.alternate,
        alternate_scroll: mapping.alternate_scroll,
        shift_override,
    }
}

fn grid(mapping: ScreenMapping) -> Grid {
    Grid {
        columns: mapping.size.columns,
        rows: mapping.size.rows,
        cell_width: f32::from(mapping.size.cell_width),
        cell_height: f32::from(mapping.size.cell_height),
    }
}

/// One native wheel event scrolls at most this many lines of the local view.
const MAX_WHEEL_LINES: i32 = MAX_WHEEL_STEPS * MAX_WHEEL_PARTS;

/// The server accepts at most this many steps per axis in one wheel event.
pub(super) const MAX_WHEEL_STEPS: i32 = 100;

/// One native event moves at most this many server events' worth of steps. A larger delta is not
/// a plausible device event; it is bounded rather than queued as a long tail.
const MAX_WHEEL_PARTS: i32 = 16;

/// Divides one native wheel event into events the server accepts, in order, keeping its start
/// edge on the first part and its end edge on the last.
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_large_native_delta_is_divided_into_accepted_events_without_losing_steps() {
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
}
