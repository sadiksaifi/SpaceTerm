//! One-time Worktree Location selection. Initialization data grants no native authority.

use super::{SqliteOrganizationStore, StoreIdentity, StoreLimits};
use crate::git::path_template;
#[cfg(feature = "test-support")]
use crate::git::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE;
use crate::storage::Database;
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
    /// Fails closed until the SQLite owner supplies retained new-storage and
    /// pre-migration application-ownership evidence. An empty inventory is not
    /// evidence, and this entry point never writes initialization data.
    pub fn initialize_new(
        database: Database,
        seed: NewStoreSeed,
        limits: StoreLimits,
    ) -> InitializationDisposition {
        let _ = limits;
        if let Err(error) = database
            .verify_authority()
            .map_err(super::database_adapter::classify)
        {
            return InitializationDisposition::NotApplied(error);
        }
        if let Err(error) = database
            .schema_version()
            .map_err(super::database_adapter::classify)
            .and_then(|version| {
                super::schema::check_version(version).map_err(super::database_adapter::classify)
            })
        {
            return InitializationDisposition::NotApplied(error);
        }
        if let Err(error) = validate_identity(seed.identity) {
            return InitializationDisposition::NotApplied(error);
        }
        InitializationDisposition::NotApplied(StoreFailure::RecoveryRequired)
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

#[cfg(feature = "test-support")]
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
