use super::*;
use crate::direct::tests::Fixture;

#[test]
fn metadata_write_failure_preserves_the_live_connection_and_its_exact_authority() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let outbox =
        crate::connection::outbox::Outbox::new(Arc::new(|| {}), crate::protocol::version::WORK);
    let mut state = fixture.state.lock().unwrap();
    let generation = state.generation;
    state
        .register(
            &paired,
            generation,
            [5; 32],
            paired.client,
            None,
            Some(outbox.clone()),
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

#[test]
fn a_work_major_strands_the_devices_whose_last_hello_offered_none_of_it() {
    use crate::protocol::version::ProtocolVersion;
    let v = |major| ProtocolVersion { major, minor: 0 };
    let fixture = Fixture::new();
    fixture.enable();
    let current = fixture.enroll(&DeviceCredential::generate().unwrap());
    let behind = fixture.enroll(&DeviceCredential::generate().unwrap());
    // Admitted under no record, so the Core cannot know which majors it speaks.
    fixture.enroll(&DeviceCredential::generate().unwrap());
    {
        let mut state = fixture.state.lock().unwrap();
        state.note_offered(&current, &[v(3), v(2)]);
        state.note_offered(&behind, &[v(1)]);
        // The last admitted Hello replaces the one before it.
        state.note_offered(&behind, &[v(2), v(1)]);
        let pairings = |major| {
            state
                .stranded(major)
                .into_iter()
                .map(|device| device.pairing)
                .collect::<Vec<_>>()
        };
        assert_eq!(pairings(3), vec![behind.pairing]);
        assert_eq!(pairings(2), Vec::<[u8; 32]>::new());
        assert_eq!(pairings(1), vec![current.pairing]);
        state.stop();
    }
    let reopened = State::open(&fixture.paths).unwrap();
    let pairings: Vec<_> = reopened
        .lock()
        .unwrap()
        .stranded(3)
        .into_iter()
        .map(|device| device.pairing)
        .collect();
    assert_eq!(
        pairings,
        vec![behind.pairing],
        "the record survives a restart"
    );
}
