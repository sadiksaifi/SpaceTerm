//! Account-local capacity proof without changing the frozen admission messages.
#[cfg(feature = "client")]
use super::Paths;
use super::{Directory, MAX_LOCAL_CONNECTIONS, Running, UnixStream};
#[cfg(feature = "client")]
use crate::CoreError;
use crate::Lock;
use std::os::fd::AsRawFd;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

pub(super) const MARKER: &str = "admission-capacity.lock";

pub(super) struct Publisher<'a> {
    runtime: &'a Directory,
    full: Option<Lock>,
    reported: bool,
}

impl<'a> Publisher<'a> {
    pub(super) fn new(runtime: &'a Directory) -> Self {
        Self {
            runtime,
            full: None,
            reported: false,
        }
    }

    pub(super) fn update(&mut self, connections: &[Running]) {
        if connections.len() < MAX_LOCAL_CONNECTIONS {
            self.full = None;
            self.reported = false;
            return;
        }
        if self.full.is_none() {
            self.full = self.runtime.try_lock(MARKER).ok().flatten();
        }
        if !self.reported {
            let mut phases = [0_u8; 3];
            let mut processes = std::collections::BTreeSet::new();
            let mut unknown = 0_u8;
            for connection in connections.iter().filter_map(|c| c.local.as_ref()) {
                phases[usize::from(connection.phase.load(Ordering::Acquire)).min(2)] += 1;
                if let Some(process) = connection.process {
                    processes.insert(process);
                } else {
                    unknown += 1;
                }
            }
            eprintln!(
                "spaceterm_local_admission_capacity held={} admitting={} work={} service={} processes={} unknown_peer_connections={} signal={}",
                connections.len().min(MAX_LOCAL_CONNECTIONS),
                phases[0],
                phases[1],
                phases[2],
                processes.len(),
                unknown,
                u8::from(self.full.is_some())
            );
            self.reported = true;
        }
    }
}

pub(super) struct LocalConnection {
    process: Option<u32>,
    pub(super) phase: Arc<AtomicU8>,
}

impl LocalConnection {
    pub(super) fn new(stream: &UnixStream) -> Self {
        Self {
            process: peer_process(stream),
            phase: Arc::default(),
        }
    }
}

// The PID is used only to count distinct local processes, never retained or emitted.
fn peer_process(stream: &UnixStream) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
        // SAFETY: the output pointer and length describe the retained local PID buffer.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                0,
                libc::LOCAL_PEERPID,
                (&raw mut pid).cast(),
                &mut length,
            )
        };
        (result == 0 && length as usize == std::mem::size_of_val(&pid))
            .then(|| u32::try_from(pid).ok())
            .flatten()
    }
    #[cfg(target_os = "linux")]
    {
        let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&peer) as libc::socklen_t;
        // SAFETY: the output pointer and length describe the retained credential buffer.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut peer).cast(),
                &mut length,
            )
        };
        (result == 0 && length as usize == std::mem::size_of_val(&peer))
            .then(|| u32::try_from(peer.pid).ok())
            .flatten()
    }
}

#[cfg(feature = "client")]
pub(crate) fn full(paths: &Paths) -> Result<bool, CoreError> {
    let runtime = match Directory::open(&paths.runtime, false) {
        Ok(runtime) => runtime,
        Err(CoreError::NotSetUp) => return Ok(false),
        Err(error) => return Err(error),
    };
    runtime.verify_path(&paths.runtime)?;
    runtime.existing_lock_held(MARKER)
}
