use super::*;
use crate::direct::tests::Fixture;

#[test]
fn metadata_write_failure_preserves_the_live_connection_and_its_exact_authority() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let outbox = crate::connection::outbox::Outbox::new(Arc::new(|| {}));
    let mut state = fixture.state.lock().unwrap();
    let generation = state.generation;
    state
        .register(
            &paired,
            generation,
            [5; 32],
            outbox.clone(),
            Box::new(crate::direct::tests::Quiet),
        )
        .unwrap();
    let record = fixture.paths.data.join("direct.json");
    let original = fixture.paths.data.join("retained-direct.json");
    std::fs::rename(&record, &original).unwrap();
    // Renaming a regular file onto this directory fails at the real storage boundary.
    std::fs::create_dir(&record).unwrap();
    assert_eq!(state.note_connection(&paired), Ok(()));
    assert!(!state.uncertain);
    assert_eq!(state.generation, generation);
    assert_eq!(state.authorized(&paired), Ok(()));
    assert_eq!(state.connections.len(), 1);
    assert!(!outbox.is_closing());
    assert!(
        state.record.devices[0]
            .info
            .last_connected_at_unix_seconds
            .is_some()
    );
    std::fs::remove_dir(&record).unwrap();
    std::fs::rename(&original, &record).unwrap();
}

#[test]
fn retained_connection_times_outside_the_supported_unix_range_are_refused() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.enroll(&DeviceCredential::generate().unwrap());
    fixture.state.lock().unwrap().stop();
    let record = fixture.paths.data.join("direct.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    document["devices"][0]["info"]["last_connected_at_unix_seconds"] = serde_json::json!(u64::MAX);
    std::fs::write(&record, serde_json::to_vec(&document).unwrap()).unwrap();
    assert!(matches!(
        State::open(&fixture.paths),
        Err(DirectError::StorageUnavailable)
    ));
}
