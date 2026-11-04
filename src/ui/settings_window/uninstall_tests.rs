//! Settings > Advanced > Uninstall SpaceTerm, driven through a scripted removal. The installed
//! app's own removal is tested in `crate::installed_app`.
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
    open_with(removal, None, cx)
}

/// Opens Settings on Advanced with `removal` offered and `machines` as the desktop's registry.
fn open_with<'a>(
    removal: Option<&Arc<ScriptedRemoval>>,
    machines: Option<DirectConnections>,
    cx: &'a mut TestAppContext,
) -> &'a mut VisualTestContext {
    if let Some(machines) = machines {
        // The offline registry's worker tries its server on its own thread.
        cx.executor().allow_parking();
        cx.update(|cx| crate::ui::direct_machines::install(machines, cx));
    }
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
    let (machines, _) = crate::direct_connections::tests::offline("studio");
    let cx = open_with(Some(&removal), Some(machines), cx);

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
            Step::ReadCliCredentials,
            Step::RemoveCore(sessions(1), Data::Delete),
            Step::DeleteData {
                cli: CliCredentials::default(),
                machines: true
            },
            Step::RemoveApp,
        ],
        "the command-line tool's keys are read before SpaceTerm Core's data goes, and the \
         desktop's registry removes its own"
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
    *removal.app.lock().unwrap() = Err(AppKept);
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

/// A SpaceTerm Core removal that fails partway may already have deleted the command-line tool's
/// records, so the retry reads none. The keys the first attempt read still go with the data.
#[gpui::test]
fn a_retry_after_a_partial_core_removal_still_removes_the_command_line_tools_keys(
    cx: &mut TestAppContext,
) {
    let removal = ScriptedRemoval::new(0);
    *removal.cli.lock().unwrap() = vec![CliCredentials::read(&[0x61, 0x62])];
    *removal.core.lock().unwrap() = Err(RemovalFailure::Core);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    click(SUPPRESSION, cx);
    click_modal(CONFIRM_SELECTOR, cx);
    assert!(shows(
        "SpaceTerm couldn’t remove SpaceTerm Core from this computer, so try again.",
        cx
    ));
    click_modal(DISMISS_SELECTOR, cx);

    *removal.core.lock().unwrap() = Ok(());
    click(UNINSTALL_SELECTOR, cx);
    click(SUPPRESSION, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert_eq!(
        removal.steps().into_iter().rev().nth(1),
        Some(Step::DeleteData {
            cli: CliCredentials::read(&[0x61, 0x62]),
            machines: false,
        }),
        "the retry deletes the keys the first attempt read"
    );
}

#[gpui::test]
fn data_that_stays_is_named_instead_of_saying_it_was_deleted(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(0);
    *removal.data.lock().unwrap() = Err(DataKept);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    click(SUPPRESSION, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert!(shows(
        "SpaceTerm and SpaceTerm Core were removed from this computer, but SpaceTerm couldn’t \
         delete all of its Workspaces, paired devices and settings.",
        cx
    ));
    assert!(!shows("SpaceTerm Was Uninstalled", cx));
    assert!(alert_open(QUIT_SELECTOR, cx));
}

/// The person asked to delete the data, so an app that can't remove itself doesn't keep it.
#[gpui::test]
fn the_data_goes_even_when_the_app_cannot_remove_itself(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(0);
    *removal.app.lock().unwrap() = Err(AppKept);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    click(SUPPRESSION, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert_eq!(
        removal.steps(),
        vec![
            Step::Observe,
            Step::ReadCliCredentials,
            Step::RemoveCore(sessions(0), Data::Delete),
            Step::DeleteData {
                cli: CliCredentials::default(),
                machines: false
            },
            Step::RemoveApp,
        ]
    );
    assert!(shows(
        "SpaceTerm Core was removed, but SpaceTerm couldn’t remove itself, so run install.sh \
         --uninstall.",
        cx
    ));
    assert!(alert_open(QUIT_SELECTOR, cx));
}

#[gpui::test]
fn an_app_that_cannot_remove_itself_also_names_data_that_stays(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(0);
    *removal.app.lock().unwrap() = Err(AppKept);
    *removal.data.lock().unwrap() = Err(DataKept);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    click(SUPPRESSION, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert!(shows(
        "SpaceTerm Core was removed, but SpaceTerm couldn’t remove itself, so run install.sh \
         --uninstall. Some of its Workspaces, paired devices and settings also remain.",
        cx
    ));
}

#[gpui::test]
fn a_build_that_is_not_an_installed_app_offers_no_uninstall(cx: &mut TestAppContext) {
    let cx = open(None, cx);
    assert!(cx.debug_bounds(UNINSTALL_SELECTOR).is_none());
    assert!(!shows("Uninstall SpaceTerm", cx));
}

/// A scratch directory reached without symbolic links, as the command-line tool's store requires.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut suffix = [0; 8];
        getrandom::fill(&mut suffix).unwrap();
        let path = std::env::temp_dir().join(format!(
            "spaceterm-uninstall-{}-{:x}",
            std::process::id(),
            u64::from_be_bytes(suffix)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(std::fs::canonicalize(&path).unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A credential store that keeps `refused` whatever the person answers.
#[derive(Default)]
struct Items {
    values: std::sync::Mutex<Vec<[u8; 32]>>,
    refused: Option<[u8; 32]>,
}

impl CredentialStore for Items {
    fn kind(&self) -> StoreKind {
        StoreKind::Keychain
    }
    fn read(
        &self,
        _: &[u8; 32],
        _: credentials::Interaction,
    ) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, CredentialError> {
        Err(CredentialError::Unavailable)
    }
    fn insert(&self, key: &[u8; 32], _: &[u8]) -> Result<(), CredentialError> {
        self.values.lock().unwrap().push(*key);
        Ok(())
    }
    fn remove(&self, key: &[u8; 32], _: credentials::Interaction) -> Result<(), CredentialError> {
        if self.refused == Some(*key) {
            return Err(CredentialError::InteractionRequired);
        }
        self.values.lock().unwrap().retain(|value| value != key);
        Ok(())
    }
}

/// The command-line tool's store at `<scratch>/cli` with one saved Machine and one it was
/// still adding, and the desktop's four directories.
fn desktop_data(scratch: &Scratch, items: Arc<Items>) -> DesktopData {
    use crate::direct_connections::{PendingEnrollment, SavedConnection, UnpairState};
    let cli = ConnectionStore::new(scratch.0.join("cli"));
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let route = |id: u8, key: u8| crate::direct_connections::tests::saved(id, key);
    let saved = route(1, 0x61);
    cli.transaction(deadline, &|| false)
        .commit_guarded(|rows| {
            rows.push(SavedConnection {
                name: "penguin".into(),
                route: saved.route.id,
                endpoint: saved.route.endpoint.clone(),
                paired: saved.route.paired.clone(),
                credential: saved.credential,
                credential_store: StoreKind::Keychain,
                unpair: UnpairState::Paired,
            });
            Ok(())
        })
        .unwrap();
    let enrolling = route(2, 0x62);
    cli.transaction(deadline, &|| false)
        .reserve(PendingEnrollment {
            route: enrolling.route.id,
            name: "studio".into(),
            endpoint: enrolling.route.endpoint,
            pin: enrolling.route.paired.pin,
            claim: [0x72; 32],
            credential: enrolling.credential,
            credential_store: StoreKind::Keychain,
            public_key: [0x73; 32],
            offered: false,
            confirmed: false,
        })
        .unwrap();
    for key in [[0x61; 32], [0x62; 32]] {
        items.insert(&key, b"secret").unwrap();
    }
    let roots: Vec<_> = ["config", "data", "state", "cache"]
        .into_iter()
        .map(|name| scratch.0.join(name))
        .collect();
    for root in &roots {
        std::fs::create_dir(root).unwrap();
    }
    DesktopData {
        cli: Some(cli),
        open: Box::new(move |kind| {
            assert_eq!(
                kind,
                StoreKind::Keychain,
                "the store each record names opens"
            );
            Ok(Arc::clone(&items) as Arc<dyn CredentialStore>)
        }),
        roots: Some(roots),
    }
}

/// "Also delete..." removes the keys of the command-line tool's saved Machines and of the one it
/// was still adding, which SpaceTerm Core's data held, and the desktop's own Machines and
/// directories.
#[test]
fn deleting_the_data_removes_the_command_line_tools_keys_and_the_desktops() {
    let scratch = Scratch::new();
    let items = Arc::new(Items::default());
    let data = desktop_data(&scratch, Arc::clone(&items));
    let cli = data.cli_credentials();
    // SpaceTerm Core's data goes between the read and the removal.
    std::fs::remove_dir_all(scratch.0.join("cli")).unwrap();
    let (machines, _) = crate::direct_connections::tests::offline("studio");

    assert_eq!(data.delete(cli, Some(machines.clone())), Ok(()));

    assert!(items.values.lock().unwrap().is_empty());
    assert!(
        machines.routes().is_empty(),
        "the desktop's Machines go too"
    );
    for name in ["config", "data", "state", "cache"] {
        assert!(!scratch.0.join(name).exists());
    }
}

/// A key the store keeps doesn't stop the rest, and the uninstall says some data stayed.
#[test]
fn a_key_that_stays_is_reported_after_everything_else_goes() {
    let scratch = Scratch::new();
    let items = Arc::new(Items {
        refused: Some([0x61; 32]),
        ..Items::default()
    });
    let data = desktop_data(&scratch, Arc::clone(&items));
    let cli = data.cli_credentials();
    let (machines, _) = crate::direct_connections::tests::offline("studio");

    assert_eq!(data.delete(cli, Some(machines)), Err(DataKept));

    assert_eq!(*items.values.lock().unwrap(), vec![[0x61; 32]]);
    assert!(!scratch.0.join("config").exists());
}

/// Without the desktop's registry, its keys can't be found, so they may stay.
#[test]
fn deleting_without_the_desktops_registry_reports_data_that_may_stay() {
    let scratch = Scratch::new();
    let items = Arc::new(Items::default());
    let data = desktop_data(&scratch, Arc::clone(&items));
    let cli = data.cli_credentials();

    assert_eq!(data.delete(cli, None), Err(DataKept));
    assert!(items.values.lock().unwrap().is_empty());
}
