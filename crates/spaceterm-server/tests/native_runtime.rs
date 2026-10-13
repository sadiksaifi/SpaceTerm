#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "support/job_a.rs"]
mod support;

use spaceterm_core::{
    authority::*, content::ContentBase, content_queries::*, operations::*, organization::*,
};
use spaceterm_host::shell_launch::ShellLaunchPlanner;
use spaceterm_server::{
    adapters::terminal::{
        AccountLaunchFacts, AccountLaunchProvider, NativeRuntime, NativeRuntimeClock,
    },
    api::*,
    contracts::*,
    service::RecoveredServer,
};
use spaceterm_terminal::{
    metadata::LocalMachine, runtime::budget::RuntimeLimits, runtime_contracts::*,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use support::*;

struct Account(PathBuf);
impl AccountLaunchProvider for Account {
    fn account(&self) -> Result<AccountLaunchFacts, ServerFailure> {
        let uid = std::process::Command::new("/usr/bin/id")
            .arg("-u")
            .output()
            .unwrap();
        Ok(AccountLaunchFacts {
            uid: std::str::from_utf8(&uid.stdout)
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
            home: self.0.clone(),
            planner: ShellLaunchPlanner::for_test(self.0.join("shell"), self.0.clone()),
            machine: LocalMachine::default(),
        })
    }
}

fn fence(authority: RuntimeAuthority, content: Option<ContentRevision>) -> EffectFence {
    let p = authority.parts();
    EffectFence::new(
        p.session,
        p.grant.unwrap(),
        p.connection.unwrap(),
        p.context,
        content,
    )
    .unwrap()
}

async fn output_contains(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    session: SessionKey,
    events: &mut dyn RuntimeStream<RuntimePresentationEvent>,
    expected: &str,
    columns: u16,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut base: Option<ContentBase> = None;
        let mut generation = 0;
        loop {
            if let Some(current) = base {
                generation += 1;
                let access = handle.runtime_access(principal, session).await.unwrap();
                let stamp = QueryStamp::new(
                    fence(access.authority, Some(current.revision)),
                    current,
                    ContentRequestId::new(generation).unwrap(),
                    RequestGeneration::new(generation),
                )
                .unwrap();
                let request = ContentPageRequest::new(
                    stamp,
                    ContentRange::ActiveGrid,
                    None,
                    PageLimits::new(128, 16384, 1024 * 1024).unwrap(),
                )
                .unwrap();
                match access.handle.read_page(request).unwrap().await.unwrap() {
                    ContentReadResult::Ready(reply) => {
                        let page = reply.page().parts();
                        assert_eq!(page.columns, columns);
                        let text = page
                            .rows
                            .iter()
                            .map(|row| {
                                row.cells()
                                    .iter()
                                    .map(|cell| cell.content().text())
                                    .collect::<String>()
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if text.contains(expected) {
                            break;
                        }
                    }
                    ContentReadResult::ResyncRequired { current, .. } => {
                        base = Some(current);
                        continue;
                    }
                    ContentReadResult::Rejected { .. } => panic!("semantic page was rejected"),
                }
            }
            if let RuntimePresentationEvent::ContentInvalidated(current) =
                events.next().await.unwrap().unwrap()
            {
                base = Some(current);
            }
        }
    })
    .await
    .expect("shell output must reach semantic rows");
}

async fn input(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    session: SessionKey,
    text: &str,
) {
    let access = handle.runtime_access(principal, session).await.unwrap();
    assert_eq!(
        handle
            .runtime_command(
                principal,
                FencedRuntimeCommand {
                    fence: fence(access.authority, None),
                    command: RuntimeCommand::InputMethodCommit(
                        BoundedInputMethodText::new(text.trim_end_matches('\n').into()).unwrap()
                    ),
                }
            )
            .await
            .unwrap(),
        EffectDisposition::Applied
    );
    if text.ends_with('\n') {
        let mut enter = spaceterm_terminal::KeyInput::text_input("");
        enter.physical_key = spaceterm_terminal::PhysicalKey::Enter;
        enter.logical_key = "enter".into();
        enter.text = None;
        assert_eq!(
            handle
                .runtime_command(
                    principal,
                    FencedRuntimeCommand {
                        fence: fence(access.authority, None),
                        command: RuntimeCommand::Key(enter),
                    }
                )
                .await
                .unwrap(),
            EffectDisposition::Applied,
        );
    }
}

async fn real_actor_runtime(end_all: bool) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap();
    // Record the PTY's first dimensions before any client resize or input can arrive.
    // The wrapper and the interactive shell both execute the real /bin/sh.
    std::fs::write(
        home.join("shell"),
        "#!/bin/sh\n/bin/stty size > initial-grid\nexec /bin/sh \"$@\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(home.join("shell"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut fixture = Fixture::new();
    fixture.db.lock().unwrap().recovered.organization.panes[0].starting_directory =
        ServerDirectory::new(server(), home.as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let clock = Arc::new(NativeRuntimeClock::default());
    let account = Arc::new(Account(home.clone()));
    let runtime = NativeRuntime::new(
        server(),
        account.account().unwrap().uid,
        account,
        clock.clone(),
        RuntimeLimits::default(),
    )
    .unwrap();
    fixture.config.launch = runtime.clone();
    fixture.config.cleanup = runtime.clone();
    fixture.dependencies.runtime = runtime;
    fixture.dependencies.clock = clock;
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    let session = tokio::time::timeout(
        Duration::from_secs(10),
        start(&handle, principal, &codec, grant),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while std::fs::read_to_string(home.join("initial-grid"))
            .unwrap_or_default()
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(home.join("initial-grid")).unwrap(),
        "37 113\n"
    );
    handle
        .update_client_focus(
            principal,
            ClientFocusUpdate {
                context: ContextGeneration::new(2),
                focused_pane: Some(pane()),
                terminal_input_focus: true,
                native_effects_eligible: true,
            },
        )
        .await
        .unwrap();
    let access = handle.runtime_access(principal, session).await.unwrap();
    let mut events = handle
        .subscribe_runtime(principal, fence(access.authority, None))
        .await
        .unwrap();
    input(
        &handle,
        principal,
        session,
        "printf 'START=%s\\n' \"$(/bin/stty size)\"; printf '%s' \"$$\" > runtime-pid\n",
    )
    .await;
    output_contains(
        &handle,
        principal,
        session,
        &mut *events,
        "START=37 113",
        113,
    )
    .await;
    let pid: i32 = std::fs::read_to_string(home.join("runtime-pid"))
        .unwrap()
        .parse()
        .unwrap();
    let resized = spaceterm_core::geometry::MeasuredGrid::new(
        29,
        97,
        spaceterm_core::geometry::CellPixels::new(8, 16).unwrap(),
        spaceterm_core::geometry::SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let access = handle.runtime_access(principal, session).await.unwrap();
    assert_eq!(
        handle
            .runtime_command(
                principal,
                FencedRuntimeCommand {
                    fence: fence(access.authority, None),
                    command: RuntimeCommand::Resize(resized),
                }
            )
            .await
            .unwrap(),
        EffectDisposition::Applied
    );
    input(
        &handle,
        principal,
        session,
        "printf 'RESIZE=%s\\n' \"$(/bin/stty size)\"\n",
    )
    .await;
    output_contains(
        &handle,
        principal,
        session,
        &mut *events,
        "RESIZE=29 97",
        97,
    )
    .await;
    drop(events);
    input(
        &handle,
        principal,
        session,
        "printf 'DETACHED=%s\\n' alive\n",
    )
    .await;
    let access = handle.runtime_access(principal, session).await.unwrap();
    let mut events = handle
        .subscribe_runtime(principal, fence(access.authority, None))
        .await
        .unwrap();
    output_contains(
        &handle,
        principal,
        session,
        &mut *events,
        "DETACHED=alive",
        97,
    )
    .await;
    if end_all {
        let end_request = request(
            &codec,
            1,
            4,
            Command::EndAllSessions {
                server: support::server(),
                assessment: AssessmentRef {
                    operation: operation(1, 3),
                    digest: RequestDigest::from_bytes([5; 32]),
                },
            },
        );
        let end_binding = end_request.binding();
        handle.submit(principal, end_request).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut snapshots = handle.subscribe_snapshot();
            loop {
                if snapshots.borrow().runtime()[0].phase
                    == (SessionPhase::Ended {
                        reason: EndReason::EndAllSessions,
                    })
                {
                    break;
                }
                snapshots.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let OperationLookup::Completed {
                    result: Outcome::Applied,
                    ..
                } = handle.lookup_state(principal, end_binding).await.unwrap()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let process = std::process::Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "pid="])
            .output()
            .unwrap();
        assert!(
            process.stdout.is_empty(),
            "End All must reap before Stop runs"
        );
        assert_eq!(
            handle.snapshot().runtime()[0].control,
            ControlRecord::Held {
                client: principal.client(),
                generation: grant.generation()
            }
        );
    }
    let report = stop(&handle, host).await.unwrap();
    assert!(report.cleanup_confirmed);
    server.wait().await.unwrap();
    // An independently queried process table must no longer contain the shell.
    let process = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .unwrap();
    assert!(
        process.stdout.is_empty(),
        "shell must be reaped, not merely disconnected"
    );
    let db = db.lock().unwrap();
    use spaceterm_core::{
        operation_ledger_contracts::{ExternalProgressState, LedgerWrite},
        retirement::{NativeRetirementMutation, RetirementCleanupState},
        storage::OperationWrite,
    };
    let expected_reason = if end_all {
        RetirementReason::EndAllSessions
    } else {
        RetirementReason::Stop
    };
    let confirmed = db
        .transactions
        .iter()
        .flat_map(|t| &t.operation_writes)
        .any(|write| {
            let sessions = match write {
                OperationWrite::NativeRetirement(NativeRetirementMutation::Progress {
                    next,
                    ..
                }) => next.sessions(),
                OperationWrite::Ledger(write) => match &**write {
                    LedgerWrite::RecordExternalProgress { record, .. } => {
                        match record.external_progress.as_ref().map(|v| v.state()) {
                            Some(ExternalProgressState::AcceptedRetirement(descriptor)) => {
                                descriptor.sessions()
                            }
                            _ => return false,
                        }
                    }
                    _ => return false,
                },
                _ => return false,
            };
            sessions.iter().any(|v| {
                v.session() == session
                    && v.first_reason() == expected_reason
                    && matches!(v.cleanup(), RetirementCleanupState::Confirmed(_))
            })
        });
    assert!(
        confirmed,
        "the exact Session and first reason require committed positive cleanup evidence"
    );
    assert_eq!(handle.snapshot().state().panes.len(), 1);
    assert_eq!(
        handle.snapshot().runtime()[0].phase,
        SessionPhase::Ended {
            reason: EndReason::Stop
        }
    );
}

#[tokio::test]
async fn actor_runs_real_shell_at_measured_geometry_resizes_and_stop_reaps_exact_session() {
    real_actor_runtime(false).await;
}

#[tokio::test]
async fn actor_end_all_reaps_real_shell_without_deleting_organization() {
    real_actor_runtime(true).await;
}

fn measured_request(path: &std::path::Path) -> RuntimeStartRequest {
    use spaceterm_core::{control::*, geometry::*};
    let old_session = SessionKey {
        pane: pane(),
        epoch: ServerEpoch::new(1),
        generation: SessionGeneration::new(1),
    };
    let session = SessionKey {
        generation: SessionGeneration::new(2),
        ..old_session
    };
    let grant = ControlGrant::new(pane(), client(1), ControlGeneration::new(1)).unwrap();
    let connection = ConnectionBinding {
        client: client(1),
        generation: ConnectionGeneration::new(1),
    };
    let context = ContextGeneration::new(1);
    let fence = EffectFence::new(old_session, grant, connection, context, None).unwrap();
    let intent =
        StartupIntent::new(operation(1, 1), fence, StartupSource::ExplicitSelection).unwrap();
    let grid = MeasuredGrid::new(
        31,
        103,
        CellPixels::new(7, 15).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let reservation = MeasuredStartupReservation::new(
        intent,
        MeasuredStartupMeasurement {
            intent: intent.id(),
            fence,
            grid,
        },
        session,
    )
    .unwrap();
    RuntimeStartRequest::new(
        reservation,
        ServerDirectory::new(
            support::server(),
            path.as_os_str().as_encoded_bytes().to_vec(),
        )
        .unwrap(),
        RuntimeAuthority::new(RuntimeAuthorityParts {
            session,
            grant: Some(grant),
            connection: Some(connection),
            context,
            focused_pane: true,
            terminal_input_focus: true,
            native_effects_eligible: true,
        })
        .unwrap(),
        appearance(),
    )
    .unwrap()
}

fn runtime(root: &std::path::Path) -> Arc<NativeRuntime> {
    let account = Arc::new(Account(root.to_owned()));
    NativeRuntime::new(
        server(),
        account.account().unwrap().uid,
        account,
        Arc::new(NativeRuntimeClock::default()),
        RuntimeLimits::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn cancelled_prepared_launch_retains_exact_absence_and_never_creates_a_process() {
    use spaceterm_core::retirement::{CleanupResult, ConfirmedCleanupClass};
    use spaceterm_server::adapters::terminal::{
        LaunchGate, LaunchPreparationPort, RuntimeCleanupPort,
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let request = measured_request(&path);
    let runtime = runtime(&path);
    let prepared = runtime.prepare(&request).await.unwrap();
    let gate = Arc::new(LaunchGate::default());
    gate.cancel();
    let owner = prepared
        .start(runtime.clone(), request.clone(), gate.clone())
        .await
        .unwrap();
    assert!(!gate.process_admitted());
    assert_eq!(
        owner
            .lifecycle_snapshot()
            .terminal_result()
            .unwrap()
            .status(),
        RuntimeTerminalStatus::StartupFailed {
            class: FailureClass::StaleAuthority
        }
    );
    let cleanup = runtime.abandon(owner).await.unwrap();
    assert_eq!(cleanup.session(), request.reservation().session());
    assert_eq!(
        cleanup.result(),
        CleanupResult::Confirmed(ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership)
    );
}

#[tokio::test]
async fn abandoning_a_real_owner_joins_cleanup_despite_retained_handles_and_subscriptions() {
    use spaceterm_core::retirement::{CleanupResult, ConfirmedCleanupClass};
    use spaceterm_server::adapters::terminal::RuntimeCleanupPort;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    std::fs::write(
        path.join("shell"),
        "#!/bin/sh\nprintf '%s' \"$$\" > runtime-pid\nexec /bin/sh \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(path.join("shell"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let request = measured_request(&path);
    let runtime = runtime(&path);
    let owner = runtime.start(&request).await.unwrap();
    assert_eq!(owner.lifecycle_snapshot(), RuntimeLifecycle::Running);
    let inert_handle = owner.handle();
    let mut inert_subscription = owner.subscribe(fence(request.authority(), None)).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while std::fs::read_to_string(path.join("runtime-pid"))
            .unwrap_or_default()
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(path.join("runtime-pid")).unwrap();
    let cleanup = runtime.abandon(owner).await.unwrap();
    assert_eq!(cleanup.session(), request.reservation().session());
    assert_eq!(cleanup.first_reason(), RetirementReason::RuntimeFailure);
    assert_eq!(
        cleanup.result(),
        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased)
    );
    let process = std::process::Command::new("/bin/ps")
        .args(["-p", &pid, "-o", "pid="])
        .output()
        .unwrap();
    assert!(process.stdout.is_empty());
    assert!(
        inert_handle
            .try_submit(FencedRuntimeCommand {
                fence: fence(request.authority(), None),
                command: RuntimeCommand::FocusReport(true)
            })
            .is_err()
    );
    assert!(matches!(
        inert_subscription.next().await,
        Some(Err(ServerFailure {
            class: ServerFailureClass::StaleAuthority,
            ..
        }))
    ));
}
