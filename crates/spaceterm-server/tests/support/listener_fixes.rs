use super::*;

#[tokio::test]
async fn production_account_advertises_installed_git_and_directory_providers_in_both_majors() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for major in [1, 2] {
            let seed = Seed::new().await;
            let supervisor = seed.supervisor(Arc::new(test_support::StartupAbsence));
            let startup = AccountStartup::admit(&supervisor).unwrap();
            let server = RecoveredAccountServer::recover(supervisor, startup, seed.launch())
                .await
                .unwrap();
            let handle = server.handle();
            let run = tokio::spawn(server.run_until(std::future::pending()));
            while handle.phase() != ServicePhase::Serving {
                tokio::task::yield_now().await;
            }
            let transport = IpcTransport::connect(
                &seed.registration.endpoint().unwrap(),
                MessageLimit::envelope(),
            )
            .await
            .unwrap();
            let codec = handshake(
                &transport,
                seed.identity.pin(),
                seed.principal,
                seed.key.clone(),
                major,
            )
            .await;
            for capability in [
                spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
                spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
            ] {
                assert!(
                    codec.capabilities().contains(&capability),
                    "production provider capability {capability} missing in major {major}"
                );
            }
            transport.close().await.unwrap();
            stop(&seed, handle.snapshot().epoch(), Vec::new(), None).await;
            run.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}

async fn receive_native(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
) -> spaceterm_core::native_requests::NativeRequest {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let bytes = transport
                .recv()
                .await
                .unwrap()
                .expect("production connection remains open");
            let payload = codec.decode(&bytes).unwrap();
            let received = spaceterm_core::observations::MonotonicTick::new(0);
            match payload {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::NativeRequest(value)),
                }) => {
                    return spaceterm_protocol::convert::native_requests::v1::decode_request(
                        &value, received,
                    )
                    .unwrap();
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::NativeRequest(value)),
                }) => {
                    return spaceterm_protocol::convert::native_requests::v2::decode_request(
                        &value, received,
                    )
                    .unwrap();
                }
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::RuntimeOutcome(value)),
                }) => {
                    assert!(
                        value.failure.is_none(),
                        "native work was denied instead of delivered: {:?}",
                        value.failure
                    );
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::RuntimeOutcome(value)),
                }) => {
                    assert!(
                        value.failure.is_none(),
                        "native work was denied instead of delivered: {:?}",
                        value.failure
                    );
                }
                _ => {}
            }
        }
    })
    .await
    .expect("native request must reach the production client")
}

async fn complete_native(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
    request: &spaceterm_core::native_requests::NativeRequest,
    kind: spaceterm_core::native_requests::NativeCompletionKind,
) {
    complete_native_expected(transport, codec, request, kind, None).await;
}

async fn complete_native_expected(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
    request: &spaceterm_core::native_requests::NativeRequest,
    kind: spaceterm_core::native_requests::NativeCompletionKind,
    expected_failure: Option<i32>,
) {
    let completion = spaceterm_core::native_requests::NativeCompletion::new(request, kind).unwrap();
    let payload = match codec.major() {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::NativeCompletion(
                spaceterm_protocol::convert::native_requests::v1::encode_completion(&completion)
                    .unwrap(),
            )),
        }),
        _ => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::NativeCompletion(
                spaceterm_protocol::convert::native_requests::v2::encode_completion(&completion)
                    .unwrap(),
            )),
        }),
    };
    transport
        .send(&codec.encode(&payload).unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let payload = codec
                .decode(&transport.recv().await.unwrap().unwrap())
                .unwrap();
            match payload {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::RuntimeOutcome(value)),
                }) if value.request == request.id().get() => {
                    assert_eq!(
                        value.failure.map(|failure| failure.classification),
                        expected_failure
                    );
                    return;
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::RuntimeOutcome(value)),
                }) if value.request == request.id().get() => {
                    assert_eq!(
                        value.failure.map(|failure| failure.classification),
                        expected_failure
                    );
                    return;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}

async fn shell_line(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    session: SessionKey,
    text: &str,
) {
    let access = handle.runtime_access(principal, session).await.unwrap();
    let effect_fence = fence(access.authority, None);
    handle
        .runtime_command(
            principal,
            FencedRuntimeCommand {
                fence: effect_fence,
                command: RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input(text)),
            },
        )
        .await
        .unwrap();
    let mut enter = spaceterm_terminal::KeyInput::text_input("");
    enter.physical_key = spaceterm_terminal::PhysicalKey::Enter;
    enter.logical_key = "enter".into();
    enter.text = None;
    handle
        .runtime_command(
            principal,
            FencedRuntimeCommand {
                fence: effect_fence,
                command: RuntimeCommand::Key(enter),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn production_account_delivers_unsafe_paste_clipboard_and_notifications_in_both_majors() {
    use spaceterm_core::native_requests::{
        NativeCompletionKind, NativeRequestKind, NativeText, NotificationNotice, PasteDecision,
    };
    tokio::time::timeout(Duration::from_secs(45), async {
        for major in [1, 2] {
            let seed = Seed::new().await;
            let supervisor = seed.supervisor(Arc::new(test_support::StartupAbsence));
            let startup = AccountStartup::admit(&supervisor).unwrap();
            let server = RecoveredAccountServer::recover(supervisor, startup, seed.launch()).await.unwrap();
            let handle = server.handle();
            let run = tokio::spawn(server.run_until(std::future::pending()));
            while handle.phase() != ServicePhase::Serving { tokio::task::yield_now().await; }
            let transport = IpcTransport::connect(&seed.registration.endpoint().unwrap(), MessageLimit::envelope()).await.unwrap();
            let mut codec = handshake(&transport, seed.identity.pin(), seed.principal, seed.key.clone(), major).await;
            let connection = ConnectionGeneration::new(reconcile(&transport, &mut codec, &handle).await);
            let principal = VerifiedPrincipal::from_authenticated(&codec, connection).unwrap();
            let (_, pane, grant) = create(&seed, &transport, &mut codec).await;
            let generation = handle.snapshot().runtime().iter().find(|v| v.pane == pane).unwrap().session_generation;
            let session = start_shell(&seed, &transport, &mut codec, principal.epoch(), generation, connection, grant).await;
            let pid = wait_pid(&seed).await;
            handle.update_client_focus(principal, ClientFocusUpdate { context: ContextGeneration::new(2), focused_pane: Some(pane), terminal_input_focus: true, native_effects_eligible: true }).await.unwrap();
            let effect_fence = fence(handle.runtime_access(principal, session).await.unwrap().authority, None);
            macro_rules! subscription {
                ($p:ident, $variant:ident) => { Payload::$variant($p::Message { kind: Some($p::message::Kind::PaneSubscription($p::PaneSubscription {
                    request: 9, request_generation: 1, fence: Some(spaceterm_protocol::convert::identity::encode_fence(effect_fence)), cancel: false,
                    limits: Some($p::PageLimits { rows: 4, cells: 1024, bytes: 16384 }),
                })) }) };
            }
            let subscription = if major == 1 { subscription!(v1, Major1) } else { subscription!(v2, Major2) };
            transport.send(&codec.encode(&subscription).unwrap()).await.unwrap();
            let paste = FencedRuntimeCommand { fence: effect_fence, command: RuntimeCommand::RequestPaste(BoundedPaste::new("echo production-paste\n".into()).unwrap()) };
            let paste = if major == 1 {
                Payload::Major1(v1::Message { kind: Some(v1::message::Kind::RuntimeCommand(spaceterm_protocol::convert::runtime::v1::encode_command(100, &paste).unwrap())) })
            } else {
                Payload::Major2(v2::Message { kind: Some(v2::message::Kind::RuntimeCommand(spaceterm_protocol::convert::runtime::v2::encode_command(100, &paste).unwrap())) })
            };
            transport.send(&codec.encode(&paste).unwrap()).await.unwrap();
            let approval = receive_native(&transport, &mut codec).await;
            let NativeRequestKind::UnsafePaste { approval: id, text } = approval.kind() else { panic!("unsafe paste approval"); };
            assert_eq!(text.as_str(), "echo production-paste\n");
            assert!(approval.deadline().unwrap().get() > 0);
            complete_native(&transport, &mut codec, &approval, NativeCompletionKind::Paste { approval: *id, decision: PasteDecision::Confirm }).await;
            shell_line(&handle, principal, session, r"printf '\033]52;c;c2VjcmV0\033\\'").await;
            let write = receive_native(&transport, &mut codec).await;
            assert!(matches!(write.kind(), NativeRequestKind::ClipboardWrite(text) if text.as_str() == "secret"));
            complete_native(&transport, &mut codec, &write, NativeCompletionKind::ClipboardWritten).await;
            shell_line(&handle, principal, session, r"printf '\033]7701;permissions=accessibility\033\\'").await;
            let permission = receive_native(&transport, &mut codec).await;
            assert!(matches!(permission.kind(), NativeRequestKind::Permission(notice) if notice.accessibility()));
            tokio::time::sleep(Duration::from_millis(501)).await;
            complete_native(&transport, &mut codec, &permission, NativeCompletionKind::PermissionAcknowledged).await;
            shell_line(&handle, principal, session, r"printf '\007'").await;
            let notice = receive_native(&transport, &mut codec).await;
            assert!(matches!(notice.kind(), NativeRequestKind::Notification(NotificationNotice::Bell)));
            complete_native(&transport, &mut codec, &notice, NativeCompletionKind::NotificationAcknowledged).await;
            shell_line(&handle, principal, session, r"printf '\033]52;c;?\033\\'").await;
            let read = receive_native(&transport, &mut codec).await;
            assert!(matches!(read.kind(), NativeRequestKind::ClipboardRead));
            complete_native(&transport, &mut codec, &read, NativeCompletionKind::ClipboardText(NativeText::new("public-fixture".into()).unwrap())).await;

            transport.close().await.unwrap();
            stop(&seed, session.epoch, vec![session], Some(pid)).await;
            run.await.unwrap().unwrap();
        }
    }).await.unwrap();
}

async fn protocol_shell_line(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
    effect_fence: EffectFence,
    request: u64,
    text: &str,
) {
    let mut enter = spaceterm_terminal::KeyInput::text_input("");
    enter.physical_key = spaceterm_terminal::PhysicalKey::Enter;
    enter.logical_key = "enter".into();
    enter.text = None;
    for (offset, key) in [spaceterm_terminal::KeyInput::text_input(text), enter]
        .into_iter()
        .enumerate()
    {
        let command = FencedRuntimeCommand {
            fence: effect_fence,
            command: RuntimeCommand::Key(key),
        };
        let request = request + offset as u64;
        let payload = match codec.major() {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::RuntimeCommand(
                    spaceterm_protocol::convert::runtime::v1::encode_command(request, &command)
                        .unwrap(),
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::RuntimeCommand(
                    spaceterm_protocol::convert::runtime::v2::encode_command(request, &command)
                        .unwrap(),
                )),
            }),
        };
        transport
            .send(&codec.encode(&payload).unwrap())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn production_account_clipboard_read_expiry_through_production_factory_in_both_majors() {
    use spaceterm_core::native_requests::{NativeCompletionKind, NativeRequestKind, NativeText};

    tokio::time::timeout(Duration::from_secs(45), async {
        for major in [1, 2] {
            let seed = Seed::new().await;
            let supervisor = seed.supervisor(Arc::new(test_support::StartupAbsence));
            let startup = AccountStartup::admit(&supervisor).unwrap();
            let server = RecoveredAccountServer::recover(supervisor, startup, seed.launch())
                .await
                .unwrap();
            let handle = server.handle();
            let run = tokio::spawn(server.run_until(std::future::pending()));
            while handle.phase() != ServicePhase::Serving {
                tokio::task::yield_now().await;
            }
            let transport = IpcTransport::connect(
                &seed.registration.endpoint().unwrap(),
                MessageLimit::envelope(),
            )
            .await
            .unwrap();
            let mut codec = handshake(
                &transport,
                seed.identity.pin(),
                seed.principal,
                seed.key.clone(),
                major,
            )
            .await;
            let connection =
                ConnectionGeneration::new(reconcile(&transport, &mut codec, &handle).await);
            let principal = VerifiedPrincipal::from_authenticated(&codec, connection).unwrap();
            let (_, pane, grant) = create(&seed, &transport, &mut codec).await;
            let generation = handle
                .snapshot()
                .runtime()
                .iter()
                .find(|record| record.pane == pane)
                .unwrap()
                .session_generation;
            let session = start_shell(
                &seed,
                &transport,
                &mut codec,
                principal.epoch(),
                generation,
                connection,
                grant,
            )
            .await;
            let pid = wait_pid(&seed).await;
            handle
                .update_client_focus(
                    principal,
                    ClientFocusUpdate {
                        context: ContextGeneration::new(2),
                        focused_pane: Some(pane),
                        terminal_input_focus: true,
                        native_effects_eligible: true,
                    },
                )
                .await
                .unwrap();
            let effect_fence = fence(
                handle.runtime_access(principal, session).await.unwrap().authority,
                None,
            );
            let mut events = handle
                .subscribe_runtime(principal, effect_fence)
                .await
                .unwrap();
            macro_rules! subscription {
                ($p:ident, $variant:ident) => {
                    Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::PaneSubscription($p::PaneSubscription {
                            request: 9,
                            request_generation: 1,
                            fence: Some(spaceterm_protocol::convert::identity::encode_fence(
                                effect_fence,
                            )),
                            cancel: false,
                            limits: Some($p::PageLimits {
                                rows: 4,
                                cells: 1024,
                                bytes: 16384,
                            }),
                        })),
                    })
                };
            }
            let subscription = if major == 1 {
                subscription!(v1, Major1)
            } else {
                subscription!(v2, Major2)
            };
            transport
                .send(&codec.encode(&subscription).unwrap())
                .await
                .unwrap();

            // Input follows the subscription on the authenticated connection.
            // Observe the PTY's empty OSC 52 response, not just the protocol rejection.
            protocol_shell_line(
                &transport,
                &mut codec,
                effect_fence,
                100,
                r"stty -echo -icanon; printf '\033]52;c;?\033\\'; dd bs=1 count=9 2>/dev/null | od -An -tx1 | tr -d ' \n'; stty echo icanon; printf '\n'",
            )
            .await;
            let expired = receive_native(&transport, &mut codec).await;
            assert!(matches!(expired.kind(), NativeRequestKind::ClipboardRead));
            assert!((1..=1000).contains(&expired.deadline().unwrap().get()));
            tokio::time::sleep(Duration::from_millis(1050)).await;
            complete_native_expected(
                &transport,
                &mut codec,
                &expired,
                NativeCompletionKind::ClipboardText(NativeText::new("late-text".into()).unwrap()),
                Some(spaceterm_protocol::wire::common::FailureClass::Stale as i32),
            )
            .await;
            assert_output(
                &handle,
                principal,
                session,
                events.as_mut(),
                "1b5d35323b633b1b5c",
            )
            .await;

            // Expiry must not disconnect the client or poison the next clipboard read.
            protocol_shell_line(
                &transport,
                &mut codec,
                effect_fence,
                102,
                r"stty -echo -icanon; printf '\033]52;c;?\033\\'; dd bs=1 count=17 2>/dev/null | od -An -tx1 | tr -d ' \n'; stty echo icanon; printf '\n'",
            )
            .await;
            let fresh = receive_native(&transport, &mut codec).await;
            assert!(matches!(fresh.kind(), NativeRequestKind::ClipboardRead));
            assert_ne!(fresh.id(), expired.id());
            complete_native(
                &transport,
                &mut codec,
                &fresh,
                NativeCompletionKind::ClipboardText(NativeText::new("secret".into()).unwrap()),
            )
            .await;
            assert_output(
                &handle,
                principal,
                session,
                events.as_mut(),
                "1b5d35323b633b6332566a636d56301b5c",
            )
            .await;

            transport.close().await.unwrap();
            stop(&seed, session.epoch, vec![session], Some(pid)).await;
            run.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}

async fn directory_query(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
    context: spaceterm_core::server_directories::DirectoryContext,
    action: spaceterm_core::server_directories::DirectoryQueryAction,
) -> spaceterm_core::server_directories::DirectoryQueryResult {
    use spaceterm_core::server_directories::{DirectoryQuery, DirectoryQueryParts};
    use spaceterm_protocol::convert::directories;
    let request = directories::DirectoryRequest::new(
        200,
        3,
        DirectoryQuery::new(DirectoryQueryParts { context, action }).unwrap(),
    )
    .unwrap();
    let response = round_trip(
        transport,
        codec,
        directories::encode_query(codec.major(), &request).unwrap(),
    )
    .await;
    let response = directories::decode_response(&response, context).unwrap();
    assert_eq!(response.request, request.request);
    assert_eq!(response.generation, request.generation);
    response.result.unwrap()
}

#[tokio::test]
async fn production_account_queries_git_and_pages_repairs_owning_directories_over_ipc_and_wss() {
    use spaceterm_core::{
        server_directories::*,
        worktrees::{GitQuery, GitStatusTarget, queries::*},
    };
    use spaceterm_protocol::convert::git;
    tokio::time::timeout(Duration::from_secs(45), async {
        for major in [1, 2] {
            let seed = Seed::new().await;
            let output = std::process::Command::new("/usr/bin/git")
                .args(["init", "--quiet"])
                .current_dir(seed.root.path())
                .output()
                .unwrap();
            assert!(output.status.success());
            for n in 0..140 {
                fs::create_dir(seed.root.path().join(format!("choice-{n:03}"))).unwrap();
            }
            let supervisor = seed.supervisor(Arc::new(test_support::StartupAbsence));
            let startup = AccountStartup::admit(&supervisor).unwrap();
            let server = RecoveredAccountServer::recover(supervisor, startup, seed.launch())
                .await
                .unwrap();
            let handle = server.handle();
            let run = tokio::spawn(server.run_until(std::future::pending()));
            while handle.phase() != ServicePhase::Serving {
                tokio::task::yield_now().await;
            }
            let transport = IpcTransport::connect(
                &seed.registration.endpoint().unwrap(),
                MessageLimit::envelope(),
            )
            .await
            .unwrap();
            let mut codec = handshake(
                &transport,
                seed.identity.pin(),
                seed.principal,
                seed.key.clone(),
                major,
            )
            .await;
            let connection =
                ConnectionGeneration::new(reconcile(&transport, &mut codec, &handle).await);
            let principal = VerifiedPrincipal::from_authenticated(&codec, connection).unwrap();
            let context = DirectoryContext::new(DirectoryContextParts {
                server: principal.server(),
                epoch: principal.epoch(),
                pairing: principal.pairing(),
                client: principal.client(),
            })
            .unwrap();
            let DirectoryQueryResult::Home(home) =
                directory_query(&transport, &mut codec, context, DirectoryQueryAction::Home).await
            else {
                panic!("home directory");
            };
            assert_eq!(home.server(), principal.server());
            assert_eq!(
                home.bytes(),
                seed.root.path().as_os_str().as_encoded_bytes()
            );
            let mut cursor = None;
            let mut names = std::collections::BTreeSet::new();
            let mut pages = 0;
            let mut selected = None;
            loop {
                let DirectoryQueryResult::Page(page) = directory_query(
                    &transport,
                    &mut codec,
                    context,
                    DirectoryQueryAction::Browse {
                        directory: home.clone(),
                        cursor,
                    },
                )
                .await
                else {
                    panic!("directory page");
                };
                pages += 1;
                assert!(page.parts().entries.len() <= MAX_DIRECTORY_PAGE_ENTRIES);
                for entry in &page.parts().entries {
                    assert_eq!(entry.directory.server(), principal.server());
                    assert!(
                        names.insert(entry.name.bytes().to_vec()),
                        "no directory may repeat across pages"
                    );
                    if entry.name.bytes() == b"choice-002" {
                        selected = Some(entry.directory.clone());
                    }
                }
                cursor = page.parts().next.clone();
                if cursor.is_none() {
                    break;
                }
            }
            assert!(pages >= 2);
            assert_eq!(
                names.iter().filter(|n| n.starts_with(b"choice-")).count(),
                140
            );
            let selected = selected.unwrap();
            let DirectoryQueryResult::Validation(validation) = directory_query(
                &transport,
                &mut codec,
                context,
                DirectoryQueryAction::Validate {
                    directory: selected.clone(),
                    purpose: DirectoryPurpose::PaneRepair,
                },
            )
            .await
            else {
                panic!("repair validation");
            };
            assert_eq!(
                validation.class,
                DirectoryValidationClass::AvailableDirectory
            );
            let (workspace, pane, _) = create(&seed, &transport, &mut codec).await;
            let snapshot = handle.snapshot();
            let workspace_record = snapshot
                .state()
                .workspaces
                .iter()
                .find(|v| v.id == workspace)
                .unwrap();
            operation(
                &seed,
                &transport,
                &mut codec,
                2,
                Command::SetPinnedDirectory {
                    workspace,
                    expected_version: workspace_record.pinned_directory.version,
                    value: Some(home.clone()),
                },
            )
            .await;
            for (n, query) in [
                GitQuery::Status {
                    target: GitStatusTarget::Workspace(workspace),
                },
                GitQuery::Inventory {
                    workspace,
                    cursor: None,
                },
            ]
            .into_iter()
            .enumerate()
            {
                let request = GitQueryRequest::new(
                    GitCorrelation::new(300 + n as u64, 1).unwrap(),
                    query,
                    None,
                    false,
                )
                .unwrap();
                let reply = round_trip(
                    &transport,
                    &mut codec,
                    git::encode_query(major, &request).unwrap(),
                )
                .await;
                let result = git::decode_response(&reply, principal.server()).unwrap();
                assert_eq!(result.correlation, request.correlation);
                match result.result.unwrap() {
                    GitQueryResult::Status(_) if n == 0 => {}
                    GitQueryResult::Inventory(page) if n == 1 => assert!(!page.records.is_empty()),
                    result => panic!("unexpected Git query result: {result:?}"),
                }
            }
            let snapshot = handle.snapshot();
            let pane_record = snapshot
                .state()
                .panes
                .iter()
                .find(|v| v.id == pane)
                .unwrap();
            operation(
                &seed,
                &transport,
                &mut codec,
                3,
                Command::RepairPaneDirectory {
                    pane,
                    expected_version: pane_record.confirmed_restoration_directory.version,
                    value: selected.clone(),
                },
            )
            .await;
            let repaired = handle.snapshot();
            assert_eq!(
                repaired
                    .state()
                    .panes
                    .iter()
                    .find(|v| v.id == pane)
                    .unwrap()
                    .restoration_directory(),
                &selected
            );
            assert_eq!(
                repaired
                    .state()
                    .workspaces
                    .iter()
                    .find(|v| v.id == workspace)
                    .unwrap()
                    .pinned_directory
                    .value,
                Some(home.clone())
            );
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            drop(listener);
            operation(
                &seed,
                &transport,
                &mut codec,
                4,
                Command::SetRemoteAccess {
                    server: principal.server(),
                    expected_version: repaired
                        .state()
                        .retained
                        .as_ref()
                        .unwrap()
                        .remote_access()
                        .version,
                    value: RemoteAccessPolicy::enabled(principal.server(), vec![address]).unwrap(),
                },
            )
            .await;
            transport.close().await.unwrap();
            let remote = WssTransport::connect(
                address,
                "localhost",
                EnrolledServerKey(seed.identity.pin().public_key),
                MessageLimit::envelope(),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
            let mut codec = handshake(
                &remote,
                seed.identity.pin(),
                seed.principal,
                seed.key.clone(),
                major,
            )
            .await;
            reconcile(&remote, &mut codec, &handle).await;
            // WSS peers have no local account identity. Directory values remain
            // owning-server comparison data, never client-local file authority.
            assert_eq!(remote.peer_facts().local_uid, None);
            let DirectoryQueryResult::Home(remote_home) =
                directory_query(&remote, &mut codec, context, DirectoryQueryAction::Home).await
            else {
                panic!("remote home");
            };
            assert_eq!(remote_home, home);
            let DirectoryQueryResult::Validation(validation) = directory_query(
                &remote,
                &mut codec,
                context,
                DirectoryQueryAction::Validate {
                    directory: selected,
                    purpose: DirectoryPurpose::PaneRepair,
                },
            )
            .await
            else {
                panic!("remote repair directory");
            };
            assert_eq!(
                validation.class,
                DirectoryValidationClass::AvailableDirectory
            );
            remote.close().await.unwrap();
            stop(&seed, principal.epoch(), Vec::new(), None).await;
            run.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}
