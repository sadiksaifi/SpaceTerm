//! A read of visible terminal text without attaching a channel to any Pane.

use super::CorePaneHost;
use super::screen_encoder::preview_rows;
use crate::terminal::emulator::{PreviewGrid, ScreenSnapshot};
use crate::terminal::session::PreviewCapture;
use spaceterm_core::connection::outbox::Outbox;
use spaceterm_core::connection::service::Dispatch;
use spaceterm_core::hierarchy::Id;
use spaceterm_core::protocol::preview::{self, PanePreview, Screen, State};
use spaceterm_core::protocol::{Refusal, ReplyBody, RequestId};
use spaceterm_terminal_client::snapshot::ColorValue;
use std::sync::{Arc, Mutex};

pub(super) enum Source<'a> {
    Missing,
    NotStarted,
    Pane(&'a CorePaneHost),
}

/// A reply is owed even when some of the Panes' workers end before handling the read.
struct Batch {
    outbox: Outbox,
    request: RequestId,
    panes: Vec<Id>,
    states: Mutex<Vec<Result<State, Refusal>>>,
}

impl Batch {
    fn record(&self, index: usize, result: Result<State, Refusal>) {
        self.states.lock().unwrap_or_else(|e| e.into_inner())[index] = result;
    }
    fn result(self) -> Result<ReplyBody, Refusal> {
        let states = self.states.into_inner().unwrap_or_else(|e| e.into_inner());
        self.panes
            .into_iter()
            .zip(states)
            .map(|(pane, state)| state.map(|state| PanePreview { pane, state }))
            .collect::<Result<Vec<_>, _>>()
            .map(ReplyBody::PanePreviews)
    }

    fn send(self) {
        let outbox = self.outbox.clone();
        let request = self.request;
        let _ = outbox.reply_or_refuse(request, self.result());
    }
}

pub(super) fn read<'a>(
    panes: Vec<Id>,
    source: impl Fn(Id) -> Source<'a>,
    request: RequestId,
    outbox: &Outbox,
) -> Dispatch {
    let batch = Arc::new(Batch {
        outbox: outbox.clone(),
        request,
        states: Mutex::new(vec![Err(Refusal::Failed); panes.len()]),
        panes,
    });
    for (index, id) in batch.panes.iter().copied().enumerate() {
        let pane = match source(id) {
            Source::Missing => {
                batch.record(index, Ok(State::Missing));
                continue;
            }
            Source::NotStarted => {
                batch.record(index, Ok(State::NotStarted));
                continue;
            }
            Source::Pane(pane) => pane,
        };
        if let Some(live) = &pane.pane {
            let session = live.lease.session;
            let result = live.runtime.read_preview({
                let batch = Arc::clone(&batch);
                move |capture| {
                    batch.record(index, Ok(captured(session, capture)));
                    if let Some(batch) = Arc::into_inner(batch) {
                        batch.send();
                    }
                }
            });
            if let Err(error) = result {
                batch.record(index, Err(error));
            }
        } else if let Some(dormant) = &pane.dormant {
            batch.record(
                index,
                Ok(State::Ended {
                    session: dormant.session.map(|target| target.session),
                    screen: pane.last_preview.clone(),
                }),
            );
        } else {
            batch.record(index, Ok(State::NotStarted));
        }
    }
    // The last owner replies. If everything settled immediately, the sync worker encodes it
    // without holding the service lock. Otherwise the last terminal worker replies.
    match Arc::into_inner(batch) {
        Some(batch) => Dispatch::After(Box::new(move || batch.result())),
        None => Dispatch::Deferred,
    }
}

fn captured(session: Id, capture: PreviewCapture) -> State {
    match capture {
        PreviewCapture::Current(grid) => State::Live {
            session,
            screen: Some(current(&grid)),
        },
        PreviewCapture::Published {
            ended: true,
            screen,
        } => State::Ended {
            session: Some(session),
            screen: screen.as_deref().and_then(published),
        },
        PreviewCapture::Published {
            ended: false,
            screen,
        } => State::Live {
            session,
            screen: screen.as_deref().and_then(published),
        },
    }
}

fn current(grid: &PreviewGrid) -> Screen {
    Screen {
        columns: grid.columns,
        row_count: grid.rows.len() as u16,
        cropped: grid.cropped,
        foreground: grid.foreground.rgb(),
        background: grid.background.rgb(),
        reversed: grid.reversed,
        rows: preview_rows(
            grid.rows
                .iter()
                .map(|(row, wrapped)| (row.as_ref(), *wrapped)),
            grid.columns,
            &grid.palette,
        ),
    }
}

pub(super) fn published(screen: &ScreenSnapshot) -> Option<Screen> {
    let columns = screen.size.cols.min(preview::MAX_COLUMNS);
    let row_count = screen.size.rows.min(preview::MAX_ROWS);
    if columns == 0 || row_count == 0 || screen.rows.is_empty() {
        return None;
    }
    Some(Screen {
        columns,
        row_count,
        cropped: columns < screen.size.cols || row_count < screen.size.rows,
        foreground: screen.colors.foreground.rgb(),
        background: screen.colors.background.rgb(),
        reversed: screen.colors.reversed,
        rows: preview_rows(
            screen
                .rows
                .iter()
                .take(usize::from(row_count))
                .enumerate()
                .map(|(index, row)| {
                    (
                        row.as_ref(),
                        screen.row_soft_wrapped.get(index).copied().unwrap_or(false),
                    )
                }),
            columns,
            &screen.colors.palette,
        ),
    })
}
