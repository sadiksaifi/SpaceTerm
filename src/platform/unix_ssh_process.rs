use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use crate::ssh::process::{
    ProcessExit, ProcessSignal, SpawnedSshProcess, SshProcessAdapter, SshProcessExitObservation,
    SshProcessMechanismError, SshProcessPipes, SshProcessSpawnRequest, SshProcessStdio,
};

mod exit_observation;

#[derive(Clone, Copy, Default)]
pub(crate) struct UnixSshProcessAdapter;

pub(crate) struct UnixSshProcess {
    child: Child,
    process_group: libc::pid_t,
    observed_exit: Option<ProcessExit>,
}

impl SshProcessAdapter for UnixSshProcessAdapter {
    type Process = UnixSshProcess;

    fn spawn(
        &self,
        request: SshProcessSpawnRequest,
    ) -> Result<SpawnedSshProcess<Self::Process>, SshProcessMechanismError> {
        let mut command = Command::new(request.executable());
        command
            .args(request.arguments())
            .env_clear()
            .current_dir(request.current_directory())
            .process_group(0)
            .stdin(stdio(request.stdin()))
            .stdout(stdio(request.stdout()))
            .stderr(stdio(request.stderr()));
        for (name, value) in request.environment() {
            command.env(name, value);
        }
        if let Some((name, capability)) = request.askpass_capability_environment() {
            command.env(name, std::ffi::OsStr::from_bytes(capability));
        }
        // SAFETY: the callback runs after fork and performs only async-signal-safe signal-mask
        // operations before exec. It does not access shared application state.
        unsafe {
            command.pre_exec(clear_child_signal_mask);
        }
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                SshProcessMechanismError::NotFound
            } else {
                SshProcessMechanismError::LaunchFailed
            }
        })?;
        let process_group = match libc::pid_t::try_from(child.id()) {
            Ok(process_group) => process_group,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SshProcessMechanismError::LaunchFailed);
            }
        };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        use std::os::fd::AsFd;
        let pollable = stdin
            .as_ref()
            .zip(stdout.as_ref())
            .and_then(|(write, read)| {
                Some((
                    Box::new(std::fs::File::from(
                        write.as_fd().try_clone_to_owned().ok()?,
                    ))
                        as Box<dyn spaceterm_server::account_bridge::AccountWrite>,
                    Box::new(std::fs::File::from(read.as_fd().try_clone_to_owned().ok()?))
                        as Box<dyn spaceterm_server::account_bridge::AccountRead>,
                ))
            });
        Ok(SpawnedSshProcess::new(
            UnixSshProcess {
                child,
                process_group,
                observed_exit: None,
            },
            SshProcessPipes::new_pollable(
                stdin.map(|pipe| Box::new(pipe) as Box<dyn std::io::Write + Send>),
                stdout.map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
                stderr.map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
                pollable,
            ),
        ))
    }

    fn try_status(
        &self,
        process: &mut Self::Process,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        process.observe_status(true)
    }

    fn signal(
        &self,
        process: &mut Self::Process,
        signal: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError> {
        let signal = match signal {
            ProcessSignal::Terminate => libc::SIGTERM,
            ProcessSignal::Kill => libc::SIGKILL,
        };
        signal_group(process, signal)
    }

    fn observe_exit(&self, process: &mut Self::Process) -> Option<SshProcessExitObservation> {
        exit_observation::observe(self, process)
    }

    fn reap(&self, mut process: Self::Process) -> Result<(), SshProcessMechanismError> {
        // A group signal can miss a child still being forked by its leader. Reaping runs on
        // the cleanup worker: observe the leader's exit without releasing its identity, then
        // signal the group after its final fork has completed. Signal delivery itself never
        // waits for exit, so a caller can still enforce its shutdown deadline.
        process
            .observe_status(false)
            .map_err(|_| SshProcessMechanismError::ReapFailed)?;
        let group = signal_group(&mut process, libc::SIGKILL);
        let reaped = process
            .child
            .wait()
            .map(|_| ())
            .map_err(|_| SshProcessMechanismError::ReapFailed);
        reaped.and(group)
    }
}

impl UnixSshProcess {
    fn observe_status(
        &mut self,
        nonblocking: bool,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        if let Some(exit) = self.observed_exit {
            return Ok(Some(exit));
        }
        loop {
            let mut information = MaybeUninit::<libc::siginfo_t>::zeroed();
            // SAFETY: this exclusively owned child is unreaped. WNOWAIT retains its PID and
            // process group identity until reap, including after a successful status query.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id() as libc::id_t,
                    information.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT | if nonblocking { libc::WNOHANG } else { 0 },
                )
            };
            if result == -1 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(SshProcessMechanismError::StatusFailed);
            }
            // SAFETY: the structure was zeroed and waitid succeeded. WNOHANG leaves si_pid zero
            // when no exit is available; otherwise si_code and si_status describe that exit.
            let information = unsafe { information.assume_init() };
            if unsafe { information.si_pid() } == 0 {
                return Ok(None);
            }
            let code = (information.si_code == libc::CLD_EXITED)
                .then(|| unsafe { information.si_status() });
            let exit = ProcessExit::new(code == Some(0), code);
            self.observed_exit = Some(exit);
            return Ok(Some(exit));
        }
    }
}

fn clear_child_signal_mask() -> io::Result<()> {
    let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: sigemptyset initializes the provided signal set.
    if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: sigemptyset initialized the signal set after succeeding above.
    let empty = unsafe { empty.assume_init() };
    // SAFETY: empty is initialized and the previous mask is not needed in the child process.
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn stdio(mode: SshProcessStdio) -> Stdio {
    match mode {
        SshProcessStdio::Null => Stdio::null(),
        SshProcessStdio::Piped => Stdio::piped(),
    }
}

fn signal_group(
    process: &mut UnixSshProcess,
    signal: libc::c_int,
) -> Result<(), SshProcessMechanismError> {
    // SAFETY: the unreaped child reserves the private process group until final cleanup.
    let result = unsafe { libc::kill(-process.process_group, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    #[cfg(target_os = "macos")]
    if error.raw_os_error() == Some(libc::EPERM)
        && process.observe_status(true).ok().flatten().is_some()
        && retained_group_has_only_zombies(process.process_group)
    {
        // macOS reports EPERM when every group member is a zombie. Prove that state rather
        // than suppressing a permission failure for a live descendant.
        return Ok(());
    }
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(SshProcessMechanismError::SignalFailed)
    }
}

#[cfg(target_os = "macos")]
fn retained_group_has_only_zombies(leader: libc::pid_t) -> bool {
    let Some(members) = process_group_members(leader) else {
        return false;
    };
    if !members.contains(&leader) {
        return false;
    }
    let exited = members.iter().all(|member| {
        let mut information = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: proc_pidinfo writes at most size bytes into this valid structure. Process
        // identities here are observed only; no descendant PID is ever used for signalling.
        let bytes = unsafe {
            libc::proc_pidinfo(
                *member,
                libc::PROC_PIDTBSDINFO,
                0,
                information.as_mut_ptr().cast(),
                size,
            )
        };
        if bytes != size {
            return bytes == 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        }
        // SAFETY: proc_pidinfo returned the complete initialized BSD information structure.
        let information = unsafe { information.assume_init() };
        information.pbi_pid == *member as u32
            && information.pbi_pgid == leader as u32
            && information.pbi_status == libc::SZOMB
    });
    // A member could finish a fork while membership was being read. Once the observed
    // members are all dead, reject any later member rather than guessing its state.
    exited
        && process_group_members(leader)
            .is_some_and(|current| current.iter().all(|member| members.contains(member)))
}

#[cfg(target_os = "macos")]
fn process_group_members(leader: libc::pid_t) -> Option<Vec<libc::pid_t>> {
    // PROC_PGRP_ONLY from sys/proc_info.h.
    const PROC_PGRP_ONLY: u32 = 2;
    let width = std::mem::size_of::<libc::pid_t>();
    // SAFETY: a null buffer queries the required byte count without writing memory.
    let size =
        unsafe { libc::proc_listpids(PROC_PGRP_ONLY, leader as u32, std::ptr::null_mut(), 0) };
    let count = usize::try_from(size)
        .ok()?
        .checked_div(width)?
        .checked_add(1)?;
    let mut members = vec![0; count];
    let capacity = libc::c_int::try_from(std::mem::size_of_val(members.as_slice())).ok()?;
    // SAFETY: proc_listpids writes at most capacity bytes into this live PID buffer.
    let bytes = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            leader as u32,
            members.as_mut_ptr().cast(),
            capacity,
        )
    };
    let bytes = usize::try_from(bytes).ok()?;
    if bytes >= capacity as usize || bytes % width != 0 {
        return None;
    }
    members.truncate(bytes / width);
    Some(members)
}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::platform::unix_adapter_tests::short_temporary_root;
    use crate::ssh::askpass::AskPassCapabilityCopy;

    fn shell_request(script: &str) -> SshProcessSpawnRequest {
        SshProcessSpawnRequest::new(
            PathBuf::from("/bin/sh"),
            vec![OsString::from("-c"), OsString::from(script)],
            short_temporary_root().to_path_buf(),
            vec![
                (
                    OsString::from("HOME"),
                    short_temporary_root().as_os_str().to_owned(),
                ),
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            ],
            SshProcessStdio::Null,
            SshProcessStdio::Null,
            SshProcessStdio::Null,
        )
    }

    fn current_test_request(test: &str) -> SshProcessSpawnRequest {
        SshProcessSpawnRequest::new(
            std::env::current_exe().unwrap(),
            vec![
                OsString::from("--ignored"),
                OsString::from("--exact"),
                OsString::from(test),
            ],
            short_temporary_root().to_path_buf(),
            Vec::new(),
            SshProcessStdio::Null,
            SshProcessStdio::Null,
            SshProcessStdio::Null,
        )
    }

    struct SignalMaskRestore(libc::sigset_t);

    impl Drop for SignalMaskRestore {
        fn drop(&mut self) {
            // SAFETY: the saved signal set was initialized by pthread_sigmask for this thread.
            let result =
                unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut()) };
            assert_eq!(result, 0, "the test thread signal mask should be restored");
        }
    }

    fn block_sigwinch() -> SignalMaskRestore {
        let mut blocked = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: sigemptyset initializes the provided signal set.
        assert_eq!(unsafe { libc::sigemptyset(blocked.as_mut_ptr()) }, 0);
        // SAFETY: sigemptyset initialized the signal set above.
        let mut blocked = unsafe { blocked.assume_init() };
        // SAFETY: blocked is initialized and SIGWINCH is a valid signal number.
        assert_eq!(unsafe { libc::sigaddset(&mut blocked, libc::SIGWINCH) }, 0);
        let mut previous = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: both signal-set pointers are valid for the duration of this call.
        assert_eq!(
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, previous.as_mut_ptr()) },
            0
        );
        // SAFETY: pthread_sigmask initialized previous after succeeding above.
        SignalMaskRestore(unsafe { previous.assume_init() })
    }

    #[test]
    #[ignore = "runs only as a child of the SSH process signal-mask test"]
    fn spawned_ssh_process_child_requires_unblocked_sigwinch() {
        let mut current = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: a null input set queries the current thread mask into the valid output pointer.
        assert_eq!(
            unsafe {
                libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), current.as_mut_ptr())
            },
            0
        );
        // SAFETY: pthread_sigmask initialized current after succeeding above.
        let current = unsafe { current.assume_init() };
        // SAFETY: current is initialized and SIGWINCH is a valid signal number.
        assert_eq!(unsafe { libc::sigismember(&current, libc::SIGWINCH) }, 0);
        // A dedicated exit code proves this ignored child reached its assertion.
        std::process::exit(73);
    }

    #[test]
    fn spawned_ssh_process_should_not_inherit_the_calling_thread_signal_mask() {
        let adapter = UnixSshProcessAdapter;
        let restore = block_sigwinch();
        let mut spawned = adapter
            .spawn(current_test_request(
                "platform::unix_ssh_process::tests::spawned_ssh_process_child_requires_unblocked_sigwinch",
            ))
            .unwrap();
        drop(restore);

        let exit = (0..100).find_map(|_| {
            let exit = adapter.try_status(spawned.process_mut()).unwrap();
            if exit.is_none() {
                std::thread::sleep(Duration::from_millis(10));
            }
            exit
        });
        adapter.reap(spawned.into_process()).unwrap();

        assert_eq!(exit.map(ProcessExit::code), Some(Some(73)));
    }

    #[test]
    fn process_should_have_a_private_group_and_nonblocking_status() {
        let adapter = UnixSshProcessAdapter;
        let mut spawned = adapter.spawn(shell_request("sleep 1")).unwrap();
        let process_group = spawned.process_mut().process_group;

        // SAFETY: getpgid only reads the identity of the live child owned by this test.
        let group = unsafe { libc::getpgid(process_group) };
        let initial = adapter.try_status(spawned.process_mut()).unwrap();
        adapter
            .signal(spawned.process_mut(), ProcessSignal::Kill)
            .unwrap();
        adapter.reap(spawned.into_process()).unwrap();

        assert!(group > 0 && group == process_group && initial.is_none());
    }

    #[test]
    fn observed_process_exit_should_retain_its_identity_and_remain_observable() {
        let adapter = UnixSshProcessAdapter;
        let mut spawned = adapter.spawn(shell_request("exit 7")).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let exit = loop {
            if let Some(exit) = adapter.try_status(spawned.process_mut()).unwrap() {
                break exit;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture child did not exit"
            );
            std::thread::yield_now();
        };
        let mut observation = adapter.observe_exit(spawned.process_mut()).unwrap();
        let interrupt = observation.interrupt_handle();
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || sender.send(observation.wait()).unwrap());
        let observed = receiver.recv_timeout(Duration::from_secs(2));
        interrupt.interrupt();
        waiter.join().unwrap();
        let process = spawned.process_mut().process_group;
        let mut retained = MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: this checks that status observation has left our child waitable. WNOWAIT
        // preserves that child for the adapter's final cleanup below.
        let waitable = unsafe {
            libc::waitid(
                libc::P_PID,
                process as libc::id_t,
                retained.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        adapter.reap(spawned.into_process()).unwrap();
        assert_eq!(
            waitable, 0,
            "status observation must not reap the owned child"
        );
        // SAFETY: waitid succeeded and initialized the zeroed information structure.
        assert_eq!(unsafe { retained.assume_init().si_pid() }, process);
        assert_eq!(exit.code(), Some(7));
        assert_eq!(
            observed.unwrap(),
            Ok(crate::ssh::process::SshProcessExitWake::Exit)
        );
    }

    #[test]
    fn askpass_capability_should_cross_the_native_spawn_boundary_separately() {
        let capability = b"test-only-capability";
        let script = "test \"$SPACETERM_SSH_ASKPASS_CAPABILITY\" = test-only-capability";
        let request = shell_request(script)
            .with_askpass_capability(Some(AskPassCapabilityCopy::from_test_bytes(capability)));
        assert!(
            request
                .environment()
                .iter()
                .all(|(name, _)| name != "SPACETERM_SSH_ASKPASS_CAPABILITY")
        );
        assert_eq!(
            request.askpass_capability_environment(),
            Some((
                std::ffi::OsStr::new("SPACETERM_SSH_ASKPASS_CAPABILITY"),
                capability.as_slice(),
            ))
        );

        let adapter = UnixSshProcessAdapter;
        let mut spawned = adapter.spawn(request).unwrap();
        let exit = (0..100).find_map(|_| {
            let exit = adapter.try_status(spawned.process_mut()).unwrap();
            if exit.is_none() {
                std::thread::sleep(Duration::from_millis(10));
            }
            exit
        });
        adapter.reap(spawned.into_process()).unwrap();

        assert!(exit.is_some_and(ProcessExit::is_success));
    }

    #[test]
    fn cleanup_after_observed_leader_exit_closes_member_stdout() {
        use std::io::Read;

        let adapter = UnixSshProcessAdapter;
        let mut spawned = adapter
            .spawn(SshProcessSpawnRequest::new(
                PathBuf::from("/bin/sh"),
                vec!["-c".into(), "read value".into()],
                short_temporary_root().to_path_buf(),
                Vec::new(),
                SshProcessStdio::Piped,
                SshProcessStdio::Null,
                SshProcessStdio::Null,
            ))
            .unwrap();
        let group = spawned.process_mut().process_group;
        let mut member = Command::new("/bin/sleep")
            .arg("30")
            .process_group(group)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = member.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            sender.send(stdout.read_to_end(&mut bytes)).unwrap();
        });
        // Dropping the owned stdin lets the leader exit, while the other owned group member
        // still holds stdout. No signal is sent before observing that exit.
        let mut process = spawned.into_process();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let exited = loop {
            if adapter.try_status(&mut process).unwrap().is_some() {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !exited {
            adapter.signal(&mut process, ProcessSignal::Kill).unwrap();
        }
        let cleanup = adapter.reap(process);
        let eof = receiver.recv_timeout(Duration::from_secs(2));
        // Keep the test's direct child owned until cleanup even when the regression returns.
        let _ = member.kill();
        let _ = member.wait();
        reader.join().unwrap();

        assert!(exited, "the fixture leader must exit before cleanup");
        cleanup.unwrap();
        assert!(
            eof.unwrap().is_ok(),
            "cleanup must close the member-held pipe"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reaping_a_group_with_multiple_zombies_should_succeed() {
        let adapter = UnixSshProcessAdapter;
        let mut spawned = adapter.spawn(shell_request("exec /bin/sleep 30")).unwrap();
        let group = spawned.process_mut().process_group;
        let mut member = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .env_clear()
            .process_group(group)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut information = MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: this child belongs to the test. Retain its zombie in the owned group until
        // after adapter cleanup, so the all-exited group state does not depend on launchd.
        let waitable = unsafe {
            libc::waitid(
                libc::P_PID,
                member.id() as libc::id_t,
                information.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        let signal = adapter.signal(spawned.process_mut(), ProcessSignal::Kill);
        let cleanup = adapter.reap(spawned.into_process());
        let exit = member.wait().unwrap();

        assert_eq!(waitable, 0);
        assert_eq!(exit.code(), Some(7));
        signal.unwrap();
        cleanup.unwrap();
    }

    #[test]
    fn terminate_and_kill_should_reap_the_leader_and_descendants() {
        for signal in [ProcessSignal::Terminate, ProcessSignal::Kill] {
            let sequence = match signal {
                ProcessSignal::Terminate => "term",
                ProcessSignal::Kill => "kill",
            };
            let pid_file = short_temporary_root().join(format!(
                "spaceterm-process-{sequence}-{}.pid",
                std::process::id()
            ));
            let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
            let adapter = UnixSshProcessAdapter;
            let mut spawned = adapter.spawn(shell_request(&script)).unwrap();
            let mut descendant = None;
            for _ in 0..100 {
                descendant = fs::read_to_string(&pid_file)
                    .ok()
                    .and_then(|value| value.trim().parse::<libc::pid_t>().ok());
                if descendant.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let descendant = descendant.expect("the child must finish publishing its process ID");
            let leader = spawned.process_mut().process_group;

            adapter.signal(spawned.process_mut(), signal).unwrap();
            adapter.reap(spawned.into_process()).unwrap();

            assert!(wait_for_missing_process(leader) && wait_for_missing_process(descendant));
            let _ = fs::remove_file(pid_file);
        }
    }

    fn wait_for_missing_process(process: libc::pid_t) -> bool {
        (0..100).any(|_| {
            // SAFETY: signal zero checks process existence and dereferences no pointers.
            let missing = unsafe { libc::kill(process, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if !missing {
                std::thread::sleep(Duration::from_millis(10));
            }
            missing
        })
    }
}

#[cfg(feature = "desktop")]
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
#[cfg(feature = "desktop")]
impl crate::ssh::server_process::SshServerBootstrap for UnixSshProcessAdapter {
    fn key_public(&self, path: &std::path::Path) -> Option<crate::ssh::command::SshCommandSpec> {
        crate::ssh::command::SshCommandContext::key_public("/usr/bin/ssh-keygen".into(), path).ok()
    }
    /// This script selects only the approved digest in a fixed account/build namespace.
    /// It never evaluates a path or an endpoint supplied by the remote protocol.
    fn locator(
        &self,
        identity: &str,
        action: &str,
        missing: bool,
        arguments: &[crate::ssh::command::ValidatedRemoteServerArgument],
    ) -> String {
        let mut script = format!(
            r#"set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
uid=$(id -u)
[ "$uid" -gt 0 ] || exit 71
root="$HOME/.local/share/{}/server"
policy="$root/service.json"
if [ ! -e "$policy" ]; then
"#,
            identity
        );
        if missing {
            script.push_str(
                r#"case "$(uname -s):$(uname -m)" in
Darwin:arm64) target=aarch64-apple-darwin;;
Linux:x86_64) target=x86_64-unknown-linux-gnu;;
*) exit 72;;
esac
printf 'SPACETERM_MISSING %s %s\n' "$uid" "$target"
exit 0
"#,
            );
        } else {
            script.push_str("exit 73\n");
        }
        script.push_str(r#"fi
[ ! -L "$HOME/.local" ] && [ ! -L "$HOME/.local/share" ] && [ ! -L "${root%/server}" ] && [ ! -L "$root" ] && [ ! -L "$policy" ] || exit 74
case "$(uname -s)" in
Darwin) owner=$(stat -f '%u:%Lp' "$policy"); dir_owner=$(stat -f '%u:%Lp' "$root");;
Linux) owner=$(stat -c '%u:%a' "$policy"); dir_owner=$(stat -c '%u:%a' "$root");;
*) exit 72;;
esac
[ "$owner" = "$uid:600" ] && [ "$dir_owner" = "$uid:700" ] || exit 74
[ "$(wc -c < "$policy")" -le 16384 ] || exit 74
[ "$(grep -o '"installation"' "$policy" | wc -l | tr -d ' ')" = 1 ] || exit 74
digest=$(sed -n 's/.*"installation"[[:space:]]*:[[:space:]]*"\([0-9a-f]\{64\}\)".*/\1/p' "$policy")
[ "${#digest}" = 64 ] || exit 74
case "$digest" in *[!0-9a-f]*) exit 74;; esac
[ ! -L "$root/installations" ] && [ ! -L "$root/installations/$digest" ] && [ ! -L "$root/installations/$digest/bin" ] && [ ! -L "$root/installations/$digest/bin/spaceterm-server" ] || exit 74
"#);
        script.push_str(&format!(
            "exec \"$root/installations/$digest/bin/spaceterm-server\" {} {}",
            quote(action),
            quote(identity)
        ));
        for argument in arguments {
            script.push(' ');
            script.push_str(&quote(argument.as_str()));
        }
        script.push('\n');
        script
    }
    fn install(
        &self,
        identity: &str,
        archive_length: usize,
        signature: &[u8],
        digest: &str,
        password: bool,
    ) -> String {
        transfer_script(
            "ssh-install",
            identity,
            archive_length,
            signature,
            digest,
            Some(password),
        )
    }
    fn stage(
        &self,
        identity: &str,
        archive_length: usize,
        signature: &[u8],
        digest: &str,
    ) -> String {
        transfer_script(
            "ssh-stage",
            identity,
            archive_length,
            signature,
            digest,
            None,
        )
    }
}

/// The body both transferred-runtime actions share. Boot authorization belongs to installation
/// alone: staging changes no service policy, so it never asks for one.
#[cfg(feature = "desktop")]
fn transfer_script(
    action: &str,
    identity: &str,
    archive_length: usize,
    signature: &[u8],
    digest: &str,
    linger: Option<bool>,
) -> String {
    use base64::Engine;
    let signature = base64::engine::general_purpose::STANDARD.encode(signature);
    let work = action.trim_start_matches("ssh-");
    let password = linger == Some(true);
    let read_authorization = if password {
        "IFS= read -r authorization\n"
    } else {
        ""
    };
    let authorize = match linger {
        Some(true) => format!(
            "if [ \"$(uname -s)\" = Linux ]; then\n    printf '%s\\n' \"$authorization\" | \"$work/runtime/server/bin/spaceterm-server\" ssh-enable-linger-password {} || exit 77\nunset authorization\nfi\n",
            quote(identity)
        ),
        Some(false) => format!(
            "if [ \"$(uname -s)\" = Linux ]; then\n    \"$work/runtime/server/bin/spaceterm-server\" ssh-enable-linger {} || exit 77\nfi\n",
            quote(identity)
        ),
        None => String::new(),
    };
    format!(
        r#"set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077
work=$(mktemp -d "${{TMPDIR:-/tmp}}/spaceterm-{work}.XXXXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
{read_authorization}cat > "$work/artifact.tar.gz"
[ "$(wc -c < "$work/artifact.tar.gz")" -eq {archive_length} ] || exit 75
printf %s {} | openssl base64 -d -A > "$work/artifact.sig"
mkdir "$work/runtime"
tar -xzf "$work/artifact.tar.gz" -C "$work/runtime"
{authorize}"$work/runtime/server/bin/spaceterm-server" {} {} "$work/artifact.tar.gz" "$work/artifact.sig" {}
"#,
        quote(&signature),
        quote(action),
        quote(identity),
        quote(digest)
    )
}
