use crate::{contract::*, scope};
use serde::Serialize;
use serde_json::Value;
use std::io::Write;

pub struct Output {
    pub envelope: Envelope<Value>,
    pub text: String,
    pub diagnostics: String,
}

impl Output {
    pub fn ok<D: Serialize>(
        command: &str,
        server: ServerRef,
        data: D,
        text: String,
        target: &[TargetStep],
    ) -> Self {
        Self {
            envelope: Envelope::ok(command, Some(server), value(data)),
            text,
            diagnostics: scope::verbose(target),
        }
    }
    pub fn failed(
        command: &str,
        server: Option<ServerRef>,
        failure: Failure,
        data: Value,
        text: String,
    ) -> Self {
        Self {
            envelope: Envelope::error(command, server, failure, data),
            text,
            diagnostics: String::new(),
        }
    }
    pub fn confirmation(
        command: &str,
        server: ServerRef,
        confirmation: Confirmation<CloseEffects>,
        text: String,
    ) -> Self {
        let envelope = Envelope::confirmation(command, Some(server), confirmation);
        Self {
            envelope: Envelope {
                schema: envelope.schema,
                command: envelope.command,
                status: envelope.status,
                server: envelope.server,
                data: Data::new(value(envelope.data.body)),
                error: envelope.error,
                warnings: envelope.warnings,
            },
            text,
            diagnostics: String::new(),
        }
    }
    pub fn write(
        &self,
        json: bool,
        quiet: bool,
        verbose: bool,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> u8 {
        let written = if json {
            serde_json::to_writer(&mut *stdout, &self.envelope)
                .map_err(std::io::Error::from)
                .and_then(|()| stdout.write_all(b"\n"))
                .and_then(|()| stdout.flush())
        } else if quiet && self.envelope.exit() == 0 {
            Ok(())
        } else {
            stdout
                .write_all(self.text.as_bytes())
                .and_then(|()| stdout.flush())
        };
        if let Err(error) = written {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                return EXIT_BROKEN_PIPE;
            }
            let _ = writeln!(stderr, "{}", Failure::new(Code::OutputFailed).text());
            return Code::OutputFailed.exit();
        }
        if !json {
            if let Some(error) = &self.envelope.error {
                let _ = writeln!(stderr, "error: {} ({})", error.message, error.code.as_str());
                if let Details::Usage(details) = &error.details
                    && let Some(argument) = &details.argument
                {
                    let _ = writeln!(stderr, "argument: {argument}");
                }
            }
            for warning in &self.envelope.warnings {
                let _ = writeln!(stderr, "warning: {}", warning.message);
            }
        }
        if verbose {
            let _ = stderr.write_all(self.diagnostics.as_bytes());
        }
        self.envelope.exit()
    }
}

pub(crate) fn value(data: impl Serialize) -> Value {
    serde_json::to_value(data).expect("CLI contract types serialize")
}

pub(crate) fn preview(effects: &CloseEffects) -> String {
    let mut text = format!("{} Terminal Sessions will end.\n", effects.ends_sessions);
    for session in &effects.sessions {
        text.push_str(&format!("  {} --pane id:{}\n", session.label, session.pane));
    }
    if effects.removes_workspace {
        text.push_str("The Workspace will be removed.\n");
    }
    text
}
