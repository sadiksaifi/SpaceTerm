use super::*;
use gpui::{TestAppContext, VisualTestContext};
use spaceterm_client::git::rpc::*;
use spaceterm_core::{
    authority::{ConnectionGeneration, ServerEpoch},
    identity::*,
    operations::{Command, OperationSequence},
    server_directories::{DirectoryContext, DirectoryContextParts},
    worktrees::{queries::*, *},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Peer {
    state: Mutex<GitConnectionState>,
    queries: Mutex<Vec<GitQuery>>,
    directory: Mutex<String>,
    submissions: Mutex<Vec<GitCommand>>,
    operation: Mutex<Option<Arc<Operation>>>,
    lookups: Mutex<Vec<OperationId>>,
    preview_sequence: AtomicUsize,
    preview_gate: Mutex<Option<async_channel::Receiver<()>>>,
    submit_gate: Mutex<Option<async_channel::Receiver<()>>>,
    cancelled_reads: Arc<AtomicUsize>,
    submit_failure: Mutex<Option<ClientGitFailure>>,
    state_gate: Mutex<Option<async_channel::Receiver<()>>>,
    cancelled_observations: Arc<AtomicUsize>,
}
struct ReadGuard(Arc<AtomicUsize>);
impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
struct Operation {
    id: OperationId,
    command: GitCommand,
    state: Mutex<GitOperationState>,
    state_gate: Mutex<Option<async_channel::Receiver<()>>>,
    cancelled_observations: Arc<AtomicUsize>,
}
impl GitOperationPort for Operation {
    fn id(&self) -> OperationId {
        self.id
    }
    fn command(&self) -> &GitCommand {
        &self.command
    }
    fn state(&self) -> GitClientFuture<'_, Result<GitOperationState, ClientGitFailure>> {
        let state = self.state.lock().unwrap().clone();
        let gate = self.state_gate.lock().unwrap().take();
        let guard = gate
            .as_ref()
            .map(|_| ReadGuard(self.cancelled_observations.clone()));
        Box::pin(async move {
            if let Some(gate) = gate {
                let _guard = guard;
                gate.recv()
                    .await
                    .map_err(|_| ClientGitFailure::Unavailable)?;
            }
            Ok(state)
        })
    }
}
impl GitConnectionPort for Peer {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        Ok(*self.state.lock().unwrap())
    }
    fn query(
        &self,
        query: GitQuery,
    ) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>> {
        self.queries.lock().unwrap().push(query.clone());
        let context = self.state.lock().unwrap().context.parts();
        let scope = RepositoryScope {
            server: context.server,
            repository: RepositoryId::new(context.server, 1).unwrap(),
        };
        let result = match query {
            GitQuery::Branches { .. } => GitQueryResult::Branches(GitBranchPage {
                records: vec![GitBranchRecord {
                    name: ServerBranchName::new("main".into()).unwrap(),
                    remote: false,
                    checked_out: true,
                }],
                next: None,
            }),
            GitQuery::PreviewCreate { request } => {
                let sequence = self.preview_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                let (directory, version) = match &request.parts().location {
                    WorktreeLocationChoice::ServerTemplate { expected_version } => (
                        ServerDirectory::new(
                            context.server,
                            self.directory.lock().unwrap().as_bytes().to_vec(),
                        )
                        .unwrap(),
                        Some(*expected_version),
                    ),
                    WorktreeLocationChoice::Explicit(directory) => (directory.clone(), None),
                };
                GitQueryResult::Assessment(Box::new(GitAssessmentPage {
                    token: GitAssessmentToken::new(GitAssessmentTokenParts {
                        server: context.server,
                        epoch: context.epoch,
                        pairing: context.pairing,
                        client: context.client,
                        kind: GitAssessmentKind::Create,
                        random: [sequence as u8; 32],
                    })
                    .unwrap(),
                    tabs: vec![],
                    panes: vec![],
                    fallbacks: vec![],
                    next: None,
                    requires_confirmation: false,
                    creation_preview: Some(
                        WorktreeCreationPreview::new(request, directory, version).unwrap(),
                    ),
                    removal: None,
                }))
            }
            _ => return Box::pin(async { Err(ClientGitFailure::InvalidResponse) }),
        };
        let gate = if matches!(result, GitQueryResult::Assessment(_)) {
            self.preview_gate.lock().unwrap().take()
        } else {
            None
        };
        let guard = gate
            .as_ref()
            .map(|_| ReadGuard(self.cancelled_reads.clone()));
        Box::pin(async move {
            if let Some(gate) = gate {
                let _guard = guard;
                gate.recv()
                    .await
                    .map_err(|_| ClientGitFailure::Unavailable)?;
            }
            Ok(GitReadResult {
                epoch: context.epoch,
                scope: Some(scope),
                result,
            })
        })
    }
    fn subscribe(
        &self,
        _: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        Err(ClientGitFailure::Unavailable)
    }
    fn submit(
        &self,
        command: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        let Command::Worktree(command) = command else {
            panic!("only Git commands")
        };
        self.submissions.lock().unwrap().push(command.clone());
        let context = self.state.lock().unwrap().context.parts();
        let operation = Arc::new(Operation {
            id: OperationId::new(
                OperationStreamId::new(
                    OperationStreamKey::new(context.server, 1).unwrap(),
                    context.pairing,
                    context.client,
                )
                .unwrap(),
                OperationSequence::new(1).unwrap(),
            ),
            command,
            state: Mutex::new(GitOperationState::Unresolved),
            state_gate: Mutex::new(self.state_gate.lock().unwrap().take()),
            cancelled_observations: self.cancelled_observations.clone(),
        });
        *self.operation.lock().unwrap() = Some(operation.clone());
        let gate = self.submit_gate.lock().unwrap().take();
        let failure = self.submit_failure.lock().unwrap().take();
        Box::pin(async move {
            if let Some(error) = failure {
                return Err(error);
            }
            if let Some(gate) = gate {
                gate.recv()
                    .await
                    .map_err(|_| ClientGitFailure::Unavailable)?;
            }
            Ok(operation as Arc<dyn GitOperationPort>)
        })
    }
    fn lookup(
        &self,
        operation: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        self.lookups.lock().unwrap().push(operation);
        let tracker = self.operation.lock().unwrap().clone().unwrap();
        Box::pin(async move { Ok(tracker as Arc<dyn GitOperationPort>) })
    }
}
struct Harness {
    form: Entity<WorktreeForm>,
}
impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        spaceterm_ui::ModalLayer::new(div().size_full())
    }
}
type Events = Rc<std::cell::RefCell<Vec<WorktreeFormEvent>>>;
fn form_window(
    cx: &mut TestAppContext,
) -> (
    Entity<WorktreeForm>,
    Arc<Peer>,
    Events,
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init).unwrap();
    let server = ServerId::new([9; 16]);
    let peer = Arc::new(Peer {
        state: Mutex::new(GitConnectionState {
            context: DirectoryContext::new(DirectoryContextParts {
                server,
                epoch: ServerEpoch::new(7),
                pairing: PairingId::new(server, 2).unwrap(),
                client: ClientId::new(server, 3).unwrap(),
            })
            .unwrap(),
            generation: ConnectionGeneration::new(1),
            readiness: GitReadiness::Ready,
        }),
        queries: Mutex::default(),
        directory: Mutex::new("/srv/work/demo".into()),
        submissions: Mutex::default(),
        operation: Mutex::default(),
        lookups: Mutex::default(),
        preview_sequence: AtomicUsize::new(0),
        preview_gate: Mutex::default(),
        submit_gate: Mutex::default(),
        cancelled_reads: Arc::default(),
        submit_failure: Mutex::default(),
        state_gate: Mutex::default(),
        cancelled_observations: Arc::default(),
    });
    let backend = peer.clone();
    let events: Events = Rc::default();
    let captured = events.clone();
    let (harness, cx) = cx.add_window_view(move |window, cx| {
        let store = cx.new(super::super::server_git::ServerGitStore::new);
        store.update(cx, |store, cx| {
            store
                .connect(
                    GitClient::new(server, backend).unwrap(),
                    ServerEpoch::new(7),
                    cx,
                )
                .unwrap()
        });
        let requests = store.read(cx).requests(server).unwrap();
        let form = cx.new(|cx| {
            WorktreeForm::new_server(
                requests,
                ServerWorktreeFormContext {
                    workspace: ServerWorkspaceId::new(server, 5).unwrap(),
                    repository: RepositoryScope {
                        server,
                        repository: RepositoryId::new(server, 1).unwrap(),
                    },
                    repository_name: "app".into(),
                    location_version: FieldVersion::new(42),
                    checked_out: vec![("main".into(), "app".into())],
                    default_base: Some("main".into()),
                    online: true,
                    original_operation: None,
                },
                window,
                cx,
            )
            .unwrap()
        });
        cx.subscribe(&form, move |_, _, event, _| {
            captured.borrow_mut().push(event.clone())
        })
        .detach();
        Harness { form }
    });
    let form = harness.read_with(cx, |harness, _| harness.form.clone());
    cx.update(|window, cx| {
        window.activate_window();
        form.update(cx, |form, cx| assert!(form.present(window, cx)));
    });
    cx.run_until_parked();
    (form, peer, events, cx)
}
fn name(form: &Entity<WorktreeForm>, value: &str, cx: &mut VisualTestContext) {
    super::tests::type_into(form, FormField::Name, value, cx);
}
#[gpui::test]
fn preview_uses_the_selected_servers_setting_and_directory(cx: &mut TestAppContext) {
    let (form, peer, _, cx) = form_window(cx);
    name(&form, "demo", cx);
    assert_eq!(super::tests::location(&form, cx), "/srv/work/demo");
    let queries = peer.queries.lock().unwrap();
    let draft = queries
        .iter()
        .find_map(|query| match query {
            GitQuery::PreviewCreate { request } => Some(request),
            _ => None,
        })
        .expect("server preview request");
    assert_eq!(
        draft.parts().location,
        WorktreeLocationChoice::ServerTemplate {
            expected_version: FieldVersion::new(42)
        }
    );
    assert_eq!(draft.parts().repository.server, ServerId::new([9; 16]));
}

#[gpui::test]
fn a_changed_server_preview_requires_another_explicit_create(cx: &mut TestAppContext) {
    let (form, peer, _, cx) = form_window(cx);
    name(&form, "demo", cx);
    *peer.directory.lock().unwrap() = "/srv/changed/demo".into();
    super::tests::create(cx);
    assert!(peer.submissions.lock().unwrap().is_empty());
    assert_eq!(super::tests::location(&form, cx), "/srv/changed/demo");
    assert!(form.read_with(cx, |form, _| form.is_open()));
    super::tests::create(cx);
    let commands = peer.submissions.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].assessment().parts().random, [3; 32]);
}

#[gpui::test]
fn a_changed_selected_server_setting_is_reassessed_without_client_home(cx: &mut TestAppContext) {
    let (form, peer, _, cx) = form_window(cx);
    name(&form, "demo", cx);
    *peer.directory.lock().unwrap() = "/server/new-setting/demo".into();
    form.update(cx, |form, cx| {
        form.server_context_changed(FieldVersion::new(43), true, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        super::tests::location(&form, cx),
        "/server/new-setting/demo"
    );
    assert!(peer.queries.lock().unwrap().iter().any(|query| matches!(query,
        GitQuery::PreviewCreate { request } if request.parts().location == WorktreeLocationChoice::ServerTemplate { expected_version: FieldVersion::new(43) })));
    assert!(peer.submissions.lock().unwrap().is_empty());
}

#[gpui::test]
fn explicit_location_is_qualified_and_not_replaced_by_the_template(cx: &mut TestAppContext) {
    let (form, peer, _, cx) = form_window(cx);
    name(&form, "demo", cx);
    super::tests::type_into(&form, FormField::Location, "/srv/person/checkout", cx);
    name(&form, "other", cx);
    assert_eq!(super::tests::location(&form, cx), "/srv/person/checkout");
    assert!(peer.queries.lock().unwrap().iter().any(|query| matches!(query,
        GitQuery::PreviewCreate { request } if matches!(&request.parts().location,
            WorktreeLocationChoice::Explicit(directory) if directory.server() == ServerId::new([9;16]) && directory.bytes() == b"/srv/person/checkout"))));
    super::tests::type_into(&form, FormField::Location, "~/client-only", cx);
    super::tests::create(cx);
    assert!(peer.submissions.lock().unwrap().is_empty());
}

#[gpui::test]
fn an_offline_server_refuses_creation_without_using_a_native_backend(cx: &mut TestAppContext) {
    let (form, peer, _, cx) = form_window(cx);
    name(&form, "demo", cx);
    peer.state.lock().unwrap().readiness = GitReadiness::Offline;
    form.update(cx, |form, cx| {
        form.server_context_changed(FieldVersion::new(42), false, cx)
    });
    super::tests::create(cx);
    assert!(peer.submissions.lock().unwrap().is_empty());
    assert!(form.read_with(cx, |form, _| form.is_open()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!form.read_with(cx, |form, _| form.is_open()));
}

#[gpui::test]
fn escape_cancels_an_assessment_before_submission(cx: &mut TestAppContext) {
    let (form, peer, events, cx) = form_window(cx);
    name(&form, "demo", cx);
    let (send, recv) = async_channel::bounded(1);
    *peer.preview_gate.lock().unwrap() = Some(recv);
    super::tests::create(cx);
    assert!(form.read_with(cx, |form, _| form.pending));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!form.read_with(cx, |form, _| form.is_open()));
    assert!(send.try_send(()).is_err());
    assert_eq!(peer.cancelled_reads.load(Ordering::SeqCst), 1);
    assert!(peer.submissions.lock().unwrap().is_empty());
    assert_eq!(*events.borrow(), [WorktreeFormEvent::Cancelled]);
}

fn completed(peer: &Peer) -> OperationId {
    let operation = peer.operation.lock().unwrap().clone().unwrap();
    *operation.state.lock().unwrap() = GitOperationState::Completed(
        GitMutationOutcome::new(GitMutationOutcomeParts::Created {
            worktree: ServerWorktreeId::new(ServerId::new([9; 16]), 8).unwrap(),
            revision: OrganizationRevision::new(17),
        })
        .unwrap(),
    );
    operation.id
}

#[gpui::test]
fn a_lost_reply_checks_only_the_original_operation_and_returns_qualified_membership(
    cx: &mut TestAppContext,
) {
    let (form, peer, events, cx) = form_window(cx);
    name(&form, "demo", cx);
    super::tests::create(cx);
    assert!(form.read_with(cx, |form, _| form.is_open() && !form.pending));
    let original = completed(&peer);
    super::tests::create(cx);
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
    assert_eq!(*peer.lookups.lock().unwrap(), [original]);
    assert!(!form.read_with(cx, |form, _| form.is_open()));
    assert!(events.borrow().iter().any(|event| matches!(event,
        WorktreeFormEvent::ServerCreated { workspace, repository, operation, worktree, revision }
            if *workspace == ServerWorkspaceId::new(ServerId::new([9;16]), 5).unwrap()
            && repository.server == ServerId::new([9;16]) && *operation == original
            && *worktree == ServerWorktreeId::new(ServerId::new([9;16]), 8).unwrap()
            && *revision == OrganizationRevision::new(17))));
    assert!(
        !events
            .borrow()
            .iter()
            .any(|event| matches!(event, WorktreeFormEvent::Created(_)))
    );
}

#[gpui::test]
fn dismissal_after_admission_keeps_the_write_but_never_requests_attachment_or_startup(
    cx: &mut TestAppContext,
) {
    let (form, peer, events, cx) = form_window(cx);
    name(&form, "demo", cx);
    let (send, recv) = async_channel::bounded(1);
    *peer.submit_gate.lock().unwrap() = Some(recv);
    super::tests::create(cx);
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let original = completed(&peer);
    send.try_send(()).unwrap();
    cx.run_until_parked();
    assert!(!form.read_with(cx, |form, _| form.is_open()));
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
    assert_eq!(
        peer.operation.lock().unwrap().as_ref().unwrap().id,
        original
    );
    assert_eq!(*events.borrow(), [WorktreeFormEvent::Cancelled]);
}

#[gpui::test]
fn server_form_retains_keyboard_source_navigation_and_named_editors(cx: &mut TestAppContext) {
    let (form, _, _, cx) = form_window(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(tree.exposed(tree.node("Branch name")));
    assert!(tree.exposed(tree.node("Location")));
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Branch name");
    cx.simulate_keystrokes("shift-tab right");
    cx.run_until_parked();
    assert_eq!(
        form.read_with(cx, |form, _| form.source),
        BranchSource::Existing
    );
    form.update(cx, |form, cx| form.set_source(BranchSource::New, cx));
    assert!(cx.debug_bounds("new-worktree-name").is_some());
    assert!(cx.debug_bounds("new-worktree-location").is_some());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!form.read_with(cx, |form, _| form.is_open()));
}

#[gpui::test]
fn a_setting_changed_while_assessing_cannot_submit_the_old_assessment(cx: &mut TestAppContext) {
    let (form, peer, _, cx) = form_window(cx);
    name(&form, "demo", cx);
    let (send, recv) = async_channel::bounded(1);
    *peer.preview_gate.lock().unwrap() = Some(recv);
    super::tests::create(cx);
    form.update(cx, |form, cx| {
        form.server_context_changed(FieldVersion::new(43), true, cx)
    });
    send.try_send(()).unwrap();
    cx.run_until_parked();
    assert!(peer.submissions.lock().unwrap().is_empty());
    assert!(form.read_with(cx, |form, _| form.is_open() && !form.pending));
    super::tests::create(cx);
    assert!(peer.submissions.lock().unwrap().is_empty());
    super::tests::create(cx);
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
}

#[gpui::test]
fn an_uncertain_submission_without_a_tracker_cannot_be_repeated(cx: &mut TestAppContext) {
    let (form, peer, _, cx) = form_window(cx);
    name(&form, "demo", cx);
    *peer.submit_failure.lock().unwrap() = Some(ClientGitFailure::Unavailable);
    super::tests::create(cx);
    super::tests::create(cx);
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
    assert!(form.read_with(cx, |form, _| form.is_open()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|window, cx| form.update(cx, |form, cx| assert!(form.present(window, cx))));
    cx.run_until_parked();
    super::tests::create(cx);
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
}

#[gpui::test]
fn reopening_from_the_native_journal_looks_up_the_original_instead_of_submitting(
    cx: &mut TestAppContext,
) {
    let (form, peer, _, cx) = form_window(cx);
    name(&form, "demo", cx);
    super::tests::create(cx);
    let original = completed(&peer);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let mut target = form.read_with(cx, |form, _| match &form.backend {
        FormBackend::Server(server) => server.target.clone(),
        _ => unreachable!(),
    });
    target.original_operation = Some(original);
    let reopened = cx.update(|window, cx| {
        let store = cx.new(super::super::server_git::ServerGitStore::new);
        store.update(cx, |store, cx| {
            store
                .connect(
                    GitClient::new(ServerId::new([9; 16]), peer.clone()).unwrap(),
                    ServerEpoch::new(7),
                    cx,
                )
                .unwrap()
        });
        let requests = store.read(cx).requests(ServerId::new([9; 16])).unwrap();
        let reopened = cx.new(|cx| WorktreeForm::new_server(requests, target, window, cx).unwrap());
        reopened.update(cx, |form, cx| assert!(form.present(window, cx)));
        reopened
    });
    cx.run_until_parked();
    super::tests::create(cx);
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
    assert_eq!(*peer.lookups.lock().unwrap(), [original]);
    assert!(!reopened.read_with(cx, |form, _| form.is_open()));
}

#[gpui::test]
fn dismissal_cancels_outcome_observation_without_cancelling_the_admitted_write(
    cx: &mut TestAppContext,
) {
    let (form, peer, events, cx) = form_window(cx);
    name(&form, "demo", cx);
    let (send, recv) = async_channel::bounded(1);
    *peer.state_gate.lock().unwrap() = Some(recv);
    super::tests::create(cx);
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        send.try_send(()).is_err(),
        "dismissal must drop the read observer"
    );
    assert_eq!(peer.cancelled_observations.load(Ordering::SeqCst), 1);
    completed(&peer);
    cx.run_until_parked();
    assert!(!form.read_with(cx, |form, _| form.is_open()));
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
    assert!(
        !events
            .borrow()
            .iter()
            .any(|event| matches!(event, WorktreeFormEvent::ServerCreated { .. }))
    );
}
