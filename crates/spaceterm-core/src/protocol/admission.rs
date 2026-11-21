//! Native and paired admission transport for SpaceTerm Core.
#[cfg(test)]
use super::version;
#[cfg(test)]
use super::version::Incompatible;
use super::version::{ProtocolVersion, negotiate};
use super::{AgentName, CoreVersions, Lane, Negotiated};
use crate::pane::control::ClientName;
#[cfg(test)]
use crate::update::facts::Version;
use crate::{AuthenticatedStatus, CoreError};
pub(crate) use spaceterm_protocol::protocol::admission::prefixed;
use spaceterm_protocol::protocol::admission::{Hello, MAX_FRAME, Offer, well_formed};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::Instant;

pub(crate) use spaceterm_client::admission::{
    CONNECTION_LIMIT, Caller, Preamble, Refused, Unsent, Wire, admit_service_lane,
    admit_transport_by, random,
};
#[cfg(test)]
pub(crate) use spaceterm_client::admission::{admit_lane, admit_transport};

#[cfg(test)]
pub(crate) fn versions_for_tests() -> CoreVersions {
    CoreVersions::current(Version::parse(crate::UNVERSIONED).unwrap_or_else(|_| unreachable!()))
}

// A native-local transport refusal, never a Hello or Offer. The impossible frame length
// keeps released readers' existing Protocol outcome when no admission slot is available.
const LOCAL_CAPACITY_RECEIPT: &[u8] = b"\xff\xff\xff\xffSpaceTerm local capacity\0";

/// Publishes a rejection on the connection it rejected, so capacity returning or a new Core
/// taking over the runtime cannot change its classification. Never blocks the accept loop.
pub(crate) fn reject_local_capacity(mut stream: UnixStream) {
    if stream.set_nonblocking(true).is_ok() {
        let _ = stream.write_all(LOCAL_CAPACITY_RECEIPT);
    }
}

pub(crate) struct LocalPreamble<T>(pub(crate) T);

impl<T: std::borrow::BorrowMut<UnixStream>> Preamble for LocalPreamble<T> {
    /// Writes nothing once `deadline` has passed: the clock is read again before each write.
    fn send_frame(&mut self, frame: &[u8], deadline: Instant) -> Result<(), Unsent> {
        let frame = prefixed(frame);
        let mut rest = frame.as_slice();
        let unsent = |rest: &[u8]| Unsent {
            error: CoreError::Protocol,
            written: rest.len() < frame.len(),
        };
        let stream = self.0.borrow_mut();
        if stream.set_nonblocking(true).is_err() {
            return Err(unsent(rest));
        }
        while !rest.is_empty() {
            if Instant::now() >= deadline {
                return Err(unsent(rest));
            }
            match stream.write(rest) {
                Ok(0) => return Err(unsent(rest)),
                Ok(count) => rest = &rest[count..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !crate::connection::io::wait_fd(stream.as_raw_fd(), true, deadline) {
                        return Err(unsent(rest));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(unsent(rest)),
            }
        }
        Ok(())
    }

    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, CoreError> {
        receive_frame(self.0.borrow_mut(), max, deadline)
    }
}

fn receive_frame(
    stream: &mut UnixStream,
    max: usize,
    deadline: Instant,
) -> Result<Vec<u8>, CoreError> {
    stream
        .set_nonblocking(true)
        .map_err(|_| CoreError::Protocol)?;
    let mut size = [0; 4];
    read_exact(stream, &mut size, deadline)?;
    if size == [0xff; 4] {
        let mut receipt = [0; LOCAL_CAPACITY_RECEIPT.len() - 4];
        read_exact(stream, &mut receipt, deadline)?;
        return Err(if receipt == LOCAL_CAPACITY_RECEIPT[4..] {
            CoreError::Capacity
        } else {
            CoreError::Protocol
        });
    }
    let size = u32::from_be_bytes(size) as usize;
    if size > max {
        return Err(CoreError::Protocol);
    }
    let mut bytes = vec![0; size];
    read_exact(stream, &mut bytes, deadline)?;
    Ok(bytes)
}

fn read_exact(
    stream: &mut UnixStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), CoreError> {
    while !bytes.is_empty() {
        match stream.read(bytes) {
            Ok(0) => return Err(CoreError::Protocol),
            Ok(count) => {
                bytes = &mut bytes[count..];
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if !crate::connection::io::wait_fd(stream.as_raw_fd(), false, deadline) {
                    return Err(CoreError::Protocol);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(CoreError::Protocol),
        }
    }
    Ok(())
}

/// A connection the server admitted: its effective Client Identity, which an agent name derives
/// from the base one, the name it presents and the work version it selected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Admitted {
    pub status: AuthenticatedStatus,
    pub client: [u8; 32],
    pub agent: Option<AgentName>,
    pub name: ClientName,
    /// Every version the client's Hello offered, highest first.
    pub offered: Vec<ProtocolVersion>,
}

/// A service-lane connection the server admitted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AdmittedLane {
    pub actor: crate::service::operations::Actor,
    pub selected: ProtocolVersion,
}

#[cfg(test)]
impl From<crate::service::operations::Actor> for AdmittedLane {
    /// A lane admitted at this build's service version.
    fn from(actor: crate::service::operations::Actor) -> Self {
        Self {
            actor,
            selected: version::SERVICE,
        }
    }
}

/// What a client's hello selected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// A work connection, which the caller serves with the protocol.
    Work(Admitted),
    /// A service-lane connection. The caller serves the lane, or closes the connection when it
    /// has none.
    Service(AdmittedLane),
}

#[cfg(test)]
impl Admission {
    /// The admitted work connection. Panics on a service-lane hello.
    pub(crate) fn work(self) -> Admitted {
        match self {
            Self::Work(admitted) => admitted,
            Self::Service(_) => panic!("expected a work connection"),
        }
    }
}

/// Both peers verify the owning account before accepting any protocol values. The client's
/// identity and name are nonsecret and grant nothing beyond the connection's authority.
pub(crate) fn admit_core(
    stream: &mut UnixStream,
    core: &[u8; 32],
    build: &str,
    versions: &CoreVersions,
) -> Result<Admission, CoreError> {
    crate::host::check_peer(stream)?;
    admit_core_transport(&mut LocalPreamble(stream), core, build, versions, None)
}

/// Only the paired host calls this after pinned-server and enrolled-device proof validation.
/// Every agent it admits derives from the pairing's own Client Identity.
pub(crate) fn admit_direct_machine(
    stream: &mut dyn Preamble,
    paired: &crate::direct::PairedDevice,
    versions: &CoreVersions,
) -> Result<Admission, CoreError> {
    admit_core_transport(
        stream,
        &paired.pin.core,
        &paired.pin.build,
        versions,
        Some((paired.client, paired.pairing)),
    )
}

fn admit_core_transport(
    stream: &mut dyn Preamble,
    core: &[u8; 32],
    build: &str,
    versions: &CoreVersions,
    paired: Option<([u8; 32], crate::direct::PairingId)>,
) -> Result<Admission, CoreError> {
    let expected_client = paired.map(|(client, _)| client);
    let mut wire = Wire::new(stream);
    let hello: Hello = wire.read()?;
    if hello.build != build
        || hello.core != *core
        || hello.client == [0; 32]
        || expected_client.is_some_and(|client| hello.client != client)
    {
        return Err(CoreError::AuthenticationRefused);
    }
    if !well_formed(&hello.versions) {
        return Err(CoreError::Protocol);
    }
    // An agent's Client Name keeps " CLI <agent>" whole, so every client can tell agents apart.
    if let Some(agent) = &hello.agent
        && !hello.name.as_str().ends_with(&agent.name_suffix())
    {
        return Err(CoreError::Protocol);
    }
    let client = hello
        .agent
        .as_ref()
        .map_or(hello.client, |agent| agent.client(&hello.client));
    let negotiated = negotiate(&hello.versions, versions.of(hello.lane));
    wire.write(&Offer {
        selected: negotiated.ok(),
        versions: versions.clone(),
        build: build.to_owned(),
        core: *core,
        client,
    })?;
    let selected = negotiated.map_err(CoreError::IncompatibleProtocol)?;
    if hello.lane == Lane::Service {
        stream.admitted();
        return Ok(Admission::Service(AdmittedLane {
            actor: crate::service::operations::Actor {
                pairing: paired.map(|(_, pairing)| pairing),
                client,
            },
            selected,
        }));
    }
    Ok(Admission::Work(Admitted {
        status: AuthenticatedStatus { work: selected },
        client,
        agent: hello.agent,
        name: hello.name,
        offered: hello.versions,
    }))
}

#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "client side used by desktop and admission tests")
)]
pub(crate) fn admit_client<'a>(
    stream: &mut UnixStream,
    caller: impl Into<Caller<'a>>,
    core: &[u8; 32],
    build: &str,
) -> Result<Negotiated, Refused> {
    admit_client_by(
        stream,
        caller,
        core,
        build,
        Instant::now() + CONNECTION_LIMIT,
    )
}

/// [`admit_client`] that sends and waits for no admission frame after `deadline`.
#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "client side used by desktop and admission tests")
)]
pub(crate) fn admit_client_by<'a>(
    stream: &mut UnixStream,
    caller: impl Into<Caller<'a>>,
    core: &[u8; 32],
    build: &str,
    deadline: Instant,
) -> Result<Negotiated, Refused> {
    crate::host::check_peer(stream)?;
    admit_transport_by(&mut LocalCaller(stream), caller, core, build, deadline)
}

struct LocalCaller<'a>(&'a mut UnixStream);

impl Preamble for LocalCaller<'_> {
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), CoreError> {
        let mut stream = LocalPreamble(&mut *self.0);
        stream.send(frame, deadline).map_err(|error| {
            // A rejecting Core can close before Hello is written. Its queued receipt still
            // belongs to this connection; an empty or malformed reply proves nothing.
            match stream.receive(MAX_FRAME, deadline) {
                Err(CoreError::Capacity) => CoreError::Capacity,
                _ => error,
            }
        })
    }
    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, CoreError> {
        LocalPreamble(&mut *self.0).receive(max, deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const fn v(major: u16, minor: u16) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    fn versions() -> CoreVersions {
        CoreVersions::current(Version::parse("1.4.0").unwrap())
    }

    /// Serves one admission as a Core listening with `core`.
    fn serve(
        core: CoreVersions,
    ) -> (
        UnixStream,
        std::thread::JoinHandle<Result<Admission, CoreError>>,
    ) {
        let (client, mut stream) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_core(&mut stream, &[7; 32], "spaceterm-development", &core)
        });
        (client, thread)
    }

    /// Admits `lane` offering `offered` against a Core listening with `core`.
    fn admit(
        lane: Lane,
        offered: &[ProtocolVersion],
        core: CoreVersions,
    ) -> (Result<Negotiated, Refused>, Result<Admission, CoreError>) {
        let (mut client, thread) = serve(core);
        let result = admit_lane(
            &mut LocalPreamble(&mut client),
            (&[9; 32], &"Build Mac".into()).into(),
            &[7; 32],
            "spaceterm-development",
            (lane, offered),
            Instant::now() + CONNECTION_LIMIT,
        );
        (result, thread.join().unwrap())
    }

    #[test]
    fn account_admission_preserves_nonsecret_client_identity() {
        let (mut client, thread) = serve(versions());
        let result = admit_client(
            &mut client,
            (&[9; 32], &"Build Mac".into()),
            &[7; 32],
            "spaceterm-development",
        );
        assert_eq!(
            result,
            Ok(Negotiated {
                selected: version::WORK,
                core: versions()
            })
        );
        let admitted = thread.join().unwrap().unwrap().work();
        assert_eq!(
            (admitted.client, admitted.name.as_str()),
            ([9; 32], "Build Mac")
        );
        assert_eq!(admitted.status.work, version::WORK);
        assert_eq!(admitted.offered, Lane::Work.offered());
    }

    /// A client one major ahead still speaks the Core's major, at the lower of the two minors.
    #[test]
    fn a_client_at_the_next_major_still_talks_to_a_core_at_the_previous_one() {
        let core = CoreVersions {
            work: v(1, 3),
            ..versions()
        };
        let (client, admitted) = admit(Lane::Work, &[v(2, 0), v(1, 2)], core.clone());
        assert_eq!(
            client,
            Ok(Negotiated {
                selected: v(1, 2),
                core
            })
        );
        let admitted = admitted.unwrap().work();
        assert_eq!(admitted.status.work, v(1, 2));
        assert_eq!(admitted.offered, [v(2, 0), v(1, 2)]);
    }

    #[test]
    fn with_no_shared_major_both_sides_learn_which_one_is_older() {
        for (core_work, older) in [
            (v(3, 0), Incompatible::CallerTooOld),
            (v(1, 9), Incompatible::ListenerTooOld),
        ] {
            let core = CoreVersions {
                work: core_work,
                ..versions()
            };
            let (client, admitted) = admit(Lane::Work, &[v(2, 1)], core.clone());
            assert_eq!(client, Err(Refused::Incompatible { older, core }));
            assert_eq!(admitted, Err(CoreError::IncompatibleProtocol(older)));
        }
    }

    /// The service lane negotiates its own major, so a client that shares no work major still
    /// administers the Core, and one that shares no service major is refused.
    #[test]
    fn the_service_lane_negotiates_its_own_major() {
        let core = CoreVersions {
            work: v(3, 0),
            service: v(1, 4),
            ..versions()
        };
        let (client, admitted) = admit(Lane::Service, &[v(1, 2)], core.clone());
        assert_eq!(
            client,
            Ok(Negotiated {
                selected: v(1, 2),
                core: core.clone()
            })
        );
        assert!(matches!(
            admitted,
            Ok(Admission::Service(AdmittedLane { selected, .. })) if selected == v(1, 2)
        ));
        let (client, admitted) = admit(Lane::Service, &[v(2, 0)], core.clone());
        assert_eq!(
            client,
            Err(Refused::Incompatible {
                older: Incompatible::ListenerTooOld,
                core
            })
        );
        assert_eq!(
            admitted,
            Err(CoreError::IncompatibleProtocol(
                Incompatible::ListenerTooOld
            ))
        );
    }

    #[test]
    fn a_hello_must_offer_one_or_two_descending_majors() {
        for offered in [
            vec![],
            vec![v(1, 0), v(2, 0)],
            vec![v(1, 0), v(1, 1)],
            vec![v(3, 0), v(2, 0), v(1, 0)],
        ] {
            let (_, admitted) = admit(Lane::Work, &offered, versions());
            assert_eq!(admitted, Err(CoreError::Protocol), "{offered:?}");
        }
    }

    #[test]
    fn a_client_refuses_an_offer_it_did_not_negotiate() {
        for selected in [Some(v(1, 1)), Some(v(2, 0)), None] {
            let (mut client, mut core) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                let mut preamble = LocalPreamble(&mut core);
                let mut wire = Wire::new(&mut preamble);
                let _: Hello = wire.read().unwrap();
                wire.write(&Offer {
                    selected,
                    versions: CoreVersions {
                        work: v(1, 1),
                        ..versions()
                    },
                    build: "spaceterm-development".into(),
                    core: [7; 32],
                    client: [9; 32],
                })
                .unwrap();
            });
            let result = admit_lane(
                &mut LocalPreamble(&mut client),
                (&[9; 32], &"Build Mac".into()).into(),
                &[7; 32],
                "spaceterm-development",
                (Lane::Work, &[v(1, 0)]),
                Instant::now() + CONNECTION_LIMIT,
            );
            assert_eq!(result, Err(Refused::Failed(CoreError::Protocol)));
            thread.join().unwrap();
        }
    }

    #[test]
    fn substituted_build_or_core_is_refused_before_operations() {
        for (build, id) in [("spaceterm", [7; 32]), ("spaceterm-development", [8; 32])] {
            let (mut client, mut core) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                admit_core(&mut core, &[7; 32], "spaceterm-development", &versions())
            });
            assert!(
                admit_client(&mut client, (&[9; 32], &"Build Mac".into()), &id, build).is_err()
            );
            assert_eq!(
                thread.join().unwrap(),
                Err(CoreError::AuthenticationRefused)
            );
        }
    }
    #[test]
    fn client_refuses_an_offer_for_another_build_or_core() {
        for (build, machine_id) in [("spaceterm", [7; 32]), ("spaceterm-development", [8; 32])] {
            let (mut client, mut core) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                let mut preamble = LocalPreamble(&mut core);
                let mut wire = Wire::new(&mut preamble);
                let _: Hello = wire.read().unwrap();
                wire.write(&Offer {
                    selected: Some(version::WORK),
                    versions: versions(),
                    build: build.into(),
                    core: machine_id,
                    client: [9; 32],
                })
                .unwrap();
            });
            assert_eq!(
                admit_client(
                    &mut client,
                    (&[9; 32], &"Build Mac".into()),
                    &[7; 32],
                    "spaceterm-development",
                ),
                Err(Refused::Failed(CoreError::AuthenticationRefused))
            );
            thread.join().unwrap();
        }
    }
    #[test]
    fn oversized_frames_are_refused_before_admission() {
        let (mut client, mut core) = UnixStream::pair().unwrap();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert_eq!(
            admit_core(&mut core, &[7; 32], "spaceterm-development", &versions()),
            Err(CoreError::Protocol)
        );
    }
    #[test]
    fn stalled_connections_obey_an_absolute_deadline() {
        let (_client, mut core) = UnixStream::pair().unwrap();
        let mut preamble = LocalPreamble(&mut core);
        let mut wire = Wire::until(&mut preamble, Instant::now() + Duration::from_millis(10));
        assert!(matches!(wire.read::<Hello>(), Err(CoreError::Protocol)));
    }
    #[test]
    fn an_agent_is_admitted_as_the_identity_the_machine_derives_from_its_base() {
        let agent: AgentName = "agent-1".parse().unwrap();
        let derived = agent.client(&[9; 32]);
        assert_ne!(derived, [9; 32]);
        assert_ne!(
            derived,
            agent.client(&[8; 32]),
            "each base derives its own agents"
        );
        let other: AgentName = "agent-2".parse().unwrap();
        assert_ne!(derived, other.client(&[9; 32]));
        let (mut client, mut core) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_core(&mut core, &[7; 32], "spaceterm-development", &versions())
        });
        let name: ClientName = "Build Mac CLI agent-1".into();
        admit_client(
            &mut client,
            Caller {
                client: &[9; 32],
                name: &name,
                agent: Some(&agent),
            },
            &[7; 32],
            "spaceterm-development",
        )
        .unwrap();
        let admitted = thread.join().unwrap().unwrap().work();
        assert_eq!(admitted.client, derived);
        assert_eq!(admitted.agent, Some(agent));
    }

    #[test]
    fn an_agent_whose_name_hides_it_is_refused() {
        let agent: AgentName = "agent-1".parse().unwrap();
        let (mut client, mut core) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_core(&mut core, &[7; 32], "spaceterm-development", &versions())
        });
        let name: ClientName = "Build Mac".into();
        assert!(
            admit_client(
                &mut client,
                Caller {
                    client: &[9; 32],
                    name: &name,
                    agent: Some(&agent),
                },
                &[7; 32],
                "spaceterm-development",
            )
            .is_err()
        );
        assert_eq!(thread.join().unwrap(), Err(CoreError::Protocol));
    }

    /// A paired connection's Hello must carry its pairing's own Client Identity, so its agents
    /// always derive from it and never from another pairing's.
    #[test]
    fn a_paired_client_cannot_admit_an_agent_of_another_pairing() {
        let agent: AgentName = "agent-1".parse().unwrap();
        let (mut client, mut core) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_core_transport(
                &mut LocalPreamble(&mut core),
                &[7; 32],
                "spaceterm-development",
                &versions(),
                Some(([9; 32], [8; 32])),
            )
        });
        let name: ClientName = "Build Mac CLI agent-1".into();
        assert!(
            admit_transport(
                &mut LocalPreamble(&mut client),
                Caller {
                    client: &[8; 32],
                    name: &name,
                    agent: Some(&agent),
                },
                &[7; 32],
                "spaceterm-development",
            )
            .is_err()
        );
        assert_eq!(
            thread.join().unwrap(),
            Err(CoreError::AuthenticationRefused)
        );
    }

    #[test]
    fn agent_names_follow_their_pattern() {
        for valid in ["a", "agent-1", "0", &"a".repeat(32)] {
            assert!(valid.parse::<AgentName>().is_ok(), "{valid}");
        }
        for invalid in ["", "-a", "Agent", "a_b", "a b", &"a".repeat(33), "é"] {
            assert!(invalid.parse::<AgentName>().is_err(), "{invalid}");
        }
    }

    /// The account-local transport reads the clock before each write: a frame whose deadline
    /// passed writes nothing, and one whose deadline passes while its peer reads nothing says
    /// that part of it left.
    #[test]
    fn a_unix_frame_writes_nothing_after_its_deadline_and_says_when_part_of_it_left() {
        let (sender, mut peer) = UnixStream::pair().unwrap();
        let mut sender = LocalPreamble(sender);
        assert_eq!(
            sender.send_frame(b"{\"request\":\"execute\"}", Instant::now()),
            Err(Unsent {
                error: CoreError::Protocol,
                written: false,
            })
        );
        peer.set_nonblocking(true).unwrap();
        let mut byte = [0; 1];
        assert_eq!(
            peer.read(&mut byte).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::WouldBlock)
        );
        // Far more than the socket buffers, which nobody reads.
        let frame = vec![0; 8 * 1024 * 1024];
        let began = Instant::now();
        let unsent = sender
            .send_frame(&frame, began + Duration::from_millis(200))
            .unwrap_err();
        assert!(unsent.written);
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "{:?}",
            began.elapsed()
        );
    }
}
