//! Settings > Advanced > Uninstall SpaceTerm, driven through a scripted removal, and the
//! installed app's own removal on a real file system.
use std::path::Path;
use std::sync::Arc;

use gpui::{Modifiers, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::A11yTree;

use super::testing::{ScriptedRemoval, Step};
use super::*;
use crate::ui::settings_window::machines::OfferedMachine;
use crate::ui::settings_window::machines::testing::{ScriptedCore, connected};
use crate::ui::settings_window::machines::tests::{alert_open, click_modal, open_settings};

const ADVANCED: &str = "settings-navigation-settings-section-advanced";
const SUPPRESSION: &str = "modal-alert-suppression";

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

/// Opens Settings on Advanced with `removal` offered.
fn open<'a>(
    removal: Option<&Arc<ScriptedRemoval>>,
    cx: &'a mut TestAppContext,
) -> &'a mut VisualTestContext {
    let offered = OfferedMachine {
        connection: Ok(ScriptedCore::new(Ok(connected(2)))),
        service: None,
        update: None,
        command_line_tool: None,
        removal: removal.map(|removal| Arc::clone(removal) as Arc<dyn SpaceTermRemoval>),
    };
    let (_, cx) = open_settings(Some(offered), cx);
    click(ADVANCED, cx);
    cx
}

fn shows(text: &str, cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx).find_text(text).is_some()
}

fn sessions(count: u8) -> Observed {
    Observed::new((0..count).map(|index| [index; 32]).collect())
}

#[gpui::test]
fn uninstall_names_the_sessions_that_close_and_keeps_data_unless_asked(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(3);
    let cx = open(Some(&removal), cx);
    assert!(shows(
        "Removes SpaceTerm and SpaceTerm Core from this computer.",
        cx
    ));

    click(UNINSTALL_SELECTOR, cx);
    assert!(shows("Uninstall SpaceTerm?", cx));
    assert!(shows(
        "This will close your 3 running sessions on this computer.",
        cx
    ));
    assert!(
        A11yTree::read(cx)
            .find("Also delete Workspaces, paired devices and settings")
            .is_some(),
        "the alert offers deleting the data too"
    );
    assert_eq!(
        removal.steps(),
        vec![Step::Observe],
        "asking removes nothing"
    );

    click_modal(CONFIRM_SELECTOR, cx);
    assert_eq!(
        removal.steps(),
        vec![
            Step::Observe,
            Step::RemoveCore(sessions(3), Data::Keep),
            Step::RemoveApp,
        ]
    );
    assert!(shows("SpaceTerm Was Uninstalled", cx));
    assert!(shows(
        "SpaceTerm and SpaceTerm Core were removed from this computer. Workspaces, paired \
         devices and settings were kept.",
        cx
    ));

    // SpaceTerm Core is gone, so Quit exits without the quit warning.
    let quits = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = std::rc::Rc::clone(&quits);
    cx.update(|_, cx| {
        cx.on_action(move |_: &crate::app::QuitUninstalledApplication, _| {
            counted.set(counted.get() + 1);
        });
    });
    click_modal(QUIT_SELECTOR, cx);
    assert_eq!(quits.get(), 1);
}

#[gpui::test]
fn opting_in_also_deletes_workspaces_paired_devices_and_settings(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(1);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    assert!(shows(
        "This will close your 1 running session on this computer.",
        cx
    ));
    click(SUPPRESSION, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert_eq!(
        removal.steps(),
        vec![
            Step::Observe,
            Step::RemoveCore(sessions(1), Data::Delete),
            Step::RemoveApp,
            Step::DeleteDesktopData,
        ]
    );
    assert!(shows(
        "SpaceTerm and SpaceTerm Core were removed from this computer. Workspaces, paired \
         devices and settings were deleted.",
        cx
    ));
}

#[gpui::test]
fn cancel_removes_nothing(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(0);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    assert!(shows(
        "This will remove SpaceTerm and SpaceTerm Core from this computer.",
        cx
    ));
    click_modal(CANCEL_SELECTOR, cx);

    assert_eq!(removal.steps(), vec![Step::Observe]);
    assert!(!alert_open(CONFIRM_SELECTOR, cx));
    click(UNINSTALL_SELECTOR, cx);
    assert!(
        alert_open(CONFIRM_SELECTOR, cx),
        "Uninstall is offered again"
    );
}

#[gpui::test]
fn sessions_that_changed_since_asking_stop_the_uninstall_before_anything_is_removed(
    cx: &mut TestAppContext,
) {
    let removal = ScriptedRemoval::new(2);
    *removal.core.lock().unwrap() = Err(RemovalFailure::Changed);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert_eq!(
        removal.steps(),
        vec![Step::Observe, Step::RemoveCore(sessions(2), Data::Keep)],
        "the app stays when SpaceTerm Core does"
    );
    assert!(shows("Couldn’t Uninstall SpaceTerm", cx));
    assert!(shows(
        "The running sessions on this computer changed, so nothing was uninstalled. Try again.",
        cx
    ));
    click_modal(DISMISS_SELECTOR, cx);
    click(UNINSTALL_SELECTOR, cx);
    assert!(
        alert_open(CONFIRM_SELECTOR, cx),
        "Uninstall is offered again"
    );
}

#[gpui::test]
fn an_app_that_cannot_remove_itself_says_how_to_finish(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(0);
    *removal.app.lock().unwrap() = Err(RemovalFailure::App);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert!(shows(
        "SpaceTerm Core was removed, but SpaceTerm couldn’t remove itself, so run install.sh \
         --uninstall.",
        cx
    ));
    assert!(
        alert_open(QUIT_SELECTOR, cx),
        "without SpaceTerm Core the app can only quit"
    );
}

#[gpui::test]
fn a_build_that_is_not_an_installed_app_offers_no_uninstall(cx: &mut TestAppContext) {
    let cx = open(None, cx);
    assert!(cx.debug_bounds(UNINSTALL_SELECTOR).is_none());
    assert!(!shows("Uninstall SpaceTerm", cx));
}

/// A temporary directory removed when the test ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "spaceterm-uninstall-{}-{:x}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(&path).unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rand_suffix() -> u64 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(std::time::SystemTime::now())
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"").unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn an_app_bundle_is_found_from_its_executable_and_removed_whole() {
    let scratch = Scratch::new();
    let bundle = scratch.0.join("Applications/SpaceTerm.app");
    let executable = bundle.join("Contents/MacOS/spaceterm");
    touch(&executable);
    touch(&bundle.join("Contents/Resources/icon.icns"));

    let app = InstalledApp::locate(&executable).expect("the bundle is found");
    assert_eq!(app, InstalledApp::Bundle(bundle.clone()));
    app.remove().unwrap();

    assert!(!bundle.exists());
    assert!(scratch.0.join("Applications").is_dir());
    assert!(InstalledApp::locate(&scratch.0.join("target/debug/spaceterm")).is_none());
}

#[test]
fn an_installed_tree_is_removed_with_its_launcher_icon_and_desktop_entry() {
    let scratch = Scratch::new();
    let home = scratch.0.join("home");
    let data_home = home.join(".local/share");
    let root = home.join(".local/lib/spaceterm");
    touch(&root.join("bin/spaceterm-desktop"));
    touch(&root.join("share/spaceterm/resources"));
    touch(&home.join(".local/lib/.spaceterm.update/partial"));
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::os::unix::fs::symlink(
        root.join("bin/spaceterm-desktop"),
        home.join(".local/bin/spaceterm-desktop"),
    )
    .unwrap();
    let icons = data_home.join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&icons).unwrap();
    std::os::unix::fs::symlink(
        root.join("share/spaceterm/resources"),
        icons.join("io.github.sadiksaifi.spaceterm.svg"),
    )
    .unwrap();
    let entry = data_home.join("applications/io.github.sadiksaifi.spaceterm.desktop");
    touch(&entry);
    let other_entry = data_home.join("applications/other.desktop");
    touch(&other_entry);

    InstalledApp::Tree {
        root: root.clone(),
        home: home.clone(),
        data_home: data_home.clone(),
        application_id: "io.github.sadiksaifi.spaceterm",
    }
    .remove()
    .unwrap();

    assert!(!root.exists());
    assert!(!home.join(".local/lib/.spaceterm.update").exists());
    assert!(
        std::fs::symlink_metadata(home.join(".local/bin/spaceterm-desktop")).is_err(),
        "the launcher that pointed at the app is gone"
    );
    assert!(std::fs::symlink_metadata(icons.join("io.github.sadiksaifi.spaceterm.svg")).is_err());
    assert!(!entry.exists());
    assert!(other_entry.exists(), "other desktop entries stay");
}

/// Preflight installs beside Release under its own names.
#[test]
fn a_preflight_tree_is_removed_with_its_own_launcher_and_leaves_release_alone() {
    let scratch = Scratch::new();
    let home = scratch.0.join("home");
    let data_home = home.join(".local/share");
    let lib = home.join(".local/lib");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["spaceterm", "spaceterm-preflight"] {
        touch(&lib.join(name).join("bin/spaceterm-desktop"));
        std::os::unix::fs::symlink(
            lib.join(name).join("bin/spaceterm-desktop"),
            bin.join(format!("{name}-desktop")),
        )
        .unwrap();
    }
    let entry = data_home.join("applications/io.github.sadiksaifi.spaceterm-preflight.desktop");
    touch(&entry);

    InstalledApp::Tree {
        root: lib.join("spaceterm-preflight"),
        home: home.clone(),
        data_home,
        application_id: "io.github.sadiksaifi.spaceterm-preflight",
    }
    .remove()
    .unwrap();

    assert!(!lib.join("spaceterm-preflight").exists());
    assert!(std::fs::symlink_metadata(bin.join("spaceterm-preflight-desktop")).is_err());
    assert!(!entry.exists());
    assert!(lib.join("spaceterm/bin/spaceterm-desktop").exists());
    assert_eq!(
        std::fs::read_link(bin.join("spaceterm-desktop")).unwrap(),
        lib.join("spaceterm/bin/spaceterm-desktop"),
        "Release's launcher stays"
    );
}

#[test]
fn a_launcher_that_points_elsewhere_stays() {
    let scratch = Scratch::new();
    let home = scratch.0.join("home");
    let root = home.join(".local/lib/spaceterm");
    touch(&root.join("bin/spaceterm-desktop"));
    let foreign = scratch.0.join("elsewhere/spaceterm-desktop");
    touch(&foreign);
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    let launcher = home.join(".local/bin/spaceterm-desktop");
    std::os::unix::fs::symlink(&foreign, &launcher).unwrap();

    InstalledApp::Tree {
        root: root.clone(),
        home: home.clone(),
        data_home: home.join(".local/share"),
        application_id: "io.github.sadiksaifi.spaceterm",
    }
    .remove()
    .unwrap();

    assert!(!root.exists());
    assert_eq!(std::fs::read_link(&launcher).unwrap(), foreign);
}

#[test]
fn a_symbolic_link_in_place_of_the_app_is_refused() {
    let scratch = Scratch::new();
    let target = scratch.0.join("target");
    std::fs::create_dir_all(&target).unwrap();
    touch(&target.join("kept"));
    let bundle = scratch.0.join("SpaceTerm.app");
    std::os::unix::fs::symlink(&target, &bundle).unwrap();

    assert!(InstalledApp::Bundle(bundle).remove().is_err());
    assert!(target.join("kept").exists());
}
