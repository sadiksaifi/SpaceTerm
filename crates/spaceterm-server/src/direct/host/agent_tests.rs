//! The agent names a pairing admitted stay bounded, and only a name that holds nothing is
//! evicted.
use super::*;
use crate::direct::DeviceCredential;
use crate::direct::tests::{Fixture, Quiet};

fn agent(index: usize) -> crate::protocol::AgentName {
    format!("agent-{index}").parse().unwrap()
}

/// Admits `agent` as a connection would, with every idle name it could evict settled.
fn admit_settled(
    state: &mut State,
    paired: &PairedDevice,
    agent: &crate::protocol::AgentName,
) -> Result<Option<[u8; 32]>, DirectError> {
    let reserved = state.reserve_evictable(paired, agent)?.unwrap_or_default();
    let admitted = state.admit_agent(paired, agent, &reserved.iter().copied().collect());
    state.release_retiring(&reserved);
    admitted
}

#[test]
fn a_new_agent_name_past_the_limit_evicts_the_least_recently_admitted_idle_name() {
    let fixture = Fixture::new();
    fixture.enable();
    let paired = fixture.enroll(&DeviceCredential::generate().unwrap());
    let mut state = fixture.state.lock().unwrap();
    for index in 0..storage::MAX_AGENTS {
        assert_eq!(admit_settled(&mut state, &paired, &agent(index)), Ok(None));
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
    assert_eq!(admit_settled(&mut state, &paired, &agent(1)), Ok(None));
    let evicted = admit_settled(&mut state, &paired, &agent(storage::MAX_AGENTS)).unwrap();
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
        admit_settled(&mut state, &paired, &agent(2)),
        Ok(Some(agent(3).client(&paired.client)))
    );
    assert!(
        state
            .retiring
            .contains_key(&agent(3).client(&paired.client))
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
    fn holds(&self, client: [u8; 32]) -> bool {
        self.0.lock().unwrap().outcomes.contains(&client)
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
/// agent 0, holds a control grant.
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
            assert_eq!(admit_settled(&mut state, &paired, &agent(index)), Ok(None));
        }
    }
    let seen = Arc::new(Mutex::new(Seen::default()));
    let oldest = agent(0).client(&paired.client);
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
fn a_name_that_holds_an_outcome_is_never_evicted_and_a_full_list_of_them_refuses_a_new_name() {
    let (fixture, credential, paired, seen, panes) = full_pairing();
    let oldest = agent(0).client(&paired.client);
    let settled = agent(1).client(&paired.client);
    seen.lock().unwrap().outcomes.insert(oldest);
    let first = fixture.serve_next_with(Arc::clone(&panes));
    let link = admit(&fixture, &credential, &paired, &"newcomer".parse().unwrap());
    // Any reply follows the eviction's release, which runs before serving. The oldest name
    // holds an outcome, so the oldest settled name goes instead.
    assert!(!resolve(&link));
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.events, vec![Event::Released(settled)]);
        assert!(seen.outcomes.contains(&oldest) && seen.grants.contains(&oldest));
    }
    // Agent 0 is still listed, so its pairing's revocation can still free its outcome.
    let second = fixture.serve_next_with(Arc::clone(&panes));
    let kept = admit(&fixture, &credential, &paired, &agent(0));
    assert!(resolve(&kept));
    drop((link, kept));
    for server in [first, second] {
        let _ = server.join().unwrap();
    }
    // Once every listed name holds an outcome, a new name is refused and nothing is evicted.
    let listed = |state: &State| {
        state
            .record
            .devices
            .iter()
            .find(|device| device.info.pairing == paired.pairing)
            .unwrap()
            .agents
            .clone()
    };
    let before = listed(&fixture.state.lock().unwrap());
    assert_eq!(before.len(), storage::MAX_AGENTS);
    seen.lock()
        .unwrap()
        .outcomes
        .extend(before.iter().map(|known| known.client(&paired.client)));
    let refused = fixture.serve_next_with(Arc::clone(&panes));
    let connection = DirectConnection::connect(&fixture.endpoint(), &paired, &credential).unwrap();
    let late: crate::protocol::AgentName = "late".parse().unwrap();
    let _ = connection.admit(crate::protocol::Caller {
        client: &paired.client,
        name: &format!("Test{}", late.name_suffix()).into(),
        agent: Some(&late),
    });
    assert!(matches!(
        refused.join().unwrap(),
        Err(ServerError::Direct(DirectError::Capacity))
    ));
    let state = fixture.state.lock().unwrap();
    assert_eq!(listed(&state), before);
    assert!(state.retiring.is_empty());
    assert_eq!(seen.lock().unwrap().events, vec![Event::Released(settled)]);
}

#[test]
fn a_returning_agent_waits_for_its_eviction_release_so_its_new_grant_survives() {
    let (fixture, credential, paired, seen, panes) = full_pairing();
    let oldest = agent(0).client(&paired.client);
    let newcomer: crate::protocol::AgentName = "newcomer".parse().unwrap();
    // Holding the pane service delays the eviction, which holds every idle name until it ends.
    let held = panes.lock().unwrap();
    let first = fixture.serve_next_with(Arc::clone(&panes));
    let link = admit(&fixture, &credential, &paired, &newcomer);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !fixture.state.lock().unwrap().retiring.contains_key(&oldest) {
        assert!(std::time::Instant::now() < deadline, "no eviction");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // The evicted agent returns while its eviction and release are still pending. It is not
    // registered, and so cannot claim, until the release ends.
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

/// Waits until `done` holds, or fails as `what` after five seconds.
fn within(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Where the paused admissions of a test are.
#[derive(Default)]
struct Paused {
    /// How many admissions of the new name reached their commit.
    settled: usize,
    /// Whether the first of them may commit.
    gate: bool,
    /// Whether the returning agent began to wait for its identity.
    waiting: bool,
}

#[test]
fn an_admission_never_waits_for_its_identity_while_it_holds_names_another_admission_waits_for() {
    let (fixture, credential, paired, seen, panes) = full_pairing();
    let newcomer: crate::protocol::AgentName = "newcomer".parse().unwrap();
    let new = newcomer.client(&paired.client);
    let returning = agent(0).client(&paired.client);
    let paused = Arc::new((Mutex::new(Paused::default()), Condvar::new()));
    let order = Arc::clone(&paused);
    fixture.state.lock().unwrap().pause = Some(Arc::new(move |at, client| {
        let (paused, changed) = &*order;
        let mut paused = paused.lock().unwrap();
        match at {
            Pause::Settled if client == new => {
                paused.settled += 1;
                changed.notify_all();
                if paused.settled == 1 {
                    drop(changed.wait_while(paused, |paused| !paused.gate).unwrap());
                }
            }
            Pause::Waiting if client == returning => {
                paused.waiting = true;
                changed.notify_all();
            }
            _ => {}
        }
    }));
    let reached = |what: &str, done: fn(&Paused) -> bool| {
        let (paused, changed) = &*paused;
        let (paused, timeout) = changed
            .wait_timeout_while(
                paused.lock().unwrap(),
                std::time::Duration::from_secs(5),
                |paused| !done(paused),
            )
            .unwrap();
        drop(paused);
        assert!(!timeout.timed_out(), "{what}");
    };
    // The second admission of the new name reserves every idle name, agent 0 among them, and
    // pauses before its commit.
    let second = fixture.serve_next_with(Arc::clone(&panes));
    let second_link = admit(&fixture, &credential, &paired, &newcomer);
    reached("no first reservation", |paused| paused.settled == 1);
    // The first admission of the new name evicts agent 0 and disconnects. The new name is then
    // listed and idle.
    let first = fixture.serve_next_with(Arc::clone(&panes));
    let first_link = admit(&fixture, &credential, &paired, &newcomer);
    assert!(!resolve(&first_link));
    drop(first_link);
    let _ = first.join().unwrap();
    assert_eq!(
        seen.lock().unwrap().events,
        vec![Event::Released(returning)]
    );
    // Agent 0 returns while the paused admission still holds it. It must not hold the new name
    // while it waits, or the paused admission could never commit.
    let third = fixture.serve_next_with(Arc::clone(&panes));
    let returned = admit(&fixture, &credential, &paired, &agent(0));
    reached("the returning agent never waited", |paused| paused.waiting);
    {
        let (paused, changed) = &*paused;
        paused.lock().unwrap().gate = true;
        changed.notify_all();
    }
    within("the admissions deadlocked", || {
        let state = fixture.state.lock().unwrap();
        state.connections.serves(new) && state.connections.serves(returning)
    });
    // The returning agent evicted the oldest name left that holds nothing, and releases it after
    // it registers.
    within("no second eviction", || {
        seen.lock().unwrap().events.len() == 2
    });
    assert_eq!(
        seen.lock().unwrap().events,
        vec![
            Event::Released(returning),
            Event::Released(agent(1).client(&paired.client))
        ]
    );
    drop((second_link, returned));
    for server in [second, third] {
        let _ = server.join().unwrap();
    }
    assert!(fixture.state.lock().unwrap().retiring.is_empty());
}

#[test]
fn stopping_the_host_refuses_an_admission_that_waits_for_its_identity() {
    let (fixture, credential, paired, _, panes) = full_pairing();
    let held = agent(0).client(&paired.client);
    let (waiting, waited) = std::sync::mpsc::channel();
    {
        let mut state = fixture.state.lock().unwrap();
        state.hold_retiring(&[held]);
        state.pause = Some(Arc::new(move |at, client| {
            if at == Pause::Waiting && client == held {
                let _ = waiting.send(());
            }
        }));
    }
    let server = fixture.serve_next_with(panes);
    let _link = admit(&fixture, &credential, &paired, &agent(0));
    waited
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let stopped = std::time::Instant::now();
    fixture.state.lock().unwrap().stopping();
    // Without the wake, the admission would wait until its deadline and refuse with capacity.
    assert!(matches!(
        server.join().unwrap(),
        Err(ServerError::Direct(DirectError::Disabled))
    ));
    assert!(stopped.elapsed() < super::super::quic::HANDSHAKE_LIMIT);
}
