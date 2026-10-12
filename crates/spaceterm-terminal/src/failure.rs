use std::fmt;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureClass {
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
pub enum Recoverability {
    Recoverable,
    Fatal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureReason {
    Io(std::io::ErrorKind),
    OutOfMemory,
    InvalidValue,
    OutOfSpace,
    InvalidInput,
    InvalidUtf8,
    CapacityExceeded,
    ChannelClosed,
}

#[cfg(feature = "engine")]
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
pub struct TerminalFailure {
    class: FailureClass,
    recoverability: Recoverability,
    operation: &'static str,
    reason: Option<FailureReason>,
}

impl TerminalFailure {
    pub const fn pty(operation: &'static str) -> Self {
        Self::new(FailureClass::Pty, Recoverability::Fatal, operation)
    }

    pub const fn emulator(operation: &'static str) -> Self {
        Self::new(FailureClass::Emulator, Recoverability::Fatal, operation)
    }

    #[cfg(feature = "engine")]
    pub fn emulator_error(operation: &'static str, error: libghostty_vt::Error) -> Self {
        Self::emulator(operation).with_reason(error.into())
    }

    pub const fn with_reason(mut self, reason: FailureReason) -> Self {
        self.reason = Some(reason);
        self
    }

    pub const fn presentation(operation: &'static str) -> Self {
        Self::new(
            FailureClass::Presentation,
            Recoverability::Recoverable,
            operation,
        )
    }

    pub const fn platform(operation: &'static str) -> Self {
        Self::new(
            FailureClass::Platform,
            Recoverability::Recoverable,
            operation,
        )
    }

    pub const fn resource(operation: &'static str) -> Self {
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

    pub fn from_session(failure: &impl SessionFailureFacts) -> Self {
        failure.terminal_failure()
    }

    pub const fn class(&self) -> FailureClass {
        self.class
    }

    pub const fn recoverability(&self) -> Recoverability {
        self.recoverability
    }

    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    pub const fn reason(&self) -> Option<FailureReason> {
        self.reason
    }

    pub const fn is_fatal(&self) -> bool {
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

pub trait SessionFailureFacts {
    fn terminal_failure(&self) -> TerminalFailure;
}
