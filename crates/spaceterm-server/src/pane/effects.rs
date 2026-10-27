//! Bounded native requests. These values carry no client-local file authority.
use serde::{Deserialize, Serialize};

pub const MAX_SCREEN_HYPERLINKS: usize = 1024;
pub const MAX_SCREEN_HYPERLINK_BYTES: usize = 256 * 1024;

pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum ClipboardTarget {
    Default,
    Standard,
    Selection,
    Primary,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Effect {
    ClipboardRead {
        target: ClipboardTarget,
    },
    ClipboardWrite {
        target: ClipboardTarget,
        text: String,
    },
    PermissionSetup {
        screen_recording: bool,
        accessibility: bool,
    },
    Bell,
    CommandFinished {
        exit_status: Option<i32>,
        duration_ms: u64,
    },
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ClipboardRead { .. } => "ClipboardRead { .. }",
            Self::ClipboardWrite { .. } => "ClipboardWrite { .. }",
            Self::PermissionSetup { .. } => "PermissionSetup { .. }",
            Self::Bell => "Bell",
            Self::CommandFinished { .. } => "CommandFinished { .. }",
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub generation: u64,
    pub remaining_ms: u32,
    pub effect: Effect,
}

/// The native effect one output check took for the focus generation it named.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Poll {
    pub request: Option<Request>,
    pub hidden_input: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Completion {
    Denied,
    Written,
    Text(String),
}

impl std::fmt::Debug for Completion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Denied => "Denied",
            Self::Written => "Written",
            Self::Text(_) => "Text { .. }",
        })
    }
}

/// File values become local targets only after validation by an explicitly composed client adapter.
#[derive(Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Hyperlink {
    Url(String),
    File(String),
}
impl std::fmt::Debug for Hyperlink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Url(_) => "Url { .. }",
            Self::File(_) => "File { .. }",
        })
    }
}
