//! The local service's changes through inotify on its directories and a pidfd for its server's
//! process. The watch thread sleeps in `poll` between changes.
use crate::status_menu::service_changes::{Fired, Watch};
use std::ffi::CString;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;

struct InotifyWatch {
    inotify: OwnedFd,
    /// Each watch descriptor with the index of its directory.
    directories: Vec<(i32, usize)>,
    process: Option<OwnedFd>,
    /// A directory that went away before its watch was registered, reported at once.
    missing: Option<usize>,
}

const CHANGES: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;

pub(crate) fn start(directories: &[PathBuf], process: Option<u32>) -> Option<Box<dyn Watch>> {
    // SAFETY: plain syscall with constant flags; a valid result is a new descriptor.
    let inotify = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if inotify < 0 {
        return None;
    }
    // SAFETY: a non-negative result is a descriptor this function now owns.
    let inotify = unsafe { OwnedFd::from_raw_fd(inotify) };
    let mut watch = InotifyWatch {
        inotify,
        directories: Vec::new(),
        process: None,
        missing: None,
    };
    for (index, directory) in directories.iter().enumerate() {
        let Ok(path) = CString::new(directory.as_os_str().as_bytes()) else {
            continue;
        };
        // SAFETY: adds a watch for the terminated path to the live descriptor.
        let descriptor =
            unsafe { libc::inotify_add_watch(watch.inotify.as_raw_fd(), path.as_ptr(), CHANGES) };
        if descriptor < 0 {
            watch.missing.get_or_insert(index);
            continue;
        }
        watch.directories.push((descriptor, index));
    }
    if let Some(process) = process {
        // SAFETY: plain syscall; a valid result is a new descriptor for the process.
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, process as libc::pid_t, 0) };
        // A process that already exited, or a kernel without pidfds, leaves it unwatched.
        if descriptor >= 0 {
            // SAFETY: a non-negative result is a descriptor this function now owns.
            watch.process = Some(unsafe { OwnedFd::from_raw_fd(descriptor as i32) });
        }
    }
    Some(Box::new(watch))
}

impl InotifyWatch {
    /// The directory of the first event read, or `None` when reading failed.
    fn read_directory(&self) -> Option<Option<usize>> {
        let mut buffer = [0u8; 4096];
        // SAFETY: reads at most the buffer's length into the live local buffer.
        let read = unsafe {
            libc::read(
                self.inotify.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if read < 0 {
            return (std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted)
                .then_some(None);
        }
        let header = std::mem::size_of::<libc::inotify_event>();
        if (read as usize) < header {
            return Some(None);
        }
        // SAFETY: the kernel wrote at least one whole event header at the buffer's start.
        let event: libc::inotify_event =
            unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast()) };
        Some(
            self.directories
                .iter()
                .find(|(descriptor, _)| *descriptor == event.wd)
                .map(|(_, index)| *index),
        )
    }
}

impl Watch for InotifyWatch {
    fn wait(&mut self) -> Option<Fired> {
        if let Some(index) = self.missing.take() {
            return Some(Fired::Directory(index));
        }
        loop {
            let mut descriptors = [
                libc::pollfd {
                    fd: self.inotify.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self
                        .process
                        .as_ref()
                        .map_or(-1, |process| process.as_raw_fd()),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: polls the live local descriptors with no timeout; a negative one is ignored.
            let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return None;
            }
            if descriptors[1].revents != 0 {
                return Some(Fired::Process);
            }
            if descriptors[0].revents != 0 {
                match self.read_directory()? {
                    Some(index) => return Some(Fired::Directory(index)),
                    None => continue,
                }
            }
        }
    }
}
