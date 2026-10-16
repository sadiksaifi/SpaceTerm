//! Length-prefixed JSON with bounded frames and absolute exchange deadlines.
//! Signatures cover both advertisements, the selected major, both nonces, and both identities.
use std::io::{Read, Write};
use std::os::{fd::AsRawFd, unix::net::UnixStream};
use std::time::{Duration, Instant};

use ring::{
    rand::{SecureRandom, SystemRandom},
    signature::{self, Ed25519KeyPair, KeyPair},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{AuthenticatedStatus, ServerError};

const VERSIONS: &[u16] = &[2, 1];
const MAX_FRAME: usize = 8192;
const CONNECTION_LIMIT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    versions: Vec<u16>,
    device: [u8; 32],
    nonce: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    versions: Vec<u16>,
    selected: u16,
    server: [u8; 32],
    nonce: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Challenge {
    offer: Offer,
    proof: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    signature: Vec<u8>,
}

fn negotiate(client: &[u16], server: &[u16]) -> Option<u16> {
    client.iter().filter(|v| server.contains(v)).copied().max()
}

pub(crate) fn key(seed: &[u8; 32]) -> Result<Ed25519KeyPair, ServerError> {
    Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| ServerError::AuthenticationRefused)
}

pub(crate) fn public(key: &Ed25519KeyPair) -> [u8; 32] {
    // Ed25519 always has a 32-byte public key.
    let mut bytes = [0; 32];
    bytes.copy_from_slice(key.public_key().as_ref());
    bytes
}

pub(crate) fn random() -> Result<[u8; 32], ServerError> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| ServerError::StorageUnavailable)?;
    Ok(bytes)
}

fn transcript(hello: &Hello, offer: &Offer) -> Result<Vec<u8>, ServerError> {
    serde_json::to_vec(&("SpaceTerm local authentication 1", hello, offer))
        .map_err(|_| ServerError::Protocol)
}
fn message(role: &[u8], transcript: &[u8], previous: &[u8]) -> Vec<u8> {
    [role, transcript, previous].concat()
}
fn verify(public: &[u8; 32], message: &[u8], signature: &[u8]) -> Result<(), ServerError> {
    signature::UnparsedPublicKey::new(&signature::ED25519, public)
        .verify(message, signature)
        .map_err(|_| ServerError::AuthenticationRefused)
}

pub(crate) struct Wire<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
    max_frame: usize,
}
impl<'a> Wire<'a> {
    fn new(stream: &'a mut UnixStream) -> Result<Self, ServerError> {
        stream
            .set_nonblocking(true)
            .map_err(|_| ServerError::Protocol)?;
        Ok(Self {
            stream,
            deadline: Instant::now() + CONNECTION_LIMIT,
            max_frame: MAX_FRAME,
        })
    }
    pub(crate) fn terminal(stream: &'a mut UnixStream) -> Result<Self, ServerError> {
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
                fd: self.stream.as_raw_fd(),
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
        let mut size = [0; 4];
        self.read_exact(&mut size)?;
        let size = u32::from_be_bytes(size) as usize;
        if size > self.max_frame {
            return Err(ServerError::Protocol);
        }
        let mut bytes = vec![0; size];
        self.read_exact(&mut bytes)?;
        serde_json::from_slice(&bytes).map_err(|_| ServerError::Protocol)
    }
    pub(crate) fn write(&mut self, value: &impl Serialize) -> Result<(), ServerError> {
        let bytes = serde_json::to_vec(value).map_err(|_| ServerError::Protocol)?;
        if bytes.len() > self.max_frame {
            return Err(ServerError::Protocol);
        }
        let frame = [&(bytes.len() as u32).to_be_bytes()[..], &bytes].concat();
        let mut rest = frame.as_slice();
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

#[cfg(test)]
pub(crate) fn authenticate_server(
    stream: &mut UnixStream,
    server: &Ed25519KeyPair,
    paired: &[[u8; 32]],
) -> Result<AuthenticatedStatus, ServerError> {
    authenticate_server_device(stream, server, paired).map(|(status, _)| status)
}

pub(crate) fn authenticate_server_device(
    stream: &mut UnixStream,
    server: &Ed25519KeyPair,
    paired: &[[u8; 32]],
) -> Result<(AuthenticatedStatus, [u8; 32]), ServerError> {
    let mut wire = Wire::new(stream)?;
    let hello: Hello = wire.read()?;
    if hello.versions.is_empty() || hello.versions.len() > 2 || !paired.contains(&hello.device) {
        return Err(ServerError::AuthenticationRefused);
    }
    let offer = Offer {
        versions: VERSIONS.to_vec(),
        selected: negotiate(&hello.versions, VERSIONS).unwrap_or(0),
        server: public(server),
        nonce: random()?,
    };
    let transcript = transcript(&hello, &offer)?;
    let proof = server
        .sign(&message(b"server", &transcript, &[]))
        .as_ref()
        .to_vec();
    let selected = offer.selected;
    wire.write(&Challenge {
        offer,
        proof: proof.clone(),
    })?;
    if selected == 0 {
        return Err(ServerError::IncompatibleProtocol);
    }
    let client: Proof = wire.read()?;
    verify(
        &hello.device,
        &message(b"client", &transcript, &proof),
        &client.signature,
    )?;
    wire.write(&Proof {
        signature: server
            .sign(&message(b"status", &transcript, &client.signature))
            .as_ref()
            .to_vec(),
    })?;
    Ok((
        AuthenticatedStatus {
            protocol_major: selected,
            development_pane: false,
        },
        hello.device,
    ))
}

#[cfg_attr(
    not(feature = "client"),
    allow(
        dead_code,
        reason = "client side used by desktop and authentication tests"
    )
)]
pub(crate) fn authenticate_client(
    stream: &mut UnixStream,
    device: &Ed25519KeyPair,
    pin: &[u8; 32],
    versions: &[u16],
) -> Result<AuthenticatedStatus, ServerError> {
    let hello = Hello {
        versions: versions.to_vec(),
        device: public(device),
        nonce: random()?,
    };
    let mut wire = Wire::new(stream)?;
    wire.write(&hello)?;
    let challenge: Challenge = wire.read()?;
    if challenge.offer.server != *pin || challenge.offer.versions.len() > 2 {
        return Err(ServerError::AuthenticationRefused);
    }
    let transcript = transcript(&hello, &challenge.offer)?;
    verify(pin, &message(b"server", &transcript, &[]), &challenge.proof)?;
    let selected = negotiate(versions, &challenge.offer.versions);
    if selected.is_none() && challenge.offer.selected == 0 {
        return Err(ServerError::IncompatibleProtocol);
    }
    if selected != Some(challenge.offer.selected) || !VERSIONS.contains(&challenge.offer.selected) {
        return Err(ServerError::AuthenticationRefused);
    }
    let proof = device
        .sign(&message(b"client", &transcript, &challenge.proof))
        .as_ref()
        .to_vec();
    wire.write(&Proof {
        signature: proof.clone(),
    })?;
    let status: Proof = wire.read()?;
    verify(
        pin,
        &message(b"status", &transcript, &proof),
        &status.signature,
    )?;
    Ok(AuthenticatedStatus {
        protocol_major: challenge.offer.selected,
        development_pane: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticates_both_supported_majors() {
        for major in [1, 2] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let server_key = key(&random().unwrap()).unwrap();
            let pin = public(&server_key);
            let device = key(&random().unwrap()).unwrap();
            let paired = [public(&device)];
            let thread =
                std::thread::spawn(move || authenticate_server(&mut server, &server_key, &paired));
            let result = authenticate_client(&mut client, &device, &pin, &[major]);
            drop(client);
            assert_eq!(
                (result, thread.join().unwrap()),
                (
                    Ok(AuthenticatedStatus {
                        protocol_major: major,
                        development_pane: false
                    }),
                    Ok(AuthenticatedStatus {
                        protocol_major: major,
                        development_pane: false
                    })
                )
            );
        }
    }
    #[test]
    fn selects_newest_shared_major_independently_of_advertisement_order() {
        assert_eq!(negotiate(&[1, 2], &[1, 2]), Some(2));
    }
}

#[cfg(test)]
mod rejection_tests {
    use super::*;
    #[test]
    fn wrong_server_pin_is_refused_in_both_majors() {
        for major in [1, 2] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let server_key = key(&random().unwrap()).unwrap();
            let device = key(&random().unwrap()).unwrap();
            let paired = [public(&device)];
            let thread =
                std::thread::spawn(move || authenticate_server(&mut server, &server_key, &paired));
            assert_eq!(
                authenticate_client(&mut client, &device, &[0; 32], &[major]),
                Err(ServerError::AuthenticationRefused)
            );
            drop(client);
            assert!(thread.join().unwrap().is_err());
        }
    }
    #[test]
    fn unpaired_devices_never_receive_authenticated_status() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let server_key = key(&random().unwrap()).unwrap();
        let pin = public(&server_key);
        let device = key(&random().unwrap()).unwrap();
        let thread = std::thread::spawn(move || authenticate_server(&mut server, &server_key, &[]));
        assert!(authenticate_client(&mut client, &device, &pin, &[2, 1]).is_err());
        assert_eq!(
            thread.join().unwrap(),
            Err(ServerError::AuthenticationRefused)
        );
    }
    #[test]
    fn forged_device_proofs_are_refused_in_both_majors() {
        for major in [1, 2] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let server_key = key(&random().unwrap()).unwrap();
            let device = key(&random().unwrap()).unwrap();
            let paired = [public(&device)];
            let thread =
                std::thread::spawn(move || authenticate_server(&mut server, &server_key, &paired));
            let mut wire = Wire::new(&mut client).unwrap();
            wire.write(&Hello {
                versions: vec![major],
                device: public(&device),
                nonce: random().unwrap(),
            })
            .unwrap();
            let _: Challenge = wire.read().unwrap();
            wire.write(&Proof {
                signature: vec![0; 64],
            })
            .unwrap();
            assert_eq!(
                thread.join().unwrap(),
                Err(ServerError::AuthenticationRefused)
            );
        }
    }
    #[test]
    fn refuses_signed_downgrades_from_the_newest_shared_major() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let server_key = key(&random().unwrap()).unwrap();
        let pin = public(&server_key);
        let device = key(&random().unwrap()).unwrap();
        let thread = std::thread::spawn(move || {
            let mut wire = Wire::new(&mut server).unwrap();
            let hello: Hello = wire.read().unwrap();
            let offer = Offer {
                versions: vec![2, 1],
                selected: 1,
                server: public(&server_key),
                nonce: random().unwrap(),
            };
            let proof = server_key
                .sign(&message(
                    b"server",
                    &transcript(&hello, &offer).unwrap(),
                    &[],
                ))
                .as_ref()
                .to_vec();
            wire.write(&Challenge { offer, proof }).unwrap();
        });
        assert_eq!(
            authenticate_client(&mut client, &device, &pin, &[2, 1]),
            Err(ServerError::AuthenticationRefused)
        );
        thread.join().unwrap();
    }
    #[test]
    fn tampered_server_advertisements_are_refused_with_a_valid_selection() {
        for major in [1, 2] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let server_key = key(&random().unwrap()).unwrap();
            let pin = public(&server_key);
            let device = key(&random().unwrap()).unwrap();
            let thread = std::thread::spawn(move || {
                let mut wire = Wire::new(&mut server).unwrap();
                let hello: Hello = wire.read().unwrap();
                let mut offer = Offer {
                    versions: vec![2, 1],
                    selected: major,
                    server: public(&server_key),
                    nonce: random().unwrap(),
                };
                let proof = server_key
                    .sign(&message(
                        b"server",
                        &transcript(&hello, &offer).unwrap(),
                        &[],
                    ))
                    .as_ref()
                    .to_vec();
                // Keep the selected newest intersection valid, so only transcript verification
                // can reject the changed advertisement.
                offer.versions = vec![major, 0];
                assert_eq!(negotiate(&hello.versions, &offer.versions), Some(major));
                wire.write(&Challenge { offer, proof }).unwrap();
            });
            assert_eq!(
                authenticate_client(&mut client, &device, &pin, &[major]),
                Err(ServerError::AuthenticationRefused)
            );
            thread.join().unwrap();
        }
    }
    #[test]
    fn tampered_client_advertisements_are_refused_with_a_valid_selection() {
        for major in [1, 2] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let server_key = key(&random().unwrap()).unwrap();
            let pin = public(&server_key);
            let device = key(&random().unwrap()).unwrap();
            let thread = std::thread::spawn(move || {
                let mut wire = Wire::new(&mut server).unwrap();
                let mut hello: Hello = wire.read().unwrap();
                hello.versions = vec![major, 0];
                let offer = Offer {
                    versions: vec![2, 1],
                    selected: major,
                    server: public(&server_key),
                    nonce: random().unwrap(),
                };
                assert_eq!(negotiate(&hello.versions, &offer.versions), Some(major));
                let proof = server_key
                    .sign(&message(
                        b"server",
                        &transcript(&hello, &offer).unwrap(),
                        &[],
                    ))
                    .as_ref()
                    .to_vec();
                wire.write(&Challenge { offer, proof }).unwrap();
            });
            assert_eq!(
                authenticate_client(&mut client, &device, &pin, &[major]),
                Err(ServerError::AuthenticationRefused)
            );
            thread.join().unwrap();
        }
    }
    #[test]
    fn incompatible_versions_are_refused_after_authenticating_advertisements() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let server_key = key(&random().unwrap()).unwrap();
        let pin = public(&server_key);
        let device = key(&random().unwrap()).unwrap();
        let paired = [public(&device)];
        let thread =
            std::thread::spawn(move || authenticate_server(&mut server, &server_key, &paired));
        assert_eq!(
            authenticate_client(&mut client, &device, &pin, &[7]),
            Err(ServerError::IncompatibleProtocol)
        );
        assert_eq!(
            thread.join().unwrap(),
            Err(ServerError::IncompatibleProtocol)
        );
    }
    #[test]
    fn oversized_frames_are_refused_before_handshake() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert_eq!(
            authenticate_server(&mut server, &key(&random().unwrap()).unwrap(), &[]),
            Err(ServerError::Protocol)
        );
    }

    #[test]
    fn authenticated_graphics_chunks_round_trip_through_both_majors() {
        use crate::pane::{Lease, Reply, Request, graphics::*};
        for major in [1, 2] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let server_key = key(&random().unwrap()).unwrap();
            let pin = public(&server_key);
            let device = key(&random().unwrap()).unwrap();
            let paired = [public(&device)];
            let image = ImageKey {
                image_id: 7,
                generation: 9,
            };
            let lease = Lease {
                server: [1; 32],
                session: [2; 32],
                control: [3; 32],
                connection: [4; 32],
            };
            let thread = std::thread::spawn(move || {
                assert_eq!(
                    authenticate_server(&mut server, &server_key, &paired)
                        .unwrap()
                        .protocol_major,
                    major
                );
                let mut wire = Wire::terminal(&mut server).unwrap();
                assert!(
                    matches!(wire.read::<Request>().unwrap(), Request::GraphicsChunk { lease: actual, image: key, offset: CHUNK_BYTES } if actual == lease && key == image)
                );
                wire.write(&Reply::GraphicsChunk(Ok(Chunk {
                    image,
                    offset: CHUNK_BYTES,
                    bytes: vec![255; CHUNK_BYTES],
                })))
                .unwrap();
            });
            assert_eq!(
                authenticate_client(&mut client, &device, &pin, &[major])
                    .unwrap()
                    .protocol_major,
                major
            );
            let mut wire = Wire::terminal(&mut client).unwrap();
            wire.write(&Request::GraphicsChunk {
                lease,
                image,
                offset: CHUNK_BYTES,
            })
            .unwrap();
            assert!(
                matches!(wire.read::<Reply>().unwrap(), Reply::GraphicsChunk(Ok(chunk)) if chunk.image == image && chunk.offset == CHUNK_BYTES && chunk.bytes == vec![255; CHUNK_BYTES])
            );
            thread.join().unwrap();
        }
    }
    #[test]
    fn stalled_connections_obey_an_absolute_deadline() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        let mut wire = Wire::new(&mut server).unwrap();
        wire.deadline = Instant::now() + Duration::from_millis(10);
        assert!(matches!(wire.read::<Hello>(), Err(ServerError::Protocol)));
    }
}
