//! The agent names a pairing admitted stay bounded without blocking a new name.
use super::*;
use crate::direct::DeviceCredential;
use crate::direct::tests::{Fixture, Quiet};

fn agent(index: usize) -> crate::protocol::AgentName {
    format!("agent-{index}").parse().unwrap()
}

#[test]
fn a_new_agent_name_past_the_limit_evicts_the_least_recently_admitted_idle_name() {
    let fixture = Fixture::new();
    fixture.enable();
    let paired = fixture.enroll(&DeviceCredential::generate().unwrap());
    let mut state = fixture.state.lock().unwrap();
    for index in 0..storage::MAX_AGENTS {
        assert_eq!(state.admit_agent(&paired, &agent(index)), Ok(None));
    }
    // Agent 0 is the oldest name, but it has a live connection.
    let generation = state.generation;
    state
        .register(
            &paired,
            generation,
            [5; 32],
            agent(0).client(&paired.client),
            None,
            Box::new(Quiet),
        )
        .unwrap();
    // Admitting a known name again makes it the most recent, and evicts nothing.
    assert_eq!(state.admit_agent(&paired, &agent(1)), Ok(None));
    let evicted = state
        .admit_agent(&paired, &agent(storage::MAX_AGENTS))
        .unwrap();
    assert_eq!(evicted, Some(agent(2).client(&paired.client)));
    let agents = |state: &State| {
        state
            .record
            .devices
            .iter()
            .find(|device| device.info.pairing == paired.pairing)
            .unwrap()
            .agents
            .clone()
    };
    let known = agents(&state);
    assert_eq!(known.len(), storage::MAX_AGENTS);
    assert!(known.contains(&agent(0)) && known.contains(&agent(1)));
    assert!(!known.contains(&agent(2)));
    assert_eq!(known.last(), Some(&agent(storage::MAX_AGENTS)));
    // An evicted agent that returns is admitted again.
    assert_eq!(
        state.admit_agent(&paired, &agent(2)),
        Ok(Some(agent(3).client(&paired.client)))
    );
    // The bounded list survives a reload.
    drop(state);
    let reopened = State::open(&fixture.paths).unwrap();
    assert_eq!(agents(&reopened.lock().unwrap()).len(), storage::MAX_AGENTS);
}

/// What a pane service saw: identities with a retained edit outcome, identities holding a
/// control grant, and each release and revocation in order.
#[derive(Default)]
struct Seen {
    outcomes: std::collections::BTreeSet<[u8; 32]>,
    grants: std::collections::BTreeSet<[u8; 32]>,
    events: Vec<Event>,
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Released([u8; 32]),
    Revoked([u8; 32]),
    Claimed([u8; 32]),
}

/// A pane service that answers `Resolve { sequence: 1 }` from each identity's retained outcome,
/// and records each `Claim` as a control grant for the claiming identity. It has no Sessions to
/// attach, so a claim is then refused.
struct Panes(Arc<Mutex<Seen>>);
impl crate::connection::service::PaneService for Panes {
    fn admitted(
        &mut self,
        _: &crate::connection::service::Peer,
        _: ClientName,
        _: crate::connection::outbox::Outbox,
    ) {
    }
    fn request(
        &mut self,
        peer: &crate::connection::service::Peer,
        _: crate::protocol::RequestId,
        operation: crate::protocol::Operation,
    ) -> crate::connection::service::Dispatch {
        use crate::protocol::{Operation, Refusal, ReplyBody};
        let mut seen = self.0.lock().unwrap();
        crate::connection::service::Dispatch::reply(match operation {
            Operation::Resolve { sequence: 1 } if seen.outcomes.contains(&peer.client) => {
                Ok(ReplyBody::Done)
            }
            Operation::Resolve { .. } => Err(Refusal::Stale),
            Operation::Claim(_) => {
                seen.grants.insert(peer.client);
                seen.events.push(Event::Claimed(peer.client));
                Err(Refusal::Unsupported)
            }
            _ => Err(Refusal::Unsupported),
        })
    }
    fn revoked(&mut self, client: [u8; 32]) {
        let mut seen = self.0.lock().unwrap();
        seen.outcomes.remove(&client);
        seen.grants.remove(&client);
        seen.events.push(Event::Revoked(client));
    }
    fn release_control(&mut self, client: [u8; 32]) {
        let mut seen = self.0.lock().unwrap();
        seen.grants.remove(&client);
        seen.events.push(Event::Released(client));
    }
    fn input(&mut self, _: &crate::connection::service::Peer, _: crate::protocol::Input) {}
    fn view(&mut self, _: &crate::connection::service::Peer, _: crate::protocol::View) {}
    fn effect_done(
        &mut self,
        _: &crate::connection::service::Peer,
        _: crate::protocol::EffectDone,
    ) {
    }
    fn acknowledge(&mut self, _: &crate::connection::service::Peer, _: u64) {}
    fn disconnected(&mut self, _: &crate::connection::service::Peer) {}
    fn shutdown(&mut self) -> Result<(), ServerError> {
        Ok(())
    }
}

/// A pairing whose agent names fill its limit, and a pane service in which the oldest name,
/// agent 0, holds a retained outcome and a control grant.
fn full_pairing() -> (
    Fixture,
    DeviceCredential,
    PairedDevice,
    Arc<Mutex<Seen>>,
    crate::connection::service::SharedPaneService,
) {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    {
        let mut state = fixture.state.lock().unwrap();
        for index in 0..storage::MAX_AGENTS {
            assert_eq!(state.admit_agent(&paired, &agent(index)), Ok(None));
        }
    }
    let seen = Arc::new(Mutex::new(Seen::default()));
    let oldest = agent(0).client(&paired.client);
    seen.lock().unwrap().outcomes.insert(oldest);
    seen.lock().unwrap().grants.insert(oldest);
    let panes = crate::direct::tests::service(Panes(Arc::clone(&seen)));
    (fixture, credential, paired, seen, panes)
}

/// Connects and admits `name` as an agent of `paired`.
fn admit(
    fixture: &Fixture,
    credential: &DeviceCredential,
    paired: &PairedDevice,
    name: &crate::protocol::AgentName,
) -> crate::connection::link::ServerLink {
    let connection = DirectConnection::connect(&fixture.endpoint(), paired, credential).unwrap();
    let carrier = connection
        .admit(crate::protocol::Caller {
            client: &paired.client,
            name: &format!("Test{}", name.name_suffix()).into(),
            agent: Some(name),
        })
        .unwrap();
    crate::connection::io::spawn_link(carrier, Arc::new(|_| {})).unwrap()
}

fn resolve(link: &crate::connection::link::ServerLink) -> bool {
    matches!(
        crate::direct::tests::request(link, crate::protocol::Operation::Resolve { sequence: 1 }),
        Ok(crate::protocol::ReplyBody::Done)
    )
}

fn claim(link: &crate::connection::link::ServerLink) {
    let _ = crate::direct::tests::request(
        link,
        crate::protocol::Operation::Claim(crate::protocol::Claim {
            channel: crate::protocol::Channel(1),
            attachment: crate::attachments::Attachment {
                workspace: [1; 32],
                generation: 1,
            },
            pane: [2; 32],
            claim: crate::pane::AttachmentClaim::Select { explicit: true },
            size: None,
            style: None,
        }),
    );
}

#[test]
fn an_evicted_agent_keeps_its_outcome_and_resolves_it_when_it_returns() {
    let (fixture, credential, paired, seen, panes) = full_pairing();
    let oldest = agent(0).client(&paired.client);
    let newcomer: crate::protocol::AgentName = "newcomer".parse().unwrap();
    let first = fixture.serve_next_with(Arc::clone(&panes));
    let link = admit(&fixture, &credential, &paired, &newcomer);
    // Any reply follows the eviction's release, which runs before serving.
    assert!(!resolve(&link));
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.events, vec![Event::Released(oldest)]);
        assert!(seen.outcomes.contains(&oldest) && !seen.grants.contains(&oldest));
    }
    // The returning agent is admitted again and resolves its real outcome.
    let second = fixture.serve_next_with(Arc::clone(&panes));
    let returned = admit(&fixture, &credential, &paired, &agent(0));
    assert!(resolve(&returned));
    assert_eq!(
        seen.lock().unwrap().events,
        vec![
            Event::Released(oldest),
            Event::Released(agent(1).client(&paired.client)),
        ]
    );
    drop((link, returned));
    for server in [first, second] {
        let _ = server.join().unwrap();
    }
}

#[test]
fn a_returning_agent_waits_for_its_eviction_release_so_its_new_grant_survives() {
    let (fixture, credential, paired, seen, panes) = full_pairing();
    let oldest = agent(0).client(&paired.client);
    let newcomer: crate::protocol::AgentName = "newcomer".parse().unwrap();
    // Holding the pane service delays the eviction's release past the direct-network lock.
    let held = panes.lock().unwrap();
    let first = fixture.serve_next_with(Arc::clone(&panes));
    let link = admit(&fixture, &credential, &paired, &newcomer);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !fixture.state.lock().unwrap().retiring.contains(&oldest) {
        assert!(std::time::Instant::now() < deadline, "no eviction");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // The evicted agent returns while its release is still pending. It is not registered, and
    // so cannot claim, until the release ends.
    let second = fixture.serve_next_with(Arc::clone(&panes));
    let returned = admit(&fixture, &credential, &paired, &agent(0));
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!fixture.state.lock().unwrap().connections.serves(oldest));
    drop(held);
    claim(&returned);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.events[0], Event::Released(oldest));
        assert!(seen.events.contains(&Event::Claimed(oldest)));
        assert!(seen.grants.contains(&oldest), "the new grant survives");
        assert!(!seen.events.contains(&Event::Revoked(oldest)));
    }
    drop((link, returned));
    for server in [first, second] {
        let _ = server.join().unwrap();
    }
}
