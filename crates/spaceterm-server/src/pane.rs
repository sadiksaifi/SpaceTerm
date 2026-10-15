//! Optional Development terminal operations. No native effects or file authority cross this API.
use crate::ServerError;
use serde::{Deserialize, Serialize};

pub const MAX_COLUMNS: u16 = 240;
pub const MAX_ROWS: u16 = 100;
pub const MAX_TERMINAL_FRAME: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSize {
    pub columns: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}
impl PaneSize {
    pub fn validate(self) -> Result<Self, ServerError> {
        if !(2..=MAX_COLUMNS).contains(&self.columns)
            || !(2..=MAX_ROWS).contains(&self.rows)
            || !(1..=256).contains(&self.cell_width)
            || !(1..=256).contains(&self.cell_height)
        {
            return Err(ServerError::Unsupported);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub server: [u8; 32],
    pub session: [u8; 32],
    pub control: [u8; 32],
    pub connection: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Key {
    pub physical: u32,
    pub action: u8,
    pub logical: String,
    pub text: Option<String>,
    pub unshifted: Option<char>,
    pub modifiers: u16,
    pub consumed: u16,
    pub option_as_alt: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresentationStyle {
    pub light: bool,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub palette: [[u8; 3]; 16],
    pub cursor: [u8; 3],
    pub cursor_text: Option<[u8; 3]>,
    pub bold_as_bright: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Capabilities,
    Attach {
        create: bool,
        size: PaneSize,
        resume: Option<Lease>,
        style: Option<PresentationStyle>,
    },
    Poll {
        lease: Lease,
    },
    Key {
        lease: Lease,
        key: Key,
    },
    Resize {
        lease: Lease,
        size: PaneSize,
    },
    Style {
        lease: Lease,
        style: PresentationStyle,
    },
    Close {
        lease: Lease,
    },
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    Capabilities { development_pane: bool },
    Attached { lease: Lease },
    Screen(Box<Screen>),
    Unchanged,
    Ended,
    Closed,
    Refused(Refusal),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Refusal {
    Controlled,
    Missing,
    Stale,
    Unsupported,
    Failed,
    CleanupUnresolved,
}
impl From<Refusal> for ServerError {
    fn from(value: Refusal) -> Self {
        match value {
            Refusal::Controlled => Self::Controlled,
            Refusal::Missing => Self::SessionUnavailable,
            Refusal::Stale => Self::StaleControl,
            Refusal::Unsupported => Self::Unsupported,
            Refusal::Failed => Self::StartupFailed,
            Refusal::CleanupUnresolved => Self::CleanupUnresolved,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Color {
    Default,
    Palette(u8),
    Rgb([u8; 3]),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cell {
    pub text: String,
    pub foreground: Color,
    pub background: Color,
    pub underline_color: Color,
    pub flags: u16,
    pub underline: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Screen {
    pub columns: u16,
    pub rows: Vec<Vec<Cell>>,
    pub wrapped: Vec<bool>,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub palette: Vec<[u8; 3]>,
    pub reversed: bool,
    pub foreground_overridden: bool,
    pub background_overridden: bool,
    pub cursor_overridden: bool,
    pub palette_overrides: Vec<bool>,
    pub cursor: Option<(u16, u16, u8)>,
    pub cursor_visible: bool,
    pub cursor_blinking: bool,
    pub cursor_shape: u8,
    pub cursor_color: [u8; 3],
    pub cursor_text: [u8; 3],
    pub alternate: bool,
}

/// The host supplies runtime ownership; the protocol supplies authenticated device/connection facts.
pub trait PaneService: Send {
    fn request(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply;
    fn disconnected(&mut self, connection: [u8; 32]);
    fn shutdown(&mut self) -> Result<(), ServerError>;
}

#[cfg(feature = "client")]
pub struct PaneConnection {
    pub(crate) stream: std::os::unix::net::UnixStream,
}
#[cfg(feature = "client")]
impl PaneConnection {
    pub fn exchange(&mut self, request: &Request) -> Result<Reply, ServerError> {
        let mut wire = crate::protocol::Wire::terminal(&mut self.stream)?;
        wire.write(request)?;
        match wire.read()? {
            Reply::Refused(reason) => Err(reason.into()),
            reply => Ok(reply),
        }
    }
}
