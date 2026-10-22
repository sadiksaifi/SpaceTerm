//! Owning-server directory values. None of these paths grant client-local filesystem authority.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub boot: [u8; 32],
    pub pane: [u8; 32],
    pub original: PathBuf,
}
impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirectoryRecoveryTarget(<redacted>)")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Begin { id: u64, operation: Operation },
    Poll { id: u64 },
    Cancel { id: u64 },
}
/// An explicit, idempotent filesystem write, independent of Workspace or pin edits.
/// Cancellation stops observation; directories already created are never rolled back.
#[derive(Clone, Serialize, Deserialize)]
pub enum CreationRequest {
    Begin { id: u64, path: PathBuf },
    Poll { id: u64 },
    Cancel { id: u64 },
}
impl std::fmt::Debug for CreationRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirectoryCreation(<redacted>)")
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Operation {
    Home,
    Ancestor { path: PathBuf },
    List { path: PathBuf },
    Validate { path: PathBuf },
}
impl std::fmt::Debug for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirectoryRead(<redacted>)")
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Value {
    Directory(PathBuf),
    Listing { names: Vec<String>, truncated: bool },
}
impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirectoryValue(<redacted>)")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Progress {
    Pending,
    Ready(Value),
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Error {
    Missing,
    NotDirectory,
    PermissionDenied,
    Unavailable,
    Capacity,
    Cancelled,
}
pub const MAX_ROWS: usize = 1024;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_LIST_BYTES: usize = 256 * 1024;
