use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use crate::ssh::process::{
    ProcessExit, ProcessSignal, SpawnedSshProcess, SshProcessAdapter, SshProcessMechanismError,
    SshProcessPipes, SshProcessSpawnRequest, SshProcessStdio,
};

#[derive(Clone, Copy, Default)]
pub(crate) struct MacOsSshProcessAdapter;

pub(crate) struct MacOsSshProcess {
    child: Child,
    process_group: libc::pid_t,
    collected_exit: Option<ProcessExit>,
}

impl SshProcessAdapter for MacOsSshProcessAdapter {
    type Process = MacOsSshProcess;

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
        let stdin = child
            .stdin
            .take()
            .map(|stdin| Box::new(stdin) as Box<dyn Write + Send>);
        let stdout = child
            .stdout
            .take()
            .map(|stdout| Box::new(stdout) as Box<dyn Read + Send>);
        let stderr = child
            .stderr
            .take()
            .map(|stderr| Box::new(stderr) as Box<dyn Read + Send>);
        Ok(SpawnedSshProcess::new(
            MacOsSshProcess {
                child,
                process_group,
                collected_exit: None,
            },
            SshProcessPipes::new(stdin, stdout, stderr),
        ))
    }

    fn try_status(
        &self,
        process: &mut Self::Process,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        if let Some(exit) = process.collected_exit {
            return Ok(Some(exit));
        }
        process
            .child
            .try_wait()
            .map_err(|_| SshProcessMechanismError::StatusFailed)
            .map(|status| {
                status.map(|status| {
                    let exit = ProcessExit::new(status.success(), status.code());
                    process.collected_exit = Some(exit);
                    exit
                })
            })
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
        signal_group(process.process_group, signal)
    }

    fn reap(&self, mut process: Self::Process) -> Result<(), SshProcessMechanismError> {
        if process.collected_exit.is_none() {
            process
                .child
                .wait()
                .map_err(|_| SshProcessMechanismError::ReapFailed)?;
        }
        Ok(())
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
    process_group: libc::pid_t,
    signal: libc::c_int,
) -> Result<(), SshProcessMechanismError> {
    // SAFETY: the positive process group belongs to a child launched above with a private group.
    let result = unsafe { libc::kill(-process_group, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(SshProcessMechanismError::SignalFailed)
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::platform::askpass::AskPassCapabilityCopy;

    fn shell_request(script: &str) -> SshProcessSpawnRequest {
        SshProcessSpawnRequest::new(
            PathBuf::from("/bin/sh"),
            vec![OsString::from("-c"), OsString::from(script)],
            PathBuf::from("/private/tmp"),
            vec![
                (OsString::from("HOME"), OsString::from("/private/tmp")),
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
            PathBuf::from("/private/tmp"),
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
    }

    #[test]
    fn spawned_ssh_process_should_not_inherit_the_calling_thread_signal_mask() {
        let adapter = MacOsSshProcessAdapter;
        let restore = block_sigwinch();
        let mut spawned = adapter
            .spawn(current_test_request(
                "platform::macos_ssh_process::tests::spawned_ssh_process_child_requires_unblocked_sigwinch",
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

        assert!(exit.is_some_and(ProcessExit::is_success));
    }

    #[test]
    fn process_should_have_a_private_group_and_nonblocking_status() {
        let adapter = MacOsSshProcessAdapter;
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

        let adapter = MacOsSshProcessAdapter;
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
    fn terminate_and_kill_should_reap_the_leader_and_descendants() {
        for signal in [ProcessSignal::Terminate, ProcessSignal::Kill] {
            let sequence = match signal {
                ProcessSignal::Terminate => "term",
                ProcessSignal::Kill => "kill",
            };
            let pid_file = PathBuf::from(format!(
                "/private/tmp/spaceterm-process-{sequence}-{}.pid",
                std::process::id()
            ));
            let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
            let adapter = MacOsSshProcessAdapter;
            let mut spawned = adapter.spawn(shell_request(&script)).unwrap();
            for _ in 0..100 {
                if pid_file.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let descendant = fs::read_to_string(&pid_file)
                .unwrap()
                .trim()
                .parse::<libc::pid_t>()
                .unwrap();
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
