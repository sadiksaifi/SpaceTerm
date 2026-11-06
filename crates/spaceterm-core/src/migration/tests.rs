use super::*;
use serde_json::json;
use std::cell::{Cell, RefCell};

/// A root in memory that records every marker write and credential effect in order.
#[derive(Default)]
struct Memory {
    files: RefCell<BTreeMap<(u8, String), Vec<u8>>>,
    events: RefCell<Vec<String>>,
    /// Writing the root's marker fails while set, as a crash just before it would.
    marker_fails: Cell<bool>,
}

fn area(area: Area) -> u8 {
    match area {
        Area::Root => 0,
        Area::Backup => 1,
        Area::Reset => 2,
    }
}

impl Memory {
    fn with(files: &[(&str, Value)]) -> Self {
        let memory = Self::default();
        for (path, value) in files {
            memory.put(Area::Root, path, value);
        }
        memory
    }
    fn put(&self, at: Area, path: &str, value: &Value) {
        self.files
            .borrow_mut()
            .insert((area(at), path.into()), serde_json::to_vec(value).unwrap());
    }
    fn get(&self, at: Area, path: &str) -> Option<Value> {
        self.files
            .borrow()
            .get(&(area(at), path.to_owned()))
            .map(|bytes| serde_json::from_slice(bytes).unwrap())
    }
    fn generation(&self, at: Area) -> Option<Value> {
        self.get(at, MARKER)
            .map(|marker| marker["generation"].clone())
    }
}

impl Store for Memory {
    fn list(&self, at: Area) -> Result<Vec<String>, StoreError> {
        Ok(self
            .files
            .borrow()
            .keys()
            .filter(|(stored, _)| *stored == area(at))
            .map(|(_, path)| path.clone())
            .collect())
    }
    fn read(&self, at: Area, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.files.borrow().get(&(area(at), path.into())).cloned())
    }
    fn write(&self, at: Area, path: &str, bytes: &[u8]) -> Result<(), StoreError> {
        if at == Area::Root && path == MARKER {
            if self.marker_fails.get() {
                return Err(StoreError::Unavailable);
            }
            self.events.borrow_mut().push("marker".into());
        }
        self.files
            .borrow_mut()
            .insert((area(at), path.into()), bytes.to_vec());
        Ok(())
    }
    fn remove(&self, at: Area, path: &str) -> Result<(), StoreError> {
        self.files.borrow_mut().remove(&(area(at), path.into()));
        Ok(())
    }
}

/// Credential effects recorded into the store's event log, so their order against the marker
/// shows.
struct Recorded<'a>(&'a Memory);

impl Credentials for Recorded<'_> {
    fn copy(
        &mut self,
        from: &CredentialItem,
        to: &CredentialItem,
    ) -> Result<(), CredentialFailure> {
        self.0
            .events
            .borrow_mut()
            .push(format!("copy {} {}", from.key[0], to.key[0]));
        Ok(())
    }
    fn remove(&mut self, item: &CredentialItem) -> Result<(), CredentialFailure> {
        self.0
            .events
            .borrow_mut()
            .push(format!("remove {}", item.key[0]));
        Ok(())
    }
}

fn counter(value: &Value) -> bool {
    value.get("count").and_then(Value::as_u64).is_some() && value.as_object().unwrap().len() == 1
}
fn named(value: &Value) -> bool {
    value.get("name").and_then(Value::as_str).is_some() && value.as_object().unwrap().len() == 1
}

const RULES: &[FileRule] = &[
    FileRule {
        name: "view.json",
        kind: DataKind::View,
        read: counter,
        stamp: None,
    },
    FileRule {
        name: "identity.json",
        kind: DataKind::MachineId,
        read: named,
        stamp: None,
    },
    FileRule {
        name: "organization.json",
        kind: DataKind::WorkspaceOrganization,
        read: named,
        stamp: None,
    },
];

/// Generation 1 counts in `n`; generation 2 counts in `count`.
fn rename(value: &mut Value) -> Result<(), Unmigratable> {
    let object = value.as_object_mut().ok_or(Unmigratable)?;
    let count = object.remove("n").ok_or(Unmigratable)?;
    object.insert("count".into(), count);
    Ok(())
}
/// Generation 2 to 3 counts once more, so a step that runs twice shows.
fn increment(value: &mut Value) -> Result<(), Unmigratable> {
    let count = value["count"].as_u64().ok_or(Unmigratable)?;
    value["count"] = json!(count + 1);
    Ok(())
}
/// Generation 2 to 3 also splits the organization out of `identity.json`.
fn split(migration: &mut Migration<'_>) -> Result<(), Unmigratable> {
    let identity = migration
        .files
        .get_mut("identity.json")
        .and_then(Value::as_object_mut)
        .ok_or(Unmigratable)?;
    let organization = identity.remove("organization").ok_or(Unmigratable)?;
    migration
        .files
        .insert("organization.json".into(), json!({ "name": organization }));
    Ok(())
}

const STEPS: &[&[Step]] = &[
    &[Step::File {
        name: "view.json",
        apply: rename,
    }],
    &[
        Step::File {
            name: "view.json",
            apply: increment,
        },
        Step::Root(split),
    ],
];

const REGISTRY: Registry = Registry {
    admission: crate::migration::Admission::GenerationOne,
    generations: Generations {
        oldest: 1,
        current: 3,
    },
    steps: STEPS,
    files: &[RULES],
};

fn generation_one() -> Memory {
    Memory::with(&[
        (MARKER, json!({ "generation": 1 })),
        ("clients/a/view.json", json!({ "n": 4 })),
        (
            "identity.json",
            json!({ "name": "core", "organization": "work" }),
        ),
    ])
}

fn run_once(registry: &Registry, store: &Memory) -> Result<Outcome, Refusal> {
    run(registry, store, &mut Recorded(store), false)
}

#[test]
fn confirmed_resets_preserve_earlier_files_and_resume_after_a_failed_marker() {
    let registry = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 3,
            current: 3,
        },
        steps: &[],
        ..REGISTRY
    };
    let memory = Memory::with(&[
        (MARKER, json!({ "generation": 1 })),
        ("identity.json", json!({ "name": "first" })),
    ]);
    reset_confirmed(&registry, &memory, &mut NoCredentials).unwrap();
    memory.put(Area::Root, MARKER, &json!({ "generation": 1 }));
    memory.put(Area::Root, "identity.json", &json!({ "name": "second" }));
    memory.marker_fails.set(true);
    assert_eq!(
        reset_confirmed(&registry, &memory, &mut NoCredentials),
        Err(Refusal::Storage(StoreError::Unavailable))
    );
    memory.marker_fails.set(false);
    assert!(run_once(&registry, &memory).is_ok());
    assert_eq!(
        memory.get(Area::Reset, "gen-1/identity.json"),
        Some(json!({ "name": "first" }))
    );
    assert_eq!(
        memory.get(Area::Reset, "gen-1-1/identity.json"),
        Some(json!({ "name": "second" }))
    );
    assert_eq!(memory.generation(Area::Root), Some(json!(3)));
}

#[test]
fn a_root_at_the_current_generation_is_left_alone() {
    let store = Memory::with(&[
        (MARKER, json!({ "generation": 3 })),
        ("view.json", json!({ "unreadable": true })),
    ]);
    assert_eq!(run_once(&REGISTRY, &store), Ok(Outcome::Current));
    assert_eq!(
        store.get(Area::Root, "view.json"),
        Some(json!({ "unreadable": true }))
    );
    assert!(store.list(Area::Backup).unwrap().is_empty());
}

#[test]
fn an_empty_root_starts_at_the_current_generation_and_a_retained_one_at_generation_one() {
    let empty = Memory::default();
    assert_eq!(run_once(&REGISTRY, &empty), Ok(Outcome::Current));
    assert_eq!(empty.generation(Area::Root), Some(json!(3)));

    let unmarked = generation_one();
    unmarked.remove(Area::Root, MARKER).unwrap();
    assert_eq!(
        run_once(&REGISTRY, &unmarked),
        Ok(Outcome::Migrated { from: 1 })
    );
}

#[test]
fn a_root_between_oldest_and_current_migrates_through_every_step_with_a_backup() {
    let store = generation_one();
    assert_eq!(
        run_once(&REGISTRY, &store),
        Ok(Outcome::Migrated { from: 1 })
    );
    assert_eq!(
        store.get(Area::Root, "clients/a/view.json"),
        Some(json!({ "count": 5 }))
    );
    assert_eq!(
        store.get(Area::Root, "identity.json"),
        Some(json!({ "name": "core" }))
    );
    assert_eq!(
        store.get(Area::Root, "organization.json"),
        Some(json!({ "name": "work" }))
    );
    assert_eq!(store.generation(Area::Root), Some(json!(3)));
    assert_eq!(store.generation(Area::Backup), Some(json!(1)));
    assert_eq!(
        store.get(Area::Backup, "clients/a/view.json"),
        Some(json!({ "n": 4 }))
    );
    assert_eq!(store.get(Area::Backup, "organization.json"), None);
    assert_eq!(run_once(&REGISTRY, &store), Ok(Outcome::Current));
}

#[test]
fn a_migration_interrupted_before_its_marker_restarts_from_the_backup() {
    let store = generation_one();
    store.marker_fails.set(true);
    assert_eq!(
        run_once(&REGISTRY, &store),
        Err(Refusal::Storage(StoreError::Unavailable))
    );
    // The files were rewritten, but the root still names generation 1.
    assert_eq!(
        store.get(Area::Root, "clients/a/view.json"),
        Some(json!({ "count": 5 }))
    );
    assert_eq!(store.generation(Area::Root), Some(json!(1)));
    store.marker_fails.set(false);
    assert_eq!(
        run_once(&REGISTRY, &store),
        Ok(Outcome::Migrated { from: 1 })
    );
    assert_eq!(
        store.get(Area::Root, "clients/a/view.json"),
        Some(json!({ "count": 5 })),
        "each step ran once on the generation 1 files"
    );
    assert_eq!(
        store.get(Area::Root, "organization.json"),
        Some(json!({ "name": "work" }))
    );
}

#[test]
fn a_newer_root_is_refused_unchanged_unless_an_activation_returned_to_its_backup() {
    let older = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 1,
            current: 1,
        },
        steps: &[],
        files: &[RULES],
    };
    let store = generation_one();
    assert_eq!(
        run_once(&REGISTRY, &store),
        Ok(Outcome::Migrated { from: 1 })
    );
    let migrated = store.files.borrow().clone();
    assert_eq!(run_once(&older, &store), Err(Refusal::Newer));
    assert_eq!(*store.files.borrow(), migrated);

    assert_eq!(
        run(&older, &store, &mut Recorded(&store), true),
        Ok(Outcome::Restored)
    );
    assert_eq!(store.generation(Area::Root), Some(json!(1)));
    assert_eq!(
        store.get(Area::Root, "clients/a/view.json"),
        Some(json!({ "n": 4 }))
    );
    assert_eq!(
        store.get(Area::Root, "identity.json"),
        Some(json!({ "name": "core", "organization": "work" }))
    );
    assert_eq!(store.get(Area::Root, "organization.json"), None);

    // A backup of another generation is not this build's data.
    let newer = Memory::with(&[
        (MARKER, json!({ "generation": 5 })),
        ("identity.json", json!({ "name": "core" })),
    ]);
    newer.put(Area::Backup, MARKER, &json!({ "generation": 2 }));
    assert_eq!(
        run(&older, &newer, &mut Recorded(&newer), true),
        Err(Refusal::Newer)
    );
    assert_eq!(newer.generation(Area::Root), Some(json!(5)));
}

#[test]
fn a_migration_a_newer_build_left_unfinished_is_undone_by_the_build_it_started_from() {
    let newer = generation_one();
    // A newer build took its backup and rewrote the files, then stopped before its marker.
    newer.put(Area::Backup, MARKER, &json!({ "generation": 1 }));
    newer.put(
        Area::Backup,
        "clients/a/view.json",
        &newer.get(Area::Root, "clients/a/view.json").unwrap(),
    );
    newer.put(
        Area::Backup,
        "identity.json",
        &newer.get(Area::Root, "identity.json").unwrap(),
    );
    newer.put(Area::Root, "clients/a/view.json", &json!({ "count": 9 }));
    newer.put(Area::Root, "organization.json", &json!({ "name": "work" }));
    let older = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 1,
            current: 1,
        },
        steps: &[],
        files: &[RULES],
    };
    assert_eq!(run_once(&older, &newer), Ok(Outcome::Current));
    assert_eq!(
        newer.get(Area::Root, "clients/a/view.json"),
        Some(json!({ "n": 4 }))
    );
    assert_eq!(newer.get(Area::Root, "organization.json"), None);
    // The backup is settled, so a later write of this build stays.
    newer.put(Area::Root, "clients/a/view.json", &json!({ "n": 5 }));
    assert_eq!(run_once(&older, &newer), Ok(Outcome::Current));
    assert_eq!(
        newer.get(Area::Root, "clients/a/view.json"),
        Some(json!({ "n": 5 }))
    );
}

#[test]
fn an_interrupted_return_restore_repeats_until_its_marker_is_written() {
    let older = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 1,
            current: 1,
        },
        steps: &[],
        files: &[RULES],
    };
    let store = generation_one();
    assert_eq!(
        run_once(&REGISTRY, &store),
        Ok(Outcome::Migrated { from: 1 })
    );
    store.marker_fails.set(true);
    assert!(run(&older, &store, &mut Recorded(&store), true).is_err());
    store.marker_fails.set(false);
    assert_eq!(
        run(&older, &store, &mut Recorded(&store), true),
        Ok(Outcome::Restored)
    );
    assert_eq!(store.generation(Area::Root), Some(json!(1)));
    assert_eq!(
        store.get(Area::Root, "identity.json"),
        Some(json!({ "name": "core", "organization": "work" }))
    );
}

#[test]
fn a_root_below_oldest_changes_nothing_until_the_person_confirms_its_reset() {
    let registry = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 2,
            current: 3,
        },
        steps: &STEPS[1..],
        files: &[RULES],
    };
    let store = generation_one();
    let before = store.files.borrow().clone();
    assert_eq!(
        run_once(&registry, &store),
        Err(Refusal::Reset {
            reason: ResetReason::TooOld,
            kinds: vec![DataKind::MachineId],
        })
    );
    // Every file stays as it was, the rebuildable data included, and nothing records the
    // refusal, so Quit leaves the root unchanged.
    assert_eq!(*store.files.borrow(), before);
    assert!(store.get(Area::Root, "identity.json").is_some());
    assert_eq!(store.generation(Area::Root), Some(json!(1)));
    // The refusal repeats until the person confirms.
    assert!(matches!(
        run_once(&registry, &store),
        Err(Refusal::Reset { .. })
    ));

    assert_eq!(
        reset_confirmed(&registry, &store, &mut Recorded(&store)),
        Ok(vec![DataKind::View, DataKind::MachineId])
    );
    assert_eq!(
        store.get(Area::Reset, "gen-1/clients/a/view.json"),
        Some(json!({ "n": 4 }))
    );
    assert_eq!(store.get(Area::Root, "identity.json"), None);
    assert_eq!(
        store.get(Area::Reset, "gen-1/identity.json"),
        Some(json!({ "name": "core", "organization": "work" }))
    );
    assert_eq!(store.generation(Area::Root), Some(json!(3)));
    assert_eq!(take_notice(&store), Ok(vec![DataKind::View]));
    assert_eq!(take_notice(&store), Ok(Vec::new()), "the notice shows once");
}

#[test]
fn only_a_recorded_run_records_a_refusal_for_another_process() {
    let registry = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 2,
            current: 3,
        },
        steps: &STEPS[1..],
        files: &[RULES],
    };
    let store = generation_one();
    assert!(run(&registry, &store, &mut Recorded(&store), false).is_err());
    assert_eq!(pending(&store), Ok(None));
    assert!(run_recorded(&registry, &store, &mut Recorded(&store), false).is_err());
    assert_eq!(
        pending(&store),
        Ok(Some(PendingReset {
            reason: ResetReason::TooOld,
            kinds: vec![DataKind::MachineId],
        }))
    );
}

#[test]
fn a_root_of_only_rebuildable_data_resets_without_asking() {
    let store = Memory::with(&[
        (MARKER, json!({ "generation": 2 })),
        ("view.json", json!({ "count": "unreadable" })),
    ]);
    assert_eq!(run_once(&REGISTRY, &store), Ok(Outcome::Reset));
    assert_eq!(store.get(Area::Root, "view.json"), None);
    assert_eq!(store.generation(Area::Root), Some(json!(3)));
    assert_eq!(take_notice(&store), Ok(vec![DataKind::View]));
}

#[test]
fn a_refusing_step_or_an_unreadable_result_keeps_the_original_files() {
    let refusing = Memory::with(&[
        (MARKER, json!({ "generation": 2 })),
        ("identity.json", json!({ "name": "core" })),
    ]);
    assert_eq!(
        run_once(&REGISTRY, &refusing),
        Err(Refusal::Reset {
            reason: ResetReason::Unmigratable,
            kinds: vec![DataKind::MachineId],
        })
    );
    assert_eq!(
        refusing.get(Area::Root, "identity.json"),
        Some(json!({ "name": "core" }))
    );

    let invalid = Memory::with(&[
        (MARKER, json!({ "generation": 2 })),
        (
            "identity.json",
            json!({ "name": "core", "organization": 7, "extra": 1 }),
        ),
    ]);
    assert!(matches!(
        run_once(&REGISTRY, &invalid),
        Err(Refusal::Reset {
            reason: ResetReason::Invalid,
            ..
        })
    ));
    assert_eq!(invalid.generation(Area::Root), Some(json!(2)));
}

#[test]
fn a_confirmation_never_resets_a_root_that_no_longer_needs_it() {
    let store = generation_one();
    assert_eq!(
        reset_confirmed(&REGISTRY, &store, &mut Recorded(&store)),
        Ok(Vec::new())
    );
    assert_eq!(
        store.get(Area::Root, "identity.json"),
        Some(json!({ "name": "core" }))
    );
    assert!(store.list(Area::Reset).unwrap().is_empty());
}

fn item(key: u8) -> CredentialItem {
    CredentialItem {
        store: "keychain".into(),
        key: [key; 32],
    }
}

fn replace(migration: &mut Migration<'_>) -> Result<(), Unmigratable> {
    migration.replace_credential(item(1), item(2))?;
    let identity = migration
        .files
        .get_mut("identity.json")
        .ok_or(Unmigratable)?;
    identity["name"] = json!("replaced");
    Ok(())
}

fn replace_then_refuse(migration: &mut Migration<'_>) -> Result<(), Unmigratable> {
    migration.replace_credential(item(1), item(2))?;
    Err(Unmigratable)
}

#[test]
fn replacement_credentials_come_first_and_old_items_go_only_once_their_backup_is_gone() {
    const REPLACING: &[&[Step]] = &[&[Step::Root(replace)], &[]];
    let registry = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 1,
            current: 2,
        },
        steps: &REPLACING[..1],
        files: &[RULES],
    };
    let later = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 1,
            current: 3,
        },
        steps: REPLACING,
        files: &[RULES],
    };
    let older = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 1,
            current: 1,
        },
        steps: &[],
        files: &[RULES],
    };
    let store = Memory::with(&[
        (MARKER, json!({ "generation": 1 })),
        ("identity.json", json!({ "name": "core" })),
    ]);
    // The marker may have published even though its write failed, so the replacement stays.
    store.marker_fails.set(true);
    assert!(run_once(&registry, &store).is_err());
    assert_eq!(*store.events.borrow(), ["copy 1 2"]);
    store.events.borrow_mut().clear();
    store.marker_fails.set(false);
    assert_eq!(
        run_once(&registry, &store),
        Ok(Outcome::Migrated { from: 1 })
    );
    assert_eq!(
        *store.events.borrow(),
        ["marker", "copy 1 2", "marker"],
        "the unfinished attempt is undone, and the original stays while its backup can come back"
    );
    store.events.borrow_mut().clear();
    assert_eq!(run_once(&registry, &store), Ok(Outcome::Current));
    assert!(store.events.borrow().is_empty());
    // A later migration replaces the backup, so the original goes first.
    assert_eq!(run_once(&later, &store), Ok(Outcome::Migrated { from: 2 }));
    assert_eq!(*store.events.borrow(), ["remove 1", "marker"]);

    // A return restores the files that refer to the original, which stays for good.
    let store = Memory::with(&[
        (MARKER, json!({ "generation": 1 })),
        ("identity.json", json!({ "name": "core" })),
    ]);
    run_once(&registry, &store).unwrap();
    assert_eq!(
        run(&older, &store, &mut Recorded(&store), true),
        Ok(Outcome::Restored)
    );
    assert_eq!(store.get(Area::Backup, RETIRE), None);
    assert_eq!(run_once(&older, &store), Ok(Outcome::Current));
    assert!(
        !store
            .events
            .borrow()
            .iter()
            .any(|event| event == "remove 1"),
        "{:?}",
        store.events.borrow()
    );

    const REFUSING: &[&[Step]] = &[&[Step::Root(replace_then_refuse)]];
    let refusing = Registry {
        steps: REFUSING,
        ..registry
    };
    let store = Memory::with(&[
        (MARKER, json!({ "generation": 1 })),
        ("identity.json", json!({ "name": "core" })),
    ]);
    assert!(matches!(
        run_once(&refusing, &store),
        Err(Refusal::Reset { .. })
    ));
    assert_eq!(*store.events.borrow(), ["copy 1 2", "remove 2"]);
}

#[test]
fn a_document_migrates_through_its_file_steps_and_carries_the_new_generation() {
    fn stamped(value: &Value) -> bool {
        value.get("schema_version").is_some() && value.get("count").is_some()
    }
    fn rename_document(value: &mut Value) -> Result<(), Unmigratable> {
        let object = value.as_object_mut().ok_or(Unmigratable)?;
        let count = object.remove("n").ok_or(Unmigratable)?;
        object.insert("count".into(), count);
        Ok(())
    }
    const DOCUMENT_STEPS: &[&[Step]] = &[&[Step::File {
        name: "settings.json",
        apply: rename_document,
    }]];
    const DOCUMENT_RULES: &[FileRule] = &[FileRule {
        name: "settings.json",
        kind: DataKind::Settings,
        read: stamped,
        stamp: Some("schema_version"),
    }];
    let registry = Registry {
        admission: crate::migration::Admission::GenerationOne,
        generations: Generations {
            oldest: 1,
            current: 2,
        },
        steps: DOCUMENT_STEPS,
        files: &[DOCUMENT_RULES],
    };
    assert_eq!(
        migrate_document(
            &registry,
            "settings.json",
            json!({ "schema_version": 1, "n": 2 })
        ),
        Ok(json!({ "schema_version": 2, "count": 2 }))
    );
    assert_eq!(
        migrate_document(
            &registry,
            "settings.json",
            json!({ "schema_version": 3, "n": 2 })
        ),
        Err(Refusal::Newer)
    );
    assert!(matches!(
        migrate_document(&registry, "settings.json", json!({ "schema_version": 1 })),
        Err(Refusal::Reset {
            reason: ResetReason::Unmigratable,
            ..
        })
    ));
}
