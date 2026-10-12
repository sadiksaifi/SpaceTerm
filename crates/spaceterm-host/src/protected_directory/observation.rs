//! Operation-owned observation of covered native mutations, not an immutability proof.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};

use super::ProtectedDirectoryError;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;

// Leave one enrollment for the regular file. Each enrollment owns one duplicate;
// the backend owns one queue, and Linux also retains one trusted proc directory.
pub(super) const MAX_HANDLES: usize = 256;
pub(super) const MAX_DIRECTORY_HANDLES: usize = MAX_HANDLES - 1;
const MAX_NATIVE_CALLS: usize = 4096;
const MAX_INTERRUPTS: usize = 16;
const MAX_DRAIN_CALLS: usize = 64;
const MAX_EVENTS: usize = 4096;

pub(super) struct Observation {
    // Drop registrations before the descriptors they select.
    backend: platform::Observation,
    handles: Vec<File>,
    work: WorkBudget,
}

impl Observation {
    pub(super) fn begin() -> Result<Self, ProtectedDirectoryError> {
        let mut work = WorkBudget::new();
        let mut handles = Vec::new();
        handles
            .try_reserve_exact(MAX_HANDLES)
            .map_err(|_| ProtectedDirectoryError::Unavailable)?;
        let backend = platform::Observation::new(&mut work)?;
        Ok(Self {
            backend,
            handles,
            work,
        })
    }

    pub(super) fn enroll(&mut self, file: &File) -> Result<(), ProtectedDirectoryError> {
        if self.handles.len() == MAX_HANDLES {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        let descriptor = self.work.retry(|| {
            // SAFETY: file owns a live descriptor. Duplication does not resolve a
            // pathname, and CLOEXEC applies to the new descriptor atomically.
            let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            if descriptor < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(descriptor)
            }
        })?;
        // SAFETY: fcntl returned a new descriptor whose ownership transfers here.
        let held = unsafe { File::from_raw_fd(descriptor) };
        self.handles.push(held);
        self.backend
            .enroll(&self.handles[self.handles.len() - 1], &mut self.work)
    }

    pub(super) fn finish(mut self) -> Result<(), ProtectedDirectoryError> {
        // No baseline or event state is reset between inspection rounds. A clean
        // drain is an observation only, not a barrier against concurrent writers.
        self.backend.finish(&mut self.work)
    }
}

struct WorkBudget {
    calls: usize,
    interrupts: usize,
}

impl WorkBudget {
    fn new() -> Self {
        Self {
            calls: 0,
            interrupts: 0,
        }
    }

    fn step(&mut self) -> Result<(), ProtectedDirectoryError> {
        if self.calls == MAX_NATIVE_CALLS {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        self.calls += 1;
        Ok(())
    }

    fn call<T>(
        &mut self,
        operation: impl FnOnce() -> io::Result<T>,
    ) -> Result<T, ProtectedDirectoryError> {
        self.step()?;
        operation().map_err(classify)
    }

    fn retry<T>(
        &mut self,
        mut operation: impl FnMut() -> io::Result<T>,
    ) -> Result<T, ProtectedDirectoryError> {
        loop {
            self.step()?;
            match operation() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    if self.interrupts == MAX_INTERRUPTS {
                        return Err(ProtectedDirectoryError::Unavailable);
                    }
                    self.interrupts += 1;
                }
                result => return result.map_err(classify),
            }
        }
    }
}

fn classify(error: io::Error) -> ProtectedDirectoryError {
    match error.raw_os_error() {
        Some(error) if [libc::ENOSYS, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&error) => {
            ProtectedDirectoryError::NativeUnavailable
        }
        _ => ProtectedDirectoryError::Unavailable,
    }
}

#[cfg(test)]
pub(super) mod testing {
    use std::cell::Cell;
    use std::io;

    // Faults only poison an observation. They cannot synthesize a clean interval
    // or replace real identities, protection inspection, or filesystem I/O.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(in crate::protected_directory) enum Fault {
        QueueCreation,
        Enrollment,
        UnsupportedEnrollment,
        QueueRead,
        WatchLoss,
        MalformedRecord,
        UnboundedDrain,
        InterruptedDrain,
        #[cfg(target_os = "linux")]
        QueueOverflow,
    }

    std::thread_local! {
        static FAULT: Cell<Option<Fault>> = const { Cell::new(None) };
    }

    pub(in crate::protected_directory) struct FaultScope;

    impl FaultScope {
        pub(in crate::protected_directory) fn begin(fault: Fault) -> Self {
            FAULT.with(|current| {
                assert!(
                    current.replace(Some(fault)).is_none(),
                    "observation fault is already armed"
                );
            });
            Self
        }
    }

    impl Drop for FaultScope {
        fn drop(&mut self) {
            FAULT.with(|current| current.set(None));
        }
    }

    pub(in crate::protected_directory) fn fault() -> Option<Fault> {
        FAULT.with(Cell::get)
    }

    pub(in crate::protected_directory) fn take_fault() -> Option<Fault> {
        FAULT.with(Cell::take)
    }

    pub(in crate::protected_directory) fn creation() -> io::Result<()> {
        if fault() == Some(Fault::QueueCreation) {
            Err(io::Error::from_raw_os_error(libc::EMFILE))
        } else {
            Ok(())
        }
    }

    pub(in crate::protected_directory) fn enrollment() -> io::Result<()> {
        match fault() {
            Some(Fault::Enrollment) => Err(io::Error::from_raw_os_error(libc::ENOSPC)),
            Some(Fault::UnsupportedEnrollment) => {
                Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
            }
            _ => Ok(()),
        }
    }
}
