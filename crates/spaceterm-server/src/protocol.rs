//! Bounded account-local admission and length-prefixed JSON exchanges.
//! Peer-account authority comes from the socket. IDs are nonsecret fences, never credentials.
use crate::pane::control::ClientName;
use crate::transport::Transport;
use crate::{AuthenticatedStatus, ServerError};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
#[cfg(test)]
use std::io::Write;
use std::os::{fd::RawFd, unix::net::UnixStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) const MAJOR: u16 = 1;
const MAX_FRAME: usize = 8192;
const CONNECTION_LIMIT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    versions: Vec<u16>,
    build: String,
    server: [u8; 32],
    client: [u8; 32],
    name: ClientName,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    selected: u16,
    build: String,
    server: [u8; 32],
}

pub(crate) fn random() -> Result<[u8; 32], ServerError> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| ServerError::StorageUnavailable)?;
    Ok(bytes)
}

pub(crate) struct Wire<'a> {
    stream: &'a mut dyn Transport,
    deadline: Instant,
    max_frame: usize,
}
impl<'a> Wire<'a> {
    pub(crate) fn new(stream: &'a mut dyn Transport) -> Result<Self, ServerError> {
        crate::transport::nonblocking(stream.read_fd())?;
        crate::transport::nonblocking(stream.write_fd())?;
        Ok(Self {
            stream,
            deadline: Instant::now() + CONNECTION_LIMIT,
            max_frame: MAX_FRAME,
        })
    }
    pub(crate) fn terminal(stream: &'a mut dyn Transport) -> Result<Self, ServerError> {
        let mut wire = Self::new(stream)?;
        wire.max_frame = crate::pane::MAX_TERMINAL_FRAME;
        wire.deadline = Instant::now() + Duration::from_secs(10);
        Ok(wire)
    }
    fn ready(&self, events: libc::c_short) -> Result<(), ServerError> {
        loop {
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .filter(|v| !v.is_zero())
                .ok_or(ServerError::Protocol)?;
            let mut fd = libc::pollfd {
                fd: if events == libc::POLLIN {
                    self.stream.read_fd()
                } else {
                    self.stream.write_fd()
                },
                events,
                revents: 0,
            };
            // SAFETY: poll receives one writable descriptor entry retained for the call.
            let result = unsafe {
                libc::poll(
                    &mut fd,
                    1,
                    (remaining.as_millis() + 1).min(i32::MAX as u128) as i32,
                )
            };
            if result > 0 {
                return Ok(());
            }
            if result == 0 {
                return Err(ServerError::Protocol);
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return Err(ServerError::Protocol);
            }
        }
    }
    fn read_exact(&mut self, mut bytes: &mut [u8]) -> Result<(), ServerError> {
        while !bytes.is_empty() {
            self.ready(libc::POLLIN)?;
            let count = match self.stream.read(bytes) {
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(_) => return Err(ServerError::Protocol),
            };
            if count == 0 {
                return Err(ServerError::Protocol);
            }
            bytes = &mut bytes[count..];
        }
        Ok(())
    }
    pub(crate) fn read<T: DeserializeOwned>(&mut self) -> Result<T, ServerError> {
        serde_json::from_slice(&self.read_frame()?).map_err(|_| ServerError::Protocol)
    }
    pub(crate) fn read_frame(&mut self) -> Result<Vec<u8>, ServerError> {
        let mut size = [0; 4];
        self.read_exact(&mut size)?;
        let size = u32::from_be_bytes(size) as usize;
        if size > self.max_frame {
            return Err(ServerError::Protocol);
        }
        let mut bytes = vec![0; size];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    pub(crate) fn write(&mut self, value: &impl Serialize) -> Result<(), ServerError> {
        self.write_value(&[], value)
    }
    /// Writes raw bytes as one untagged frame; only byte-level transport tests need this.
    #[cfg(test)]
    pub(crate) fn write_frame(&mut self, bytes: &[u8]) -> Result<(), ServerError> {
        if bytes.len() > self.max_frame {
            return Err(ServerError::Protocol);
        }
        let mut frame = Vec::with_capacity(4 + bytes.len());
        frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        frame.extend_from_slice(bytes);
        self.write_all(&frame)
    }
    /// Writes one terminal frame. Its request ID correlates the reply on a pipelined connection.
    pub(crate) fn send(&mut self, id: u64, value: &impl Serialize) -> Result<(), ServerError> {
        self.write_value(&id.to_be_bytes(), value)
    }
    /// Reads one terminal frame with its request ID, leaving the body for its owner to decode.
    pub(crate) fn receive_frame(&mut self) -> Result<(u64, Vec<u8>), ServerError> {
        let mut header = [0; 12];
        self.read_exact(&mut header)?;
        let (size, id) = header.split_at(4);
        let size = (u32::from_be_bytes(size.try_into().map_err(|_| ServerError::Protocol)?)
            as usize)
            .checked_sub(id.len())
            .filter(|size| *size <= self.max_frame)
            .ok_or(ServerError::Protocol)?;
        let id = u64::from_be_bytes(id.try_into().map_err(|_| ServerError::Protocol)?);
        let mut bytes = vec![0; size];
        self.read_exact(&mut bytes)?;
        Ok((id, bytes))
    }
    pub(crate) fn receive<T: DeserializeOwned>(&mut self) -> Result<(u64, T), ServerError> {
        let (id, bytes) = self.receive_frame()?;
        Ok((
            id,
            serde_json::from_slice(&bytes).map_err(|_| ServerError::Protocol)?,
        ))
    }
    /// One unshared exchange on a connection with no other request outstanding.
    pub(crate) fn call<T: DeserializeOwned>(
        &mut self,
        id: u64,
        request: &impl Serialize,
    ) -> Result<T, ServerError> {
        self.send(id, request)?;
        match self.receive()? {
            (replied, reply) if replied == id => Ok(reply),
            _ => Err(ServerError::Protocol),
        }
    }
    /// Serializes after a reserved header, so the frame leaves in one write without a copy.
    /// A small frame then crosses WSS as one message and never waits for Nagle buffering.
    fn write_value(&mut self, tag: &[u8], value: &impl Serialize) -> Result<(), ServerError> {
        let header = 4 + tag.len();
        let mut frame = vec![0; header];
        serde_json::to_writer(&mut frame, value).map_err(|_| ServerError::Protocol)?;
        if frame.len() - header > self.max_frame {
            return Err(ServerError::Protocol);
        }
        let size = u32::try_from(frame.len() - 4).map_err(|_| ServerError::Protocol)?;
        frame[..4].copy_from_slice(&size.to_be_bytes());
        frame[4..header].copy_from_slice(tag);
        self.write_all(&frame)
    }
    fn write_all(&mut self, mut rest: &[u8]) -> Result<(), ServerError> {
        while !rest.is_empty() {
            self.ready(libc::POLLOUT)?;
            let count = match self.stream.write(rest) {
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(_) => return Err(ServerError::Protocol),
            };
            if count == 0 {
                return Err(ServerError::Protocol);
            }
            rest = &rest[count..];
        }
        Ok(())
    }
}

impl<T: Transport + ?Sized> Transport for &mut T {
    fn read_fd(&self) -> RawFd {
        (**self).read_fd()
    }
    fn write_fd(&self) -> RawFd {
        (**self).write_fd()
    }
}

/// One framed reader and one framed writer share a transport. Each side waits for readiness
/// without the I/O lock, so a reply still arriving never delays a request, or the reverse.
pub(crate) struct Duplex<T> {
    io: Mutex<T>,
    read_fd: RawFd,
    write_fd: RawFd,
    reading: Mutex<()>,
    writing: Mutex<()>,
}
struct Side<'a, T>(&'a Duplex<T>);
impl<T: Transport> Duplex<T> {
    pub(crate) fn new(stream: T) -> Self {
        Self {
            read_fd: stream.read_fd(),
            write_fd: stream.write_fd(),
            io: Mutex::new(stream),
            reading: Mutex::new(()),
            writing: Mutex::new(()),
        }
    }
    pub(crate) fn send(&self, id: u64, value: &impl Serialize) -> Result<(), ServerError> {
        let _writing = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        Wire::terminal(&mut Side(self))?.send(id, value)
    }
    #[cfg_attr(
        not(feature = "client"),
        allow(dead_code, reason = "clients route reply frames by request ID")
    )]
    pub(crate) fn receive_frame(&self) -> Result<(u64, Vec<u8>), ServerError> {
        let _reading = self.reading.lock().unwrap_or_else(|e| e.into_inner());
        Wire::terminal(&mut Side(self))?.receive_frame()
    }
    pub(crate) fn receive<R: DeserializeOwned>(&self) -> Result<(u64, R), ServerError> {
        let _reading = self.reading.lock().unwrap_or_else(|e| e.into_inner());
        Wire::terminal(&mut Side(self))?.receive()
    }
    /// Ends both directions for every waiting side and the peer. Descriptors stay open
    /// until the owner drops, so no waiting side can observe a reused descriptor.
    pub(crate) fn close(&self) {
        for fd in [self.read_fd, self.write_fd] {
            // SAFETY: shutdown only changes the state of a descriptor this duplex keeps open.
            unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
        }
    }
}
impl<T: Transport> std::io::Read for Side<'_, T> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(
            &mut *self.0.io.lock().unwrap_or_else(|e| e.into_inner()),
            bytes,
        )
    }
}
impl<T: Transport> std::io::Write for Side<'_, T> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(
            &mut *self.0.io.lock().unwrap_or_else(|e| e.into_inner()),
            bytes,
        )
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut *self.0.io.lock().unwrap_or_else(|e| e.into_inner()))
    }
}
impl<T: Transport> Transport for Side<'_, T> {
    fn read_fd(&self) -> RawFd {
        self.0.read_fd
    }
    fn write_fd(&self) -> RawFd {
        self.0.write_fd
    }
}

/// Both peers verify the owning account before accepting any protocol values. The client's
/// identity and name are nonsecret and grant nothing beyond the connection's authority.
pub(crate) fn admit_server(
    stream: &mut UnixStream,
    server: &[u8; 32],
    build: &str,
) -> Result<(AuthenticatedStatus, [u8; 32], ClientName), ServerError> {
    crate::host::check_peer(stream)?;
    admit_server_transport(stream, server, build, None)
}

/// Only the WSS host calls this after pinned-server and enrolled-device proof validation.
pub(crate) fn admit_direct_server(
    stream: &mut dyn Transport,
    paired: &crate::direct::PairedDevice,
) -> Result<(AuthenticatedStatus, [u8; 32], ClientName), ServerError> {
    admit_server_transport(
        stream,
        &paired.pin.server,
        &paired.pin.build,
        Some(paired.client),
    )
}

fn admit_server_transport(
    stream: &mut dyn Transport,
    server: &[u8; 32],
    build: &str,
    expected_client: Option<[u8; 32]>,
) -> Result<(AuthenticatedStatus, [u8; 32], ClientName), ServerError> {
    let mut wire = Wire::new(stream)?;
    let hello: Hello = wire.read()?;
    if hello.build != build
        || hello.server != *server
        || hello.client == [0; 32]
        || expected_client.is_some_and(|client| hello.client != client)
    {
        return Err(ServerError::AuthenticationRefused);
    }
    if hello.versions.is_empty() || hello.versions.len() > 16 {
        return Err(ServerError::Protocol);
    }
    let selected = if hello.versions.contains(&MAJOR) {
        MAJOR
    } else {
        0
    };
    wire.write(&Offer {
        selected,
        build: build.to_owned(),
        server: *server,
    })?;
    if selected == 0 {
        return Err(ServerError::IncompatibleProtocol);
    }
    Ok((
        AuthenticatedStatus {
            protocol_major: selected,
            development_pane: false,
        },
        hello.client,
        hello.name,
    ))
}

#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "client side used by desktop and admission tests")
)]
pub(crate) fn admit_client(
    stream: &mut UnixStream,
    client: (&[u8; 32], &ClientName),
    server: &[u8; 32],
    build: &str,
    versions: &[u16],
) -> Result<AuthenticatedStatus, ServerError> {
    crate::host::check_peer(stream)?;
    admit_transport(stream, client, server, build, versions)
}

pub(crate) fn admit_transport(
    stream: &mut dyn Transport,
    (client, name): (&[u8; 32], &ClientName),
    server: &[u8; 32],
    build: &str,
    versions: &[u16],
) -> Result<AuthenticatedStatus, ServerError> {
    let mut wire = Wire::new(stream)?;
    wire.write(&Hello {
        versions: versions.to_vec(),
        build: build.to_owned(),
        server: *server,
        client: *client,
        name: name.clone(),
    })?;
    let offer: Offer = wire.read()?;
    if offer.build != build || offer.server != *server {
        return Err(ServerError::AuthenticationRefused);
    }
    if offer.selected == 0 {
        return Err(ServerError::IncompatibleProtocol);
    }
    if offer.selected != MAJOR || !versions.contains(&offer.selected) {
        return Err(ServerError::IncompatibleProtocol);
    }
    Ok(AuthenticatedStatus {
        protocol_major: offer.selected,
        development_pane: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_admission_preserves_nonsecret_client_identity_and_refuses_incompatible_majors() {
        for versions in [vec![1], vec![2], vec![3, 1]] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let accepted = versions.contains(&1);
            let thread = std::thread::spawn(move || {
                admit_server(&mut server, &[7; 32], "spaceterm-development")
            });
            let result = admit_client(
                &mut client,
                (&[9; 32], &"Build Mac".into()),
                &[7; 32],
                "spaceterm-development",
                &versions,
            );
            let server_result = thread.join().unwrap();
            if accepted {
                assert_eq!(result.unwrap().protocol_major, 1);
                let (_, client, name) = server_result.unwrap();
                assert_eq!((client, name.as_str()), ([9; 32], "Build Mac"));
            } else {
                assert_eq!(result, Err(ServerError::IncompatibleProtocol));
                assert_eq!(server_result, Err(ServerError::IncompatibleProtocol));
            }
        }
    }
    #[test]
    fn substituted_build_or_server_is_refused_before_operations() {
        for (build, id) in [("spaceterm", [7; 32]), ("spaceterm-development", [8; 32])] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                admit_server(&mut server, &[7; 32], "spaceterm-development")
            });
            assert!(
                admit_client(
                    &mut client,
                    (&[9; 32], &"Build Mac".into()),
                    &id,
                    build,
                    &[1]
                )
                .is_err()
            );
            assert_eq!(
                thread.join().unwrap(),
                Err(ServerError::AuthenticationRefused)
            );
        }
    }
    #[test]
    fn client_refuses_an_offer_for_another_build_or_server() {
        for (build, server_id) in [("spaceterm", [7; 32]), ("spaceterm-development", [8; 32])] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                let mut wire = Wire::new(&mut server).unwrap();
                let _: Hello = wire.read().unwrap();
                wire.write(&Offer {
                    selected: 1,
                    build: build.into(),
                    server: server_id,
                })
                .unwrap();
            });
            assert_eq!(
                admit_client(
                    &mut client,
                    (&[9; 32], &"Build Mac".into()),
                    &[7; 32],
                    "spaceterm-development",
                    &[1]
                ),
                Err(ServerError::AuthenticationRefused)
            );
            thread.join().unwrap();
        }
    }
    #[test]
    fn oversized_frames_are_refused_before_admission() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert_eq!(
            admit_server(&mut server, &[7; 32], "spaceterm-development"),
            Err(ServerError::Protocol)
        );
    }
    #[test]
    fn a_small_frame_leaves_in_one_write() {
        // One write lets the WSS bridge forward the frame as one message and TLS record.
        struct Counted(UnixStream, usize);
        impl std::io::Read for Counted {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                std::io::Read::read(&mut self.0, bytes)
            }
        }
        impl Write for Counted {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.1 += 1;
                self.0.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.0.flush()
            }
        }
        impl Transport for Counted {
            fn read_fd(&self) -> RawFd {
                self.0.read_fd()
            }
            fn write_fd(&self) -> RawFd {
                self.0.write_fd()
            }
        }
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut client = Counted(client, 0);
        Wire::terminal(&mut client)
            .unwrap()
            .write(&crate::pane::Request::Inventory)
            .unwrap();
        assert_eq!(client.1, 1);
        assert!(matches!(
            Wire::terminal(&mut server).unwrap().read().unwrap(),
            crate::pane::Request::Inventory
        ));
    }
    #[test]
    fn stalled_connections_obey_an_absolute_deadline() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        let mut wire = Wire::new(&mut server).unwrap();
        wire.deadline = Instant::now() + Duration::from_millis(10);
        assert!(matches!(wire.read::<Hello>(), Err(ServerError::Protocol)));
    }
}
