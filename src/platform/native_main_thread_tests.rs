use super::{macos_appearance, macos_application_quit, macos_pasteboard, macos_services, macos_window_backdrop};

pub(crate) fn run() {
    use objc2::rc::autoreleasepool;

    assert!(objc2::MainThreadMarker::new().is_some());

    fn run_gpui(name: &'static str, test: fn(&mut gpui::TestAppContext)) {
        let dispatcher = gpui::TestDispatcher::new(0);
        let mut cx = gpui::TestAppContext::build(dispatcher.clone(), Some(name));
        autoreleasepool(|_| test(&mut cx));
        cx.run_until_parked();
        cx.update(|cx| {
            cx.background_executor().forbid_parking();
            cx.quit();
        });
        cx.run_until_parked();
        drop(cx);
        dispatcher.drain_tasks();
        println!("{name} ... ok");
    }

    macro_rules! gpui_test {
        ($path:path) => {
            run_gpui(stringify!($path), $path);
        };
    }
    macro_rules! native_test {
        ($path:path) => {
            autoreleasepool(|_| $path());
            println!("{} ... ok", stringify!($path));
        };
    }

    gpui_test!(
        macos_appearance::tests::forcing_native_chrome_does_not_change_the_system_preference
    );
    gpui_test!(
        macos_appearance::tests::native_observer_coalesces_wakeups_and_closes_with_its_owner
    );
    gpui_test!(macos_appearance::tests::native_observer_coalesces_show_borders_notifications_and_removes_registration);
    gpui_test!(
        macos_window_backdrop::tests::backdrop_installation_is_ordered_idempotent_and_reversible
    );
    gpui_test!(
        macos_window_backdrop::tests::backdrop_tracks_content_bounds_through_appkit_autoresizing
    );
    gpui_test!(macos_window_backdrop::tests::changing_tone_replaces_the_material_in_place);
    gpui_test!(
        macos_application_quit::tests::native_hook_cancels_policy_then_consumes_one_confirmation
    );
    gpui_test!(macos_services::tests::service_type_classifies_nil_and_empty_nsstring_as_absent);
    gpui_test!(macos_services::tests::nsstring_decode_enforces_the_paste_limit_before_copying);
    gpui_test!(macos_services::tests::nsstring_decode_rejects_embedded_nul_without_truncation);
    gpui_test!(macos_services::tests::nsstring_decode_rejects_nonempty_failed_utf8_conversion);
    gpui_test!(macos_services::tests::service_pasteboard_round_trip_uses_only_public_utf8_text);
    gpui_test!(
        macos_services::tests::native_selectors_publish_selection_and_accept_exactly_one_return
    );
    gpui_test!(macos_services::tests::native_selectors_reject_stale_validation_and_stale_return);
    gpui_test!(
        macos_services::tests::native_requestors_keep_overlapping_window_equivalent_owners_isolated
    );
    gpui_test!(
        macos_services::tests::native_validation_survives_requestor_deallocation_inside_status
    );
    gpui_test!(macos_services::tests::native_selection_survives_operation_and_owner_deallocation_without_publishing);
    gpui_test!(macos_services::tests::native_return_survives_operation_and_owner_deallocation_inside_status);
    gpui_test!(macos_services::tests::native_operation_deallocation_inside_insertion_releases_state_and_gate_after_callback);
    gpui_test!(
        macos_services::tests::native_modern_validation_accepts_legacy_only_write_types_once
    );
    native_test!(macos_pasteboard::tests::oversized_file_url_is_rejected_before_conversion);
    native_test!(macos_pasteboard::tests::native_file_discovery_counts_only_file_representations);
    native_test!(macos_pasteboard::tests::native_file_discovery_rejects_unreadable_file_representation_with_text);
    native_test!(macos_pasteboard::tests::native_file_discovery_preserves_items_and_rejects_invalid_authority);
    native_test!(
        macos_pasteboard::tests::native_write_declares_every_representation_before_publishing_data
    );
    println!("25 native main-thread tests passed");
}
