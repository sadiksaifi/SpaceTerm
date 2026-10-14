use super::*;
use spaceterm_client::management_cli::confirmation::{
    AssessmentEvidence, AssessmentToken, ManagementAction, ManagementAssessment,
};
use spaceterm_core::{
    authority::FieldVersion,
    identity::{ClientId, DeviceId, PairingId},
    observations::MonotonicTick,
    operations::{AssessmentRef, RequestDigest},
    pairing::{
        ClientRecord, PairingTarget, PairingTargetParts, UnpairAssessment, UnpairAssessmentParts,
    },
};

fn device(server: &ServerSummary, sequence: u64, name: &str) -> model::PairedDevice {
    let server_id = server.pairing.parts().server;
    model::PairedDevice {
        target: PairingTarget::new(PairingTargetParts {
            server: server_id,
            pairing: PairingId::new(server_id, sequence).unwrap(),
            device: DeviceId::new([sequence as u8; 16]),
        })
        .unwrap(),
        name: name.to_owned().into(),
    }
}

fn with_devices(mut server: ServerSummary) -> ServerSummary {
    server.paired_devices = Some(model::PairedDevices {
        version: FieldVersion::new(3),
        this_device: DeviceId::new([1; 16]),
        total: 2,
        devices: vec![
            device(&server, 1, "Laptop"),
            device(&server, 2, "Build device"),
        ],
        next: None,
    });
    server
}

fn assessment(server: &ServerSummary, target: PairingTarget, token: &str) -> ManagementAssessment {
    let parts = target.parts();
    let evidence = UnpairAssessment::new(UnpairAssessmentParts {
        reference: AssessmentRef {
            operation: operation(parts.server.bytes()[0]),
            digest: RequestDigest::from_bytes([2; 32]),
        },
        target: target.clone(),
        authorization_version: FieldVersion::new(1),
        clients: vec![ClientRecord {
            id: ClientId::new(server.pairing.parts().server, 2).unwrap(),
            pairing: parts.pairing,
        }],
    })
    .unwrap();
    ManagementAssessment::new(
        AssessmentToken::new(token.to_owned()).unwrap(),
        ManagementAction::Unpair { target },
        AssessmentEvidence::Unpair(evidence),
        MonotonicTick::new(1),
        MonotonicTick::new(30_001),
    )
    .unwrap()
}

fn request_review(
    fixture: &mut Fixture<'_>,
    server: &ServerSummary,
    index: usize,
) -> PairingTarget {
    let page = server.paired_devices.as_ref().unwrap();
    let target = page.devices[index].target.clone();
    fixture.click(&format!(
        "settings-paired-device-{}-unpair",
        target.parts().pairing.local()
    ));
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::AssessUnpairDevice {
            server: server.key.clone(),
            target: target.clone(),
            version: page.version,
        }]
    );
    assert!(!fixture.modal_open());
    target
}

fn inventory_is_accessible(appearance: Appearance, cx: &mut TestAppContext) {
    let mut fixture = open(appearance, snapshot(vec![with_devices(local())]), cx);
    fixture.search("paired devices");
    let tree = A11yTree::read(fixture.cx);
    named(&tree, "Group", "Paired devices on Studio");
    let this = named(&tree, "Group", "Laptop (this device)");
    assert_eq!(this["aria"]["description"], "Authorized to use Studio");
    let other = named(&tree, "Button", "Unpair Device…");
    assert!(spaceterm_ui::a11y_testing::supports(
        other,
        gpui::accesskit::Action::Click
    ));
    assert!(tree.find_text("Showing 2 of 2 paired devices").is_some());
    assert!(
        tree.find_text("This device ID: 01010101010101010101010101010101")
            .is_some()
    );
}

#[gpui::test]
fn paired_devices_are_named_and_actionable_in_dark_settings(cx: &mut TestAppContext) {
    inventory_is_accessible(Appearance::Dark, cx);
}

#[gpui::test]
fn paired_devices_are_named_and_actionable_in_light_settings(cx: &mut TestAppContext) {
    inventory_is_accessible(Appearance::Light, cx);
}

#[gpui::test]
fn device_unpair_waits_for_a_fresh_assessment_and_confirms_the_exact_server_and_pairing(
    cx: &mut TestAppContext,
) {
    let mut server = with_devices(local());
    let mut fixture = open(Appearance::Dark, snapshot(vec![server.clone(), ssh()]), cx);
    fixture.search("paired devices");
    let target = request_review(&mut fixture, &server, 1);
    let assessed = assessment(&server, target.clone(), "fresh-review");
    server.unpair_assessment = Some(assessed.clone());
    fixture.publish(snapshot(vec![server.clone(), ssh()]));
    assert!(fixture.modal_open());
    let tree = A11yTree::read(fixture.cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Cancel");
    assert!(tree.find_text("Unpair Build device from Studio?").is_some());
    // A selection change must not retarget an already displayed confirmation.
    let settings = fixture.window.clone();
    fixture.cx.update(|_, cx| {
        settings.update(cx, |settings, cx| {
            settings.select_server(ServerKey::new("build"), cx)
        })
    });
    fixture.click("modal-action-settings-paired-device-unpair-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::UnpairDevice {
            server: ServerKey::new("local"),
            target,
            assessment: Box::new(assessed),
        }]
    );
}

#[gpui::test]
fn keyboard_and_assistive_technology_review_a_device_and_return_cancels(cx: &mut TestAppContext) {
    let mut server = with_devices(local());
    let target = server.paired_devices.as_ref().unwrap().devices[1]
        .target
        .clone();
    let mut fixture = open(Appearance::Light, snapshot(vec![server.clone()]), cx);
    fixture.search("paired devices");
    let tree = A11yTree::read(fixture.cx);
    perform(
        fixture.cx,
        named(&tree, "Button", "Unpair Device…"),
        gpui::accesskit::Action::Focus,
    );
    fixture.cx.simulate_keystrokes("tab");
    fixture.cx.simulate_keystrokes("shift-tab");
    let tree = A11yTree::read(fixture.cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Unpair Device…");
    fixture.press_return();
    assert!(
        matches!(fixture.requests.take().as_slice(), [ServerRequest::AssessUnpairDevice { target: requested, .. }] if requested == &target)
    );
    server.unpair_assessment = Some(assessment(&server, target, "keyboard-review"));
    fixture.publish(snapshot(vec![server]));
    assert!(fixture.modal_open());
    fixture.press_return();
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());
    let tree = A11yTree::read(fixture.cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Unpair Device…");
}

#[gpui::test]
fn a_revoked_or_reenrolled_device_cannot_be_unpaired_by_an_old_confirmation(
    cx: &mut TestAppContext,
) {
    let mut server = with_devices(local());
    let mut fixture = open(Appearance::Dark, snapshot(vec![server.clone()]), cx);
    fixture.search("paired devices");
    let target = request_review(&mut fixture, &server, 1);
    server.unpair_assessment = Some(assessment(&server, target, "old-review"));
    fixture.publish(snapshot(vec![server.clone()]));
    assert!(fixture.modal_open());
    let replacement = device(&server, 3, "Build device");
    let page = server.paired_devices.as_mut().unwrap();
    page.devices[1] = replacement;
    page.version = FieldVersion::new(4);
    fixture.publish(snapshot(vec![server]));
    fixture.click("modal-action-settings-paired-device-unpair-confirm");
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn this_and_last_device_removal_explain_local_detachment_without_stopping_sessions(
    cx: &mut TestAppContext,
) {
    let mut server = with_devices(local());
    let page = server.paired_devices.as_mut().unwrap();
    page.devices.truncate(1);
    page.total = 1;
    let mut fixture = open(Appearance::Dark, snapshot(vec![server.clone()]), cx);
    fixture.search("paired devices");
    let target = request_review(&mut fixture, &server, 0);
    server.unpair_assessment = Some(assessment(&server, target.clone(), "last-device"));
    fixture.publish(snapshot(vec![server.clone()]));
    let tree = A11yTree::read(fixture.cx);
    assert!(tree.find_text("Unpair this device from Studio?").is_some());
    assert!(tree.find_text("This device stops using Studio now. Studio's Workspaces leave this device's windows, and its saved connection is removed. Studio and its Terminal Sessions keep running. This is the server's last paired device. Pair a device again before using the server.").is_some());
    fixture.click("modal-action-settings-paired-device-unpair-confirm");
    assert!(
        matches!(fixture.requests.take().as_slice(), [ServerRequest::UnpairDevice { target: actual, .. }] if actual == &target)
    );
}

#[gpui::test]
fn device_pages_use_the_exact_cursor_and_revision_and_replace_instead_of_accumulating(
    cx: &mut TestAppContext,
) {
    let mut server = with_devices(local());
    let page = server.paired_devices.as_mut().unwrap();
    page.total = 4;
    let after = page.devices.last().unwrap().target.parts().pairing;
    page.next = Some(after);
    let mut fixture = open(Appearance::Light, snapshot(vec![server.clone()]), cx);
    fixture.search("paired devices");
    fixture.click("settings-paired-devices-next");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::ReadPairedDevices {
            server: server.key.clone(),
            after: Some(after),
            version: Some(FieldVersion::new(3))
        }]
    );
    let next = vec![device(&server, 3, "Tablet"), device(&server, 4, "Phone")];
    let page = server.paired_devices.as_mut().unwrap();
    page.devices = next;
    page.next = None;
    fixture.publish(snapshot(vec![server.clone()]));
    let tree = A11yTree::read(fixture.cx);
    assert!(tree.find("Laptop (this device)").is_none());
    named(&tree, "Group", "Tablet");
    named(&tree, "Group", "Phone");
    assert!(!fixture.rendered("settings-paired-devices-next"));
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Refresh Devices");
    fixture.click("settings-paired-devices-refresh");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::ReadPairedDevices {
            server: server.key,
            after: None,
            version: None
        }]
    );
}

#[gpui::test]
fn invalid_or_cross_server_device_pages_expose_no_removal_actions(cx: &mut TestAppContext) {
    let mut server = with_devices(local());
    server.paired_devices.as_mut().unwrap().devices[1] = device(&ssh(), 2, "Wrong server");
    let mut fixture = open(Appearance::Dark, snapshot(vec![server]), cx);
    fixture.search("paired devices");
    let tree = A11yTree::read(fixture.cx);
    assert!(tree.find("Laptop (this device)").is_none());
    assert!(tree.find("Wrong server").is_none());
    fixture.click("settings-paired-devices-refresh");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::ReadPairedDevices {
            server: ServerKey::new("local"),
            after: None,
            version: None
        }]
    );
}

#[gpui::test]
fn empty_and_offline_device_pages_are_distinct_and_do_not_create_a_replacement(
    cx: &mut TestAppContext,
) {
    let mut server = with_devices(local());
    server.paired_devices.as_mut().unwrap().devices.clear();
    server.paired_devices.as_mut().unwrap().total = 0;
    let mut fixture = open(Appearance::Dark, snapshot(vec![server.clone()]), cx);
    fixture.search("paired devices");
    let tree = A11yTree::read(fixture.cx);
    assert!(tree.find_text("No paired devices are listed.").is_some());
    server.status = ServerStatus::Offline;
    server.paired_devices = None;
    fixture.publish(snapshot(vec![server]));
    let tree = A11yTree::read(fixture.cx);
    assert!(tree.find_text("Connect to this server to read its paired devices. You can still unpair this device below.").is_some());
    fixture.click("settings-paired-devices-refresh");
    assert!(fixture.requests.take().is_empty());
    assert_eq!(fixture.document(), SettingsDocument::default());
}

#[gpui::test]
fn long_device_names_remain_bounded_and_actions_fit_a_narrow_settings_window(
    cx: &mut TestAppContext,
) {
    let mut server = with_devices(local());
    server.paired_devices.as_mut().unwrap().devices[1].name = "W".repeat(200).into();
    let mut fixture = open(Appearance::Light, snapshot(vec![server]), cx);
    fixture
        .cx
        .simulate_resize(gpui::size(gpui::px(650.0), gpui::px(550.0)));
    fixture.search("paired devices");
    let tree = A11yTree::read(fixture.cx);
    named(&tree, "Group", &format!("{}…", "W".repeat(63)));
    let content = fixture.cx.debug_bounds("settings-paired-devices").unwrap();
    let action = fixture
        .cx
        .debug_bounds("settings-paired-device-2-unpair")
        .unwrap();
    assert!(action.left() >= content.left());
    assert!(
        action.right() <= content.right(),
        "{action:?} escapes {content:?}"
    );
}

#[gpui::test]
fn cancelling_a_device_review_ignores_its_late_assessment_and_requires_a_fresh_one(
    cx: &mut TestAppContext,
) {
    let mut server = with_devices(local());
    let mut fixture = open(Appearance::Dark, snapshot(vec![server.clone()]), cx);
    fixture.search("paired devices");
    let target = request_review(&mut fixture, &server, 1);
    fixture.click("settings-paired-devices-cancel-review");
    let old = assessment(&server, target.clone(), "cancelled-review");
    server.unpair_assessment = Some(old);
    fixture.publish(snapshot(vec![server.clone()]));
    assert!(!fixture.modal_open());
    request_review(&mut fixture, &server, 1);
    fixture.publish(snapshot(vec![server.clone()]));
    assert!(
        !fixture.modal_open(),
        "an unchanged old assessment is not a new review"
    );
    server.unpair_assessment = Some(assessment(&server, target, "replacement-review"));
    fixture.publish(snapshot(vec![server]));
    assert!(fixture.modal_open());
    fixture.press_return();
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn an_inventory_change_during_review_announces_cancellation_without_showing_a_stale_confirmation(
    cx: &mut TestAppContext,
) {
    let mut server = with_devices(local());
    let mut fixture = open(Appearance::Dark, snapshot(vec![server.clone()]), cx);
    fixture.search("paired devices");
    let target = request_review(&mut fixture, &server, 1);
    server.paired_devices.as_mut().unwrap().version = FieldVersion::new(4);
    server.unpair_assessment = Some(assessment(&server, target, "outdated-inventory"));
    fixture.publish(snapshot(vec![server]));
    assert!(!fixture.modal_open());
    let tree = A11yTree::read(fixture.cx);
    named(
        &tree,
        "Status",
        "The server or device inventory changed. Review the device again before unpairing.",
    );
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn device_action_focus_is_distinct_for_servers_with_the_same_local_pairing_number(
    cx: &mut TestAppContext,
) {
    let original = with_devices(local());
    let mut selected = with_devices(ssh());
    selected.status = ServerStatus::Running;
    let mut fixture = open(
        Appearance::Dark,
        snapshot(vec![original.clone(), selected.clone()]),
        cx,
    );
    fixture.search("paired devices");
    fixture.click("settings-server-option-build");
    let target = request_review(&mut fixture, &selected, 1);
    selected.unpair_assessment = Some(assessment(&selected, target, "second-server-review"));
    fixture.publish(snapshot(vec![original, selected]));
    assert!(fixture.modal_open());
    fixture.press_return();
    let tree = A11yTree::read(fixture.cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Unpair Device…");
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn retained_offline_devices_are_labelled_as_last_read_and_cannot_be_revoked_from_the_cached_page(
    cx: &mut TestAppContext,
) {
    let mut server = with_devices(local());
    server.status = ServerStatus::Offline;
    let mut fixture = open(Appearance::Dark, snapshot(vec![server]), cx);
    fixture.search("paired devices");
    let tree = A11yTree::read(fixture.cx);
    assert!(
        tree.find_text("Last read: 2 of 2 paired devices. Connect to refresh this inventory.")
            .is_some()
    );
    let cached = named(&tree, "Group", "Build device");
    assert_eq!(
        cached["aria"]["description"],
        "Listed in the last read inventory for Studio"
    );
    fixture.click("settings-paired-device-2-unpair");
    assert!(fixture.requests.take().is_empty());
    assert!(!fixture.modal_open());
}
