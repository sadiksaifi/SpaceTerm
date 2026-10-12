//! One-time Worktree Location selection. Initialization data grants no native authority.

use super::{
    Health, SqliteOrganizationStore, StoreIdentity, StoreLimits, StoreServices,
    database_adapter::DatabaseAdapter,
};
use crate::git::path_template;
use crate::git::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE;
use crate::startup::BuildIdentity;
use crate::storage::Database;
use spaceterm_core::authority::{OrganizationRevision, StoreRevision};
use spaceterm_core::organization::WorktreeLocationTemplate;
use spaceterm_core::storage::{CommitReceipt, StoreFailure};
use thiserror::Error;

/// Invalid legacy initialization data, with no retained template or parser error.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("legacy Worktree Location is invalid")]
pub struct SeedFailure;

/// A validated legacy value. It is never read from the Settings Document here.
#[derive(Clone, Eq, PartialEq)]
pub struct LegacyWorktreeLocation(WorktreeLocationTemplate);
impl std::fmt::Debug for LegacyWorktreeLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LegacyWorktreeLocation([redacted])")
    }
}
impl LegacyWorktreeLocation {
    /// Applies the owning-host template policy and the core's content-free bound.
    pub fn parse(value: String) -> Result<Self, SeedFailure> {
        path_template::validate(&value).map_err(|_| SeedFailure)?;
        WorktreeLocationTemplate::new(value)
            .map(Self)
            .map_err(|_| SeedFailure)
    }
}

/// Only new local desktop creation may supply a legacy client preference.
#[derive(Debug)]
pub enum NewServerOrigin {
    /// Explicit new local desktop bootstrap, with an optional validated template.
    LocalDesktop {
        /// Already validated legacy schema-7 preference. No file authority is implied.
        legacy_worktree_location: Option<LegacyWorktreeLocation>,
    },
    /// New local headless bootstrap uses the server default permanently.
    LocalHeadless,
    /// New remote bootstrap uses the server default permanently.
    Remote,
}

/// Original native identity and initialization data, not creation permission.
#[derive(Debug)]
pub struct NewStoreSeed {
    /// Exact original account/build/server binding.
    pub identity: StoreIdentity,
    /// The one-time initialization origin.
    pub origin: NewServerOrigin,
}

/// Initialization shares the authority's uncertainty and receipt semantics.
#[must_use]
#[expect(
    clippy::large_enum_variant,
    reason = "Transfer the owning authority without a new allocation after its durable receipt."
)]
pub enum InitializationDisposition {
    /// Complete initialization and validation were confirmed durable.
    Durable {
        /// Healthy store bound to the original native identity.
        store: SqliteOrganizationStore,
        /// Receipt for the one initialization transaction.
        receipt: CommitReceipt,
    },
    /// Confirmed no initialization transaction was applied.
    NotApplied(StoreFailure),
    /// Disposal and fresh native authority recovery are required.
    Indeterminate,
}
impl std::fmt::Debug for InitializationDisposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Durable { .. } => f.write_str("InitializationDisposition::Durable(..)"),
            Self::NotApplied(error) => f
                .debug_tuple("InitializationDisposition::NotApplied")
                .field(error)
                .finish(),
            Self::Indeterminate => f.write_str("InitializationDisposition::Indeterminate"),
        }
    }
}

impl SqliteOrganizationStore {
    /// Initializes only storage exclusively created by this application opener.
    /// Existing empty or incomplete storage never authorizes replacement or seeding.
    /// Domain services validate all resulting records before the one durable commit.
    pub fn initialize_new(
        mut database: Database,
        seed: NewStoreSeed,
        limits: StoreLimits,
        services: StoreServices,
    ) -> InitializationDisposition {
        let prerequisites = validate_identity(seed.identity).and_then(|()| {
            database
                .verify_application(super::schema::APPLICATION_ID)
                .map_err(super::database_adapter::classify)?;
            database
                .claim_new_storage()
                .map_err(super::database_adapter::classify)?;
            super::schema::validate_existing(&database).map_err(super::database_adapter::classify)
        });
        if let Err(error) = prerequisites {
            return InitializationDisposition::NotApplied(error);
        }
        Self::initialize_validated(database, seed, limits, services)
    }

    pub(super) fn initialize_validated(
        database: Database,
        seed: NewStoreSeed,
        limits: StoreLimits,
        services: StoreServices,
    ) -> InitializationDisposition {
        let (origin, legacy, template) = match selection(&seed.origin) {
            Ok(value) => value,
            Err(error) => return InitializationDisposition::NotApplied(error),
        };
        let mut adapter = DatabaseAdapter::new(database);
        let identity = seed.identity;
        let disposition = adapter.commit(|sql| {
            // Missing metadata alone cannot distinguish a fixture from partially
            // initialized state. Any retained row rejects the whole bootstrap.
            for table in super::schema::TABLES {
                let count: u64 = sql.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0)).map_err(super::database_adapter::classify)?;
                if count != 0 {
                    return Err(spaceterm_core::storage::StoreFailure::InvalidTransaction);
                }
            }
            let zero = 0_u64.to_be_bytes();
            let one = 1_u64.to_be_bytes();
            let build = match identity.account.build_identity { BuildIdentity::Release => 0, BuildIdentity::Preflight => 1, BuildIdentity::Development => 2 };
            sql.execute("INSERT INTO store_metadata VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10)", rusqlite::params![super::schema::APPLICATION_ID, super::schema::ENCODING_VERSION, identity.server.bytes().as_slice(), u64::from(identity.account.account_uid).to_be_bytes().as_slice(), build, identity.server_public_key.as_slice(), one.as_slice(), zero.as_slice(), origin, legacy]).map_err(super::database_adapter::classify)?;
            sql.execute("INSERT INTO identity_high_water VALUES (1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1)", [zero.as_slice()]).map_err(super::database_adapter::classify)?;
            sql.execute("INSERT INTO server_settings VALUES (1, ?1, ?2, ?2)", rusqlite::params![template.as_str(), one.as_slice()]).map_err(super::database_adapter::classify)?;
            super::recovery::load(sql, identity, &limits, &services)?;
            Ok(CommitReceipt { store_revision: StoreRevision::new(1), organization_revision: OrganizationRevision::new(0) })
        });
        match disposition {
            spaceterm_core::storage::CommitDisposition::Durable(receipt) => {
                InitializationDisposition::Durable {
                    store: Self {
                        database: adapter,
                        identity,
                        limits,
                        services,
                        health: Health::Healthy,
                    },
                    receipt,
                }
            }
            spaceterm_core::storage::CommitDisposition::NotApplied(error) => {
                InitializationDisposition::NotApplied(error)
            }
            spaceterm_core::storage::CommitDisposition::Indeterminate => {
                InitializationDisposition::Indeterminate
            }
        }
    }
}

pub(super) fn validate_identity(identity: StoreIdentity) -> Result<(), StoreFailure> {
    if identity.server.bytes() == [0; 16]
        || identity.server_public_key == [0; 32]
        || identity.account.account_uid == 0
        || identity.account.account_uid == u32::MAX
    {
        return Err(StoreFailure::IdentityMismatch);
    }
    Ok(())
}

pub(super) fn selection(
    origin: &NewServerOrigin,
) -> Result<(i64, i64, WorktreeLocationTemplate), StoreFailure> {
    let (origin, value) = match origin {
        NewServerOrigin::LocalDesktop {
            legacy_worktree_location,
        } => (0, legacy_worktree_location.as_ref()),
        NewServerOrigin::LocalHeadless => (1, None),
        NewServerOrigin::Remote => (2, None),
    };
    if let Some(value) = value {
        Ok((origin, 1, value.0.clone()))
    } else {
        Ok((
            origin,
            0,
            WorktreeLocationTemplate::new(DEFAULT_WORKTREE_PATH_TEMPLATE.into())
                .map_err(|_| StoreFailure::InvalidTransaction)?,
        ))
    }
}
