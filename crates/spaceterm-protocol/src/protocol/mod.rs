//! SpaceTerm Protocol messages, framing and bounds.
//!
//! Admission keeps its Hello/Offer preamble in `admission`, which every major reads the same way
//! so an incompatible peer learns which side is older ([`version::Incompatible`]). After admission, both peers exchange
//! postcard messages in `len:u32be | kind:u8 | body` frames ([`frame`]). Nothing after admission
//! is JSON.
pub mod admission;
pub mod capability;
pub mod frame;
pub mod inventory_1_0;
pub mod limits;
pub mod message;
pub mod operation;
pub mod screen;
#[cfg(test)]
mod tests;
pub use crate::version;

pub use crate::version::MAJOR;
pub use admission::{AgentName, CoreVersions, InvalidAgentName, Lane, Negotiated};
pub use frame::{DecodeError, Kind, RawFrame, WireMessage};
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
