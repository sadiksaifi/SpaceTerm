use super::{
    macos_appearance, macos_application_quit, macos_pasteboard, macos_services,
    macos_window_backdrop,
};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> *mut std::ffi::c_void;
}

/// Why AppKit cannot make a window key in this process, which focus fixtures require.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionUnavailable {
    NoGraphicalSession,
    ScreenLocked,
}

impl SessionUnavailable {
    const fn explanation(self) -> &'static str {
        match self {
            Self::NoGraphicalSession => {
                "this process has no graphical login session. Run them from a user logged in at the Mac."
            }
            Self::ScreenLocked => "the screen is locked. Unlock the Mac, then run them again.",
        }
    }
}

fn session_unavailable() -> Option<SessionUnavailable> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSDictionary, NSNumber, NSString};

    // SAFETY: The call takes no arguments and returns a +1 CFDictionary, or null outside a
    // graphical login session. CFDictionary is toll-free bridged to NSDictionary, so `Retained`
    // takes that reference.
    let session = unsafe {
        Retained::from_raw(
            CGSessionCopyCurrentDictionary().cast::<NSDictionary<NSString, AnyObject>>(),
        )
    };
    let Some(session) = session else {
        return Some(SessionUnavailable::NoGraphicalSession);
    };
    // The key is present only while the screen is locked. A session switched away from the console
    // keeps drawing, so console ownership alone does not decide whether a window can become key.
    session
        .objectForKey(&NSString::from_str("CGSSessionScreenIsLocked"))
        .and_then(|value| value.downcast::<NSNumber>().ok())
        .is_some_and(|locked| locked.boolValue())
        .then_some(SessionUnavailable::ScreenLocked)
}

pub(crate) fn run() {
    use objc2::rc::autoreleasepool;

    assert!(objc2::MainThreadMarker::new().is_some());
    // Some fixtures make a window key, which AppKit refuses while the screen is locked.
    // Stop before any fixture runs so the failure names the session instead of a focus assertion.
    if let Some(reason) = session_unavailable() {
        eprintln!(
            "native macOS tests need an unlocked graphical login session: {}",
            reason.explanation()
        );
        std::process::exit(1);
    }

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

    native_test!(super::macos_keyboard_layout::tests::native_layouts_resolve_dispatch_and_system_reservations);

    native_test!(super::macos_setup_guide_host::tests::guide_mouse_click_preserves_keyboard_focus);

    native_test!(super::macos_accessibility::tests::bundled_font_metadata_does_not_require_system_installation);
    gpui_test!(
        super::macos_accessibility::tests::presented_pane_attaches_its_text_area_to_the_pane_node
    );
    gpui_test!(super::macos_accessibility::tests::text_area_focus_request_focuses_its_pane);

    gpui_test!(
        macos_appearance::tests::forcing_native_chrome_does_not_change_the_system_preference
    );
    gpui_test!(
        macos_appearance::tests::native_observer_coalesces_wakeups_and_closes_with_its_owner
    );
    gpui_test!(macos_appearance::tests::native_observer_coalesces_show_borders_notifications_and_closes_subscription);
    gpui_test!(
        macos_window_backdrop::tests::backdrop_installation_is_ordered_idempotent_and_reversible
    );
    gpui_test!(
        macos_window_backdrop::tests::backdrop_tracks_content_bounds_through_appkit_autoresizing
    );
    gpui_test!(macos_window_backdrop::tests::changing_tone_replaces_the_material_in_place);
    gpui_test!(
        macos_window_backdrop::tests::background_image_lies_beneath_a_material_that_blends_within_the_window
    );
    gpui_test!(macos_window_backdrop::tests::the_background_image_survives_redraws_and_resizing);
    gpui_test!(
        macos_window_backdrop::tests::the_decoded_background_image_goes_with_its_last_window
    );
    gpui_test!(
        macos_application_quit::tests::native_hook_cancels_policy_then_consumes_one_confirmation
    );
    native_test!(
        macos_application_quit::tests::dismissal_alert_focuses_the_default_and_keys_never_quit_completely
    );
    gpui_test!(macos_services::tests::service_type_classifies_nil_and_empty_nsstring_as_absent);
    gpui_test!(macos_services::tests::nsstring_decode_enforces_the_paste_byte_limit);
    gpui_test!(macos_services::tests::nsstring_decode_rejects_embedded_nul_without_truncation);
    gpui_test!(macos_services::tests::nsstring_decode_rejects_nonempty_failed_utf8_conversion);
    gpui_test!(macos_services::tests::service_pasteboard_round_trips_public_utf8_text);
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
    native_test!(macos_pasteboard::tests::native_file_reference_url_inserts_resolved_path);
    native_test!(macos_pasteboard::tests::native_deleted_file_reference_url_returns_typed_failure);
    native_test!(macos_pasteboard::tests::native_file_reference_and_path_urls_preserve_order);
    native_test!(
        macos_pasteboard::tests::native_write_declares_every_representation_before_publishing_data
    );
    native_test!(macos_pasteboard::tests::native_text_clipboard_ignores_image_representations);
    native_test!(
        macos_pasteboard::tests::native_text_clipboard_preserves_utf8_and_rejects_oversized_text
    );
    println!("34 native main-thread tests passed");
}
