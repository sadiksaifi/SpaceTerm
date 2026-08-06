use std::collections::VecDeque;
use std::fmt;
use std::path::Path;

use super::emulator::PresentationGeneration;
use super::session::{SessionExit, SessionFailure, SessionStartupStage};

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
            Self::Platform => "macOS integration",
            Self::Resource => "renderer resource",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Recoverability {
    Recoverable,
    Fatal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalFailure {
    class: FailureClass,
    recoverability: Recoverability,
    operation: &'static str,
}

impl TerminalFailure {
    pub(crate) const fn pty(operation: &'static str) -> Self {
        Self::new(FailureClass::Pty, Recoverability::Fatal, operation)
    }

    pub(crate) const fn emulator(operation: &'static str) -> Self {
        Self::new(FailureClass::Emulator, Recoverability::Fatal, operation)
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
        }
    }

    pub(crate) fn from_session(failure: &SessionFailure) -> Self {
        match failure {
            SessionFailure::Startup { stage, .. } => match stage {
                SessionStartupStage::Pty
                | SessionStartupStage::Reader
                | SessionStartupStage::ReaderThread => Self::pty("session-startup"),
                SessionStartupStage::Emulator => Self::emulator("session-startup"),
            },
            SessionFailure::Runtime(_) => Self::emulator("session-runtime"),
            SessionFailure::PtyRead { .. } => Self::pty("read-shell-output"),
            SessionFailure::ShellWait { .. } => Self::pty("reap-shell-process"),
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
    Exited(SessionExit),
    Failed {
        failure: TerminalFailure,
        last_valid_frame: Option<PresentationGeneration>,
    },
}

impl PaneTerminalState {
    pub(crate) const fn exited(exit: SessionExit) -> Self {
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiagnosticRecord {
    class: FailureClass,
    recoverability: Recoverability,
    operation: &'static str,
}

impl DiagnosticRecord {
    fn encode(&self) -> String {
        format!(
            "class={:?} recoverability={:?} operation={}\n",
            self.class, self.recoverability, self.operation
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DiagnosticBundle {
    records: VecDeque<DiagnosticRecord>,
}

impl DiagnosticBundle {
    pub(crate) const MAX_RECORDS: usize = 128;
    pub(crate) const MAX_BYTES: usize = 64 * 1024;
    const HEADER: &'static str =
        "SpaceTerm diagnostics\nnetwork_telemetry=false\nterminal_content=false\n";

    pub(crate) fn record(&mut self, failure: &TerminalFailure) {
        self.records.push_back(DiagnosticRecord {
            class: failure.class(),
            recoverability: failure.recoverability(),
            operation: failure.operation(),
        });
        while self.records.len() > Self::MAX_RECORDS || self.encoded_len() > Self::MAX_BYTES {
            self.records.pop_front();
        }
    }

    pub(crate) fn record_count(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn encoded_len(&self) -> usize {
        Self::HEADER.len()
            + self
                .records
                .iter()
                .map(|record| record.encode().len())
                .sum::<usize>()
    }

    pub(crate) fn export(&self, path: &Path) -> std::io::Result<()> {
        let mut encoded = String::with_capacity(self.encoded_len());
        encoded.push_str(Self::HEADER);
        for record in &self.records {
            encoded.push_str(&record.encode());
        }
        std::fs::write(path, encoded)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::terminal::{PresentationGeneration, SessionFailure};

    #[test]
    fn normal_exit_and_every_failure_class_are_distinguishable() {
        let states = [
            PaneTerminalState::exited(crate::terminal::SessionExit::Success),
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
    fn session_mapping_redacts_raw_terminal_content_and_secrets() {
        let failure = TerminalFailure::from_session(&SessionFailure::Runtime(
            "password=hunter2 output=private terminal text".to_owned(),
        ));
        let rendered = failure.to_string();
        assert_eq!(failure.class(), FailureClass::Emulator);
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("private terminal text"));
        assert!(rendered.contains("restart"));
    }

    #[test]
    fn renderer_failure_keeps_last_valid_generation() {
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
    fn diagnostics_are_bounded_local_and_exported_only_explicitly() {
        let mut bundle = DiagnosticBundle::default();
        for _ in 0..200 {
            bundle.record(&TerminalFailure::platform("native-event"));
        }
        assert!(bundle.record_count() <= DiagnosticBundle::MAX_RECORDS);
        assert!(bundle.encoded_len() <= DiagnosticBundle::MAX_BYTES);

        let directory =
            std::env::temp_dir().join(format!("spaceterm-diagnostics-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("diagnostics.txt");
        assert!(!path.exists());
        bundle.export(&path).unwrap();
        let exported = fs::read_to_string(&path).unwrap();
        assert!(exported.contains("network_telemetry=false"));
        assert!(!exported.contains("terminal text"));
        fs::remove_dir_all(directory).unwrap();
    }
}
