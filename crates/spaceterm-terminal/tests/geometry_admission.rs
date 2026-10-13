#![cfg(feature = "runtime")]
mod support;
use spaceterm_core::{
    geometry::{CellPixels, MeasuredGrid, SUPPORTED_GRID_LIMITS},
    organization::{FailureClass, RetirementReason},
};
use spaceterm_terminal::runtime::{budget::RuntimeLimits, ports::*};
use spaceterm_terminal::runtime_contracts::{EffectDisposition, RuntimeCommand};
use support::*;

#[test]
fn startup_uses_exact_measured_dimensions_without_provisional_shell() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(37, 113));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    assert_eq!(
        pty.inspect(|state| state.sizes.clone()),
        vec![PtySize {
            rows: 37,
            columns: 113,
            pixel_width: 904,
            pixel_height: 592
        }]
    );
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn invalid_or_unreserved_start_creates_no_pty() {
    for request in [request(1, grid(513, 80)), request(1, grid(24, 1025))] {
        let factory = factory(RuntimeLimits::default());
        let pty = Pty::default();
        assert_eq!(
            factory
                .start(request, pty.launch(), engine())
                .err()
                .unwrap()
                .class,
            FailureClass::ResourceLimit
        );
        assert_eq!(pty.inspect(|state| state.launch_count), 0);
    }
    let factory = factory(RuntimeLimits {
        active_grid_slots: 4 * 24 * 80 - 1,
        ..RuntimeLimits::default()
    });
    let pty = Pty::default();
    assert!(
        factory
            .start(request(1, grid(24, 80)), pty.launch(), engine())
            .is_err()
    );
    assert_eq!(pty.inspect(|state| state.launch_count), 0);
}
#[test]
fn duplicate_start_for_same_pane_creates_one_runtime() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    assert!(factory.start(request, pty.launch(), engine()).is_err());
    assert_eq!(pty.inspect(|state| state.launch_count), 1);
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn resize_limit_plus_one_leaves_existing_grid_and_pty_unchanged() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    assert_eq!(
        owner
            .handle()
            .try_submit(
                fence(request.authority()),
                RuntimeCommand::Resize(grid(513, 80))
            )
            .unwrap()
            .receive_blocking()
            .unwrap_err()
            .class,
        FailureClass::ResourceLimit
    );
    assert_eq!(pty.inspect(|state| state.sizes.len()), 1);
    assert_eq!(
        owner.lifecycle_snapshot(),
        spaceterm_terminal::runtime_contracts::RuntimeLifecycle::Running
    );
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn aggregate_grid_reservation_accounts_for_alternate_and_resize_scratch() {
    // 400 old steady slots + 600 temporary slots, not merely 480 new steady slots.
    let factory = factory(RuntimeLimits {
        active_grid_slots: 1079,
        ..RuntimeLimits::default()
    });
    let request = request(1, grid(10, 10));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    assert_eq!(
        owner
            .handle()
            .try_submit(
                fence(request.authority()),
                RuntimeCommand::Resize(grid(10, 12))
            )
            .unwrap()
            .receive_blocking()
            .unwrap_err()
            .class,
        FailureClass::ResourceLimit
    );
    assert_eq!(pty.inspect(|state| state.sizes.len()), 1);
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    let exact = support::factory(RuntimeLimits {
        active_grid_slots: 1080,
        ..RuntimeLimits::default()
    });
    let pty = Pty::default();
    let mut owner = running(&exact, request.clone(), &pty);
    assert_eq!(
        owner
            .handle()
            .try_submit(
                fence(request.authority()),
                RuntimeCommand::Resize(grid(10, 12))
            )
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::Applied
    );
    // The temporary and old claims are released, leaving exactly 480 + 600 slots.
    let other = Pty::default();
    let mut second = running(&exact, support::request(2, grid(10, 15)), &other);
    second.retire(RetirementReason::Stop);
    second.join().unwrap();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn pixel_only_resize_does_not_require_another_grid_reservation() {
    let factory = factory(RuntimeLimits {
        active_grid_slots: 400,
        ..RuntimeLimits::default()
    });
    let request = request(1, grid(10, 10));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let pixels = MeasuredGrid::new(
        10,
        10,
        CellPixels::new(9, 17).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    owner
        .handle()
        .try_submit(fence(request.authority()), RuntimeCommand::Resize(pixels))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(
        pty.inspect(|state| state.sizes.last().copied()),
        Some(PtySize {
            rows: 10,
            columns: 10,
            pixel_width: 90,
            pixel_height: 170
        })
    );
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn cell_pixel_metrics_resize_the_pty_without_reserving_another_grid() {
    let factory = factory(RuntimeLimits {
        active_grid_slots: 4 * 10 * 10,
        ..RuntimeLimits::default()
    });
    let request = request(1, grid(10, 10));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let result = owner
        .handle()
        .try_submit(
            fence(request.authority()),
            RuntimeCommand::CellPixelMetrics(CellPixels::new(9, 17).unwrap()),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert_eq!(result, EffectDisposition::Applied);
    assert_eq!(
        pty.inspect(|state| state.sizes.clone()),
        vec![
            PtySize {
                rows: 10,
                columns: 10,
                pixel_width: 80,
                pixel_height: 160
            },
            PtySize {
                rows: 10,
                columns: 10,
                pixel_width: 90,
                pixel_height: 170
            },
        ]
    );
}

#[test]
fn unrepresentable_pty_pixels_are_zero_not_wrapped() {
    let factory = factory(RuntimeLimits::default());
    let measured = MeasuredGrid::new(
        24,
        1024,
        CellPixels::new(64, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let request = request(1, measured);
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    assert_eq!(pty.inspect(|state| state.sizes[0].pixel_width), 0);
    assert_eq!(pty.inspect(|state| state.sizes[0].pixel_height), 384);
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn resize_failure_retires_runtime_and_preserves_failure_classification() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    pty.update(|state| state.resize_failure = true);
    assert_eq!(
        owner
            .handle()
            .try_submit(
                fence(request.authority()),
                RuntimeCommand::Resize(grid(25, 80))
            )
            .unwrap()
            .receive_blocking()
            .unwrap_err()
            .stage,
        RuntimeStage::Resize
    );
    assert_eq!(
        owner.join().unwrap().first_reason(),
        RetirementReason::RuntimeFailure
    );
    assert_eq!(pty.inspect(|state| state.sizes.len()), 1);
}

#[test]
fn maximum_runtime_grid_is_admitted_with_four_plane_capacity() {
    let factory = factory(RuntimeLimits {
        active_grid_slots: 4 * 512 * 1024,
        ..RuntimeLimits::default()
    });
    let request = request(1, grid(512, 1024));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    assert_eq!(
        pty.inspect(|state| state.sizes[0]),
        PtySize {
            rows: 512,
            columns: 1024,
            pixel_width: 8192,
            pixel_height: 8192
        }
    );
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
