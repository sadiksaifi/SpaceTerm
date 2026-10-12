use spaceterm_core::authority::*;
use spaceterm_core::geometry::*;
use spaceterm_core::identity::*;

fn server() -> ServerId {
    ServerId::new([7; 16])
}

#[test]
fn allocation_resumes_after_persisted_high_water_without_reusing_deleted_ids() {
    let mut ids = IdAllocator::<PaneId>::from_high_water(server(), 41);
    assert_eq!(ids.allocate().unwrap(), PaneId::new(server(), 42).unwrap());
    assert_eq!(ids.high_water(), 42);
}

#[test]
fn exhausted_allocation_rejects_before_changing_high_water() {
    let mut ids = IdAllocator::<WorkspaceId>::from_high_water(server(), u64::MAX);
    assert_eq!(ids.allocate(), Err(IdentityError::Exhausted));
    assert_eq!(ids.high_water(), u64::MAX);
}

#[test]
fn last_identity_is_allocated_once_without_wrapping() {
    let mut ids = IdAllocator::<TabId>::from_high_water(server(), u64::MAX - 1);
    assert_eq!(ids.allocate().unwrap().local(), u64::MAX);
    assert_eq!(ids.allocate(), Err(IdentityError::Exhausted));
}

#[test]
fn deserialization_cannot_construct_zero_or_cross_server_identities() {
    let id = PaneId::new(server(), 1).unwrap();
    let mut value = serde_json::to_value(id).unwrap();
    value["local"] = 0.into();
    assert!(serde_json::from_value::<PaneId>(value).is_err());
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server(), 1).unwrap(),
        PairingId::new(server(), 1).unwrap(),
        ClientId::new(server(), 1).unwrap(),
    )
    .unwrap();
    let mut value = serde_json::to_value(stream).unwrap();
    value["client"]["server"] = serde_json::to_value(ServerId::new([8; 16])).unwrap();
    assert!(serde_json::from_value::<OperationStreamId>(value).is_err());
}

#[test]
fn every_comparison_stamp_rejects_wrapping_successors() {
    macro_rules! check {
        ($($stamp:ident),+ $(,)?) => { $(
            assert_eq!($stamp::new(u64::MAX).checked_successor(), None);
            assert_eq!($stamp::new(4).checked_successor(), Some($stamp::new(5)));
        )+ };
    }
    check!(
        ServerEpoch,
        SessionGeneration,
        ControlGeneration,
        ConnectionGeneration,
        ContextGeneration,
        ContentRevision,
        OrganizationRevision,
        StoreRevision,
        FieldVersion,
        CreationOrdinal
    );
}

fn fence() -> EffectFence {
    let grant = ControlGrant::new(
        PaneId::new(server(), 1).unwrap(),
        ClientId::new(server(), 2).unwrap(),
        ControlGeneration::new(3),
    )
    .unwrap();
    EffectFence::new(
        SessionKey {
            epoch: ServerEpoch::new(4),
            pane: grant.pane(),
            generation: SessionGeneration::new(5),
        },
        grant,
        ConnectionBinding::new(grant.client(), ConnectionGeneration::new(6)),
        ContextGeneration::new(7),
        Some(ContentRevision::new(8)),
    )
    .unwrap()
}

#[test]
fn fence_rejects_every_replaced_authority_dimension() {
    let original = fence();
    let session = original.session();
    let grant = original.grant();
    let replacements = [
        EffectFence::new(
            SessionKey {
                epoch: ServerEpoch::new(5),
                ..session
            },
            grant,
            original.connection(),
            original.context(),
            original.content(),
        )
        .unwrap(),
        EffectFence::new(
            SessionKey {
                generation: SessionGeneration::new(6),
                ..session
            },
            grant,
            original.connection(),
            original.context(),
            original.content(),
        )
        .unwrap(),
        EffectFence::new(
            session,
            ControlGrant::new(grant.pane(), grant.client(), ControlGeneration::new(4)).unwrap(),
            original.connection(),
            original.context(),
            original.content(),
        )
        .unwrap(),
        EffectFence::new(
            session,
            grant,
            ConnectionBinding::new(grant.client(), ConnectionGeneration::new(7)),
            original.context(),
            original.content(),
        )
        .unwrap(),
        EffectFence::new(
            session,
            grant,
            original.connection(),
            ContextGeneration::new(8),
            original.content(),
        )
        .unwrap(),
        EffectFence::new(
            session,
            grant,
            original.connection(),
            original.context(),
            Some(ContentRevision::new(9)),
        )
        .unwrap(),
    ];
    assert!(
        replacements
            .iter()
            .all(|current| !original.is_current(current))
    );
    assert!(original.matches(&original));
}

#[test]
fn content_independent_input_does_not_stale_on_output_but_content_actions_do() {
    let original = fence();
    let input = EffectFence::new(
        original.session(),
        original.grant(),
        original.connection(),
        original.context(),
        None,
    )
    .unwrap();
    let current = EffectFence::new(
        original.session(),
        original.grant(),
        original.connection(),
        original.context(),
        Some(ContentRevision::new(9)),
    )
    .unwrap();
    assert!(input.is_current(&current));
    assert!(!original.is_current(&current));
}

#[test]
fn mismatched_client_cannot_be_constructed_or_deserialized_into_a_fence() {
    let original = fence();
    let connection = ConnectionBinding::new(
        ClientId::new(server(), 3).unwrap(),
        ConnectionGeneration::new(6),
    );
    assert_eq!(
        EffectFence::new(
            original.session(),
            original.grant(),
            connection,
            original.context(),
            None
        ),
        Err(FenceError::WrongBinding)
    );
    let mut value = serde_json::to_value(original).unwrap();
    value["connection"] = serde_json::to_value(connection).unwrap();
    assert!(serde_json::from_value::<EffectFence>(value).is_err());
}

#[test]
fn grid_rejects_total_cells_before_any_runtime_reservation() {
    let limits = GridLimits::new(512, 512, 16_384).unwrap();
    assert_eq!(
        GridSize::new(256, 256, CellPixels::new(8, 16).unwrap(), limits),
        Err(GridError::TooManyCells)
    );
}

#[test]
fn negotiated_grid_cannot_exceed_supported_pty_limits() {
    assert_eq!(
        GridLimits::new(u32::MAX, 120, 1000),
        Err(GridError::UnsupportedLimits)
    );
}

#[test]
fn supported_geometry_admits_8k_small_cells_and_exact_cell_ceiling() {
    let small = CellPixels::new(4, 8).unwrap();
    assert_eq!(
        GridSize::new(540, 1920, small, SUPPORTED_GRID_LIMITS)
            .unwrap()
            .cells(),
        1_036_800
    );
    assert_eq!(
        GridSize::new(1024, 1024, small, SUPPORTED_GRID_LIMITS)
            .unwrap()
            .cells(),
        1_048_576
    );
    assert_eq!(
        GridSize::new(1024, 1025, small, SUPPORTED_GRID_LIMITS),
        Err(GridError::TooManyCells)
    );
}

#[test]
fn pty_pixel_overflow_and_zero_geometry_are_rejected() {
    assert_eq!(
        GridSize::new(
            1,
            1024,
            CellPixels::new(64, 16).unwrap(),
            SUPPORTED_GRID_LIMITS
        ),
        Err(GridError::Overflow)
    );
    assert_eq!(
        GridSize::new(
            0,
            80,
            CellPixels::new(8, 16).unwrap(),
            SUPPORTED_GRID_LIMITS
        ),
        Err(GridError::ZeroDimension)
    );
}

#[test]
fn decoded_geometry_is_rechecked_against_negotiated_limits() {
    let large = GridSize::new(
        24,
        120,
        CellPixels::new(8, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let decoded: GridSize = serde_json::from_value(serde_json::to_value(large).unwrap()).unwrap();
    assert_eq!(
        decoded.validate(GridLimits::new(24, 80, 1920).unwrap()),
        Err(GridError::TooLarge)
    );
    let mut value = serde_json::to_value(large).unwrap();
    value["cell_pixels"]["width"] = 0.into();
    assert!(serde_json::from_value::<GridSize>(value).is_err());
}
