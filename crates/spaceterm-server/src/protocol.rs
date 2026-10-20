//! Bounded account-local admission and length-prefixed JSON exchanges.
//! Peer-account authority comes from the socket. IDs are nonsecret fences, never credentials.
use crate::transport::Transport;
use crate::{AuthenticatedStatus, ServerError};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
#[cfg(test)]
use std::io::Write;
use std::os::unix::net::UnixStream;
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
        let bytes = serde_json::to_vec(value).map_err(|_| ServerError::Protocol)?;
        self.write_frame(&bytes)
    }
    pub(crate) fn write_frame(&mut self, bytes: &[u8]) -> Result<(), ServerError> {
        if bytes.len() > self.max_frame {
            return Err(ServerError::Protocol);
        }
        let prefix = (bytes.len() as u32).to_be_bytes();
        for mut rest in [prefix.as_slice(), bytes] {
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
        }
        Ok(())
    }
}

/// Both peers verify the owning account before accepting any protocol values.
pub(crate) fn admit_server(
    stream: &mut UnixStream,
    server: &[u8; 32],
    build: &str,
) -> Result<(AuthenticatedStatus, [u8; 32]), ServerError> {
    crate::host::check_peer(stream)?;
    let mut wire = Wire::new(stream)?;
    let hello: Hello = wire.read()?;
    if hello.build != build || hello.server != *server || hello.client == [0; 32] {
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
    ))
}

#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "client side used by desktop and admission tests")
)]
pub(crate) fn admit_client(
    stream: &mut UnixStream,
    client: &[u8; 32],
    server: &[u8; 32],
    build: &str,
    versions: &[u16],
) -> Result<AuthenticatedStatus, ServerError> {
    crate::host::check_peer(stream)?;
    admit_transport(stream, client, server, build, versions)
}

pub(crate) fn admit_transport(
    stream: &mut dyn Transport,
    client: &[u8; 32],
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
                &[9; 32],
                &[7; 32],
                "spaceterm-development",
                &versions,
            );
            let server_result = thread.join().unwrap();
            if accepted {
                assert_eq!(result.unwrap().protocol_major, 1);
                assert_eq!(server_result.unwrap().1, [9; 32]);
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
            assert!(admit_client(&mut client, &[9; 32], &id, build, &[1]).is_err());
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
                    &[9; 32],
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
    fn stalled_connections_obey_an_absolute_deadline() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        let mut wire = Wire::new(&mut server).unwrap();
        wire.deadline = Instant::now() + Duration::from_millis(10);
        assert!(matches!(wire.read::<Hello>(), Err(ServerError::Protocol)));
    }
}
