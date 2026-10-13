use super::server_source::ServerDirectorySource;
use super::*;
use gpui::{TestAppContext, VisualTestContext};
use spaceterm_client::{directories::*, git::rpc::*};
use spaceterm_core::{
    authority::{ConnectionGeneration, ServerEpoch},
    identity::*,
    organization::ServerDirectory,
    server_directories::*,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
};
use std::{collections::VecDeque, num::NonZeroU64};

pub(in crate::ui) struct Peer {
    pub(super) state: Mutex<GitConnectionState>,
    pub(super) queries: Mutex<Vec<DirectoryQuery>>,
    pages: Mutex<VecDeque<Result<DirectoryPage, ClientDirectoryFailure>>>,
    pub(super) class: Mutex<DirectoryValidationClass>,
    pub(super) gate: Mutex<Option<async_channel::Receiver<()>>>,
    cancellations: Arc<AtomicUsize>,
}
struct ReadGuard(Arc<AtomicUsize>);
impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, AtomicOrdering::SeqCst);
    }
}
impl Peer {
    pub(in crate::ui) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GitConnectionState {
                context: DirectoryContext::new(DirectoryContextParts {
                    server: server(),
                    epoch: ServerEpoch::new(1),
                    pairing: PairingId::new(server(), 1).unwrap(),
                    client: ClientId::new(server(), 1).unwrap(),
                })
                .unwrap(),
                generation: ConnectionGeneration::new(1),
                readiness: GitReadiness::Ready,
            }),
            queries: Mutex::default(),
            pages: Mutex::default(),
            class: Mutex::new(DirectoryValidationClass::AvailableDirectory),
            gate: Mutex::default(),
            cancellations: Arc::default(),
        })
    }
    fn page(&self, scope: &str, observation: u64, names: &[&str], next: bool) -> DirectoryPage {
        let context = self.state.lock().unwrap().context;
        let directory = directory(scope);
        let observation = NonZeroU64::new(observation).unwrap();
        DirectoryPage::new(DirectoryPageParts {
            context,
            directory: directory.clone(),
            observation,
            entries: names
                .iter()
                .map(|name| DirectoryEntry {
                    name: DirectoryEntryName::new(name.as_bytes().to_vec()).unwrap(),
                    directory: crate_directory(&format!(
                        "{}/{}",
                        scope.trim_end_matches('/'),
                        name
                    )),
                    selectable: true,
                })
                .collect(),
            next: next.then(|| {
                DirectoryCursor::new(DirectoryCursorParts {
                    context,
                    scope: directory,
                    observation,
                    random: [observation.get() as u8; 32],
                })
                .unwrap()
            }),
        })
        .unwrap()
    }
    fn source(self: &Arc<Self>) -> ServerDirectorySource {
        ServerDirectorySource::new(
            server(),
            "Owning server",
            self.clone(),
            DirectoryPurpose::PaneRepair,
        )
        .unwrap()
    }
}
impl DirectoryConnectionPort for Peer {
    fn state(&self) -> Result<GitConnectionState, ClientDirectoryFailure> {
        Ok(*self.state.lock().unwrap())
    }
    fn query(
        &self,
        query: DirectoryQuery,
    ) -> GitClientFuture<'_, Result<DirectoryQueryResult, ClientDirectoryFailure>> {
        assert_eq!(query.parts().context, self.state.lock().unwrap().context);
        self.queries.lock().unwrap().push(query.clone());
        let result = match &query.parts().action {
            DirectoryQueryAction::Home => Ok(DirectoryQueryResult::Home(directory("/server/home"))),
            DirectoryQueryAction::Browse { directory, .. } => self
                .pages
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(self.page(
                        std::str::from_utf8(directory.bytes()).unwrap(),
                        1,
                        &["alpha", "Projects"],
                        false,
                    ))
                })
                .map(|page| DirectoryQueryResult::Page(Box::new(page))),
            DirectoryQueryAction::Validate { directory, purpose } => {
                Ok(DirectoryQueryResult::Validation(DirectoryValidation {
                    directory: directory.clone(),
                    purpose: *purpose,
                    class: *self.class.lock().unwrap(),
                }))
            }
        };
        let gate = self.gate.lock().unwrap().take();
        let guard = gate.as_ref().map(|_| ReadGuard(self.cancellations.clone()));
        Box::pin(async move {
            if let Some(gate) = gate {
                let _guard = guard;
                gate.recv()
                    .await
                    .map_err(|_| ClientDirectoryFailure::Unavailable)?;
            }
            result
        })
    }
}
fn server() -> ServerId {
    ServerId::new([7; 16])
}
fn directory(path: &str) -> ServerDirectory {
    crate_directory(path)
}
fn crate_directory(path: &str) -> ServerDirectory {
    ServerDirectory::new(server(), path.as_bytes().to_vec()).unwrap()
}
fn path(path: &str) -> PickerPath {
    PickerPath::new(path.to_owned()).unwrap()
}
struct Harness {
    picker: Entity<DirectoryPicker>,
}
impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(self.picker.clone())
    }
}
type PickerEvents = Rc<std::cell::RefCell<Vec<DirectoryPickerEvent>>>;
type ServerEvents = Rc<std::cell::RefCell<Vec<ServerDirectorySelected>>>;
fn picker(
    peer: Arc<Peer>,
    cx: &mut TestAppContext,
) -> (
    Entity<DirectoryPicker>,
    PickerEvents,
    ServerEvents,
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init).unwrap();
    let legacy = Rc::new(std::cell::RefCell::new(Vec::new()));
    let selected = Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = legacy.clone();
    let server_recorded = selected.clone();
    let (harness, cx) = cx.add_window_view(move |window, cx| {
        let picker = cx.new(|cx| {
            DirectoryPicker::new(Rc::new(peer.source()), "Choose Directory", window, cx)
                .with_system_selection("System Directory Selection...".into())
        });
        cx.subscribe(&picker, move |_, _, event: &DirectoryPickerEvent, _| {
            recorded.borrow_mut().push(event.clone())
        })
        .detach();
        cx.subscribe(&picker, move |_, _, event: &ServerDirectorySelected, _| {
            server_recorded.borrow_mut().push(event.clone())
        })
        .detach();
        Harness { picker }
    });
    let picker = harness.read_with(cx, |h, _| h.picker.clone());
    cx.update(|window, cx| {
        window.activate_window();
        picker.update(cx, |p, cx| {
            p.open(window, cx);
        });
    });
    cx.run_until_parked();
    (picker, legacy, selected, cx)
}

#[gpui::test]
fn server_missing_directory_never_offers_creation(cx: &mut TestAppContext) {
    let peer = Peer::new();
    *peer.class.lock().unwrap() = DirectoryValidationClass::Missing;
    let (picker, legacy, selected, cx) = picker(peer.clone(), cx);
    assert!(!picker.read_with(cx, |p, _| p.can_confirm()));
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("directory-picker-create").is_none());
    assert!(selected.borrow().is_empty());
    assert!(!legacy.borrow().iter().any(|e| matches!(
        e,
        DirectoryPickerEvent::Confirmed(_) | DirectoryPickerEvent::SystemSelectionRequested
    )));
    assert!(picker.read_with(cx, |p, _| {
        p.empty_notice().description.unwrap().contains("existing")
    }));
}

#[gpui::test]
fn server_directory_selection_is_not_a_native_pin_or_system_choice(cx: &mut TestAppContext) {
    let peer = Peer::new();
    let (picker, legacy, selected, cx) = picker(peer.clone(), cx);
    cx.update(|window, cx| picker.update(cx, |p, cx| p.request_system_selection(window, cx)));
    assert!(picker.read_with(cx, |p, _| p.is_open()));
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert_eq!(
        &*selected.borrow(),
        &[ServerDirectorySelected(directory("/server/home"))]
    );
    assert!(!legacy.borrow().iter().any(|e| matches!(
        e,
        DirectoryPickerEvent::Confirmed(_) | DirectoryPickerEvent::SystemSelectionRequested
    )));
    assert!(
        peer.queries
            .lock()
            .unwrap()
            .iter()
            .filter_map(|q| match &q.parts().action {
                DirectoryQueryAction::Validate { purpose, .. } => Some(purpose),
                _ => None,
            })
            .all(|p| *p == DirectoryPurpose::PaneRepair)
    );
}

#[gpui::test]
fn server_replaced_directory_cursor_discards_all_partial_rows(cx: &mut TestAppContext) {
    let peer = Peer::new();
    peer.pages.lock().unwrap().extend([
        Ok(peer.page("/server/home", 1, &["old-child"], true)),
        Err(ClientDirectoryFailure::Remote(
            DirectoryFailure::StaleCursor,
        )),
    ]);
    let (picker, _, _, cx) = picker(peer, cx);
    assert!(picker.read_with(cx, |p, _| p.row_names().is_empty()));
    assert!(picker.read_with(cx, |p, _| p.snapshot.is_none()));
}

#[gpui::test]
fn server_page_scope_and_observation_cannot_be_replaced(cx: &mut TestAppContext) {
    for replaced_scope in [true, false] {
        let peer = Peer::new();
        let source = peer.source();
        peer.pages.lock().unwrap().extend([
            Ok(peer.page("/server/home", 1, &["old-child"], true)),
            Ok(peer.page(
                if replaced_scope {
                    "/other"
                } else {
                    "/server/home"
                },
                2,
                &["new-child"],
                false,
            )),
        ]);
        let result = cx
            .foreground_executor()
            .block_test(source.list_directories(path("/server/home")));
        assert!(result.is_err());
    }
}

#[gpui::test]
fn server_source_rejects_reconnected_or_changed_principals_before_read(cx: &mut TestAppContext) {
    let peer = Peer::new();
    let source = peer.source();
    cx.foreground_executor()
        .block_test(source.discover_home())
        .unwrap();
    peer.queries.lock().unwrap().clear();
    peer.state.lock().unwrap().generation = ConnectionGeneration::new(2);
    assert!(
        cx.foreground_executor()
            .block_test(source.list_directories(path("~/")))
            .is_err()
    );
    assert!(
        cx.foreground_executor()
            .block_test(source.select(path("~/")))
            .is_err()
    );
    assert!(peer.queries.lock().unwrap().is_empty());
}

#[gpui::test]
fn server_directory_source_cannot_mkdir_or_acquire_a_client_native_pin(cx: &mut TestAppContext) {
    let peer = Peer::new();
    let source = peer.source();
    assert!(
        cx.foreground_executor()
            .block_test(source.create_directory_recursively(path("/new")))
            .is_err()
    );
    assert!(
        cx.foreground_executor()
            .block_test(source.pin(path("/existing")))
            .is_err()
    );
    assert!(peer.queries.lock().unwrap().is_empty());
}

#[gpui::test]
fn server_cancelled_selection_drops_read_and_never_confirms(cx: &mut TestAppContext) {
    let peer = Peer::new();
    let (picker, legacy, selected, cx) = picker(peer.clone(), cx);
    let (_sender, gate) = async_channel::bounded(1);
    *peer.gate.lock().unwrap() = Some(gate);
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert!(picker.read_with(cx, |p, _| p.busy.is_some()));
    cx.update(|window, cx| picker.update(cx, |p, cx| p.cancel(window, cx)));
    cx.run_until_parked();
    assert_eq!(peer.cancellations.load(AtomicOrdering::SeqCst), 1);
    assert!(selected.borrow().is_empty());
    assert!(
        !legacy
            .borrow()
            .iter()
            .any(|e| matches!(e, DirectoryPickerEvent::Confirmed(_)))
    );
    assert!(!picker.read_with(cx, |p, _| p.blocks_terminal_input()));
}

#[gpui::test]
fn server_picker_keyboard_and_accessibility_keep_focus_on_the_owning_source(
    cx: &mut TestAppContext,
) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};
    let peer = Peer::new();
    let (picker, _, selected, cx) = picker(peer.clone(), cx);
    let tree = A11yTree::read(cx);
    assert_eq!(
        tree.with_role("Dialog")[0]["aria"]["label"],
        "Choose Directory"
    );
    assert_eq!(tree.with_role("Dialog")[0]["aria"]["modal"], true);
    assert_eq!(tree.focused().unwrap()["aria"]["role"], "TextInput");
    assert!(
        tree.with_role("Button")
            .iter()
            .all(|n| n["aria"]["label"] != "System Directory Selection...")
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        picker.read_with(cx, |p, cx| p.palette.read(cx).query().to_owned()),
        "~/alpha/"
    );
    let tree = A11yTree::read(cx);
    let actions = tree.with_role("Button");
    let choose = actions
        .iter()
        .find(|node| node["aria"]["label"] == "Choose Directory")
        .unwrap();
    perform(cx, choose, Action::Click);
    assert_eq!(selected.borrow().len(), 1);
    assert_eq!(selected.borrow()[0].0, directory("/server/home/alpha/"));
    assert!(
        peer.queries
            .lock()
            .unwrap()
            .iter()
            .any(|q| matches!(&q.parts().action,
        DirectoryQueryAction::Validate { directory, purpose: DirectoryPurpose::PaneRepair }
        if directory.bytes() == b"/server/home/alpha/"))
    );
}

#[gpui::test]
fn server_leaf_filter_refresh_reobserves_directory_instead_of_reusing_path_cache(
    cx: &mut TestAppContext,
) {
    let peer = Peer::new();
    peer.pages.lock().unwrap().extend([
        Ok(peer.page("/server/home", 1, &["before"], false)),
        Ok(peer.page("/server/home", 2, &["after"], false)),
    ]);
    let (picker, _, _, cx) = picker(peer.clone(), cx);
    picker.update(cx, |p, cx| {
        p.palette
            .update(cx, |palette, cx| palette.set_query("~/a", cx))
    });
    cx.run_until_parked();
    assert_eq!(picker.read_with(cx, |p, _| p.row_names()), ["after"]);
    assert_eq!(
        peer.queries
            .lock()
            .unwrap()
            .iter()
            .filter(|q| matches!(q.parts().action, DirectoryQueryAction::Browse { .. }))
            .count(),
        2
    );
}

#[gpui::test]
fn server_picker_short_layout_keeps_confirm_and_back_reachable(cx: &mut TestAppContext) {
    let (picker, _, _, cx) = picker(Peer::new(), cx);
    cx.simulate_resize(gpui::size(gpui::px(360.0), gpui::px(300.0)));
    cx.run_until_parked();
    let panel = cx.debug_bounds("command-palette-panel").unwrap();
    let confirm = cx.debug_bounds(CONFIRM_ACTION).unwrap();
    let back = cx.debug_bounds(ENCLOSING_ROW).unwrap();
    assert!(panel.contains(&confirm.center()));
    assert!(panel.contains(&back.center()));
    assert!(confirm.origin.y + confirm.size.height <= gpui::px(300.0));
    assert!(picker.read_with(cx, |p, _| p.blocks_terminal_input()));
}

#[gpui::test]
fn server_directory_pages_from_another_principal_or_epoch_are_rejected(cx: &mut TestAppContext) {
    for other_epoch in [false, true] {
        let peer = Peer::new();
        let mut parts = peer
            .page("/server/home", 1, &["child"], false)
            .parts()
            .clone();
        let mut context = parts.context.parts();
        if other_epoch {
            context.epoch = ServerEpoch::new(2);
        } else {
            context.pairing = PairingId::new(server(), 2).unwrap();
        }
        parts.context = DirectoryContext::new(context).unwrap();
        peer.pages
            .lock()
            .unwrap()
            .push_back(Ok(DirectoryPage::new(parts).unwrap()));
        assert!(
            cx.foreground_executor()
                .block_test(peer.source().list_directories(path("/server/home")))
                .is_err()
        );
    }
}

#[gpui::test]
fn server_directory_empty_continuations_are_bounded(cx: &mut TestAppContext) {
    let peer = Peer::new();
    for index in 1..=8 {
        let mut parts = peer.page("/server/home", 1, &[], true).parts().clone();
        let mut cursor = parts.next.as_ref().unwrap().parts().clone();
        cursor.random = [index as u8; 32];
        parts.next = Some(DirectoryCursor::new(cursor).unwrap());
        peer.pages
            .lock()
            .unwrap()
            .push_back(Ok(DirectoryPage::new(parts).unwrap()));
    }
    let listing = cx
        .foreground_executor()
        .block_test(peer.source().list_directories(path("/server/home")))
        .unwrap();
    assert!(listing.rows().is_empty());
    assert!(listing.is_truncated());
    assert_eq!(peer.queries.lock().unwrap().len(), 8);
}
