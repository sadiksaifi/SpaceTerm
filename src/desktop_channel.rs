//! The account-local channel from `spaceterm` to the desktop on the same machine and account.
//!
//! One connection carries one request. The client half-closes after its request; the desktop
//! answers once and closes. Both ends require the peer to run as their own account. Requests
//! carry exact server, Workspace, Tab, Pane and Terminal Session IDs as fences, never authority:
//! the desktop decides what it presents, and only a Local Pane may open a local file.
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

/// The format of every request and reply. A different schema is refused as incompatible.
pub const SCHEMA: &str = "spaceterm.desktop.v1";
/// The largest request or reply, which bounds a local path of 4 KiB with its envelope.
const MAX_MESSAGE_BYTES: u64 = 16 * 1024;
/// How long the client waits for the desktop's answer, including File Preview handoff.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

pub type Id = [u8; 32];

/// The Terminal Session `spaceterm` runs in, from its Session context.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionContext {
    pub server: Id,
    pub pane: Id,
    pub session: Id,
}

impl std::fmt::Debug for SessionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SessionContext { .. }")
    }
}

/// What the client asks of the desktop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Opens an absolute local file with its default application, or presents its File
    /// Preview, for the Session's Pane.
    Open {
        session: SessionContext,
        path: PathBuf,
        preview: bool,
    },
    /// Brings the desktop's own view to a Workspace, and to a Tab and Pane within it when given.
    Show {
        server: Id,
        workspace: Id,
        tab: Option<Id>,
        pane: Option<Id>,
    },
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Open { preview: false, .. } => "Request::Open { .. }",
            Self::Open { preview: true, .. } => "Request::Preview { .. }",
            Self::Show { .. } => "Request::Show { .. }",
        })
    }
}

/// The desktop's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Done,
    Refused(Refusal),
}

/// Why the desktop refused a request. Each names a fixed classification only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// This desktop presents no such live Terminal Session, Workspace, Tab or Pane.
    NotShown,
    /// The Session's Pane is Remote, so its paths grant no local file access.
    RemoteSession,
    /// The path names no regular file that this account can read.
    FileUnavailable,
    /// This desktop has no File Preview.
    PreviewUnavailable,
    /// The request's schema or application identity is not this desktop's.
    Incompatible,
}

/// Why the client reached no answer. Each names a fixed classification only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelError {
    /// No desktop of this application identity listens for this account.
    Unavailable,
    /// The endpoint or its directory is not private to this account.
    UnsafeEndpoint,
    /// The process at the endpoint runs as another account.
    AccountMismatch,
    /// The desktop did not answer in time.
    Timeout,
    /// The exchange did not complete.
    Protocol,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    schema: String,
    identity: String,
    body: T,
}

/// Sends one request on a connected, authenticated stream and reads the desktop's reply.
pub fn exchange<S: Read + Write + HalfClose>(
    mut stream: S,
    identity: &str,
    request: &Request,
) -> Result<Reply, ChannelError> {
    let message = serde_json::to_vec(&Envelope {
        schema: SCHEMA.to_owned(),
        identity: identity.to_owned(),
        body: request,
    })
    .map_err(|_| ChannelError::Protocol)?;
    if message.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(ChannelError::Protocol);
    }
    stream.write_all(&message).map_err(io_error)?;
    stream.flush().map_err(io_error)?;
    stream.close_write().map_err(io_error)?;
    let reply: Envelope<Reply> = read_message(&mut stream)?;
    let incompatible = reply.body == Reply::Refused(Refusal::Incompatible);
    if reply.schema != SCHEMA || (reply.identity != identity && !incompatible) {
        return Err(ChannelError::Protocol);
    }
    Ok(reply.body)
}

/// Reads one request from an authenticated stream, answers it with `handle`, and writes the
/// reply. A request in another schema or for another application identity is refused without
/// reaching `handle`. A malformed or oversized request gets no answer.
pub fn serve<S: Read + Write>(
    mut stream: S,
    identity: &str,
    handle: impl FnOnce(Request) -> Reply,
) -> Result<(), ChannelError> {
    let reply = match read_message::<serde_json::Value>(&mut stream)? {
        value if value.get("schema").and_then(|schema| schema.as_str()) != Some(SCHEMA) => {
            Reply::Refused(Refusal::Incompatible)
        }
        value => {
            let request: Envelope<Request> =
                serde_json::from_value(value).map_err(|_| ChannelError::Protocol)?;
            if request.identity == identity {
                handle(request.body)
            } else {
                Reply::Refused(Refusal::Incompatible)
            }
        }
    };
    let message = serde_json::to_vec(&Envelope {
        schema: SCHEMA.to_owned(),
        identity: identity.to_owned(),
        body: reply,
    })
    .map_err(|_| ChannelError::Protocol)?;
    stream.write_all(&message).map_err(io_error)?;
    stream.flush().map_err(io_error)
}

/// A connection from a peer the desktop authenticated as its own account.
#[cfg(feature = "desktop")]
pub(crate) trait ChannelStream: Read + Write + Send {}
#[cfg(feature = "desktop")]
impl<T: Read + Write + Send> ChannelStream for T {}

/// The desktop's listening endpoint, which host composition selects.
#[cfg(feature = "desktop")]
pub(crate) trait DesktopChannelEndpoint: Send + Sync {
    /// Waits for the next connection. `Ok(None)` is a connection closed unread because its
    /// peer runs as another account.
    fn accept(&self) -> std::io::Result<Option<Box<dyn ChannelStream>>>;
}

/// Ends the client's request so the desktop reads it to its end.
pub trait HalfClose {
    fn close_write(&mut self) -> std::io::Result<()>;
}

fn read_message<T: serde::de::DeserializeOwned>(stream: &mut impl Read) -> Result<T, ChannelError> {
    let mut message = Vec::new();
    stream
        .take(MAX_MESSAGE_BYTES + 1)
        .read_to_end(&mut message)
        .map_err(io_error)?;
    if message.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(ChannelError::Protocol);
    }
    serde_json::from_slice(&message).map_err(|_| ChannelError::Protocol)
}

fn io_error(error: std::io::Error) -> ChannelError {
    match error.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => ChannelError::Timeout,
        _ => ChannelError::Protocol,
    }
}

/// Connects to this account's desktop of `identity` and asks it one request.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn request(identity: &str, request: &Request) -> Result<Reply, ChannelError> {
    let stream = crate::platform::unix_desktop_channel::connect(identity, REPLY_TIMEOUT)?;
    exchange(stream, identity, request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    const IDENTITY: &str = "spaceterm-development";

    fn open(preview: bool) -> Request {
        Request::Open {
            session: SessionContext {
                server: [1; 32],
                pane: [2; 32],
                session: [3; 32],
            },
            path: "/private/fixture.txt".into(),
            preview,
        }
    }

    fn answer(
        client_identity: &'static str,
        request: Request,
        reply: Reply,
    ) -> (Result<Reply, ChannelError>, Option<Request>) {
        let (client, desktop) = UnixStream::pair().unwrap();
        let served = std::thread::spawn(move || {
            let mut received = None;
            serve(desktop, IDENTITY, |request| {
                received = Some(request);
                reply
            })
            .unwrap();
            received
        });
        let result = exchange(client, client_identity, &request);
        (result, served.join().unwrap())
    }

    #[test]
    fn one_request_reaches_the_desktop_and_returns_its_reply() {
        for request in [
            open(false),
            open(true),
            Request::Show {
                server: [1; 32],
                workspace: [4; 32],
                tab: Some([5; 32]),
                pane: None,
            },
        ] {
            let (reply, received) = answer(IDENTITY, request.clone(), Reply::Done);
            assert_eq!(reply, Ok(Reply::Done));
            assert_eq!(received, Some(request));
        }
        let (reply, _) = answer(
            IDENTITY,
            open(true),
            Reply::Refused(Refusal::PreviewUnavailable),
        );
        assert_eq!(reply, Ok(Reply::Refused(Refusal::PreviewUnavailable)));
    }

    #[test]
    fn another_identity_or_schema_is_refused_before_the_handler() {
        let (reply, received) = answer("spaceterm", open(false), Reply::Done);
        assert_eq!(reply, Ok(Reply::Refused(Refusal::Incompatible)));
        assert_eq!(received, None);

        let (mut client, desktop) = UnixStream::pair().unwrap();
        let served = std::thread::spawn(move || {
            serve(desktop, IDENTITY, |_| {
                panic!("an old schema reached the handler")
            })
        });
        client
            .write_all(br#"{"schema":"spaceterm.desktop.v0","identity":"x","body":{}}"#)
            .unwrap();
        client.close_write().unwrap();
        let reply: Envelope<Reply> = read_message(&mut client).unwrap();
        assert_eq!(reply.body, Reply::Refused(Refusal::Incompatible));
        served.join().unwrap().unwrap();
    }

    #[test]
    fn oversized_or_malformed_requests_get_no_answer() {
        for message in [vec![b' '; MAX_MESSAGE_BYTES as usize + 1], b"{".to_vec()] {
            let (mut client, desktop) = UnixStream::pair().unwrap();
            let served = std::thread::spawn(move || {
                serve(desktop, IDENTITY, |_| {
                    panic!("a malformed request was handled")
                })
            });
            client.write_all(&message).unwrap();
            client.close_write().unwrap();
            assert_eq!(served.join().unwrap(), Err(ChannelError::Protocol));
            let mut reply = Vec::new();
            client.read_to_end(&mut reply).unwrap();
            assert!(reply.is_empty());
        }
    }

    #[test]
    fn debug_output_and_refusals_carry_no_paths_or_ids() {
        let text = format!("{:?}{:?}", open(true), Refusal::FileUnavailable);
        assert!(!text.contains("fixture") && !text.contains("[1"), "{text}");
        assert_eq!(
            serde_json::to_string(&Reply::Refused(Refusal::RemoteSession)).unwrap(),
            r#"{"refused":"remote_session"}"#
        );
    }
}
