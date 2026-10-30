//! SpaceTerm Protocol messages, framing and bounds.
//!
//! Admission keeps its Hello/Offer preamble in `admission`, which every major reads the same way
//! so an incompatible peer receives `IncompatibleProtocol`. After admission, both peers exchange
//! postcard messages in `len:u32be | kind:u8 | body` frames ([`frame`]). Nothing after admission
//! is JSON.
mod admission;
pub mod frame;
pub mod limits;
pub mod message;
pub mod operation;
pub mod screen;
#[cfg(test)]
mod tests;

pub(crate) use admission::*;
pub use admission::{AgentName, InvalidAgentName, Lane, MAJOR};
pub use frame::{DecodeError, Kind, RawFrame, WireMessage};
pub use message::{
    Acknowledge, Cancel, Channel, ClientMessage, EffectDone, Fatal, Frame, FrameAck, Input,
    InputEvent, InputOutcome, InventoryRevision, PaneEvent, PaneMessage, Ping, Pong, Refusal,
    Reply, Request, RequestId, ServerMessage, View,
};
pub use operation::{
    Claim, ClaimReply, HistoryPosition, MAX_RUN_COMMAND_BYTES, ObserveReply, Operation, ReplyBody,
    Restored, RunDoubt, RunId, RunReceipt, RunRefusal, RunReply, RunState, SessionState, Takeover,
};
pub use screen::{Page, ScreenUpdate};
