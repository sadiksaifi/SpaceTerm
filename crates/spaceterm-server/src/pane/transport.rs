//! Pipelined exchanges on one shared transport. Requests leave in FIFO ticket order, so a
//! chunk producer stays behind already waiting callers. Replies return by request ID, so a
//! slow reply never holds a faster one. Any failure fences every outstanding exchange.
use crate::{ServerError, protocol::Duplex, transport::Transport};
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex, MutexGuard},
};

const MAX_PENDING: u64 = 2 * crate::hierarchy::MAX_PANES as u64;

pub(super) struct Exchanges {
    duplex: Duplex<Box<dyn Transport>>,
    state: Mutex<State>,
    replies: Condvar,
}
#[derive(Default)]
struct State {
    next: u64,
    serving: u64,
    waiters: VecDeque<Arc<Condvar>>,
    outstanding: HashMap<u64, Slot>,
    /// The one exchange reading frames for all outstanding exchanges.
    reader: Option<u64>,
    fence: Option<ServerError>,
}
enum Slot {
    Waiting,
    Ready(Vec<u8>),
    /// Its caller left after writing. The next reader discards the reply.
    Abandoned,
}
/// The FIFO right to write the next request.
pub(super) struct Turn<'a> {
    exchanges: &'a Exchanges,
    ticket: u64,
}
/// A written request whose reply belongs to this caller until it is read or abandoned.
pub(super) struct Pending<'a> {
    exchanges: &'a Exchanges,
    id: u64,
}
impl Exchanges {
    pub(super) fn new(stream: Box<dyn Transport>) -> Self {
        Self {
            duplex: Duplex::new(stream),
            state: Mutex::default(),
            replies: Condvar::new(),
        }
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(super) fn admit(&self) -> Result<Turn<'_>, ServerError> {
        let mut state = self.state();
        if state.fence.is_some() {
            return Err(ServerError::NotRunning);
        }
        if state.next - state.serving + state.outstanding.len() as u64 >= MAX_PENDING {
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
        Ok(Turn {
            exchanges: self,
            ticket,
        })
    }
    pub(super) fn fenced(&self) -> bool {
        self.state().fence.is_some()
    }
    /// Fails every outstanding exchange as unconfirmed and ends the connection. Nothing is
    /// replayed; a new connection gets a fresh host identity.
    pub(super) fn fence(&self, error: ServerError) {
        self.fence_locked(&mut self.state(), error);
    }
    fn fence_locked(&self, state: &mut State, error: ServerError) -> ServerError {
        let error = *state.fence.get_or_insert_with(|| {
            self.duplex.close();
            error
        });
        self.replies.notify_all();
        error
    }
}
impl<'a> Turn<'a> {
    /// Writes this request, then releases the writer before its reply arrives.
    pub(super) fn write(self, request: &impl Serialize) -> Result<Pending<'a>, ServerError> {
        let exchanges = self.exchanges;
        {
            let mut state = exchanges.state();
            if state.fence.is_some() {
                return Err(ServerError::NotRunning);
            }
            // Registered first, so a reply read before this write returns finds its owner.
            state.outstanding.insert(self.ticket, Slot::Waiting);
        }
        let pending = Pending {
            exchanges,
            id: self.ticket,
        };
        if let Err(error) = exchanges.duplex.send(self.ticket, request) {
            let error = exchanges.fence_locked(&mut exchanges.state(), error);
            return Err(error);
        }
        Ok(pending)
    }
}
impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut state = self.exchanges.state();
        state.serving += 1;
        if let Some(ready) = state.waiters.pop_front() {
            ready.notify_one();
        }
    }
}
impl Pending<'_> {
    /// Waits for this request's reply. Whichever waiting caller finds no reader reads frames
    /// for everyone until its own reply arrives, then hands reading to the next waiter.
    pub(super) fn reply(self) -> Result<Vec<u8>, ServerError> {
        let exchanges = self.exchanges;
        let mut state = exchanges.state();
        loop {
            if let Some(Slot::Ready(bytes)) = state.outstanding.get_mut(&self.id) {
                let bytes = std::mem::take(bytes);
                state.outstanding.remove(&self.id);
                return Ok(bytes);
            }
            if let Some(error) = state.fence {
                return Err(error);
            }
            if state.reader.is_none() {
                break;
            }
            state = exchanges
                .replies
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
        state.reader = Some(self.id);
        drop(state);
        loop {
            let frame = exchanges.duplex.receive_frame();
            let mut state = exchanges.state();
            let (id, bytes) = match frame {
                Ok(frame) => frame,
                Err(error) => {
                    state.reader = None;
                    return Err(exchanges.fence_locked(&mut state, error));
                }
            };
            if id == self.id {
                state.reader = None;
                state.outstanding.remove(&id);
                exchanges.replies.notify_all();
                return Ok(bytes);
            }
            match state.outstanding.get_mut(&id) {
                Some(slot @ Slot::Waiting) => {
                    *slot = Slot::Ready(bytes);
                    exchanges.replies.notify_all();
                }
                Some(Slot::Abandoned) => {
                    state.outstanding.remove(&id);
                }
                Some(Slot::Ready(_)) | None => {
                    state.reader = None;
                    return Err(exchanges.fence_locked(&mut state, ServerError::Protocol));
                }
            }
        }
    }
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        let exchanges = self.exchanges;
        let mut state = exchanges.state();
        if state.reader == Some(self.id) {
            // Left while reading, so the stream position is unknown.
            state.reader = None;
            exchanges.fence_locked(&mut state, ServerError::Protocol);
        }
        let fenced = state.fence.is_some();
        match state.outstanding.get_mut(&self.id) {
            Some(slot @ Slot::Waiting) if !fenced => *slot = Slot::Abandoned,
            Some(_) => {
                state.outstanding.remove(&self.id);
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Wire;
    use std::{
        os::unix::net::UnixStream,
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };

    fn exchanges() -> (Arc<Exchanges>, UnixStream) {
        let (client, server) = UnixStream::pair().unwrap();
        (Arc::new(Exchanges::new(Box::new(client))), server)
    }

    #[test]
    fn a_chunk_producer_cannot_reenter_ahead_of_a_waiting_input() {
        let (admission, _server) = exchanges();
        let bulk = admission.admit().unwrap();
        let (sent, received) = mpsc::channel();
        let input = Arc::clone(&admission);
        let input_thread = std::thread::spawn(move || {
            let _turn = input.admit().unwrap();
            sent.send("input").unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while admission.state().next != 2 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        drop(bulk);
        let _next_chunk = admission.admit().unwrap();
        assert_eq!(received.try_recv().unwrap(), "input");
        input_thread.join().unwrap();
    }

    #[test]
    fn saturation_refuses_before_allocating_another_turn() {
        let (admission, _server) = exchanges();
        admission.state().next = MAX_PENDING;
        assert!(matches!(admission.admit(), Err(ServerError::Capacity)));
    }

    #[test]
    fn an_abandoned_reply_is_discarded_by_the_next_reader() {
        let (exchanges, mut server) = exchanges();
        let abandoned = exchanges.admit().unwrap().write(&"first").unwrap();
        drop(abandoned);
        let kept = exchanges.admit().unwrap().write(&"second").unwrap();
        let mut wire = Wire::terminal(&mut server).unwrap();
        for expected in ["first", "second"] {
            let (id, request) = wire.receive::<String>().unwrap();
            assert_eq!(request, expected);
            wire.send(id, &format!("reply to {request}")).unwrap();
        }
        assert_eq!(
            serde_json::from_slice::<String>(&kept.reply().unwrap()).unwrap(),
            "reply to second"
        );
        assert!(!exchanges.fenced());
        assert!(exchanges.state().outstanding.is_empty());
    }
}
