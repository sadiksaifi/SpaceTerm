use crate::{contract::*, scope};
use serde::Serialize;
use serde_json::Value;
use std::io::Write;

pub struct Output {
    pub envelope: Envelope<Value>,
    pub text: String,
    pub diagnostics: String,
    /// The `notice:` line about a version that waits for a restart, for stderr after the result.
    pub notice: Option<String>,
}

impl Output {
    pub fn ok<D: Serialize>(
        command: &str,
        core: MachineRef,
        data: D,
        text: String,
        target: &[TargetStep],
    ) -> Self {
        Self {
            envelope: Envelope::ok(command, Some(core), value(data)),
            text,
            diagnostics: scope::verbose(target),
            notice: None,
        }
    }
    pub fn failed(
        command: &str,
        core: Option<MachineRef>,
        failure: Failure,
        data: Value,
        text: String,
    ) -> Self {
        Self {
            envelope: Envelope::error(command, core, failure, data),
            text,
            diagnostics: String::new(),
            notice: None,
        }
    }
    pub fn confirmation(
        command: &str,
        core: MachineRef,
        confirmation: Confirmation<CloseEffects>,
        text: String,
    ) -> Self {
        let envelope = Envelope::confirmation(command, Some(core), confirmation);
        Self {
            envelope: Envelope {
                schema: envelope.schema,
                command: envelope.command,
                status: envelope.status,
                machine: envelope.machine,
                data: Data::new(value(envelope.data.body)),
                error: envelope.error,
                warnings: envelope.warnings,
            },
            text,
            diagnostics: String::new(),
            notice: None,
        }
    }
    pub fn write(&self, mode: Mode, stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
        self.write_to(
            mode,
            crate::admin::terminal(libc::STDERR_FILENO),
            stdout,
            stderr,
        )
    }

    /// [`Self::write`], with whether stderr is a terminal given.
    pub(crate) fn write_to(
        &self,
        mode: Mode,
        stderr_terminal: bool,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> u8 {
        let exit = render(
            &self.envelope,
            &self.text,
            &self.diagnostics,
            mode,
            crate::interrupt::finish(self.envelope.exit()),
            stdout,
            stderr,
        );
        crate::notice::print(self.notice.as_deref(), exit, mode, stderr_terminal, stderr);
        exit
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
