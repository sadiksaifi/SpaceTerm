#[expect(dead_code, reason = "authentication fixture shared with other suites")]
mod support;

use spaceterm_core::{
    authority::{FieldVersion, ServerEpoch},
    identity::{ClientId, PairingId, PaneId, RepositoryId, ServerId, WorkspaceId, WorktreeId},
    operations::RepositoryScope,
    organization::{BranchName, ServerDirectory},
    worktrees::{queries::*, *},
};
use spaceterm_protocol::{ProtocolError, convert::git};

fn server() -> ServerId {
    ServerId::new([1; 16])
}
fn scope() -> RepositoryScope {
    RepositoryScope {
        server: server(),
        repository: RepositoryId::new(server(), 1).unwrap(),
    }
}
fn correlation() -> GitCorrelation {
    GitCorrelation::new(1, 2).unwrap()
}
fn token(kind: GitAssessmentKind) -> GitAssessmentToken {
    GitAssessmentToken::new(GitAssessmentTokenParts {
        server: server(),
        epoch: ServerEpoch::new(7),
        pairing: PairingId::new(server(), 3).unwrap(),
        client: ClientId::new(server(), 4).unwrap(),
        kind,
        random: [9; 32],
    })
    .unwrap()
}

#[test]
fn qualified_queries_preserve_targets_and_creation_choices_in_both_majors() {
    let mut queries = vec![
        GitQuery::Status {
            target: GitStatusTarget::Pane(PaneId::new(server(), 1).unwrap()),
        },
        GitQuery::Status {
            target: GitStatusTarget::Worktree(WorktreeId::new(server(), 8).unwrap()),
        },
        GitQuery::Inventory {
            workspace: WorkspaceId::new(server(), 2).unwrap(),
            cursor: None,
        },
        GitQuery::Branches {
            repository: scope(),
            cursor: None,
        },
        GitQuery::AssessRemove {
            worktree: WorktreeId::new(server(), 8).unwrap(),
        },
        GitQuery::AssessmentPage {
            assessment: token(GitAssessmentKind::Remove),
            cursor: None,
        },
    ];
    for branch in [
        WorktreeBranchChoice::New {
            name: BranchName::new("new".into()).unwrap(),
            base: BranchReference::Local(BranchName::new("main".into()).unwrap()),
        },
        WorktreeBranchChoice::Existing {
            name: BranchName::new("existing".into()).unwrap(),
        },
        WorktreeBranchChoice::Remote {
            remote: BranchName::new("origin".into()).unwrap(),
            name: BranchName::new("feature".into()).unwrap(),
        },
    ] {
        for location in [
            WorktreeLocationChoice::ServerTemplate {
                expected_version: FieldVersion::new(5),
            },
            WorktreeLocationChoice::Explicit(
                ServerDirectory::new(server(), b"/server/only".to_vec()).unwrap(),
            ),
        ] {
            queries.push(GitQuery::PreviewCreate {
                request: WorktreeCreateDraft::new(scope(), branch.clone(), location).unwrap(),
            });
        }
    }
    for major in [1, 2] {
        for query in &queries {
            let request = GitQueryRequest::new(correlation(), query.clone(), None, false).unwrap();
            let wire = git::encode_query(major, &request).unwrap();
            assert_eq!(git::decode_query(&wire, server()).unwrap(), request);
        }
    }
}

#[test]
fn typed_query_failures_are_not_collapsed_into_mutation_outcomes() {
    for major in [1, 2] {
        for failure in [
            GitFailure::MainWorktree,
            GitFailure::LockedWorktree,
            GitFailure::HoldsPinnedDirectory,
            GitFailure::IndeterminateDurability,
        ] {
            let response = GitQueryResponse {
                correlation: correlation(),
                scope: None,
                result: Err(failure),
            };
            assert_eq!(
                git::decode_response(
                    &git::encode_response(major, &response, server()).unwrap(),
                    server()
                )
                .unwrap(),
                response
            );
        }
    }
}

#[test]
fn wrong_server_and_wrong_confirmation_kind_reject_before_dispatch() {
    let request = GitQueryRequest::new(
        correlation(),
        GitQuery::Status {
            target: GitStatusTarget::Pane(PaneId::new(server(), 1).unwrap()),
        },
        None,
        false,
    )
    .unwrap();
    for major in [1, 2] {
        assert_eq!(
            git::decode_query(
                &git::encode_query(major, &request).unwrap(),
                ServerId::new([2; 16])
            ),
            Err(ProtocolError::Malformed)
        );
    }
    let command = GitCommand::CreateConfirmed {
        assessment: token(GitAssessmentKind::Remove),
    };
    assert_eq!(
        git::v1::encode_command(&command),
        Err(ProtocolError::Malformed)
    );
    assert_eq!(
        git::v2::encode_command(&command),
        Err(ProtocolError::Malformed)
    );
}

fn capable_connections(
    major: u32,
) -> (
    spaceterm_protocol::connection::AuthenticatedConnection,
    spaceterm_protocol::connection::AuthenticatedConnection,
) {
    use spaceterm_protocol::authentication::{
        ClientHandshake, Established, HandshakeConfig, Purpose, ServerHandshake,
    };
    let mut fixture = support::Fixture::new();
    let local = |id: u64| {
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&id.to_be_bytes());
        bytes
    };
    fixture.principal.pairing = local(3);
    fixture.principal.client = local(4);
    fixture.pairings.record.principal = fixture.principal;
    let config = HandshakeConfig {
        majors: vec![major],
        capabilities: vec![
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
        ],
        ..Default::default()
    };
    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        config.clone(),
        fixture.device_key.clone(),
    )
    .unwrap();
    let (server, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        config,
        fixture.server_key.clone(),
        &fixture.pairings,
        &hello,
    )
    .unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (server, ready) = server.receive_device(&fixture.pairings, &proof).unwrap();
    let Established::Ordinary(client) = client.receive_ready(&ready).unwrap() else {
        unreachable!()
    };
    let Established::Ordinary(server) = server else {
        unreachable!()
    };
    (client, server)
}

#[test]
fn authenticated_both_major_queries_and_pages_use_normal_negotiation() {
    for major in [1, 2] {
        let (mut client, mut server_connection) = capable_connections(major);
        let request = GitQueryRequest::new(
            correlation(),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 2).unwrap(),
                cursor: None,
            },
            None,
            false,
        )
        .unwrap();
        let sent = git::encode_query(major, &request).unwrap();
        let decoded = server_connection
            .decode(&client.encode(&sent).unwrap())
            .unwrap();
        assert_eq!(git::decode_query(&decoded, server()).unwrap(), request);
        let result = GitQueryResponse {
            correlation: correlation(),
            scope: Some(scope()),
            result: Ok(GitQueryResult::Inventory(GitInventoryPage {
                records: vec![GitInventoryRecord {
                    worktree: WorktreeId::new(server(), 8).unwrap(),
                    directory: ServerDirectory::new(server(), b"/remote/\xff".to_vec()).unwrap(),
                    main: false,
                    missing: true,
                    listed: true,
                    version: FieldVersion::new(2),
                }],
                next: None,
                version: spaceterm_core::authority::OrganizationRevision::new(10),
            })),
        };
        let sent = git::encode_response(major, &result, server()).unwrap();
        let decoded = client
            .decode(&server_connection.encode(&sent).unwrap())
            .unwrap();
        assert_eq!(git::decode_response(&decoded, server()).unwrap(), result);
    }
}

#[test]
fn both_major_mutation_outcomes_preserve_original_revision_and_classifications() {
    use spaceterm_core::authority::OrganizationRevision;
    let worktree = WorktreeId::new(server(), 8).unwrap();
    let outcomes = [
        GitMutationOutcomeParts::Created {
            worktree,
            revision: OrganizationRevision::new(12),
        },
        GitMutationOutcomeParts::Removed {
            worktree,
            closed_tabs: 4096,
            ended_sessions: 16384,
            revision: OrganizationRevision::new(13),
        },
        GitMutationOutcomeParts::Failed {
            server: server(),
            failure: GitFailure::StorageFailure,
            ended_sessions: 7,
            external: ExternalDisposition::PartialOrAmbiguous,
        },
        GitMutationOutcomeParts::Recovered {
            server: server(),
            observation: RecoveryObservation::DesiredStateObserved,
            revision: Some(OrganizationRevision::new(14)),
        },
        GitMutationOutcomeParts::Recovered {
            server: server(),
            observation: RecoveryObservation::Unavailable,
            revision: None,
        },
    ];
    for parts in outcomes {
        let outcome = GitMutationOutcome::new(parts).unwrap();
        assert_eq!(
            git::v1::decode_outcome(&git::v1::encode_outcome(&outcome).unwrap(), server()).unwrap(),
            outcome
        );
        assert_eq!(
            git::v2::decode_outcome(&git::v2::encode_outcome(&outcome).unwrap(), server()).unwrap(),
            outcome
        );
    }
    assert!(
        git::v1::decode_outcome(
            &spaceterm_protocol::wire::git::v1::GitMutationOutcome {
                result: Some(
                    spaceterm_protocol::wire::git::v1::git_mutation_outcome::Result::Removed(
                        spaceterm_protocol::wire::git::v1::GitRemoved {
                            worktree: Some(support::entity(8)),
                            closed_tabs: 4097,
                            ended_sessions: 0,
                            revision: 1
                        }
                    )
                )
            },
            server()
        )
        .is_err()
    );
}

#[test]
fn all_git_failure_numbers_have_one_exact_both_major_meaning() {
    let failures = [
        GitFailure::Unauthorized,
        GitFailure::WrongServer,
        GitFailure::TargetGone,
        GitFailure::Offline,
        GitFailure::Busy,
        GitFailure::Capacity,
        GitFailure::ExpiredAssessment,
        GitFailure::StaleAssessment,
        GitFailure::MainWorktree,
        GitFailure::LockedWorktree,
        GitFailure::UnlistedWorktree,
        GitFailure::HoldsPinnedDirectory,
        GitFailure::NoFallback,
        GitFailure::InvalidBranch,
        GitFailure::InvalidTemplate,
        GitFailure::InvalidDirectory,
        GitFailure::OccupiedTarget,
        GitFailure::BranchExists,
        GitFailure::BranchMissing,
        GitFailure::BranchCheckedOut,
        GitFailure::IdentityReplaced,
        GitFailure::GitStateChanged,
        GitFailure::Unchecked,
        GitFailure::ToolMissing,
        GitFailure::ToolTooOld,
        GitFailure::CancelledRead,
        GitFailure::TimedOutRead,
        GitFailure::InvalidResponse,
        GitFailure::OutputTooLarge,
        GitFailure::WriteFailed,
        GitFailure::ExternalStateUncertain,
        GitFailure::RecoveryNeedsReview,
        GitFailure::StorageLocked,
        GitFailure::StorageUnavailable,
        GitFailure::UnsafeStorage,
        GitFailure::StorageFailure,
        GitFailure::IndeterminateDurability,
        GitFailure::BareRepositoryWithoutWorktree,
    ];
    for (index, failure) in failures.into_iter().enumerate() {
        let number = index as i32 + 1;
        assert_eq!(git::v1::encode_failure(failure), number);
        assert_eq!(git::v2::encode_failure(failure), number);
        assert_eq!(git::v1::decode_failure(number), Ok(failure));
        assert_eq!(git::v2::decode_failure(number), Ok(failure));
    }
    for number in [0, 39, -1] {
        assert_eq!(
            git::v1::decode_failure(number),
            Err(ProtocolError::Malformed)
        );
        assert_eq!(
            git::v2::decode_failure(number),
            Err(ProtocolError::Malformed)
        );
    }
}

#[test]
fn duplicate_repository_and_legacy_creation_fields_must_agree() {
    macro_rules! check {
        ($p:ident) => {{
            use spaceterm_protocol::wire::git::$p as w;
            let draft = WorktreeCreateDraft::new(
                scope(),
                WorktreeBranchChoice::New {
                    name: BranchName::new("topic".into()).unwrap(),
                    base: BranchReference::Remote(BranchName::new("origin/main".into()).unwrap()),
                },
                WorktreeLocationChoice::Explicit(
                    ServerDirectory::new(server(), b"/new".to_vec()).unwrap(),
                ),
            )
            .unwrap();
            let mut request = git::$p::encode_query(
                &GitQueryRequest::new(
                    correlation(),
                    GitQuery::PreviewCreate {
                        request: draft.clone(),
                    },
                    None,
                    false,
                )
                .unwrap(),
            )
            .unwrap();
            let Some(w::git_query::Action::AssessCreate(c)) = request.action.as_mut() else {
                unreachable!()
            };
            c.name = Some(w::BranchText {
                value: "topic".into(),
            });
            c.base = Some(w::BranchText {
                value: "origin/main".into(),
            });
            c.remote_base = true;
            c.directory = Some(
                spaceterm_protocol::convert::directories::$p::encode_directory(
                    &ServerDirectory::new(server(), b"/new".to_vec()).unwrap(),
                ),
            );
            assert!(git::$p::decode_query(&request, server()).is_ok());
            request
                .scope
                .as_mut()
                .unwrap()
                .repository
                .as_mut()
                .unwrap()
                .local_id = 99;
            assert_eq!(
                git::$p::decode_query(&request, server()),
                Err(ProtocolError::Malformed)
            );
            request.scope = Some(git::$p::encode_scope(scope()).unwrap());
            let Some(w::git_query::Action::AssessCreate(c)) = request.action.as_mut() else {
                unreachable!()
            };
            c.remote_base = false;
            assert_eq!(
                git::$p::decode_query(&request, server()),
                Err(ProtocolError::Malformed)
            );
            let Some(w::git_query::Action::AssessCreate(c)) = request.action.as_mut() else {
                unreachable!()
            };
            c.remote_base = true;
            c.name.as_mut().unwrap().value = "changed".into();
            assert_eq!(
                git::$p::decode_query(&request, server()),
                Err(ProtocolError::Malformed)
            );
        }};
    }
    check!(v1);
    check!(v2);
}

#[test]
fn creation_preview_and_removal_pages_preserve_optional_membership() {
    use spaceterm_core::authority::{CloseWorkGeneration, ControlGeneration, SessionGeneration};
    let draft = WorktreeCreateDraft::new(
        scope(),
        WorktreeBranchChoice::Existing {
            name: BranchName::new("topic".into()).unwrap(),
        },
        WorktreeLocationChoice::ServerTemplate {
            expected_version: FieldVersion::new(4),
        },
    )
    .unwrap();
    let preview = WorktreeCreationPreview::new(
        draft,
        ServerDirectory::new(server(), b"/proposed".to_vec()).unwrap(),
        Some(FieldVersion::new(4)),
    )
    .unwrap();
    let pages = [
        GitAssessmentPage {
            token: token(GitAssessmentKind::Create),
            tabs: vec![],
            panes: vec![],
            fallbacks: vec![],
            next: None,
            requires_confirmation: true,
            creation_preview: Some(preview),
            removal: None,
        },
        GitAssessmentPage {
            token: token(GitAssessmentKind::Remove),
            tabs: vec![GitAffectedTab {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                tab: spaceterm_core::identity::TabId::new(server(), 2).unwrap(),
                worktree: None,
                membership_version: FieldVersion::new(5),
            }],
            panes: vec![GitAffectedPane {
                pane: PaneId::new(server(), 3).unwrap(),
                tab: spaceterm_core::identity::TabId::new(server(), 2).unwrap(),
                session_generation: SessionGeneration::new(2),
                control_generation: ControlGeneration::new(3),
                work_generation: CloseWorkGeneration::new(4),
                starting: true,
            }],
            fallbacks: vec![GitFallback {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                worktree: None,
                directory: ServerDirectory::new(server(), b"/fallback".to_vec()).unwrap(),
            }],
            next: None,
            requires_confirmation: true,
            creation_preview: None,
            removal: None,
        },
    ];
    for page in pages {
        let response = GitQueryResponse {
            correlation: correlation(),
            scope: Some(scope()),
            result: Ok(GitQueryResult::Assessment(Box::new(page))),
        };
        for major in [1, 2] {
            let payload = git::encode_response(major, &response, server()).unwrap();
            let bytes = payload.encode().unwrap();
            assert_eq!(
                git::decode_response(
                    &spaceterm_protocol::codec::Payload::decode(major, &bytes).unwrap(),
                    server()
                )
                .unwrap(),
                response
            );
        }
    }
}

#[test]
fn git_payload_count_and_byte_bounds_plus_one_reject_without_truncation() {
    use spaceterm_protocol::{
        codec::Payload,
        wire::{v1, v2},
    };
    macro_rules! check {
        ($p:ident,$major:ident) => {{
            use spaceterm_protocol::wire::git::$p as w;
            let page = |count| {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::GitQueryResult(w::GitQueryResult {
                        request: 1,
                        request_generation: 2,
                        scope: Some(git::$p::encode_scope(scope()).unwrap()),
                        result: Some(w::git_query_result::Result::Branches(w::GitBranchPage {
                            records: (0..count)
                                .map(|i| w::GitBranchRecord {
                                    name: Some(w::BranchText {
                                        value: format!("b{i}"),
                                    }),
                                    remote: false,
                                    checked_out: false,
                                })
                                .collect(),
                            next: None,
                        })),
                    })),
                })
            };
            assert!(page(128).encode().is_ok());
            assert_eq!(page(129).encode(), Err(ProtocolError::LimitExceeded));
            let large = GitQueryResponse {
                correlation: correlation(),
                scope: Some(scope()),
                result: Ok(GitQueryResult::Branches(GitBranchPage {
                    records: (0..128)
                        .map(|i| GitBranchRecord {
                            name: BranchName::new(format!("{i:03}{}", "x".repeat(1021))).unwrap(),
                            remote: false,
                            checked_out: false,
                        })
                        .collect(),
                    next: None,
                })),
            };
            assert_eq!(
                git::$p::encode_response(&large, server()),
                Err(ProtocolError::LimitExceeded)
            );
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

fn field(tag: u64, value: &[u8]) -> Vec<u8> {
    fn varint(mut value: u64, bytes: &mut Vec<u8>) {
        while value >= 128 {
            bytes.push((value as u8 & 127) | 128);
            value >>= 7;
        }
        bytes.push(value as u8);
    }
    let mut bytes = Vec::new();
    varint((tag << 3) | 2, &mut bytes);
    varint(value.len() as u64, &mut bytes);
    bytes.extend_from_slice(value);
    bytes
}
#[test]
fn forged_force_native_proofs_and_duplicate_actions_fail_bounded_admission() {
    use prost::Message;
    use spaceterm_protocol::codec::Payload;
    for major in [1, 2] {
        let command = GitCommand::RemoveConfirmed {
            assessment: token(GitAssessmentKind::Remove),
        };
        let bytes = if major == 1 {
            git::v1::encode_command(&command).unwrap().encode_to_vec()
        } else {
            git::v2::encode_command(&command).unwrap().encode_to_vec()
        };
        for append in [vec![0x18, 1], field(4, b"native-proof"), bytes.clone()] {
            let mut forged = bytes.clone();
            forged.extend_from_slice(&append);
            let mut operation = field(1, &support::operation_id().encode_to_vec());
            operation.extend(field(13, &forged));
            assert_eq!(
                Payload::decode(major, &field(4, &operation)),
                Err(ProtocolError::Malformed)
            );
        }
    }
}

#[test]
fn remote_git_values_and_debug_exclude_paths_tokens_and_branch_text() {
    let response = GitQueryResponse {
        correlation: correlation(),
        scope: Some(scope()),
        result: Ok(GitQueryResult::Status(GitStatusFacts {
            revision: std::num::NonZeroU64::new(1).unwrap(),
            branch: Some(BranchName::new("sensitive-branch".into()).unwrap()),
            changes: vec![GitChange::new(b"sensitive-path".to_vec(), 1).unwrap()],
            dirty: true,
            unchecked: false,
            pull_request: None,
            upstream: None,
            operation: None,
            counts: None,
        })),
    };
    let debug = format!("{response:?} {:?}", token(GitAssessmentKind::Remove));
    assert!(!debug.contains("sensitive-branch"));
    assert!(!debug.contains("sensitive-path"));
}

#[test]
fn authenticated_directory_queries_are_bound_to_same_negotiated_principal() {
    use spaceterm_core::server_directories::*;
    use spaceterm_protocol::convert::directories;
    let context = DirectoryContext::new(DirectoryContextParts {
        server: server(),
        epoch: ServerEpoch::new(7),
        pairing: PairingId::new(server(), 3).unwrap(),
        client: ClientId::new(server(), 4).unwrap(),
    })
    .unwrap();
    for major in [1, 2] {
        let (mut client, mut server_connection) = capable_connections(major);
        let request = directories::DirectoryRequest::new(
            1,
            2,
            DirectoryQuery::new(DirectoryQueryParts {
                context,
                action: DirectoryQueryAction::Home,
            })
            .unwrap(),
        )
        .unwrap();
        let sent = directories::encode_query(major, &request).unwrap();
        let received = server_connection
            .decode(&client.encode(&sent).unwrap())
            .unwrap();
        assert_eq!(
            directories::decode_query(&received, context).unwrap(),
            request
        );
        let mut other = context.parts();
        other.client = ClientId::new(server(), 99).unwrap();
        assert_eq!(
            directories::decode_query(&received, DirectoryContext::new(other).unwrap()),
            Err(ProtocolError::PairingDenied)
        );
        let response = directories::DirectoryResponse {
            request: std::num::NonZeroU64::new(1).unwrap(),
            generation: std::num::NonZeroU64::new(2).unwrap(),
            result: Ok(DirectoryQueryResult::Home(
                ServerDirectory::new(server(), b"/owning/server/home".to_vec()).unwrap(),
            )),
        };
        let sent = directories::encode_response(major, &response, context).unwrap();
        let received = client
            .decode(&server_connection.encode(&sent).unwrap())
            .unwrap();
        assert_eq!(
            directories::decode_response(&received, context).unwrap(),
            response
        );
    }
}

fn authenticated_response_round_trip(major: u32, response: &GitQueryResponse) -> GitQueryResponse {
    let (mut client, mut connection) = capable_connections(major);
    let payload = git::encode_response(major, response, server()).unwrap();
    let received = client
        .decode(&connection.encode(&payload).unwrap())
        .unwrap();
    git::decode_response(&received, server()).unwrap()
}

#[test]
fn removal_target_and_all_destructive_categories_round_trip_both_majors() {
    for major in [1, 2] {
        for risk in [
            ApprovedRemoval::Clean,
            ApprovedRemoval::DiscardChanges,
            ApprovedRemoval::DeleteSubmoduleCheckouts,
            ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts,
            ApprovedRemoval::Missing,
        ] {
            let response = GitQueryResponse {
                correlation: correlation(),
                scope: Some(scope()),
                result: Ok(GitQueryResult::Assessment(Box::new(GitAssessmentPage {
                    token: token(GitAssessmentKind::Remove),
                    tabs: vec![],
                    panes: vec![],
                    fallbacks: vec![],
                    next: None,
                    requires_confirmation: true,
                    creation_preview: None,
                    removal: Some(GitRemovalSummary {
                        worktree: WorktreeId::new(server(), 8).unwrap(),
                        directory: ServerDirectory::new(
                            server(),
                            b"/owning/server/linked".to_vec(),
                        )
                        .unwrap(),
                        branch: Some(BranchName::new("kept-branch".into()).unwrap()),
                        risk,
                    }),
                }))),
            };
            assert_eq!(
                authenticated_response_round_trip(major, &response),
                response
            );
        }
    }
}

#[test]
fn status_preserves_pull_request_divergence_operation_and_full_counts_both_majors() {
    for major in [1, 2] {
        for total in [GitChangeTotal::Exact(2000), GitChangeTotal::AtLeast(2000)] {
            for operation in [
                GitRepositoryOperation::Rebasing {
                    step: Some(GitOperationStep {
                        current: 2,
                        total: 7,
                    }),
                },
                GitRepositoryOperation::Applying { step: None },
                GitRepositoryOperation::Merging,
                GitRepositoryOperation::Reverting,
                GitRepositoryOperation::CherryPicking,
                GitRepositoryOperation::Bisecting,
            ] {
                let response = GitQueryResponse {
                    correlation: correlation(),
                    scope: Some(scope()),
                    result: Ok(GitQueryResult::Status(GitStatusFacts {
                        revision: std::num::NonZeroU64::new(1).unwrap(),
                        branch: Some(BranchName::new("local-topic".into()).unwrap()),
                        changes: (0..20)
                            .map(|i| GitChange::new(format!("file-{i}").into_bytes(), 1).unwrap())
                            .collect(),
                        dirty: true,
                        unchecked: false,
                        pull_request: Some(
                            GitPullRequestFacts::new(
                                "github.com".into(),
                                "base-owner".into(),
                                "app".into(),
                                BranchName::new("pushed-topic".into()).unwrap(),
                                Some("fork-owner".into()),
                            )
                            .unwrap(),
                        ),
                        upstream: Some(GitUpstream {
                            name: BranchName::new("upstream/main".into()).unwrap(),
                            divergence: Some(GitDivergence {
                                ahead: 12,
                                behind: 4,
                            }),
                        }),
                        operation: Some(operation),
                        counts: Some(GitChangeCounts {
                            total,
                            staged: 10,
                            modified: 1500,
                            deleted: 7,
                            renamed: 5,
                            conflicted: 3,
                            untracked: 475,
                        }),
                    })),
                };
                assert_eq!(
                    authenticated_response_round_trip(major, &response),
                    response
                );
            }
        }
    }
}

#[test]
fn status_extensions_reject_credentials_invalid_progress_and_inconsistent_counts() {
    macro_rules! check {
        ($p:ident) => {{
            use spaceterm_protocol::wire::git::$p as w;
            let response = |status| w::GitQueryResult {
                request: 1,
                request_generation: 2,
                scope: Some(git::$p::encode_scope(scope()).unwrap()),
                result: Some(w::git_query_result::Result::Status(status)),
            };
            let base = w::GitStatusFacts {
                revision: 1,
                branch: Some(w::BranchText {
                    value: "topic".into(),
                }),
                ..Default::default()
            };
            for (host, owner, repository, head_owner) in [
                ("credential@github.com", "owner", "repo", None),
                ("GitHub.com", "owner", "repo", None),
                ("github.com", "owner/path", "repo", None),
                ("github.com", "owner", "repo?token=credential", None),
                ("github.com", "owner", "repo", Some("token@owner")),
            ] {
                let status = w::GitStatusFacts {
                    pull_request: Some(w::GitPullRequestFacts {
                        host: host.into(),
                        owner: owner.into(),
                        repository: repository.into(),
                        head_branch: Some(w::BranchText {
                            value: "topic".into(),
                        }),
                        head_owner: head_owner.map(Into::into),
                    }),
                    ..base.clone()
                };
                assert_eq!(
                    git::$p::decode_response(&response(status), server()),
                    Err(ProtocolError::Malformed)
                );
            }
            let status = w::GitStatusFacts {
                pull_request: Some(w::GitPullRequestFacts {
                    host: "github.com".into(),
                    owner: "x".repeat(257),
                    repository: "repo".into(),
                    head_branch: Some(w::BranchText {
                        value: "topic".into(),
                    }),
                    head_owner: None,
                }),
                ..base.clone()
            };
            assert_eq!(
                git::$p::decode_response(&response(status), server()),
                Err(ProtocolError::Malformed)
            );
            for (kind, current, total) in [
                (w::GitRepositoryOperationKind::Rebasing, 0, 7),
                (w::GitRepositoryOperationKind::Applying, 8, 7),
                (w::GitRepositoryOperationKind::Merging, 2, 7),
            ] {
                let status = w::GitStatusFacts {
                    operation: Some(w::GitRepositoryOperation {
                        kind: kind as i32,
                        step: Some(w::GitOperationStep { current, total }),
                    }),
                    ..base.clone()
                };
                assert_eq!(
                    git::$p::decode_response(&response(status), server()),
                    Err(ProtocolError::Malformed)
                );
            }
            for counts in [
                w::GitChangeCounts::default(),
                w::GitChangeCounts {
                    total: Some(w::git_change_counts::Total::Exact(0)),
                    modified: 1,
                    ..Default::default()
                },
            ] {
                let status = w::GitStatusFacts {
                    counts: Some(counts),
                    ..base.clone()
                };
                assert_eq!(
                    git::$p::decode_response(&response(status), server()),
                    Err(ProtocolError::Malformed)
                );
            }
            // Legacy facts still read, but absence must not imply an exact display-list count.
            let decoded = git::$p::decode_response(&response(base), server()).unwrap();
            let Ok(GitQueryResult::Status(status)) = decoded.result else {
                panic!("expected status");
            };
            assert_eq!(status.counts, None);
        }};
    }
    check!(v1);
    check!(v2);
}

#[test]
fn removal_summary_requires_its_remove_token_and_owning_server() {
    macro_rules! check {
        ($p:ident) => {{
            use spaceterm_protocol::wire::git::$p as w;
            let mut response = w::GitQueryResult {
                request: 1,
                request_generation: 2,
                scope: Some(git::$p::encode_scope(scope()).unwrap()),
                result: Some(w::git_query_result::Result::Assessment(
                    w::GitAssessmentPage {
                        token: Some(git::$p::encode_token(&token(GitAssessmentKind::Create))),
                        requires_confirmation: true,
                        removal: Some(w::GitRemovalSummary {
                            worktree: Some(support::entity(8)),
                            directory: Some(
                                spaceterm_protocol::convert::directories::$p::encode_directory(
                                    &ServerDirectory::new(server(), b"/linked".to_vec()).unwrap(),
                                ),
                            ),
                            risk: w::GitRemovalRisk::DiscardChanges as i32,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
            };
            assert_eq!(
                git::$p::decode_response(&response, server()),
                Err(ProtocolError::Malformed)
            );
            let Some(w::git_query_result::Result::Assessment(page)) = response.result.as_mut()
            else {
                unreachable!();
            };
            page.token = Some(git::$p::encode_token(&token(GitAssessmentKind::Remove)));
            page.removal.as_mut().unwrap().risk = 0;
            assert_eq!(
                git::$p::decode_response(&response, server()),
                Err(ProtocolError::Malformed)
            );
            let Some(w::git_query_result::Result::Assessment(page)) = response.result.as_mut()
            else {
                unreachable!();
            };
            page.removal.as_mut().unwrap().risk = w::GitRemovalRisk::Clean as i32;
            page.removal
                .as_mut()
                .unwrap()
                .worktree
                .as_mut()
                .unwrap()
                .server
                .as_mut()
                .unwrap()
                .value = vec![2; 16];
            assert_eq!(
                git::$p::decode_response(&response, server()),
                Err(ProtocolError::Malformed)
            );
        }};
    }
    check!(v1);
    check!(v2);
}
