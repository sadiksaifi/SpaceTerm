//! Native stdio framing uses the same bounded big-endian frames as private IPC.
//! Safe rustix descriptors give reads and writes an absolute deadline, including
//! partial frames and backpressure. No blocking stdin task survives that deadline.

use super::{
    ServerInvocation,
    owner::{OWNER_FRAME_LIMIT, OwnerEndpointFailure},
};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    io::{Errno, fcntl_dupfd_cloexec, read, write},
};
use spaceterm_host::{
    account_service::{
        LingerAuthorization, ServiceFailure, ServiceSupervisor, SudoLingerAuthorization, SudoMode,
        isolate_bootstrap_execution_session,
    },
    transport::{MessageTransport, PeerFacts, TransportError, TransportFuture, TransportRoute},
};
use std::{
    io::{IsTerminal as _, Write as _},
    os::fd::OwnedFd,
    sync::Arc,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

struct NativeStdio {
    input: Arc<OwnedFd>,
    output: Arc<OwnedFd>,
    retirement: Arc<OwnedFd>,
    retirement_sender: OwnedFd,
    deadline: Instant,
    uid: u32,
}
impl NativeStdio {
    fn new(uid: u32, budget: Duration) -> Result<Self, OwnerEndpointFailure> {
        // Bearer delivery is never printed to a terminal.
        if std::io::stdin().is_terminal() || std::io::stdout().is_terminal() {
            return Err(OwnerEndpointFailure::InvalidRequest);
        }
        let input = fcntl_dupfd_cloexec(std::io::stdin(), 3)
            .map_err(|_| OwnerEndpointFailure::Transport)?;
        let output = fcntl_dupfd_cloexec(std::io::stdout(), 3)
            .map_err(|_| OwnerEndpointFailure::Transport)?;
        for fd in [&input, &output] {
            let flags = fcntl_getfl(fd).map_err(|_| OwnerEndpointFailure::Transport)?;
            fcntl_setfl(fd, flags | OFlags::NONBLOCK)
                .map_err(|_| OwnerEndpointFailure::Transport)?;
        }
        let (retirement, retirement_sender) = spaceterm_host::process::anonymous_pipe()
            .map_err(|_| OwnerEndpointFailure::Transport)?;
        Ok(Self {
            input: Arc::new(input),
            output: Arc::new(output),
            retirement: Arc::new(retirement),
            retirement_sender,
            deadline: Instant::now() + budget,
            uid,
        })
    }
    fn retire(&self) -> Result<(), OwnerEndpointFailure> {
        // Wake every pending bounded read/write, including a partial frame or
        // blocked output. One byte latches this private cancellation pipe. No
        // reader consumes it, so every later frame also observes retirement.
        // The sole hangup branch writes once; this empty pipe cannot backpressure.
        match write(&self.retirement_sender, &[1]) {
            Ok(1) => Ok(()),
            _ => Err(OwnerEndpointFailure::Transport),
        }
    }
}
fn wait(
    fd: &OwnedFd,
    flags: PollFlags,
    deadline: Instant,
    retirement: Option<&OwnedFd>,
) -> Result<(), TransportError> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(TransportError::HandshakeExpired)?;
        let timeout = Timespec {
            tv_sec: remaining.as_secs() as _,
            tv_nsec: remaining.subsec_nanos() as _,
        };
        let mut descriptors = [
            PollFd::new(fd, flags),
            PollFd::new(
                retirement.unwrap_or(fd),
                if retirement.is_some() {
                    PollFlags::IN
                } else {
                    PollFlags::empty()
                },
            ),
        ];
        match poll(&mut descriptors, Some(&timeout)) {
            Ok(0) => return Err(TransportError::HandshakeExpired),
            Ok(_) if retirement.is_some() && !descriptors[1].revents().is_empty() => {
                return Err(TransportError::Closed);
            }
            Ok(_) => return Ok(()),
            Err(Errno::INTR) => {}
            Err(_) => return Err(TransportError::Unavailable),
        }
    }
}
fn read_exact(
    fd: &OwnedFd,
    mut bytes: &mut [u8],
    deadline: Instant,
    retirement: &OwnedFd,
) -> Result<bool, TransportError> {
    let mut started = false;
    while !bytes.is_empty() {
        wait(fd, PollFlags::IN, deadline, Some(retirement))?;
        match read(fd, &mut *bytes) {
            Ok(0) if !started => return Ok(false),
            Ok(0) => return Err(TransportError::InvalidFrame),
            Ok(count) => {
                started = true;
                bytes = &mut bytes[count..];
            }
            Err(Errno::INTR | Errno::AGAIN) => {}
            Err(_) => return Err(TransportError::Unavailable),
        }
    }
    Ok(true)
}
fn write_all(
    fd: &OwnedFd,
    mut bytes: &[u8],
    deadline: Instant,
    retirement: &OwnedFd,
) -> Result<(), TransportError> {
    while !bytes.is_empty() {
        wait(fd, PollFlags::OUT, deadline, Some(retirement))?;
        match write(fd, bytes) {
            Ok(0) => return Err(TransportError::Closed),
            Ok(count) => bytes = &bytes[count..],
            Err(Errno::INTR | Errno::AGAIN) => {}
            Err(_) => return Err(TransportError::Unavailable),
        }
    }
    Ok(())
}
impl MessageTransport for NativeStdio {
    fn peer_facts(&self) -> PeerFacts {
        PeerFacts {
            route: TransportRoute::PrivateIpc,
            local_uid: Some(self.uid),
            local_pid: None,
            pinned_server_key: None,
        }
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            if bytes.is_empty() || bytes.len() > OWNER_FRAME_LIMIT {
                return Err(TransportError::MessageTooLarge);
            }
            let bytes = Zeroizing::new(bytes.to_vec());
            let fd = self.output.clone();
            let retirement = self.retirement.clone();
            let deadline = self.deadline;
            tokio::task::spawn_blocking(move || {
                write_all(
                    &fd,
                    &(bytes.len() as u32).to_be_bytes(),
                    deadline,
                    &retirement,
                )?;
                write_all(&fd, &bytes, deadline, &retirement)
            })
            .await
            .map_err(|_| TransportError::Unavailable)?
        })
    }
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            let fd = self.input.clone();
            let retirement = self.retirement.clone();
            let deadline = self.deadline;
            tokio::task::spawn_blocking(move || {
                let mut prefix = [0; 4];
                if !read_exact(&fd, &mut prefix, deadline, &retirement)? {
                    return Ok(None);
                }
                let count = u32::from_be_bytes(prefix) as usize;
                if count == 0 || count > OWNER_FRAME_LIMIT {
                    return Err(TransportError::MessageTooLarge);
                }
                let mut bytes = Zeroizing::new(vec![0; count]);
                if !read_exact(&fd, &mut bytes, deadline, &retirement)? {
                    return Err(TransportError::InvalidFrame);
                }
                Ok(Some(std::mem::take(&mut *bytes)))
            })
            .await
            .map_err(|_| TransportError::Unavailable)?
        })
    }
    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Invoked before ordinary account startup. Only deliberate mutation commands
/// construct the native setup and owner providers.
pub async fn run_native(invocation: ServerInvocation) -> Option<Result<(), OwnerEndpointFailure>> {
    if !matches!(
        invocation,
        ServerInvocation::OwnerEnrollment
            | ServerInvocation::SetupConfirmed
            | ServerInvocation::ReserveNativeRequest
            | ServerInvocation::BootstrapAccount
    ) {
        return None;
    }
    let authorization =
        if cfg!(target_os = "linux") && invocation == ServerInvocation::SetupConfirmed {
            Some(
                Arc::new(SudoLingerAuthorization::new(SudoMode::Interactive))
                    as Arc<dyn LingerAuthorization>,
            )
        } else {
            None
        };
    let supervisor = match ServiceSupervisor::current(authorization) {
        Ok(supervisor) => Arc::new(supervisor),
        Err(error) => return Some(Err(OwnerEndpointFailure::Account(error))),
    };
    Some(run_native_with_supervisor(invocation, supervisor).await)
}

pub async fn run_native_with_supervisor(
    invocation: ServerInvocation,
    supervisor: Arc<ServiceSupervisor>,
) -> Result<(), OwnerEndpointFailure> {
    match invocation {
        ServerInvocation::BootstrapAccount => {
            // OpenSSH runs this fixed helper without a PTY. Its session can still
            // send SIGHUP when the local SSH process group is retired. Intercept
            // hangup only for this invocation and retire its foreground transport,
            // rather than killing the runtime that owns an admitted install.
            let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .map_err(|_| OwnerEndpointFailure::Transport)?;
            let channel = NativeStdio::new(supervisor.key().account_uid, Duration::from_secs(600))?;
            // The fixed SSH staging script retains its shell for EXIT/HUP cleanup
            // and invokes this helper as a child, without exec or a PTY. Leave that
            // shell's session before admitting installs: its group hangup must not
            // kill the helper's fixed manager utilities either. setsid is available
            // on both supported hosts. A group leader cannot detach and refuses
            // here before receiving any request or publishing native effects.
            isolate_bootstrap_execution_session().map_err(|_| OwnerEndpointFailure::Transport)?;
            let execution = super::bootstrap::execute(supervisor, &channel);
            tokio::pin!(execution);
            tokio::select! {
                result = &mut execution => result,
                _ = hangup.recv() => {
                    let retired = channel.retire();
                    // Do not drop owner::execute: it must close Running forwarding
                    // or await exact Stopped-owner cleanup. The retired channel
                    // wakes blocked framing and rejects every subsequent frame.
                    let result = execution.await;
                    retired.and(result)
                }
            }
        }
        ServerInvocation::OwnerEnrollment => {
            let channel = NativeStdio::new(supervisor.key().account_uid, Duration::from_secs(60))?;
            super::owner::execute(supervisor, &channel).await
        }
        ServerInvocation::ReserveNativeRequest => {
            let request = supervisor
                .allocate_request()
                .map_err(OwnerEndpointFailure::Account)?;
            output_json(&serde_json::json!({"schema": 1, "request": request}))
        }
        ServerInvocation::SetupConfirmed => {
            let bytes = tokio::task::spawn_blocking(read_setup_input)
                .await
                .map_err(|_| OwnerEndpointFailure::Transport)??;
            let request: super::setup::SetupRequest =
                serde_json::from_slice(&bytes).map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
            let confirmation = super::setup::prepare_confirmed_setup(&supervisor, &request)
                .map_err(OwnerEndpointFailure::Account)?;
            let progress = super::setup::account_setup(&supervisor, request.request, confirmation)
                .await
                .map_err(OwnerEndpointFailure::Account)?;
            output_json(
                &serde_json::json!({"schema": 1, "request": progress.request,
                "request_digest": progress.request_digest, "phase": progress.phase, "state": progress.state}),
            )
        }
        _ => Err(OwnerEndpointFailure::Account(ServiceFailure::InvalidRecord)),
    }
}
fn output_json(value: &serde_json::Value) -> Result<(), OwnerEndpointFailure> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
    bytes.push(b'\n');
    std::io::stdout()
        .lock()
        .write_all(&bytes)
        .map_err(|_| OwnerEndpointFailure::Transport)
}

fn read_setup_input() -> Result<Vec<u8>, OwnerEndpointFailure> {
    if std::io::stdin().is_terminal() {
        return Err(OwnerEndpointFailure::InvalidRequest);
    }
    let fd =
        fcntl_dupfd_cloexec(std::io::stdin(), 3).map_err(|_| OwnerEndpointFailure::Transport)?;
    let flags = fcntl_getfl(&fd).map_err(|_| OwnerEndpointFailure::Transport)?;
    fcntl_setfl(&fd, flags | OFlags::NONBLOCK).map_err(|_| OwnerEndpointFailure::Transport)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut bytes = Vec::new();
    loop {
        wait(&fd, PollFlags::IN, deadline, None)?;
        let mut buffer = [0; 4096];
        match read(&fd, &mut buffer) {
            Ok(0) if bytes.is_empty() => return Err(OwnerEndpointFailure::InvalidRequest),
            Ok(0) => return Ok(bytes),
            Ok(count) => {
                if count > OWNER_FRAME_LIMIT - bytes.len() {
                    return Err(OwnerEndpointFailure::InvalidRequest);
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(Errno::INTR | Errno::AGAIN) => {}
            Err(_) => return Err(OwnerEndpointFailure::Transport),
        }
    }
}
