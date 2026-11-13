use std::io;

use crate::platform::unix_pty::{
    MasterReadError, ProcessIdentity, ProcessObservation, ProcessStart, UnixPtyHost,
};

/// macOS process facts through libproc. A PTY master read after hangup reports end of file.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MacosPtyHost;

impl UnixPtyHost for MacosPtyHost {
    fn process_image(&self, identity: ProcessIdentity) -> io::Result<std::path::PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        if self.observe_exactly(identity.process)?.map(|p| p.identity) != Some(identity) {
            return Err(io::Error::other("process identity changed"));
        }
        let mut path = [0_u8; 4096];
        // SAFETY: proc_pidpath writes at most this buffer's length and terminates the result.
        if unsafe {
            libc::proc_pidpath(
                identity.process,
                path.as_mut_ptr().cast(),
                path.len() as u32,
            )
        } <= 0
        {
            return Err(io::Error::other("the process image is unavailable"));
        }
        let image = std::ffi::CStr::from_bytes_until_nul(&path)
            .map_err(|_| io::Error::other("unreadable process image"))?;
        if self.observe_exactly(identity.process)?.map(|p| p.identity) != Some(identity) {
            return Err(io::Error::other("process identity changed"));
        }
        Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
            image.to_bytes(),
        )))
    }

    #[allow(
        deprecated,
        reason = "libc exposes the kernel timebase through this API"
    )]
    fn cpu_time(&self, identity: ProcessIdentity) -> io::Result<std::time::Duration> {
        if self.observe_exactly(identity.process)?.map(|p| p.identity) != Some(identity) {
            return Err(io::Error::other("process identity changed"));
        }
        let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        // SAFETY: flavor 4 writes exactly proc_taskinfo into this initialized-size buffer.
        if unsafe {
            libc::proc_pidinfo(
                identity.process,
                libc::PROC_PIDTASKINFO,
                0,
                task.as_mut_ptr().cast(),
                size,
            )
        } != size
        {
            return Err(io::Error::other("CPU time is unavailable"));
        }
        // SAFETY: the complete structure was initialized by proc_pidinfo.
        let task = unsafe { task.assume_init() };
        let nanos = task
            .pti_total_user
            .checked_add(task.pti_total_system)
            .ok_or_else(|| io::Error::other("CPU time overflow"))?;
        if self.observe_exactly(identity.process)?.map(|p| p.identity) != Some(identity) {
            return Err(io::Error::other("process identity changed"));
        }
        let mut timebase = libc::mach_timebase_info_data_t { numer: 0, denom: 0 };
        // SAFETY: timebase is writable storage for the kernel conversion ratio.
        if unsafe { libc::mach_timebase_info(&mut timebase) } != 0
            || timebase.denom == 0
            || timebase.numer == 0
        {
            return Err(io::Error::other("CPU timebase is unavailable"));
        }
        let nanos = u64::try_from(
            u128::from(nanos) * u128::from(timebase.numer) / u128::from(timebase.denom),
        )
        .map_err(|_| io::Error::other("CPU time conversion overflow"))?;
        Ok(std::time::Duration::from_nanos(nanos))
    }
    fn prompt_waiting(
        &self,
        identity: ProcessIdentity,
        _slave: &std::path::Path,
        frozen: bool,
    ) -> io::Result<bool> {
        if self.observe_exactly(identity.process)?.map(|p| p.identity) != Some(identity) {
            return Err(io::Error::other("process identity changed"));
        }
        let threads = process_threads(identity.process)?;
        if threads.is_empty() {
            return Err(io::Error::other("no readable threads"));
        }
        for &thread in &threads {
            let mut info = std::mem::MaybeUninit::<libc::proc_threadinfo>::zeroed();
            let size = std::mem::size_of::<libc::proc_threadinfo>() as i32;
            // SAFETY: flavor 5 writes proc_threadinfo for this exact thread identifier.
            if unsafe {
                libc::proc_pidinfo(
                    identity.process,
                    libc::PROC_PIDTHREADINFO,
                    thread,
                    info.as_mut_ptr().cast(),
                    size,
                )
            } != size
            {
                return Err(io::Error::other("thread state is unavailable"));
            }
            // SAFETY: proc_pidinfo initialized the complete structure.
            let state = unsafe { info.assume_init() }.pth_run_state;
            if state != libc::TH_STATE_WAITING && !(frozen && state == libc::TH_STATE_STOPPED) {
                return Ok(false);
            }
        }
        if process_threads(identity.process)? != threads
            || self.observe_exactly(identity.process)?.map(|p| p.identity) != Some(identity)
        {
            return Err(io::Error::other("thread membership changed"));
        }
        Ok(true)
    }

    fn pin_process(
        &self,
        identity: ProcessIdentity,
    ) -> io::Result<Box<dyn super::unix_pty::ProcessControl>> {
        let facts = bsd_with_unique(identity.process)?;
        if process_identity(&facts.bsd) != Some(identity) {
            return Err(io::Error::other("process identity changed"));
        }
        let mut token = [0_u32; 8];
        token[5] = identity.process as u32;
        token[7] = facts.unique.version as u32;
        Ok(Box::new(PinnedProcess { identity, token }))
    }
    fn holds_slave(&self, pid: i32, slave: &std::path::Path) -> io::Result<bool> {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        let expected = std::fs::metadata(slave)?;
        let descriptors = process_fds(pid)?;
        let mut found = false;
        for &(fd, kind) in &descriptors {
            if kind != 1 {
                continue;
            } // PROX_FDTYPE_VNODE
            let mut info = std::mem::MaybeUninit::<VnodeFdPath>::zeroed();
            let size = std::mem::size_of::<VnodeFdPath>() as i32;
            // SAFETY: info is writable storage for this SDK-defined structure.
            let read = unsafe { libc::proc_pidfdinfo(pid, fd, 2, info.as_mut_ptr().cast(), size) };
            if read != size {
                return Err(io::Error::other("a vnode descriptor could not be read"));
            }
            // SAFETY: proc_pidfdinfo initialized the entire structure.
            let info = unsafe { info.assume_init() };
            let path = std::ffi::CStr::from_bytes_until_nul(&info.path)
                .map_err(|_| io::Error::other("unreadable vnode path"))?;
            // A generic controlling-terminal alias does not establish its original slave
            // after the process leaves the session. Refuse instead of assuming it is unrelated.
            if path.to_bytes() == b"/dev/tty" {
                return Err(io::Error::other("an alias terminal holder is unresolved"));
            }
            if path.to_bytes() == slave.as_os_str().as_bytes()
                || (u32::from(info.vnode.stat.mode) & u32::from(libc::S_IFMT)
                    == u32::from(libc::S_IFCHR)
                    && u64::from(info.vnode.stat.rdev) == expected.rdev())
            {
                found = true;
            }
        }
        if process_fds(pid)? != descriptors {
            return Err(io::Error::other(
                "process changed during descriptor enumeration",
            ));
        }
        Ok(found)
    }

    fn observe_process(&self, process: i32) -> Option<ProcessObservation> {
        observe_process(process)
    }

    fn process_ids(&self) -> io::Result<Vec<i32>> {
        all_process_ids()
    }

    fn observe_exactly(&self, process: i32) -> io::Result<Option<ProcessObservation>> {
        observe_exactly(process)
    }

    /// A session member can drop its controlling terminal without leaving the session.
    /// Query every listed PID's session instead of selecting by terminal device. Any
    /// unreadable PID leaves membership uncertain and therefore counts as work.
    fn session_candidates(&self, leader: i32) -> io::Result<Vec<i32>> {
        let mut members = Vec::new();
        for process in all_process_ids()? {
            if process < 1 {
                continue;
            }
            // SAFETY: getsid performs a read-only process identity query.
            let session = unsafe { libc::getsid(process) };
            if session < 1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    continue;
                }
                return Err(error);
            }
            if session == leader {
                members.push(process);
            }
        }
        Ok(members)
    }

    fn master_read_error(&self, _error: &io::Error) -> MasterReadError {
        MasterReadError::Failure
    }
}

// Kernel-validated audit tokens bind signals to the PID version, not a reusable PID.
// Layout and flavors: apple-oss-distributions/xnu bsd/sys/proc_info_private.h.
#[repr(C)]
struct UniqueInfo {
    uuid: [u8; 16],
    unique: u64,
    parent_unique: u64,
    version: i32,
    parent_version: i32,
    reserved: [u64; 2],
}
#[repr(C)]
struct BsdWithUnique {
    bsd: libc::proc_bsdinfo,
    unique: UniqueInfo,
}
fn bsd_with_unique(pid: i32) -> io::Result<BsdWithUnique> {
    let mut info = std::mem::MaybeUninit::<BsdWithUnique>::zeroed();
    let size = std::mem::size_of::<BsdWithUnique>() as i32;
    // SAFETY: flavor 18 initializes exactly proc_bsdinfowithuniqid, with this repr(C) layout.
    if unsafe { libc::proc_pidinfo(pid, 18, 0, info.as_mut_ptr().cast(), size) } != size {
        return Err(io::Error::other("process identity is unavailable"));
    }
    // SAFETY: the complete structure was initialized.
    Ok(unsafe { info.assume_init() })
}
fn process_threads(pid: i32) -> io::Result<Vec<u64>> {
    let mut capacity = 16;
    loop {
        let mut threads = vec![0_u64; capacity];
        let size = (capacity * std::mem::size_of::<u64>()) as i32;
        // SAFETY: flavor 6 writes a list of 64-bit thread identifiers to this sized buffer.
        let read = unsafe { libc::proc_pidinfo(pid, 6, 0, threads.as_mut_ptr().cast(), size) };
        if read <= 0 || !(read as usize).is_multiple_of(8) {
            return Err(io::Error::other("thread enumeration is unavailable"));
        }
        if read >= size {
            if capacity >= 4096 {
                return Err(io::Error::other("incomplete thread enumeration"));
            }
            capacity *= 2;
            continue;
        }
        threads.truncate(read as usize / 8);
        threads.sort_unstable();
        return Ok(threads);
    }
}

struct PinnedProcess {
    identity: ProcessIdentity,
    token: [u32; 8],
}
unsafe extern "C" {
    fn proc_signal_with_audittoken(token: *mut [u32; 8], signal: libc::c_int) -> libc::c_int;
}
impl super::unix_pty::ProcessControl for PinnedProcess {
    fn identity(&self) -> ProcessIdentity {
        self.identity
    }
    fn stopped(&self) -> io::Result<bool> {
        let facts = bsd_with_unique(self.identity.process)?;
        if process_identity(&facts.bsd) != Some(self.identity)
            || facts.unique.version as u32 != self.token[7]
        {
            return Err(io::Error::other("process identity changed"));
        }
        Ok(facts.bsd.pbi_status == libc::SSTOP)
    }
    fn signal(&self, signal: i32) -> io::Result<()> {
        let mut token = self.token;
        // SAFETY: the token is initialized. The kernel matches both PID and PID version and
        // performs the caller's normal signal permission check. Never fall back to kill(pid).
        let error = unsafe { proc_signal_with_audittoken(&mut token, signal) };
        if error == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(error))
        }
    }
}
fn process_fds(pid: i32) -> io::Result<Vec<(i32, u32)>> {
    let mut capacity = 32;
    loop {
        let mut fds = vec![
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0
            };
            capacity
        ];
        let size = (capacity * std::mem::size_of::<libc::proc_fdinfo>()) as i32;
        // SAFETY: errno is thread-local and the descriptor buffer is live writable storage.
        let (read, error) = unsafe {
            *libc::__error() = 0;
            let read =
                libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), size);
            (read, *libc::__error())
        };
        if read < 0
            || (read == 0 && error != 0)
            || !(read as usize).is_multiple_of(std::mem::size_of::<libc::proc_fdinfo>())
        {
            return Err(io::Error::other("process descriptors are unavailable"));
        }
        if read >= size {
            if capacity >= 262_144 {
                return Err(io::Error::other("incomplete descriptor enumeration"));
            }
            capacity *= 2;
            continue;
        }
        fds.truncate(read as usize / std::mem::size_of::<libc::proc_fdinfo>());
        let mut fds = fds
            .into_iter()
            .map(|fd| (fd.proc_fd, fd.proc_fdtype))
            .collect::<Vec<_>>();
        fds.sort_unstable();
        return Ok(fds);
    }
}
// sys/proc_info.h: vinfo_stat (136 bytes), vnode_info (152), vnode_fdinfowithpath
// (1200). Keep the fixed-width SDK layout at this native boundary.
#[repr(C)]
struct VinfoStat {
    dev: u32,
    mode: u16,
    links: u16,
    inode: u64,
    uid: u32,
    gid: u32,
    times: [i64; 8],
    size: i64,
    blocks: i64,
    blocksize: i32,
    flags: u32,
    generation: u32,
    rdev: u32,
    reserved: [i64; 2],
}
#[repr(C)]
struct VnodeInfo {
    stat: VinfoStat,
    kind: i32,
    pad: i32,
    fsid: [i32; 2],
}
#[repr(C)]
struct VnodeFdPath {
    file: [u64; 3],
    vnode: VnodeInfo,
    path: [u8; 1024],
}
const _: () = assert!(std::mem::size_of::<VnodeFdPath>() == 1200);
const _: () = assert!(std::mem::size_of::<UniqueInfo>() == 56);

fn observe_process(process: i32) -> Option<ProcessObservation> {
    let before = process_bsd_info(process)?;
    // SAFETY: getsid performs a read-only process identity query.
    let session = unsafe { libc::getsid(process) };
    if session < 1 {
        return None;
    }
    let after = process_bsd_info(process)?;
    let before_identity = process_identity(&before)?;
    let after_identity = process_identity(&after)?;
    if before_identity != after_identity
        || before.pbi_pgid != after.pbi_pgid
        || before.e_tdev != after.e_tdev
    {
        return None;
    }
    Some(ProcessObservation {
        identity: after_identity,
        process_group: i32::try_from(after.pbi_pgid).ok()?,
        session,
    })
}

/// As [`observe_process`], telling a process proven gone (`ESRCH`) from one that could not be
/// read or changed while it was read.
fn observe_exactly(process: i32) -> io::Result<Option<ProcessObservation>> {
    let unreadable = || io::Error::other("the process could not be read");
    let Some(before) = exact_bsd_info(process)? else {
        return Ok(None);
    };
    // SAFETY: getsid performs a read-only process identity query.
    let session = unsafe { libc::getsid(process) };
    if session < 1 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let Some(after) = exact_bsd_info(process)? else {
        return Ok(None);
    };
    let before_identity = process_identity(&before).ok_or_else(unreadable)?;
    let after_identity = process_identity(&after).ok_or_else(unreadable)?;
    if before_identity != after_identity
        || before.pbi_pgid != after.pbi_pgid
        || before.e_tdev != after.e_tdev
    {
        return Err(io::Error::other("the process changed while it was read"));
    }
    Ok(Some(ProcessObservation {
        identity: after_identity,
        process_group: i32::try_from(after.pbi_pgid).map_err(|_| unreadable())?,
        session,
    }))
}

/// The process's BSD info; None only when it is proven gone.
fn exact_bsd_info(process: i32) -> io::Result<Option<libc::proc_bsdinfo>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| io::Error::other("proc_bsdinfo is too large"))?;
    // SAFETY: info points to writable storage of `expected` bytes.
    let read = unsafe {
        libc::proc_pidinfo(
            process,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<libc::c_void>(),
            expected,
        )
    };
    if read == expected {
        // SAFETY: proc_pidinfo initialized the complete proc_bsdinfo structure.
        return Ok(Some(unsafe { info.assume_init() }));
    }
    let error = io::Error::last_os_error();
    if read <= 0 && error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(io::Error::other("the process could not be read"))
    }
}

fn process_bsd_info(process: i32) -> Option<libc::proc_bsdinfo> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    let read = unsafe {
        libc::proc_pidinfo(
            process,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<libc::c_void>(),
            expected,
        )
    };
    if read != expected {
        return None;
    }
    // SAFETY: proc_pidinfo initialized the complete proc_bsdinfo structure.
    Some(unsafe { info.assume_init() })
}

fn process_identity(info: &libc::proc_bsdinfo) -> Option<ProcessIdentity> {
    Some(ProcessIdentity {
        process: i32::try_from(info.pbi_pid).ok()?,
        start: ProcessStart {
            coarse: info.pbi_start_tvsec,
            fine: info.pbi_start_tvusec,
        },
    })
}

fn all_process_ids() -> io::Result<Vec<i32>> {
    let process_count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if process_count < 1 {
        return Err(io::Error::other("failed to size the macOS process list"));
    }
    let mut capacity = process_count as usize + 64;
    loop {
        let mut process_ids = vec![0_i32; capacity];
        let buffer_size = capacity
            .checked_mul(std::mem::size_of::<i32>())
            .and_then(|size| i32::try_from(size).ok())
            .ok_or_else(|| io::Error::other("process-list buffer size overflowed"))?;
        let listed = unsafe {
            libc::proc_listallpids(process_ids.as_mut_ptr().cast::<libc::c_void>(), buffer_size)
        };
        if listed < 0 {
            return Err(io::Error::last_os_error());
        }
        if listed as usize >= capacity {
            capacity = capacity
                .checked_mul(2)
                .ok_or_else(|| io::Error::other("process-list capacity overflowed"))?;
            continue;
        }
        process_ids.truncate(listed as usize);
        return Ok(process_ids);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_process_image_matches_the_native_executable() {
        use crate::platform::unix_pty::UnixPtyHost;
        let host = super::MacosPtyHost;
        let identity = host
            .observe_exactly(std::process::id() as i32)
            .unwrap()
            .unwrap()
            .identity;
        assert_eq!(
            host.process_image(identity).unwrap(),
            std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
        );
    }

    #[test]
    fn an_observed_cpu_interval_uses_the_macos_timebase() {
        fn used() -> std::time::Duration {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
            // SAFETY: usage is sized writable storage for this process's resource accounting.
            assert_eq!(
                unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
                0
            );
            // SAFETY: getrusage initialized the whole record.
            let usage = unsafe { usage.assume_init() };
            std::time::Duration::from_secs((usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64)
                + std::time::Duration::from_micros(
                    (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64,
                )
        }
        use crate::platform::unix_pty::UnixPtyHost;
        let pid = std::process::id() as i32;
        let identity = super::MacosPtyHost
            .observe_exactly(pid)
            .unwrap()
            .unwrap()
            .identity;
        let before = super::MacosPtyHost.cpu_time(identity).unwrap();
        let start = used();
        while used().saturating_sub(start) < std::time::Duration::from_millis(50) {
            std::hint::spin_loop();
        }
        let elapsed = super::MacosPtyHost
            .cpu_time(identity)
            .unwrap()
            .saturating_sub(before);
        assert!(
            elapsed >= std::time::Duration::from_millis(40),
            "native CPU accounting must use timebase-converted time: {elapsed:?}"
        );
    }

    use super::*;

    #[test]
    fn an_audit_token_for_another_pid_version_cannot_signal_the_current_process() {
        use crate::platform::unix_pty::ProcessControl;
        let pid = std::process::id() as i32;
        let facts = bsd_with_unique(pid).unwrap();
        let mut token = [0_u32; 8];
        token[5] = pid as u32;
        token[7] = (facts.unique.version as u32).wrapping_add(1);
        let process = PinnedProcess {
            identity: process_identity(&facts.bsd).unwrap(),
            token,
        };
        MacosPtyHost
            .pin_process(process.identity)
            .unwrap()
            .signal(libc::SIGCONT)
            .unwrap();
        assert!(
            process.signal(libc::SIGCONT).is_err(),
            "a signal must reject another PID version"
        );
    }
    #[test]
    fn an_exact_read_sees_this_process_and_proves_a_missing_one_gone() {
        let current = i32::try_from(std::process::id()).unwrap();
        let observation = observe_exactly(current).unwrap().unwrap();
        // SAFETY: getsid and getpgrp perform read-only identity queries for this process.
        let (session, group) = unsafe { (libc::getsid(0), libc::getpgrp()) };
        assert_eq!(
            (
                observation.identity.process,
                observation.session,
                observation.process_group
            ),
            (current, session, group)
        );
        assert_eq!(observe_exactly(i32::MAX).unwrap(), None);
    }
}
