use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            net::UnixStream,
        },
    },
};

/// Includes framing fields, native argument bytes and the complete environment.
pub(super) const MAX_PAYLOAD: usize = 256 * 1024;
const MAX_DESCRIPTORS: usize = 2;
pub(super) const MAX_CHILDREN: usize = 4096;
pub(super) const HELLO_MAGIC: &[u8; 4] = b"STS2";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum Failure {
    Invalid = 1,
    NotFound = 2,
    Unavailable = 3,
    Limit = 4,
    CleanupUnresolved = 5,
    // Local request-budget failures, never sent by the service.
    TimedOut = 6,
    Cancelled = 7,
}

pub(super) type Result<T> = std::result::Result<T, Failure>;

impl Failure {
    pub(super) fn io(self) -> io::Error {
        io::Error::new(
            match self {
                Self::Invalid | Self::Limit => io::ErrorKind::InvalidInput,
                Self::NotFound => io::ErrorKind::NotFound,
                Self::TimedOut => io::ErrorKind::TimedOut,
                _ => io::ErrorKind::Other,
            },
            "credential-free process operation failed",
        )
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum ProgramKind {
    Pty,
    Tool,
}

// No Debug implementation: arguments and environment are ordinary account data, not diagnostics.
pub(super) struct Program {
    pub kind: ProgramKind,
    pub executable: OsString,
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
}

#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum Retirement {
    Normal = 0,
    KeepGroup = 1,
    Hangup = 2,
    Kill = 3,
}

pub(super) enum Request {
    Spawn(Program),
    Poll(u64),
    KillGroup(u64),
    Retire(u64, Retirement),
    ReleaseGroup(u64),
}

#[derive(Clone, Copy)]
pub(super) enum Reply {
    Spawned { handle: u64, pid: u32 },
    Poll(bool),
    Retired { status: i32, forced: bool },
    Retiring,
    GroupRetained,
}

impl Request {
    pub(super) fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Encoder(Vec::new());
        match self {
            Self::Spawn(program) => {
                out.0
                    .extend_from_slice(&[1, u8::from(program.kind == ProgramKind::Pty)]);
                out.string(&program.executable)?;
                if program.arguments.len() > 4096 || program.environment.len() > 4096 {
                    return Err(Failure::Limit);
                }
                out.count(program.arguments.len())?;
                for argument in &program.arguments {
                    out.string(argument)?;
                }
                out.count(program.environment.len())?;
                for (name, value) in &program.environment {
                    if name.is_empty() || name.as_bytes().contains(&b'=') {
                        return Err(Failure::Invalid);
                    }
                    out.string(name)?;
                    out.string(value)?;
                }
            }
            Self::ReleaseGroup(handle) => {
                out.0.push(5);
                out.0.extend_from_slice(&handle.to_be_bytes());
            }
            Self::Poll(handle) => {
                out.0.push(2);
                out.0.extend_from_slice(&handle.to_be_bytes());
            }
            Self::KillGroup(handle) => {
                out.0.push(4);
                out.0.extend_from_slice(&handle.to_be_bytes());
            }
            Self::Retire(handle, mode) => {
                out.0.push(3);
                out.0.extend_from_slice(&handle.to_be_bytes());
                out.0.push(*mode as u8);
            }
        }
        if out.0.len() > MAX_PAYLOAD - 8 {
            return Err(Failure::Limit);
        }
        Ok(out.0)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut input = Decoder(bytes);
        let request = match input.byte()? {
            1 => {
                let kind = match input.byte()? {
                    0 => ProgramKind::Tool,
                    1 => ProgramKind::Pty,
                    _ => return Err(Failure::Invalid),
                };
                let executable = input.string()?;
                if executable.is_empty() {
                    return Err(Failure::Invalid);
                }
                let count = input.count()?;
                if count > 4096 || count > input.0.len() / 4 {
                    return Err(Failure::Limit);
                }
                let mut arguments = Vec::with_capacity(count);
                for _ in 0..count {
                    arguments.push(input.string()?);
                }
                let count = input.count()?;
                if count > 4096 || count > input.0.len() / 4 {
                    return Err(Failure::Limit);
                }
                let mut environment = Vec::with_capacity(count);
                for _ in 0..count {
                    let name = input.string()?;
                    if name.is_empty() || name.as_bytes().contains(&b'=') {
                        return Err(Failure::Invalid);
                    }
                    environment.push((name, input.string()?));
                }
                Self::Spawn(Program {
                    kind,
                    executable,
                    arguments,
                    environment,
                })
            }
            2 => Self::Poll(input.handle()?),
            3 => {
                let handle = input.handle()?;
                let mode = match input.byte()? {
                    0 => Retirement::Normal,
                    1 => Retirement::KeepGroup,
                    2 => Retirement::Hangup,
                    3 => Retirement::Kill,
                    _ => return Err(Failure::Invalid),
                };
                Self::Retire(handle, mode)
            }
            4 => Self::KillGroup(input.handle()?),
            5 => Self::ReleaseGroup(input.handle()?),
            _ => return Err(Failure::Invalid),
        };
        if !input.0.is_empty() {
            return Err(Failure::Invalid);
        }
        Ok(request)
    }
}

struct Encoder(Vec<u8>);
impl Encoder {
    fn count(&mut self, count: usize) -> Result<()> {
        self.0.extend_from_slice(
            &u32::try_from(count)
                .map_err(|_| Failure::Limit)?
                .to_be_bytes(),
        );
        Ok(())
    }
    fn string(&mut self, value: &OsStr) -> Result<()> {
        let bytes = value.as_bytes();
        if bytes.contains(&0) {
            return Err(Failure::Invalid);
        }
        if self
            .0
            .len()
            .checked_add(4)
            .and_then(|size| size.checked_add(bytes.len()))
            .is_none_or(|size| size > MAX_PAYLOAD)
        {
            return Err(Failure::Limit);
        }
        self.count(bytes.len())?;
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

struct Decoder<'a>(&'a [u8]);
impl Decoder<'_> {
    fn take(&mut self, size: usize) -> Result<&[u8]> {
        if size > self.0.len() {
            return Err(Failure::Invalid);
        }
        let (head, tail) = self.0.split_at(size);
        self.0 = tail;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn count(&mut self) -> Result<usize> {
        let count =
            u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| Failure::Invalid)?) as usize;
        // Even an empty native string requires its length field on the wire.
        if count > MAX_PAYLOAD {
            return Err(Failure::Limit);
        }
        Ok(count)
    }
    fn string(&mut self) -> Result<OsString> {
        let size = self.count()?;
        let bytes = self.take(size)?;
        if bytes.contains(&0) {
            return Err(Failure::Invalid);
        }
        Ok(OsString::from_vec(bytes.to_vec()))
    }
    fn handle(&mut self) -> Result<u64> {
        let handle = u64::from_be_bytes(self.take(8)?.try_into().map_err(|_| Failure::Invalid)?);
        if handle == 0 {
            return Err(Failure::Invalid);
        }
        Ok(handle)
    }
}

pub(super) fn configure(channel: &UnixStream) -> Result<()> {
    let fd = channel.as_raw_fd();
    // SAFETY: fd is owned by the channel and these operations change only its local flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(Failure::Unavailable);
    }
    #[cfg(target_os = "macos")]
    {
        let enabled: libc::c_int = 1;
        // Descriptor-local SIGPIPE suppression does not change any thread's signal mask.
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&enabled as *const libc::c_int).cast(),
                mem::size_of_val(&enabled) as _,
            )
        } < 0
        {
            return Err(Failure::Unavailable);
        }
    }
    Ok(())
}

pub(super) fn send_request(
    channel: &mut (impl Write + AsRawFd),
    payload: &[u8],
    descriptors: &[OwnedFd],
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    if payload.len() > MAX_PAYLOAD || descriptors.len() > MAX_DESCRIPTORS {
        return Err(Failure::Limit);
    }
    let mut marker = *b"S";
    let mut vector = libc::iovec {
        iov_base: marker.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // usize alignment is sufficient for cmsghdr on both supported hosts.
    let mut control = [0usize; 8];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    if !descriptors.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen =
            unsafe { libc::CMSG_SPACE((descriptors.len() * mem::size_of::<libc::c_int>()) as _) }
                as _;
        // SAFETY: control has room for the fixed maximum and is correctly aligned.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len =
                libc::CMSG_LEN((descriptors.len() * mem::size_of::<libc::c_int>()) as _) as _;
            for (index, descriptor) in descriptors.iter().enumerate() {
                libc::CMSG_DATA(header)
                    .cast::<libc::c_int>()
                    .add(index)
                    .write(descriptor.as_raw_fd());
            }
        }
    }
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_NOSIGNAL;
    #[cfg(target_os = "macos")]
    let flags = 0;
    loop {
        check()?;
        // SAFETY: all buffers remain live for the synchronous descriptor transfer.
        if unsafe { libc::sendmsg(channel.as_raw_fd(), &message, flags) } == 1 {
            break;
        }
        if !matches!(
            io::Error::last_os_error().kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ) {
            return Err(Failure::Unavailable);
        }
    }
    write_frame(channel, payload)
}

pub(super) fn receive_request(channel: &mut UnixStream) -> Result<Option<(Vec<u8>, Vec<OwnedFd>)>> {
    let mut marker = [0u8];
    let mut vector = libc::iovec {
        iov_base: marker.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 8];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = mem::size_of_val(&control) as _;
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_CMSG_CLOEXEC;
    // The exec-separated spawner receives and configures these descriptors on its
    // single request thread before spawning. macOS has no MSG_CMSG_CLOEXEC, but no
    // other thread can fork during this recvmsg-to-fcntl interval.
    #[cfg(target_os = "macos")]
    let flags = 0;
    let received = loop {
        // SAFETY: the receive buffers are writable and valid for their declared sizes.
        message.msg_controllen = mem::size_of_val(&control) as _;
        message.msg_flags = 0;
        let count = unsafe { libc::recvmsg(channel.as_raw_fd(), &mut message, flags) };
        if count >= 0 {
            break count;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(Failure::Unavailable);
        }
    };
    if received == 0 {
        return Ok(None);
    }
    let mut descriptors = Vec::new();
    let mut valid = received == 1
        && marker[0] == b'S'
        && message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) == 0;
    // Own every installed descriptor before validating the frame, including rejection paths.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let base = libc::CMSG_LEN(0) as usize;
                let length = (*header).cmsg_len as usize;
                if length < base || !(length - base).is_multiple_of(mem::size_of::<libc::c_int>()) {
                    return Err(Failure::Invalid);
                }
                for index in 0..(length - base) / mem::size_of::<libc::c_int>() {
                    let fd = libc::CMSG_DATA(header)
                        .cast::<libc::c_int>()
                        .add(index)
                        .read();
                    let owned = OwnedFd::from_raw_fd(fd);
                    valid &= libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == 0;
                    descriptors.push(owned);
                }
            } else {
                valid = false;
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    if !valid || descriptors.len() > MAX_DESCRIPTORS {
        return Err(Failure::Invalid);
    }
    Ok(Some((read_frame(channel)?, descriptors)))
}

fn write_frame(channel: &mut impl Write, bytes: &[u8]) -> Result<()> {
    channel
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|()| channel.write_all(bytes))
        .map_err(|_| Failure::Unavailable)
}
fn read_frame(channel: &mut impl Read) -> Result<Vec<u8>> {
    let mut length = [0u8; 4];
    channel
        .read_exact(&mut length)
        .map_err(|_| Failure::Unavailable)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_PAYLOAD {
        return Err(Failure::Limit);
    }
    let mut bytes = vec![0; length];
    channel
        .read_exact(&mut bytes)
        .map_err(|_| Failure::Unavailable)?;
    Ok(bytes)
}

pub(super) fn send_reply(
    channel: &mut UnixStream,
    sequence: u64,
    result: Result<Reply>,
) -> Result<()> {
    let mut bytes = Vec::with_capacity(16);
    match result {
        Err(failure) => bytes.extend_from_slice(&[0, failure as u8]),
        Ok(Reply::Spawned { handle, pid }) => {
            bytes.push(1);
            bytes.extend_from_slice(&handle.to_be_bytes());
            bytes.extend_from_slice(&pid.to_be_bytes());
        }
        Ok(Reply::Poll(exited)) => bytes.extend_from_slice(&[2, u8::from(exited)]),
        Ok(Reply::Retired { status, forced }) => {
            bytes.push(3);
            bytes.extend_from_slice(&status.to_be_bytes());
            bytes.push(u8::from(forced));
        }
        Ok(Reply::Retiring) => bytes.push(4),
        Ok(Reply::GroupRetained) => bytes.push(5),
    }
    tag(&mut bytes, sequence)?;
    write_frame(channel, &bytes)
}

pub(super) fn receive_reply(channel: &mut impl Read) -> Result<(u64, Result<Reply>)> {
    let bytes = read_frame(channel)?;
    let (sequence, bytes) = untag(&bytes)?;
    let mut input = Decoder(bytes);
    let reply = match input.byte()? {
        0 => Err(match input.byte()? {
            1 => Failure::Invalid,
            2 => Failure::NotFound,
            3 => Failure::Unavailable,
            4 => Failure::Limit,
            5 => Failure::CleanupUnresolved,
            _ => return Err(Failure::Invalid),
        }),
        1 => Ok(Reply::Spawned {
            handle: input.handle()?,
            pid: u32::from_be_bytes(input.take(4)?.try_into().map_err(|_| Failure::Invalid)?),
        }),
        2 => Ok(Reply::Poll(match input.byte()? {
            0 => false,
            1 => true,
            _ => return Err(Failure::Invalid),
        })),
        3 => Ok(Reply::Retired {
            status: i32::from_be_bytes(input.take(4)?.try_into().map_err(|_| Failure::Invalid)?),
            forced: match input.byte()? {
                0 => false,
                1 => true,
                _ => return Err(Failure::Invalid),
            },
        }),
        4 => Ok(Reply::Retiring),
        5 => Ok(Reply::GroupRetained),
        _ => return Err(Failure::Invalid),
    };
    if !input.0.is_empty() {
        return Err(Failure::Invalid);
    }
    Ok((sequence, reply))
}

// Sequences identify transactions, not child or native execution authority. Each bounded frame
// remains independently decodable after a caller stops waiting for its status observation.
pub(super) fn tag(payload: &mut Vec<u8>, sequence: u64) -> Result<()> {
    if sequence == 0 || payload.len() > MAX_PAYLOAD - 8 {
        return Err(Failure::Limit);
    }
    payload.extend_from_slice(&sequence.to_be_bytes());
    Ok(())
}

pub(super) fn untag(payload: &[u8]) -> Result<(u64, &[u8])> {
    let boundary = payload.len().checked_sub(8).ok_or(Failure::Invalid)?;
    let mut input = Decoder(&payload[boundary..]);
    Ok((input.handle()?, &payload[..boundary]))
}
