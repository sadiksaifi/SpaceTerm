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
