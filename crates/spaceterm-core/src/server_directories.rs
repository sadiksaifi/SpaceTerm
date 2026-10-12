//! Owning-server directory query values. None is native filesystem authority.

use crate::authority::ServerEpoch;
use crate::identity::{ClientId, PairingId, ServerId};
use crate::organization::ServerDirectory;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::NonZeroU64;

/// Further reduce a page when encoded bytes reach the page ceiling.
pub const MAX_DIRECTORY_PAGE_ENTRIES: usize = 128;
/// Includes page metadata and cursor, not merely entry names.
pub const MAX_DIRECTORY_PAGE_BYTES: usize = 48 * 1024;

/// Classified directory failure with no path or native error text.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DirectoryFailure {
    Unauthorized,
    WrongServer,
    InvalidDirectory,
    Unavailable,
    Missing,
    NotDirectory,
    IdentityChanged,
    Capacity,
    StaleCursor,
    Unchecked,
}
impl fmt::Display for DirectoryFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Directory request failed ({self:?})")
    }
}
impl std::error::Error for DirectoryFailure {}

/// Comparison context supplied only after ordinary server authentication.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DirectoryContextParts {
    pub server: ServerId,
    pub epoch: ServerEpoch,
    pub pairing: PairingId,
    pub client: ClientId,
}
/// Structural checking cannot manufacture an authenticated principal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "DirectoryContextParts", into = "DirectoryContextParts")]
pub struct DirectoryContext(DirectoryContextParts);
impl DirectoryContext {
    pub fn new(parts: DirectoryContextParts) -> Result<Self, DirectoryFailure> {
        if parts.server != parts.pairing.server() || parts.server != parts.client.server() {
            return Err(DirectoryFailure::WrongServer);
        }
        Ok(Self(parts))
    }
    pub const fn parts(self) -> DirectoryContextParts {
        self.0
    }
    pub const fn server(self) -> ServerId {
        self.0.server
    }
}
impl TryFrom<DirectoryContextParts> for DirectoryContext {
    type Error = DirectoryFailure;
    fn try_from(parts: DirectoryContextParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<DirectoryContext> for DirectoryContextParts {
    fn from(value: DirectoryContext) -> Self {
        value.0
    }
}

/// Cursor input. The provider retains the actual directory observation and native handles.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct DirectoryCursorParts {
    pub context: DirectoryContext,
    pub scope: ServerDirectory,
    pub observation: NonZeroU64,
    pub random: [u8; 32],
}
/// Opaque continuation reference bound to one principal, epoch, scope and observation.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "DirectoryCursorParts", into = "DirectoryCursorParts")]
pub struct DirectoryCursor(DirectoryCursorParts);
impl DirectoryCursor {
    pub fn new(parts: DirectoryCursorParts) -> Result<Self, DirectoryFailure> {
        if parts.scope.server() != parts.context.server() {
            return Err(DirectoryFailure::WrongServer);
        }
        if parts.random == [0; 32] {
            return Err(DirectoryFailure::StaleCursor);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &DirectoryCursorParts {
        &self.0
    }
    pub fn matches(
        &self,
        context: DirectoryContext,
        scope: &ServerDirectory,
        observation: Option<NonZeroU64>,
    ) -> bool {
        self.0.context == context
            && &self.0.scope == scope
            && observation.is_none_or(|v| v == self.0.observation)
    }
}
impl fmt::Debug for DirectoryCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DirectoryCursor([redacted])")
    }
}
impl TryFrom<DirectoryCursorParts> for DirectoryCursor {
    type Error = DirectoryFailure;
    fn try_from(parts: DirectoryCursorParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<DirectoryCursor> for DirectoryCursorParts {
    fn from(value: DirectoryCursor) -> Self {
        value.0
    }
}

/// Distinguishes policy checks without permitting filesystem mutation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DirectoryPurpose {
    WorkspacePin,
    SessionLaunch,
    PaneRepair,
    WorktreeCreateTarget,
    WorktreeFallback,
}
/// Read actions only. Mutations continue through identified core Commands.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DirectoryQueryAction {
    Home,
    Browse {
        directory: ServerDirectory,
        cursor: Option<DirectoryCursor>,
    },
    Validate {
        directory: ServerDirectory,
        purpose: DirectoryPurpose,
    },
}
/// Raw query input; conversion must invoke the checked constructor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DirectoryQueryParts {
    pub context: DirectoryContext,
    pub action: DirectoryQueryAction,
}
/// Checked portable query. The server also compares context to its authenticated connection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "DirectoryQueryParts", into = "DirectoryQueryParts")]
pub struct DirectoryQuery(DirectoryQueryParts);
impl DirectoryQuery {
    pub fn new(parts: DirectoryQueryParts) -> Result<Self, DirectoryFailure> {
        match &parts.action {
            DirectoryQueryAction::Home => {}
            DirectoryQueryAction::Validate { directory, .. } => {
                if directory.server() != parts.context.server() {
                    return Err(DirectoryFailure::WrongServer);
                }
            }
            DirectoryQueryAction::Browse { directory, cursor } => {
                if directory.server() != parts.context.server() {
                    return Err(DirectoryFailure::WrongServer);
                }
                if cursor
                    .as_ref()
                    .is_some_and(|c| !c.matches(parts.context, directory, None))
                {
                    return Err(DirectoryFailure::StaleCursor);
                }
            }
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &DirectoryQueryParts {
        &self.0
    }
}
impl TryFrom<DirectoryQueryParts> for DirectoryQuery {
    type Error = DirectoryFailure;
    fn try_from(parts: DirectoryQueryParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<DirectoryQuery> for DirectoryQueryParts {
    fn from(value: DirectoryQuery) -> Self {
        value.0
    }
}

/// One bounded filesystem basename. It may be non-UTF-8; presentation sanitizes separately.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "Vec<u8>", into = "Vec<u8>")]
pub struct DirectoryEntryName(Vec<u8>);
impl DirectoryEntryName {
    pub fn new(bytes: Vec<u8>) -> Result<Self, DirectoryFailure> {
        if bytes.is_empty()
            || bytes.len() > 255
            || bytes.contains(&0)
            || bytes.contains(&b'/')
            || bytes == b"."
            || bytes == b".."
        {
            return Err(DirectoryFailure::InvalidDirectory);
        }
        Ok(Self(bytes))
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for DirectoryEntryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DirectoryEntryName([redacted])")
    }
}
impl TryFrom<Vec<u8>> for DirectoryEntryName {
    type Error = DirectoryFailure;
    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Self::new(bytes)
    }
}
impl From<DirectoryEntryName> for Vec<u8> {
    fn from(value: DirectoryEntryName) -> Self {
        value.0
    }
}

/// Display/selectability observation, not permission or a retained directory lease.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DirectoryEntry {
    pub name: DirectoryEntryName,
    pub directory: ServerDirectory,
    pub selectable: bool,
}
/// Page input. Decode bounds must precede collection allocation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DirectoryPageParts {
    pub context: DirectoryContext,
    pub directory: ServerDirectory,
    pub observation: NonZeroU64,
    pub entries: Vec<DirectoryEntry>,
    pub next: Option<DirectoryCursor>,
}
/// Bounded page from one exact directory observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "DirectoryPageParts", into = "DirectoryPageParts")]
pub struct DirectoryPage(DirectoryPageParts);
impl DirectoryPage {
    pub fn new(parts: DirectoryPageParts) -> Result<Self, DirectoryFailure> {
        if parts.directory.server() != parts.context.server() {
            return Err(DirectoryFailure::WrongServer);
        }
        if parts.entries.len() > MAX_DIRECTORY_PAGE_ENTRIES {
            return Err(DirectoryFailure::Capacity);
        }
        if parts
            .next
            .as_ref()
            .is_some_and(|c| !c.matches(parts.context, &parts.directory, Some(parts.observation)))
        {
            return Err(DirectoryFailure::StaleCursor);
        }
        let mut bytes = 256 + parts.directory.bytes().len();
        if let Some(cursor) = &parts.next {
            bytes += 256 + cursor.parts().scope.bytes().len();
        }
        for (index, entry) in parts.entries.iter().enumerate() {
            if entry.directory.server() != parts.context.server() {
                return Err(DirectoryFailure::WrongServer);
            }
            if parts.entries[..index]
                .iter()
                .any(|earlier| earlier.name == entry.name)
            {
                return Err(DirectoryFailure::InvalidDirectory);
            }
            bytes = bytes
                .checked_add(128 + entry.name.bytes().len() + entry.directory.bytes().len())
                .ok_or(DirectoryFailure::Capacity)?;
        }
        if bytes > MAX_DIRECTORY_PAGE_BYTES {
            return Err(DirectoryFailure::Capacity);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &DirectoryPageParts {
        &self.0
    }
    pub fn complete(&self) -> bool {
        self.0.next.is_none()
    }
}
impl TryFrom<DirectoryPageParts> for DirectoryPage {
    type Error = DirectoryFailure;
    fn try_from(parts: DirectoryPageParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<DirectoryPage> for DirectoryPageParts {
    fn from(value: DirectoryPage) -> Self {
        value.0
    }
}

/// Classification only. A native launch/repair adapter must retain and revalidate authority separately.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DirectoryValidationClass {
    AvailableDirectory,
    AbsentCreateTarget,
    EmptyCreateTarget,
    Missing,
    NotDirectory,
    Unavailable,
    IdentityChanged,
    Unchecked,
}
/// Read-side validation result for the exact requested value and purpose.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DirectoryValidation {
    pub directory: ServerDirectory,
    pub purpose: DirectoryPurpose,
    pub class: DirectoryValidationClass,
}
/// Query results are never organization writes or client-local capabilities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DirectoryQueryResult {
    Home(ServerDirectory),
    Page(Box<DirectoryPage>),
    Validation(DirectoryValidation),
}
/// Physical containment observation. Failed validation is always Unchecked, never Outside.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryContainment {
    Inside,
    Outside,
    Unchecked,
}

/// Read-only provider port, called on an owned blocking worker.
/// Authentication, query quotas and cursor-cache ownership remain with server composition.
/// Returned facts do not replace the provider's private native leases.
pub trait ServerDirectoryQueries: Send + Sync {
    fn query(&self, request: &DirectoryQuery) -> Result<DirectoryQueryResult, DirectoryFailure>;
    fn containment(
        &self,
        ancestor: &ServerDirectory,
        candidate: &ServerDirectory,
    ) -> Result<DirectoryContainment, DirectoryFailure>;
}
