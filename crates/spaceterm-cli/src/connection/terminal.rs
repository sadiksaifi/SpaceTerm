//! Bounded invitation input and terminal interaction. Secrets are never echoed.
use super::InteractionIo;
use crate::contract::{self, Code, Failure};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcsetattr};
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::os::fd::AsFd;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub struct Terminal {
    tty: Option<File>,
    interrupted: Arc<AtomicBool>,
    signal: signal_hook::SigId,
}
impl Terminal {
    pub fn new(allow_input: bool) -> Result<Self, Failure> {
        let interrupted = Arc::new(AtomicBool::new(false));
        let signal = signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())
            .map_err(|_| Failure::new(Code::StorageUnavailable))?;
        let tty = allow_input
            .then(|| {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/tty")
                    .ok()
            })
            .flatten();
        Ok(Self {
            tty,
            interrupted,
            signal,
        })
    }
    fn answer(&mut self, prompt: &str, deadline: Instant) -> Result<bool, Failure> {
        let tty = self.tty.as_mut().ok_or(Code::InteractionRequired)?;
        tty.write_all(prompt.as_bytes())
            .and_then(|()| tty.flush())
            .map_err(|_| Failure::new(Code::OutputFailed))?;
        let answer = read_line(tty, 16, deadline, &self.interrupted)?;
        Ok(answer.eq_ignore_ascii_case(b"y") || answer.eq_ignore_ascii_case(b"yes"))
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.signal);
    }
}

struct HiddenInput<'a> {
    tty: &'a File,
    original: Termios,
}
impl<'a> HiddenInput<'a> {
    fn new(tty: &'a File) -> Result<Self, Failure> {
        let original = tcgetattr(tty).map_err(|_| Failure::new(Code::InteractionRequired))?;
        let mut hidden = original.clone();
        hidden.local_modes.remove(LocalModes::ECHO);
        hidden.local_modes.insert(LocalModes::ECHONL);
        tcsetattr(tty, OptionalActions::Now, &hidden)
            .map_err(|_| Failure::new(Code::InteractionRequired))?;
        Ok(Self { tty, original })
    }
}
impl Drop for HiddenInput<'_> {
    fn drop(&mut self) {
        let _ = tcsetattr(self.tty, OptionalActions::Now, &self.original);
    }
}

impl InteractionIo for Terminal {
    fn interactive(&self) -> bool {
        self.tty.is_some()
    }
    fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }
    fn invitation(&mut self, deadline: Instant) -> Result<Zeroizing<String>, Failure> {
        let stdin = std::io::stdin();
        let bytes = if stdin.is_terminal() {
            let tty = self.tty.as_mut().ok_or(Code::InteractionRequired)?;
            tty.write_all(b"Invitation Link (hidden): ")
                .and_then(|()| tty.flush())
                .map_err(|_| Failure::new(Code::OutputFailed))?;
            let hidden = HiddenInput::new(tty)?;
            let mut input = tty
                .try_clone()
                .map_err(|_| Failure::new(Code::InteractionRequired))?;
            let result = read_line(&mut input, 8192, deadline, &self.interrupted);
            drop(hidden);
            result?
        } else {
            // StdinLock buffers ahead. Poll only an unbuffered duplicate, so bytes
            // already read into a Rust buffer cannot make an open pipe appear silent.
            let mut input = File::from(
                rustix::io::dup(&stdin).map_err(|_| Failure::new(Code::InteractionRequired))?,
            );
            read_line(&mut input, 8192, deadline, &self.interrupted)?
        };
        let text =
            std::str::from_utf8(&bytes).map_err(|_| Failure::new(Code::InvalidInvitation))?;
        Ok(Zeroizing::new(text.to_owned()))
    }
    fn consent(
        &mut self,
        invitation: &spaceterm_server::direct::Invitation,
        deadline: Instant,
    ) -> Result<bool, Failure> {
        self.answer(
            &format!(
                "Join {} at {}? Server fingerprint: {}. [y/N] ",
                invitation.server_name(),
                invitation.endpoint(),
                invitation.server_pin().fingerprint()
            ),
            deadline,
        )
    }
    fn confirm_removal(&mut self, name: &str, deadline: Instant) -> Result<bool, Failure> {
        self.answer(
            &format!("Unpair this computer from {name}? [y/N] "),
            deadline,
        )
    }
    fn comparison(
        &mut self,
        event: &contract::ComparisonEvent,
        json: bool,
    ) -> Result<(), super::OutputError> {
        if json {
            let mut output = std::io::stdout().lock();
            serde_json::to_writer(&mut output, event)
                .map_err(std::io::Error::from)
                .and_then(|()| output.write_all(b"\n"))
                .and_then(|()| output.flush())
                .map_err(super::OutputError::from)
        } else {
            writeln!(
                std::io::stderr().lock(),
                "Comparison Code: {}. Compare it on the server, then approve this device.",
                event.data.code
            )
            .map_err(super::OutputError::from)
        }
    }
    fn wait(&mut self, duration: Duration) {
        std::thread::sleep(duration.min(Duration::from_millis(100)));
    }
}

fn read_line(
    reader: &mut (impl Read + AsFd),
    maximum: usize,
    deadline: Instant,
    interrupted: &AtomicBool,
) -> Result<Zeroizing<Vec<u8>>, Failure> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(maximum.min(256)));
    loop {
        if interrupted.load(Ordering::Acquire) {
            return Err(Code::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(Code::Timeout.into());
        }
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        let mut descriptors = [PollFd::new(&*reader, PollFlags::IN)];
        match poll(&mut descriptors, Some(&timeout)) {
            Ok(0) => continue,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return Err(Code::InteractionRequired.into()),
            Ok(_) => {}
        }
        let mut byte = [0];
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                bytes.push(byte[0]);
                if bytes.len() > maximum {
                    return Err(Code::InvalidInvitation.into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Code::InvalidInvitation.into()),
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn invitation_input_is_bounded_before_parsing_or_credential_access() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || writer.write_all(&vec![b'x'; 8194]).unwrap());
        let failure = read_line(
            &mut reader,
            8192,
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(failure.code, Code::InvalidInvitation);
        let mut unread = [0];
        assert_eq!(reader.read(&mut unread).unwrap(), 1);
        writer.join().unwrap();
    }
    #[test]
    fn piped_invitation_accepts_eof_and_crlf_and_cancels_while_silent() {
        for ending in ["", "\r\n"] {
            let (mut reader, mut writer) = UnixStream::pair().unwrap();
            writer
                .write_all(format!("spaceterm://pair/fixture{ending}").as_bytes())
                .unwrap();
            drop(writer);
            let value = read_line(
                &mut reader,
                8192,
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(value.as_slice(), b"spaceterm://pair/fixture");
        }
        let (mut reader, _writer) = UnixStream::pair().unwrap();
        let failure = read_line(
            &mut reader,
            8192,
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert_eq!(failure.code, Code::Interrupted);
    }
}

#[cfg(test)]
mod pipe_tests {
    use super::*;
    #[test]
    fn open_pipe_child() {
        if std::env::var_os("SPACETERM_TEST_INVITATION_PIPE").is_none() {
            return;
        }
        let mut terminal = Terminal::new(false).unwrap();
        let value = terminal
            .invitation(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(value.as_str(), "spaceterm://pair/fixture");
    }
    #[test]
    fn invitation_line_completes_while_its_pipe_stays_open() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "connection::terminal::pipe_tests::open_pipe_child",
            ])
            .env("SPACETERM_TEST_INVITATION_PIPE", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(b"spaceterm://pair/fixture\n").unwrap();
        input.flush().unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "the Invitation Link completes without pipe EOF"
        );
        drop(input);
    }
}
