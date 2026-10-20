//! FIFO exchange admission keeps a chunk producer behind already waiting callers.
use crate::ServerError;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
};

const MAX_PENDING: u64 = 2 * crate::hierarchy::MAX_PANES as u64;

#[derive(Default)]
pub(super) struct Admission {
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    next: u64,
    serving: u64,
    waiters: VecDeque<Arc<Condvar>>,
}
pub(super) struct Turn<'a>(&'a Admission);
impl Admission {
    pub(super) fn enter(&self) -> Result<Turn<'_>, ServerError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.next - state.serving >= MAX_PENDING {
            return Err(ServerError::Capacity);
        }
        let ticket = state.next;
        state.next = state.next.checked_add(1).ok_or(ServerError::Capacity)?;
        let ready = (state.serving != ticket).then(|| {
            let ready = Arc::new(Condvar::new());
            state.waiters.push_back(Arc::clone(&ready));
            ready
        });
        if let Some(ready) = ready {
            while state.serving != ticket {
                state = ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        }
        Ok(Turn(self))
    }
}
impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.serving += 1;
        if let Some(ready) = state.waiters.pop_front() {
            ready.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };

    #[test]
    fn a_chunk_producer_cannot_reenter_ahead_of_a_waiting_input() {
        let admission = Arc::new(Admission::default());
        let bulk = admission.enter().unwrap();
        let (sent, received) = mpsc::channel();
        let input = Arc::clone(&admission);
        let input_thread = std::thread::spawn(move || {
            let _turn = input.enter().unwrap();
            sent.send("input").unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while admission.state.lock().unwrap().next != 2 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        drop(bulk);
        let _next_chunk = admission.enter().unwrap();
        assert_eq!(received.try_recv().unwrap(), "input");
        input_thread.join().unwrap();
    }

    #[test]
    fn saturation_refuses_before_allocating_another_turn() {
        let admission = Admission::default();
        admission.state.lock().unwrap().next = MAX_PENDING;
        assert!(matches!(admission.enter(), Err(ServerError::Capacity)));
    }
}
