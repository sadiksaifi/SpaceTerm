//! SpaceTerm Protocol messages, framing and bounds.
//!
//! Admission keeps its Hello/Offer preamble in `admission`, which every major reads the same way
//! so an incompatible peer learns which side is older ([`version::Incompatible`]). After admission, both peers exchange
//! postcard messages in `len:u32be | kind:u8 | body` frames ([`frame`]). Nothing after admission
//! is JSON.
mod admission;
pub mod capability;
pub mod frame;
pub mod inventory_1_0;
pub mod limits;
mod major;
pub mod message;
pub mod operation;
pub mod screen;
#[cfg(test)]
mod tests;
pub mod version;

pub(crate) use admission::*;
pub use admission::{AgentName, CoreVersions, InvalidAgentName, Lane, Negotiated};
pub use frame::{DecodeError, Kind, RawFrame, WireMessage};
pub use major::MAJOR;
pub use message::{
    AccessibilityAck, AccessibilityMessage, Acknowledge, Cancel, Channel, ClientMessage,
    CoreMessage, EffectDone, Fatal, Frame, FrameAck, Input, InputEvent, InputOutcome,
    InventoryRevision, PaneEvent, PaneMessage, Ping, Pong, Refusal, Reply, Request, RequestId,
    View,
};
pub use operation::{
    Claim, ClaimReply, HistoryPosition, MAX_RUN_COMMAND_BYTES, ObserveReply, Operation, ReplyBody,
    Restored, RunDoubt, RunId, RunReceipt, RunRefusal, RunReply, RunState, SessionState, Takeover,
};
pub use screen::{Page, ScreenUpdate};
