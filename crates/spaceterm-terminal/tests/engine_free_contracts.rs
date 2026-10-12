use spaceterm_core::geometry::{CellPixels, MeasuredGrid, SUPPORTED_GRID_LIMITS};
use spaceterm_terminal::runtime_contracts::{BoundedInputMethodText, RuntimeCommand};

#[test]
fn measured_resize_and_input_contracts_do_not_require_the_terminal_engine() {
    let grid = MeasuredGrid::new(
        24,
        1024,
        CellPixels::new(64, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let RuntimeCommand::Resize(measured) = RuntimeCommand::Resize(grid) else {
        unreachable!()
    };
    assert_eq!(measured.pty_pixels(), (0, 384));
    assert!(BoundedInputMethodText::new("x".repeat(65537)).is_err());
}

#[test]
fn metadata_snapshot_checks_bounds_without_the_terminal_engine() {
    use spaceterm_core::{authority::*, identity::*, observations::*};
    use spaceterm_terminal::{metadata::*, runtime_contracts::*};
    let session = SessionKey {
        pane: PaneId::new(ServerId::new([7; 16]), 1).unwrap(),
        epoch: ServerEpoch::new(1),
        generation: SessionGeneration::new(1),
    };
    let parts = RuntimeMetadataParts {
        session,
        sequence: RuntimeObservationSequence::new(1).unwrap(),
        observed_at: MonotonicTick::new(2),
        title: TitleMetadata {
            value: std::sync::Arc::from("title-only"),
            provenance: TitleProvenance::TerminalControl,
        },
        origin: RuntimeMetadataOrigin::Local {
            user: None,
            host: None,
        },
        progress: ProgressMetadata::Normal(42),
        freshness: MetadataFreshness::Live,
        prompt_zone: PromptZone::Prompt,
        hidden_input: false,
    };
    let snapshot = RuntimeMetadataSnapshot::new(parts.clone()).unwrap();
    assert_eq!(snapshot.parts().progress, ProgressMetadata::Normal(42));
    assert!(!format!("{snapshot:?}").contains("title-only"));
    let mut invalid = parts.clone();
    invalid.title.value = std::sync::Arc::from("x".repeat(257));
    assert_eq!(
        RuntimeMetadataSnapshot::new(invalid),
        Err(RuntimeMetadataError::InvalidTitle)
    );
    let mut invalid = parts.clone();
    invalid.progress = ProgressMetadata::Paused(101);
    assert_eq!(
        RuntimeMetadataSnapshot::new(invalid),
        Err(RuntimeMetadataError::InvalidProgress)
    );
    let mut invalid = parts;
    invalid.session.generation = SessionGeneration::new(0);
    assert_eq!(
        RuntimeMetadataSnapshot::new(invalid),
        Err(RuntimeMetadataError::InvalidSession)
    );
    assert!(RuntimeMetadataIdentity::new("x".repeat(1025)).is_err());
}
