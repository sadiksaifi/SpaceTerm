//! The account-local channel from `spaceterm` to the desktop on the same machine and account.
//!
//! One connection carries one request. The client half-closes after its request; the desktop
//! answers once and closes. Both ends require the peer to run as their own account. Requests
//! carry exact server, Workspace, Tab, Pane and Terminal Session IDs as fences, never authority:
//! the desktop decides what it presents, and only a Local Pane may open a local file. A
//! Diagnostics request only reads the desktop's Terminal Diagnostics counters.
//!
//! Every connect, read and write ends by an absolute deadline, recomputed before each operation.
//! The client's request carries the instant it stops waiting, on the host's monotonic clock that
//! every process on the machine reads alike. The desktop performs no effect after that instant,
//! however long the request waited in its queue, so a request the client gave up on never acts.
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The format of every request and reply. A different schema is refused as incompatible.
pub const SCHEMA: &str = "spaceterm.desktop.v2";
/// The largest request or reply, which bounds a local path of 4 KiB with its envelope.
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// How long the desktop gives a connected client to deliver its whole request, and itself to
/// deliver its whole reply.
pub const TRANSFER_WINDOW: Duration = Duration::from_secs(2);
/// The longest a request stays able to act, however long its client says it waits.
pub const MAX_EFFECT_WINDOW: Duration = Duration::from_secs(60);
/// How much sooner than its client gives up the desktop stops acting, so a reply can arrive.
const REPLY_MARGIN: Duration = Duration::from_millis(250);
/// The longest one blocking read or write waits before it rechecks its deadline.
const POLL: Duration = Duration::from_millis(100);

/// When the waiting ends. It is read again before every operation, so a caller can bring it
/// forward while an exchange runs, as an interrupt does.
pub type Deadline<'a> = &'a dyn Fn() -> Instant;

/// Local and cross-process readings of the same monotonic clock.
trait Clock {
    fn now(&self) -> Instant;
    fn host(&self) -> u64;
}
struct HostClock;
impl Clock for HostClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn host(&self) -> u64 {
        host_clock()
    }
}

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
    /// Reads the desktop's Terminal Diagnostics counters. It changes nothing and needs no
    /// presented Workspace, Pane or server connection.
    Diagnostics,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Open { preview: false, .. } => "Request::Open { .. }",
            Self::Open { preview: true, .. } => "Request::Preview { .. }",
            Self::Show { .. } => "Request::Show { .. }",
            Self::Diagnostics => "Request::Diagnostics",
        })
    }
}

/// The desktop's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Done,
    Refused(Refusal),
    /// The desktop's Terminal Diagnostics counters, totals since it started.
    Diagnostics(spaceterm_server::diagnostics::TerminalCounterSnapshot),
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
    /// The desktop is still checking as many earlier files as it checks at once.
    Busy,
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
    /// The deadline passed.
    Timeout,
    /// The desktop received the whole request and closed without an answer, so whether it
    /// acted is unknown.
    Unanswered,
    /// The exchange failed before the desktop received the whole request.
    Protocol,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEnvelope<T> {
    schema: String,
    identity: String,
    /// When the client stops waiting, in nanoseconds on the host's monotonic clock.
    expires_at: u64,
    body: T,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyEnvelope {
    schema: String,
    identity: String,
    body: Reply,
}

/// A connected stream whose waits the exchange bounds.
pub trait ChannelIo: Read + Write {
    /// Ends the client's request so the desktop reads it to its end.
    fn close_write(&mut self) -> std::io::Result<()>;
    /// Waits at most `wait`, which is never zero, until a read would not block. Reads and writes
    /// never block: they report `WouldBlock` instead.
    fn limit_read(&mut self, wait: Duration) -> std::io::Result<()>;
    /// Waits at most `wait`, which is never zero, until a write would not block.
    fn limit_write(&mut self, wait: Duration) -> std::io::Result<()>;
}

impl<T: ChannelIo + ?Sized> ChannelIo for Box<T> {
    fn close_write(&mut self) -> std::io::Result<()> {
        (**self).close_write()
    }
    fn limit_read(&mut self, wait: Duration) -> std::io::Result<()> {
        (**self).limit_read(wait)
    }
    fn limit_write(&mut self, wait: Duration) -> std::io::Result<()> {
        (**self).limit_write(wait)
    }
}

/// Sends one request on a connected, authenticated stream and reads the desktop's reply by
/// `deadline`.
pub fn exchange(
    stream: impl ChannelIo,
    identity: &str,
    request: &Request,
    deadline: Deadline<'_>,
) -> Result<Reply, ChannelError> {
    exchange_on(stream, identity, request, deadline, &HostClock)
}

fn exchange_on(
    mut stream: impl ChannelIo,
    identity: &str,
    request: &Request,
    deadline: Deadline<'_>,
    clock: &impl Clock,
) -> Result<Reply, ChannelError> {
    let within = remaining(deadline, clock)?;
    let message = serde_json::to_vec(&RequestEnvelope {
        schema: SCHEMA.to_owned(),
        identity: identity.to_owned(),
        expires_at: clock
            .host()
            .saturating_add(u64::try_from(within.as_nanos()).unwrap_or(u64::MAX)),
        body: request,
    })
    .map_err(|_| ChannelError::Protocol)?;
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(ChannelError::Protocol);
    }
    write_by(&mut stream, &message, deadline, clock)?;
    stream.close_write().map_err(|_| ChannelError::Protocol)?;
    let reply = match read_by(&mut stream, deadline, clock) {
        Ok(reply) => reply,
        Err(ChannelError::Timeout) => return Err(ChannelError::Timeout),
        Err(_) => return Err(ChannelError::Unanswered),
    };
    let reply: ReplyEnvelope =
        serde_json::from_slice(&reply).map_err(|_| ChannelError::Unanswered)?;
    let incompatible = reply.body == Reply::Refused(Refusal::Incompatible);
    if reply.schema != SCHEMA || (reply.identity != identity && !incompatible) {
        return Err(ChannelError::Unanswered);
    }
    Ok(reply.body)
}

/// Reads one request from an authenticated stream by `request_by`, answers it with `handle`,
/// and writes the reply within [`TRANSFER_WINDOW`]. `handle` receives the instant after which
/// the request must not act, and answers `None` when it gave up, which writes no reply. A
/// request whose client already stopped waiting never reaches `handle` and gets no answer. A
/// request in another schema or for another application identity is refused without reaching
/// `handle`. A malformed, oversized or late request gets no answer.
pub fn serve(
    stream: impl ChannelIo,
    identity: &str,
    request_by: Instant,
    handle: impl FnOnce(Request, Instant) -> Option<Reply>,
) -> Result<(), ChannelError> {
    serve_on(stream, identity, request_by, handle, &HostClock)
}

fn serve_on(
    mut stream: impl ChannelIo,
    identity: &str,
    request_by: Instant,
    handle: impl FnOnce(Request, Instant) -> Option<Reply>,
    clock: &impl Clock,
) -> Result<(), ChannelError> {
    let message = read_by(&mut stream, &|| request_by, clock)?;
    let received = clock.now();
    let value: serde_json::Value =
        serde_json::from_slice(&message).map_err(|_| ChannelError::Protocol)?;
    let reply = if value.get("schema").and_then(|schema| schema.as_str()) != Some(SCHEMA) {
        Reply::Refused(Refusal::Incompatible)
    } else {
        let request: RequestEnvelope<Request> =
            serde_json::from_value(value).map_err(|_| ChannelError::Protocol)?;
        if request.identity != identity {
            Reply::Refused(Refusal::Incompatible)
        } else {
            let Some(act_by) = act_by(request.expires_at, received, clock) else {
                return Ok(());
            };
            match handle(request.body, act_by) {
                Some(reply) => reply,
                None => return Ok(()),
            }
        }
    };
    let message = serde_json::to_vec(&ReplyEnvelope {
        schema: SCHEMA.to_owned(),
        identity: identity.to_owned(),
        body: reply,
    })
    .map_err(|_| ChannelError::Protocol)?;
    let reply_by = clock.now() + TRANSFER_WINDOW;
    write_by(&mut stream, &message, &|| reply_by, clock)
}

/// A connection from a peer the desktop authenticated as its own account.
#[cfg(feature = "desktop")]
pub(crate) trait ChannelStream: ChannelIo + Send {}
#[cfg(feature = "desktop")]
impl<T: ChannelIo + Send> ChannelStream for T {}

/// The desktop's listening endpoint, which host composition selects.
#[cfg(feature = "desktop")]
pub(crate) trait DesktopChannelEndpoint: Send + Sync {
    /// Waits for the next connection. `Ok(None)` is a connection closed unread because its
    /// peer runs as another account.
    fn accept(&self) -> std::io::Result<Option<Box<dyn ChannelStream>>>;
}

/// Nanoseconds on the host's monotonic clock, the one `Instant` reads, which counts from boot and
/// so means the same in every process on this machine.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn host_clock() -> u64 {
    crate::platform::unix_desktop_channel::monotonic_nanoseconds()
}

/// The instant a request received at `received` stops acting: [`REPLY_MARGIN`] before its
/// client's expiry, and never later than [`MAX_EFFECT_WINDOW`] after it arrived. `None` when that
/// already passed.
fn act_by(expires_at: u64, received: Instant, clock: &impl Clock) -> Option<Instant> {
    let left = Duration::from_nanos(expires_at.saturating_sub(clock.host()));
    let act_by =
        (clock.now() + left.saturating_sub(REPLY_MARGIN)).min(received + MAX_EFFECT_WINDOW);
    (clock.now() < act_by).then_some(act_by)
}

fn remaining(deadline: Deadline<'_>, clock: &impl Clock) -> Result<Duration, ChannelError> {
    let remaining = deadline().saturating_duration_since(clock.now());
    if remaining.is_zero() {
        Err(ChannelError::Timeout)
    } else {
        Ok(remaining)
    }
}

/// The next wait by the deadline: at most [`POLL`], so a deadline brought forward takes effect.
fn wait(deadline: Deadline<'_>, clock: &impl Clock) -> Result<Duration, ChannelError> {
    Ok(remaining(deadline, clock)?.min(POLL))
}

fn retried(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    )
}

/// Reads to the end of the stream by `deadline`, refusing more than [`MAX_MESSAGE_BYTES`].
fn read_by(
    stream: &mut impl ChannelIo,
    deadline: Deadline<'_>,
    clock: &impl Clock,
) -> Result<Vec<u8>, ChannelError> {
    let mut message = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        stream
            .limit_read(wait(deadline, clock)?)
            .map_err(|_| ChannelError::Protocol)?;
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(message),
            Ok(count) => {
                message.extend_from_slice(&buffer[..count]);
                if message.len() > MAX_MESSAGE_BYTES {
                    return Err(ChannelError::Protocol);
                }
            }
            Err(error) if retried(&error) => {}
            Err(_) => return Err(ChannelError::Protocol),
        }
    }
}

fn write_by(
    stream: &mut impl ChannelIo,
    mut message: &[u8],
    deadline: Deadline<'_>,
    clock: &impl Clock,
) -> Result<(), ChannelError> {
    while !message.is_empty() {
        stream
            .limit_write(wait(deadline, clock)?)
            .map_err(|_| ChannelError::Protocol)?;
        match stream.write(message) {
            Ok(0) => return Err(ChannelError::Protocol),
            Ok(count) => message = &message[count..],
            Err(error) if retried(&error) => {}
            Err(_) => return Err(ChannelError::Protocol),
        }
    }
    Ok(())
}

/// The directory holding this account's endpoint of the desktop of `identity`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn endpoint_directory(identity: &str) -> Result<PathBuf, ChannelError> {
    crate::platform::unix_desktop_channel::directory(identity)
}

/// Connects to this account's desktop of `identity` and asks it one request by `deadline`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn request(
    identity: &str,
    request: &Request,
    deadline: Deadline<'_>,
) -> Result<Reply, ChannelError> {
    let stream = crate::platform::unix_desktop_channel::connect(identity, deadline)?;
    exchange(stream, identity, request, deadline)
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

    fn after(wait: Duration) -> Instant {
        Instant::now() + wait
    }

    /// Exchanges `request` with a desktop answering `reply`, and returns what the desktop's
    /// handler received with its effect deadline.
    fn answer(
        client_identity: &'static str,
        request: Request,
        reply: Option<Reply>,
        wait: Duration,
    ) -> (Result<Reply, ChannelError>, Option<(Request, Instant)>) {
        let (client, desktop) = UnixStream::pair().unwrap();
        let served = std::thread::spawn(move || {
            let mut received = None;
            let _ = serve(desktop, IDENTITY, after(TRANSFER_WINDOW), |request, by| {
                received = Some((request, by));
                reply
            });
            received
        });
        let deadline = after(wait);
        let result = exchange(client, client_identity, &request, &|| deadline);
        (result, served.join().unwrap())
    }

    struct ManualClock {
        start: Instant,
        elapsed: std::cell::Cell<Duration>,
    }
    impl ManualClock {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                elapsed: std::cell::Cell::new(Duration::ZERO),
            }
        }
        fn advance(&self, by: Duration) {
            self.elapsed.set(self.elapsed.get() + by);
        }
    }
    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            self.start + self.elapsed.get()
        }
        fn host(&self) -> u64 {
            10_000_000_000 + self.elapsed.get().as_nanos() as u64
        }
    }

    struct Peer<'a> {
        clock: &'a ManualClock,
        input: std::collections::VecDeque<(Duration, Vec<u8>)>,
        output: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
        interrupt: Option<&'a std::cell::Cell<Instant>>,
    }
    impl<'a> Peer<'a> {
        fn new(clock: &'a ManualClock, input: Vec<(Duration, Vec<u8>)>) -> Self {
            Self {
                clock,
                input: input.into(),
                output: Default::default(),
                interrupt: None,
            }
        }
    }
    impl Read for Peer<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let Some((delay, bytes)) = self.input.front_mut() else {
                return Ok(0);
            };
            if !delay.is_zero() {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let count = bytes.len().min(buffer.len());
            buffer[..count].copy_from_slice(&bytes[..count]);
            bytes.drain(..count);
            if bytes.is_empty() {
                self.input.pop_front();
            }
            Ok(count)
        }
    }
    impl Write for Peer<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.output.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl ChannelIo for Peer<'_> {
        fn close_write(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn limit_read(&mut self, wait: Duration) -> std::io::Result<()> {
            assert!(!wait.is_zero() && wait <= Duration::from_millis(100));
            if let Some(deadline) = self.interrupt.take() {
                deadline.set(self.clock.now() + Duration::from_millis(150));
            }
            if let Some((delay, _)) = self.input.front_mut() {
                let elapsed = wait.min(*delay);
                self.clock.advance(elapsed);
                *delay -= elapsed;
            }
            Ok(())
        }
        fn limit_write(&mut self, wait: Duration) -> std::io::Result<()> {
            assert!(!wait.is_zero() && wait <= Duration::from_millis(100));
            Ok(())
        }
    }

    fn outgoing(clock: &ManualClock, duration: Duration) -> Vec<u8> {
        let peer = Peer::new(clock, vec![]);
        let output = peer.output.clone();
        let deadline = clock.now() + duration;
        assert_eq!(
            exchange_on(peer, IDENTITY, &open(false), &|| deadline, clock),
            Err(ChannelError::Unanswered)
        );
        let bytes = output.borrow().clone();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["expires_at"],
            clock.host() + duration.as_nanos() as u64
        );
        bytes
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
            let (reply, received) = answer(
                IDENTITY,
                request.clone(),
                Some(Reply::Done),
                Duration::from_secs(5),
            );
            assert_eq!(reply, Ok(Reply::Done));
            assert_eq!(received.map(|(request, _)| request), Some(request));
        }
        let (reply, _) = answer(
            IDENTITY,
            open(true),
            Some(Reply::Refused(Refusal::PreviewUnavailable)),
            Duration::from_secs(5),
        );
        assert_eq!(reply, Ok(Reply::Refused(Refusal::PreviewUnavailable)));
    }

    #[test]
    fn the_desktop_acts_only_while_its_client_still_waits() {
        for (window, expected) in [
            (Duration::from_secs(3), Duration::from_millis(2750)),
            (Duration::from_secs(24 * 60 * 60), Duration::from_secs(61)),
        ] {
            let clock = ManualClock::new();
            let message = outgoing(&clock, window);
            // The desktop receives the complete request one second after dispatch.
            clock.advance(Duration::from_secs(1));
            let peer = Peer::new(&clock, vec![(Duration::ZERO, message)]);
            let mut called = false;
            assert_eq!(
                serve_on(
                    peer,
                    IDENTITY,
                    clock.now() + TRANSFER_WINDOW,
                    |request, by| {
                        assert_eq!(request, open(false));
                        assert_eq!(by, clock.start + expected);
                        called = true;
                        Some(Reply::Done)
                    },
                    &clock
                ),
                Ok(())
            );
            assert!(called);
        }
    }

    #[test]
    fn a_request_read_after_its_client_stopped_waiting_never_acts() {
        let clock = ManualClock::new();
        let message = outgoing(&clock, Duration::from_secs(3));
        clock.advance(Duration::from_secs(3));
        let peer = Peer::new(&clock, vec![(Duration::ZERO, message)]);
        let output = peer.output.clone();
        assert_eq!(
            serve_on(
                peer,
                IDENTITY,
                clock.now() + TRANSFER_WINDOW,
                |_, _| panic!("an expired request reached its handler"),
                &clock
            ),
            Ok(())
        );
        assert!(output.borrow().is_empty());
    }

    #[test]
    fn the_shared_clock_is_the_one_instant_reads() {
        let instant_before = Instant::now();
        let host_before = host_clock();
        let instant_inner_before = Instant::now();
        let host_middle = host_clock();
        let instant_inner_after = Instant::now();
        let host_after = host_clock();
        let instant_after = Instant::now();
        assert!(host_before <= host_middle && host_middle <= host_after);
        let host_elapsed = Duration::from_nanos(host_after - host_before);
        assert!(instant_inner_after.duration_since(instant_inner_before) <= host_elapsed);
        assert!(host_elapsed <= instant_after.duration_since(instant_before));
    }

    #[test]
    fn a_request_the_desktop_gave_up_on_is_unanswered() {
        let (reply, received) = answer(IDENTITY, open(false), None, Duration::from_secs(5));
        assert!(received.is_some());
        assert_eq!(reply, Err(ChannelError::Unanswered));
    }

    #[test]
    fn the_client_stops_at_its_deadline_even_when_it_moves_earlier() {
        for interrupted in [false, true] {
            let clock = ManualClock::new();
            let initial = if interrupted {
                Duration::from_secs(30)
            } else {
                Duration::from_millis(300)
            };
            let deadline = std::cell::Cell::new(clock.now() + initial);
            let mut peer = Peer::new(&clock, vec![(Duration::from_secs(60), vec![])]);
            if interrupted {
                peer.interrupt = Some(&deadline);
            }
            assert_eq!(
                exchange_on(peer, IDENTITY, &open(false), &|| deadline.get(), &clock),
                Err(ChannelError::Timeout)
            );
            assert_eq!(
                clock.elapsed.get(),
                if interrupted {
                    Duration::from_millis(150)
                } else {
                    Duration::from_millis(300)
                }
            );
        }
    }

    #[test]
    fn a_client_trickling_its_request_holds_the_desktop_only_until_the_request_deadline() {
        let clock = ManualClock::new();
        let peer = Peer::new(&clock, vec![(Duration::from_millis(100), vec![b' ']); 20]);
        let by = clock.now() + Duration::from_millis(500);
        assert_eq!(
            serve_on(
                peer,
                IDENTITY,
                by,
                |_, _| panic!("a late request reached its handler"),
                &clock
            ),
            Err(ChannelError::Timeout)
        );
        assert_eq!(clock.elapsed.get(), Duration::from_millis(500));
    }

    #[test]
    fn another_identity_or_schema_is_refused_before_the_handler() {
        let (reply, received) = answer(
            "spaceterm",
            open(false),
            Some(Reply::Done),
            Duration::from_secs(5),
        );
        assert_eq!(reply, Ok(Reply::Refused(Refusal::Incompatible)));
        assert!(received.is_none());

        let (mut client, desktop) = UnixStream::pair().unwrap();
        let served = std::thread::spawn(move || {
            serve(desktop, IDENTITY, after(TRANSFER_WINDOW), |_, _| {
                panic!("an old schema reached the handler")
            })
        });
        client
            .write_all(br#"{"schema":"spaceterm.desktop.v0","identity":"x","body":{}}"#)
            .unwrap();
        client.close_write().unwrap();
        let deadline = after(Duration::from_secs(5));
        let reply: ReplyEnvelope =
            serde_json::from_slice(&read_by(&mut client, &|| deadline, &HostClock).unwrap())
                .unwrap();
        assert_eq!(reply.body, Reply::Refused(Refusal::Incompatible));
        served.join().unwrap().unwrap();
    }

    #[test]
    fn oversized_or_malformed_requests_get_no_answer() {
        for message in [vec![b' '; MAX_MESSAGE_BYTES + 1], b"{".to_vec()] {
            let (mut client, desktop) = UnixStream::pair().unwrap();
            let served = std::thread::spawn(move || {
                serve(desktop, IDENTITY, after(TRANSFER_WINDOW), |_, _| {
                    panic!("a malformed request was handled")
                })
            });
            let _ = client.write_all(&message);
            let _ = client.close_write();
            assert_eq!(served.join().unwrap(), Err(ChannelError::Protocol));
            let mut reply = Vec::new();
            let _ = client.read_to_end(&mut reply);
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
