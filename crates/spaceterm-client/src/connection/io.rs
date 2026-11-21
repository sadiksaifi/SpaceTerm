//! One I/O thread per connection.
//!
//! The thread waits in `poll` on the carrier's descriptors and on a wake pipe that other threads
//! ring after they queue work. It never sleeps or polls on a timer: its only timeout is the next
//! health deadline.
use super::Wake;
use super::carrier::{Carrier, CarrierError, Offer, Priority};
use super::link::{CoreLink, LinkError, LinkSink, WeakLink};
use crate::protocol::limits;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
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
/// `false` at the deadline. Used by blocking preambles such as the QUIC handshake.
pub fn wait_fd(fd: RawFd, write: bool, deadline: Instant) -> bool {
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
pub fn wait(
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
    if writing && let Some(fd) = readiness.progress {
        descriptors.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    if let Some(fd) = readiness.signal {
        descriptors.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    let deadline = [deadline, readiness.deadline].into_iter().flatten().min();
    let _ = poll(&mut descriptors, deadline);
    waker.drain();
}

/// Frames taken from the outbox or link but not yet accepted by the carrier, highest priority
/// first and in order within a level.
pub type Held = Vec<(Vec<u8>, Priority)>;

pub struct Written {
    pub progressed: bool,
    /// Bytes or frames still wait, in the carrier or in `held`.
    pub pending: bool,
    /// The carrier has buffered bytes, so the thread waits for the write descriptor.
    pub buffered: bool,
}

/// Offers frames from `next` until the carrier is full or nothing is left, flushing as it goes.
///
/// A carrier may refuse one level and accept another. While a bulk (L2) frame is refused, the
/// thread holds at most that one and takes only frames above L2 from `next`, so a bulk transfer
/// waiting on flow control never holds back a reply or a ping. `next` takes frames down to the
/// level it is given. A frame never overtakes a refused frame of its own level.
pub fn write(
    carrier: &mut dyn Carrier,
    held: &mut Held,
    mut next: impl FnMut(Priority) -> Option<(Vec<u8>, Priority)>,
) -> Result<Written, CarrierError> {
    let mut progressed = false;
    // Whether the held frames were already offered again after a flush that freed no room.
    let mut retried = false;
    loop {
        let mut refused = false;
        let mut index = 0;
        while index < held.len() {
            let level = held[index].1;
            if held[..index].iter().any(|(_, earlier)| *earlier == level) {
                index += 1;
                continue;
            }
            match carrier.offer(&held[index].0, level)? {
                Offer::Accepted => {
                    held.remove(index);
                    retried = false;
                }
                Offer::Full => {
                    refused = true;
                    index += 1;
                }
            }
        }
        // Take more while nothing above bulk is refused; a refused bulk frame allows only
        // frames above it.
        let lowest = if held.iter().any(|(_, level)| *level == Priority::L2) {
            Priority::L1
        } else {
            Priority::L2
        };
        if held.iter().all(|(_, level)| *level == Priority::L2)
            && let Some(frame) = next(lowest)
        {
            held.push(frame);
            // Stable, so frames of one level keep their order.
            held.sort_by_key(|(_, level)| *level);
            continue;
        }
        if !refused {
            break;
        }
        let flush = carrier.flush()?;
        progressed |= flush.progressed;
        // Bytes written outside `flush`, for example while `receive` answered the peer, can
        // leave the carrier empty without this flush writing anything. Offer once more before
        // waiting: an empty carrier has no write readiness to wake on.
        if flush.progressed || (!flush.pending && !retried) {
            retried = !flush.progressed;
            continue;
        }
        break;
    }
    let flush = carrier.flush()?;
    progressed |= flush.progressed;
    Ok(Written {
        progressed,
        pending: flush.pending || !held.is_empty(),
        buffered: flush.pending,
    })
}

/// After `close`, flushes until the carrier is done or stalls, so a goodbye and the end of every
/// stream leave the socket.
pub fn drain(carrier: &mut dyn Carrier, waker: &Waker) {
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
/// when the link fails, is closed, or every [`CoreLink`] handle is dropped.
pub fn spawn_link(
    carrier: Box<dyn Carrier>,
    events: Arc<dyn LinkSink>,
) -> std::io::Result<CoreLink> {
    let (waker, wake) = Waker::new()?;
    let link = CoreLink::new(events, wake, Instant::now());
    let weak = link.downgrade();
    std::thread::Builder::new()
        .name("spaceterm-link".into())
        .spawn(move || run_link(carrier, &weak, &waker))?;
    Ok(link)
}

fn run_link(mut carrier: Box<dyn Carrier>, weak: &WeakLink, waker: &Waker) {
    let mut held = Vec::new();
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
        // An unanswered ping may mean the network changed under a still-valid address.
        if link.suspect(now) {
            carrier.path_suspect();
        }
        drop(link);
        wait(carrier.as_ref(), (true, buffered), waker, Some(deadline));
    }
    drain(carrier.as_mut(), waker);
}

/// One turn of the client loop. Returns whether the carrier holds buffered bytes.
fn turn(
    carrier: &mut dyn Carrier,
    link: &CoreLink,
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
    // A client writes no bulk, so every frame qualifies.
    let written = write(carrier, held, |_| link.next_frame()).map_err(link_error)?;
    link.wrote(now, written.progressed, written.pending);
    Ok(written.buffered)
}

fn link_error(error: CarrierError) -> LinkError {
    match error {
        CarrierError::Closed | CarrierError::Failed => LinkError::Closed,
        CarrierError::Decode(_) => LinkError::Violation,
    }
}
