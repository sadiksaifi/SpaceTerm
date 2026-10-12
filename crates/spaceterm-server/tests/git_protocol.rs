//! Codec/service composition with an actor-port fake. Real authenticated
//! IPC/WSS and canonical ledger/store composition remain integration gates.

use spaceterm_core::{authority::*, identity::*, operations::*, storage::*, worktrees::*};
use spaceterm_protocol::convert::git;
use spaceterm_server::{
    contracts::ConnectionAccessSnapshot,
    git::{authority::*, host::*, writes::*},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn server() -> ServerId {
    ServerId::new([1; 16])
}
fn operation() -> OperationId {
    OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(server(), 1).unwrap(),
            PairingId::new(server(), 3).unwrap(),
            ClientId::new(server(), 4).unwrap(),
        )
        .unwrap(),
        OperationSequence::new(1).unwrap(),
    )
}
fn command() -> GitCommand {
    GitCommand::RemoveConfirmed {
        assessment: GitAssessmentToken::new(GitAssessmentTokenParts {
            server: server(),
            epoch: ServerEpoch::new(6),
            pairing: PairingId::new(server(), 3).unwrap(),
            client: ClientId::new(server(), 4).unwrap(),
            kind: GitAssessmentKind::Remove,
            random: [9; 32],
        })
        .unwrap(),
    }
}
struct Actor {
    unexpected_effects: AtomicUsize,
    outcome: GitMutationOutcome,
}
impl Actor {
    fn effect(&self) {
        self.unexpected_effects.fetch_add(1, Ordering::SeqCst);
    }
}
impl GitOrganizationPort for Actor {
    fn authorize(&self, _: AuthenticatedGitPrincipal) -> GitFuture<'_, Result<(), GitFailure>> {
        Box::pin(async { Ok(()) })
    }
    fn lookup<'a>(
        &'a self,
        _: AuthenticatedGitPrincipal,
        id: OperationId,
        c: &'a GitCommand,
    ) -> GitFuture<'a, Result<GitOperationLookup, GitFailure>> {
        assert_eq!(id, operation());
        assert_eq!(c, &command());
        Box::pin(async { Ok(GitOperationLookup::Completed(self.outcome.clone())) })
    }
    fn reserve_confirmed(
        &self,
        _: ConfirmedGitAdmission,
    ) -> GitFuture<'_, Result<GitReservation, GitFailure>> {
        self.effect();
        Box::pin(async { Err(GitFailure::Busy) })
    }
    fn persist_prepared<'a>(&'a self, _: &'a GitReservation) -> GitFuture<'a, CommitDisposition> {
        self.effect();
        Box::pin(async { CommitDisposition::NotApplied(StoreFailure::Unavailable) })
    }
    fn barrier_and_revalidate<'a>(
        &'a self,
        _: &'a GitReservation,
    ) -> GitFuture<'a, Result<(), GitFailure>> {
        self.effect();
        Box::pin(async { Err(GitFailure::Busy) })
    }
    fn persist_started<'a>(&'a self, _: &'a GitReservation) -> GitFuture<'a, CommitDisposition> {
        self.effect();
        Box::pin(async { CommitDisposition::NotApplied(StoreFailure::Unavailable) })
    }
    fn retire_affected<'a>(
        &'a self,
        _: &'a GitReservation,
    ) -> GitFuture<'a, Result<GitRetirementReport, GitFailure>> {
        self.effect();
        Box::pin(async { Err(GitFailure::Busy) })
    }
    fn complete<'a>(
        &'a self,
        _: &'a GitReservation,
        _: &'a GitCompletionPlan,
    ) -> GitFuture<'a, GitCompletionCommit> {
        self.effect();
        Box::pin(async {
            GitCompletionCommit {
                disposition: CommitDisposition::NotApplied(StoreFailure::Unavailable),
                outcome: None,
            }
        })
    }
    fn release(&self, _: GitReservation) -> GitFuture<'_, ()> {
        self.effect();
        Box::pin(async {})
    }
    fn recover_reservation<'a>(
        &'a self,
        _: &'a GitWriteIntent,
    ) -> GitFuture<'a, Result<GitReservation, GitFailure>> {
        self.effect();
        Box::pin(async { Err(GitFailure::Busy) })
    }
    fn close_mutation_admission(&self) -> GitFuture<'_, ()> {
        self.effect();
        Box::pin(async {})
    }
}
struct Host {
    writes: AtomicUsize,
}
impl ServerGitHost for Host {
    fn prepare_write(
        &self,
        _: HostGitWriteRequest,
        _: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err(GitFailure::Unchecked)
    }
    fn reconcile(
        &self,
        _: &GitWriteIntent,
        _: &GitReadCancellation,
    ) -> Result<HostRecoveryObservation, GitFailure> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err(GitFailure::Unchecked)
    }
    fn shutdown(&self) -> Result<(), GitFailure> {
        Ok(())
    }
}

#[tokio::test]
async fn both_major_completed_retry_reaches_service_without_second_write_or_new_admission() {
    let outcome = GitMutationOutcome::new(GitMutationOutcomeParts::Removed {
        worktree: WorktreeId::new(server(), 8).unwrap(),
        closed_tabs: 2,
        ended_sessions: 3,
        revision: OrganizationRevision::new(42),
    })
    .unwrap();
    let actor = Arc::new(Actor {
        unexpected_effects: AtomicUsize::new(0),
        outcome: outcome.clone(),
    });
    let host = Arc::new(Host {
        writes: AtomicUsize::new(0),
    });
    let service = GitService::new(
        server(),
        ServerEpoch::new(7),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let client = ClientId::new(server(), 4).unwrap();
    let principal = AuthenticatedGitPrincipal::from_connection_access(&ConnectionAccessSnapshot {
        server: server(),
        device: DeviceId::new([2; 16]),
        pairing: PairingId::new(server(), 3).unwrap(),
        client,
        binding: ConnectionBinding::new(client, ConnectionGeneration::new(1)),
        epoch: ServerEpoch::new(7),
        reconciled_revision: OrganizationRevision::new(100),
        effects_enabled: true,
    })
    .unwrap();
    for major in [1, 2] {
        let decoded = if major == 1 {
            git::v1::decode_command(&git::v1::encode_command(&command()).unwrap(), server())
                .unwrap()
        } else {
            git::v2::decode_command(&git::v2::encode_command(&command()).unwrap(), server())
                .unwrap()
        };
        let GitSubmission::Retained(result) = service
            .submit(principal, operation(), decoded)
            .await
            .unwrap()
        else {
            panic!("retained operation became a new pending write")
        };
        assert_eq!(result, outcome);
        let returned = if major == 1 {
            git::v1::decode_outcome(&git::v1::encode_outcome(&result).unwrap(), server()).unwrap()
        } else {
            git::v2::decode_outcome(&git::v2::encode_outcome(&result).unwrap(), server()).unwrap()
        };
        assert_eq!(returned.revision(), Some(OrganizationRevision::new(42)));
    }
    assert_eq!(actor.unexpected_effects.load(Ordering::SeqCst), 0);
    assert_eq!(host.writes.load(Ordering::SeqCst), 0);
}

#[test]
fn pull_request_remote_precedence_push_branch_and_fork_owner_survive_both_majors() {
    use spaceterm_core::{organization::BranchName, worktrees::queries::*};
    use spaceterm_host::git::{RepositoryConfig, RepositoryHead};
    let remotes = [
        ("origin", "git@github.com:origin-owner/app.git"),
        (
            "work",
            "https://account:credential@acme.ghe.com/team/app.git",
        ),
        (
            "upstream",
            "ssh://git@ssh.github.com:443/base-owner/app.git",
        ),
        ("default", "git@github.com:default-owner/app.git"),
        ("push", "git@github.com:push-owner/app.git"),
    ];
    for (
        base_remote,
        push_remote,
        push_default,
        push_to_upstream,
        expected_host,
        expected_owner,
        expected_head_owner,
    ) in [
        (None, None, None, false, "github.com", "base-owner", None),
        (
            Some("work"),
            Some("push"),
            Some("default"),
            true,
            "github.com",
            "base-owner",
            Some("push-owner"),
        ),
        (
            Some("work"),
            None,
            Some("default"),
            false,
            "github.com",
            "base-owner",
            Some("default-owner"),
        ),
        (
            Some("origin"),
            None,
            None,
            true,
            "github.com",
            "base-owner",
            Some("origin-owner"),
        ),
    ] {
        let config = RepositoryConfig {
            remotes: remotes
                .iter()
                .map(|(name, url)| ((*name).into(), (*url).into()))
                .collect(),
            branch_remote: base_remote.map(Into::into),
            branch_push_remote: push_remote.map(Into::into),
            push_default: push_default.map(Into::into),
            push_to_upstream,
            branch_merge: Some("refs/heads/pushed-topic".into()),
            ..Default::default()
        };
        let facts = spaceterm_host::git::server::pull_request_facts(
            &config,
            &RepositoryHead::Branch("local-topic".into()),
        )
        .unwrap();
        assert_eq!(facts.host(), expected_host);
        assert_eq!(facts.owner(), expected_owner);
        assert_eq!(facts.head_owner(), expected_head_owner);
        assert_eq!(
            facts.head_branch().as_str(),
            if push_to_upstream {
                "pushed-topic"
            } else {
                "local-topic"
            }
        );
        for major in [1, 2] {
            let response = GitQueryResponse {
                correlation: GitCorrelation::new(1, 1).unwrap(),
                scope: Some(RepositoryScope {
                    server: server(),
                    repository: RepositoryId::new(server(), 1).unwrap(),
                }),
                result: Ok(GitQueryResult::Status(GitStatusFacts {
                    revision: std::num::NonZeroU64::new(1).unwrap(),
                    branch: Some(BranchName::new("local-topic".into()).unwrap()),
                    changes: vec![],
                    dirty: false,
                    unchecked: false,
                    pull_request: Some(facts.clone()),
                    upstream: None,
                    operation: None,
                    counts: None,
                })),
            };
            let payload = git::encode_response(major, &response, server()).unwrap();
            let bytes = payload.encode().unwrap();
            assert!(
                !bytes
                    .windows(b"credential".len())
                    .any(|w| w == b"credential")
            );
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
    // With no upstream remote, the branch remote wins over origin. Userinfo is stripped.
    for (branch_remote, expected_host, expected_owner) in [
        (Some("work"), "acme.ghe.com", "team"),
        (None, "github.com", "origin-owner"),
    ] {
        let config = RepositoryConfig {
            remotes: remotes
                .iter()
                .filter(|(name, _)| *name != "upstream")
                .map(|(name, url)| ((*name).into(), (*url).into()))
                .collect(),
            branch_remote: branch_remote.map(Into::into),
            ..Default::default()
        };
        let facts = spaceterm_host::git::server::pull_request_facts(
            &config,
            &RepositoryHead::Branch("local-topic".into()),
        )
        .unwrap();
        assert_eq!(
            (facts.host(), facts.owner()),
            (expected_host, expected_owner)
        );
    }
}
