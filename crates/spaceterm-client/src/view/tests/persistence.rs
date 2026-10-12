//! Retained view document, its bounds and the publication policy over an
//! injected store. The scripted memory store stands in for the native adapter,
//! which desktop owns; it models compare-and-swap publication and crashes
//! before or after publication.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use spaceterm_core::authority::{ControlGeneration, SessionGeneration};
use spaceterm_core::organization::{ControlRecord, SessionPhase, SplitAxis, SplitEdge};

use super::{
    A, B, Org, Runtime, attach, attached, chain, client, metrics, pairing, pane, ready,
    retained_workspace, single, space, tab, ws,
};
use crate::view::{
    ArrangementEdit, ArrangementNode, ArrangementRecord, AttachmentIncarnation, AttachmentRemoval,
    ClientViewState, EncodedView, MAX_VIEW_DOCUMENT_BYTES, PersistedView, PersistedViewParts,
    PersistedWorkspace, RestorationFilter, SplitId, StoredView, VIEW_FORMAT_VERSION,
    ViewAvailability, ViewChange, ViewCommit, ViewError, ViewPersistence, ViewResource,
    ViewRevision, ViewStore, ViewStoreFailure, ViewStoreFailureClass, ViewStoreStage,
    WorkspaceAttachment,
};

const INVALID: ViewError = ViewError::InvalidPersistedState;

const fn limit(resource: ViewResource) -> ViewError {
    ViewError::ResourceLimit { resource }
}

// Scripted store.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Call {
    Load,
    Replace { expected: Option<u64> },
}

#[derive(Clone, Copy, Debug)]
enum Script {
    Fail(ViewStoreFailure),
    /// The process dies before the new document becomes current.
    CrashBeforePublish,
    /// The new document becomes current, then the reply is lost.
    CrashAfterPublish,
}

#[derive(Default)]
struct Disk {
    document: Option<(u64, Vec<u8>)>,
    last_version: u64,
    calls: Vec<Call>,
    load_failure: Option<ViewStoreFailure>,
    scripts: VecDeque<Script>,
    /// A careless adapter whose `replace` with no expected publication
    /// overwrites any current document.
    blind_create: bool,
}

impl Disk {
    fn publish(&mut self, bytes: Vec<u8>) -> u64 {
        self.last_version += 1;
        self.document = Some((self.last_version, bytes));
        self.last_version
    }
}

/// One shared disk; each clone is one process's handle to it.
#[derive(Clone, Default)]
struct MemoryStore(Arc<Mutex<Disk>>);

impl MemoryStore {
    fn with_document(bytes: Vec<u8>) -> Self {
        let store = Self::default();
        store.0.lock().unwrap().publish(bytes);
        store
    }

    fn bytes(&self) -> Option<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .document
            .as_ref()
            .map(|(_, bytes)| bytes.clone())
    }

    /// Another writer publishes a document.
    fn compete(&self, bytes: Vec<u8>) {
        self.0.lock().unwrap().publish(bytes);
    }

    fn script(&self, script: Script) {
        self.0.lock().unwrap().scripts.push_back(script);
    }

    fn fail_loads(&self, failure: Option<ViewStoreFailure>) {
        self.0.lock().unwrap().load_failure = failure;
    }

    fn create_blindly(&self) {
        self.0.lock().unwrap().blind_create = true;
    }

    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().calls.clone()
    }

    fn replaces(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| matches!(call, Call::Replace { .. }))
            .count()
    }
}

impl ViewStore for MemoryStore {
    type Version = u64;

    fn load(&mut self, maximum_bytes: usize) -> Result<Option<StoredView<u64>>, ViewStoreFailure> {
        let mut disk = self.0.lock().unwrap();
        disk.calls.push(Call::Load);
        if let Some(failure) = disk.load_failure {
            return Err(failure);
        }
        match &disk.document {
            None => Ok(None),
            Some((_, bytes)) if bytes.len() > maximum_bytes => Err(ViewStoreFailure {
                class: ViewStoreFailureClass::TooLarge,
                stage: ViewStoreStage::Load,
            }),
            Some((version, bytes)) => Ok(Some(StoredView {
                version: *version,
                bytes: bytes.clone(),
            })),
        }
    }

    fn replace(&mut self, expected: Option<&u64>, document: &EncodedView) -> ViewCommit<u64> {
        let mut disk = self.0.lock().unwrap();
        disk.calls.push(Call::Replace {
            expected: expected.copied(),
        });
        let script = disk.scripts.pop_front();
        if let Some(Script::Fail(failure)) = script {
            return ViewCommit::NotApplied(failure);
        }
        let current = disk.document.as_ref().map(|(version, _)| *version);
        let blind = disk.blind_create && expected.is_none();
        if current != expected.copied() && !blind {
            return ViewCommit::NotApplied(ViewStoreFailure {
                class: ViewStoreFailureClass::Conflict,
                stage: ViewStoreStage::Publish,
            });
        }
        match script {
            Some(Script::CrashBeforePublish) => ViewCommit::Indeterminate,
            Some(Script::CrashAfterPublish) => {
                disk.publish(document.as_bytes().to_vec());
                ViewCommit::Indeterminate
            }
            Some(Script::Fail(_)) | None => {
                ViewCommit::Durable(disk.publish(document.as_bytes().to_vec()))
            }
        }
    }
}

// Views built through the public interfaces.

/// Server A Workspace 1 with Tab 10 split three ways and Tab 11; Server B
/// Workspace 1 with Tab 10. Personal order, ratio, axis, focus and zoom differ
/// from the defaults.
fn personal_state() -> ClientViewState {
    let mut first = Org::new(A);
    first
        .add_workspace(1, 10, 100)
        .split(10, 101, 100, SplitAxis::Horizontal, SplitEdge::After)
        .split(10, 102, 101, SplitAxis::Vertical, SplitEdge::Before)
        .add_tab(1, 11, 110, None);
    let mut second = Org::new(B);
    second.add_workspace(1, 10, 100);
    let snapshot = first.snapshot();
    let mut state = ClientViewState::new();
    attach(&mut state, &first, ready(A), 1).unwrap();
    attach(&mut state, &second, ready(B), 1).unwrap();
    let workspace = ws(A, 1);
    state.move_tab(workspace, tab(A, 11), 0, &snapshot).unwrap();
    let outer = SplitId::new(1).unwrap();
    let inner = SplitId::new(2).unwrap();
    state
        .arrange(
            workspace,
            tab(A, 10),
            ArrangementEdit::Resize {
                split: outer,
                available: space(),
                metrics: metrics(),
                ratio: 0.3,
            },
        )
        .unwrap();
    state
        .arrange(
            workspace,
            tab(A, 10),
            ArrangementEdit::SetSplitAxis {
                split: inner,
                axis: SplitAxis::Horizontal,
                available: space(),
                metrics: metrics(),
            },
        )
        .unwrap();
    state
        .arrange(workspace, tab(A, 10), ArrangementEdit::Focus(pane(A, 102)))
        .unwrap();
    state
        .arrange(workspace, tab(A, 10), ArrangementEdit::ToggleZoom)
        .unwrap();
    state.activate_tab(workspace, tab(A, 11)).unwrap();
    state.activate_workspace(workspace).unwrap();
    state
}

fn personal_view() -> PersistedView {
    personal_state().persisted().unwrap()
}

/// A different valid view, as another writer would publish.
fn other_view() -> PersistedView {
    let mut org = Org::new(A);
    org.add_workspace(5, 50, 500);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 5).unwrap();
    state.persisted().unwrap()
}

fn encoded(view: &PersistedView) -> Vec<u8> {
    view.encode().unwrap().into_bytes()
}

// Raw version-1 writer for documents the encoder never produces.

#[derive(Clone, Debug)]
enum RawNode {
    Pane(u64),
    Split {
        id: u64,
        axis: u8,
        ratio: u32,
        first: u32,
        second: u32,
    },
    Tag(u8),
}

fn split(id: u64, ratio: f32, first: u32, second: u32) -> RawNode {
    RawNode::Split {
        id,
        axis: 0,
        ratio: ratio.to_bits(),
        first,
        second,
    }
}

#[derive(Clone, Debug)]
struct RawArrangement {
    root: u32,
    nodes: Vec<RawNode>,
    high_water: u64,
    focused: u64,
    focus: Vec<u64>,
    zoomed: u64,
}

impl RawArrangement {
    fn single(pane: u64) -> Self {
        Self {
            root: 0,
            nodes: vec![RawNode::Pane(pane)],
            high_water: 0,
            focused: pane,
            focus: Vec::new(),
            zoomed: 0,
        }
    }

    fn from_record(record: &ArrangementRecord) -> Self {
        Self {
            root: record.root,
            nodes: record
                .nodes
                .iter()
                .map(|node| match *node {
                    ArrangementNode::Pane(pane) => RawNode::Pane(pane.local()),
                    ArrangementNode::Split {
                        id,
                        axis,
                        ratio,
                        first,
                        second,
                    } => RawNode::Split {
                        id: id.get(),
                        axis: u8::from(axis == SplitAxis::Vertical),
                        ratio: ratio.to_bits(),
                        first,
                        second,
                    },
                })
                .collect(),
            high_water: record.split_high_water,
            focused: record.focused.local(),
            focus: record
                .focus_history
                .iter()
                .map(|pane| pane.local())
                .collect(),
            zoomed: record.zoomed.map_or(0, |pane| pane.local()),
        }
    }
}

#[derive(Clone, Debug)]
struct RawAttachment {
    server: u32,
    workspace: u64,
    pairing: u64,
    client: u64,
    incarnation: u64,
    availability: u8,
    tabs: Vec<(u64, RawArrangement)>,
    active_tab: u64,
    history: Vec<u64>,
}

impl RawAttachment {
    /// Workspace `local` with incarnation `local` and one single-Pane Tab.
    fn filler(local: u64) -> Self {
        Self::with_tabs(local, vec![(local, RawArrangement::single(local))])
    }

    fn with_tabs(local: u64, tabs: Vec<(u64, RawArrangement)>) -> Self {
        let first = tabs.first().map_or(0, |(tab, _)| *tab);
        Self {
            server: 0,
            workspace: local,
            pairing: 1,
            client: 1,
            incarnation: local,
            availability: 0,
            tabs,
            active_tab: first,
            history: if first == 0 { Vec::new() } else { vec![first] },
        }
    }
}

#[derive(Clone, Debug)]
struct RawView {
    version: u32,
    revision: u64,
    high_water: u64,
    servers: Vec<[u8; 16]>,
    attachments: Vec<RawAttachment>,
    active: u32,
    /// Replaces the computed payload length.
    length: Option<u64>,
    trailing: Vec<u8>,
}

impl RawView {
    fn new(attachments: Vec<RawAttachment>) -> Self {
        let high_water = attachments
            .iter()
            .map(|attachment| attachment.incarnation)
            .max()
            .unwrap_or(0);
        let active = if attachments.is_empty() { u32::MAX } else { 0 };
        Self {
            version: VIEW_FORMAT_VERSION,
            revision: 1,
            high_water,
            servers: vec![[A; 16]],
            attachments,
            active,
            length: None,
            trailing: Vec::new(),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        put64(&mut payload, self.revision);
        put64(&mut payload, self.high_water);
        put32(&mut payload, count(self.servers.len()));
        for server in &self.servers {
            payload.extend_from_slice(server);
        }
        put32(&mut payload, count(self.attachments.len()));
        for attachment in &self.attachments {
            put32(&mut payload, attachment.server);
            put64(&mut payload, attachment.workspace);
            put64(&mut payload, attachment.pairing);
            put64(&mut payload, attachment.client);
            put64(&mut payload, attachment.incarnation);
            payload.push(attachment.availability);
            put32(&mut payload, count(attachment.tabs.len()));
            for (tab, arrangement) in &attachment.tabs {
                put64(&mut payload, *tab);
                put32(&mut payload, arrangement.root);
                put32(&mut payload, count(arrangement.nodes.len()));
                for node in &arrangement.nodes {
                    match *node {
                        RawNode::Pane(pane) => {
                            payload.push(0);
                            put64(&mut payload, pane);
                        }
                        RawNode::Split {
                            id,
                            axis,
                            ratio,
                            first,
                            second,
                        } => {
                            payload.push(1);
                            put64(&mut payload, id);
                            payload.push(axis);
                            put32(&mut payload, ratio);
                            put32(&mut payload, first);
                            put32(&mut payload, second);
                        }
                        RawNode::Tag(tag) => {
                            payload.push(tag);
                            put64(&mut payload, 1);
                        }
                    }
                }
                put64(&mut payload, arrangement.high_water);
                put64(&mut payload, arrangement.focused);
                put32(&mut payload, count(arrangement.focus.len()));
                for pane in &arrangement.focus {
                    put64(&mut payload, *pane);
                }
                put64(&mut payload, arrangement.zoomed);
            }
            put64(&mut payload, attachment.active_tab);
            put32(&mut payload, count(attachment.history.len()));
            for tab in &attachment.history {
                put64(&mut payload, *tab);
            }
        }
        put32(&mut payload, self.active);
        payload.extend_from_slice(&self.trailing);
        let length = self
            .length
            .unwrap_or_else(|| u64::try_from(payload.len()).unwrap());
        let mut out = b"SPTMVIEW".to_vec();
        put32(&mut out, self.version);
        put64(&mut out, length);
        out.extend_from_slice(&payload);
        out
    }

    fn decode(&self) -> Result<PersistedView, ViewError> {
        PersistedView::decode(&self.bytes())
    }
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap()
}

/// One Server A attachment: Tab 10 is `H(100,101)` and Tab 11 holds 110.
fn base() -> RawView {
    RawView::new(vec![RawAttachment {
        server: 0,
        workspace: 1,
        pairing: 1,
        client: 1,
        incarnation: 1,
        availability: 0,
        tabs: vec![
            (
                10,
                RawArrangement {
                    root: 0,
                    nodes: vec![split(1, 0.5, 1, 2), RawNode::Pane(100), RawNode::Pane(101)],
                    high_water: 1,
                    focused: 100,
                    focus: vec![100],
                    zoomed: 0,
                },
            ),
            (11, RawArrangement::single(110)),
        ],
        active_tab: 10,
        history: vec![11, 10],
    }])
}

fn attachment(raw: &mut RawView) -> &mut RawAttachment {
    &mut raw.attachments[0]
}

fn arrangement(raw: &mut RawView) -> &mut RawArrangement {
    &mut raw.attachments[0].tabs[0].1
}

fn root_split(raw: &mut RawView) -> &mut RawNode {
    &mut arrangement(raw).nodes[0]
}

fn set_ratio(raw: &mut RawView, value: f32) {
    if let RawNode::Split { ratio, .. } = root_split(raw) {
        *ratio = value.to_bits();
    }
}

fn set_children(raw: &mut RawView, left: u32, right: u32) {
    if let RawNode::Split { first, second, .. } = root_split(raw) {
        *first = left;
        *second = right;
    }
}

// Round trip and fixture.

#[test]
fn view_v1_round_trip_preserves_attachments_order_tree_ratios_focus_and_zoom() {
    let state = personal_state();
    let view = state.persisted().unwrap();
    let decoded = PersistedView::decode(view.encode().unwrap().as_bytes()).unwrap();
    assert_eq!(decoded, view);

    let parts = decoded.parts();
    assert_eq!(parts.revision, state.revision());
    assert_eq!(parts.active, Some(ws(A, 1)));
    assert_eq!(parts.workspaces[0].attachment, state.attachments()[0]);
    assert_eq!(parts.workspaces[1].attachment, state.attachments()[1]);
    let first = &parts.workspaces[0];
    assert_eq!(first.tabs.order, vec![tab(A, 11), tab(A, 10)]);
    assert_eq!(first.tabs.active, Some(tab(A, 11)));
    assert_eq!(first.tabs.activation_history, vec![tab(A, 10), tab(A, 11)]);
    let record = &first.arrangements[1];
    assert_eq!(record.focused, pane(A, 102));
    assert_eq!(record.zoomed, Some(pane(A, 102)));
    assert_eq!(record.focus_history, vec![pane(A, 100), pane(A, 102)]);
    let splits: Vec<(u64, SplitAxis, u32)> = record
        .nodes
        .iter()
        .filter_map(|node| match *node {
            ArrangementNode::Split {
                id, axis, ratio, ..
            } => Some((id.get(), axis, ratio.to_bits())),
            ArrangementNode::Pane(_) => None,
        })
        .collect();
    assert_eq!(
        splits,
        vec![
            (1, SplitAxis::Horizontal, 0.3_f32.to_bits()),
            (2, SplitAxis::Horizontal, 0.5_f32.to_bits()),
        ]
    );

    // Restoring and persisting again is the identity.
    let (restored_state, update) =
        ClientViewState::restore(Some(&decoded), &RestorationFilter::new()).unwrap();
    assert!(update.is_empty());
    assert_eq!(restored_state.persisted().unwrap(), view);
    assert_eq!(restored_state.attachments(), state.attachments());
    assert_eq!(restored_state.active_workspace(), state.active_workspace());

    // And through the publication facade.
    let store = MemoryStore::default();
    let mut persistence = ViewPersistence::new(store.clone());
    assert_eq!(persistence.load().unwrap(), None);
    persistence.save(&view).unwrap();
    let mut relaunched = ViewPersistence::new(store);
    assert_eq!(relaunched.load().unwrap(), Some(view));
}

/// Workspace 1 of Server A: Tab 10 is `H(100,101)` with ratio 0.25, Pane 101
/// focused and zoomed.
#[rustfmt::skip]
const FIXTURE: &[u8] = &[
    // magic "SPTMVIEW", version 1, payload length 205
    b'S', b'P', b'T', b'M', b'V', b'I', b'E', b'W',
    0, 0, 0, 1,
    0, 0, 0, 0, 0, 0, 0, 205,
    // view revision 4, incarnation high water 1
    0, 0, 0, 0, 0, 0, 0, 4,
    0, 0, 0, 0, 0, 0, 0, 1,
    // one Server: [7; 16]
    0, 0, 0, 1,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    // one attachment: Server 0, Workspace 1, pairing 1, Client 1, incarnation 1
    0, 0, 0, 1,
    0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 1,
    0, 0, 0, 0, 0, 0, 0, 1,
    0, 0, 0, 0, 0, 0, 0, 1,
    0, 0, 0, 0, 0, 0, 0, 1,
    // available
    0,
    // one Tab: Tab 10
    0, 0, 0, 1,
    0, 0, 0, 0, 0, 0, 0, 10,
    // arrangement: root 0, three nodes
    0, 0, 0, 0,
    0, 0, 0, 3,
    // Split 1, horizontal, ratio 0.25 (0x3E80_0000), children 1 and 2
    1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0x3E, 0x80, 0, 0, 0, 0, 0, 1, 0, 0, 0, 2,
    // Pane 100, Pane 101
    0, 0, 0, 0, 0, 0, 0, 0, 100,
    0, 0, 0, 0, 0, 0, 0, 0, 101,
    // Split high water 1, focused Pane 101
    0, 0, 0, 0, 0, 0, 0, 1,
    0, 0, 0, 0, 0, 0, 0, 101,
    // focus history: 100, 101
    0, 0, 0, 2,
    0, 0, 0, 0, 0, 0, 0, 100,
    0, 0, 0, 0, 0, 0, 0, 101,
    // zoomed Pane 101
    0, 0, 0, 0, 0, 0, 0, 101,
    // active Tab 10, activation history: 10
    0, 0, 0, 0, 0, 0, 0, 10,
    0, 0, 0, 1,
    0, 0, 0, 0, 0, 0, 0, 10,
    // active attachment 0
    0, 0, 0, 0,
];

fn fixture_state(org: &Org) -> ClientViewState {
    let mut state = ClientViewState::new();
    attach(&mut state, org, ready(A), 1).unwrap();
    let workspace = ws(A, 1);
    state
        .arrange(
            workspace,
            tab(A, 10),
            ArrangementEdit::Resize {
                split: SplitId::new(1).unwrap(),
                available: space(),
                metrics: metrics(),
                ratio: 0.25,
            },
        )
        .unwrap();
    state
        .arrange(workspace, tab(A, 10), ArrangementEdit::Focus(pane(A, 101)))
        .unwrap();
    state
        .arrange(workspace, tab(A, 10), ArrangementEdit::ToggleZoom)
        .unwrap();
    state
}

#[test]
fn view_v1_fixture_is_stable_and_contains_no_runtime_or_terminal_state() {
    let mut plain = Org::new(A);
    plain
        .add_workspace(1, 10, 100)
        .split(10, 101, 100, SplitAxis::Horizontal, SplitEdge::After);
    let encoded_plain = encoded(&fixture_state(&plain).persisted().unwrap());
    assert_eq!(encoded_plain.len(), FIXTURE.len());
    assert_eq!(encoded_plain, FIXTURE);

    // Names, directories, Session phases, generations and control grants do
    // not reach the document.
    let mut busy = plain.clone();
    busy.relabel("secret-workspace-name", b"/secret/directory")
        .set_runtime(
            100,
            Runtime {
                session: 41,
                phase: SessionPhase::Running {
                    generation: SessionGeneration::new(41),
                },
                control_generation: 9,
                control: ControlRecord::Held {
                    client: client(A, 1),
                    generation: ControlGeneration::new(9),
                },
            },
        );
    let encoded_busy = encoded(&fixture_state(&busy).persisted().unwrap());
    assert_eq!(encoded_busy, FIXTURE);
    for needle in [&b"secret"[..], b"/secret/directory", b"work"] {
        assert!(!FIXTURE.windows(needle.len()).any(|window| window == needle));
    }

    let decoded = PersistedView::decode(FIXTURE).unwrap();
    assert_eq!(decoded, fixture_state(&plain).persisted().unwrap());
}

// Load outcomes and publication policy.

#[test]
fn missing_document_loads_empty_view() {
    let store = MemoryStore::default();
    let mut persistence = ViewPersistence::new(store.clone());
    assert_eq!(persistence.load().unwrap(), None);
    assert_eq!(persistence.blocked(), None);
    assert_eq!(store.calls(), vec![Call::Load]);

    // The launch view for a missing document is the empty view.
    let (state, update) = ClientViewState::restore(None, &RestorationFilter::new()).unwrap();
    assert!(update.is_empty());
    assert_eq!(state, ClientViewState::new());

    // The first save publishes only where no document exists.
    persistence.save(&personal_view()).unwrap();
    assert_eq!(
        store.calls(),
        vec![Call::Load, Call::Replace { expected: None }]
    );
    assert_eq!(store.bytes(), Some(encoded(&personal_view())));
}

#[test]
fn unknown_schema_and_malformed_document_are_not_overwritten() {
    let mut newer = encoded(&personal_view());
    newer[8..12].copy_from_slice(&2_u32.to_be_bytes());
    let mut malformed = encoded(&personal_view());
    malformed.push(0);
    for (bytes, error) in [
        (newer, ViewError::UnsupportedFormat { version: 2 }),
        (malformed, INVALID),
    ] {
        let store = MemoryStore::with_document(bytes.clone());
        let mut persistence = ViewPersistence::new(store.clone());
        assert_eq!(persistence.load().unwrap_err(), error);
        assert_eq!(persistence.blocked(), Some(error));

        // Nothing rewrites the preserved document.
        assert_eq!(persistence.save(&personal_view()).unwrap_err(), error);
        assert_eq!(store.replaces(), 0);
        assert_eq!(store.bytes(), Some(bytes));

        // Once another writer publishes a valid document, recovery validates
        // it and saves resume over it.
        store.compete(encoded(&other_view()));
        assert_eq!(persistence.recover().unwrap(), Some(other_view()));
        assert_eq!(persistence.blocked(), None);
        persistence.save(&personal_view()).unwrap();
        assert_eq!(store.bytes(), Some(encoded(&personal_view())));
    }
}

#[test]
fn locked_storage_is_not_treated_as_missing() {
    for (class, error) in [
        (ViewStoreFailureClass::Locked, ViewError::StorageUnavailable),
        (
            ViewStoreFailureClass::Unavailable,
            ViewError::StorageUnavailable,
        ),
        (ViewStoreFailureClass::Unsafe, ViewError::UnsafeStorage),
    ] {
        let existing = encoded(&other_view());
        let store = MemoryStore::with_document(existing.clone());
        // Even an adapter that would overwrite an unread document is never
        // asked to.
        store.create_blindly();
        store.fail_loads(Some(ViewStoreFailure {
            class,
            stage: ViewStoreStage::Load,
        }));
        let mut persistence = ViewPersistence::new(store.clone());
        assert_eq!(persistence.load().unwrap_err(), error);
        assert_eq!(persistence.blocked(), Some(error));

        // A save cannot replace the unread document.
        assert_eq!(persistence.save(&personal_view()).unwrap_err(), error);
        assert_eq!(store.replaces(), 0);
        assert_eq!(store.bytes(), Some(existing.clone()));

        store.fail_loads(None);
        assert_eq!(persistence.recover().unwrap(), Some(other_view()));
        persistence.save(&personal_view()).unwrap();
        assert_eq!(
            store.calls().last(),
            Some(&Call::Replace { expected: Some(1) })
        );
    }
}

#[test]
fn saves_wait_for_a_validated_load() {
    let existing = encoded(&other_view());
    let store = MemoryStore::with_document(existing.clone());
    store.create_blindly();

    // Before any load the facade cannot know what a save would replace.
    let mut persistence = ViewPersistence::new(store.clone());
    assert_eq!(persistence.blocked(), Some(ViewError::StorageUnavailable));
    assert_eq!(
        persistence.save(&personal_view()).unwrap_err(),
        ViewError::StorageUnavailable
    );
    assert_eq!(store.replaces(), 0);
    assert_eq!(store.bytes(), Some(existing.clone()));

    // After a validated load, a reload that cannot read the store blocks
    // saves again until one succeeds.
    assert_eq!(persistence.load().unwrap(), Some(other_view()));
    assert_eq!(persistence.blocked(), None);
    store.fail_loads(Some(ViewStoreFailure {
        class: ViewStoreFailureClass::Unsafe,
        stage: ViewStoreStage::Load,
    }));
    assert_eq!(persistence.recover().unwrap_err(), ViewError::UnsafeStorage);
    assert_eq!(persistence.blocked(), Some(ViewError::UnsafeStorage));
    assert_eq!(
        persistence.save(&personal_view()).unwrap_err(),
        ViewError::UnsafeStorage
    );
    assert_eq!(store.replaces(), 0);
    assert_eq!(store.bytes(), Some(existing));

    store.fail_loads(None);
    assert_eq!(persistence.recover().unwrap(), Some(other_view()));
    persistence.save(&personal_view()).unwrap();
    assert_eq!(
        store.calls().last(),
        Some(&Call::Replace { expected: Some(1) })
    );
}

#[test]
fn publication_conflict_preserves_competing_document() {
    let store = MemoryStore::with_document(encoded(&personal_view()));
    let mut persistence = ViewPersistence::new(store.clone());
    persistence.load().unwrap();

    let competing = encoded(&other_view());
    store.compete(competing.clone());
    assert_eq!(
        persistence.save(&personal_view()).unwrap_err(),
        ViewError::PublicationConflict
    );
    assert_eq!(store.bytes(), Some(competing.clone()));
    assert_eq!(persistence.blocked(), Some(ViewError::PublicationConflict));

    // Further saves are refused without touching the store.
    let replaces = store.replaces();
    assert_eq!(
        persistence.save(&personal_view()).unwrap_err(),
        ViewError::PublicationConflict
    );
    assert_eq!(store.replaces(), replaces);
    assert_eq!(store.bytes(), Some(competing));

    // Recovery surfaces the competing view; the caller decides to save.
    assert_eq!(persistence.recover().unwrap(), Some(other_view()));
    persistence.save(&personal_view()).unwrap();
    assert_eq!(
        store.calls().last(),
        Some(&Call::Replace { expected: Some(2) })
    );
}

#[test]
fn indeterminate_publication_blocks_saves_until_validated_recovery() {
    let store = MemoryStore::default();
    let mut persistence = ViewPersistence::new(store.clone());
    persistence.load().unwrap();
    store.script(Script::CrashBeforePublish);
    let error = ViewError::IndeterminatePublication;
    assert_eq!(persistence.save(&personal_view()).unwrap_err(), error);
    assert_eq!(persistence.blocked(), Some(error));
    assert_eq!(persistence.save(&personal_view()).unwrap_err(), error);
    assert_eq!(store.replaces(), 1);

    // A recovery that cannot read the document keeps saves blocked.
    store.fail_loads(Some(ViewStoreFailure {
        class: ViewStoreFailureClass::Locked,
        stage: ViewStoreStage::Load,
    }));
    assert_eq!(
        persistence.recover().unwrap_err(),
        ViewError::StorageUnavailable
    );
    assert_eq!(persistence.blocked(), Some(error));
    assert_eq!(persistence.save(&personal_view()).unwrap_err(), error);

    // A validated recovery unblocks saves against what it found.
    store.fail_loads(None);
    assert_eq!(persistence.recover().unwrap(), None);
    assert_eq!(persistence.blocked(), None);
    persistence.save(&personal_view()).unwrap();
    assert_eq!(store.bytes(), Some(encoded(&personal_view())));
}

#[test]
fn failed_publication_without_conflict_keeps_saves_available() {
    for (class, error) in [
        (
            ViewStoreFailureClass::Unavailable,
            ViewError::StorageUnavailable,
        ),
        (ViewStoreFailureClass::Locked, ViewError::StorageUnavailable),
        (ViewStoreFailureClass::Unsafe, ViewError::UnsafeStorage),
        (
            ViewStoreFailureClass::TooLarge,
            limit(ViewResource::DocumentBytes),
        ),
    ] {
        let store = MemoryStore::with_document(encoded(&other_view()));
        let mut persistence = ViewPersistence::new(store.clone());
        persistence.load().unwrap();
        store.script(Script::Fail(ViewStoreFailure {
            class,
            stage: ViewStoreStage::Write,
        }));
        assert_eq!(persistence.save(&personal_view()).unwrap_err(), error);
        assert_eq!(persistence.blocked(), None);
        assert_eq!(store.bytes(), Some(encoded(&other_view())));
        persistence.save(&personal_view()).unwrap();
        assert_eq!(store.bytes(), Some(encoded(&personal_view())));
    }
}

#[test]
fn oversized_document_is_not_read_and_blocks_saves() {
    let oversized = vec![0; MAX_VIEW_DOCUMENT_BYTES + 1];
    let store = MemoryStore::with_document(oversized);
    let mut persistence = ViewPersistence::new(store.clone());
    let error = limit(ViewResource::DocumentBytes);
    assert_eq!(persistence.load().unwrap_err(), error);
    assert_eq!(persistence.blocked(), Some(error));
    assert_eq!(persistence.save(&personal_view()).unwrap_err(), error);
    assert_eq!(store.replaces(), 0);
    assert_eq!(
        store.bytes().map(|bytes| bytes.len()),
        Some(MAX_VIEW_DOCUMENT_BYTES + 1)
    );
}

#[test]
fn crash_before_publication_restores_old_complete_view() {
    let old = other_view();
    let store = MemoryStore::with_document(encoded(&old));
    let mut persistence = ViewPersistence::new(store.clone());
    persistence.load().unwrap();
    store.script(Script::CrashBeforePublish);
    assert_eq!(
        persistence.save(&personal_view()).unwrap_err(),
        ViewError::IndeterminatePublication
    );

    // The next launch reads the old complete document.
    let mut relaunched = ViewPersistence::new(store.clone());
    let loaded = relaunched.load().unwrap().unwrap();
    assert_eq!(loaded, old);
    let (state, _) = ClientViewState::restore(Some(&loaded), &RestorationFilter::new()).unwrap();
    assert_eq!(attached(&state), vec![(A, 5)]);
}

#[test]
fn crash_after_publication_before_reply_recovers_new_complete_view() {
    let store = MemoryStore::with_document(encoded(&other_view()));
    let mut persistence = ViewPersistence::new(store.clone());
    persistence.load().unwrap();
    store.script(Script::CrashAfterPublish);
    assert_eq!(
        persistence.save(&personal_view()).unwrap_err(),
        ViewError::IndeterminatePublication
    );

    // The next launch reads the new complete document.
    let mut relaunched = ViewPersistence::new(store.clone());
    assert_eq!(relaunched.load().unwrap(), Some(personal_view()));

    // The same process recovers it too and saves over the new publication.
    assert_eq!(persistence.recover().unwrap(), Some(personal_view()));
    persistence.save(&other_view()).unwrap();
    assert_eq!(
        store.calls().last(),
        Some(&Call::Replace { expected: Some(2) })
    );
}

#[test]
fn durable_detach_marker_prevents_old_view_file_resurrection() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100).add_workspace(2, 20, 200);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    attach(&mut state, &org, ready(A), 2).unwrap();
    let store = MemoryStore::default();
    let mut persistence = ViewPersistence::new(store.clone());
    persistence.load().unwrap();
    persistence.save(&state.persisted().unwrap()).unwrap();

    // Marker first: client-core durably records the exact attachment, then
    // the view applies the plan, then the view save fails.
    let plan = state.prepare_detach(ws(A, 1)).unwrap();
    let mut markers = RestorationFilter::new();
    markers.suppress_attachment(plan.attachment());
    state.apply_detach(plan).unwrap();
    store.script(Script::Fail(ViewStoreFailure {
        class: ViewStoreFailureClass::Unavailable,
        stage: ViewStoreStage::Sync,
    }));
    assert_eq!(
        persistence.save(&state.persisted().unwrap()).unwrap_err(),
        ViewError::StorageUnavailable
    );

    // The next launch reads the old document; the marker removes the
    // attachment before anything can use it.
    let mut relaunched = ViewPersistence::new(store.clone());
    let old = relaunched.load().unwrap().unwrap();
    assert_eq!(old.parts().workspaces.len(), 2);
    let (restored_state, update) = ClientViewState::restore(Some(&old), &markers).unwrap();
    assert_eq!(attached(&restored_state), vec![(A, 2)]);
    assert_eq!(
        update.changes[0],
        ViewChange::AttachmentRemoved {
            attachment: plan.attachment(),
            reason: AttachmentRemoval::Suppressed,
        }
    );

    // Once the filtered view is durable, the marker is no longer needed.
    relaunched
        .save(&restored_state.persisted().unwrap())
        .unwrap();
    let current = ViewPersistence::new(store).load().unwrap().unwrap();
    let (unfiltered, _) =
        ClientViewState::restore(Some(&current), &RestorationFilter::new()).unwrap();
    assert_eq!(attached(&unfiltered), vec![(A, 2)]);
}

#[test]
fn detached_incarnation_newer_than_the_retained_document_is_never_reissued() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100);
    let store = MemoryStore::default();
    let mut persistence = ViewPersistence::new(store.clone());
    persistence.load().unwrap();
    let mut state = ClientViewState::new();
    persistence.save(&state.persisted().unwrap()).unwrap();

    // The attachment never becomes durable: its save fails.
    attach(&mut state, &org, ready(A), 1).unwrap();
    store.script(Script::Fail(ViewStoreFailure {
        class: ViewStoreFailureClass::Unavailable,
        stage: ViewStoreStage::Sync,
    }));
    assert_eq!(
        persistence.save(&state.persisted().unwrap()).unwrap_err(),
        ViewError::StorageUnavailable
    );

    // Marker first: client-core durably records the detach, the view applies
    // it, and the process dies before any view save.
    let old_plan = state.prepare_detach(ws(A, 1)).unwrap();
    let mut markers = RestorationFilter::new();
    markers.suppress_attachment(old_plan.attachment());
    state.apply_detach(old_plan).unwrap();
    drop(state);

    // The retained document predates the detached attachment.
    let mut relaunched_store = ViewPersistence::new(store.clone());
    let old = relaunched_store.load().unwrap().unwrap();
    assert_eq!(old.parts().incarnation_high_water, 0);
    let (mut relaunched, update) = ClientViewState::restore(Some(&old), &markers).unwrap();
    assert!(update.is_empty());

    // Reattaching issues a fresh incarnation, at the very revision the old
    // plan was prepared at.
    attach(&mut relaunched, &org, ready(A), 1).unwrap();
    let new = relaunched.attachments()[0];
    assert!(new.incarnation() > old_plan.attachment().incarnation());
    assert_eq!(relaunched.revision(), old_plan.revision());

    // Settling the old detach or reapplying its marker removes nothing.
    let before = relaunched.clone();
    assert_eq!(
        relaunched.apply_detach(old_plan).unwrap_err(),
        ViewError::StaleView
    );
    assert!(relaunched.filter_suppressed(&markers).unwrap().is_empty());
    assert_eq!(relaunched, before);

    // Nor does the old marker on the next launch.
    relaunched_store
        .save(&relaunched.persisted().unwrap())
        .unwrap();
    let current = ViewPersistence::new(store).load().unwrap().unwrap();
    let (next, update) = ClientViewState::restore(Some(&current), &markers).unwrap();
    assert!(update.is_empty());
    assert_eq!(next.attachments(), &[new]);

    // Client-core keeps the floor after the marker clears.
    let mut cleared = RestorationFilter::new();
    cleared.raise_incarnation_floor(old_plan.attachment().incarnation());
    cleared.raise_incarnation_floor(AttachmentIncarnation::FIRST);
    let (mut again, update) = ClientViewState::restore(Some(&old), &cleared).unwrap();
    assert!(update.is_empty());
    attach(&mut again, &org, ready(A), 1).unwrap();
    assert!(again.attachments()[0].incarnation() > old_plan.attachment().incarnation());
}

#[test]
fn detached_incarnation_is_never_reissued_when_no_document_was_published() {
    for first_save in [
        Script::Fail(ViewStoreFailure {
            class: ViewStoreFailureClass::Unavailable,
            stage: ViewStoreStage::Write,
        }),
        Script::CrashBeforePublish,
    ] {
        let mut org = Org::new(A);
        org.add_workspace(1, 10, 100);
        let store = MemoryStore::default();
        let mut persistence = ViewPersistence::new(store.clone());
        assert_eq!(persistence.load().unwrap(), None);
        let (mut state, _) = ClientViewState::restore(None, &RestorationFilter::new()).unwrap();

        // The first save fails, so no document is ever published.
        attach(&mut state, &org, ready(A), 1).unwrap();
        store.script(first_save);
        persistence.save(&state.persisted().unwrap()).unwrap_err();
        assert_eq!(store.bytes(), None);

        // Marker first: client-core durably records the detach, the view
        // applies it, and the process dies before any view save.
        let old_plan = state.prepare_detach(ws(A, 1)).unwrap();
        let mut markers = RestorationFilter::new();
        markers.suppress_attachment(old_plan.attachment());
        state.apply_detach(old_plan).unwrap();
        drop(state);

        // The relaunch still finds no document, and the launch view applies
        // the markers' incarnation floor anyway.
        let mut relaunched_store = ViewPersistence::new(store.clone());
        let loaded = relaunched_store.load().unwrap();
        assert_eq!(loaded, None);
        let (mut relaunched, update) = ClientViewState::restore(loaded.as_ref(), &markers).unwrap();
        assert!(update.is_empty());
        assert!(relaunched.attachments().is_empty());

        // Reattaching issues a fresh incarnation.
        attach(&mut relaunched, &org, ready(A), 1).unwrap();
        let new = relaunched.attachments()[0];
        assert_eq!(
            old_plan.attachment().incarnation(),
            AttachmentIncarnation::FIRST
        );
        assert!(new.incarnation() > old_plan.attachment().incarnation());

        // Settling the old detach or reapplying its marker removes nothing.
        let before = relaunched.clone();
        assert_eq!(
            relaunched.apply_detach(old_plan).unwrap_err(),
            ViewError::StaleView
        );
        assert!(relaunched.filter_suppressed(&markers).unwrap().is_empty());
        assert_eq!(relaunched, before);

        // Nor does the old marker on the next launch, once the reattachment
        // is durable.
        relaunched_store
            .save(&relaunched.persisted().unwrap())
            .unwrap();
        let current = ViewPersistence::new(store).load().unwrap().unwrap();
        let (next, update) = ClientViewState::restore(Some(&current), &markers).unwrap();
        assert!(update.is_empty());
        assert_eq!(next.attachments(), &[new]);
    }
}

#[test]
fn old_detach_completion_cannot_remove_new_attachment_incarnation() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    let old_plan = state.prepare_detach(ws(A, 1)).unwrap();
    let mut markers = RestorationFilter::new();
    markers.suppress_attachment(old_plan.attachment());
    state.apply_detach(old_plan).unwrap();

    // The user attaches the Workspace again before the old task settles.
    attach(&mut state, &org, ready(A), 1).unwrap();
    let new = state.attachments()[0];
    assert!(new.incarnation() > old_plan.attachment().incarnation());
    let store = MemoryStore::default();
    let mut persistence = ViewPersistence::new(store.clone());
    persistence.load().unwrap();
    persistence.save(&state.persisted().unwrap()).unwrap();

    // The old marker suppresses only the old incarnation.
    let saved = ViewPersistence::new(store).load().unwrap().unwrap();
    let (mut restored_state, update) = ClientViewState::restore(Some(&saved), &markers).unwrap();
    assert!(update.is_empty());
    assert_eq!(restored_state.attachments(), &[new]);

    // Completing the old detach plan removes nothing.
    assert_eq!(
        restored_state.apply_detach(old_plan).unwrap_err(),
        ViewError::StaleView
    );
    assert_eq!(restored_state.attachments(), &[new]);
}

// Bounds at the limit and one past it.

#[test]
fn attachment_and_server_counts_decode_at_the_limit_and_reject_one_more() {
    let at_limit = RawView::new((1..=256).map(RawAttachment::filler).collect());
    assert_eq!(at_limit.decode().unwrap().parts().workspaces.len(), 256);

    let over = RawView::new((1..=257).map(RawAttachment::filler).collect());
    assert_eq!(over.decode().unwrap_err(), limit(ViewResource::Attachments));

    // The Server count is checked before any entry is read.
    let mut servers = RawView::new(vec![RawAttachment::filler(1)]);
    servers.servers = vec![[A; 16]; 257];
    assert_eq!(
        servers.decode().unwrap_err(),
        limit(ViewResource::Attachments)
    );

    // The checked constructor applies the same bound.
    let parts = |count: u64| PersistedViewParts {
        revision: ViewRevision::new(1),
        incarnation_high_water: count,
        workspaces: (1..=count)
            .map(|local| retained_workspace(B, local, local, vec![(local, single(B, local))]))
            .collect(),
        active: Some(ws(B, 1)),
    };
    assert!(PersistedView::new(parts(256)).is_ok());
    assert_eq!(
        PersistedView::new(parts(257)).unwrap_err(),
        limit(ViewResource::Attachments)
    );
}

#[test]
fn tab_count_decodes_at_the_limit_and_rejects_one_more() {
    let tabs = |range: std::ops::RangeInclusive<u64>| -> Vec<(u64, RawArrangement)> {
        range
            .map(|local| (local, RawArrangement::single(local)))
            .collect()
    };
    let at_limit = RawView::new(vec![
        RawAttachment::with_tabs(1, tabs(1..=4096)),
        RawAttachment::with_tabs(2, tabs(4097..=8192)),
    ]);
    let decoded = at_limit.decode().unwrap();
    let total: usize = decoded
        .parts()
        .workspaces
        .iter()
        .map(|workspace| workspace.tabs.order.len())
        .sum();
    assert_eq!(total, 8192);

    let over = RawView::new(vec![
        RawAttachment::with_tabs(1, tabs(1..=4096)),
        RawAttachment::with_tabs(2, tabs(4097..=8193)),
    ]);
    assert_eq!(over.decode().unwrap_err(), limit(ViewResource::Tabs));

    let mut parts = decoded.into_parts();
    parts.workspaces[1].tabs.order.push(tab(A, 8193));
    parts.workspaces[1].arrangements.push(single(A, 8193));
    assert_eq!(
        PersistedView::new(parts).unwrap_err(),
        limit(ViewResource::Tabs)
    );
}

#[test]
fn pane_counts_decode_at_the_limit_and_reject_one_more() {
    // Sixteen full Tabs hold exactly 65,536 Panes.
    let full: Vec<(u64, RawArrangement)> = (1..=16)
        .map(|local| {
            (
                local,
                RawArrangement::from_record(&chain(A, local * 10_000, 4096)),
            )
        })
        .collect();
    let at_limit = RawView::new(vec![RawAttachment::with_tabs(1, full.clone())]);
    let decoded = at_limit.decode().unwrap();
    assert_eq!(decoded.parts().workspaces[0].arrangements.len(), 16);

    let mut over = full;
    over.push((17, RawArrangement::single(17)));
    let over_view = RawView::new(vec![RawAttachment::with_tabs(1, over)]);
    assert_eq!(over_view.decode().unwrap_err(), limit(ViewResource::Panes));

    let mut parts = decoded.into_parts();
    parts.workspaces[0].tabs.order.push(tab(A, 17));
    parts.workspaces[0].arrangements.push(single(A, 17));
    assert_eq!(
        PersistedView::new(parts).unwrap_err(),
        limit(ViewResource::Panes)
    );
}

#[test]
fn tab_pane_and_focus_counts_decode_at_the_limit_and_reject_one_more() {
    let mut full = chain(A, 1, 4096);
    full.focus_history = (1..=4096).map(|local| pane(A, local)).collect();
    full.focused = pane(A, 4096);
    let at_limit = RawView::new(vec![RawAttachment::with_tabs(
        1,
        vec![(10, RawArrangement::from_record(&full))],
    )]);
    assert!(at_limit.decode().is_ok());

    let over = chain(A, 1, 4097);
    let over_view = RawView::new(vec![RawAttachment::with_tabs(
        1,
        vec![(10, RawArrangement::from_record(&over))],
    )]);
    assert_eq!(
        over_view.decode().unwrap_err(),
        limit(ViewResource::TabPanes)
    );
    let tabs = vec![(10, over)];
    let parts = PersistedViewParts {
        revision: ViewRevision::new(1),
        incarnation_high_water: 1,
        workspaces: vec![retained_workspace(A, 1, 1, tabs)],
        active: Some(ws(A, 1)),
    };
    assert_eq!(
        PersistedView::new(parts).unwrap_err(),
        limit(ViewResource::TabPanes)
    );

    let mut focus = RawView::new(vec![RawAttachment::with_tabs(
        1,
        vec![(10, RawArrangement::from_record(&full))],
    )]);
    arrangement(&mut focus).focus.push(1);
    assert_eq!(focus.decode().unwrap_err(), limit(ViewResource::TabPanes));
}

#[test]
fn document_bytes_decode_at_the_limit_and_reject_one_more() {
    // At the limit the length is accepted and the content is judged.
    let mut at_limit = base().bytes();
    at_limit.resize(MAX_VIEW_DOCUMENT_BYTES, 0);
    assert_eq!(PersistedView::decode(&at_limit).unwrap_err(), INVALID);

    let mut over = base().bytes();
    over.resize(MAX_VIEW_DOCUMENT_BYTES + 1, 0);
    assert_eq!(
        PersistedView::decode(&over).unwrap_err(),
        limit(ViewResource::DocumentBytes)
    );

    // The largest view this build can represent stays far below the limit.
    let full: Vec<(u64, RawArrangement)> = (1..=16)
        .map(|local| {
            let mut record = chain(A, local * 10_000, 4096);
            record.focus_history = record
                .nodes
                .iter()
                .filter_map(|node| match node {
                    ArrangementNode::Pane(pane) => Some(*pane),
                    ArrangementNode::Split { .. } => None,
                })
                .collect();
            record.focused = *record.focus_history.last().unwrap();
            (local, RawArrangement::from_record(&record))
        })
        .collect();
    let largest = RawView::new(vec![RawAttachment::with_tabs(1, full)]);
    let view = largest.decode().unwrap();
    assert!(view.encode().unwrap().as_bytes().len() < MAX_VIEW_DOCUMENT_BYTES);
}

// Malformed documents.

#[test]
fn malformed_documents_are_rejected_without_partial_state() {
    assert!(base().decode().is_ok());
    type Edit = fn(&mut RawView);
    let cases: Vec<(&str, Edit, ViewError)> = vec![
        (
            "zero workspace",
            |raw| attachment(raw).workspace = 0,
            INVALID,
        ),
        ("zero pairing", |raw| attachment(raw).pairing = 0, INVALID),
        ("zero client", |raw| attachment(raw).client = 0, INVALID),
        (
            "zero incarnation",
            |raw| attachment(raw).incarnation = 0,
            INVALID,
        ),
        (
            "incarnation above high water",
            |raw| attachment(raw).incarnation = 2,
            INVALID,
        ),
        ("zero tab", |raw| attachment(raw).tabs[0].0 = 0, INVALID),
        (
            "zero pane",
            |raw| arrangement(raw).nodes[1] = RawNode::Pane(0),
            INVALID,
        ),
        (
            "zero split",
            |raw| arrangement(raw).nodes[0] = split(0, 0.5, 1, 2),
            INVALID,
        ),
        (
            "zero focused pane",
            |raw| arrangement(raw).focused = 0,
            INVALID,
        ),
        (
            "duplicate leaves",
            |raw| arrangement(raw).nodes[2] = RawNode::Pane(100),
            INVALID,
        ),
        ("cyclic node", |raw| set_children(raw, 1, 0), INVALID),
        (
            "multiply referenced node",
            |raw| set_children(raw, 1, 1),
            INVALID,
        ),
        (
            "unreachable node",
            |raw| arrangement(raw).nodes.push(RawNode::Pane(102)),
            INVALID,
        ),
        (
            "root out of range",
            |raw| arrangement(raw).root = 3,
            INVALID,
        ),
        ("child out of range", |raw| set_children(raw, 1, 7), INVALID),
        (
            "unknown availability",
            |raw| attachment(raw).availability = 4,
            INVALID,
        ),
        (
            "unknown node tag",
            |raw| arrangement(raw).nodes[1] = RawNode::Tag(2),
            INVALID,
        ),
        (
            "unknown axis",
            |raw| {
                if let RawNode::Split { axis, .. } = root_split(raw) {
                    *axis = 2;
                }
            },
            INVALID,
        ),
        ("NaN ratio", |raw| set_ratio(raw, f32::NAN), INVALID),
        (
            "infinite ratio",
            |raw| set_ratio(raw, f32::INFINITY),
            INVALID,
        ),
        (
            "negative infinite ratio",
            |raw| set_ratio(raw, f32::NEG_INFINITY),
            INVALID,
        ),
        ("zero ratio", |raw| set_ratio(raw, 0.0), INVALID),
        ("one ratio", |raw| set_ratio(raw, 1.0), INVALID),
        ("negative ratio", |raw| set_ratio(raw, -0.5), INVALID),
        ("ratio above one", |raw| set_ratio(raw, 1.5), INVALID),
        (
            "split above high water",
            |raw| arrangement(raw).high_water = 0,
            INVALID,
        ),
        (
            "duplicate split identity",
            |raw| {
                let nested = arrangement(raw);
                nested.nodes = vec![
                    split(1, 0.5, 1, 2),
                    RawNode::Pane(100),
                    split(1, 0.5, 3, 4),
                    RawNode::Pane(101),
                    RawNode::Pane(102),
                ];
            },
            INVALID,
        ),
        (
            "focused pane outside arrangement",
            |raw| arrangement(raw).focused = 999,
            INVALID,
        ),
        (
            "focus history outside arrangement",
            |raw| arrangement(raw).focus = vec![100, 999],
            INVALID,
        ),
        (
            "duplicate focus history",
            |raw| arrangement(raw).focus = vec![100, 100],
            INVALID,
        ),
        (
            "zoom on an unfocused pane",
            |raw| arrangement(raw).zoomed = 101,
            INVALID,
        ),
        (
            "zoom in a single-pane arrangement",
            |raw| attachment(raw).tabs[1].1.zoomed = 110,
            INVALID,
        ),
        (
            "active tab outside order",
            |raw| attachment(raw).active_tab = 99,
            INVALID,
        ),
        (
            "active tab not most recent",
            |raw| attachment(raw).history = vec![10, 11],
            INVALID,
        ),
        (
            "no active tab with tabs",
            |raw| {
                attachment(raw).active_tab = 0;
                attachment(raw).history.clear();
            },
            INVALID,
        ),
        (
            "history outside order",
            |raw| attachment(raw).history = vec![12, 10],
            INVALID,
        ),
        (
            "duplicate history",
            |raw| attachment(raw).history = vec![10, 10],
            INVALID,
        ),
        (
            "history longer than tabs",
            |raw| attachment(raw).history = vec![11, 12, 10],
            INVALID,
        ),
        (
            "active attachment out of range",
            |raw| raw.active = 1,
            INVALID,
        ),
        (
            "no active attachment with attachments",
            |raw| raw.active = u32::MAX,
            INVALID,
        ),
        (
            "duplicate server entry",
            |raw| raw.servers.push([A; 16]),
            INVALID,
        ),
        (
            "unreferenced server",
            |raw| raw.servers.push([B; 16]),
            INVALID,
        ),
        (
            "server index out of range",
            |raw| attachment(raw).server = 1,
            INVALID,
        ),
        (
            "duplicate workspace",
            |raw| {
                let mut copy = RawAttachment::filler(2);
                copy.workspace = 1;
                raw.attachments.push(copy);
                raw.high_water = 2;
            },
            INVALID,
        ),
        (
            "duplicate incarnation",
            |raw| {
                let mut copy = RawAttachment::filler(2);
                copy.incarnation = 1;
                raw.attachments.push(copy);
            },
            INVALID,
        ),
        (
            "duplicate tab across attachments",
            |raw| {
                let mut copy = RawAttachment::filler(2);
                copy.tabs[0].0 = 10;
                copy.active_tab = 10;
                copy.history = vec![10];
                raw.attachments.push(copy);
                raw.high_water = 2;
            },
            INVALID,
        ),
        (
            "duplicate pane across tabs",
            |raw| attachment(raw).tabs[1].1 = RawArrangement::single(100),
            INVALID,
        ),
        ("version zero", |raw| raw.version = 0, INVALID),
        (
            "newer version",
            |raw| raw.version = 2,
            ViewError::UnsupportedFormat { version: 2 },
        ),
        (
            "newest version",
            |raw| raw.version = u32::MAX,
            ViewError::UnsupportedFormat { version: u32::MAX },
        ),
        (
            "length overflow",
            |raw| raw.length = Some(u64::MAX),
            INVALID,
        ),
        (
            "length one short",
            |raw| raw.length = Some(u64::try_from(raw.bytes().len() - 21).unwrap()),
            INVALID,
        ),
        (
            "length one long",
            |raw| raw.length = Some(u64::try_from(raw.bytes().len() - 19).unwrap()),
            INVALID,
        ),
        ("trailing data", |raw| raw.trailing = vec![0], INVALID),
    ];
    for (name, edit, error) in cases {
        let mut raw = base();
        edit(&mut raw);
        assert_eq!(raw.decode().unwrap_err(), error, "{name}");
    }

    // Truncation anywhere, including inside the header, is malformed.
    let bytes = base().bytes();
    for end in 0..bytes.len() {
        assert_eq!(
            PersistedView::decode(&bytes[..end]).unwrap_err(),
            INVALID,
            "truncated at {end}"
        );
    }
    let mut magic = bytes.clone();
    magic[0] = b's';
    assert_eq!(PersistedView::decode(&magic).unwrap_err(), INVALID);
    let mut truncated_payload = base();
    let full = truncated_payload.bytes();
    truncated_payload.length = Some(u64::try_from(full.len() - 20 - 1).unwrap());
    let mut short = truncated_payload.bytes();
    short.pop();
    assert_eq!(PersistedView::decode(&short).unwrap_err(), INVALID);
}

#[test]
fn persisted_view_rejects_inconsistent_parts_and_wrong_server_references() {
    let valid = personal_view().into_parts();
    assert!(PersistedView::new(valid.clone()).is_ok());
    assert_eq!(
        WorkspaceAttachment::new(
            ws(A, 1),
            pairing(B, 1),
            client(A, 1),
            AttachmentIncarnation::FIRST
        )
        .unwrap_err(),
        ViewError::WrongServer
    );
    assert_eq!(
        WorkspaceAttachment::new(
            ws(A, 1),
            pairing(A, 1),
            client(B, 1),
            AttachmentIncarnation::FIRST
        )
        .unwrap_err(),
        ViewError::WrongServer
    );

    type Edit = fn(&mut PersistedViewParts);
    let cases: Vec<(&str, Edit)> = vec![
        ("no active workspace", |parts| parts.active = None),
        ("active workspace not attached", |parts| {
            parts.active = Some(ws(A, 9));
        }),
        ("active workspace on empty view", |parts| {
            parts.workspaces.clear();
        }),
        ("duplicate workspace", |parts| {
            let mut copy = parts.workspaces[0].clone();
            copy.attachment = WorkspaceAttachment::new(
                copy.attachment.workspace(),
                copy.attachment.pairing(),
                copy.attachment.client(),
                AttachmentIncarnation::new(9).unwrap(),
            )
            .unwrap();
            parts.incarnation_high_water = 9;
            parts.workspaces.push(copy);
        }),
        ("duplicate incarnation", |parts| {
            parts
                .workspaces
                .push(retained_workspace(A, 7, 1, vec![(70, single(A, 700))]));
        }),
        ("incarnation above high water", |parts| {
            parts.incarnation_high_water = 1;
        }),
        ("arrangement count", |parts| {
            parts.workspaces[0].arrangements.pop();
        }),
        ("tab of another server", |parts| {
            parts.workspaces[0].tabs.order[0] = tab(B, 11);
        }),
        ("pane of another server", |parts| {
            parts.workspaces[0].arrangements[0] = single(B, 110);
        }),
        ("focused pane of another server", |parts| {
            parts.workspaces[0].arrangements[0].focused = pane(B, 110);
        }),
        ("duplicate tab across attachments", |parts| {
            let tab_id = parts.workspaces[0].tabs.order[0];
            let record = parts.workspaces[0].arrangements[0].clone();
            parts.workspaces.push(PersistedWorkspace {
                tabs: crate::view::TabOrderRecord {
                    order: vec![tab_id],
                    active: Some(tab_id),
                    activation_history: vec![tab_id],
                },
                arrangements: vec![single(A, 800)],
                ..retained_workspace(A, 8, 3, vec![(80, record)])
            });
            parts.incarnation_high_water = 3;
        }),
        ("duplicate pane across tabs", |parts| {
            parts.workspaces[0].arrangements[0] = single(A, 100);
        }),
        ("limited by attachments", |parts| {
            parts.workspaces[0].availability = ViewAvailability::ResourceLimited {
                resource: ViewResource::Attachments,
            };
        }),
        ("limited by document bytes", |parts| {
            parts.workspaces[0].availability = ViewAvailability::ResourceLimited {
                resource: ViewResource::DocumentBytes,
            };
        }),
    ];
    for (name, edit) in cases {
        let mut parts = valid.clone();
        edit(&mut parts);
        assert_eq!(PersistedView::new(parts).unwrap_err(), INVALID, "{name}");
    }
}
