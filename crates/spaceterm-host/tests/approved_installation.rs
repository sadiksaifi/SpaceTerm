use sha2::{Digest, Sha256};
use spaceterm_host::{
    approved_installation::ApprovedInstallationDescriptor,
    native_deployment::contracts::NativeFailure,
    startup::{AccountBuildKey, BuildIdentity, InstallationId},
};
use std::num::NonZeroU64;

fn descriptor() -> ApprovedInstallationDescriptor {
    ApprovedInstallationDescriptor {
        schema_version: 1,
        key: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        installation: InstallationId(NonZeroU64::new(u64::MAX - 1).unwrap()),
        server_id: [1; 16],
        account_binding_sha256: [2; 32],
        registration_profile_sha256: [3; 32],
        publication_nonce: [4; 32],
        archive_sha256: [5; 32],
        manifest_sha256: [6; 32],
        server_artifact_sha256: [7; 32],
        execution_closure_sha256: [8; 32],
        launcher_abi: 2,
    }
}

#[test]
fn installation_binding_covers_full_descriptor_without_digest_truncation() {
    let d = descriptor();
    let mut bytes = b"SpaceTerm approved installation binding v1\0".to_vec();
    bytes.push(3);
    bytes.extend(501u32.to_le_bytes());
    bytes.extend((u64::MAX - 1).to_le_bytes());
    bytes.extend([1; 16]);
    for b in 2..=8 {
        bytes.extend([b; 32]);
    }
    bytes.extend(2u16.to_le_bytes());
    let expected: [u8; 32] = Sha256::digest(bytes).into();
    assert_eq!(d.installation_binding().unwrap(), expected);
    let baseline = d.installation_binding().unwrap();
    for field in 0..10 {
        let mut changed = d.clone();
        match field {
            0 => changed.key.account_uid += 1,
            1 => changed.key.build_identity = BuildIdentity::Release,
            2 => changed.installation = InstallationId(NonZeroU64::new(1).unwrap()),
            3 => changed.server_id[15] ^= 1,
            4 => changed.account_binding_sha256[31] ^= 1,
            5 => changed.registration_profile_sha256[31] ^= 1,
            6 => changed.publication_nonce[31] ^= 1,
            7 => changed.archive_sha256[31] ^= 1,
            8 => changed.manifest_sha256[31] ^= 1,
            9 => changed.execution_closure_sha256[31] ^= 1,
            _ => unreachable!(),
        }
        assert_ne!(changed.installation_binding().unwrap(), baseline);
    }
    let mut changed = d.clone();
    changed.server_artifact_sha256[31] ^= 1;
    assert_ne!(changed.installation_binding().unwrap(), baseline);
}

#[test]
fn descriptor_rejects_duplicate_unknown_missing_noncanonical_and_trailing_json() {
    let d = descriptor();
    let bytes = d.encode().unwrap();
    assert_eq!(ApprovedInstallationDescriptor::decode(&bytes).unwrap(), d);
    assert!(String::from_utf8_lossy(&bytes).starts_with("{\"account_binding_sha256\":"));
    let text = String::from_utf8(bytes.clone()).unwrap();
    for rejected in [
        format!(" {text}"),
        format!("{text}\n"),
        format!("{text}{{}}"),
        text.replacen('{', "{\"unknown\":0,", 1),
        text.replacen('{', "{\"schema_version\":1,", 1),
        text.replace("\"schema_version\":1,", ""),
        text.replace("\"launcher_abi\":2", "\"launcher_abi\":2.0"),
        text.replace("\"development\"", "\"Development\""),
    ] {
        assert!(ApprovedInstallationDescriptor::decode(rejected.as_bytes()).is_err());
    }
    assert_eq!(
        ApprovedInstallationDescriptor::decode(&vec![b' '; 16385]),
        Err(NativeFailure::LimitExceeded)
    );
}

#[test]
fn exact_descriptor_authentication_rejects_substituted_artifact_and_resources() {
    let d = descriptor();
    let bytes = d.encode().unwrap();
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    assert_eq!(
        ApprovedInstallationDescriptor::decode_authenticated(&bytes, &hash).unwrap(),
        d
    );
    for field in 0..2 {
        let mut other = d.clone();
        if field == 0 {
            other.server_artifact_sha256[0] ^= 1;
        } else {
            other.execution_closure_sha256[0] ^= 1;
        }
        assert_eq!(
            ApprovedInstallationDescriptor::decode_authenticated(&other.encode().unwrap(), &hash),
            Err(NativeFailure::ArtifactRejected)
        );
    }
}

#[test]
fn descriptor_rejects_zero_bindings_and_unsupported_abi() {
    let mut d = descriptor();
    d.publication_nonce = [0; 32];
    assert_eq!(d.encode(), Err(NativeFailure::InvalidRecord));
    let mut d = descriptor();
    d.launcher_abi = 1;
    assert_eq!(d.encode(), Err(NativeFailure::UnsupportedSchema));
}
