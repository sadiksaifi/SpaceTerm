use spaceterm_core::{authority::*, control::*, control_policy::*, identity::*, organization::*};

fn record() -> PaneRuntimeRecord {
    PaneRuntimeRecord {
        pane: PaneId::new(ServerId::new([1; 16]), 1).unwrap(),
        session_generation: SessionGeneration::new(4),
        phase: SessionPhase::Ended {
            reason: EndReason::ServerRestart,
        },
        control_generation: ControlGeneration::new(7),
        control: ControlRecord::Unclaimed,
    }
}
fn client(n: u64) -> ClientId {
    ClientId::new(record().pane.server(), n).unwrap()
}
fn acquire(current: PaneRuntimeRecord, n: u64) -> CompareAcquireRequest {
    CompareAcquireRequest {
        epoch: ServerEpoch::new(2),
        pane: current.pane,
        client: client(n),
        expected_generation: current.control_generation,
    }
}

#[test]
fn compare_acquire_has_one_winner_and_retains_session() {
    let original = record();
    let first = compare_acquire(
        ServerEpoch::new(2),
        client(1),
        original,
        acquire(original, 1),
    )
    .unwrap();
    assert_eq!(first.granted().unwrap().epoch(), ServerEpoch::new(2));
    assert_eq!(first.next().session_generation, original.session_generation);
    assert_eq!(first.next().phase, original.phase);
    assert_eq!(first.next().control_generation, ControlGeneration::new(8));
    assert_eq!(
        compare_acquire(
            ServerEpoch::new(2),
            client(2),
            first.next(),
            acquire(original, 2)
        ),
        Err(ControlPolicyError::StaleComparison)
    );
}

#[test]
fn takeover_replaces_disconnected_owner_and_exact_release_cannot_release_it() {
    let a = compare_acquire(
        ServerEpoch::new(2),
        client(1),
        record(),
        acquire(record(), 1),
    )
    .unwrap();
    let old = a.granted().unwrap();
    let b = take_over(
        ServerEpoch::new(2),
        client(2),
        a.next(),
        TakeOverRequest {
            epoch: ServerEpoch::new(2),
            pane: record().pane,
            client: client(2),
        },
    )
    .unwrap();
    assert_eq!(b.next().control_generation, ControlGeneration::new(9));
    assert_eq!(
        release_exact(
            ServerEpoch::new(2),
            client(1),
            b.next(),
            ReleaseControlRequest {
                epoch: old.epoch(),
                grant: old.grant()
            }
        ),
        Err(ControlPolicyError::StaleComparison)
    );
    let released = release_exact(
        ServerEpoch::new(2),
        client(2),
        b.next(),
        ReleaseControlRequest {
            epoch: ServerEpoch::new(2),
            grant: b.granted().unwrap().grant(),
        },
    )
    .unwrap();
    assert_eq!(released.next().control, ControlRecord::Unclaimed);
    assert_eq!(
        released.next().control_generation,
        ControlGeneration::new(10)
    );
}

#[test]
fn old_epoch_foreign_scope_and_claimed_client_are_rejected() {
    let mut request = acquire(record(), 1);
    request.epoch = ServerEpoch::new(1);
    assert_eq!(
        compare_acquire(ServerEpoch::new(2), client(1), record(), request),
        Err(ControlPolicyError::StaleEpoch)
    );
    request = acquire(record(), 2);
    assert_eq!(
        compare_acquire(ServerEpoch::new(2), client(1), record(), request),
        Err(ControlPolicyError::ScopeMismatch)
    );
    request = acquire(record(), 1);
    request.pane = PaneId::new(ServerId::new([2; 16]), 1).unwrap();
    assert_eq!(
        compare_acquire(ServerEpoch::new(2), client(1), record(), request),
        Err(ControlPolicyError::ScopeMismatch)
    );
}

#[test]
fn control_counter_exhaustion_never_wraps() {
    let mut current = record();
    current.control_generation = ControlGeneration::new(u64::MAX);
    assert_eq!(
        compare_acquire(ServerEpoch::new(2), client(1), current, acquire(current, 1)),
        Err(ControlPolicyError::Exhausted)
    );
}
