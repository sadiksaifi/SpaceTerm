//! Confirmation reads the controlling terminal, never a pipeline's stdin.

use std::io::{Read, Write};

use spaceterm_client::management_cli::{
    ConfirmationPrompt, ConfirmationReply, confirmation::MAX_CONFIRMATION_LINE_BYTES,
};

use super::{CliFailure, output::assessment_description, runner::CliIo};

#[derive(Default)]
pub struct TerminalIo;
impl CliIo for TerminalIo {
    fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
        std::io::stdout()
            .lock()
            .write_all(bytes)
            .map_err(|_| CliFailure::IoUnavailable)
    }
    fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
        std::io::stderr()
            .lock()
            .write_all(bytes)
            .map_err(|_| CliFailure::IoUnavailable)
    }
    fn confirm_server_fingerprint(
        &mut self,
        fingerprint: super::arguments::PublicFingerprint,
    ) -> Result<(), CliFailure> {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|_| CliFailure::ConfirmationRequired)?;
        let prompt = format!(
            "Server fingerprint: {}\nType the full fingerprint to enroll this CLI Client: ",
            super::output::hex(&fingerprint.bytes())
        );
        tty.write_all(prompt.as_bytes())
            .and_then(|()| tty.flush())
            .map_err(|_| CliFailure::IoUnavailable)?;
        let line = read_confirmation_line(&mut tty)?;
        let reply = std::str::from_utf8(&line).map_err(|_| CliFailure::ConfirmationRequired)?;
        if super::arguments::parse_hex::<32>(reply).ok() != Some(fingerprint.bytes()) {
            return Err(CliFailure::ConfirmationRequired);
        }
        Ok(())
    }
    fn confirm_native_action(
        &mut self,
        prompt: &crate::native::NativeActionPrompt,
    ) -> Result<(), CliFailure> {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|_| CliFailure::ConfirmationRequired)?;
        tty.write_all(prompt.description().as_bytes())
            .and_then(|()| tty.write_all(b"\nType 'confirm' to continue: "))
            .and_then(|()| tty.flush())
            .map_err(|_| CliFailure::IoUnavailable)?;
        if read_confirmation_line(&mut tty)? != b"confirm" {
            return Err(CliFailure::ConfirmationRequired);
        }
        Ok(())
    }
    fn read_confirmation(
        &mut self,
        prompt: &ConfirmationPrompt,
    ) -> Result<ConfirmationReply, CliFailure> {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|_| CliFailure::ConfirmationRequired)?;
        let description = assessment_description(prompt.assessment());
        tty.write_all(description.as_bytes())
            .and_then(|()| tty.write_all(b"\nType 'confirm' to continue: "))
            .and_then(|()| tty.flush())
            .map_err(|_| CliFailure::IoUnavailable)?;
        let line = read_confirmation_line(&mut tty)?;
        prompt.reply(&line)
    }
}
/// This bounded mechanical reader also handles a terminal closing mid-line.
pub fn read_confirmation_line(reader: &mut impl Read) -> Result<Vec<u8>, CliFailure> {
    let mut line = Vec::with_capacity(MAX_CONFIRMATION_LINE_BYTES);
    loop {
        let mut byte = [0];
        match reader.read(&mut byte) {
            Ok(0) => return Err(CliFailure::ConfirmationRequired),
            Ok(_) if byte[0] == b'\n' => {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            Ok(_) if line.len() == MAX_CONFIRMATION_LINE_BYTES => {
                return Err(CliFailure::LimitExceeded);
            }
            Ok(_) => line.push(byte[0]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(CliFailure::IoUnavailable),
        }
    }
}
