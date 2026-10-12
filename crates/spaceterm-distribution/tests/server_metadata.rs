//! Compile the server-owned metadata port without a fabricated build identity.
#[path = "../../spaceterm-server/src/distribution_metadata.rs"]
mod metadata;

#[test]
fn server_metadata_is_unavailable_without_a_compiled_server_identity() {
    assert!(matches!(
        metadata::encode(
            vec![1],
            spaceterm_distribution::StorageCompatibility {
                read: vec![1],
                write: 1,
                migrate_from: vec![1]
            }
        ),
        Err(spaceterm_distribution::DistributionFailure::IdentityMismatch)
    ));
}
