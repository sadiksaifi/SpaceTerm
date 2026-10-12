//! Bounded launchctl and systemctl execution under retained native custody.

use super::{
    ServiceDefinition, SupervisorCommand, SupervisorCommands, SupervisorCustody,
    SupervisorObservation, SupervisorReply,
};
use crate::{
    native_deployment::{
        contracts::{NativeFailure, PublicationDisposition},
        profile::NativePlatform,
    },
    native_management::helper_lifetime::HelperDeadline,
};
use std::{
    io::{self, Read},
    os::fd::AsRawFd,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::Duration,
};

const OUTPUT_LIMIT: usize = 16 * 1024;

/// Fixed system utilities. The installation owner's lease binds their code and closure to exec.
#[derive(Default)]
pub struct SystemSupervisorCommands;
impl<C: SupervisorCustody> SupervisorCommands<C> for SystemSupervisorCommands {
    fn call(
        &self,
        custody: &C,
        lease: &mut C::Lease,
        action: SupervisorCommand,
        deadline: HelperDeadline,
    ) -> Result<SupervisorReply, NativeFailure> {
        let retirement_deadline = deadline.reserve_cleanup(Duration::from_millis(20))?;
        let execution_deadline = retirement_deadline.reserve_cleanup(Duration::from_millis(100))?;
        let definition = custody.definition(lease);
        let platform = definition.platform();
        let mut command = fixed_command(definition, action)?;
        custody.bind_exec(lease, &mut command)?;
        command
            .env_clear()
            .env("LC_ALL", "C")
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        execution_deadline.check()?;
        match custody.begin_utility_custody(lease) {
            PublicationDisposition::Durable(()) => {}
            PublicationDisposition::NotApplied(failure) => return Err(failure),
            PublicationDisposition::Indeterminate => return Err(NativeFailure::Indeterminate),
        }
        // Once armed, any uncertain spawn or reap retains the durable exclusion.
        // The watchdog can exit without allowing another publisher to replace live code.
        execution_deadline
            .check()
            .map_err(|_| NativeFailure::CleanupUnresolved)?;
        let mut child = command
            .spawn()
            .map_err(|_| NativeFailure::CleanupUnresolved)?;
        let output = collect(&mut child, execution_deadline);
        let (status, bytes) = match output {
            Ok(output) => output,
            Err(failure) => {
                retire_utility(&mut child, retirement_deadline)?;
                custody
                    .finish_utility_custody(lease)
                    .map_err(|_| NativeFailure::CleanupUnresolved)?;
                return Err(failure);
            }
        };
        // collect returns only after this exact child has been reaped.
        custody
            .finish_utility_custody(lease)
            .map_err(|_| NativeFailure::CleanupUnresolved)?;
        decode_reply(platform, action, status.code(), &bytes)
    }
}

/// Exact utility-child retirement boundary. Production uses its owned Child;
/// tests inject delayed native reap and failed signal results, never a PID lookup.
pub trait UtilityChild {
    /// Nonblocking exact-child reap. True proves this child has been reaped.
    fn poll_reaped(&mut self) -> Result<bool, NativeFailure>;
    /// Signals only this owned utility child, never the server or Git processes.
    fn kill(&mut self) -> Result<(), NativeFailure>;
}
impl UtilityChild for Child {
    fn poll_reaped(&mut self) -> Result<bool, NativeFailure> {
        self.try_wait()
            .map(|status| status.is_some())
            .map_err(|_| NativeFailure::CleanupUnresolved)
    }
    fn kill(&mut self) -> Result<(), NativeFailure> {
        Child::kill(self).map_err(|_| NativeFailure::CleanupUnresolved)
    }
}

/// Reaps only the exact owned utility within the original cleanup budget.
/// CleanupUnresolved leaves the installation owner's durable exclusion armed.
/// No blocking wait or replacement helper can outlive/reset this deadline.
pub fn retire_utility(
    child: &mut impl UtilityChild,
    deadline: HelperDeadline,
) -> Result<(), NativeFailure> {
    deadline
        .check()
        .map_err(|_| NativeFailure::CleanupUnresolved)?;
    if child
        .poll_reaped()
        .map_err(|_| NativeFailure::CleanupUnresolved)?
    {
        return Ok(());
    }
    if child.kill().is_err() {
        // Exit may win the signal race, but the signal failure is not absence.
        return if child
            .poll_reaped()
            .map_err(|_| NativeFailure::CleanupUnresolved)?
        {
            Ok(())
        } else {
            Err(NativeFailure::CleanupUnresolved)
        };
    }
    loop {
        deadline
            .check()
            .map_err(|_| NativeFailure::CleanupUnresolved)?;
        if child
            .poll_reaped()
            .map_err(|_| NativeFailure::CleanupUnresolved)?
        {
            return Ok(());
        }
        let remaining = deadline
            .remaining()
            .map_err(|_| NativeFailure::CleanupUnresolved)?;
        thread::sleep(Duration::from_millis(2).min(remaining));
    }
}

/// Constructs only a closed command, for inspection or retained native execution.
/// This command is not execution authority; SystemSupervisorCommands binds custody before spawning.
pub fn fixed_command(
    definition: &ServiceDefinition,
    action: SupervisorCommand,
) -> Result<Command, NativeFailure> {
    let label = definition.label();
    match definition.platform() {
        NativePlatform::Linux => {
            let mut command = Command::new("/usr/bin/systemctl");
            command.args(["--system", "--no-pager", "--no-ask-password"]);
            match action {
                SupervisorCommand::Observe => {
                    command.args(["show", "--property=LoadState,ActiveState", "--", &label]);
                }
                SupervisorCommand::Reload => {
                    command.arg("daemon-reload");
                }
                SupervisorCommand::Bootstrap => return Err(NativeFailure::InvalidRecord),
                SupervisorCommand::Enable => {
                    command.args(["enable", "--", &label]);
                }
                SupervisorCommand::Disable => {
                    command.args(["disable", "--", &label]);
                }
                SupervisorCommand::Start => {
                    command.args(["--no-block", "start", "--", &label]);
                }
                SupervisorCommand::Retire => {
                    command.args(["--no-block", "stop", "--", &label]);
                }
            }
            Ok(command)
        }
        NativePlatform::Macos => {
            let target = format!("system/{label}");
            let mut command = Command::new("/bin/launchctl");
            match action {
                SupervisorCommand::Observe => {
                    command.args(["print", &target]);
                }
                SupervisorCommand::Reload => return Err(NativeFailure::InvalidRecord),
                SupervisorCommand::Bootstrap => {
                    command.args(["bootstrap", "system", &definition.configuration_path()]);
                }
                SupervisorCommand::Enable => {
                    command.args(["enable", &target]);
                }
                SupervisorCommand::Disable => {
                    command.args(["disable", &target]);
                }
                SupervisorCommand::Start => {
                    command.args(["kickstart", &target]);
                }
                SupervisorCommand::Retire => {
                    command.args(["bootout", &target]);
                }
            }
            Ok(command)
        }
    }
}

fn collect(
    child: &mut Child,
    deadline: HelperDeadline,
) -> Result<(ExitStatus, Vec<u8>), NativeFailure> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or(NativeFailure::NativeUnavailable)?;
    let fd = stdout.as_raw_fd();
    // SAFETY: stdout owns this live pipe descriptor. Nonblocking reads preserve the absolute deadline.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(NativeFailure::NativeUnavailable);
    }
    let mut bytes = Vec::new();
    let mut eof = false;
    let mut status = None;
    loop {
        deadline.check()?;
        let mut buffer = [0; 2048];
        match stdout.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(n) => {
                if bytes
                    .len()
                    .checked_add(n)
                    .is_none_or(|length| length > OUTPUT_LIMIT)
                {
                    return Err(NativeFailure::LimitExceeded);
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(NativeFailure::NativeUnavailable),
        }
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|_| NativeFailure::NativeUnavailable)?;
        }
        if eof && let Some(status) = status {
            return Ok((status, bytes));
        }
        thread::sleep(Duration::from_millis(2).min(deadline.remaining()?));
    }
}

/// Decodes bounded utility output into classifications. Raw native text is never exported.
pub fn decode_reply(
    platform: NativePlatform,
    action: SupervisorCommand,
    exit: Option<i32>,
    bytes: &[u8],
) -> Result<SupervisorReply, NativeFailure> {
    if bytes.len() > OUTPUT_LIMIT {
        return Err(NativeFailure::LimitExceeded);
    }
    if platform == NativePlatform::Macos
        && exit == Some(113)
        && matches!(
            action,
            SupervisorCommand::Observe | SupervisorCommand::Retire
        )
    {
        return Ok(if action == SupervisorCommand::Observe {
            SupervisorReply::Observation(SupervisorObservation::NotLoaded)
        } else {
            SupervisorReply::Applied
        });
    }
    if exit != Some(0) {
        return Err(NativeFailure::SupervisorUnavailable);
    }
    if action != SupervisorCommand::Observe {
        return Ok(SupervisorReply::Applied);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| NativeFailure::InvalidRecord)?;
    let state = match platform {
        NativePlatform::Linux => {
            let mut load = None;
            let mut active = None;
            for line in text.lines() {
                if let Some(value) = line.strip_prefix("LoadState=") {
                    if load.replace(value).is_some() {
                        return Err(NativeFailure::InvalidRecord);
                    }
                } else if let Some(value) = line.strip_prefix("ActiveState=") {
                    if active.replace(value).is_some() {
                        return Err(NativeFailure::InvalidRecord);
                    }
                } else {
                    return Err(NativeFailure::InvalidRecord);
                }
            }
            match (load, active) {
                (Some("not-found"), Some("inactive")) => SupervisorObservation::NotLoaded,
                (Some("loaded"), Some("active")) => SupervisorObservation::Running,
                (Some("loaded"), Some("inactive" | "failed")) => SupervisorObservation::Inactive,
                (Some("loaded"), Some("activating" | "deactivating" | "reloading")) => {
                    SupervisorObservation::Transitioning
                }
                _ => return Err(NativeFailure::SupervisorUnavailable),
            }
        }
        NativePlatform::Macos => {
            // Only the root service's tab-indented state field is authoritative.
            // Nested environment/resource fields cannot substitute for it.
            let mut state = None;
            for line in text.lines() {
                if let Some(value) = line.strip_prefix("\tstate = ")
                    && state.replace(value).is_some()
                {
                    return Err(NativeFailure::InvalidRecord);
                }
            }
            match state {
                Some("running") => SupervisorObservation::Running,
                Some("not running" | "waiting" | "exited") => SupervisorObservation::Inactive,
                Some("spawn scheduled" | "spawning") => SupervisorObservation::Transitioning,
                _ => return Err(NativeFailure::SupervisorUnavailable),
            }
        }
    };
    Ok(SupervisorReply::Observation(state))
}
