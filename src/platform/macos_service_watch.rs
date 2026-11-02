//! The local service's changes through kqueue: vnode events on its directories and the exit of
//! its server's process. The watch thread sleeps in `kevent` between changes.
use crate::status_menu::service_changes::{Fired, Watch};
use std::ffi::CString;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;

struct KqueueWatch {
    queue: OwnedFd,
    /// Kept open so their events stay registered.
    _directories: Vec<OwnedFd>,
    /// A directory that went away before its watch was registered, reported at once.
    missing: Option<usize>,
}

pub(crate) fn start(directories: &[PathBuf], process: Option<u32>) -> Option<Box<dyn Watch>> {
    // SAFETY: `kqueue` takes no arguments and returns a new descriptor or -1.
    let queue = unsafe { libc::kqueue() };
    if queue < 0 {
        return None;
    }
    // SAFETY: a non-negative result is a descriptor this function now owns.
    let queue = unsafe { OwnedFd::from_raw_fd(queue) };
    let mut watch = KqueueWatch {
        queue,
        _directories: Vec::new(),
        missing: None,
    };
    for (index, directory) in directories.iter().enumerate() {
        let Ok(path) = CString::new(directory.as_os_str().as_bytes()) else {
            continue;
        };
        // SAFETY: opens the terminated path for events only.
        let descriptor = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_EVTONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            watch.missing.get_or_insert(index);
            continue;
        }
        // SAFETY: a non-negative result is a descriptor this function now owns.
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let event = libc::kevent {
            ident: descriptor.as_raw_fd() as usize,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE,
            data: 0,
            udata: index as *mut libc::c_void,
        };
        if !register(&watch.queue, &event) {
            return None;
        }
        watch._directories.push(descriptor);
    }
    if let Some(process) = process {
        let event = libc::kevent {
            ident: process as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // A process that already exited cannot be registered, and is not watched.
        register(&watch.queue, &event);
    }
    Some(Box::new(watch))
}

fn register(queue: &OwnedFd, event: &libc::kevent) -> bool {
    // SAFETY: registers one event on the live queue; no events are returned.
    unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        ) == 0
    }
}

impl Watch for KqueueWatch {
    fn wait(&mut self) -> Option<Fired> {
        if let Some(index) = self.missing.take() {
            return Some(Fired::Directory(index));
        }
        loop {
            // SAFETY: a zeroed kevent is a valid output buffer.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            // SAFETY: waits on the live queue for one event into the local buffer, with no timeout.
            let received = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    std::ptr::null(),
                )
            };
            if received < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return None;
            }
            if received == 0 {
                continue;
            }
            return Some(if event.filter == libc::EVFILT_PROC {
                Fired::Process
            } else {
                Fired::Directory(event.udata as usize)
            });
        }
    }
}
