//! One I/O thread per connection.
//!
//! The thread waits in `poll` on the carrier's descriptors and on a wake pipe that other threads
//! ring after they queue work. It never sleeps or polls on a timer: its only timeout is the next
//! health deadline. Each turn follows the rules in [`super::carrier`]: receive until the carrier
//! has nothing, offer frames in priority order until it is full, flush, record the write attempt,
//! then check health.
use super::Wake;
use super::carrier::{Carrier, CarrierError, Offer, Priority};
use super::link::{LinkError, LinkSink, ServerLink, WeakLink};
use super::server::{AdmittedConnection, Inbound};
use super::service::SharedPaneService;
use crate::protocol::{Fatal, limits};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// The read end of a connection's wake pipe. [`Waker::new`] also returns the [`Wake`] handle that
/// rings it.
pub struct Waker {
    bell: UnixStream,
    /// Keeps the pipe's write end open after every other handle dropped, so the bell never reads
    /// as closed and a draining thread does not wake in a loop.
    _ring: Arc<Ring>,
}

struct Ring(UnixStream);

impl Wake for Ring {
    fn wake(&self) {
        // A full pipe already wakes the thread, so a refused byte is harmless.
        let _ = (&self.0).write(&[1]);
    }
}

impl Waker {
    pub fn new() -> std::io::Result<(Self, Arc<dyn Wake>)> {
        let (bell, ring) = UnixStream::pair()?;
        bell.set_nonblocking(true)?;
        ring.set_nonblocking(true)?;
        let ring = Arc::new(Ring(ring));
        Ok((
            Self {
                bell,
                _ring: Arc::clone(&ring),
            },
            ring,
        ))
    }

    fn descriptor(&self) -> RawFd {
        self.bell.as_raw_fd()
    }

    fn drain(&self) {
        let mut bytes = [0; 64];
        while matches!((&self.bell).read(&mut bytes), Ok(count) if count > 0) {}
    }
}

/// Waits until `fd` is readable, or writable when `write` is set, or `deadline` passes. Returns
/// `false` at the deadline. Used by blocking preambles such as TLS and WebSocket handshakes.
pub(crate) fn wait_fd(fd: RawFd, write: bool, deadline: Instant) -> bool {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let events = if write { libc::POLLOUT } else { libc::POLLIN };
        let mut descriptors = [libc::pollfd {
            fd,
            events,
            revents: 0,
        }];
        match poll(&mut descriptors, Some(deadline)) {
            Ok(0) => continue,
            Ok(_) => return true,
            Err(()) => return false,
        }
    }
}

/// `poll(2)` until `deadline`, or without a timeout. An interrupted call reports no events.
fn poll(descriptors: &mut [libc::pollfd], deadline: Option<Instant>) -> Result<usize, ()> {
    let timeout = deadline.map_or(-1, |deadline| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Round up, so the thread never wakes just before its deadline and spins.
        let millis = remaining.as_micros().div_ceil(1000);
        millis.min(i32::MAX as u128) as i32
    });
    // SAFETY: `descriptors` is a live, writable array of `pollfd` for the whole call.
    let result = unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as libc::nfds_t,
            timeout,
        )
    };
    if result >= 0 {
        return Ok(result as usize);
    }
    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
        Ok(0)
    } else {
        Err(())
    }
}

/// Waits for the carrier, the wake pipe or the deadline. A connection that stopped reading
/// leaves out the read descriptor, which a closed socket would keep reporting.
fn wait(
    carrier: &dyn Carrier,
    (reading, writing): (bool, bool),
    waker: &Waker,
    deadline: Option<Instant>,
) {
    let readiness = carrier.readiness();
    let mut descriptors = vec![libc::pollfd {
        fd: waker.descriptor(),
        events: libc::POLLIN,
        revents: 0,
    }];
    if reading {
        descriptors.push(libc::pollfd {
            fd: readiness.read,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    if writing && let Some(fd) = readiness.write {
        descriptors.push(libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        });
    }
    let _ = poll(&mut descriptors, deadline);
    waker.drain();
}

/// The frame taken from the outbox or link but not yet accepted by the carrier.
type Held = Option<(Vec<u8>, Priority)>;

struct Written {
    progressed: bool,
    /// Bytes or frames still wait, in the carrier or in `held`.
    pending: bool,
    /// The carrier has buffered bytes, so the thread waits for the write descriptor.
    buffered: bool,
}

/// Offers frames from `next` until the carrier is full or nothing is left, flushing as it goes.
fn write(
    carrier: &mut dyn Carrier,
    held: &mut Held,
    mut next: impl FnMut() -> Held,
) -> Result<Written, CarrierError> {
    let mut progressed = false;
    // Whether the held frame was already offered again after a flush that freed no room.
    let mut retried = false;
    loop {
        if held.is_none() {
            *held = next();
        }
        let Some((frame, priority)) = held.as_ref() else {
            break;
        };
        match carrier.offer(frame, *priority)? {
            Offer::Accepted => {
                *held = None;
                retried = false;
            }
            Offer::Full => {
                let flush = carrier.flush()?;
                progressed |= flush.progressed;
                // Bytes written outside `flush`, for example while `receive` answered the peer,
                // can leave the carrier empty without this flush writing anything. Offer once
                // more before waiting: an empty carrier has no write readiness to wake on.
                if flush.progressed || (!flush.pending && !retried) {
                    retried = !flush.progressed;
                    continue;
                }
                break;
            }
        }
    }
    let flush = carrier.flush()?;
    progressed |= flush.progressed;
    Ok(Written {
        progressed,
        pending: flush.pending || held.is_some(),
        buffered: flush.pending,
    })
}

/// After `close`, flushes until the carrier is done or stalls, so a goodbye and a WebSocket
/// Close leave the socket.
fn drain(carrier: &mut dyn Carrier, waker: &Waker) {
    carrier.close();
    let mut stalled = Instant::now() + limits::STALL_AFTER;
    loop {
        let flush = match carrier.flush() {
            Ok(flush) => flush,
            Err(_) => return,
        };
        if !flush.pending {
            return;
        }
        let now = Instant::now();
        if flush.progressed {
            stalled = now + limits::STALL_AFTER;
        } else if now >= stalled {
            return;
        }
        wait(carrier, (false, true), waker, Some(stalled));
    }
}

/// Starts a client connection over an admitted carrier and returns its link. The I/O thread ends
/// when the link fails, is closed, or every [`ServerLink`] handle is dropped.
pub fn spawn_link(
    carrier: Box<dyn Carrier>,
    events: Arc<dyn LinkSink>,
) -> std::io::Result<ServerLink> {
    let (waker, wake) = Waker::new()?;
    let link = ServerLink::new(events, wake, Instant::now());
    let weak = link.downgrade();
    std::thread::Builder::new()
        .name("spaceterm-link".into())
        .spawn(move || run_link(carrier, &weak, &waker))?;
    Ok(link)
}

fn run_link(mut carrier: Box<dyn Carrier>, weak: &WeakLink, waker: &Waker) {
    let mut held = None;
    loop {
        let Some(link) = weak.upgrade() else {
            break;
        };
        let now = Instant::now();
        let buffered = match turn(carrier.as_mut(), &link, &mut held, now) {
            Ok(buffered) => buffered,
            Err(error) => {
                link.fail(error);
                break;
            }
        };
        let Some(deadline) = link.check(now) else {
            break;
        };
        drop(link);
        wait(carrier.as_ref(), (true, buffered), waker, Some(deadline));
    }
    drain(carrier.as_mut(), waker);
}

/// One turn of the client loop. Returns whether the carrier holds buffered bytes.
fn turn(
    carrier: &mut dyn Carrier,
    link: &ServerLink,
    held: &mut Held,
    now: Instant,
) -> Result<bool, LinkError> {
    loop {
        match carrier.receive() {
            Ok(Some(frame)) => link.receive(frame, now)?,
            Ok(None) => break,
            Err(error) => return Err(link_error(error)),
        }
    }
    let written = write(carrier, held, || link.next_frame()).map_err(link_error)?;
    link.wrote(now, written.progressed, written.pending);
    Ok(written.buffered)
}

fn link_error(error: CarrierError) -> LinkError {
    match error {
        CarrierError::Closed | CarrierError::Failed => LinkError::Closed,
        CarrierError::Decode(_) => LinkError::Violation,
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
    let mut held = None;
    let mut receiving = true;
    loop {
        let now = Instant::now();
        let outbox = connection.outbox().clone();
        if stopping.load(Ordering::Acquire) {
            receiving = false;
        }
        if receiving {
            match receive(carrier.as_mut(), &mut connection, service, now) {
                Ok(()) => {}
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
        let written = match write(carrier.as_mut(), &mut held, || outbox.next_frame()) {
            Ok(written) => written,
            Err(_) => break,
        };
        connection.wrote(now, written.progressed, written.pending);
        if !written.pending && !receiving {
            if !outbox.is_closing() {
                // Stopping: every queued reply is written, so the goodbye goes last.
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
        wait(
            carrier.as_ref(),
            (receiving, written.buffered),
            waker,
            deadline,
        );
    }
    drain(carrier.as_mut(), waker);
    connection.outbox().disconnected();
    service
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .disconnected(connection.peer());
}

/// Receives until the carrier has nothing, dispatching under one lock per turn. `Err(None)` means
/// the socket closed; `Err(Some)` is the goodbye to send.
fn receive(
    carrier: &mut dyn Carrier,
    connection: &mut AdmittedConnection,
    service: &SharedPaneService,
    now: Instant,
) -> Result<(), Option<Fatal>> {
    let mut inbound: Vec<Inbound> = Vec::new();
    let mut result = Ok(());
    loop {
        match carrier.receive() {
            Ok(Some(frame)) => match connection.receive(frame, now) {
                Ok(Some(message)) => inbound.push(message),
                Ok(None) => {}
                Err(fatal) => {
                    result = Err(Some(fatal));
                    break;
                }
            },
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
    use super::*;
    use crate::connection::carrier::{Flush, Readiness};
    use crate::protocol::RawFrame;
    use std::sync::atomic::AtomicUsize;

    /// A carrier full until its next flush, which writes nothing because the backlog already left
    /// while it received, as a WebSocket answering the peer does.
    struct Backpressured {
        room: bool,
        accepted: usize,
        flushes: Arc<AtomicUsize>,
        pending: bool,
        bell: UnixStream,
    }

    impl Backpressured {
        fn new(pending: bool) -> (Self, Arc<AtomicUsize>) {
            let flushes = Arc::new(AtomicUsize::new(0));
            let (bell, _) = UnixStream::pair().unwrap();
            let carrier = Self {
                room: false,
                accepted: 0,
                flushes: Arc::clone(&flushes),
                pending,
                bell,
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
            self.flushes.fetch_add(1, Ordering::Relaxed);
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
            }
        }
        fn close(&mut self) {}
    }

    #[test]
    fn a_frame_refused_while_the_carrier_emptied_itself_is_offered_again_at_once() {
        let (mut carrier, _) = Backpressured::new(false);
        let mut held = None;
        let mut frames = vec![(vec![0; 8], Priority::L1)].into_iter();
        let written = write(&mut carrier, &mut held, || frames.next()).unwrap();
        assert!(held.is_none(), "the frame must not wait for a later wake");
        assert_eq!(carrier.accepted, 1);
        assert!(!written.pending);
    }

    #[test]
    fn a_draining_link_without_handles_waits_instead_of_spinning() {
        let (carrier, flushes) = Backpressured::new(true);
        let link = spawn_link(Box::new(carrier), Arc::new(|_| {})).unwrap();
        drop(link);
        let deadline = Instant::now() + std::time::Duration::from_millis(200);
        while Instant::now() < deadline {
            std::thread::park_timeout(deadline - Instant::now());
        }
        // The drain flushes once, then waits on its wake pipe and the stall deadline.
        assert!(flushes.load(Ordering::Relaxed) < 10);
    }
}
