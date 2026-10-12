//! Schema inventory and pre-migration checks through the existing SQLite authority.
//!
//! The authority must establish new-storage evidence and application ownership
//! before applying these migrations. Missing rows or an empty schema are not that
//! evidence. This module never opens, repairs, or replaces a database.

use rusqlite::types::FromSql;
use spaceterm_host::storage::{Database, Migration, StorageFailure, Transaction};

/// Fixed SQLite application ID, the big-endian ASCII bytes "STOR".
pub const APPLICATION_ID: u32 = 0x5354_4f52;
/// First organization schema. Protocol versions are independent.
pub const SCHEMA_VERSION: u32 = 1;
/// Version of store-owned field codecs and metadata, not ledger codecs.
pub const ENCODING_VERSION: u32 = 1;

const SQL: &str = include_str!("sql/001.sql");
const MIGRATIONS: &[Migration<'static>] = &[Migration { sql: SQL }];
const MAX_SCHEMA_SQL_BYTES: usize = 16 * 1024;

/// Exact table inventory. SQL definitions in 001.sql own columns and constraints.
pub const TABLES: &[&str] = &[
    "store_metadata",
    "identity_high_water",
    "workspaces",
    "workspace_field_versions",
    "workspace_name_bases",
    "tabs",
    "tab_field_versions",
    "panes",
    "pane_field_versions",
    "pane_generation_high_water",
    "tab_creation_replay",
    "repositories",
    "repository_field_versions",
    "worktrees",
    "worktree_inventory",
    "server_settings",
    "remote_bind_endpoints",
    "pairings",
    "paired_clients",
    "enrollment_authorizations",
    "pairing_receipts",
    "revocation_receipts",
    "operation_streams",
    "operation_entries",
    "git_repository_locators",
    "git_allocation_reservations",
    "git_write_intents",
    "native_retirements",
];

/// Ordered migrations for the authority's single physical migration transaction.
/// Callers must not use this list to adopt existing foreign or uninitialized data.
pub fn migrations() -> &'static [Migration<'static>] {
    MIGRATIONS
}

/// Checks a version before any migration. Newer schemas remain untouched.
pub fn check_version(found: u32) -> Result<(), StorageFailure> {
    if found > SCHEMA_VERSION {
        Err(StorageFailure::UnknownSchema {
            found,
            supported: SCHEMA_VERSION,
        })
    } else {
        Ok(())
    }
}

/// Checks store-owned record encoding independently of schema version.
pub fn check_encoding_version(found: u32) -> Result<(), StorageFailure> {
    if found > ENCODING_VERSION {
        return Err(StorageFailure::UnknownSchema {
            found,
            supported: ENCODING_VERSION,
        });
    }
    if found != ENCODING_VERSION {
        return Err(StorageFailure::InvalidState);
    }
    Ok(())
}

/// Rejects foreign/unsupported storage before a caller requests migrations.
/// Version zero is always recovery-required without provider-issued new evidence.
/// This check does not grant initialization authority or migrate anything.
pub fn validate_migration_source(database: &Database) -> Result<(), StorageFailure> {
    let found = database.schema_version()?;
    check_version(found)?;
    if found == 0 {
        let objects: i64 =
            database.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?;
        if objects != 0 {
            return Err(StorageFailure::InvalidState);
        }
        return Err(StorageFailure::RecoveryRequired);
    }
    validate_existing(database)
}

/// Checks application ownership and the exact v1 inventory after authority open.
/// This cannot substitute for the authority's pre-migration ownership check.
/// The current authority denies application-ID pragmas, including table functions;
/// that provider prerequisite fails closed rather than accepting metadata alone.
pub fn validate_existing(database: &Database) -> Result<(), StorageFailure> {
    let found = database.schema_version()?;
    check_version(found)?;
    if found != SCHEMA_VERSION {
        return Err(StorageFailure::InvalidState);
    }
    let application: u32 = database.query_row(
        "SELECT application_id FROM pragma_application_id",
        [],
        |row| row.get(0),
    )?;
    if application != APPLICATION_ID {
        return Err(StorageFailure::InvalidState);
    }
    validate_inventory(database)
}

/// Validates all table definitions and rejects extra objects without allocating
/// their SQL. Call within consistent authority recovery before exposing state.
/// Application ownership, integrity and foreign-key checks remain prerequisites.
pub fn validate_inventory(database: &Database) -> Result<(), StorageFailure> {
    validate(&DatabaseReader(database))
}

/// Validates the same inventory inside the caller's existing authority transaction.
/// This operation neither commits nor exposes the native SQLite connection.
pub fn validate_transaction_inventory(transaction: &Transaction<'_>) -> Result<(), StorageFailure> {
    validate(&TransactionReader(transaction))
}

trait SchemaReader {
    fn scalar<T: FromSql>(&self, sql: &str, table: &str) -> Result<T, StorageFailure>;
}
struct DatabaseReader<'a>(&'a Database);
impl SchemaReader for DatabaseReader<'_> {
    fn scalar<T: FromSql>(&self, sql: &str, table: &str) -> Result<T, StorageFailure> {
        self.0.query_row(sql, [table], |row| row.get(0))
    }
}
struct TransactionReader<'a, 'b>(&'a Transaction<'b>);
impl SchemaReader for TransactionReader<'_, '_> {
    fn scalar<T: FromSql>(&self, sql: &str, table: &str) -> Result<T, StorageFailure> {
        self.0.query_row(sql, [table], |row| row.get(0))
    }
}

fn validate(reader: &impl SchemaReader) -> Result<(), StorageFailure> {
    // Only declared tables and SQLite's own constraint indexes are allowed. A
    // user-created index has SQL; triggers/views/virtual shadow tables add objects.
    let objects: i64 = reader.scalar(
        "SELECT count(*) FROM sqlite_schema WHERE (type != 'index' OR sql IS NOT NULL) AND ?1 IS NOT NULL", "",
    )?;
    if objects != TABLES.len() as i64 {
        return Err(StorageFailure::InvalidState);
    }
    let invalid_indexes: i64 = reader.scalar(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'index' AND (sql IS NOT NULL OR name NOT GLOB 'sqlite_autoindex_*' OR tbl_name NOT IN (SELECT name FROM sqlite_schema WHERE type = 'table')) AND ?1 IS NOT NULL", "",
    )?;
    if invalid_indexes != 0 {
        return Err(StorageFailure::InvalidState);
    }
    for table in TABLES {
        let length: i64 = reader.scalar(
            "SELECT length(CAST(sql AS BLOB)) FROM sqlite_schema WHERE type = 'table' AND name = ?1", table,
        )?;
        if length <= 0 || length > MAX_SCHEMA_SQL_BYTES as i64 {
            return Err(StorageFailure::InvalidState);
        }
        let sql: String = reader.scalar("SELECT CASE WHEN length(CAST(sql AS BLOB)) <= 16384 THEN sql ELSE NULL END FROM sqlite_schema WHERE type = 'table' AND name = ?1", table)?;
        let expected = definition(table).ok_or(StorageFailure::InvalidState)?;
        // SQLite retains our original CREATE statement, without its semicolon.
        // Exact comparison also checks membership FKs, deferral and CHECK clauses.
        if sql.trim() != expected {
            return Err(StorageFailure::InvalidState);
        }
    }
    Ok(())
}

fn definition(table: &str) -> Option<&'static str> {
    SQL.split(';').find_map(|statement| {
        let start = statement.find("CREATE TABLE ")?;
        let definition = statement[start..].trim();
        let name = definition
            .strip_prefix("CREATE TABLE ")?
            .split_whitespace()
            .next()?;
        (name == table).then_some(definition)
    })
}
