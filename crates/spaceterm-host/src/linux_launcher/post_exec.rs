//! Retained Linux process binding. Publication and runtime custody come from A's issuer.

use std::{
    ffi::CStr,
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::fs::MetadataExt,
};

use crate::{
    linux_custody::NativeLinuxCustody,
    native_deployment::contracts::NativeFailure,
    startup::{PendingStartup, PendingStartupContext, PostExecProof, ProcessIdentity},
    startup_exchange::{ExchangeDeadline, MAX_EXCHANGE_WAIT, post_exec::RetainedPostExec},
};

/// A's retained port, not a mount-query result or a comparison-facts constructor.
/// Recheck must validate the boot, pinned namespaces, descriptor-bound mount
/// evidence, writable-alias exclusion, full authenticated closure, unreadable
/// main ELF and the receiver's nondumpability/channel protection for this startup.
pub(crate) trait RetainedLinuxExecution: Send + Sync {
    fn context(&self) -> &PendingStartupContext;
    fn server(&self) -> &File;
    fn recheck(
        &self,
        executable: &File,
        user_namespace: &File,
        mount_namespace: &File,
    ) -> Result<(), NativeFailure>;
}

impl NativeLinuxCustody {
    fn retain_post_exec(
        &self,
        _pending: &PendingStartup,
    ) -> Result<Box<dyn RetainedLinuxExecution>, NativeFailure> {
        // A's retained custody issuer is absent in this integration base. Only
        // its native retention adapter may replace this refusal. Unit peers are
        // composed below the adapter, never selected by a production feature.
        Err(NativeFailure::CustodyUnavailable)
    }
}

pub(crate) struct NativeLinuxPostExecLease {
    pidfd: OwnedFd,
    process: File,
    executable: File,
    user_namespace: File,
    mount_namespace: File,
    context: PendingStartupContext,
    custody: Box<dyn RetainedLinuxExecution>,
    credentials: Box<dyn ProcessCredentials>,
    deadline: ExchangeDeadline,
}

impl std::fmt::Debug for NativeLinuxPostExecLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeLinuxPostExecLease(..)")
    }
}

impl NativeLinuxPostExecLease {
    pub(crate) fn into_proof(self) -> PostExecProof {
        PostExecProof::retain(self)
    }
}

impl RetainedPostExec for NativeLinuxPostExecLease {
    fn recheck(&self, context: &PendingStartupContext) -> Result<(), NativeFailure> {
        self.deadline.remaining()?;
        if context != &self.context || self.custody.context() != &self.context {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        require_pidfd(&self.pidfd, context.original_process.pid, self.deadline)?;
        require_start(&self.process, context.original_process, self.deadline)?;
        self.credentials
            .recheck(&self.process, context, self.deadline)?;
        let executable = open_at(&self.process, c"exe", libc::O_PATH)?;
        let user = open_at(&self.process, c"ns/user", libc::O_RDONLY)?;
        let mount = open_at(&self.process, c"ns/mnt", libc::O_RDONLY)?;
        if file_binding(&executable)? != file_binding(&self.executable)?
            || file_binding(&executable)? != file_binding(self.custody.server())?
            || inode(&user)? != inode(&self.user_namespace)?
            || inode(&mount)? != inode(&self.mount_namespace)?
        {
            return Err(NativeFailure::ExecutionBindingUnproven);
        }
        self.custody.recheck(&executable, &user, &mount)?;
        // Bracket all execution/credential observations with the retained pidfd
        // and start identity. A's lease supplies the publication and runtime fence.
        require_start(&self.process, context.original_process, self.deadline)?;
        require_pidfd(&self.pidfd, context.original_process.pid, self.deadline)?;
        self.deadline.remaining()?;
        Ok(())
    }
}

#[expect(
    dead_code,
    reason = "B2 composes this issuer after retained custody is available"
)]
pub(crate) fn verify_linux_post_exec(
    pending: &PendingStartup,
    server_pidfd: OwnedFd,
) -> Result<NativeLinuxPostExecLease, NativeFailure> {
    let custody = pending.linux_custody()?.retain_post_exec(pending)?;
    if custody.context() != pending.context()
        || file_binding(custody.server())? != file_binding(pending.retained_server()?)?
    {
        return Err(NativeFailure::ExecutionBindingUnproven);
    }
    verify_retained_post_exec(server_pidfd, custody, Box::new(KernelCredentials))
}

fn verify_retained_post_exec(
    pidfd: OwnedFd,
    custody: Box<dyn RetainedLinuxExecution>,
    credentials: Box<dyn ProcessCredentials>,
) -> Result<NativeLinuxPostExecLease, NativeFailure> {
    let deadline = ExchangeDeadline::start(MAX_EXCHANGE_WAIT)?;
    let context = custody.context().clone();
    require_pidfd(&pidfd, context.original_process.pid, deadline)?;
    let process = File::open(format!("/proc/{}", context.original_process.pid))
        .map_err(|_| NativeFailure::ProcessIdentityMismatch)?;
    require_filesystem(process.as_raw_fd(), 0x9fa0)?; // procfs
    require_start(&process, context.original_process, deadline)?;
    let executable = open_at(&process, c"exe", libc::O_PATH)?;
    let user_namespace = open_at(&process, c"ns/user", libc::O_RDONLY)?;
    let mount_namespace = open_at(&process, c"ns/mnt", libc::O_RDONLY)?;
    let lease = NativeLinuxPostExecLease {
        pidfd,
        process,
        executable,
        user_namespace,
        mount_namespace,
        context,
        custody,
        credentials,
        deadline,
    };
    lease.recheck(&lease.context)?;
    Ok(lease)
}

trait ProcessCredentials: Send + Sync {
    fn recheck(
        &self,
        process: &File,
        context: &PendingStartupContext,
        deadline: ExchangeDeadline,
    ) -> Result<(), NativeFailure>;
}

struct KernelCredentials;
impl ProcessCredentials for KernelCredentials {
    fn recheck(
        &self,
        process: &File,
        context: &PendingStartupContext,
        deadline: ExchangeDeadline,
    ) -> Result<(), NativeFailure> {
        let status = open_at(process, c"status", libc::O_RDONLY | libc::O_NOFOLLOW)?;
        require_credentials(&read_bounded(&status, 16 * 1024, deadline)?, context)
    }
}

fn require_credentials(bytes: &[u8], context: &PendingStartupContext) -> Result<(), NativeFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| NativeFailure::PrivilegeDropUnproven)?;
    let field = |name: &str| -> Result<&str, NativeFailure> {
        let mut values = text.lines().filter_map(|line| line.strip_prefix(name));
        let value = values.next().ok_or(NativeFailure::PrivilegeDropUnproven)?;
        if values.next().is_some() {
            return Err(NativeFailure::PrivilegeDropUnproven);
        }
        Ok(value.trim())
    };
    for (name, id) in [
        ("Uid:", context.key.account_uid),
        ("Gid:", context.primary_gid),
    ] {
        let ids: Vec<_> = field(name)?.split_ascii_whitespace().collect();
        if id == 0
            || id == u32::MAX
            || ids.len() != 4
            || ids.iter().any(|value| value.parse::<u32>() != Ok(id))
        {
            return Err(NativeFailure::PrivilegeDropUnproven);
        }
    }
    if !field("Groups:")?.is_empty() {
        return Err(NativeFailure::PrivilegeDropUnproven);
    }
    for name in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
        let value = field(name)?;
        if value.is_empty() || value.len() > 16 || u64::from_str_radix(value, 16) != Ok(0) {
            return Err(NativeFailure::PrivilegeDropUnproven);
        }
    }
    Ok(())
}

fn require_pidfd(fd: &OwnedFd, pid: u32, deadline: ExchangeDeadline) -> Result<(), NativeFailure> {
    if !(1..=i32::MAX as u32).contains(&pid) {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    // pidfds use anon_inodefs on 5.15 and pidfs on newer kernels. Ask the
    // pidfd syscall itself to validate the handle; signal 0 sends no signal.
    // SAFETY: these calls inspect the owned descriptor without changing the target.
    let valid = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            0,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if valid != 0
        || flags < 0
        || flags & libc::FD_CLOEXEC == 0
        || unsafe { libc::poll(&mut poll, 1, 0) } != 0
    {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    let info = File::open(format!("/proc/self/fdinfo/{}", fd.as_raw_fd()))
        .map_err(|_| NativeFailure::ProcessIdentityMismatch)?;
    let bytes = read_bounded(&info, 4096, deadline)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| NativeFailure::ProcessIdentityMismatch)?;
    let mut pids = text.lines().filter_map(|line| line.strip_prefix("Pid:"));
    if pids
        .next()
        .and_then(|value| value.trim().parse::<u32>().ok())
        != Some(pid)
        || pids.next().is_some()
    {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    Ok(())
}

fn require_start(
    process: &File,
    expected: ProcessIdentity,
    deadline: ExchangeDeadline,
) -> Result<(), NativeFailure> {
    let stat = open_at(process, c"stat", libc::O_RDONLY | libc::O_NOFOLLOW)?;
    let bytes = read_bounded(&stat, 4096, deadline)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| NativeFailure::ProcessIdentityMismatch)?;
    let end = text
        .rfind(')')
        .ok_or(NativeFailure::ProcessIdentityMismatch)?;
    let fields: Vec<_> = text[end + 1..].split_ascii_whitespace().collect();
    let pid = text
        .split_once(' ')
        .and_then(|(pid, _)| pid.parse::<u32>().ok());
    let ticks = fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(NativeFailure::ProcessIdentityMismatch)?;
    // SAFETY: sysconf observes the kernel's process-start clock frequency.
    let rate = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if !(1..=1_000_000).contains(&rate) {
        return Err(NativeFailure::NativeUnavailable);
    }
    let rate = rate as u64;
    if pid != Some(expected.pid)
        || matches!(fields.first(), Some(&"Z" | &"X" | &"x"))
        || ticks / rate != expected.start_seconds
        || ((ticks % rate) * 1_000_000 / rate) as u32 != expected.start_microseconds
    {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    Ok(())
}

#[derive(Eq, PartialEq)]
struct FileBinding {
    inode: (u64, u64),
    mount: u64,
}

fn file_binding(file: &File) -> Result<FileBinding, NativeFailure> {
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    // SAFETY: AT_EMPTY_PATH binds the statx mount ID to the retained descriptor.
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_INO | libc::STATX_MNT_ID,
            &mut stat,
        )
    } != 0
        || stat.stx_mask & (libc::STATX_INO | libc::STATX_MNT_ID)
            != libc::STATX_INO | libc::STATX_MNT_ID
        || stat.stx_mnt_id == 0
    {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    Ok(FileBinding {
        inode: inode(file)?,
        mount: stat.stx_mnt_id,
    })
}

fn inode(file: &File) -> Result<(u64, u64), NativeFailure> {
    let metadata = file
        .metadata()
        .map_err(|_| NativeFailure::ExecutionBindingUnproven)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn require_filesystem(fd: i32, magic: libc::c_long) -> Result<(), NativeFailure> {
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: fstatfs writes only the provided native observation for this live descriptor.
    if unsafe { libc::fstatfs(fd, &mut stat) } != 0 || stat.f_type != magic {
        return Err(NativeFailure::ExecutionBindingUnproven);
    }
    Ok(())
}

fn open_at(directory: &File, name: &CStr, mode: i32) -> Result<File, NativeFailure> {
    // Kernel procfs magic links are intentional here, unlike application-owned records.
    // SAFETY: names are fixed, NUL-terminated and resolved relative to a retained proc directory.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            mode | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(NativeFailure::ExecutionBindingUnproven);
    }
    // SAFETY: successful openat transfers a new descriptor to this File.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn read_bounded(
    file: &File,
    limit: usize,
    deadline: ExchangeDeadline,
) -> Result<Vec<u8>, NativeFailure> {
    let mut bytes = vec![0; limit + 1];
    let mut used = 0;
    let mut interrupts = 0;
    for _ in 0..32 {
        deadline.remaining()?;
        // SAFETY: the read is confined to the initialized bounded buffer.
        let count = unsafe {
            libc::read(
                file.as_raw_fd(),
                bytes[used..].as_mut_ptr().cast(),
                bytes.len() - used,
            )
        };
        if count < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted && interrupts < 4 {
                interrupts += 1;
                continue;
            }
            return Err(NativeFailure::NativeUnavailable);
        }
        used += count as usize;
        if used > limit {
            return Err(NativeFailure::LimitExceeded);
        }
        if count == 0 {
            bytes.truncate(used);
            return Ok(bytes);
        }
    }
    Err(NativeFailure::LimitExceeded)
}

#[cfg(test)]
#[path = "../../tests/native_handoff/linux_post_exec.rs"]
mod tests;
