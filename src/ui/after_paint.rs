//! Work that waits until a frame has drawn an element, such as recording that a person saw an
//! answer.
//!
//! A surface queues work and draws [`AfterPaint::hook`] beside what it shows. Painting the hook
//! hands the queued work to the end of that frame, after it has drawn. A surface that never
//! draws the hook again, because it closed, drops the work with its queue.
use std::{cell::RefCell, rc::Rc};

use gpui::{App, IntoElement, Styled as _, canvas};

type Waiting = Vec<Box<dyn FnOnce(&mut App)>>;

#[derive(Clone, Default)]
pub(crate) struct AfterPaint(Rc<RefCell<Waiting>>);

impl AfterPaint {
    /// Runs `work` once a frame that paints the hook has drawn.
    pub(crate) fn push(&self, work: impl FnOnce(&mut App) + 'static) {
        self.0.borrow_mut().push(Box::new(work));
    }

    /// Drops the waiting work, as for a surface that closed before a frame drew it.
    pub(crate) fn clear(&self) {
        let waiting = std::mem::take(&mut *self.0.borrow_mut());
        drop(waiting);
    }

    /// A zero-size element that releases the waiting work to the end of the frame painting it.
    pub(crate) fn hook(&self) -> impl IntoElement {
        let waiting = Rc::clone(&self.0);
        canvas(
            |_, _, _| {},
            move |_, (), _, cx| {
                let drawn = std::mem::take(&mut *waiting.borrow_mut());
                for work in drawn {
                    cx.defer(work);
                }
            },
        )
        .absolute()
        .size_0()
    }
}
