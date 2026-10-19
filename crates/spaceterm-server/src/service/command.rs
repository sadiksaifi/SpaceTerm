//! Runs one owned service-manager command with bounded output and a single deadline.
use super::ServerError;
use std::{
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const CAPTURE_LIMIT: usize = 65_536;

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Child retains the unreaped process identity. Never signal a separately saved PID.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub(super) fn execute(prepared: &mut Command, timeout: Duration) -> Result<Vec<u8>, ServerError> {
    let mut child = OwnedChild(
        prepared
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ServerError::ServiceUnavailable)?,
    );
    let deadline = Instant::now() + timeout;
    let result = collect(&mut child.0, deadline);
    // On timeout or collection failure, terminate/reap before joining the exit observer.
    drop(child);
    result
}

fn collect(child: &mut Child, deadline: Instant) -> Result<Vec<u8>, ServerError> {
    let mut output = child.stdout.take().ok_or(ServerError::ServiceUnavailable)?;
    let (mut exited, mut observer) =
        UnixStream::pair().map_err(|_| ServerError::ServiceUnavailable)?;
    // SAFETY: output owns this descriptor exclusively; only its read end becomes nonblocking.
    let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
    if flags == -1
        || unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(ServerError::ServiceUnavailable);
    }
    let process = child.id();
    let waiter = std::thread::Builder::new()
        .name("spaceterm-service-command".into())
        .spawn(move || {
            loop {
                // SAFETY: the owner retains this child until collection or termination. WNOWAIT
                // observes exit without reaping, so its identity cannot be reused before Child::wait.
                let result = unsafe {
                    let mut info: libc::siginfo_t = std::mem::zeroed();
                    libc::waitid(
                        libc::P_PID,
                        process as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT,
                    )
                };
                if result == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                let _ = observer.write_all(&[u8::from(result == 0)]);
                break;
            }
        })
        .map_err(|_| ServerError::ServiceUnavailable)?;
    let result = (|| {
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        let mut eof = false;
        let mut status = None;
        loop {
            // Bound each drain so a continuously writing child cannot postpone its deadline.
            for _ in 0..8 {
                if eof {
                    break;
                }
                match output.read(&mut buffer) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(count) => {
                        let retained = count.min(CAPTURE_LIMIT - bytes.len());
                        bytes.extend_from_slice(&buffer[..retained]);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => return Err(ServerError::ServiceUnavailable),
                }
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(ServerError::ServiceUnavailable);
            };
            if let Some(status) = status
                && eof
            {
                return if status {
                    Ok(bytes)
                } else {
                    Err(ServerError::ServiceUnavailable)
                };
            }
            let mut events = [
                libc::pollfd {
                    fd: if eof { -1 } else { output.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if status.is_some() {
                        -1
                    } else {
                        exited.as_raw_fd()
                    },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let milliseconds = remaining
                .as_millis()
                .saturating_add(1)
                .min(i32::MAX as u128) as i32;
            // SAFETY: both initialized records retain their descriptors for this bounded wait.
            if unsafe {
                libc::poll(
                    events.as_mut_ptr(),
                    events.len() as libc::nfds_t,
                    milliseconds,
                )
            } == -1
            {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(ServerError::ServiceUnavailable);
            }
            if events
                .iter()
                .any(|event| event.revents & libc::POLLNVAL != 0)
            {
                return Err(ServerError::ServiceUnavailable);
            }
            if events[1].revents != 0 {
                let mut result = [0];
                exited
                    .read_exact(&mut result)
                    .map_err(|_| ServerError::ServiceUnavailable)?;
                if result != [1] {
                    return Err(ServerError::ServiceUnavailable);
                }
                status = Some(
                    child
                        .wait()
                        .map_err(|_| ServerError::ServiceUnavailable)?
                        .success(),
                );
            }
        }
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    // Retain the unreaped identity until the observer is finished, including when it was
    // not scheduled before the deadline. Otherwise waitid could attach to a reused child PID.
    let observed = waiter.join();
    if result.is_err() {
        let _ = child.wait();
    }
    observed.map_err(|_| ServerError::ServiceUnavailable)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }
    #[test]
    fn command_collects_success_and_refuses_nonzero_exit() {
        assert_eq!(
            execute(&mut shell("printf ready"), Duration::from_secs(2)).unwrap(),
            b"ready"
        );
        assert_eq!(
            execute(&mut shell("printf failure; exit 7"), Duration::from_secs(2)),
            Err(ServerError::ServiceUnavailable)
        );
    }
    #[test]
    fn command_caps_capture_and_drains_excess_output_before_exit() {
        let output = execute(
            &mut shell("head -c 262144 /dev/zero"),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(output, vec![0; CAPTURE_LIMIT]);
    }
    #[test]
    fn command_waits_for_exit_when_stdout_closes_first() {
        assert_eq!(
            execute(
                &mut shell("exec 1>&-; /bin/sleep 0.02; exit 7"),
                Duration::from_secs(2)
            ),
            Err(ServerError::ServiceUnavailable)
        );
    }
    #[test]
    fn command_deadline_kills_and_reaps_only_its_owned_child() {
        let root = std::env::temp_dir().join(format!(
            "st-command-{}-{}",
            std::process::id(),
            u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        std::fs::create_dir(&root).unwrap();
        let pid_file = root.join("child");
        let mut command = shell("printf %s $$ > \"$1\"; exec /bin/sleep 30");
        command.arg("fixture").arg(&pid_file);
        assert_eq!(
            execute(&mut command, Duration::from_millis(100)),
            Err(ServerError::ServiceUnavailable)
        );
        let process: libc::pid_t = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        let mut status = 0;
        // SAFETY: waitpid only asks about this fixture's child; it never signals a PID.
        assert_eq!(
            unsafe { libc::waitpid(process, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn descendant_stdout_cannot_extend_the_command_deadline() {
        // The background fixture exits on its own; this command owns no authority to signal it.
        let started = Instant::now();
        assert_eq!(
            execute(
                &mut shell("/bin/sleep 0.5 & exit 0"),
                Duration::from_millis(100)
            ),
            Err(ServerError::ServiceUnavailable)
        );
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "inherited stdout must not keep a reader join alive"
        );
    }
}
