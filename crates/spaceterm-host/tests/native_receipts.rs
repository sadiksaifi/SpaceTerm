use spaceterm_host::{
    native_deployment::contracts::{NativeFailure, NativeRequestId, PublicationDisposition},
    native_receipts::{
        InventoryBinding, InventoryResource, NativeEffectBinding, ProtectedInventoryStorage,
        ResourceInventory,
    },
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Storage(Arc<Mutex<StorageState>>);
#[derive(Default)]
struct StorageState {
    bytes: Option<Vec<u8>>,
    uncertain: bool,
    fail_next: bool,
}
impl ProtectedInventoryStorage for Storage {
    fn read(&self, _run: NativeRequestId) -> Result<Vec<u8>, NativeFailure> {
        let state = self.0.lock().unwrap();
        if state.uncertain {
            return Err(NativeFailure::Indeterminate);
        }
        state
            .bytes
            .clone()
            .ok_or(NativeFailure::RegistrationMissing)
    }
    fn publish(
        &self,
        _run: NativeRequestId,
        previous: Option<&[u8]>,
        bytes: &[u8],
    ) -> PublicationDisposition<()> {
        let mut state = self.0.lock().unwrap();
        if state.uncertain {
            return PublicationDisposition::Indeterminate;
        }
        if state.bytes.as_deref() != previous {
            return PublicationDisposition::NotApplied(NativeFailure::StaleAssessment);
        }
        state.bytes = Some(bytes.to_vec());
        if state.fail_next {
            state.uncertain = true;
            return PublicationDisposition::Indeterminate;
        }
        PublicationDisposition::Durable(())
    }
}
fn binding() -> InventoryBinding {
    InventoryBinding::new(NativeRequestId::new([1; 16]).unwrap(), [2; 32], [3; 32]).unwrap()
}
fn object(inode: u64) -> NativeEffectBinding {
    NativeEffectBinding::object([4; 16], 7, inode, 1, 8, 9, [5; 32]).unwrap()
}
fn process(start: u64) -> NativeEffectBinding {
    NativeEffectBinding::process([6; 16], 123, start, 4, [7; 32]).unwrap()
}

#[test]
fn interrupted_creation_retains_exact_identity_before_next_effect() {
    let storage = Storage::default();
    let mut inventory = ResourceInventory::create(storage.clone(), binding()).unwrap();
    inventory.begin(InventoryResource::Installation).unwrap();
    assert_eq!(
        inventory.identity(InventoryResource::Installation),
        Err(NativeFailure::CleanupUnresolved)
    );
    inventory
        .record_identity(InventoryResource::Installation, object(20))
        .unwrap();
    let restored = ResourceInventory::retain(storage.clone(), binding()).unwrap();
    assert_eq!(
        restored.identity(InventoryResource::Installation).unwrap(),
        &object(20)
    );
    // A publication callback losing its result does not turn a pending effect
    // into an unrecorded name-based removal.
    assert_eq!(restored.created_resources(), 0);
    inventory
        .completed(InventoryResource::Installation)
        .unwrap();
    assert_eq!(
        ResourceInventory::retain(storage, binding())
            .unwrap()
            .created_resources(),
        1
    );
}

#[test]
fn protected_inventory_is_not_an_unprivileged_log_or_an_artifact_hash_match() {
    let storage = Storage::default();
    let mut inventory = ResourceInventory::create(storage.clone(), binding()).unwrap();
    inventory.begin(InventoryResource::Installation).unwrap();
    inventory
        .record_identity(InventoryResource::Installation, object(20))
        .unwrap();
    inventory
        .completed(InventoryResource::Installation)
        .unwrap();
    // Same bytes and name, a different native inode.
    assert_ne!(
        inventory.identity(InventoryResource::Installation).unwrap(),
        &object(21)
    );
    let different_candidate =
        InventoryBinding::new(NativeRequestId::new([1; 16]).unwrap(), [2; 32], [9; 32]).unwrap();
    assert!(matches!(
        ResourceInventory::retain(storage.clone(), different_candidate),
        Err(NativeFailure::IdentityMismatch)
    ));
    storage.0.lock().unwrap().bytes = Some(b"{\"schema\":1,\"result\":\"passed\"}".to_vec());
    assert!(matches!(
        ResourceInventory::retain(storage, binding()),
        Err(NativeFailure::InvalidRecord)
    ));
}

#[test]
fn item_recreation_and_pid_reuse_preserve_original_identities_for_product_authority() {
    let storage = Storage::default();
    let mut inventory = ResourceInventory::create(storage.clone(), binding()).unwrap();
    let item = NativeEffectBinding::credential_item(vec![1, 2, 3, 4], [8; 32]).unwrap();
    for (resource, identity) in [
        (InventoryResource::Credential, item.clone()),
        (InventoryResource::Server, process(100)),
    ] {
        inventory.begin(resource).unwrap();
        inventory.record_identity(resource, identity).unwrap();
        inventory.completed(resource).unwrap();
    }
    let restored = ResourceInventory::retain(storage, binding()).unwrap();
    assert_ne!(
        restored.identity(InventoryResource::Credential).unwrap(),
        &NativeEffectBinding::credential_item(vec![1, 2, 3, 5], [8; 32]).unwrap()
    );
    assert_ne!(
        restored.identity(InventoryResource::Server).unwrap(),
        &process(101)
    );
    assert_eq!(format!("{item:?}"), "NativeEffectBinding(..)");
}

#[test]
fn uncertain_identity_publication_is_latched_and_persisted_by_storage_owner() {
    let storage = Storage::default();
    let mut inventory = ResourceInventory::create(storage.clone(), binding()).unwrap();
    inventory.begin(InventoryResource::Registration).unwrap();
    storage.0.lock().unwrap().fail_next = true;
    assert_eq!(
        inventory.record_identity(InventoryResource::Registration, object(30)),
        Err(NativeFailure::Indeterminate)
    );
    storage.0.lock().unwrap().fail_next = false;
    assert_eq!(
        inventory.completed(InventoryResource::Registration),
        Err(NativeFailure::Indeterminate)
    );
    assert!(matches!(
        ResourceInventory::retain(storage, binding()),
        Err(NativeFailure::Indeterminate)
    ));
}

#[test]
fn missing_or_wrong_identity_cannot_be_marked_created_or_replaced_after_completion() {
    let mut inventory = ResourceInventory::create(Storage::default(), binding()).unwrap();
    inventory.begin(InventoryResource::Server).unwrap();
    assert_eq!(
        inventory.completed(InventoryResource::Server),
        Err(NativeFailure::CleanupUnresolved)
    );
    assert_eq!(
        inventory.record_identity(InventoryResource::Server, object(10)),
        Err(NativeFailure::InvalidRecord)
    );
    inventory
        .record_identity(InventoryResource::Server, process(100))
        .unwrap();
    inventory.completed(InventoryResource::Server).unwrap();
    assert_eq!(
        inventory.record_identity(InventoryResource::Server, process(101)),
        Err(NativeFailure::UnsafeNativeState)
    );
    inventory.removed(InventoryResource::Server).unwrap();
    assert_eq!(
        inventory.begin(InventoryResource::Server),
        Err(NativeFailure::UnsafeNativeState)
    );
    assert_eq!(
        inventory.identity(InventoryResource::Server),
        Err(NativeFailure::CleanupUnresolved)
    );
}
