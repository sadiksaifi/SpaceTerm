//! Data Generations and the one runner that carries a data root to this build's generation.
//!
//! Each data root keeps `generation.json` beside its files. A root's [`Registry`] names its
//! [`Generations`], the ordered steps between them and a strict reader for each kind of file it
//! holds. [`run`] executes before anything reads the root, under the lock its owner holds:
//!
//! 1. A root at the current generation is left alone.
//! 2. A root between `oldest` and `current` is copied to `migration-backup/` with its generation,
//!    migrated in memory, parsed with every strict reader, written file by file, and marked with
//!    the new generation last. A crash before that last write restarts from the backup.
//! 3. A root newer than `current` is restored from the backup when an activation returned to
//!    this build and the backup holds this build's generation. Otherwise it is refused and
//!    never overwritten.
//! 4. A root older than `oldest`, a step that refuses, or a file that no reader accepts resets.
//!    Rebuildable data moves to `reset/` at once and leaves a one-time [`take_notice`]. Every
//!    other kind of data waits for the person to confirm [`reset_confirmed`].
//!
//! A root that holds files but no `generation.json` predates its marker and is at generation 1.
//! An empty root starts at the current generation.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub mod client_files;
pub mod core_data;
mod directory;
pub mod generations;
#[cfg(test)]
mod tests;

pub use directory::{DirectoryStore, Mount};
pub use generations::Generations;

const MARKER: &str = "generation.json";
const NOTICE: &str = "notice.json";
const RETIRE: &str = "retire.json";
const PENDING: &str = "pending.json";

/// A step refuses data it cannot carry forward.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unmigratable;

/// Bounded storage classifications. Paths and native errors never cross this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StoreError {
    #[error("Data storage is unavailable.")]
    Unavailable,
    #[error("Data storage has unsafe permissions.")]
    Unsafe,
}

/// The places a root's [`Store`] keeps files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Area {
    /// The root's own files and its `generation.json`.
    Root,
    /// The copy a migration takes first, `migration-backup/`.
    Backup,
    /// Files a reset moved aside, `reset/`. They are never deleted.
    Reset,
}

/// File effects for one root, addressed by `/`-separated paths relative to an [`Area`].
pub trait Store {
    /// Every regular file in `area`.
    fn list(&self, area: Area) -> Result<Vec<String>, StoreError>;
    fn read(&self, area: Area, path: &str) -> Result<Option<Vec<u8>>, StoreError>;
    /// Replaces `path` whole and durably, creating its directories.
    fn write(&self, area: Area, path: &str, bytes: &[u8]) -> Result<(), StoreError>;
    /// Removes `path`. An absent file is removed.
    fn remove(&self, area: Area, path: &str) -> Result<(), StoreError>;
}

/// The kinds of data a reset names, without paths or contents.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataKind {
    /// Client View State, attachments and retained copies of a Machine's Workspaces.
    View,
    WorkspaceOrder,
    UpdateHistory,
    Settings,
    WorkspaceOrganization,
    MachineId,
    Pairings,
    /// Saved Connections and their credential references.
    SavedConnections,
    /// A client's identity and its unsettled operations and runs.
    ClientRecords,
    /// SpaceTerm Core's service, update and operation records.
    CoreRecords,
}

impl DataKind {
    /// Rebuildable client state resets without asking. Every other kind waits for confirmation.
    pub fn automatic(self) -> bool {
        matches!(
            self,
            Self::View | Self::WorkspaceOrder | Self::UpdateHistory
        )
    }

    /// The kind's name in a sentence, such as a reset's notice or confirmation.
    pub fn noun(self) -> &'static str {
        match self {
            Self::View => "Workspace views",
            Self::WorkspaceOrder => "Workspace order",
            Self::UpdateHistory => "update history",
            Self::Settings => "Settings",
            Self::WorkspaceOrganization => "Workspace organization",
            Self::MachineId => "Machine ID",
            Self::Pairings => "pairings",
            Self::SavedConnections => "saved machines",
            Self::ClientRecords => "unsettled operations",
            Self::CoreRecords => "SpaceTerm Core records",
        }
    }
}

/// `kinds` as one phrase: "a", "a and b", or "a, b and c".
pub fn nouns(kinds: &[DataKind]) -> String {
    let names: Vec<&str> = kinds.iter().map(|kind| kind.noun()).collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// One kind of file a root holds, matched by its file name in any directory of the root.
pub struct FileRule {
    pub name: &'static str,
    pub kind: DataKind,
    /// The strict reader the file's owner uses, applied to its JSON.
    pub read: fn(&Value) -> bool,
    /// A top-level field that repeats the root's generation, as `schema_version` does in the
    /// Settings Document. Migration sets it.
    pub stamp: Option<&'static str>,
}

/// One change in a step from one generation to the next. Released steps never change.
pub enum Step {
    /// Changes every file of one name.
    File {
        name: &'static str,
        apply: fn(&mut Value) -> Result<(), Unmigratable>,
    },
    /// Splits, merges, adds or removes files, and replaces credential items.
    Root(fn(&mut Migration<'_>) -> Result<(), Unmigratable>),
}

/// A root's generations, steps and file kinds.
pub struct Registry {
    pub generations: Generations,
    /// `steps[n]` carries generation `oldest + n` to the next. An empty step changes no data
    /// and still raises the generation.
    pub steps: &'static [&'static [Step]],
    /// The kinds of file the root holds, in groups that roots share.
    pub files: &'static [&'static [FileRule]],
}

impl Registry {
    /// Whether the steps span exactly the generations the registry declares.
    pub fn consistent(&self) -> bool {
        let Generations { oldest, current } = self.generations;
        oldest >= 1
            && oldest <= current
            && self.steps.len() == (current - oldest) as usize
            && self
                .rules()
                .all(|rule| !rule.name.is_empty() && !rule.name.contains('/'))
    }

    fn rules(&self) -> impl Iterator<Item = &FileRule> {
        self.files.iter().flat_map(|group| group.iter())
    }

    fn rule(&self, path: &str) -> Option<&FileRule> {
        let name = path.rsplit('/').next()?;
        self.rules().find(|rule| rule.name == name)
    }

    /// Parses `bytes` as `path`'s file with its strict reader.
    pub fn readable(&self, path: &str, bytes: &[u8]) -> bool {
        let Some(rule) = self.rule(path) else {
            return false;
        };
        serde_json::from_slice::<Value>(bytes).is_ok_and(|value| (rule.read)(&value))
    }
}

/// A credential item one store keeps, named by its store kind and key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialItem {
    pub store: String,
    pub key: [u8; 32],
}

/// A credential failure, without the item or its contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CredentialFailure;

/// The credential stores a root's steps may replace items in.
pub trait Credentials {
    /// Saves a copy of `from`'s secret as `to`. An existing `to` with the same secret is saved.
    fn copy(&mut self, from: &CredentialItem, to: &CredentialItem)
    -> Result<(), CredentialFailure>;
    fn remove(&mut self, item: &CredentialItem) -> Result<(), CredentialFailure>;
}

/// For roots that hold no credential references. Every replacement refuses.
pub struct NoCredentials;

impl Credentials for NoCredentials {
    fn copy(&mut self, _: &CredentialItem, _: &CredentialItem) -> Result<(), CredentialFailure> {
        Err(CredentialFailure)
    }
    fn remove(&mut self, _: &CredentialItem) -> Result<(), CredentialFailure> {
        Err(CredentialFailure)
    }
}

/// A migration in memory: every file of the root by path, and the credential replacements its
/// steps made.
pub struct Migration<'a> {
    pub files: BTreeMap<String, Value>,
    credentials: &'a mut dyn Credentials,
    created: Vec<CredentialItem>,
    retired: Vec<CredentialItem>,
}

impl Migration<'_> {
    /// Creates `to` from `from` now and deletes `from` only after the new generation commits.
    pub fn replace_credential(
        &mut self,
        from: CredentialItem,
        to: CredentialItem,
    ) -> Result<(), Unmigratable> {
        self.credentials
            .copy(&from, &to)
            .map_err(|CredentialFailure| Unmigratable)?;
        self.created.push(to);
        self.retired.push(from);
        Ok(())
    }

    /// Removes the replacements of a migration that will not commit.
    fn abandon(&mut self) {
        for item in std::mem::take(&mut self.created) {
            let _ = self.credentials.remove(&item);
        }
    }
}

/// How a root reached this build's generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Current,
    Migrated {
        from: u32,
    },
    /// An activation returned to this build, which restored the backup taken before the newer
    /// build migrated the root.
    Restored,
    /// Every file needed a reset and each was rebuildable. [`take_notice`] names them once.
    Reset,
}

/// Why a root needs a reset.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetReason {
    /// The root is older than any generation this build migrates, usually because an install
    /// bypassed the updater. The person installs a Stepping Stone release first.
    TooOld,
    Unmigratable,
    /// A migrated or retained file is not one its reader accepts.
    Invalid,
}

/// Why a root stays unread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// A newer SpaceTerm wrote the root. Nothing in it changed.
    Newer,
    /// The data of `kinds` resets only after the person confirms. Rebuildable data in the root
    /// has already reset.
    Reset {
        reason: ResetReason,
        kinds: Vec<DataKind>,
    },
    Storage(StoreError),
}

impl From<StoreError> for Refusal {
    fn from(error: StoreError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    generation: u32,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Notice {
    kinds: BTreeSet<DataKind>,
}

/// The reset a root waits for, as the last [`run`] that refused it recorded.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingReset {
    pub reason: ResetReason,
    pub kinds: Vec<DataKind>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retire {
    items: Vec<CredentialItem>,
}

/// Brings the root in `store` to the registry's current generation. `returned` says that an
/// activation returned to this build. The caller holds the root's lock.
pub fn run(
    registry: &Registry,
    store: &dyn Store,
    credentials: &mut dyn Credentials,
    returned: bool,
) -> Result<Outcome, Refusal> {
    let result = bring(registry, store, credentials, returned);
    // Clients of any version read the recorded reset to explain a refusal.
    match &result {
        Err(Refusal::Reset { reason, kinds }) => {
            let pending = PendingReset {
                reason: *reason,
                kinds: kinds.clone(),
            };
            let bytes = serde_json::to_vec(&pending).map_err(|_| StoreError::Unavailable)?;
            store.write(Area::Reset, PENDING, &bytes)?;
        }
        Ok(_) => store.remove(Area::Reset, PENDING)?,
        Err(_) => {}
    }
    result
}

/// The reset the root in `store` waits for, as the last [`run`] that refused it recorded.
pub fn pending(store: &dyn Store) -> Result<Option<PendingReset>, StoreError> {
    Ok(store
        .read(Area::Reset, PENDING)?
        .and_then(|bytes| serde_json::from_slice(&bytes).ok()))
}

fn bring(
    registry: &Registry,
    store: &dyn Store,
    credentials: &mut dyn Credentials,
    returned: bool,
) -> Result<Outcome, Refusal> {
    let Generations { oldest, current } = registry.generations;
    let marked = marker(store, Area::Root)?;
    let generation = match marked {
        Some(generation) => generation,
        None if members(registry, store, Area::Root)?.is_empty() => {
            write_marker(store, Area::Root, current)?;
            return Ok(Outcome::Current);
        }
        None => 1,
    };
    if generation == current {
        if marked.is_none() {
            write_marker(store, Area::Root, current)?;
        }
        retire(store, credentials)?;
        return Ok(Outcome::Current);
    }
    if generation > current {
        if returned && marker(store, Area::Backup)? == Some(current) {
            restore_backup(registry, store)?;
            write_marker(store, Area::Root, current)?;
            return Ok(Outcome::Restored);
        }
        return Err(Refusal::Newer);
    }
    if generation < oldest {
        return reset_rebuildable(registry, store, generation, ResetReason::TooOld);
    }
    migrate(registry, store, credentials, generation)
}

/// Resets every file of a root that [`run`] refuses with [`Refusal::Reset`], after the person
/// confirmed it. Files move to `reset/`. A root that no longer needs a reset is left to [`run`]'s
/// outcome, so a stale confirmation never resets readable data.
pub fn reset_confirmed(
    registry: &Registry,
    store: &dyn Store,
    credentials: &mut dyn Credentials,
) -> Result<Vec<DataKind>, Refusal> {
    match run(registry, store, credentials, false) {
        Err(Refusal::Reset { .. }) => {}
        Ok(_) => return Ok(Vec::new()),
        Err(refusal) => return Err(refusal),
    }
    let generation = marker(store, Area::Root)?.unwrap_or(1);
    let mut kinds = BTreeSet::new();
    for path in members(registry, store, Area::Root)? {
        if let Some(rule) = registry.rule(&path) {
            kinds.insert(rule.kind);
        }
        set_aside(store, generation, &path)?;
    }
    write_marker(store, Area::Root, registry.generations.current)?;
    store.remove(Area::Reset, PENDING)?;
    Ok(kinds.into_iter().collect())
}

/// The rebuildable data a reset replaced since the last call, once.
pub fn take_notice(store: &dyn Store) -> Result<Vec<DataKind>, StoreError> {
    let Some(bytes) = store.read(Area::Reset, NOTICE)? else {
        return Ok(Vec::new());
    };
    store.remove(Area::Reset, NOTICE)?;
    Ok(serde_json::from_slice::<Notice>(&bytes)
        .map(|notice| notice.kinds.into_iter().collect())
        .unwrap_or_default())
}

/// Migrates `value`, one document of `name` that carries its generation in its rule's stamp,
/// such as an exported Settings Document, and returns it at the current generation.
pub fn migrate_document(
    registry: &Registry,
    name: &str,
    mut value: Value,
) -> Result<Value, Refusal> {
    let Generations { oldest, current } = registry.generations;
    let rule = registry.rule(name).ok_or(Refusal::Reset {
        reason: ResetReason::Invalid,
        kinds: Vec::new(),
    })?;
    let refuse = |reason| Refusal::Reset {
        reason,
        kinds: vec![rule.kind],
    };
    let stamp = rule.stamp.ok_or(refuse(ResetReason::Invalid))?;
    let generation = value
        .get(stamp)
        .and_then(Value::as_u64)
        .and_then(|generation| u32::try_from(generation).ok())
        .ok_or(refuse(ResetReason::Invalid))?;
    if generation > current {
        return Err(Refusal::Newer);
    }
    if generation < oldest {
        return Err(refuse(ResetReason::TooOld));
    }
    for steps in &registry.steps[(generation - oldest) as usize..] {
        for step in *steps {
            match step {
                Step::File {
                    name: target,
                    apply,
                } if *target == name => {
                    apply(&mut value).map_err(|Unmigratable| refuse(ResetReason::Unmigratable))?;
                }
                Step::File { .. } => {}
                // A root step may move this document's data to another file, which a lone
                // document doesn't have.
                Step::Root(_) => return Err(refuse(ResetReason::Unmigratable)),
            }
        }
    }
    if let Some(object) = value.as_object_mut() {
        object.insert(stamp.into(), current.into());
    }
    if !(rule.read)(&value) {
        return Err(refuse(ResetReason::Invalid));
    }
    Ok(value)
}

fn migrate(
    registry: &Registry,
    store: &dyn Store,
    credentials: &mut dyn Credentials,
    from: u32,
) -> Result<Outcome, Refusal> {
    let Generations { oldest, current } = registry.generations;
    if marker(store, Area::Backup)? == Some(from) {
        // An earlier attempt stopped after its backup and before its marker. Its replaced
        // credential items are still in use.
        store.remove(Area::Backup, RETIRE)?;
        restore_backup(registry, store)?;
    } else {
        take_backup(registry, store, from)?;
    }
    let before = members(registry, store, Area::Root)?;
    let mut files = BTreeMap::new();
    for path in &before {
        let bytes = store.read(Area::Root, path)?.unwrap_or_default();
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            return reset_rebuildable(registry, store, from, ResetReason::Invalid);
        };
        files.insert(path.clone(), value);
    }
    let mut migration = Migration {
        files,
        credentials,
        created: Vec::new(),
        retired: Vec::new(),
    };
    let applied = (|| {
        for steps in &registry.steps[(from - oldest) as usize..] {
            for step in *steps {
                match step {
                    Step::File { name, apply } => {
                        for (path, value) in &mut migration.files {
                            if path.rsplit('/').next() == Some(*name) {
                                apply(value).map_err(|Unmigratable| ResetReason::Unmigratable)?;
                            }
                        }
                    }
                    Step::Root(apply) => {
                        apply(&mut migration).map_err(|Unmigratable| ResetReason::Unmigratable)?;
                    }
                }
            }
        }
        for (path, value) in &mut migration.files {
            let rule = registry.rule(path).ok_or(ResetReason::Invalid)?;
            if let (Some(stamp), Some(object)) = (rule.stamp, value.as_object_mut()) {
                object.insert(stamp.into(), current.into());
            }
            if !(rule.read)(value) {
                return Err(ResetReason::Invalid);
            }
        }
        Ok(())
    })();
    if let Err(reason) = applied {
        migration.abandon();
        return reset_rebuildable(registry, store, from, reason);
    }
    let written = (|| {
        for (path, value) in &migration.files {
            let bytes = serde_json::to_vec(value).map_err(|_| StoreError::Unavailable)?;
            store.write(Area::Root, path, &bytes)?;
        }
        for path in before
            .iter()
            .filter(|path| !migration.files.contains_key(*path))
        {
            store.remove(Area::Root, path)?;
        }
        if !migration.retired.is_empty() {
            let items = Retire {
                items: migration.retired.clone(),
            };
            let bytes = serde_json::to_vec(&items).map_err(|_| StoreError::Unavailable)?;
            store.write(Area::Backup, RETIRE, &bytes)?;
        }
        write_marker(store, Area::Root, current)
    })();
    if let Err(error) = written {
        // The marker never moved, so the next run restores the backup and tries again.
        let _ = store.remove(Area::Backup, RETIRE);
        migration.abandon();
        return Err(error.into());
    }
    let Migration { credentials, .. } = migration;
    retire(store, credentials)?;
    Ok(Outcome::Migrated { from })
}

/// Deletes the credential items a committed migration replaced.
fn retire(store: &dyn Store, credentials: &mut dyn Credentials) -> Result<(), StoreError> {
    let Some(bytes) = store.read(Area::Backup, RETIRE)? else {
        return Ok(());
    };
    if let Ok(retire) = serde_json::from_slice::<Retire>(&bytes) {
        for item in &retire.items {
            // An item that stays is unused. The next run tries again.
            if credentials.remove(item).is_err() {
                return Ok(());
            }
        }
    }
    store.remove(Area::Backup, RETIRE)
}

/// Moves the rebuildable files aside and refuses until the person confirms the rest.
fn reset_rebuildable(
    registry: &Registry,
    store: &dyn Store,
    generation: u32,
    reason: ResetReason,
) -> Result<Outcome, Refusal> {
    let mut reset = BTreeSet::new();
    let mut waiting = BTreeSet::new();
    for path in members(registry, store, Area::Root)? {
        let Some(rule) = registry.rule(&path) else {
            continue;
        };
        if rule.kind.automatic() {
            set_aside(store, generation, &path)?;
            reset.insert(rule.kind);
        } else {
            waiting.insert(rule.kind);
        }
    }
    if !reset.is_empty() {
        let mut notice = store
            .read(Area::Reset, NOTICE)?
            .and_then(|bytes| serde_json::from_slice::<Notice>(&bytes).ok())
            .unwrap_or_default();
        notice.kinds.extend(reset);
        let bytes = serde_json::to_vec(&notice).map_err(|_| StoreError::Unavailable)?;
        store.write(Area::Reset, NOTICE, &bytes)?;
    }
    if waiting.is_empty() {
        write_marker(store, Area::Root, registry.generations.current)?;
        return Ok(Outcome::Reset);
    }
    Err(Refusal::Reset {
        reason,
        kinds: waiting.into_iter().collect(),
    })
}

/// Moves `path` to `reset/gen-<generation>/`.
fn set_aside(store: &dyn Store, generation: u32, path: &str) -> Result<(), StoreError> {
    if let Some(bytes) = store.read(Area::Root, path)? {
        store.write(Area::Reset, &format!("gen-{generation}/{path}"), &bytes)?;
    }
    store.remove(Area::Root, path)
}

fn take_backup(registry: &Registry, store: &dyn Store, generation: u32) -> Result<(), StoreError> {
    for path in store.list(Area::Backup)? {
        store.remove(Area::Backup, &path)?;
    }
    for path in members(registry, store, Area::Root)? {
        if let Some(bytes) = store.read(Area::Root, &path)? {
            store.write(Area::Backup, &path, &bytes)?;
        }
    }
    write_marker(store, Area::Backup, generation)
}

/// Makes the root's files those of the backup again. The caller then writes the root's marker.
fn restore_backup(registry: &Registry, store: &dyn Store) -> Result<(), StoreError> {
    let saved = members(registry, store, Area::Backup)?;
    for path in &saved {
        if let Some(bytes) = store.read(Area::Backup, path)? {
            store.write(Area::Root, path, &bytes)?;
        }
    }
    for path in members(registry, store, Area::Root)? {
        if !saved.contains(&path) {
            store.remove(Area::Root, &path)?;
        }
    }
    Ok(())
}

/// The files in `area` that the registry names, without markers or notices.
fn members(registry: &Registry, store: &dyn Store, area: Area) -> Result<Vec<String>, StoreError> {
    let mut paths: Vec<_> = store
        .list(area)?
        .into_iter()
        .filter(|path| registry.rule(path).is_some())
        .collect();
    paths.sort();
    Ok(paths)
}

fn marker(store: &dyn Store, area: Area) -> Result<Option<u32>, StoreError> {
    let Some(bytes) = store.read(area, MARKER)? else {
        return Ok(None);
    };
    // An unreadable marker names no generation this build knows.
    Ok(Some(
        serde_json::from_slice::<Marker>(&bytes)
            .map(|marker| marker.generation)
            .unwrap_or(u32::MAX),
    ))
}

fn write_marker(store: &dyn Store, area: Area, generation: u32) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec(&Marker { generation }).map_err(|_| StoreError::Unavailable)?;
    store.write(area, MARKER, &bytes)
}

/// Whether `value` is a `T` as its strict reader parses it.
pub fn strict<T: serde::de::DeserializeOwned>(value: &Value) -> bool {
    T::deserialize(value).is_ok()
}
