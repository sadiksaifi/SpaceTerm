//! The service lane: bounded JSON administration that every protocol major from 4 on reads the
//! same way, so Status, Stop, Restart and Update work across majors.
//!
//! A client selects it with [`crate::protocol::Lane::Service`] in the admission preamble. Each
//! frame is length-prefixed JSON of at most [`MAX_FRAME`] bytes. Fields may be added to these
//! messages. They are never removed or renamed.
use crate::ServerError;
use crate::protocol::Preamble;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// The largest lane frame, in bytes.
pub const MAX_FRAME: usize = 128 * 1024;
/// The largest decoded `UploadChunk::data`, in bytes.
pub const MAX_CHUNK: usize = 64 * 1024;
const REQUEST_LIMIT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaneRequest {
    Status,
    Check,
    StageFetch,
    StageUpload {
        length: u64,
        sha256: String,
        /// Base64.
        signature: String,
    },
    UploadChunk {
        upload: String,
        offset: u64,
        /// Base64, at most [`MAX_CHUNK`] bytes decoded.
        data: String,
    },
    Preview {
        action: LaneAction,
    },
    Execute {
        action: LaneAction,
        token: String,
        operation: String,
    },
    Resolve {
        operation: String,
    },
    Acknowledge {
        operation: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LaneAction {
    Stop,
    Restart,
    Update { installation: String },
    Return,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum LaneReply {
    Status(LaneStatus),
    Check(LaneCheck),
    Staged {
        installation: String,
        version: String,
        commit: String,
    },
    UploadProgress {
        upload: String,
        next_offset: u64,
    },
    Preview {
        token: String,
        expires_at: String,
        effects: LaneEffects,
    },
    Accepted {
        operation: String,
    },
    Receipt(LaneReceipt),
    Acknowledged,
    /// `code` is a code from the CLI output contract.
    Error {
        code: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneReceipt {
    pub operation: String,
    pub action: LaneAction,
    pub state: LaneOperationState,
    pub code: Option<String>,
    pub boot: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneOperationState {
    Admitted,
    Launched,
    Running,
    Done,
    Failed,
    NotPerformed,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneVersion {
    pub version: String,
    pub commit: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStaged {
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneActivation {
    Settled,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneNetwork {
    pub enabled: bool,
    pub bind: Option<String>,
    pub listening: bool,
    pub addresses: Vec<std::net::IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneDevice {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    pub last_connected_at: Option<String>,
}

/// Mirrors the `server.status` data of the CLI output contract. Held outcomes are the CLI's own
/// journals, so the lane leaves them out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStatus {
    pub running: bool,
    pub stopped: bool,
    pub installed: bool,
    pub automatic_start: bool,
    pub linger: Option<bool>,
    pub runtime: LaneVersion,
    pub staged: Vec<LaneStaged>,
    pub activation: LaneActivation,
    pub sessions: u32,
    pub network: LaneNetwork,
    pub devices: Vec<LaneDevice>,
    pub lane_major: u16,
}

/// Mirrors the `server.update --check` data of the CLI output contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCheck {
    pub approved: LaneVersion,
    pub latest: Option<LaneVersion>,
    pub staged: Vec<LaneVersion>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneUpdateKind {
    Update,
    Return,
}

/// Mirrors the confirmation `effects` of `server.stop`, `server.restart` and `server.update`.
/// Stop and Restart leave the version fields empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneEffects {
    pub ends_sessions: u32,
    pub sessions: Vec<String>,
    pub start_requires_machine: bool,
    pub from: Option<LaneVersion>,
    pub to: Option<LaneVersion>,
    pub kind: Option<LaneUpdateKind>,
}

/// The lane major this server speaks.
pub const LANE_MAJOR: u16 = 1;

/// Answers lane requests on an admitted service-lane connection until the peer closes it.
pub(crate) fn serve(stream: &mut dyn Preamble) -> Result<(), ServerError> {
    let deadline = Instant::now() + REQUEST_LIMIT;
    let bytes = stream.receive(MAX_FRAME, deadline)?;
    let reply = match serde_json::from_slice::<LaneRequest>(&bytes) {
        Ok(request) => answer(request),
        Err(_) => LaneReply::Error {
            code: "protocol_error".to_owned(),
        },
    };
    let bytes = serde_json::to_vec(&reply).map_err(|_| ServerError::Protocol)?;
    stream.send(&bytes, deadline)
}

fn answer(_request: LaneRequest) -> LaneReply {
    LaneReply::Error {
        code: "unsupported".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_messages_keep_their_frozen_json_shape() {
        let request = LaneRequest::Execute {
            action: LaneAction::Update {
                installation: "abc".into(),
            },
            token: "c1-00".into(),
            operation: "op".into(),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "request": "execute",
                "action": { "action": "update", "installation": "abc" },
                "token": "c1-00",
                "operation": "op",
            })
        );
        assert_eq!(
            serde_json::from_str::<LaneRequest>(r#"{"request":"status"}"#).unwrap(),
            LaneRequest::Status
        );
        assert!(
            serde_json::from_str::<LaneRequest>(r#"{"request":"resolve","operation":"op","x":1}"#)
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(LaneReply::Error {
                code: "unsupported".into()
            })
            .unwrap(),
            serde_json::json!({ "reply": "error", "code": "unsupported" })
        );
    }

    #[test]
    fn every_request_is_unsupported_until_the_lane_is_served() {
        let (mut client, mut server) = std::os::unix::net::UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || serve(&mut server));
        let deadline = Instant::now() + Duration::from_secs(5);
        client.send(br#"{"request":"status"}"#, deadline).unwrap();
        let reply: LaneReply =
            serde_json::from_slice(&client.receive(MAX_FRAME, deadline).unwrap()).unwrap();
        assert_eq!(
            reply,
            LaneReply::Error {
                code: "unsupported".into()
            }
        );
        thread.join().unwrap().unwrap();
    }
}
