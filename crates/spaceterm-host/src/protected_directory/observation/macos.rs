use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};

use super::{MAX_DRAIN_CALLS, MAX_EVENTS, MAX_HANDLES, WorkBudget};
use crate::protected_directory::ProtectedDirectoryError;

const NOTES: u32 = libc::NOTE_DELETE
    | libc::NOTE_WRITE
    | libc::NOTE_EXTEND
    | libc::NOTE_ATTRIB
    | libc::NOTE_LINK
    | libc::NOTE_RENAME
    | libc::NOTE_REVOKE;
const BATCH_EVENTS: usize = 32;

pub(super) struct Observation {
    queue: File,
    descriptors: Vec<usize>,
    changed: bool,
}

impl Observation {
    pub(super) fn new(work: &mut WorkBudget) -> Result<Self, ProtectedDirectoryError> {
        let mut descriptors = Vec::new();
        descriptors
            .try_reserve_exact(MAX_HANDLES)
            .map_err(|_| ProtectedDirectoryError::Unavailable)?;
        let descriptor = work.call(|| {
            #[cfg(test)]
            super::testing::creation()?;
            // SAFETY: kqueue takes no pointers and returns a new owned descriptor.
            let descriptor = unsafe { libc::kqueue() };
            if descriptor < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(descriptor)
            }
        })?;
        // SAFETY: the successful kqueue returned a new descriptor owned here.
        let queue = unsafe { File::from_raw_fd(descriptor) };
        work.call(|| {
            // SAFETY: queue owns this descriptor. No other descriptor flags are needed.
            if unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })?;
        Ok(Self {
            queue,
            descriptors,
            changed: false,
        })
    }

    pub(super) fn enroll(
        &mut self,
        held: &File,
        work: &mut WorkBudget,
    ) -> Result<(), ProtectedDirectoryError> {
        if self.descriptors.len() == MAX_HANDLES {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        let ident =
            usize::try_from(held.as_raw_fd()).map_err(|_| ProtectedDirectoryError::Unavailable)?;
        let change = libc::kevent {
            ident,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR | libc::EV_RECEIPT,
            fflags: NOTES,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let mut receipt = empty_event();
        work.call(|| {
            #[cfg(test)]
            super::testing::enrollment()?;
            // SAFETY: queue and held remain owned. Both event pointers refer to
            // live storage for one event. EV_RECEIPT returns the registration
            // result without draining or clearing earlier pending vnode notes.
            let count = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    &change,
                    1,
                    &mut receipt,
                    1,
                    &ZERO_TIMEOUT,
                )
            };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count != 1
                || receipt.ident != ident
                || receipt.filter != libc::EVFILT_VNODE
                || receipt.flags & libc::EV_ERROR == 0
            {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            if receipt.data != 0 {
                let error = i32::try_from(receipt.data)
                    .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
                return Err(io::Error::from_raw_os_error(error));
            }
            Ok(())
        })?;
        // XNU binds EVFILT_VNODE to this descriptor's vnode. The outer guard
        // retains the duplicate until after queue teardown, without path reopening.
        self.descriptors.push(ident);
        Ok(())
    }

    pub(super) fn finish(&mut self, work: &mut WorkBudget) -> Result<(), ProtectedDirectoryError> {
        let mut events = [empty_event(); BATCH_EVENTS];
        let mut observed = 0;
        for _ in 0..MAX_DRAIN_CALLS {
            let count = work.retry(|| self.read_batch(&mut events))?;
            if count == 0 {
                return if self.changed {
                    Err(ProtectedDirectoryError::Changed)
                } else {
                    Ok(())
                };
            }
            if count > events.len() || count > MAX_EVENTS - observed {
                return Err(ProtectedDirectoryError::Unavailable);
            }
            observed += count;
            for event in &events[..count] {
                // kevent is packed on Darwin. Copy fields before borrowing values.
                // XNU preserves EV_RECEIPT from registration in delivered vnode events.
                let ident = event.ident;
                let flags = event.flags;
                let notes = event.fflags;
                if event.filter != libc::EVFILT_VNODE
                    || !self.descriptors.contains(&ident)
                    || flags
                        & !(libc::EV_ADD
                            | libc::EV_ENABLE
                            | libc::EV_CLEAR
                            | libc::EV_RECEIPT
                            | libc::EV_EOF
                            | libc::EV_ERROR)
                        != 0
                    || flags & libc::EV_ERROR != 0
                    || notes & !NOTES != 0
                    || (notes == 0 && flags & libc::EV_EOF == 0)
                    || event.data != 0
                    || !event.udata.is_null()
                {
                    return Err(ProtectedDirectoryError::Unavailable);
                }
                self.changed = true;
            }
        }
        Err(ProtectedDirectoryError::Unavailable)
    }

    fn read_batch(&self, events: &mut [libc::kevent; BATCH_EVENTS]) -> io::Result<usize> {
        #[cfg(test)]
        if let Some(count) = self.inject_read(events)? {
            return Ok(count);
        }
        // SAFETY: queue is owned and the output array has BATCH_EVENTS writable
        // slots. No changes are submitted. A zero timeout never waits for an event.
        let count = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                BATCH_EVENTS as libc::c_int,
                &ZERO_TIMEOUT,
            )
        };
        if count < 0 {
            Err(io::Error::last_os_error())
        } else {
            usize::try_from(count).map_err(|_| io::Error::from_raw_os_error(libc::EIO))
        }
    }

    #[cfg(test)]
    fn inject_read(&self, events: &mut [libc::kevent; BATCH_EVENTS]) -> io::Result<Option<usize>> {
        use super::testing::{Fault, fault};

        let Some(fault) = fault() else {
            return Ok(None);
        };
        let notes = match fault {
            Fault::QueueRead => return Err(io::Error::from_raw_os_error(libc::EIO)),
            Fault::InterruptedDrain => return Err(io::Error::from_raw_os_error(libc::EINTR)),
            Fault::WatchLoss => libc::NOTE_REVOKE,
            Fault::UnboundedDrain => libc::NOTE_ATTRIB,
            Fault::MalformedRecord => libc::NOTE_ATTRIB,
            _ => return Ok(None),
        };
        events[0] = libc::kevent {
            ident: self.descriptors[0],
            filter: if fault == Fault::MalformedRecord {
                libc::EVFILT_READ
            } else {
                libc::EVFILT_VNODE
            },
            flags: if fault == Fault::WatchLoss {
                libc::EV_CLEAR | libc::EV_EOF
            } else {
                libc::EV_CLEAR
            },
            fflags: notes,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // Loss is supplied once, then the real queue is drained. Its changed bit
        // must remain sticky. The endless stream exercises the real drain budget.
        if fault != Fault::UnboundedDrain {
            let _ = super::testing::take_fault();
        }
        Ok(Some(1))
    }
}

const ZERO_TIMEOUT: libc::timespec = libc::timespec {
    tv_sec: 0,
    tv_nsec: 0,
};

fn empty_event() -> libc::kevent {
    libc::kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

// XNU posts these vnode notes on covered successful setattr/xattr, namespace and
// ordinary-write paths. Failed partial mutations need not notify. Writable-map
// stores can precede pageout notification. Neither coalescing nor a clean queue
// supplies a transaction, continuous ACL-safety proof, or concurrent-writer barrier.
