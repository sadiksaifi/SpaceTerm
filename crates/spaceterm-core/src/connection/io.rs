//! Serves work requests over an admitted carrier.
use super::admitted::{AdmittedConnection, Inbound};
use super::carrier::{Carrier, CarrierError};
use super::service::SharedPaneService;
use crate::protocol::{Fatal, limits};
pub use spaceterm_client::connection::io::{Waker, spawn_link, wait_fd};
use spaceterm_client::connection::io::{drain, wait, write};
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Reads and discards what the peer sent that this end never read, without waiting, before the
/// socket closes. A Unix or TCP socket closed with unread input resets the connection on Linux,
/// and a reset can make the peer drop a goodbye it has not read yet. Bounded, so a peer that keeps
/// sending cannot hold the close.
pub(crate) fn discard_unread(fd: RawFd) {
    let mut buffer = [0u8; 16 * 1024];
    for _ in 0..64 {
        // SAFETY: recv writes at most `buffer.len()` bytes into the live local buffer, from the
        // caller's live descriptor, without blocking.
        let count = unsafe {
            libc::recv(
                fd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if count <= 0 {
            return;
        }
    }
}

/// Serves one admitted connection on the calling thread until it ends, then reports
/// [`super::service::PaneService::disconnected`]. The caller already called
/// [`super::service::PaneService::admitted`] with `connection`'s outbox.
///
/// When `stopping` is set and the thread woken, the connection stops reading, writes everything
/// already queued, including pending replies, and ends with `Goodbye(Stopping)`.
pub fn serve(
    mut carrier: Box<dyn Carrier>,
    mut connection: AdmittedConnection,
    waker: &Waker,
    service: &SharedPaneService,
    stopping: &AtomicBool,
) {
    let _ended = Ended {
        outbox: connection.outbox().clone(),
        service: Arc::clone(service),
        peer: *connection.peer(),
    };
    let mut held = Vec::new();
    let mut receiving = true;
    let mut stopped: Option<Instant> = None;
    loop {
        let now = Instant::now();
        let outbox = connection.outbox().clone();
        if stopping.load(Ordering::Acquire) {
            receiving = false;
            stopped.get_or_insert(now);
        }
        // A turn that stopped at its receive bound continues without waiting: the socket, or the
        // QUIC streams hold, may already have more.
        let mut more = false;
        if receiving {
            match receive(carrier.as_mut(), &mut connection, service, now) {
                Ok(remaining) => more = remaining,
                Err(Some(fatal)) => {
                    let _ = outbox.goodbye(fatal);
                    receiving = false;
                }
                // The peer closed or the socket's read side was severed. Only a goodbye that is
                // already queued still leaves.
                Err(None) if outbox.is_closing() || stopping.load(Ordering::Acquire) => {
                    receiving = false
                }
                Err(None) => break,
            }
        }
        // A Terminal Session bound to this connection has something new. Publishing here, under
        // the `PaneService` lock, puts its frame in this turn's writes without the host's tick.
        // A closing connection still hands the request over, so the service can pass it on.
        if outbox.take_publication() {
            service
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .publish(connection.peer());
        }
        let written = match write(carrier.as_mut(), &mut held, |lowest| {
            outbox.next_frame_within(lowest)
        }) {
            Ok(written) => written,
            Err(_) => break,
        };
        connection.wrote(now, written.progressed, written.pending);
        if !written.pending && !receiving {
            if !outbox.is_closing() {
                // Stopping: replies the sync worker still owes go out first, for at most
                // `STALL_AFTER`, so the goodbye goes last.
                if let Some(since) = stopped
                    && outbox.owes_replies()
                    && now < since + limits::STALL_AFTER
                {
                    wait(
                        carrier.as_ref(),
                        (false, false),
                        waker,
                        Some(since + limits::STALL_AFTER),
                    );
                    continue;
                }
                let _ = outbox.goodbye(Fatal::Stopping);
                continue;
            }
            break;
        }
        let deadline = match connection.check(now) {
            Ok(deadline) => Some(deadline),
            Err(_) if !outbox.is_closing() => {
                let _ = outbox.goodbye(Fatal::Stalled);
                receiving = false;
                continue;
            }
            // A goodbye that cannot leave either ends the connection now.
            Err(_) => break,
        };
        if more {
            continue;
        }
        wait(
            carrier.as_ref(),
            (receiving, written.buffered),
            waker,
            deadline,
        );
    }
    drain(carrier.as_mut(), waker);
}

/// Reports a connection's end to its outbox and service, also when its thread panics, so its
/// state never stays registered.
struct Ended {
    outbox: super::outbox::Outbox,
    service: SharedPaneService,
    peer: super::service::Peer,
}

impl Drop for Ended {
    fn drop(&mut self) {
        self.outbox.disconnected();
        self.service
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disconnected(&self.peer);
    }
}

/// Frames one turn receives before it dispatches, writes and checks health, so a peer that never
/// stops sending cannot grow the turn without bound or starve writing.
const TURN_FRAMES: usize = 256;
/// Frame bytes one turn receives before it dispatches.
const TURN_BYTES: usize = limits::MAX_FRAME_BYTES;

/// Receives until the carrier has nothing or the turn's bound is reached, then dispatches under one
/// lock. `Ok(true)` means the turn stopped at its bound and more may be waiting. `Err(None)` means
/// the socket closed; `Err(Some)` is the goodbye to send.
fn receive(
    carrier: &mut dyn Carrier,
    connection: &mut AdmittedConnection,
    service: &SharedPaneService,
    now: Instant,
) -> Result<bool, Option<Fatal>> {
    let mut inbound: Vec<Inbound> = Vec::new();
    let mut result = Ok(false);
    let (mut frames, mut bytes) = (0, 0);
    loop {
        if frames >= TURN_FRAMES || bytes >= TURN_BYTES {
            result = Ok(true);
            break;
        }
        match carrier.receive() {
            Ok(Some(frame)) => {
                frames += 1;
                bytes += limits::HEADER_BYTES + frame.body.len();
                match connection.receive(frame, now) {
                    Ok(Some(message)) => inbound.push(message),
                    Ok(None) => {}
                    Err(fatal) => {
                        result = Err(Some(fatal));
                        break;
                    }
                }
            }
            Ok(None) => break,
            Err(CarrierError::Decode(_)) => {
                result = Err(Some(Fatal::ProtocolViolation));
                break;
            }
            Err(CarrierError::Closed | CarrierError::Failed) => {
                result = Err(None);
                break;
            }
        }
    }
    if !inbound.is_empty() {
        let mut service = service.lock().unwrap_or_else(|e| e.into_inner());
        for message in inbound {
            if let Err(fatal) = connection.dispatch(message, service.as_mut()) {
                return Err(Some(fatal));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::carrier::{Offer, Priority};
    use super::*;
    use crate::connection::carrier::{Flush, Readiness};
    use crate::protocol::RawFrame;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A carrier full until its next flush, which writes nothing because the backlog already left
    /// while it received, as a carrier answering the peer does.
    struct Backpressured {
        room: bool,
        accepted: usize,
        pending: bool,
        closed: bool,
        /// Receives one message for each flush after `close`.
        drained: mpsc::Sender<()>,
        /// Never readable, because this carrier never has a frame to receive. Its peer stays open:
        /// a closed peer would keep the descriptor readable and wake the live loop at once.
        bell: UnixStream,
        _peer: UnixStream,
    }

    impl Backpressured {
        fn new(pending: bool) -> (Self, mpsc::Receiver<()>) {
            let (drained, flushes) = mpsc::channel();
            let (bell, peer) = UnixStream::pair().unwrap();
            let carrier = Self {
                room: false,
                accepted: 0,
                pending,
                closed: false,
                drained,
                bell,
                _peer: peer,
            };
            (carrier, flushes)
        }
    }

    impl Carrier for Backpressured {
        fn offer(&mut self, _: &[u8], _: Priority) -> Result<Offer, CarrierError> {
            if !self.room {
                return Ok(Offer::Full);
            }
            self.accepted += 1;
            Ok(Offer::Accepted)
        }
        fn flush(&mut self) -> Result<Flush, CarrierError> {
            if self.closed {
                let _ = self.drained.send(());
            }
            self.room = true;
            Ok(Flush {
                progressed: false,
                pending: self.pending,
            })
        }
        fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
            Ok(None)
        }
        fn readiness(&self) -> Readiness {
            Readiness {
                read: self.bell.as_raw_fd(),
                write: None,
                progress: None,
                deadline: None,
                signal: None,
            }
        }
        fn close(&mut self) {
            self.closed = true;
        }
    }

    #[test]
    fn a_frame_refused_while_the_carrier_emptied_itself_is_offered_again_at_once() {
        let (mut carrier, _) = Backpressured::new(false);
        let mut held = Vec::new();
        let mut frames = vec![(vec![0; 8], Priority::L1)].into_iter();
        let written = write(&mut carrier, &mut held, |_| frames.next()).unwrap();
        assert!(held.is_empty(), "the frame must not wait for a later wake");
        assert_eq!(carrier.accepted, 1);
        assert!(!written.pending);
    }

    /// A carrier whose L2 stream waits on flow control: it refuses bulk and takes the rest.
    struct BulkBlocked {
        accepted: Vec<Priority>,
        bell: UnixStream,
    }

    impl Carrier for BulkBlocked {
        fn offer(&mut self, _: &[u8], priority: Priority) -> Result<Offer, CarrierError> {
            if priority == Priority::L2 {
                return Ok(Offer::Full);
            }
            self.accepted.push(priority);
            Ok(Offer::Accepted)
        }
        fn flush(&mut self) -> Result<Flush, CarrierError> {
            Ok(Flush {
                progressed: false,
                pending: true,
            })
        }
        fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
            Ok(None)
        }
        fn readiness(&self) -> Readiness {
            Readiness {
                read: self.bell.as_raw_fd(),
                write: None,
                progress: None,
                deadline: None,
                signal: None,
            }
        }
        fn close(&mut self) {}
    }

    /// Queued frames by level, taken in priority order down to the level asked for, as the
    /// outbox does.
    fn queued(frames: &[Priority]) -> impl FnMut(Priority) -> Option<(Vec<u8>, Priority)> {
        let mut queue: Vec<Priority> = frames.to_vec();
        move |lowest| {
            let (index, _) = queue
                .iter()
                .enumerate()
                .filter(|(_, level)| **level <= lowest)
                .min_by_key(|(index, level)| (**level, *index))?;
            let level = queue.remove(index);
            Some((vec![level as u8; 8], level))
        }
    }

    #[test]
    fn refused_bulk_frames_never_hold_back_replies_or_pings() {
        let (bell, _) = UnixStream::pair().unwrap();
        let mut carrier = BulkBlocked {
            accepted: Vec::new(),
            bell,
        };
        let mut held = Vec::new();
        // Several bulk frames queued ahead of a reply and a ping.
        let mut next = queued(&[
            Priority::L2,
            Priority::L2,
            Priority::L2,
            Priority::L1,
            Priority::L0,
        ]);
        // The first turn takes a bulk frame before the others arrive.
        let mut first = Some((vec![2; 8], Priority::L2));
        let written = write(&mut carrier, &mut held, |lowest| {
            first.take().or_else(|| next(lowest))
        })
        .unwrap();
        assert_eq!(carrier.accepted, [Priority::L0, Priority::L1]);
        assert_eq!(held.len(), 1, "at most one refused bulk frame is held");
        assert_eq!(held[0].1, Priority::L2);
        assert!(written.pending);
    }

    #[test]
    fn a_draining_link_without_handles_waits_instead_of_spinning() {
        let (carrier, flushes) = Backpressured::new(true);
        let link = spawn_link(Box::new(carrier), Arc::new(|_| {})).unwrap();
        drop(link);
        flushes
            .recv_timeout(Duration::from_secs(10))
            .expect("the drain flushes when it starts");
        // Then it flushes only when woken, until the stall deadline `STALL_AFTER` away. The one
        // wake left is the last handle's, which may arrive after the drain started. A spinning
        // drain flushes again at once.
        let again = std::iter::from_fn(|| flushes.recv_timeout(Duration::from_millis(200)).ok())
            .take(2)
            .count();
        assert!(again <= 1, "the drain flushed without a wake");
    }

    /// A peer that sends without pause: acknowledgements forever, which need no reply.
    struct Flood {
        frame: Vec<u8>,
        sent: usize,
        dispatched: Arc<AtomicUsize>,
        bell: UnixStream,
    }

    impl Carrier for Flood {
        fn offer(&mut self, _: &[u8], _: Priority) -> Result<Offer, CarrierError> {
            Ok(Offer::Accepted)
        }
        fn flush(&mut self) -> Result<Flush, CarrierError> {
            Ok(Flush {
                progressed: false,
                pending: false,
            })
        }
        fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
            if self.sent == 4 * TURN_FRAMES {
                return Err(CarrierError::Closed);
            }
            // Every turn dispatches before it reads further.
            assert!(self.sent - self.dispatched.load(Ordering::SeqCst) <= TURN_FRAMES);
            self.sent += 1;
            Ok(Some(RawFrame::parse(&self.frame).unwrap()))
        }
        fn readiness(&self) -> Readiness {
            Readiness {
                read: self.bell.as_raw_fd(),
                write: None,
                progress: None,
                deadline: None,
                signal: None,
            }
        }
        fn close(&mut self) {}
    }

    struct Counting(Arc<AtomicUsize>);
    impl super::super::service::PaneService for Counting {
        fn admitted(
            &mut self,
            _: &super::super::service::Peer,
            _: crate::pane::control::ClientName,
            _: super::super::outbox::Outbox,
        ) {
        }
        fn request(
            &mut self,
            _: &super::super::service::Peer,
            _: crate::protocol::RequestId,
            _: crate::protocol::Operation,
        ) -> super::super::service::Dispatch {
            super::super::service::Dispatch::reply(Err(crate::protocol::Refusal::Unsupported))
        }
        fn input(&mut self, _: &super::super::service::Peer, _: crate::protocol::Input) {}
        fn view(&mut self, _: &super::super::service::Peer, _: crate::protocol::View) {}
        fn effect_done(&mut self, _: &super::super::service::Peer, _: crate::protocol::EffectDone) {
        }
        fn acknowledge(&mut self, _: &super::super::service::Peer, _: u64) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn disconnected(&mut self, _: &super::super::service::Peer) {}
        fn shutdown(&mut self) -> Result<(), crate::CoreError> {
            Ok(())
        }
    }

    #[test]
    fn a_peer_that_never_pauses_is_served_in_bounded_turns() {
        use crate::protocol::{Acknowledge, ClientMessage, frame::encode_client};
        let dispatched = Arc::new(AtomicUsize::new(0));
        let frame = encode_client(&ClientMessage::Acknowledge(Acknowledge { sequence: 1 }))
            .unwrap()
            .frames
            .remove(0);
        let (bell, _) = UnixStream::pair().unwrap();
        let carrier = Flood {
            frame,
            sent: 0,
            dispatched: Arc::clone(&dispatched),
            bell,
        };
        let service: SharedPaneService = Arc::new(std::sync::Mutex::new(Box::new(Counting(
            Arc::clone(&dispatched),
        ))));
        let (waker, wake) = Waker::new().unwrap();
        let (sync, _) = super::super::sync::SyncWorker::spawn().unwrap();
        let connection = AdmittedConnection::new(
            super::super::service::Peer {
                client: [1; 32],
                connection: [2; 32],
                pairing: None,
            },
            super::super::outbox::Outbox::new(wake, crate::protocol::version::WORK),
            sync,
            None,
            Instant::now(),
        );
        serve(
            Box::new(carrier),
            connection,
            &waker,
            &service,
            &AtomicBool::new(false),
        );
        assert_eq!(dispatched.load(Ordering::SeqCst), 4 * TURN_FRAMES);
    }
}
