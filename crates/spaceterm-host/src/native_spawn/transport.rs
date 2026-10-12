use std::{
    collections::BTreeMap,
    io::{self, Read},
    net::Shutdown,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use super::super::protocol::{self, Failure, Reply, Request, Result, Retirement};
use super::{CHANNEL_TIMEOUT, CheckedChannel, POLL_INTERVAL, RequestBudget};
use std::os::fd::OwnedFd;

/// One reply reader retains frame progress independently of caller waits. Only read-only status
/// and kept-group preparation can abandon a wait; child-owning transactions finish before return.
/// Correlation lets group cleanup proceed while an earlier status observation is delayed.
pub(super) struct Transport {
    channel: Mutex<Option<UnixStream>>,
    replies: Arc<Replies>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

struct Replies {
    state: Mutex<ReplyState>,
}
struct ReplyState {
    next_sequence: u64,
    pending: BTreeMap<u64, Pending>,
    failure: Option<Failure>,
}
struct Pending {
    deadline: Instant,
    expected: Expected,
    reply: mpsc::Sender<Result<Reply>>,
}

#[derive(Clone, Copy)]
enum Expected {
    Spawn,
    Poll,
    Retire,
    KeepGroup,
    ReleaseGroup,
}
impl Expected {
    fn for_request(request: &Request) -> Self {
        match request {
            Request::Spawn(_) => Self::Spawn,
            Request::Poll(_) | Request::KillGroup(_) => Self::Poll,
            Request::Retire(_, Retirement::KeepGroup) => Self::KeepGroup,
            Request::Retire(_, _) => Self::Retire,
            Request::ReleaseGroup(_) => Self::ReleaseGroup,
        }
    }
    fn accepts(self, reply: Reply) -> bool {
        matches!(
            (self, reply),
            (Self::Spawn, Reply::Spawned { pid: 1.., .. })
                | (Self::Poll, Reply::Poll(_))
                | (Self::Retire, Reply::Retired { .. } | Reply::Retiring)
                | (Self::KeepGroup, Reply::GroupRetained)
                | (Self::ReleaseGroup, Reply::Retired { .. })
        )
    }
}

impl Transport {
    pub(super) fn empty() -> Self {
        Self {
            channel: Mutex::new(None),
            replies: Arc::new(Replies {
                state: Mutex::new(ReplyState {
                    next_sequence: 1,
                    pending: BTreeMap::new(),
                    failure: None,
                }),
            }),
            reader: Mutex::new(None),
        }
    }

    pub(super) fn start(&self, channel: UnixStream) -> Result<()> {
        channel
            .set_read_timeout(Some(POLL_INTERVAL))
            .and_then(|()| channel.set_write_timeout(Some(POLL_INTERVAL)))
            .map_err(|_| Failure::Unavailable)?;
        let read_channel = channel.try_clone().map_err(|_| Failure::Unavailable)?;
        protocol::configure(&read_channel)?;
        *self.channel.lock().map_err(|_| Failure::Unavailable)? = Some(channel);
        let replies = self.replies.clone();
        let reader = std::thread::Builder::new()
            .name("credential-free-replies".into())
            .spawn(move || read_replies(read_channel, replies))
            .map_err(|_| Failure::Unavailable)?;
        *self.reader.lock().map_err(|_| Failure::Unavailable)? = Some(reader);
        Ok(())
    }

    pub(super) fn request(
        &self,
        request: Request,
        descriptors: &[OwnedFd],
        budget: RequestBudget<'_>,
    ) -> Result<Reply> {
        // Validate ordinary arguments before admission; a rejected request never touches the peer.
        let mut payload = request.encode()?;
        let budget = budget.for_request();
        let mut slot = budget.lock(&self.channel)?;
        budget.check()?;
        let channel = slot.as_mut().ok_or(Failure::Unavailable)?;
        let io_budget = RequestBudget::default().for_request();
        let (sequence, reply) = self.replies.begin(Expected::for_request(&request))?;
        let sent = protocol::tag(&mut payload, sequence).and_then(|()| {
            let mut checked = CheckedChannel {
                channel,
                budget: io_budget,
            };
            protocol::send_request(&mut checked, &payload, descriptors, &|| io_budget.check())
        });
        if let Err(failure) = sent {
            // Partial writes cannot be resumed by a different request. EOF retires peer ownership.
            self.replies.fail(failure);
            let _ = channel.shutdown(Shutdown::Both);
            slot.take();
            return Err(failure);
        }
        if matches!(
            request,
            Request::Poll(_) | Request::Retire(_, Retirement::KeepGroup)
        ) {
            drop(slot);
            wait_reply(&reply, budget)
        } else {
            // Spawn and final ownership decisions cannot abandon an admitted transaction. Caller
            // aborts still govern admission; only the native I/O bound governs completion.
            let result = wait_reply(&reply, io_budget);
            if matches!(result, Err(Failure::TimedOut)) {
                self.replies.fail(Failure::TimedOut);
                let _ = channel.shutdown(Shutdown::Both);
                slot.take();
            }
            result
        }
    }

    pub(super) fn shutdown(&self) -> Result<()> {
        // Wake admitted callers before taking their send lock. The reader owns no child authority.
        self.replies.fail(Failure::Unavailable);
        if let Some(channel) = self
            .channel
            .lock()
            .map_err(|_| Failure::CleanupUnresolved)?
            .take()
        {
            let _ = channel.shutdown(Shutdown::Both);
        }
        if let Some(reader) = self
            .reader
            .lock()
            .map_err(|_| Failure::CleanupUnresolved)?
            .take()
        {
            reader.join().map_err(|_| Failure::CleanupUnresolved)?;
        }
        Ok(())
    }
}

impl Replies {
    fn begin(&self, expected: Expected) -> Result<(u64, mpsc::Receiver<Result<Reply>>)> {
        let mut state = self.state.lock().map_err(|_| Failure::Unavailable)?;
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        if state.pending.len() >= protocol::MAX_CHILDREN {
            return Err(Failure::Limit);
        }
        let sequence = state.next_sequence;
        state.next_sequence = sequence.checked_add(1).ok_or(Failure::Limit)?;
        let (reply, receiver) = mpsc::channel();
        state.pending.insert(
            sequence,
            Pending {
                deadline: Instant::now() + CHANNEL_TIMEOUT,
                expected,
                reply,
            },
        );
        Ok((sequence, receiver))
    }

    fn check(&self) -> Result<()> {
        let state = self.state.lock().map_err(|_| Failure::Unavailable)?;
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        if state
            .pending
            .values()
            .any(|pending| Instant::now() >= pending.deadline)
        {
            return Err(Failure::TimedOut);
        }
        Ok(())
    }

    fn deliver(&self, sequence: u64, result: Result<Reply>) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| Failure::Unavailable)?;
        let pending = state.pending.get(&sequence).ok_or(Failure::Invalid)?;
        if result.is_ok_and(|reply| !pending.expected.accepts(reply)) {
            return Err(Failure::Invalid);
        }
        let pending = state.pending.remove(&sequence).ok_or(Failure::Invalid)?;
        // A status caller may already have returned. Its reply is still consumed and validated.
        let _ = pending.reply.send(result);
        Ok(())
    }

    fn fail(&self, failure: Failure) {
        if let Ok(mut state) = self.state.lock() {
            let failure = *state.failure.get_or_insert(failure);
            for (_, pending) in std::mem::take(&mut state.pending) {
                let _ = pending.reply.send(Err(failure));
            }
        }
    }
}

struct ReplyReader {
    channel: UnixStream,
    replies: Arc<Replies>,
}
impl Read for ReplyReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.replies.check().map_err(Failure::io)?;
            match self.channel.read(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}
fn read_replies(channel: UnixStream, replies: Arc<Replies>) {
    let mut reader = ReplyReader { channel, replies };
    let failure = loop {
        match protocol::receive_reply(&mut reader) {
            Ok((sequence, reply)) => {
                if let Err(failure) = reader.replies.deliver(sequence, reply) {
                    break failure;
                }
            }
            Err(failure) => break reader.replies.check().err().unwrap_or(failure),
        }
    };
    reader.replies.fail(failure);
    let _ = reader.channel.shutdown(Shutdown::Both);
}

fn wait_reply(reply: &mpsc::Receiver<Result<Reply>>, budget: RequestBudget<'_>) -> Result<Reply> {
    loop {
        budget.check()?;
        match reply.recv_timeout(
            POLL_INTERVAL
                .min(budget.remaining())
                .max(Duration::from_micros(1)),
        ) {
            Ok(reply) => return reply,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Failure::Unavailable),
        }
    }
}
