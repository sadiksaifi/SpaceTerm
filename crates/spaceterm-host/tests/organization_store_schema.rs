//! Schema contract tests against the descriptor-relative SQLite authority.

// Source-included until Job B registers these real leaf modules in its facade.
// This keeps Job A inside its five exclusively owned files.
#[path = "../src/organization_store/codec.rs"]
pub mod codec;
#[path = "../src/organization_store/limits.rs"]
pub mod limits;
#[path = "../src/organization_store/schema.rs"]
pub mod schema;

use std::os::unix::fs::PermissionsExt;

use spaceterm_host::storage::{CommitDisposition, Database, Migration};

const SQL: &str = include_str!("../src/organization_store/sql/001.sql");

fn fixture(migrations: &[Migration<'_>]) -> (tempfile::TempDir, Database) {
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = Database::open(directory.path(), migrations).unwrap();
    (directory, database)
}

#[test]
fn schema_retains_full_width_high_water_and_checks_blob_shape() {
    let (_directory, mut database) = fixture(&[Migration { sql: SQL }]);
    let high = u64::MAX.to_be_bytes();
    let result = database.transaction(|transaction| {
        transaction.execute(
            "INSERT INTO identity_high_water VALUES (1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1)",
            [high.as_slice()],
        )?;
        transaction.query_row("SELECT pane FROM identity_high_water", [], |row| {
            row.get::<_, Vec<u8>>(0)
        })
    });
    assert_eq!(result, CommitDisposition::Durable(high.to_vec()));
    assert!(matches!(
        database.transaction(
            |transaction| transaction.execute("UPDATE identity_high_water SET pane = 1", [],)
        ),
        CommitDisposition::NotApplied(_)
    ));
}

use codec::{CodecFailure, RecordKind};
use limits::{Collection, LimitFailure, StoreLimits};
use spaceterm_core::authority::FieldVersion;
use spaceterm_core::identity::*;
use spaceterm_core::organization::{BranchName, ServerDirectory};
use spaceterm_core::organization_retained::TabCreationDefault;
use spaceterm_core::pairing::{
    ClientRecord, PairingClientStreams, PairingClientStreamsParts, PairingKeys, PairingKeysParts,
};
use spaceterm_core::remote_access::RemoteAccessPolicy;
use spaceterm_core::worktrees::intent::*;
use spaceterm_host::storage::{StorageFailure, Transaction};

fn server() -> ServerId {
    ServerId::new([7; 16])
}
fn directory() -> ServerDirectory {
    ServerDirectory::new(server(), b"/absent/opaque-\xff".to_vec()).unwrap()
}
fn branch(value: &str) -> BranchName {
    BranchName::new(value.to_owned()).unwrap()
}
fn schema_fixture() -> (tempfile::TempDir, Database) {
    fixture(schema::migrations())
}
fn durable<T>(result: CommitDisposition<T>) -> T {
    match result {
        CommitDisposition::Durable(value) => value,
        CommitDisposition::NotApplied(failure) => {
            panic!("fixture transaction rejected: {failure:?}")
        }
        CommitDisposition::Indeterminate => panic!("fixture transaction indeterminate"),
    }
}
fn insert_hierarchy(
    transaction: &Transaction<'_>,
    workspace: u64,
    tab: u64,
    pane: u64,
) -> Result<(), StorageFailure> {
    let workspace = codec::encode_u64(workspace);
    let tab = codec::encode_u64(tab);
    let pane = codec::encode_u64(pane);
    // Deferred membership constraints permit a single atomic cyclic insertion.
    transaction.execute("INSERT INTO panes(id, tab_id, creation_ordinal, creation_kind, starting_directory) VALUES (?1, ?2, ?1, 0, x'2f')", [pane.as_slice(), tab.as_slice()])?;
    transaction.execute(
        "INSERT INTO tabs(id, workspace_id, root_pane, creation_ordinal) VALUES (?1, ?2, ?3, ?1)",
        [tab.as_slice(), workspace.as_slice(), pane.as_slice()],
    )?;
    transaction.execute("INSERT INTO workspaces(id, root_tab, creation_ordinal, name_mode, display_name) VALUES (?1, ?2, ?1, 0, 'workspace')", [workspace.as_slice(), tab.as_slice()])?;
    Ok(())
}

#[test]
fn schema_inventory_checks_exact_columns_constraints_and_object_set() {
    let (_directory, mut database) = schema_fixture();
    schema::validate_inventory(&database).unwrap();
    durable(database.transaction(schema::validate_transaction_inventory));
    let changed = SQL.replace("CREATE TABLE panes (", "CREATE TABLE panes (extra INTEGER,");
    let (_changed_directory, changed_database) = fixture(&[Migration { sql: &changed }]);
    assert_eq!(
        schema::validate_inventory(&changed_database),
        Err(StorageFailure::InvalidState)
    );
}

#[test]
fn schema_v1_migration_is_transactional() {
    let sql = format!("{SQL}\nINSERT INTO absent_table VALUES (1);");
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        Database::open(directory.path(), &[Migration { sql: &sql }]),
        Err(StorageFailure::InvalidState)
    ));
    let database = Database::open_existing(directory.path(), &[]).unwrap();
    assert_eq!(database.schema_version().unwrap(), 0);
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn newer_schema_is_rejected_without_reinitialization() {
    let migrations = [
        Migration { sql: SQL },
        Migration {
            sql: "CREATE TABLE future_marker(id INTEGER PRIMARY KEY) STRICT;",
        },
    ];
    let (directory, database) = fixture(&migrations);
    drop(database);
    assert!(matches!(
        Database::open_existing(directory.path(), schema::migrations()),
        Err(StorageFailure::UnknownSchema {
            found: 2,
            supported: 1
        })
    ));
    let database = Database::open_existing(directory.path(), &migrations).unwrap();
    assert_eq!(database.schema_version().unwrap(), 2);
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'future_marker'",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn foreign_application_schema_is_not_adopted() {
    let (_directory, mut database) = fixture(&[]);
    durable(database.transaction(|transaction| transaction.execute_batch("CREATE TABLE foreign_data(id INTEGER PRIMARY KEY); INSERT INTO foreign_data VALUES (9);")));
    assert_eq!(
        schema::validate_migration_source(&database),
        Err(StorageFailure::InvalidState)
    );
    assert_eq!(database.schema_version().unwrap(), 0);
    assert_eq!(
        database
            .query_row("SELECT id FROM foreign_data", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        9
    );
}

#[test]
fn empty_database_is_not_new_storage_evidence() {
    let (_directory, database) = fixture(&[]);
    assert_eq!(
        schema::validate_migration_source(&database),
        Err(StorageFailure::RecoveryRequired)
    );
}

#[test]
fn missing_application_ownership_does_not_admit_an_existing_store() {
    let (_directory, database) = schema_fixture();
    assert_eq!(
        schema::validate_existing(&database),
        Err(StorageFailure::RecoveryRequired)
    );
}

#[test]
fn unexpected_trigger_view_index_and_virtual_table_are_rejected() {
    for sql in [
        "CREATE TRIGGER foreign_trigger AFTER INSERT ON workspaces BEGIN SELECT 1; END;",
        "CREATE VIEW foreign_view AS SELECT id FROM workspaces;",
        "CREATE INDEX foreign_index ON workspaces(display_name);",
        "CREATE VIRTUAL TABLE foreign_virtual USING fts5(value);",
    ] {
        let (_directory, mut database) = schema_fixture();
        durable(database.transaction(|transaction| transaction.execute_batch(sql)));
        assert_eq!(
            schema::validate_inventory(&database),
            Err(StorageFailure::InvalidState)
        );
    }
}

#[test]
fn root_references_bind_parent_membership_not_only_identity() {
    for sql in [
        "UPDATE workspaces SET root_tab = x'0000000000000016' WHERE id = x'0000000000000001'",
        "UPDATE tabs SET root_pane = x'00000000000000de' WHERE id = x'000000000000000b'",
    ] {
        let (_directory, mut database) = schema_fixture();
        durable(database.transaction(|transaction| {
            insert_hierarchy(transaction, 1, 11, 111)?;
            insert_hierarchy(transaction, 2, 22, 222)
        }));
        assert!(matches!(
            database.transaction(|transaction| transaction.execute(sql, [])),
            CommitDisposition::NotApplied(StorageFailure::InvalidState)
        ));
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM workspaces", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            2
        );
    }
}

#[test]
fn replay_membership_and_split_source_cannot_cross_tabs() {
    let (_directory, mut database) = schema_fixture();
    durable(database.transaction(|transaction| {
        insert_hierarchy(transaction, 1, 11, 111)?;
        insert_hierarchy(transaction, 2, 22, 222)
    }));
    for sql in [
        "INSERT INTO tab_creation_replay VALUES(x'000000000000000b', zeroblob(8), x'00000000000000de')",
        "UPDATE panes SET creation_kind = 1, creation_source = x'00000000000000de', split_axis = 0, split_edge = 1 WHERE id = x'000000000000006f'",
    ] {
        assert!(matches!(
            database.transaction(|transaction| transaction.execute(sql, [])),
            CommitDisposition::NotApplied(StorageFailure::InvalidState)
        ));
    }
}

#[test]
fn u64_values_above_i64_max_round_trip_exactly() {
    let (_directory, mut database) = schema_fixture();
    for value in [0, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
        let bytes = codec::encode_u64(value);
        durable(database.transaction(|transaction| transaction.execute(
            "INSERT INTO identity_high_water VALUES (1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1) ON CONFLICT(singleton) DO UPDATE SET pane = excluded.pane", [bytes.as_slice()],
        )));
        let stored = database
            .query_row("SELECT pane FROM identity_high_water", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap();
        assert_eq!(codec::decode_u64(&stored).unwrap(), value);
    }
    for invalid in [vec![0; 7], vec![0; 9]] {
        assert_eq!(
            codec::decode_u64(&invalid),
            Err(CodecFailure::InvalidEncoding)
        );
    }
    assert_eq!(
        spaceterm_core::authority::StoreRevision::new(
            codec::decode_u64(&u64::MAX.to_be_bytes()).unwrap()
        )
        .checked_successor(),
        None
    );
}

#[test]
fn relational_text_and_directory_lengths_use_bytes_and_reject_null_bytes() {
    let (_directory, mut database) = schema_fixture();
    durable(database.transaction(|transaction| insert_hierarchy(transaction, 1, 11, 111)));
    let name = "é".repeat(512);
    durable(database.transaction(|transaction| {
        transaction.execute("UPDATE workspaces SET display_name = ?1", [&name])
    }));
    let too_long = format!("{name}x");
    assert!(matches!(
        database.transaction(|transaction| transaction
            .execute("UPDATE workspaces SET display_name = ?1", [&too_long])),
        CommitDisposition::NotApplied(StorageFailure::InvalidState)
    ));
    let directory = vec![b'/'; 4096];
    durable(database.transaction(|transaction| {
        transaction.execute("UPDATE panes SET starting_directory = ?1", [&directory])
    }));
    for invalid in [vec![b'/'; 4097], b"/has\0zero".to_vec()] {
        assert!(matches!(
            database.transaction(|transaction| transaction
                .execute("UPDATE panes SET starting_directory = ?1", [&invalid])),
            CommitDisposition::NotApplied(StorageFailure::InvalidState)
        ));
    }
}

fn insert_stream(transaction: &Transaction<'_>) -> Result<(), StorageFailure> {
    transaction.execute("INSERT INTO operation_streams(stream_key, pairing_id, client_id, purpose, status, admitted_through, acknowledged_through, request_format_version, outcome_format_version, quota_policy_version) VALUES(x'0000000000000001', x'0000000000000001', x'0000000000000001', 0, 0, x'0000000000000001', zeroblob(8), 1, 1, 1)", [])?;
    Ok(())
}
#[test]
fn outcome_storage_ceiling_is_the_ledger_sixty_kib_bound() {
    let (_directory, mut database) = schema_fixture();
    durable(database.transaction(insert_stream));
    let insert = |transaction: &Transaction<'_>, length| {
        transaction.execute(
        "INSERT INTO operation_entries(stream_key, sequence, request_digest, state, reservation_class, reservation_bytes, admitted_at, completed_at, compact_outcome, publication_revision) VALUES(x'0000000000000001', x'0000000000000001', zeroblob(32), 1, 0, x'0000000000010000', x'0000000000000001', x'0000000000000002', zeroblob(?1), zeroblob(8))", [length],
    )
    };
    assert!(matches!(
        database.transaction(|transaction| insert(transaction, 61_441)),
        CommitDisposition::NotApplied(StorageFailure::InvalidState)
    ));
    durable(database.transaction(|transaction| insert(transaction, 61_440)));
    assert_eq!(
        database
            .query_row(
                "SELECT length(compact_outcome) FROM operation_entries",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        61_440
    );
}

#[test]
fn restricted_stream_cannot_lose_its_dormant_reservation_through_sql_null_semantics() {
    let (_directory, mut database) = schema_fixture();
    let reserved_bytes = codec::encode_u64(65_536);
    for (class, bytes) in [
        (None, None),
        (Some(2), None),
        (None, Some(reserved_bytes.as_slice())),
        (Some(0), Some(reserved_bytes.as_slice())),
    ] {
        assert!(matches!(database.transaction(|transaction| transaction.execute(
            "INSERT INTO operation_streams(stream_key, pairing_id, client_id, purpose, status, admitted_through, acknowledged_through, dormant_class, dormant_bytes, request_format_version, outcome_format_version, quota_policy_version) VALUES(x'0000000000000001', x'0000000000000001', x'0000000000000001', 1, 0, zeroblob(8), zeroblob(8), ?1, ?2, 1, 1, 1)", rusqlite::params![class, bytes],
        )), CommitDisposition::NotApplied(StorageFailure::InvalidState)));
    }
    durable(database.transaction(|transaction| transaction.execute(
        "INSERT INTO operation_streams(stream_key, pairing_id, client_id, purpose, status, admitted_through, acknowledged_through, dormant_class, dormant_bytes, request_format_version, outcome_format_version, quota_policy_version) VALUES(x'0000000000000001', x'0000000000000001', x'0000000000000001', 1, 0, zeroblob(8), zeroblob(8), 2, ?1, 1, 1, 1)", [reserved_bytes.as_slice()],
    )));
}

#[test]
fn nonledger_record_codec_rejects_tags_versions_truncation_trailing_bytes_and_bounds_plus_one() {
    let encoded = codec::encode_directory(&directory()).unwrap();
    for length in 0..encoded.len() {
        assert!(codec::decode_directory(&encoded[..length], server()).is_err());
    }
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert_eq!(
        codec::decode_directory(&trailing, server()),
        Err(CodecFailure::InvalidEncoding)
    );
    let mut unsupported = encoded.clone();
    unsupported[5] = 2;
    assert_eq!(
        codec::decode_directory(&unsupported, server()),
        Err(CodecFailure::UnsupportedVersion)
    );
    let mut unknown = encoded.clone();
    unknown[7] = 255;
    assert_eq!(
        codec::decode_directory(&unknown, server()),
        Err(CodecFailure::InvalidEncoding)
    );
    let mut wrong_length = encoded.clone();
    wrong_length[11] += 1;
    assert!(codec::decode_directory(&wrong_length, server()).is_err());
    let oversized = vec![0; RecordKind::Directory.maximum_payload() + 13];
    assert_eq!(
        codec::decode_directory(&oversized, server()),
        Err(CodecFailure::ResourceLimit)
    );
    assert_eq!(
        codec::decode_directory(&encoded, ServerId::new([8; 16])),
        Err(CodecFailure::WrongServer)
    );
}

#[test]
fn directory_v1_fixture_preserves_opaque_native_bytes() {
    let value = ServerDirectory::new(ServerId::new([1; 16]), vec![b'/', 0xff]).unwrap();
    let mut fixture = b"STOS\0\x01\0\x01\0\0\0\x16".to_vec();
    fixture.extend_from_slice(&[1; 16]);
    fixture.extend_from_slice(&[0, 0, 0, 2, b'/', 0xff]);
    assert_eq!(codec::encode_directory(&value).unwrap(), fixture);
    assert_eq!(
        codec::decode_directory(&fixture, value.server()).unwrap(),
        value
    );
    let maximum = ServerDirectory::new(server(), vec![1; 4096]).unwrap();
    assert_eq!(
        codec::decode_directory(&codec::encode_directory(&maximum).unwrap(), server()).unwrap(),
        maximum
    );
}

#[test]
fn replay_codec_preserves_unsorted_order_and_preflights_oversized_counts() {
    let value = TabCreationDefault::new(
        TabId::new(server(), 5).unwrap(),
        [9, 3, 7]
            .map(|id| PaneId::new(server(), id).unwrap())
            .to_vec(),
    )
    .unwrap();
    let encoded = codec::encode_creation_default(&value).unwrap();
    assert_eq!(
        codec::decode_creation_default(&encoded, server()).unwrap(),
        value
    );
    let invalid = codec::encode_record(RecordKind::CreationDefault, |writer| {
        writer.fixed(&server().bytes())?;
        writer.u64(5)?;
        writer.fixed(&65_536u32.to_be_bytes())
    })
    .unwrap();
    assert_eq!(
        codec::decode_creation_default(&invalid, server()),
        Err(CodecFailure::ResourceLimit)
    );
    let duplicate = codec::encode_record(RecordKind::CreationDefault, |writer| {
        writer.fixed(&server().bytes())?;
        writer.u64(5)?;
        writer.count(2, 2)?;
        writer.u64(9)?;
        writer.u64(9)
    })
    .unwrap();
    assert_eq!(
        codec::decode_creation_default(&duplicate, server()),
        Err(CodecFailure::InvalidEncoding)
    );
}

#[test]
fn remote_codec_keeps_ipv6_scope_and_enforces_eight_endpoint_policy() {
    let endpoints = vec![
        "127.0.0.1:0".parse().unwrap(),
        std::net::SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::LOCALHOST,
            42,
            17,
            8,
        )),
    ];
    let value = RemoteAccessPolicy::enabled(server(), endpoints).unwrap();
    assert_eq!(
        codec::decode_remote_access(&codec::encode_remote_access(&value).unwrap(), server())
            .unwrap(),
        value
    );
    let disabled = RemoteAccessPolicy::disabled(server());
    assert_eq!(
        codec::decode_remote_access(&codec::encode_remote_access(&disabled).unwrap(), server())
            .unwrap(),
        disabled
    );
    let maximum = RemoteAccessPolicy::enabled(
        server(),
        (0..8)
            .map(|port| std::net::SocketAddr::from(([127, 0, 0, 1], port)))
            .collect(),
    )
    .unwrap();
    assert_eq!(
        codec::decode_remote_access(&codec::encode_remote_access(&maximum).unwrap(), server())
            .unwrap(),
        maximum
    );
    let invalid = codec::encode_record(RecordKind::RemoteAccess, |writer| {
        writer.fixed(&server().bytes())?;
        writer.count(9, 16)
    })
    .unwrap();
    assert_eq!(
        codec::decode_remote_access(&invalid, server()),
        Err(CodecFailure::ResourceLimit)
    );
}

#[test]
fn pairing_codecs_keep_distinct_key_roles_and_exact_per_client_streams() {
    let keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: [1; 32],
        pairing_authentication_public_key: [2; 32],
    })
    .unwrap();
    assert_eq!(
        codec::decode_pairing_keys(&codec::encode_pairing_keys(&keys).unwrap()).unwrap(),
        keys
    );
    let collapsed =
        codec::encode_record(RecordKind::PairingKeys, |writer| writer.fixed(&[1; 64])).unwrap();
    assert_eq!(
        codec::decode_pairing_keys(&collapsed),
        Err(CodecFailure::InvalidEncoding)
    );
    let pairing = PairingId::new(server(), 4).unwrap();
    let client = ClientId::new(server(), 6).unwrap();
    let stream = |id| {
        OperationStreamId::new(
            OperationStreamKey::new(server(), id).unwrap(),
            pairing,
            client,
        )
        .unwrap()
    };
    let value = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: client,
            pairing,
        },
        ordinary: stream(9),
        self_revocation: stream(10),
    })
    .unwrap();
    assert_eq!(
        codec::decode_pairing_client_streams(
            &codec::encode_pairing_client_streams(&value).unwrap(),
            server()
        )
        .unwrap(),
        value
    );
}

#[test]
fn repository_locator_codec_retains_missing_paths_without_native_authority() {
    let value = RepositoryLocator::new(RepositoryLocatorParts {
        scope: spaceterm_core::operations::RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), u64::MAX).unwrap(),
        },
        main_root: directory(),
        common_directory: directory(),
    })
    .unwrap();
    assert_eq!(
        codec::decode_repository_locator(
            &codec::encode_repository_locator(&value).unwrap(),
            server()
        )
        .unwrap(),
        value
    );
}

#[test]
fn all_supported_git_action_variants_round_trip() {
    let worktree = WorktreeId::new(server(), u64::MAX).unwrap();
    for branch in [
        WorktreeBranchChoice::New {
            name: branch("new"),
            base: BranchReference::Local(branch("main")),
        },
        WorktreeBranchChoice::New {
            name: branch("new"),
            base: BranchReference::Remote(branch("origin/main")),
        },
        WorktreeBranchChoice::Existing {
            name: branch("existing"),
        },
        WorktreeBranchChoice::Remote {
            remote: branch("origin/branch"),
            name: branch("branch"),
        },
    ] {
        for target in [
            CreationTargetObservation::Absent,
            CreationTargetObservation::EmptyDirectory,
        ] {
            for template_version in [None, Some(FieldVersion::new(u64::MAX))] {
                let value = RetainedGitAction::Create {
                    worktree,
                    directory: directory(),
                    branch: branch.clone(),
                    target,
                    template_version,
                };
                assert_eq!(
                    codec::decode_git_action(&codec::encode_git_action(&value).unwrap(), server())
                        .unwrap(),
                    value
                );
            }
        }
    }
    for approval in [
        ApprovedRemoval::Clean,
        ApprovedRemoval::DiscardChanges,
        ApprovedRemoval::DeleteSubmoduleCheckouts,
        ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts,
        ApprovedRemoval::Missing,
    ] {
        for head in [
            None,
            Some(RetainedGitHead::Unborn(branch("main"))),
            Some(RetainedGitHead::Detached(GitObjectId::Sha1([4; 20]))),
            Some(RetainedGitHead::Branch {
                name: branch("main"),
                commit: GitObjectId::Sha256([5; 32]),
            }),
        ] {
            let value = RetainedGitAction::Remove {
                worktree,
                directory: directory(),
                head,
                approval,
            };
            assert_eq!(
                codec::decode_git_action(&codec::encode_git_action(&value).unwrap(), server())
                    .unwrap(),
                value
            );
        }
    }
}

#[test]
fn git_action_v1_fixture_and_unknown_operand_tag_are_checked() {
    let value = RetainedGitAction::Remove {
        worktree: WorktreeId::new(ServerId::new([1; 16]), 2).unwrap(),
        directory: ServerDirectory::new(ServerId::new([1; 16]), b"/".to_vec()).unwrap(),
        head: None,
        approval: ApprovedRemoval::Missing,
    };
    let mut fixture = b"STOS\0\x01\0\x07\0\0\0\x20".to_vec();
    fixture.extend_from_slice(&[1; 16]);
    fixture.extend_from_slice(&2u64.to_be_bytes());
    fixture.extend_from_slice(&[0, 0, 0, 1, b'/', 1, 0, 4]);
    assert_eq!(codec::encode_git_action(&value).unwrap(), fixture);
    fixture[43] = 255;
    assert_eq!(
        codec::decode_git_action(&fixture, value.worktree().server()),
        Err(CodecFailure::InvalidEncoding)
    );
}

#[test]
fn record_reader_checks_operation_binding_and_complete_consumption() {
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server(), u64::MAX).unwrap(),
        PairingId::new(server(), 1).unwrap(),
        ClientId::new(server(), 2).unwrap(),
    )
    .unwrap();
    let binding = spaceterm_core::operations::OperationBinding {
        id: spaceterm_core::operations::OperationId::new(
            stream,
            spaceterm_core::operations::OperationSequence::new(u64::MAX).unwrap(),
        ),
        digest: spaceterm_core::operations::RequestDigest::from_bytes([3; 32]),
    };
    let bytes = codec::encode_record(RecordKind::EnrollmentIssuer, |writer| {
        writer.operation_binding(binding)
    })
    .unwrap();
    assert_eq!(
        codec::decode_record(RecordKind::EnrollmentIssuer, &bytes, |reader| reader
            .operation_binding())
        .unwrap(),
        binding
    );
    assert_eq!(
        codec::decode_record(RecordKind::EnrollmentIssuer, &bytes, |reader| reader.tag()),
        Err(CodecFailure::InvalidEncoding)
    );
}

#[test]
fn every_collection_ceiling_rejects_limit_plus_one_before_allocation() {
    let limits = StoreLimits::production();
    for collection in [
        Collection::Workspaces,
        Collection::Tabs,
        Collection::Panes,
        Collection::CreationReplay,
        Collection::Worktrees,
        Collection::Repositories,
        Collection::Pairings,
        Collection::Clients,
        Collection::Enrollments,
        Collection::Receipts,
        Collection::OperationStreams,
        Collection::OperationEntries,
        Collection::GitIntents,
        Collection::NativeRetirements,
        Collection::RemoteEndpoints,
    ] {
        let max = limits.maximum(collection) as u64;
        limits.check_count(collection, max).unwrap();
        assert_eq!(
            limits.check_count(collection, max + 1),
            Err(LimitFailure::Exceeded)
        );
        assert_eq!(
            limits.check_count(collection, u64::MAX),
            Err(LimitFailure::Exceeded)
        );
    }
    assert!(
        StoreLimits::production()
            .with_test_ceiling(Collection::Panes, 65_537)
            .is_err()
    );
    assert!(
        StoreLimits::production()
            .with_test_ceiling(Collection::Panes, 1)
            .unwrap()
            .check_count(Collection::Panes, 2)
            .is_err()
    );
}

#[test]
fn cumulative_accounting_checks_bytes_row_products_and_transaction_counts() {
    let limits = StoreLimits::production();
    let mut budget = limits.transaction_budget();
    budget.charge(limits::MAX_TRANSACTION_BYTES).unwrap();
    assert_eq!(budget.charge(1), Err(LimitFailure::Exceeded));
    assert_eq!(budget.used(), limits::MAX_TRANSACTION_BYTES);
    let mut recovery = limits.recovery_budget();
    recovery
        .charge_rows(2, limits::MAX_RECOVERY_BYTES / 2)
        .unwrap();
    assert_eq!(recovery.charge(1), Err(LimitFailure::Exceeded));
    assert_eq!(
        recovery.charge_rows(u64::MAX, usize::MAX),
        Err(LimitFailure::Exceeded)
    );
    limits
        .check_transaction_counts(limits::MAX_CHANGED_ROWS, limits::MAX_AUXILIARY_WRITES)
        .unwrap();
    assert_eq!(
        limits.check_transaction_counts(limits::MAX_CHANGED_ROWS + 1, 0),
        Err(LimitFailure::Exceeded)
    );
    assert_eq!(
        limits.check_transaction_counts(0, limits::MAX_AUXILIARY_WRITES + 1),
        Err(LimitFailure::Exceeded)
    );
}

#[test]
fn relational_creation_codec_rejects_unknown_or_inconsistent_tags() {
    assert_eq!(
        codec::decode_pane_creation(server(), 0, None, None, None).unwrap(),
        spaceterm_core::organization::PaneCreation::Initial
    );
    let source = codec::encode_u64(u64::MAX);
    assert!(codec::decode_pane_creation(server(), 1, Some(&source), Some(1), Some(0)).is_ok());
    for (kind, axis, edge) in [(255, 0, 0), (1, 2, 0), (1, 0, 2), (0, 0, 0)] {
        assert_eq!(
            codec::decode_pane_creation(server(), kind, Some(&source), Some(axis), Some(edge)),
            Err(CodecFailure::InvalidEncoding)
        );
    }
}

#[test]
fn unknown_encoding_version_is_not_malformed_or_resettable() {
    schema::check_encoding_version(1).unwrap();
    assert_eq!(
        schema::check_encoding_version(0),
        Err(StorageFailure::InvalidState)
    );
    assert_eq!(
        schema::check_encoding_version(2),
        Err(StorageFailure::UnknownSchema {
            found: 2,
            supported: 1
        })
    );
}

#[test]
fn failure_metadata_and_codec_debug_redact_payload_sentinels() {
    let sentinel = b"private-path-and-credential-sentinel";
    let mut debug = String::new();
    let encoded = codec::encode_record(RecordKind::Directory, |writer| {
        writer.fixed(sentinel)?;
        debug.push_str(&format!("{writer:?}"));
        Ok(())
    })
    .unwrap();
    codec::decode_record(RecordKind::Directory, &encoded, |reader| {
        debug.push_str(&format!("{reader:?}"));
        reader.fixed(sentinel.len()).map(|_| ())
    })
    .unwrap();
    for error in [
        CodecFailure::UnsupportedVersion,
        CodecFailure::InvalidEncoding,
        CodecFailure::WrongServer,
        CodecFailure::ResourceLimit,
    ] {
        debug.push_str(&format!(
            "{error:?} {error} {:?}",
            std::error::Error::source(&error)
        ));
    }
    assert!(!debug.contains(std::str::from_utf8(sentinel).unwrap()));
}

fn operation_binding() -> spaceterm_core::operations::OperationBinding {
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server(), 5).unwrap(),
        PairingId::new(server(), 3).unwrap(),
        ClientId::new(server(), 4).unwrap(),
    )
    .unwrap();
    spaceterm_core::operations::OperationBinding {
        id: spaceterm_core::operations::OperationId::new(
            stream,
            spaceterm_core::operations::OperationSequence::new(1).unwrap(),
        ),
        digest: spaceterm_core::operations::RequestDigest::from_bytes([6; 32]),
    }
}

#[test]
fn enrollment_issuer_codec_keeps_native_and_paired_authority_distinct() {
    use spaceterm_core::pairing::EnrollmentIssuer;
    let paired = EnrollmentIssuer::Paired {
        operation: operation_binding(),
        authorization_version: FieldVersion::new(u64::MAX),
    };
    let encoded = codec::encode_enrollment_issuer(&paired).unwrap();
    assert_eq!(
        codec::decode_enrollment_issuer(&encoded, server()).unwrap(),
        paired
    );
    assert_eq!(
        codec::decode_enrollment_issuer(&encoded, ServerId::new([8; 16])),
        Err(CodecFailure::WrongServer)
    );
    let owner = EnrollmentIssuer::AccountOwner {
        native_request: [1; 16],
        native_request_digest: spaceterm_core::operations::RequestDigest::from_bytes([2; 32]),
    };
    let mut fixture = b"STOS\0\x01\0\x08\0\0\0\x31\x01".to_vec();
    fixture.extend_from_slice(&[1; 16]);
    fixture.extend_from_slice(&[2; 32]);
    assert_eq!(codec::encode_enrollment_issuer(&owner).unwrap(), fixture);
    assert_eq!(
        codec::decode_enrollment_issuer(&fixture, server()).unwrap(),
        owner
    );
    fixture[13..29].fill(0);
    assert_eq!(
        codec::decode_enrollment_issuer(&fixture, server()),
        Err(CodecFailure::InvalidEncoding)
    );
}

#[test]
fn git_allocation_codec_keeps_original_marks_and_reserved_fallback_ids() {
    use spaceterm_core::authority::CreationOrdinal;
    let old = IdentityHighWater {
        workspace: 1,
        tab: 10,
        pane: 20,
        worktree: 2,
        repository: 0,
        pairing: 3,
        client: 4,
        operation_stream: 5,
        creation_ordinal: 30,
    };
    let new = IdentityHighWater {
        tab: 11,
        pane: 21,
        repository: 1,
        worktree: 3,
        creation_ordinal: 32,
        ..old
    };
    let value = GitAllocationReservation::new(GitAllocationReservationParts {
        binding: operation_binding(),
        expected_high_water: old,
        resulting_high_water: new,
        repository: Some(RepositoryId::new(server(), 1).unwrap()),
        worktree: Some(WorktreeId::new(server(), 3).unwrap()),
        fallbacks: vec![GitReservedFallback {
            plan: WorkspaceFallbackPlan {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                worktree: WorktreeId::new(server(), 2).unwrap(),
                directory: directory(),
            },
            tab: TabId::new(server(), 11).unwrap(),
            pane: PaneId::new(server(), 21).unwrap(),
            tab_creation_ordinal: CreationOrdinal::new(31),
            pane_creation_ordinal: CreationOrdinal::new(32),
        }],
    })
    .unwrap();
    let encoded = codec::encode_git_allocation(&value).unwrap();
    assert_eq!(
        codec::decode_git_allocation(&encoded, server()).unwrap(),
        value
    );
    assert_eq!(
        codec::decode_git_allocation(&encoded, ServerId::new([8; 16])),
        Err(CodecFailure::WrongServer)
    );
    let mut corrupt = encoded;
    corrupt[236..244].copy_from_slice(&11u64.to_be_bytes());
    assert_eq!(
        codec::decode_git_allocation(&corrupt, server()),
        Err(CodecFailure::InvalidEncoding)
    );
}

#[test]
fn git_scope_codec_preserves_exact_sessions_work_generations_and_fallbacks() {
    use spaceterm_core::authority::*;
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let tab = TabId::new(server(), 2).unwrap();
    let pane = PaneId::new(server(), 3).unwrap();
    let worktree = WorktreeId::new(server(), 4).unwrap();
    let value = RetainedRemovalScope::new(RetainedRemovalScopeParts {
        server: server(),
        tabs: vec![spaceterm_core::organization::TabRecord {
            id: tab,
            workspace,
            worktree: Some(worktree),
            root_pane: pane,
            creation_ordinal: CreationOrdinal::new(1),
        }],
        membership_versions: vec![
            spaceterm_core::organization_retained::TabMembershipVersion::new(
                tab,
                FieldVersion::new(17),
            ),
        ],
        panes: vec![RetainedAffectedPane {
            hierarchy: spaceterm_core::operations::AssessedPane {
                workspace,
                tab,
                pane,
                session_generation: SessionGeneration::new(5),
                control_generation: ControlGeneration::new(6),
                context_generation: ContextGeneration::new(7),
            },
            session: Some(SessionKey {
                epoch: ServerEpoch::new(8),
                pane,
                generation: SessionGeneration::new(5),
            }),
            startup_reserved: false,
            work_generation: CloseWorkGeneration::new(9),
        }],
        fallbacks: vec![WorkspaceFallbackPlan {
            workspace,
            worktree: WorktreeId::new(server(), 10).unwrap(),
            directory: directory(),
        }],
    })
    .unwrap();
    let bytes = codec::encode_git_removal_scope(&value).unwrap();
    assert_eq!(
        codec::decode_git_removal_scope(&bytes, server()).unwrap(),
        value
    );
    assert_eq!(&bytes[4..6], &2u16.to_be_bytes());
    let mut legacy_parts = value.parts().clone();
    legacy_parts.membership_versions.clear();
    let legacy = RetainedRemovalScope::new(legacy_parts).unwrap();
    let legacy_bytes = codec::encode_git_removal_scope(&legacy).unwrap();
    assert_eq!(&legacy_bytes[4..6], &1u16.to_be_bytes());
    assert_eq!(
        codec::decode_git_removal_scope(&legacy_bytes, server()).unwrap(),
        legacy
    );
    let empty = RetainedRemovalScope::empty(server());
    assert_eq!(
        codec::decode_git_removal_scope(
            &codec::encode_git_removal_scope(&empty).unwrap(),
            server()
        )
        .unwrap(),
        empty
    );
    let oversized = codec::encode_record(RecordKind::GitRemovalScope, |writer| {
        writer.fixed(&server().bytes())?;
        writer.fixed(&4097u32.to_be_bytes())
    })
    .unwrap();
    assert_eq!(
        codec::decode_git_removal_scope(&oversized, server()),
        Err(CodecFailure::ResourceLimit)
    );
}

#[test]
fn external_result_v1_fixture_does_not_promote_uncertain_observations_to_success() {
    use spaceterm_core::worktrees::types::{ExternalDisposition, GitFailure};
    let value = RetainedExternalResult {
        disposition: ExternalDisposition::PartialOrAmbiguous,
        failure: Some(GitFailure::ExternalStateUncertain),
        ended_sessions: 16_384,
    };
    let fixture = b"STOS\0\x01\0\x0b\0\0\0\x07\x02\x01\x1e\0\0\x40\0";
    assert_eq!(codec::encode_git_external_result(&value).unwrap(), fixture);
    assert_eq!(codec::decode_git_external_result(fixture).unwrap(), value);
    let mut invalid = fixture.to_vec();
    invalid[14] = 255;
    assert_eq!(
        codec::decode_git_external_result(&invalid),
        Err(CodecFailure::InvalidEncoding)
    );
    let excess = RetainedExternalResult {
        ended_sessions: 16_385,
        ..value
    };
    assert_eq!(
        codec::encode_git_external_result(&excess),
        Err(CodecFailure::ResourceLimit)
    );
}

#[test]
fn native_retirement_codec_preserves_first_reason_and_cleanup_without_pairing_identity() {
    use spaceterm_core::authority::*;
    use spaceterm_core::native_management::*;
    use spaceterm_core::organization::{FailureClass, RetirementReason};
    use spaceterm_core::retirement::*;
    let pane = PaneId::new(server(), 3).unwrap();
    for action in [
        NativeRetirementAction::Stop,
        NativeRetirementAction::RestartApproved,
        NativeRetirementAction::ActivateUpdate,
    ] {
        for cleanup in [
            RetirementCleanupState::AwaitingEvidence,
            RetirementCleanupState::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
            RetirementCleanupState::Confirmed(
                ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
            ),
            RetirementCleanupState::Failed(FailureClass::RuntimeFault),
        ] {
            let value = NativeRetirementDescriptor::new(NativeRetirementDescriptorParts {
                request: NativeManagementRequestRef::new(
                    server(),
                    [1; 16],
                    spaceterm_core::operations::RequestDigest::from_bytes([2; 32]),
                )
                .unwrap(),
                batch: RetirementBatchId::new(server(), ServerEpoch::new(8), u64::MAX).unwrap(),
                action,
                acceptance_revision: OrganizationRevision::new(u64::MAX),
                panes: vec![
                    RetirementPane::new(
                        WorkspaceId::new(server(), 1).unwrap(),
                        TabId::new(server(), 2).unwrap(),
                        pane,
                        Some(WorktreeId::new(server(), 4).unwrap()),
                        SessionGeneration::new(6),
                    )
                    .unwrap(),
                ],
                sessions: vec![
                    RetiredSession::new(
                        SessionKey {
                            epoch: ServerEpoch::new(8),
                            pane,
                            generation: SessionGeneration::new(5),
                        },
                        RetirementReason::WorktreeRemoval,
                        cleanup,
                    )
                    .unwrap(),
                ],
            })
            .unwrap();
            let bytes = codec::encode_native_retirement(&value).unwrap();
            assert_eq!(
                codec::decode_native_retirement(&bytes, server()).unwrap(),
                value
            );
            assert_eq!(
                codec::decode_native_retirement(&bytes, ServerId::new([8; 16])),
                Err(CodecFailure::WrongServer)
            );
        }
    }
}

#[test]
fn record_writer_rejects_limit_plus_one_and_reader_preflights_truncated_counts() {
    let maximum =
        codec::encode_record(RecordKind::PairingKeys, |writer| writer.fixed(&[1; 64])).unwrap();
    assert_eq!(maximum.len(), 76);
    assert_eq!(
        codec::encode_record(RecordKind::PairingKeys, |writer| writer.fixed(&[1; 65])),
        Err(CodecFailure::ResourceLimit)
    );
    let truncated = codec::encode_record(RecordKind::CreationDefault, |writer| {
        writer.fixed(&server().bytes())?;
        writer.u64(5)?;
        writer.count(65_535, 65_535)
    })
    .unwrap();
    assert_eq!(
        codec::decode_creation_default(&truncated, server()),
        Err(CodecFailure::InvalidEncoding)
    );
}
