//! Work protocol contracts and the Core's admission transport.

mod admission;
pub mod capability;

pub(crate) use admission::*;
pub use spaceterm_protocol::protocol::{
    AccessibilityAck, AccessibilityMessage, Acknowledge, AgentName, Cancel, Channel, Claim,
    ClaimReply, ClientMessage, CoreMessage, CoreVersions, DecodeError, EffectDone, Fatal, Frame,
    FrameAck, HistoryPosition, Input, InputEvent, InputOutcome, InvalidAgentName,
    InventoryRevision, Kind, Lane, MAJOR, MAX_RUN_COMMAND_BYTES, Negotiated, ObserveReply,
    Operation, Page, PaneEvent, PaneMessage, Ping, Pong, RawFrame, Refusal, Reply, ReplyBody,
    Request, RequestId, Restored, RunDoubt, RunId, RunReceipt, RunRefusal, RunReply, RunState,
    ScreenUpdate, SessionState, Takeover, View, WireMessage, frame, inventory_1_0, limits, message,
    operation, screen, version,
};
