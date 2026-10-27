//! SpaceTerm Protocol messages, framing and bounds.
//!
//! Admission keeps its Hello/Offer preamble in `admission`, which every major reads the same way
//! so an incompatible peer receives `IncompatibleProtocol`. After admission, both peers exchange
//! postcard messages in `len:u32be | kind:u8 | body` frames ([`frame`]). Nothing after admission
//! is JSON.
mod admission;
pub mod frame;
pub mod interim;
pub mod limits;
pub mod message;
pub mod operation;
pub mod screen;
#[cfg(test)]
mod tests;

pub(crate) use admission::*;
pub use frame::{DecodeError, Kind, RawFrame, WireMessage};
pub use message::{
    Acknowledge, Cancel, Channel, ClientMessage, EffectDone, Fatal, Frame, FrameAck, Input,
    InputEvent, InputOutcome, InventoryRevision, PaneEvent, PaneMessage, Ping, Pong, Refusal,
    Reply, Request, RequestId, ServerMessage, View,
};
pub use operation::{Claim, ClaimReply, Operation, ReplyBody, Restored, Takeover};
pub use screen::{Page, ScreenUpdate};

/// The protocol major of the pushed, multiplexed exchange in this module.
///
/// Admission still selects `MAJOR` (1) for the request-and-poll exchange. The transport cutover
/// sets `MAJOR` to this value and deletes this constant.
pub const PUSH_MAJOR: u16 = 2;
