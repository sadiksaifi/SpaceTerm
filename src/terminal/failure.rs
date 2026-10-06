use std::collections::VecDeque;
use std::fmt;
use std::path::Path;
use std::time::Instant;

use super::emulator::PresentationGeneration;
use super::key::KeyAction;
use super::session::{TerminalSessionExit, TerminalSessionFailure, TerminalSessionStartupStage};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureClass {
    Pty,
    Emulator,
    Presentation,
    Platform,
    Resource,
}

impl fmt::Display for FailureClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pty => "PTY",
            Self::Emulator => "Terminal Emulator",
            Self::Presentation => "presentation",
            Self::Platform => "desktop integration",
            Self::Resource => "renderer resource",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Recoverability {
    Recoverable,
    Fatal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureReason {
    Io(std::io::ErrorKind),
    OutOfMemory,
    InvalidValue,
    OutOfSpace,
    InvalidInput,
    InvalidUtf8,
    CapacityExceeded,
    ChannelClosed,
}

impl From<libghostty_vt::Error> for FailureReason {
    fn from(error: libghostty_vt::Error) -> Self {
        match error {
            libghostty_vt::Error::OutOfMemory => Self::OutOfMemory,
            libghostty_vt::Error::InvalidValue => Self::InvalidValue,
            libghostty_vt::Error::OutOfSpace { .. } => Self::OutOfSpace,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalFailure {
    class: FailureClass,
    recoverability: Recoverability,
    operation: &'static str,
    reason: Option<FailureReason>,
}

impl TerminalFailure {
    pub(crate) const fn pty(operation: &'static str) -> Self {
        Self::new(FailureClass::Pty, Recoverability::Fatal, operation)
    }

    pub(crate) const fn emulator(operation: &'static str) -> Self {
        Self::new(FailureClass::Emulator, Recoverability::Fatal, operation)
    }

    pub(crate) fn emulator_error(operation: &'static str, error: libghostty_vt::Error) -> Self {
        Self::emulator(operation).with_reason(error.into())
    }

    pub(crate) const fn with_reason(mut self, reason: FailureReason) -> Self {
        self.reason = Some(reason);
        self
    }

    pub(crate) const fn presentation(operation: &'static str) -> Self {
        Self::new(
            FailureClass::Presentation,
            Recoverability::Recoverable,
            operation,
        )
    }

    pub(crate) const fn platform(operation: &'static str) -> Self {
        Self::new(
            FailureClass::Platform,
            Recoverability::Recoverable,
            operation,
        )
    }

    pub(crate) const fn resource(operation: &'static str) -> Self {
        Self::new(
            FailureClass::Resource,
            Recoverability::Recoverable,
            operation,
        )
    }

    const fn new(
        class: FailureClass,
        recoverability: Recoverability,
        operation: &'static str,
    ) -> Self {
        Self {
            class,
            recoverability,
            operation,
            reason: None,
        }
    }

    pub(crate) fn from_session(failure: &TerminalSessionFailure) -> Self {
        match failure {
            TerminalSessionFailure::Startup { stage, .. } => match stage {
                TerminalSessionStartupStage::Pty
                | TerminalSessionStartupStage::Reader
                | TerminalSessionStartupStage::ReaderThread => Self::pty("session-startup"),
                TerminalSessionStartupStage::Emulator => Self::emulator("session-startup"),
            },
            TerminalSessionFailure::Runtime(failure) => failure.clone(),
            TerminalSessionFailure::PtyRead { .. } => Self::pty("read-shell-output"),
            TerminalSessionFailure::ShellWait { .. } => Self::pty("reap-shell-process"),
        }
    }

    pub(crate) const fn class(&self) -> FailureClass {
        self.class
    }

    pub(crate) const fn recoverability(&self) -> Recoverability {
        self.recoverability
    }

    pub(crate) const fn operation(&self) -> &'static str {
        self.operation
    }

    pub(crate) const fn reason(&self) -> Option<FailureReason> {
        self.reason
    }

    pub(crate) const fn is_fatal(&self) -> bool {
        matches!(self.recoverability, Recoverability::Fatal)
    }
}

impl fmt::Display for TerminalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let action = match self.recoverability {
            Recoverability::Recoverable => "The last valid frame is preserved; retry the action.",
            Recoverability::Fatal => "Close this Pane and restart the terminal command.",
        };
        write!(
            formatter,
            "{} failed during {}. {action}",
            self.class, self.operation
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum PaneTerminalState {
    #[default]
    Running,
    Exited(TerminalSessionExit),
    Failed {
        failure: TerminalFailure,
        last_valid_frame: Option<PresentationGeneration>,
    },
}

impl PaneTerminalState {
    pub(crate) const fn exited(exit: TerminalSessionExit) -> Self {
        Self::Exited(exit)
    }

    pub(crate) const fn failed(
        failure: TerminalFailure,
        last_valid_frame: Option<PresentationGeneration>,
    ) -> Self {
        Self::Failed {
            failure,
            last_valid_frame,
        }
    }

    pub(crate) const fn failure(&self) -> Option<&TerminalFailure> {
        match self {
            Self::Failed { failure, .. } => Some(failure),
            Self::Running | Self::Exited(_) => None,
        }
    }

    pub(crate) const fn last_valid_frame(&self) -> Option<PresentationGeneration> {
        match self {
            Self::Failed {
                last_valid_frame, ..
            } => *last_valid_frame,
            Self::Running | Self::Exited(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiagnosticKeyEventKind {
    KeyDown,
    KeyUp,
    FlagsChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UnhandledKeyDiagnostic {
    kind: DiagnosticKeyEventKind,
    action: KeyAction,
    native_key_code: Option<u16>,
}

impl UnhandledKeyDiagnostic {
    pub(crate) const fn new(
        kind: DiagnosticKeyEventKind,
        action: KeyAction,
        native_key_code: Option<u16>,
    ) -> Self {
        Self {
            kind,
            action,
            native_key_code,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DiagnosticEvent {
    Failure {
        class: FailureClass,
        recoverability: Recoverability,
        operation: &'static str,
        reason: Option<FailureReason>,
    },
    UnhandledKey(UnhandledKeyDiagnostic),
}

impl DiagnosticEvent {
    fn encode(&self) -> String {
        match self {
            Self::Failure {
                class,
                recoverability,
                operation,
                reason,
            } => {
                let mut encoded = format!(
                    "class={class:?} recoverability={recoverability:?} operation={operation}"
                );
                if let Some(reason) = reason {
                    use std::fmt::Write;
                    let _ = write!(encoded, " reason={reason:?}");
                }
                encoded.push('\n');
                encoded
            }
            Self::UnhandledKey(event) => match event.native_key_code {
                Some(native_key_code) => format!(
                    "event=UnhandledKey kind={:?} action={:?} native_key_code={native_key_code}\n",
                    event.kind, event.action
                ),
                None => format!(
                    "event=UnhandledKey kind={:?} action={:?} native_key_code=none\n",
                    event.kind, event.action
                ),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiagnosticRecord {
    sequence: u64,
    elapsed_ms: u128,
    event: DiagnosticEvent,
}

impl DiagnosticRecord {
    fn encode(&self) -> String {
        format!(
            "sequence={} elapsed_ms={} {}",
            self.sequence,
            self.elapsed_ms,
            self.event.encode()
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiagnosticBundle {
    records: VecDeque<DiagnosticRecord>,
    started: Instant,
    next_sequence: u64,
}

impl Default for DiagnosticBundle {
    fn default() -> Self {
        Self {
            records: VecDeque::new(),
            started: Instant::now(),
            next_sequence: 1,
        }
    }
}

impl DiagnosticBundle {
    pub(crate) const MAX_RECORDS: usize = 128;
    pub(crate) const MAX_BYTES: usize = 64 * 1024;

    fn header(&self, skipped: usize) -> String {
        let omitted = self.next_sequence - 1 - self.records.len() as u64 + skipped as u64;
        format!(
            "SpaceTerm diagnostics\nschema=2\nbuild_version={}\nos={}\narch={}\nnetwork_telemetry=false\nterminal_content=false\nrecords_omitted={omitted}\n",
            env!("SPACETERM_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    }

    pub(crate) fn record(&mut self, failure: &TerminalFailure) {
        self.push(DiagnosticEvent::Failure {
            class: failure.class(),
            recoverability: failure.recoverability(),
            operation: failure.operation(),
            reason: failure.reason(),
        });
    }

    pub(crate) fn record_unhandled_key(&mut self, event: UnhandledKeyDiagnostic) {
        self.push(DiagnosticEvent::UnhandledKey(event));
    }

    fn push(&mut self, event: DiagnosticEvent) {
        self.records.push_back(DiagnosticRecord {
            sequence: self.next_sequence,
            elapsed_ms: self.started.elapsed().as_millis(),
            event,
        });
        self.next_sequence += 1;
        while self.records.len() > Self::MAX_RECORDS || self.encoded_len() > Self::MAX_BYTES {
            self.records.pop_front();
        }
    }

    pub(crate) fn record_count(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn encoded_len(&self) -> usize {
        self.header(0).len()
            + self
                .records
                .iter()
                .map(|record| record.encode().len())
                .sum::<usize>()
    }

    fn encode_from(&self, skipped: usize) -> String {
        let mut text = self.header(skipped);
        for record in self.records.iter().skip(skipped) {
            text.push_str(&record.encode());
        }
        text
    }

    /// Keeps the newest complete records and states how many earlier records were omitted.
    pub(crate) fn report_text(&self, max_bytes: usize) -> String {
        let mut skipped = 0;
        loop {
            let text = self.encode_from(skipped);
            if text.len() <= max_bytes || skipped == self.records.len() {
                return text;
            }
            skipped += 1;
        }
    }

    pub(crate) fn export(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.encode_from(0))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::terminal::{PresentationGeneration, TerminalSessionFailure};

    fn diagnostic_records(text: &str) -> Vec<(u64, u128, &str)> {
        text.lines()
            .filter(|line| line.starts_with("sequence="))
            .map(|line| {
                let mut fields = line.splitn(3, ' ');
                let sequence = fields
                    .next()
                    .unwrap()
                    .strip_prefix("sequence=")
                    .unwrap()
                    .parse()
                    .unwrap();
                let elapsed = fields
                    .next()
                    .unwrap()
                    .strip_prefix("elapsed_ms=")
                    .unwrap()
                    .parse()
                    .unwrap();
                (sequence, elapsed, fields.next().unwrap())
            })
            .collect()
    }

    #[test]
    fn normal_exit_and_every_failure_class_are_distinguishable() {
        let states = [
            PaneTerminalState::exited(crate::terminal::TerminalSessionExit::Success),
            PaneTerminalState::failed(TerminalFailure::pty("read"), None),
            PaneTerminalState::failed(TerminalFailure::emulator("feed"), None),
            PaneTerminalState::failed(TerminalFailure::presentation("prepare"), None),
            PaneTerminalState::failed(TerminalFailure::platform("pasteboard"), None),
            PaneTerminalState::failed(TerminalFailure::resource("glyph-cache"), None),
        ];
        assert!(matches!(states[0], PaneTerminalState::Exited(_)));
        assert_eq!(
            states[1..]
                .iter()
                .filter_map(PaneTerminalState::failure)
                .map(TerminalFailure::class)
                .collect::<Vec<_>>(),
            vec![
                FailureClass::Pty,
                FailureClass::Emulator,
                FailureClass::Presentation,
                FailureClass::Platform,
                FailureClass::Resource,
            ]
        );
    }

    #[test]
    fn session_mapping_preserves_typed_emulator_failure_reasons() {
        for (error, reason) in [
            (
                libghostty_vt::Error::OutOfMemory,
                FailureReason::OutOfMemory,
            ),
            (
                libghostty_vt::Error::InvalidValue,
                FailureReason::InvalidValue,
            ),
            (
                libghostty_vt::Error::OutOfSpace { required: 512 },
                FailureReason::OutOfSpace,
            ),
        ] {
            let failure = TerminalFailure::from_session(&TerminalSessionFailure::Runtime(
                TerminalFailure::emulator_error("produce-terminal-screen-snapshot", error),
            ));
            assert_eq!(
                (failure.class(), failure.operation(), failure.reason()),
                (
                    FailureClass::Emulator,
                    "produce-terminal-screen-snapshot",
                    Some(reason)
                )
            );
        }
    }

    #[test]
    fn resource_failure_state_retains_generation_and_recovery_guidance() {
        let state = PaneTerminalState::failed(
            TerminalFailure::resource("atlas"),
            Some(PresentationGeneration::test(42)),
        );
        assert_eq!(
            state.last_valid_frame(),
            Some(PresentationGeneration::test(42))
        );
        assert_eq!(
            state.failure().map(TerminalFailure::recoverability),
            Some(Recoverability::Recoverable)
        );
        assert!(
            state
                .failure()
                .unwrap()
                .to_string()
                .contains("retry the action")
        );
        assert!(
            TerminalFailure::pty("read")
                .to_string()
                .contains("Close this Pane")
        );
    }

    #[test]
    fn diagnostic_ring_is_bounded_and_export_writes_schema() {
        let mut bundle = DiagnosticBundle::default();
        for _ in 0..200 {
            bundle.record(&TerminalFailure::platform("native-event"));
        }
        assert_eq!(bundle.record_count(), 128);
        assert!(bundle.encoded_len() <= 65_536);
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-diagnostics-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("diagnostics.txt");
        assert!(!path.exists());
        bundle.export(&path).unwrap();
        let exported = fs::read_to_string(&path).unwrap();
        assert!(exported.contains("schema=2\n"));
        assert!(exported.contains(&format!("build_version={}\n", env!("SPACETERM_VERSION"))));
        assert!(exported.contains(&format!(
            "os={}\narch={}\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        )));
        assert!(
            exported
                .contains("network_telemetry=false\nterminal_content=false\nrecords_omitted=72\n")
        );
        let records = diagnostic_records(&exported);
        assert_eq!(records.len(), 128);
        for (index, (sequence, _, event)) in records.iter().enumerate() {
            assert_eq!(*sequence, 73 + index as u64);
            assert_eq!(
                *event,
                "class=Platform recoverability=Recoverable operation=native-event"
            );
        }
        assert!(records.windows(2).all(|pair| pair[0].1 <= pair[1].1));

        static LONG_OPERATION: [u8; 1024] = [b'x'; 1024];
        let long_operation = std::str::from_utf8(&LONG_OPERATION).unwrap();
        let mut bytes = DiagnosticBundle::default();
        for _ in 0..128 {
            bytes.record(&TerminalFailure::platform(long_operation));
        }
        assert!(bytes.record_count() < 128);
        assert!(bytes.encoded_len() <= 65_536);
        bytes.export(&path).unwrap();
        let exported = fs::read_to_string(&path).unwrap();
        assert_eq!(exported.len(), bytes.encoded_len());
        let records = diagnostic_records(&exported);
        assert_eq!(records.len(), bytes.record_count());
        assert_eq!(records.last().unwrap().0, 128);
        assert!(
            records
                .iter()
                .all(|(_, _, event)| event.ends_with(long_operation))
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unhandled_key_diagnostics_export_only_privacy_safe_event_identity() {
        let mut bundle = DiagnosticBundle::default();
        bundle.record_unhandled_key(UnhandledKeyDiagnostic::new(
            DiagnosticKeyEventKind::KeyDown,
            crate::terminal::KeyAction::Press,
            Some(u16::MAX),
        ));
        bundle.record(
            &TerminalFailure::pty("write-shell-input")
                .with_reason(FailureReason::Io(std::io::ErrorKind::BrokenPipe)),
        );

        let directory = std::env::temp_dir().join(format!(
            "spaceterm-unhandled-key-diagnostics-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("diagnostics.txt");
        bundle.export(&path).unwrap();
        let exported = fs::read_to_string(&path).unwrap();

        let records = diagnostic_records(&exported);
        assert_eq!(
            records
                .iter()
                .map(|(sequence, _, event)| (*sequence, *event))
                .collect::<Vec<_>>(),
            [
                (
                    1,
                    "event=UnhandledKey kind=KeyDown action=Press native_key_code=65535"
                ),
                (
                    2,
                    "class=Pty recoverability=Fatal operation=write-shell-input reason=Io(BrokenPipe)"
                ),
            ]
        );
        assert!(records[0].1 <= records[1].1);
        fs::remove_dir_all(directory).unwrap();
    }
}
