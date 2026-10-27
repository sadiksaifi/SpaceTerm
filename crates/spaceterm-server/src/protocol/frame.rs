//! Frames: `len:u32be | kind:u8 | body`.
//!
//! `len` counts the bytes after it: the kind byte and the body. The kind's low seven bits name the
//! message; the high bit marks a frame that more frames of the same kind continue. A writer
//! builds each frame in one buffer and never starts a message of a kind while another message of
//! that kind is partly written. Over IPC frames run back to back; over WSS one binary message
//! carries exactly one frame.
//!
//! Decoding is bounded per kind before allocation, and errors never carry peer bytes.
use super::limits;
use super::message::{
    Acknowledge, Cancel, ClientMessage, EffectDone, Fatal, Frame, FrameAck, Input,
    InventoryRevision, PaneMessage, Ping, Pong, Reply, Request, ServerMessage, View,
};
use serde::{Serialize, de::DeserializeOwned};

const MORE: u8 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
    Request = 0x01,
    Input = 0x02,
    FrameAck = 0x03,
    View = 0x04,
    EffectDone = 0x05,
    Acknowledge = 0x06,
    Cancel = 0x07,
    ClientPing = 0x08,
    ClientPong = 0x09,
    Reply = 0x41,
    /// A reply written at L2 in chunks of at most [`limits::BULK_CHUNK_BYTES`].
    BulkReply = 0x42,
    Frame = 0x43,
    Pane = 0x44,
    Inventory = 0x45,
    ServerPing = 0x46,
    ServerPong = 0x47,
    Goodbye = 0x48,
}

impl Kind {
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte & !MORE {
            0x01 => Self::Request,
            0x02 => Self::Input,
            0x03 => Self::FrameAck,
            0x04 => Self::View,
            0x05 => Self::EffectDone,
            0x06 => Self::Acknowledge,
            0x07 => Self::Cancel,
            0x08 => Self::ClientPing,
            0x09 => Self::ClientPong,
            0x41 => Self::Reply,
            0x42 => Self::BulkReply,
            0x43 => Self::Frame,
            0x44 => Self::Pane,
            0x45 => Self::Inventory,
            0x46 => Self::ServerPing,
            0x47 => Self::ServerPong,
            0x48 => Self::Goodbye,
            _ => return None,
        })
    }

    /// Whether clients send this kind. Servers send every other kind.
    pub fn from_client(self) -> bool {
        (self as u8) < 0x40
    }

    /// The largest message of this kind across all of its frames.
    pub fn max_message(self) -> usize {
        match self {
            Self::Request => limits::MAX_REQUEST_BYTES,
            Self::Input => limits::MAX_INPUT_BYTES,
            Self::EffectDone => limits::MAX_EFFECT_DONE_BYTES,
            Self::FrameAck
            | Self::View
            | Self::Acknowledge
            | Self::Cancel
            | Self::ClientPing
            | Self::ClientPong
            | Self::ServerPing
            | Self::ServerPong
            | Self::Goodbye => limits::MAX_CONTROL_BYTES,
            Self::Reply | Self::BulkReply => limits::MAX_REPLY_BYTES,
            Self::Frame => limits::MAX_UPDATE_BYTES,
            Self::Pane => limits::MAX_PANE_EVENT_BYTES,
            Self::Inventory => limits::MAX_INVENTORY_BYTES,
        }
    }

    /// The largest body one frame of this kind carries.
    pub fn max_frame_body(self) -> usize {
        match self {
            Self::BulkReply => limits::BULK_CHUNK_BYTES,
            _ => limits::MAX_FRAME_BODY,
        }
    }
}

/// Typed decode failures. They carry no peer bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("frame exceeds its limit")]
    Oversized,
    #[error("unknown frame kind")]
    UnknownKind,
    #[error("frame kind sent by the wrong peer")]
    WrongDirection,
    #[error("malformed frame")]
    Malformed,
    #[error("message exceeds a protocol bound")]
    OutOfBounds,
}

impl From<DecodeError> for Fatal {
    fn from(_: DecodeError) -> Self {
        Self::ProtocolViolation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("message exceeds its limit")]
    TooLarge,
    #[error("message has no wire form")]
    Unencodable,
}

/// One frame as received: the kind byte, including the continuation bit, and the body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawFrame {
    pub kind: u8,
    pub body: Vec<u8>,
}

impl RawFrame {
    /// Parses exactly one whole frame, as one WSS binary message carries.
    pub fn parse(bytes: &[u8]) -> Result<Self, DecodeError> {
        let (frame, used) = parse_prefix(bytes)?.ok_or(DecodeError::Malformed)?;
        if used != bytes.len() {
            return Err(DecodeError::Malformed);
        }
        Ok(frame)
    }

    fn kind(&self) -> Result<Kind, DecodeError> {
        Kind::from_byte(self.kind).ok_or(DecodeError::UnknownKind)
    }

    fn continued(&self) -> bool {
        self.kind & MORE != 0
    }
}

fn parse_prefix(bytes: &[u8]) -> Result<Option<(RawFrame, usize)>, DecodeError> {
    let Some(header) = bytes.get(..limits::HEADER_BYTES) else {
        return Ok(None);
    };
    let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if len == 0 {
        return Err(DecodeError::Malformed);
    }
    let kind = Kind::from_byte(header[4]).ok_or(DecodeError::UnknownKind)?;
    if len - 1 > kind.max_frame_body() {
        return Err(DecodeError::Oversized);
    }
    let end = 4 + len;
    let Some(body) = bytes.get(limits::HEADER_BYTES..end) else {
        return Ok(None);
    };
    Ok(Some((
        RawFrame {
            kind: header[4],
            body: body.to_vec(),
        },
        end,
    )))
}

/// One message as whole frames, each ready for one write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireMessage {
    pub kind: Kind,
    pub frames: Vec<Vec<u8>>,
}

impl WireMessage {
    pub fn bytes(&self) -> usize {
        self.frames.iter().map(Vec::len).sum()
    }
}

pub fn encode_client(message: &ClientMessage) -> Result<WireMessage, EncodeError> {
    match message {
        ClientMessage::Request(value) => encode(Kind::Request, value),
        ClientMessage::Input(value) => encode(Kind::Input, value),
        ClientMessage::FrameAck(value) => encode(Kind::FrameAck, value),
        ClientMessage::View(value) => encode(Kind::View, value),
        ClientMessage::EffectDone(value) => encode(Kind::EffectDone, value),
        ClientMessage::Acknowledge(value) => encode(Kind::Acknowledge, value),
        ClientMessage::Cancel(value) => encode(Kind::Cancel, value),
        ClientMessage::Ping(value) => encode(Kind::ClientPing, value),
        ClientMessage::Pong(value) => encode(Kind::ClientPong, value),
    }
}

pub fn encode_server(message: &ServerMessage) -> Result<WireMessage, EncodeError> {
    match message {
        ServerMessage::Reply(value) => {
            let bulk = matches!(&value.result, Ok(body) if body.bulk());
            encode(if bulk { Kind::BulkReply } else { Kind::Reply }, value)
        }
        ServerMessage::Frame(value) => encode(Kind::Frame, value),
        ServerMessage::Pane(value) => encode(Kind::Pane, value),
        ServerMessage::Inventory(value) => encode(Kind::Inventory, value),
        ServerMessage::Ping(value) => encode(Kind::ServerPing, value),
        ServerMessage::Pong(value) => encode(Kind::ServerPong, value),
        ServerMessage::Goodbye(value) => encode(Kind::Goodbye, value),
    }
}

/// Serializes into the first frame's buffer, and splits only a message larger than one frame.
fn encode(kind: Kind, value: &impl Serialize) -> Result<WireMessage, EncodeError> {
    let mut buffer = postcard::to_extend(value, vec![0; limits::HEADER_BYTES])
        .map_err(|_| EncodeError::Unencodable)?;
    let body = buffer.len() - limits::HEADER_BYTES;
    if body > kind.max_message() {
        return Err(EncodeError::TooLarge);
    }
    let chunk = kind.max_frame_body();
    if body <= chunk {
        write_header(&mut buffer[..limits::HEADER_BYTES], kind as u8, body);
        return Ok(WireMessage {
            kind,
            frames: vec![buffer],
        });
    }
    let mut parts = buffer[limits::HEADER_BYTES..].chunks(chunk).peekable();
    let mut frames = Vec::with_capacity(body.div_ceil(chunk));
    while let Some(part) = parts.next() {
        let mut frame = vec![0; limits::HEADER_BYTES];
        let flag = if parts.peek().is_some() { MORE } else { 0 };
        write_header(&mut frame, kind as u8 | flag, part.len());
        frame.extend_from_slice(part);
        frames.push(frame);
    }
    Ok(WireMessage { kind, frames })
}

fn write_header(header: &mut [u8], kind: u8, body: usize) {
    header[..4].copy_from_slice(&((body + 1) as u32).to_be_bytes());
    header[4] = kind;
}

/// Splits a byte stream into whole frames. Account-local carriers feed it what they read.
#[derive(Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole frame, if one has arrived. An error is final for the connection.
    pub fn next_frame(&mut self) -> Result<Option<RawFrame>, DecodeError> {
        let Some((frame, used)) = parse_prefix(&self.buffer)? else {
            return Ok(None);
        };
        self.buffer.drain(..used);
        Ok(Some(frame))
    }

    /// Whether a frame is partly received.
    pub fn partial(&self) -> bool {
        !self.buffer.is_empty()
    }
}

/// Joins continued frames into messages and decodes them for one side of a connection.
#[derive(Default)]
pub struct Assembler {
    partial: Vec<(Kind, Vec<u8>)>,
}

impl Assembler {
    /// Server side: the next client message, once its last frame arrives.
    pub fn client(&mut self, frame: RawFrame) -> Result<Option<ClientMessage>, DecodeError> {
        let Some((kind, body)) = self.join(frame, true)? else {
            return Ok(None);
        };
        let message = match kind {
            Kind::Request => ClientMessage::Request(Box::new(decode::<Request>(&body)?)),
            Kind::Input => ClientMessage::Input(decode::<Input>(&body)?),
            Kind::FrameAck => ClientMessage::FrameAck(decode::<FrameAck>(&body)?),
            Kind::View => ClientMessage::View(decode::<View>(&body)?),
            Kind::EffectDone => ClientMessage::EffectDone(decode::<EffectDone>(&body)?),
            Kind::Acknowledge => ClientMessage::Acknowledge(decode::<Acknowledge>(&body)?),
            Kind::Cancel => ClientMessage::Cancel(decode::<Cancel>(&body)?),
            Kind::ClientPing => ClientMessage::Ping(decode::<Ping>(&body)?),
            Kind::ClientPong => ClientMessage::Pong(decode::<Pong>(&body)?),
            _ => return Err(DecodeError::WrongDirection),
        };
        message.validate().map_err(|_| DecodeError::OutOfBounds)?;
        Ok(Some(message))
    }

    /// Client side: the next server message, once its last frame arrives.
    pub fn server(&mut self, frame: RawFrame) -> Result<Option<ServerMessage>, DecodeError> {
        let Some((kind, body)) = self.join(frame, false)? else {
            return Ok(None);
        };
        let message = match kind {
            Kind::Reply | Kind::BulkReply => {
                ServerMessage::Reply(Box::new(decode::<Reply>(&body)?))
            }
            Kind::Frame => ServerMessage::Frame(Box::new(decode::<Frame>(&body)?)),
            Kind::Pane => ServerMessage::Pane(decode::<PaneMessage>(&body)?),
            Kind::Inventory => {
                ServerMessage::Inventory(Box::new(decode::<InventoryRevision>(&body)?))
            }
            Kind::ServerPing => ServerMessage::Ping(decode::<Ping>(&body)?),
            Kind::ServerPong => ServerMessage::Pong(decode::<Pong>(&body)?),
            Kind::Goodbye => ServerMessage::Goodbye(decode::<Fatal>(&body)?),
            _ => return Err(DecodeError::WrongDirection),
        };
        message.validate().map_err(|_| DecodeError::OutOfBounds)?;
        Ok(Some(message))
    }

    fn join(
        &mut self,
        frame: RawFrame,
        from_client: bool,
    ) -> Result<Option<(Kind, Vec<u8>)>, DecodeError> {
        let kind = frame.kind()?;
        if kind.from_client() != from_client {
            return Err(DecodeError::WrongDirection);
        }
        if frame.body.len() > kind.max_frame_body() {
            return Err(DecodeError::Oversized);
        }
        let continued = frame.continued();
        let index = self.partial.iter().position(|(open, _)| *open == kind);
        let body = match index {
            Some(index) => {
                let open = &mut self.partial[index].1;
                if open.len() + frame.body.len() > kind.max_message() {
                    return Err(DecodeError::Oversized);
                }
                open.extend_from_slice(&frame.body);
                if continued {
                    return Ok(None);
                }
                self.partial.swap_remove(index).1
            }
            None if continued => {
                self.partial.push((kind, frame.body));
                return Ok(None);
            }
            None => frame.body,
        };
        if body.len() > kind.max_message() {
            return Err(DecodeError::Oversized);
        }
        Ok(Some((kind, body)))
    }
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, DecodeError> {
    match postcard::take_from_bytes(body) {
        Ok((value, [])) => Ok(value),
        _ => Err(DecodeError::Malformed),
    }
}
