//! Bounded admission: the Hello/Offer preamble every protocol major reads the same way, as
//! length-prefixed JSON. An incompatible peer therefore always receives a typed incompatibility.
//! After admission both peers switch to the postcard framing in [`super::frame`].
//! Peer-account authority comes from the socket. IDs are nonsecret fences, never credentials.
use crate::pane::control::ClientName;
use crate::{AuthenticatedStatus, ServerError};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// The protocol major of the pushed, multiplexed exchange.
pub const MAJOR: u16 = 3;
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

/// The transport of one admission: whole length-prefixed frames, each before a deadline. Account
/// IPC writes them back to back; a paired connection carries the same bytes on its QUIC L0 stream.
pub(crate) trait Preamble {
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), ServerError>;
    /// One frame of at most `max` bytes, without its length prefix.
    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, ServerError>;
}

/// Prefixes `bytes` with their length, the admission framing on every transport.
pub(crate) fn prefixed(bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(bytes);
    frame
}

impl Preamble for UnixStream {
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), ServerError> {
        self.set_nonblocking(true)
            .map_err(|_| ServerError::Protocol)?;
        let frame = prefixed(frame);
        let mut rest = frame.as_slice();
        while !rest.is_empty() {
            match self.write(rest) {
                Ok(0) => return Err(ServerError::Protocol),
                Ok(count) => rest = &rest[count..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !crate::connection::io::wait_fd(self.as_raw_fd(), true, deadline) {
                        return Err(ServerError::Protocol);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(ServerError::Protocol),
            }
        }
        Ok(())
    }

    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, ServerError> {
        self.set_nonblocking(true)
            .map_err(|_| ServerError::Protocol)?;
        let mut size = [0; 4];
        read_exact(self, &mut size, deadline)?;
        let size = u32::from_be_bytes(size) as usize;
        if size > max {
            return Err(ServerError::Protocol);
        }
        let mut bytes = vec![0; size];
        read_exact(self, &mut bytes, deadline)?;
        Ok(bytes)
    }
}

fn read_exact(
    stream: &mut UnixStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), ServerError> {
    while !bytes.is_empty() {
        match stream.read(bytes) {
            Ok(0) => return Err(ServerError::Protocol),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if !crate::connection::io::wait_fd(stream.as_raw_fd(), false, deadline) {
                    return Err(ServerError::Protocol);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(ServerError::Protocol),
        }
    }
    Ok(())
}

/// JSON admission messages under one absolute deadline.
pub(crate) struct Wire<'a> {
    stream: &'a mut dyn Preamble,
    deadline: Instant,
}
impl<'a> Wire<'a> {
    pub(crate) fn new(stream: &'a mut dyn Preamble) -> Self {
        Self {
            stream,
            deadline: Instant::now() + CONNECTION_LIMIT,
        }
    }
    pub(crate) fn read<T: DeserializeOwned>(&mut self) -> Result<T, ServerError> {
        let bytes = self.stream.receive(MAX_FRAME, self.deadline)?;
        serde_json::from_slice(&bytes).map_err(|_| ServerError::Protocol)
    }
    pub(crate) fn write(&mut self, value: &impl Serialize) -> Result<(), ServerError> {
        let bytes = serde_json::to_vec(value).map_err(|_| ServerError::Protocol)?;
        if bytes.len() > MAX_FRAME {
            return Err(ServerError::Protocol);
        }
        self.stream.send(&bytes, self.deadline)
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

/// Only the paired host calls this after pinned-server and enrolled-device proof validation.
pub(crate) fn admit_direct_server(
    stream: &mut dyn Preamble,
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
    stream: &mut dyn Preamble,
    server: &[u8; 32],
    build: &str,
    expected_client: Option<[u8; 32]>,
) -> Result<(AuthenticatedStatus, [u8; 32], ClientName), ServerError> {
    let mut wire = Wire::new(stream);
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
    stream: &mut dyn Preamble,
    (client, name): (&[u8; 32], &ClientName),
    server: &[u8; 32],
    build: &str,
    versions: &[u16],
) -> Result<AuthenticatedStatus, ServerError> {
    let mut wire = Wire::new(stream);
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
        for versions in [vec![2], vec![1], vec![3, 2]] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let accepted = versions.contains(&MAJOR);
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
                assert_eq!(result.unwrap().protocol_major, MAJOR);
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
                    &[MAJOR]
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
                let mut wire = Wire::new(&mut server);
                let _: Hello = wire.read().unwrap();
                wire.write(&Offer {
                    selected: MAJOR,
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
                    &[MAJOR]
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
    fn stalled_connections_obey_an_absolute_deadline() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        let mut wire = Wire::new(&mut server);
        wire.deadline = Instant::now() + Duration::from_millis(10);
        assert!(matches!(wire.read::<Hello>(), Err(ServerError::Protocol)));
    }
}
