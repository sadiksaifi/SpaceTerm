use super::{
    Candidate, ServiceFailure, Version,
    archive::{self, Tree},
    records::{Identity, Record},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::AccountInstallationManifestV2;
use std::{collections::BTreeSet, fs::File};

const LIMIT: usize = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Created {
    name: String,
    temporary: String,
    identity: Identity,
    directory: bool,
    mode: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Removal {
    parent: String,
    original: String,
    captured: String,
    identity: Identity,
    directory: bool,
    mode: u32,
    root: bool,
    temporary: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    schema: u16,
    candidate: Candidate,
    root: Identity,
    root_temporary: String,
    #[serde(skip)]
    objects: Vec<Created>,
    #[serde(skip)]
    removing: Option<Removal>,
    #[serde(skip)]
    record: Option<Record>,
    #[serde(skip)]
    cleanup_record: Option<Record>,
    #[serde(skip)]
    recorded_objects: usize,
}
impl Journal {
    pub fn create(
        installations: &Tree,
        candidate: Candidate,
    ) -> Result<(Self, Tree), ServiceFailure> {
        let temporary = archive::quarantine_name()?;
        let tree = installations.create_child(&temporary)?;
        tree.sync()?;
        installations.sync()?;
        #[cfg(feature = "test-support")]
        super::test_support::cleanup_boundary(12);
        let mut journal = Self {
            schema: 1,
            candidate,
            root: Identity {
                device: tree.identity()?.0,
                inode: tree.identity()?.1,
            },
            root_temporary: temporary,
            objects: Vec::new(),
            removing: None,
            record: None,
            cleanup_record: None,
            recorded_objects: 0,
        };
        journal.save(installations)?;
        #[cfg(feature = "test-support")]
        super::test_support::cleanup_boundary(9);
        installations
            .rename_exclusive(&journal.root_temporary, &super::version_name(candidate.id))?;
        installations.sync()?;
        Ok((journal, tree))
    }
    pub fn retain_legacy(
        installations: &Tree,
        version: Version,
        tree: &Tree,
        manifest: &AccountInstallationManifestV2,
    ) -> Result<Self, ServiceFailure> {
        // The old format recorded the root identity, not child creation progress.
        // Only a complete authenticated legacy tree can acquire cleanup evidence.
        tree.validate(manifest)?;
        let mut journal = Self {
            schema: 1,
            candidate: super::candidate(version),
            root: Identity {
                device: version.device,
                inode: version.inode,
            },
            root_temporary: archive::quarantine_name()?,
            objects: Vec::new(),
            removing: None,
            record: None,
            cleanup_record: None,
            recorded_objects: 0,
        };
        for name in archive::directories(manifest) {
            let child = parent_at(tree, &name)?;
            let (device, inode) = child.identity()?;
            journal.objects.push(Created {
                name,
                temporary: archive::quarantine_name()?,
                identity: Identity { device, inode },
                directory: true,
                mode: 0o700,
            });
        }
        for expected in &manifest.files {
            let file = tree.read_file(&expected.name, archive::installed_mode(expected.mode))?;
            journal.objects.push(Created {
                name: expected.name.clone(),
                temporary: archive::quarantine_name()?,
                identity: Identity::of(&file)?,
                directory: false,
                mode: archive::installed_mode(expected.mode),
            });
        }
        journal.save(installations)?;
        Ok(journal)
    }
    pub fn read(
        installations: &Tree,
        candidate: Candidate,
    ) -> Result<Option<Self>, ServiceFailure> {
        let Some(record) = installations.record(&name(candidate), LIMIT)? else {
            return Ok(None);
        };
        let header = frame(&record.bytes)?.ok_or(ServiceFailure::InvalidRecord)?;
        let mut remaining = header.remaining;
        let mut journal: Self =
            serde_json::from_slice(header.payload).map_err(|_| ServiceFailure::InvalidRecord)?;
        while let Some(frame) = frame(remaining)? {
            if journal.objects.len() >= 2 * spaceterm_distribution::manifest::MAX_FILES {
                return Err(ServiceFailure::LimitExceeded);
            }
            let bytes = frame.payload;
            let rest = frame.remaining;
            journal
                .objects
                .push(serde_json::from_slice(bytes).map_err(|_| ServiceFailure::InvalidRecord)?);
            remaining = rest;
        }
        // An incomplete final creation frame was never exposed. Keep its bytes
        // for exact removal, but never infer an object's identity from that tail.
        journal.recorded_objects = journal.objects.len();
        journal.cleanup_record = installations.record(&cleanup_record_name(candidate), 4096)?;
        if let Some(record) = &journal.cleanup_record {
            journal.removing =
                serde_json::from_slice(&record.bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
        }
        if journal.schema != 1
            || journal.candidate != candidate
            || journal.root.inode == 0
            || !component(&journal.root_temporary)
            || journal.objects.len() > 2 * spaceterm_distribution::manifest::MAX_FILES
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        let mut names = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for object in &journal.objects {
            if !relative(&object.name)
                || !component(&object.temporary)
                || object.identity.inode == 0
                || !matches!(object.mode, 0o600 | 0o700)
                || (object.directory && object.mode != 0o700)
                || !names.insert(&object.name)
                || !identities.insert(object.identity.tuple())
            {
                return Err(ServiceFailure::InvalidRecord);
            }
        }
        if let Some(removing) = &journal.removing {
            if (!removing.parent.is_empty() && !relative(&removing.parent))
                || !component(&removing.original)
                || !component(&removing.captured)
                || removing.identity.inode == 0
                || !matches!(removing.mode, 0o600 | 0o700)
            {
                return Err(ServiceFailure::InvalidRecord);
            }
            let recorded = if removing.root {
                removing.parent.is_empty()
                    && removing.directory
                    && removing.identity == journal.root
                    && (removing.original == super::version_name(candidate.id)
                        || removing.original == journal.root_temporary)
            } else {
                journal.objects.iter().any(|object| {
                    let (parent, leaf) = parts(&object.name);
                    ((removing.temporary
                        && removing.parent.is_empty()
                        && object.temporary == removing.original)
                        || (!removing.temporary
                            && parent == removing.parent
                            && leaf == removing.original))
                        && object.identity == removing.identity
                        && object.directory == removing.directory
                        && object.mode == removing.mode
                })
            };
            if !recorded {
                return Err(ServiceFailure::InvalidRecord);
            }
        }
        // Visible creation frames or a removal rename can precede their original
        // sync. Establish these exact recovered records before their identities
        // authorize any capture or unlink, including in a different parent.
        installations.stabilize_record(&name(candidate), &record)?;
        if let Some(record) = &journal.cleanup_record {
            installations.stabilize_record(&cleanup_record_name(candidate), record)?;
        }
        journal.record = Some(record);
        Ok(Some(journal))
    }
    pub fn version(&self) -> Version {
        Version {
            id: self.candidate.id,
            digest: self.candidate.digest,
            device: self.root.device,
            inode: self.root.inode,
            cleanup_nonce: self.candidate.cleanup_nonce,
            incomplete: true,
        }
    }
    fn save(&mut self, installations: &Tree) -> Result<(), ServiceFailure> {
        if self.record.is_none() {
            // Legacy acquisition already has its complete authenticated inventory.
            // Publish it with the header, never a prefix that recovery could mistake
            // for complete cleanup evidence. New extraction starts with no objects
            // and appends each later creation before exposing that object.
            let mut bytes = encode_frame(self)?;
            for object in &self.objects {
                let frame = encode_frame(object)?;
                if bytes
                    .len()
                    .checked_add(frame.len())
                    .is_none_or(|size| size > LIMIT)
                {
                    return Err(ServiceFailure::LimitExceeded);
                }
                bytes.extend_from_slice(&frame);
            }
            installations.publish_record(&name(self.candidate), &bytes, None)?;
            self.record = Some(
                installations
                    .record(&name(self.candidate), LIMIT)?
                    .ok_or(ServiceFailure::Indeterminate)?,
            );
            self.recorded_objects = self.objects.len();
        }
        for object in &self.objects[self.recorded_objects..] {
            let bytes = encode_frame(object)?;
            let record = self.record.as_mut().ok_or(ServiceFailure::InvalidRecord)?;
            if record
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|size| size > LIMIT)
            {
                return Err(ServiceFailure::LimitExceeded);
            }
            installations.append_record(
                &name(self.candidate),
                record.identity,
                record.bytes.len() as u64,
                &bytes,
            )?;
            record.bytes.extend_from_slice(&bytes);
        }
        self.recorded_objects = self.objects.len();
        if self.removing.is_some() || self.cleanup_record.is_some() {
            let bytes =
                serde_json::to_vec(&self.removing).map_err(|_| ServiceFailure::InvalidRecord)?;
            installations.publish_record(
                &cleanup_record_name(self.candidate),
                &bytes,
                self.cleanup_record.as_ref(),
            )?;
            self.cleanup_record =
                installations.record(&cleanup_record_name(self.candidate), 4096)?;
        }
        Ok(())
    }
    pub fn tree(&self, installations: &Tree) -> Result<Option<Tree>, ServiceFailure> {
        let original = super::version_name(self.candidate.id);
        let captured = self
            .removing
            .as_ref()
            .filter(|removing| removing.root)
            .map(|r| r.captured.as_str());
        let mut result = None;
        for name in std::iter::once(original.as_str())
            .chain(std::iter::once(self.root_temporary.as_str()))
            .chain(captured)
        {
            if let Some(found) = installations.entry_identity(name)? {
                if found != self.root || result.is_some() {
                    return Err(ServiceFailure::CleanupUnresolved);
                }
                result = installations.child(name)?;
            }
        }
        Ok(result)
    }
    pub fn parent(
        &mut self,
        installations: &Tree,
        root: &Tree,
        name: &str,
    ) -> Result<(Tree, String), ServiceFailure> {
        let (parents, leaf) = parts(name);
        let mut directory = root.retained()?;
        let mut prefix = String::new();
        for part in parents.split('/').filter(|part| !part.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            directory = match directory.child(part)? {
                Some(child) => child,
                None => {
                    let temporary = archive::quarantine_name()?;
                    let child = installations.create_child(&temporary)?;
                    child.sync()?;
                    installations.sync()?;
                    let (device, inode) = child.identity()?;
                    self.objects.push(Created {
                        name: prefix.clone(),
                        temporary: temporary.clone(),
                        identity: Identity { device, inode },
                        directory: true,
                        mode: 0o700,
                    });
                    self.save(installations)?;
                    installations.move_exclusive(&temporary, &directory, part)?;
                    directory.sync()?;
                    installations.sync()?;
                    child
                }
            };
        }
        Ok((directory, leaf.into()))
    }
    pub fn file(
        &mut self,
        installations: &Tree,
        parent: &Tree,
        name: &str,
        mode: u32,
    ) -> Result<File, ServiceFailure> {
        let temporary = archive::quarantine_name()?;
        let file = installations.create_file(&temporary, mode)?;
        archive::sync(&file)?;
        installations.sync()?;
        #[cfg(feature = "test-support")]
        super::test_support::cleanup_boundary(13);
        self.objects.push(Created {
            name: name.into(),
            temporary: temporary.clone(),
            identity: Identity::of(&file)?,
            directory: false,
            mode,
        });
        self.save(installations)?;
        #[cfg(feature = "test-support")]
        super::test_support::cleanup_boundary(11);
        installations.move_exclusive(&temporary, parent, parts(name).1)?;
        parent.sync()?;
        installations.sync()?;
        Ok(file)
    }
    pub fn cleanup(
        &mut self,
        installations: &Tree,
        manifest: Option<&AccountInstallationManifestV2>,
        incomplete: bool,
    ) -> Result<(), ServiceFailure> {
        if self.removing.is_some() {
            self.finish_removal(installations, manifest, incomplete)?;
        }
        if let Some(root) = self.tree(installations)? {
            let order: Vec<_> = (0..self.objects.len())
                .filter(|index| !self.objects[*index].directory)
                .chain(
                    (0..self.objects.len())
                        .rev()
                        .filter(|index| self.objects[*index].directory),
                )
                .collect();
            for index in order {
                let object = &self.objects[index];
                let (parent_name, leaf) = parts(&object.name);
                let parent = match parent_at(&root, parent_name) {
                    Ok(parent) => parent,
                    Err(ServiceFailure::Missing) => continue,
                    Err(error) => return Err(error),
                };
                let original = parent.entry_identity(leaf)?;
                let temporary = installations.entry_identity(&object.temporary)?;
                if original.is_some_and(|found| found != object.identity)
                    || temporary.is_some_and(|found| found != object.identity)
                    || (original.is_some() && temporary.is_some())
                {
                    return Err(ServiceFailure::CleanupUnresolved);
                }
                let found_name = if original.is_some() {
                    leaf
                } else if temporary.is_some() {
                    &object.temporary
                } else {
                    continue;
                };
                self.removing = Some(Removal {
                    parent: if temporary.is_some() {
                        String::new()
                    } else {
                        parent_name.into()
                    },
                    original: found_name.into(),
                    captured: archive::quarantine_name()?,
                    identity: object.identity,
                    directory: object.directory,
                    mode: object.mode,
                    root: false,
                    temporary: temporary.is_some(),
                });
                self.save(installations)?;
                self.finish_removal(installations, manifest, incomplete)?;
            }
            #[cfg(feature = "test-support")]
            super::test_support::removing_version();
            self.removing = Some(Removal {
                parent: String::new(),
                original: super::version_name(self.candidate.id),
                captured: super::cleanup_name(self.version()),
                identity: self.root,
                directory: true,
                mode: 0o700,
                root: true,
                temporary: false,
            });
            // A root still at its recorded creation temporary was never published.
            if installations.entry_identity(&self.root_temporary)? == Some(self.root)
                && let Some(removing) = &mut self.removing
            {
                removing.original.clone_from(&self.root_temporary);
            }
            self.save(installations)?;
            self.finish_removal(installations, manifest, incomplete)?;
            #[cfg(feature = "test-support")]
            super::test_support::removed_version();
        }
        Ok(())
    }
    fn finish_removal(
        &mut self,
        installations: &Tree,
        manifest: Option<&AccountInstallationManifestV2>,
        incomplete: bool,
    ) -> Result<(), ServiceFailure> {
        let removal = self
            .removing
            .as_ref()
            .ok_or(ServiceFailure::InvalidRecord)?;
        let parent = if removal.root || removal.temporary {
            installations.retained()?
        } else {
            parent_at(
                &self
                    .tree(installations)?
                    .ok_or(ServiceFailure::CleanupUnresolved)?,
                &removal.parent,
            )?
        };
        let original = parent.entry_identity(&removal.original)?;
        let captured = parent.entry_identity(&removal.captured)?;
        if original.is_some_and(|found| found != removal.identity)
            || captured.is_some_and(|found| found != removal.identity)
            || (original.is_some() && captured.is_some())
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        if original.is_some() {
            authenticate(&parent, &removal.original, removal, manifest, incomplete)?;
            #[cfg(feature = "test-support")]
            if !removal.directory {
                super::test_support::removing_file();
            }
            // Revalidate immediately before the rename, and again before unlinkat.
            if parent.entry_identity(&removal.original)? != Some(removal.identity) {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            parent.rename_exclusive(&removal.original, &removal.captured)?;
            #[cfg(feature = "test-support")]
            if !removal.directory {
                super::test_support::cleanup_boundary(1);
            }
        }
        if original.is_some() || captured.is_some() {
            authenticate(&parent, &removal.captured, removal, manifest, incomplete)?;
            #[cfg(feature = "test-support")]
            super::test_support::cleanup_boundary(if removal.root {
                5
            } else if removal.directory {
                6
            } else {
                2
            });
            parent.sync()?;
            #[cfg(feature = "test-support")]
            if !removal.directory {
                super::test_support::cleanup_boundary(3);
            }
            parent.unlink_expected(
                &removal.captured,
                removal.directory,
                removal.identity.tuple(),
            )?;
            #[cfg(feature = "test-support")]
            if !removal.directory {
                super::test_support::cleanup_boundary(4);
            }
        }
        parent.sync()?;
        self.removing = None;
        self.save(installations)
    }
    pub fn remove(self, installations: &Tree) -> Result<(), ServiceFailure> {
        if let Some(record) = &self.cleanup_record {
            installations.remove_record(&cleanup_record_name(self.candidate), record)?;
        }
        if let Some(record) = &self.record {
            installations.remove_record(&name(self.candidate), record)?;
        }
        Ok(())
    }
}
fn authenticate(
    parent: &Tree,
    name: &str,
    removal: &Removal,
    manifest: Option<&AccountInstallationManifestV2>,
    incomplete: bool,
) -> Result<(), ServiceFailure> {
    if removal.directory {
        let child = parent
            .child(name)?
            .ok_or(ServiceFailure::CleanupUnresolved)?;
        if child.identity()? != removal.identity.tuple() {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        child.require_empty()?;
    } else {
        let file = parent.read_file(name, removal.mode)?;
        if Identity::of(&file)? != removal.identity {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        if !incomplete {
            let full = if removal.parent.is_empty() {
                removal.original.clone()
            } else {
                format!("{}/{}", removal.parent, removal.original)
            };
            let expected = manifest
                .and_then(|manifest| manifest.files.iter().find(|file| file.name == full))
                .ok_or(ServiceFailure::ArtifactRejected)?;
            archive::verify_contents(file, expected)?;
        }
    }
    Ok(())
}
fn parent_at(root: &Tree, name: &str) -> Result<Tree, ServiceFailure> {
    let mut directory = root.retained()?;
    for part in name.split('/').filter(|part| !part.is_empty()) {
        directory = directory.child(part)?.ok_or(ServiceFailure::Missing)?;
    }
    Ok(directory)
}
fn parts(name: &str) -> (&str, &str) {
    name.rsplit_once('/').unwrap_or(("", name))
}
fn component(name: &str) -> bool {
    !name.is_empty() && !matches!(name, "." | "..") && !name.contains(['/', '\0'])
}
fn relative(name: &str) -> bool {
    name.len() <= 1024 && name.split('/').all(component)
}
fn name(candidate: Candidate) -> String {
    format!("objects-{}", candidate.id.0.get())
}

fn cleanup_record_name(candidate: Candidate) -> String {
    format!("removal-{}", candidate.id.0.get())
}
fn encode_frame(value: &impl Serialize) -> Result<Vec<u8>, ServiceFailure> {
    let bytes = serde_json::to_vec(value).map_err(|_| ServiceFailure::InvalidRecord)?;
    if bytes.len() > 4096 {
        return Err(ServiceFailure::LimitExceeded);
    }
    let mut frame = Vec::with_capacity(36 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(&Sha256::digest(&bytes));
    frame.extend_from_slice(&bytes);
    Ok(frame)
}
struct Frame<'a> {
    payload: &'a [u8],
    remaining: &'a [u8],
}
fn frame(bytes: &[u8]) -> Result<Option<Frame<'_>>, ServiceFailure> {
    if bytes.len() < 36 {
        return Ok(None);
    }
    let length = u32::from_le_bytes(
        bytes[..4]
            .try_into()
            .map_err(|_| ServiceFailure::InvalidRecord)?,
    ) as usize;
    if length > 4096 {
        return Err(ServiceFailure::InvalidRecord);
    }
    if bytes.len() < 36 + length {
        return Ok(None);
    }
    let payload = &bytes[36..36 + length];
    if Sha256::digest(payload).as_slice() != &bytes[4..36] {
        return Err(ServiceFailure::InvalidRecord);
    }
    Ok(Some(Frame {
        payload,
        remaining: &bytes[36 + length..],
    }))
}
