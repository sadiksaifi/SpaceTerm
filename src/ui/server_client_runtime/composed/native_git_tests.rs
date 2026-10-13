use super::*;
use gpui::TestAppContext;
use spaceterm_client::{
    journal::{ClientJournalChange, JournalController, JournalLimits},
    projection::ProjectionBudgets,
    testing::{FakePaneConnection, FakeServer},
};
use spaceterm_core::worktrees::{
    GitQuery, GitStatusTarget,
    queries::{GitInventoryPage, GitQueryResponse, GitQueryResult, GitStatusFacts},
};
use spaceterm_protocol::{codec::Payload, convert::git};
use std::sync::atomic::{AtomicU64, Ordering};

// Reuse the authenticated worker fixtures instead of simulating provider publications.
#[path = "../../../../crates/spaceterm-client/tests/operation_recovery/support.rs"]
mod durable;
#[path = "../../../../crates/spaceterm-client/tests/support/mod.rs"]
mod wire;

static REVISION: AtomicU64 = AtomicU64::new(1);

fn reply(payload: &Payload, target: &spaceterm_client::api::ConnectionTarget) -> Option<Payload> {
    let context = spaceterm_core::server_directories::DirectoryContext::new(
        spaceterm_core::server_directories::DirectoryContextParts {
            server: target.server(),
            pairing: target.pairing(),
            client: target.client(),
            epoch: ServerEpoch::new(1),
        },
    )
    .unwrap();
    if let Ok(request) = spaceterm_protocol::convert::directories::decode_query(payload, context) {
        return Some(
            spaceterm_protocol::convert::directories::encode_response(
                payload.major(),
                &spaceterm_protocol::convert::directories::DirectoryResponse {
                    request: request.request,
                    generation: request.generation,
                    result: Ok(
                        spaceterm_core::server_directories::DirectoryQueryResult::Home(
                            spaceterm_core::organization::ServerDirectory::new(
                                target.server(),
                                b"/server/home".to_vec(),
                            )
                            .unwrap(),
                        ),
                    ),
                },
                context,
            )
            .unwrap(),
        );
    }
    let request = git::decode_query(payload, target.server()).ok()?;
    let revision = if target.server().bytes()[0] == 1 {
        REVISION.load(Ordering::SeqCst)
    } else {
        1
    };
    let result = match request.query {
        GitQuery::Inventory { .. } => GitQueryResult::Inventory(GitInventoryPage {
            version: OrganizationRevision::new(revision),
            records: vec![],
            next: None,
        }),
        GitQuery::Status { .. } => GitQueryResult::Status(GitStatusFacts {
            revision: std::num::NonZeroU64::new(revision).unwrap(),
            branch: None,
            changes: vec![],
            dirty: false,
            unchecked: false,
            pull_request: None,
            upstream: None,
            operation: None,
            counts: None,
        }),
        _ => return None,
    };
    Some(
        git::encode_response(
            payload.major(),
            &GitQueryResponse {
                correlation: request.correlation,
                scope: None,
                result: Ok(result),
            },
            target.server(),
        )
        .unwrap(),
    )
}

fn drive(runtime: &tokio::runtime::Runtime, cx: &mut TestAppContext) {
    for _ in 0..20 {
        cx.run_until_parked();
        runtime.block_on(wire::settle());
    }
    cx.run_until_parked();
}

#[gpui::test]
fn desktop_native_git_publications_reach_unattached_interests_and_disconnect_independently(
    cx: &mut TestAppContext,
) {
    use super::tests::{Invalidator, MemoryView};
    use wire::peer::{PeerControl, PeerPlan, Peers};
    REVISION.store(1, Ordering::SeqCst);
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let bindings = vec![wire::binding(1), wire::binding(2)];
    let mut plans = BTreeMap::new();
    for binding in &bindings {
        let mut plan = PeerPlan::new(vec![if binding.target().server().bytes()[0] == 1 {
            2
        } else {
            1
        }]);
        plan.capabilities.extend([
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
        ]);
        plan.capabilities.sort_unstable();
        plan.query_handler = Some(reply);
        plans.insert(binding.target().server(), Arc::new(plan));
    }
    let (registry, runtime, journal, channels) = executor.block_on(async {
        let codec = Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec);
        let journal = Arc::new(
            JournalController::recover(
                Arc::new(durable::MemoryJournal::new(codec.clone())),
                codec,
                Arc::new(durable::Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        journal
            .transact(
                bindings
                    .iter()
                    .cloned()
                    .map(|binding| {
                        ClientJournalChange::SaveBinding(Box::new(durable::saved(binding)))
                    })
                    .collect(),
            )
            .await
            .unwrap();
        let mut services = wire::services(bindings.clone(), Arc::new(Peers(plans.clone())));
        services.journal = journal.clone();
        services.additional_capabilities = vec![
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
        ];
        let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
        for binding in &bindings {
            registry
                .server(binding.target().server())
                .unwrap()
                .connect()
                .await
                .unwrap();
        }
        wire::settle().await;
        let fake = FakeServer::new(77, vec![2, 1]).unwrap();
        let pane = FakePaneConnection::new(fake, 1, 2).await.unwrap();
        let channels = pane
            .channels(ProjectionBudgets::supported().unwrap())
            .unwrap();
        (registry, runtime, journal, channels)
    });
    let mut persistence = ViewPersistence::new(MemoryView::default());
    persistence.load().unwrap();
    let servers = bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| DesktopServerInfo {
            server: binding.target().server(),
            name: DisplayName::unnamed(),
            local: index == 0,
        })
        .collect();
    let provider = cx.update(|cx| {
        let (notifier, _) = super::super::notification_channel();
        ComposedDesktopClientProvider::new(
            DesktopClientComposition {
                registry,
                runtime,
                channels: channels.clone(),
                tracker: OperationTracker::new(journal.clone()),
                operations: Arc::new(durable::Peer::default()),
                pending: PendingTasks::new(journal, Arc::new(Invalidator(channels))),
                persistence,
                view: ClientViewState::new(),
                executor: executor.handle().clone(),
                servers,
                settings: Default::default(),
            },
            notifier,
            cx,
        )
        .unwrap()
    });
    let store = cx.update(|cx| crate::ui::server_git::InstalledServerGit::store(cx).unwrap());
    let interests = bindings
        .iter()
        .flat_map(|binding| {
            [1, 2]
                .into_iter()
                .map(move |id| WorkspaceId::new(binding.target().server(), id).unwrap())
        })
        .map(|workspace| {
            cx.update(|cx| {
                store.update(cx, |git, cx| git.register_inventory(workspace, cx).unwrap())
            })
        })
        .collect::<Vec<_>>();
    assert!(provider.inner.state.borrow().view.attachments().is_empty());
    drive(&executor, cx);
    for interest in &interests {
        let view = cx.update(|cx| store.read(cx).inventory(*interest).unwrap());
        assert_eq!(view.version, Some(OrganizationRevision::new(1)));
        assert_eq!(
            view.freshness,
            spaceterm_client::git::replica::GitFreshness::Current
        );
    }
    let statuses = bindings
        .iter()
        .map(|binding| {
            let workspace = WorkspaceId::new(binding.target().server(), 1).unwrap();
            cx.update(|cx| {
                store.update(cx, |git, cx| {
                    git.register_status(workspace, GitStatusTarget::Workspace(workspace), cx)
                        .unwrap()
                })
            })
        })
        .collect::<Vec<_>>();
    drive(&executor, cx);
    for interest in &statuses {
        let view = cx.update(|cx| store.read(cx).status(*interest).unwrap());
        assert_eq!(
            view.freshness,
            spaceterm_client::git::replica::GitFreshness::Current
        );
        assert!(view.facts.is_some());
    }

    // Execute a native directory query from GPUI, without a thread-local Tokio context.
    for binding in &bindings {
        let port = provider
            .directory_connection(binding.target().server())
            .unwrap();
        let directory =
            spaceterm_client::directories::DirectoryClient::new(binding.target().server(), port)
                .unwrap();
        let task = cx
            .foreground_executor()
            .spawn(async move { directory.home().await });
        let home = super::tests::finish(task, &executor, cx).unwrap();
        assert_eq!(home.bytes(), b"/server/home");
        assert_eq!(home.server(), binding.target().server());
    }

    REVISION.store(2, Ordering::SeqCst);
    executor.block_on(
        plans[&bindings[0].target().server()].control(PeerControl::Gap {
            base: 1,
            revision: 2,
        }),
    );
    drive(&executor, cx);
    for interest in &interests[..2] {
        let view = cx.update(|cx| store.read(cx).inventory(*interest).unwrap());
        assert_eq!(view.version, Some(OrganizationRevision::new(2)));
    }
    executor.block_on(plans[&bindings[0].target().server()].control(PeerControl::Disconnect));
    drive(&executor, cx);
    for interest in &interests[..2] {
        let view = cx.update(|cx| store.read(cx).inventory(*interest).unwrap());
        assert_ne!(
            view.freshness,
            spaceterm_client::git::replica::GitFreshness::Current
        );
        assert_eq!(view.version, Some(OrganizationRevision::new(2)));
    }
    for interest in &interests[2..] {
        let view = cx.update(|cx| store.read(cx).inventory(*interest).unwrap());
        assert_eq!(
            view.freshness,
            spaceterm_client::git::replica::GitFreshness::Current
        );
    }
    assert_eq!(
        cx.update(|cx| store.read(cx).status(statuses[0]).unwrap().freshness),
        spaceterm_client::git::replica::GitFreshness::LastKnown
    );
    assert_eq!(
        cx.update(|cx| store.read(cx).status(statuses[1]).unwrap().freshness),
        spaceterm_client::git::replica::GitFreshness::Current
    );
    let handle = provider.inner.sources[&bindings[0].target().server()]
        .handle
        .as_ref()
        .unwrap();
    executor.block_on(handle.connect()).unwrap();
    drive(&executor, cx);
    for interest in &interests[..2] {
        let view = cx.update(|cx| store.read(cx).inventory(*interest).unwrap());
        assert_eq!(
            view.freshness,
            spaceterm_client::git::replica::GitFreshness::Current
        );
        assert_eq!(view.version, Some(OrganizationRevision::new(2)));
    }
    assert_eq!(
        cx.update(|cx| store.read(cx).status(statuses[0]).unwrap().freshness),
        spaceterm_client::git::replica::GitFreshness::Current
    );
    assert_eq!(
        plans[&bindings[1].target().server()]
            .opens
            .load(Ordering::SeqCst),
        1
    );
    executor
        .block_on(
            provider
                .inner
                .runtime
                .borrow_mut()
                .take()
                .unwrap()
                .shutdown(),
        )
        .unwrap();
    // Drain the connection-lost publications before tearing down the native executor.
    drive(&executor, cx);
    for interest in &statuses {
        assert_eq!(
            cx.update(|cx| store.read(cx).status(*interest).unwrap().freshness),
            spaceterm_client::git::replica::GitFreshness::LastKnown
        );
    }
}
