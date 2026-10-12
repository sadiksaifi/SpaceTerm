//! Retained view document and its publication policy, outside the Settings
//! Document and outside preference import, export and reset.
//!
//! Version 1 is a flat big-endian binary document:
//!
//! ```text
//! document    = magic "SPTMVIEW", version u32 (1), payload-length u64, payload
//! payload     = view-revision u64, incarnation-high-water u64,
//!               server-count u32, server-id [16 bytes] * server-count,
//!               attachment-count u32, attachment * attachment-count,
//!               active-attachment u32 (index, or 0xFFFF_FFFF for none)
//! attachment  = server-index u32, workspace u64, pairing u64, client u64,
//!               incarnation u64, availability u8, tab-count u32,
//!               (tab u64, arrangement) * tab-count,
//!               active-tab u64 (0 for none), history-count u32, tab u64 * history-count
//! availability = 0 available | 1 limited by view Tabs | 2 limited by view Panes
//!               | 3 limited by Tab Panes
//! arrangement = root u32, node-count u32, node * node-count, split-high-water u64,
//!               focused-pane u64, focus-count u32, pane u64 * focus-count,
//!               zoomed-pane u64 (0 for none)
//! node        = 0u8 pane u64
//!             | 1u8 split-id u64 axis u8 (0 horizontal, 1 vertical)
//!               ratio-bits u32 (IEEE 754 binary32) first u32 second u32
//! ```
//!
//! Identities are local values under the attachment's Server. The document
//! holds no Server names, paths, pins, directories, Worktree membership,
//! control grants, Session keys, connection bindings, startup intents,
//! operation outcomes, terminal or clipboard content, Selection, Find, scroll
//! position, credentials or pending release and revocation tasks. Decoding
//! checks every count against its limit and against the remaining bytes before
//! allocating, and rejects unknown tags, truncation and trailing data.

use std::collections::BTreeSet;

use spaceterm_core::identity::{ClientId, PairingId, PaneId, ServerId, TabId, WorkspaceId};
use spaceterm_core::organization::SplitAxis;

use super::arrangement::{
    ArrangementNode, ArrangementRecord, MAX_TAB_NODES, PaneArrangement, TabOrder, TabOrderRecord,
};
use super::state::{ViewAvailability, WorkspaceAttachment};
use super::types::{
    AttachmentIncarnation, MAX_TAB_PANES, MAX_VIEW_ATTACHMENTS, MAX_VIEW_DOCUMENT_BYTES,
    MAX_VIEW_PANES, MAX_VIEW_TABS, SplitId, ViewError, ViewResource, ViewRevision,
};

/// The retained view format this build writes and reads.
pub const VIEW_FORMAT_VERSION: u32 = 1;

const MAGIC: &[u8; 8] = b"SPTMVIEW";
const NO_ACTIVE_ATTACHMENT: u32 = u32::MAX;
const PANE_TAG: u8 = 0;
const SPLIT_TAG: u8 = 1;
const AVAILABLE: u8 = 0;
const LIMITED_TABS: u8 = 1;
const LIMITED_PANES: u8 = 2;
const LIMITED_TAB_PANES: u8 = 3;
const SERVER_BYTES: usize = 16;
/// Smallest encodings, used to bound counts by the remaining bytes.
const MIN_NODE_BYTES: usize = 1 + 8;
const MIN_ARRANGEMENT_BYTES: usize = 4 + 4 + MIN_NODE_BYTES + 8 + 8 + 4 + 8;
const MIN_TAB_BYTES: usize = 8 + MIN_ARRANGEMENT_BYTES;
const MIN_ATTACHMENT_BYTES: usize = 4 + 8 * 4 + 1 + 4 + 8 + 4;

const INVALID: ViewError = ViewError::InvalidPersistedState;

const fn limit(resource: ViewResource) -> ViewError {
    ViewError::ResourceLimit { resource }
}

/// Unvalidated retained view. It becomes a [`PersistedView`] only through the
/// checked constructor.
#[derive(Clone, Debug, PartialEq)]
pub struct PersistedViewParts {
    pub revision: ViewRevision,
    /// The last attachment incarnation issued, or zero before any.
    pub incarnation_high_water: u64,
    /// Attachments in personal Workspace order.
    pub workspaces: Vec<PersistedWorkspace>,
    /// None exactly when there are no attachments.
    pub active: Option<WorkspaceId>,
}

/// One attachment's retained personal view.
#[derive(Clone, Debug, PartialEq)]
pub struct PersistedWorkspace {
    pub attachment: WorkspaceAttachment,
    /// A Resource Limited Workspace restores limited: its pruned arrangement
    /// is never presented as complete before a Ready reconcile shows that its
    /// membership fits. Only Tab, Pane and Tab Pane limits apply.
    pub availability: ViewAvailability,
    pub tabs: TabOrderRecord,
    /// One arrangement for each Tab of `tabs.order`, in that order.
    pub arrangements: Vec<ArrangementRecord>,
}

/// A validated retained view.
#[derive(Clone, Debug, PartialEq)]
pub struct PersistedView(PersistedViewParts);

/// The encoded bytes of a validated retained view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedView(Vec<u8>);

impl EncodedView {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl PersistedView {
    /// Validates bounds, unique Workspaces, Tabs, Panes and incarnations
    /// across the view, every Tab order and arrangement, and the Active
    /// Workspace.
    pub fn new(parts: PersistedViewParts) -> Result<Self, ViewError> {
        validate(&parts)?;
        Ok(Self(parts))
    }

    pub fn parts(&self) -> &PersistedViewParts {
        &self.0
    }

    pub fn into_parts(self) -> PersistedViewParts {
        self.0
    }

    pub fn encode(&self) -> Result<EncodedView, ViewError> {
        let parts = &self.0;
        let mut servers: Vec<ServerId> = Vec::new();
        for workspace in &parts.workspaces {
            let server = workspace.attachment.server();
            if !servers.contains(&server) {
                servers.push(server);
            }
        }
        let mut out = Writer::default();
        out.bytes(MAGIC);
        out.u32(VIEW_FORMAT_VERSION);
        out.u64(0);
        let payload = out.0.len();
        out.u64(parts.revision.get());
        out.u64(parts.incarnation_high_water);
        out.count(servers.len())?;
        for server in &servers {
            out.bytes(&server.bytes());
        }
        out.count(parts.workspaces.len())?;
        for workspace in &parts.workspaces {
            let attachment = workspace.attachment;
            let server = servers
                .iter()
                .position(|server| *server == attachment.server())
                .ok_or(INVALID)?;
            out.count(server)?;
            out.u64(attachment.workspace().local());
            out.u64(attachment.pairing().local());
            out.u64(attachment.client().local());
            out.u64(attachment.incarnation().get());
            out.u8(availability_tag(workspace.availability)?);
            out.count(workspace.tabs.order.len())?;
            for (tab, arrangement) in workspace.tabs.order.iter().zip(&workspace.arrangements) {
                out.u64(tab.local());
                out.arrangement(arrangement)?;
            }
            out.u64(workspace.tabs.active.map_or(0, TabId::local));
            out.count(workspace.tabs.activation_history.len())?;
            for tab in &workspace.tabs.activation_history {
                out.u64(tab.local());
            }
        }
        let active = match parts.active {
            None => NO_ACTIVE_ATTACHMENT,
            Some(active) => {
                let index = parts
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.attachment.workspace() == active)
                    .ok_or(INVALID)?;
                u32::try_from(index).map_err(|_| INVALID)?
            }
        };
        out.u32(active);
        let length = u64::try_from(out.0.len() - payload).map_err(|_| INVALID)?;
        out.0[payload - 8..payload].copy_from_slice(&length.to_be_bytes());
        if out.0.len() > MAX_VIEW_DOCUMENT_BYTES {
            return Err(limit(ViewResource::DocumentBytes));
        }
        Ok(EncodedView(out.0))
    }

    /// Decodes and validates a retained document. A newer version is
    /// [`ViewError::UnsupportedFormat`]; anything malformed is
    /// [`ViewError::InvalidPersistedState`].
    pub fn decode(bytes: &[u8]) -> Result<Self, ViewError> {
        if bytes.len() > MAX_VIEW_DOCUMENT_BYTES {
            return Err(limit(ViewResource::DocumentBytes));
        }
        let mut reader = Reader { bytes, at: 0 };
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(INVALID);
        }
        let version = reader.u32()?;
        if version > VIEW_FORMAT_VERSION {
            return Err(ViewError::UnsupportedFormat { version });
        }
        if version != VIEW_FORMAT_VERSION {
            return Err(INVALID);
        }
        let length = reader.u64()?;
        if usize::try_from(length).ok() != Some(reader.remaining()) {
            return Err(INVALID);
        }
        let parts = reader.view()?;
        if reader.remaining() != 0 {
            return Err(INVALID);
        }
        Self::new(parts)
    }
}

fn validate(parts: &PersistedViewParts) -> Result<(), ViewError> {
    if parts.workspaces.len() > MAX_VIEW_ATTACHMENTS {
        return Err(limit(ViewResource::Attachments));
    }
    let mut workspaces = BTreeSet::new();
    let mut incarnations = BTreeSet::new();
    let mut tabs = BTreeSet::new();
    let mut panes = BTreeSet::new();
    let mut tab_total = 0_usize;
    let mut pane_total = 0_usize;
    for workspace in &parts.workspaces {
        let attachment = workspace.attachment;
        if !workspaces.insert(attachment.workspace())
            || !incarnations.insert(attachment.incarnation())
            || attachment.incarnation().get() > parts.incarnation_high_water
        {
            return Err(INVALID);
        }
        availability_tag(workspace.availability)?;
        tab_total = tab_total.saturating_add(workspace.tabs.order.len());
        if tab_total > MAX_VIEW_TABS {
            return Err(limit(ViewResource::Tabs));
        }
        if workspace.arrangements.len() != workspace.tabs.order.len() {
            return Err(INVALID);
        }
        TabOrder::import(attachment.workspace(), &workspace.tabs)?;
        for (tab, record) in workspace.tabs.order.iter().zip(&workspace.arrangements) {
            if !tabs.insert(*tab) {
                return Err(INVALID);
            }
            let leaves = record
                .nodes
                .iter()
                .filter(|node| matches!(node, ArrangementNode::Pane(_)))
                .count();
            pane_total = pane_total.saturating_add(leaves);
            if pane_total > MAX_VIEW_PANES {
                return Err(limit(ViewResource::Panes));
            }
            let arrangement = PaneArrangement::import(*tab, record)?;
            if !arrangement
                .pane_ids()
                .into_iter()
                .all(|pane| panes.insert(pane))
            {
                return Err(INVALID);
            }
        }
    }
    let consistent = match parts.active {
        None => workspaces.is_empty(),
        Some(active) => workspaces.contains(&active),
    };
    if consistent { Ok(()) } else { Err(INVALID) }
}

/// The retained tag of a Workspace's availability. Only per-view Tab and Pane
/// limits and the per-Tab Pane limit can limit a Workspace.
fn availability_tag(availability: ViewAvailability) -> Result<u8, ViewError> {
    match availability {
        ViewAvailability::Available => Ok(AVAILABLE),
        ViewAvailability::ResourceLimited { resource } => match resource {
            ViewResource::Tabs => Ok(LIMITED_TABS),
            ViewResource::Panes => Ok(LIMITED_PANES),
            ViewResource::TabPanes => Ok(LIMITED_TAB_PANES),
            ViewResource::Attachments | ViewResource::DocumentBytes => Err(INVALID),
        },
    }
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn bytes(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_be_bytes());
    }

    fn count(&mut self, value: usize) -> Result<(), ViewError> {
        self.u32(u32::try_from(value).map_err(|_| INVALID)?);
        Ok(())
    }

    fn arrangement(&mut self, record: &ArrangementRecord) -> Result<(), ViewError> {
        self.u32(record.root);
        self.count(record.nodes.len())?;
        for node in &record.nodes {
            match *node {
                ArrangementNode::Pane(pane) => {
                    self.u8(PANE_TAG);
                    self.u64(pane.local());
                }
                ArrangementNode::Split {
                    id,
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    self.u8(SPLIT_TAG);
                    self.u64(id.get());
                    self.u8(match axis {
                        SplitAxis::Horizontal => 0,
                        SplitAxis::Vertical => 1,
                    });
                    self.u32(ratio.to_bits());
                    self.u32(first);
                    self.u32(second);
                }
            }
        }
        self.u64(record.split_high_water);
        self.u64(record.focused.local());
        self.count(record.focus_history.len())?;
        for pane in &record.focus_history {
            self.u64(pane.local());
        }
        self.u64(record.zoomed.map_or(0, PaneId::local));
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], ViewError> {
        let end = self.at.checked_add(count).ok_or(INVALID)?;
        let bytes = self.bytes.get(self.at..end).ok_or(INVALID)?;
        self.at = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ViewError> {
        self.take(N)?.try_into().map_err(|_| INVALID)
    }

    fn u8(&mut self) -> Result<u8, ViewError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, ViewError> {
        self.array().map(u32::from_be_bytes)
    }

    fn u64(&mut self) -> Result<u64, ViewError> {
        self.array().map(u64::from_be_bytes)
    }

    /// Reads a count, rejecting one past `maximum` with `exceeded` and one the
    /// remaining bytes cannot hold as malformed, before any allocation.
    fn count(
        &mut self,
        maximum: usize,
        exceeded: ViewError,
        element_bytes: usize,
    ) -> Result<usize, ViewError> {
        let count = usize::try_from(self.u32()?).map_err(|_| INVALID)?;
        if count > maximum {
            return Err(exceeded);
        }
        if count
            .checked_mul(element_bytes)
            .is_none_or(|bytes| bytes > self.remaining())
        {
            return Err(INVALID);
        }
        Ok(count)
    }

    fn view(&mut self) -> Result<PersistedViewParts, ViewError> {
        let revision = ViewRevision::new(self.u64()?);
        let incarnation_high_water = self.u64()?;
        let server_count = self.count(
            MAX_VIEW_ATTACHMENTS,
            limit(ViewResource::Attachments),
            SERVER_BYTES,
        )?;
        let mut servers = Vec::with_capacity(server_count);
        for _ in 0..server_count {
            let server = ServerId::new(self.array()?);
            if servers.contains(&server) {
                return Err(INVALID);
            }
            servers.push(server);
        }
        let attachment_count = self.count(
            MAX_VIEW_ATTACHMENTS,
            limit(ViewResource::Attachments),
            MIN_ATTACHMENT_BYTES,
        )?;
        let mut referenced = vec![false; server_count];
        let mut totals = Totals::default();
        let mut workspaces = Vec::with_capacity(attachment_count);
        for _ in 0..attachment_count {
            let index = usize::try_from(self.u32()?).map_err(|_| INVALID)?;
            let server = *servers.get(index).ok_or(INVALID)?;
            referenced[index] = true;
            workspaces.push(self.workspace(server, &mut totals)?);
        }
        if referenced.contains(&false) {
            return Err(INVALID);
        }
        let active = match self.u32()? {
            NO_ACTIVE_ATTACHMENT => None,
            index => {
                let index = usize::try_from(index).map_err(|_| INVALID)?;
                let workspace: &PersistedWorkspace = workspaces.get(index).ok_or(INVALID)?;
                Some(workspace.attachment.workspace())
            }
        };
        Ok(PersistedViewParts {
            revision,
            incarnation_high_water,
            workspaces,
            active,
        })
    }

    fn workspace(
        &mut self,
        server: ServerId,
        totals: &mut Totals,
    ) -> Result<PersistedWorkspace, ViewError> {
        let workspace = WorkspaceId::new(server, self.u64()?).map_err(|_| INVALID)?;
        let pairing = PairingId::new(server, self.u64()?).map_err(|_| INVALID)?;
        let client = ClientId::new(server, self.u64()?).map_err(|_| INVALID)?;
        let incarnation = AttachmentIncarnation::new(self.u64()?).ok_or(INVALID)?;
        let attachment = WorkspaceAttachment::new(workspace, pairing, client, incarnation)?;
        let limited = |resource| ViewAvailability::ResourceLimited { resource };
        let availability = match self.u8()? {
            AVAILABLE => ViewAvailability::Available,
            LIMITED_TABS => limited(ViewResource::Tabs),
            LIMITED_PANES => limited(ViewResource::Panes),
            LIMITED_TAB_PANES => limited(ViewResource::TabPanes),
            _ => return Err(INVALID),
        };
        let tab_count = self.count(
            MAX_VIEW_TABS - totals.tabs,
            limit(ViewResource::Tabs),
            MIN_TAB_BYTES,
        )?;
        totals.tabs += tab_count;
        let mut order = Vec::with_capacity(tab_count);
        let mut arrangements = Vec::with_capacity(tab_count);
        for _ in 0..tab_count {
            order.push(self.tab(server)?);
            arrangements.push(self.arrangement(server, totals)?);
        }
        let active = match self.u64()? {
            0 => None,
            local => Some(TabId::new(server, local).map_err(|_| INVALID)?),
        };
        let history_count = self.count(tab_count, INVALID, 8)?;
        let mut activation_history = Vec::with_capacity(history_count);
        for _ in 0..history_count {
            activation_history.push(self.tab(server)?);
        }
        Ok(PersistedWorkspace {
            attachment,
            availability,
            tabs: TabOrderRecord {
                order,
                active,
                activation_history,
            },
            arrangements,
        })
    }

    fn tab(&mut self, server: ServerId) -> Result<TabId, ViewError> {
        TabId::new(server, self.u64()?).map_err(|_| INVALID)
    }

    fn pane(&mut self, server: ServerId) -> Result<PaneId, ViewError> {
        PaneId::new(server, self.u64()?).map_err(|_| INVALID)
    }

    fn arrangement(
        &mut self,
        server: ServerId,
        totals: &mut Totals,
    ) -> Result<ArrangementRecord, ViewError> {
        let root = self.u32()?;
        let node_count =
            self.count(MAX_TAB_NODES, limit(ViewResource::TabPanes), MIN_NODE_BYTES)?;
        let mut nodes = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            nodes.push(match self.u8()? {
                PANE_TAG => {
                    totals.panes += 1;
                    if totals.panes > MAX_VIEW_PANES {
                        return Err(limit(ViewResource::Panes));
                    }
                    ArrangementNode::Pane(self.pane(server)?)
                }
                SPLIT_TAG => ArrangementNode::Split {
                    id: SplitId::new(self.u64()?).ok_or(INVALID)?,
                    axis: match self.u8()? {
                        0 => SplitAxis::Horizontal,
                        1 => SplitAxis::Vertical,
                        _ => return Err(INVALID),
                    },
                    ratio: f32::from_bits(self.u32()?),
                    first: self.u32()?,
                    second: self.u32()?,
                },
                _ => return Err(INVALID),
            });
        }
        let split_high_water = self.u64()?;
        let focused = self.pane(server)?;
        let focus_count = self.count(MAX_TAB_PANES, limit(ViewResource::TabPanes), 8)?;
        let mut focus_history = Vec::with_capacity(focus_count);
        for _ in 0..focus_count {
            focus_history.push(self.pane(server)?);
        }
        let zoomed = match self.u64()? {
            0 => None,
            local => Some(PaneId::new(server, local).map_err(|_| INVALID)?),
        };
        Ok(ArrangementRecord {
            root,
            nodes,
            split_high_water,
            focused,
            focus_history,
            zoomed,
        })
    }
}

#[derive(Default)]
struct Totals {
    tabs: usize,
    panes: usize,
}

/// A typed store failure. It carries no path or native error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ViewStoreFailureClass {
    Unavailable,
    Locked,
    Unsafe,
    /// The current publication is not the expected one.
    Conflict,
    /// The document exceeds the requested or supported size.
    TooLarge,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ViewStoreStage {
    Load,
    Write,
    Sync,
    Publish,
    Verify,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ViewStoreFailure {
    pub class: ViewStoreFailureClass,
    pub stage: ViewStoreStage,
}

impl ViewStoreFailure {
    pub const fn error(self) -> ViewError {
        match self.class {
            ViewStoreFailureClass::Unavailable | ViewStoreFailureClass::Locked => {
                ViewError::StorageUnavailable
            }
            ViewStoreFailureClass::Unsafe => ViewError::UnsafeStorage,
            ViewStoreFailureClass::Conflict => ViewError::PublicationConflict,
            ViewStoreFailureClass::TooLarge => limit(ViewResource::DocumentBytes),
        }
    }
}

/// One published document and its opaque publication token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredView<V> {
    pub version: V,
    pub bytes: Vec<u8>,
}

/// Outcome of one publication attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewCommit<V> {
    /// The document is durably published with this token.
    Durable(V),
    /// Nothing was published.
    NotApplied(ViewStoreFailure),
    /// The document may or may not have been published.
    Indeterminate,
}

/// Injected retained-view storage. The native adapter keeps filesystem
/// identity inside its version token and never uses Settings import or reset.
///
/// `load` returns `None` only when no document exists. Locked, unavailable or
/// unsafe storage is a failure, never a missing document. A document larger
/// than `maximum_bytes` is a `TooLarge` failure and is not read.
///
/// `replace` is a compare-and-swap. It publishes only when the current
/// publication is `expected`, where `None` means that no document exists. A
/// different current publication is a `Conflict` that publishes nothing.
/// `Durable` is reported only after the document is durable.
pub trait ViewStore: Send {
    type Version: Clone + Send;

    fn load(
        &mut self,
        maximum_bytes: usize,
    ) -> Result<Option<StoredView<Self::Version>>, ViewStoreFailure>;

    fn replace(
        &mut self,
        expected: Option<&Self::Version>,
        document: &EncodedView,
    ) -> ViewCommit<Self::Version>;
}

enum Publication<V> {
    /// No load has validated the published document or found none yet.
    Unloaded,
    /// Saves replace exactly this publication. `None` expects no document.
    Expected(Option<V>),
    /// Saves are refused until a load validates the published document.
    Blocked(ViewError),
}

/// Retained-view publication policy over an injected store.
///
/// A save replaces only the publication this facade last validated, so it
/// never overwrites a competing writer. Saves are refused before the first
/// successful load, and a conflict, an indeterminate outcome, a failed load or
/// a document that cannot be read blocks further saves until [`Self::recover`]
/// validates the published document. Nothing infers a missing document from
/// storage it could not read. An unsupported or malformed document is
/// preserved: nothing rewrites it with defaults.
pub struct ViewPersistence<S: ViewStore> {
    store: S,
    publication: Publication<S::Version>,
}

impl<S: ViewStore> ViewPersistence<S> {
    /// Saves are refused with [`ViewError::StorageUnavailable`] until a load
    /// validates the published document or finds none.
    pub fn new(store: S) -> Self {
        Self {
            store,
            publication: Publication::Unloaded,
        }
    }

    /// Loads the published document. `None` means no document exists. The
    /// launch passes either outcome to
    /// [`ClientViewState::restore`](crate::view::ClientViewState::restore), so
    /// durable suppression facts apply even without a document.
    pub fn load(&mut self) -> Result<Option<PersistedView>, ViewError> {
        self.reload()
    }

    /// Reloads and validates the published document after a conflict, an
    /// indeterminate publication or a crash. A valid result unblocks saves
    /// against that publication; the caller decides whether to save again.
    pub fn recover(&mut self) -> Result<Option<PersistedView>, ViewError> {
        self.reload()
    }

    /// Publishes a view over the last validated publication.
    pub fn save(&mut self, view: &PersistedView) -> Result<(), ViewError> {
        let expected = match &self.publication {
            Publication::Unloaded => return Err(ViewError::StorageUnavailable),
            Publication::Blocked(error) => return Err(*error),
            Publication::Expected(version) => version.clone(),
        };
        let document = view.encode()?;
        match self.store.replace(expected.as_ref(), &document) {
            ViewCommit::Durable(version) => {
                self.publication = Publication::Expected(Some(version));
                Ok(())
            }
            ViewCommit::NotApplied(failure) => {
                let error = failure.error();
                if failure.class == ViewStoreFailureClass::Conflict {
                    self.publication = Publication::Blocked(error);
                }
                Err(error)
            }
            ViewCommit::Indeterminate => {
                let error = ViewError::IndeterminatePublication;
                self.publication = Publication::Blocked(error);
                Err(error)
            }
        }
    }

    /// The failure that currently refuses saves, if any.
    pub fn blocked(&self) -> Option<ViewError> {
        match self.publication {
            Publication::Unloaded => Some(ViewError::StorageUnavailable),
            Publication::Blocked(error) => Some(error),
            Publication::Expected(_) => None,
        }
    }

    fn reload(&mut self) -> Result<Option<PersistedView>, ViewError> {
        let stored = match self.store.load(MAX_VIEW_DOCUMENT_BYTES) {
            Ok(stored) => stored,
            Err(failure) => {
                // Unread storage may hold any document, or one this facade
                // has not validated: nothing may replace it. An earlier
                // block, such as an indeterminate publication, keeps its
                // reason.
                let error = failure.error();
                if !matches!(self.publication, Publication::Blocked(_)) {
                    self.publication = Publication::Blocked(error);
                }
                return Err(error);
            }
        };
        let Some(stored) = stored else {
            self.publication = Publication::Expected(None);
            return Ok(None);
        };
        match PersistedView::decode(&stored.bytes) {
            Ok(view) => {
                self.publication = Publication::Expected(Some(stored.version));
                Ok(Some(view))
            }
            Err(error) => {
                self.publication = Publication::Blocked(error);
                Err(error)
            }
        }
    }
}
