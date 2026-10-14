use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::*;

const NOW: u64 = 1_800_000_000;

mod support {
    pub mod account_artifact;
}
fn artifact_fixture(
    build: BuildChannel,
    platform: ServerPlatform,
) -> (UpdateKey, Vec<u8>, Vec<u8>, Vec<u8>) {
    support::account_artifact::artifact_fixture_with_payload(
        build,
        platform,
        b"signed server bytes",
        NOW,
    )
}

struct ObservedAllocator;
thread_local! {
    static LARGEST_ALLOCATION: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
fn observe_allocation(size: usize) {
    let _ = LARGEST_ALLOCATION.try_with(|largest| {
        if let Some(previous) = largest.get() {
            largest.set(Some(previous.max(size)));
        }
    });
}
// SAFETY: every allocation is delegated unchanged to System; observation allocates nothing.
unsafe impl std::alloc::GlobalAlloc for ObservedAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        observe_allocation(layout.size());
        // SAFETY: the caller supplies the allocator's required valid layout.
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: the caller returns an allocation made by this same System allocator.
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        observe_allocation(size);
        // SAFETY: the caller supplies the existing System allocation and valid new size.
        unsafe { std::alloc::System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: ObservedAllocator = ObservedAllocator;

#[test]
fn signed_oversized_tar_metadata_is_rejected_without_allocating_its_body() {
    use std::io::{Read, Write};
    let signer = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let (key, bytes, _, archive) =
        artifact_fixture(BuildChannel::Development, ServerPlatform::LinuxX86_64);
    let original: AccountServerFeedV2 = serde_json::from_slice(&bytes).unwrap();
    let mut original_tar = Vec::new();
    flate2::read::GzDecoder::new(archive.as_slice())
        .read_to_end(&mut original_tar)
        .unwrap();
    for kind in *b"Lxg" {
        let mut header = tar::Header::new_gnu();
        header.set_path("metadata").unwrap();
        header.set_mode(0o644);
        header.set_size(8 * 1024 * 1024);
        header.set_entry_type(tar::EntryType::new(kind));
        header.set_cksum();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(header.as_bytes()).unwrap();
        for _ in 0..128 {
            gzip.write_all(&[0; 64 * 1024]).unwrap();
        }
        gzip.write_all(&original_tar).unwrap();
        let archive = gzip.finish().unwrap();
        let mut feed = original.clone();
        feed.archive.size = archive.len() as u64;
        feed.archive.sha256 = Sha256::digest(&archive).into();
        feed.archive.signature = signer.sign(&archive).as_ref().try_into().unwrap();
        let bytes = serde_json::to_vec(&feed).unwrap();
        let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
        let feed = verify_account_server_feed(
            &key,
            BuildChannel::Development,
            ServerPlatform::LinuxX86_64,
            &bytes,
            signature.as_bytes(),
            NOW,
        )
        .unwrap();
        let verifier = ServerArchiveVerifier::new();
        LARGEST_ALLOCATION.with(|largest| largest.set(Some(0)));
        let result = verifier.verify_account_owned(&key, feed, archive);
        let largest = LARGEST_ALLOCATION.with(|largest| largest.replace(None).unwrap());
        assert_eq!(result.err(), Some(DistributionFailure::ArchiveRejected));
        assert!(
            largest <= 2 * manifest::MAX_MANIFEST_BYTES,
            "tar metadata type {kind} allocated {largest} bytes before rejection"
        );
    }
}

#[test]
fn exact_signed_nonrelease_archives_are_validated_with_the_existing_ed25519_algorithm() {
    for build in [
        BuildChannel::Release,
        BuildChannel::Preflight,
        BuildChannel::Development,
    ] {
        for platform in [ServerPlatform::DarwinArm64, ServerPlatform::LinuxX86_64] {
            let (key, bytes, signature, archive) = artifact_fixture(build, platform);
            let feed =
                verify_account_server_feed(&key, build, platform, &bytes, &signature, NOW).unwrap();
            let verified = ServerArchiveVerifier::new()
                .verify_account_owned(&key, feed, archive)
                .unwrap();
            assert_eq!(verified.manifest().build_identity, build);
            assert_eq!(verified.manifest().platform, platform);
        }
    }
}

#[test]
fn wrong_build_target_and_signatures_are_rejected_before_installation() {
    for build in [
        BuildChannel::Release,
        BuildChannel::Preflight,
        BuildChannel::Development,
    ] {
        let (key, bytes, signature, archive) = artifact_fixture(build, ServerPlatform::LinuxX86_64);
        for other in [
            BuildChannel::Release,
            BuildChannel::Preflight,
            BuildChannel::Development,
        ]
        .into_iter()
        .filter(|b| *b != build)
        {
            assert_eq!(
                verify_account_server_feed(
                    &key,
                    other,
                    ServerPlatform::LinuxX86_64,
                    &bytes,
                    &signature,
                    NOW
                )
                .err(),
                Some(DistributionFailure::IdentityMismatch)
            );
        }
        assert_eq!(
            verify_account_server_feed(
                &key,
                build,
                ServerPlatform::DarwinArm64,
                &bytes,
                &signature,
                NOW
            )
            .err(),
            Some(DistributionFailure::IdentityMismatch)
        );
        let mut tampered = bytes.clone();
        tampered.push(b' ');
        assert_eq!(
            verify_account_server_feed(
                &key,
                build,
                ServerPlatform::LinuxX86_64,
                &tampered,
                &signature,
                NOW
            )
            .err(),
            Some(DistributionFailure::Verification)
        );
        let feed = verify_account_server_feed(
            &key,
            build,
            ServerPlatform::LinuxX86_64,
            &bytes,
            &signature,
            NOW,
        )
        .unwrap();
        let mut tampered = archive;
        tampered[0] ^= 1;
        assert_eq!(
            ServerArchiveVerifier::new()
                .verify_account_owned(&key, feed, tampered)
                .err(),
            Some(DistributionFailure::ArchiveRejected)
        );
    }
}

#[test]
fn alternate_signer_cannot_rebind_an_authenticated_feed() {
    let (key, bytes, signature, archive) =
        artifact_fixture(BuildChannel::Development, ServerPlatform::LinuxX86_64);
    let other = Ed25519KeyPair::from_seed_unchecked(&[8; 32]).unwrap();
    let other = UpdateKey::from_base64(&STANDARD.encode(other.public_key().as_ref())).unwrap();
    assert_eq!(
        verify_account_server_feed(
            &other,
            BuildChannel::Development,
            ServerPlatform::LinuxX86_64,
            &bytes,
            &signature,
            NOW
        )
        .err(),
        Some(DistributionFailure::Verification)
    );
    let feed = verify_account_server_feed(
        &key,
        BuildChannel::Development,
        ServerPlatform::LinuxX86_64,
        &bytes,
        &signature,
        NOW,
    )
    .unwrap();
    assert_eq!(
        ServerArchiveVerifier::new()
            .verify_account_owned(&other, feed, archive)
            .err(),
        Some(DistributionFailure::Verification)
    );
}

#[test]
fn account_manifest_rejects_dropped_helpers_unsafe_layout_and_wrong_commit() {
    let (key, bytes, signature, archive) =
        artifact_fixture(BuildChannel::Development, ServerPlatform::LinuxX86_64);
    let feed = verify_account_server_feed(
        &key,
        BuildChannel::Development,
        ServerPlatform::LinuxX86_64,
        &bytes,
        &signature,
        NOW,
    )
    .unwrap();
    let verified = ServerArchiveVerifier::new()
        .verify_account_owned(&key, feed, archive)
        .unwrap();
    for case in [
        "helper",
        "mode",
        "path",
        "commit",
        "identifier",
        "duplicate",
        "prefix",
        "resources",
    ] {
        let mut manifest = verified.manifest().clone();
        match case {
            "resources" => manifest.files.retain(|f| !f.name.contains("terminfo/")),
            "helper" => manifest.files[0].name = "libexec/spaceterm-server-launcher".into(),
            "mode" => manifest.files[0].mode = 0o4755,
            "path" => manifest.files[0].name = "bin/../spaceterm-server".into(),
            "commit" => manifest.build_commit = [2; 20],
            "identifier" => {
                manifest.native_identifier = BuildChannel::Release.native_identifier().into()
            }
            "duplicate" => manifest.files.push(manifest.files[0].clone()),
            "prefix" => {
                let mut file = manifest.files[0].clone();
                file.name = "bin".into();
                manifest.files.insert(0, file);
            }
            _ => unreachable!(),
        }
        assert!(manifest.validate().is_err(), "{case}");
    }
}

#[test]
fn a_signed_archive_cannot_substitute_payload_bytes_or_traversal_paths() {
    use std::io::{Read, Write};
    let signer = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let (key, bytes, _, archive) =
        artifact_fixture(BuildChannel::Development, ServerPlatform::LinuxX86_64);
    let original: AccountServerFeedV2 = serde_json::from_slice(&bytes).unwrap();
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(archive.as_slice())
        .read_to_end(&mut tar)
        .unwrap();
    for traversal in [false, true] {
        let mut tar = tar.clone();
        let payload = tar
            .windows(b"signed server bytes".len())
            .position(|v| v == b"signed server bytes")
            .unwrap();
        if traversal {
            let header = &mut tar[payload - 512..payload];
            header[..100].fill(0);
            let name = b"spaceterm-server/../outside";
            header[..name.len()].copy_from_slice(name);
            let mut parsed = tar::Header::new_ustar();
            parsed.as_mut_bytes().copy_from_slice(header);
            parsed.set_cksum();
            header.copy_from_slice(parsed.as_bytes());
        } else {
            tar[payload] ^= 1;
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&tar).unwrap();
        let archive = gzip.finish().unwrap();
        let mut feed = original.clone();
        feed.archive.size = archive.len() as u64;
        feed.archive.sha256 = Sha256::digest(&archive).into();
        feed.archive.signature = signer.sign(&archive).as_ref().try_into().unwrap();
        let bytes = serde_json::to_vec(&feed).unwrap();
        let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
        let feed = verify_account_server_feed(
            &key,
            BuildChannel::Development,
            ServerPlatform::LinuxX86_64,
            &bytes,
            signature.as_bytes(),
            NOW,
        )
        .unwrap();
        assert_eq!(
            ServerArchiveVerifier::new()
                .verify_account_owned(&key, feed, archive)
                .err(),
            Some(DistributionFailure::ArchiveRejected)
        );
    }
}

#[test]
fn account_metadata_requires_schema_two_and_rejects_the_removed_launcher_abi() {
    use std::io::Read as _;
    let signer = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let (key, bytes, signature, archive) =
        artifact_fixture(BuildChannel::Release, ServerPlatform::LinuxX86_64);
    let document: AccountServerFeedV2 = serde_json::from_slice(&bytes).unwrap();
    let feed = verify_account_server_feed(
        &key,
        BuildChannel::Release,
        ServerPlatform::LinuxX86_64,
        &bytes,
        &signature,
        NOW,
    )
    .unwrap();
    let decoder = flate2::read::GzDecoder::new(archive.as_slice());
    let mut archive = tar::Archive::new(decoder);
    let mut manifest_bytes = Vec::new();
    archive
        .entries()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .read_to_end(&mut manifest_bytes)
        .unwrap();
    let manifest = verify_account_installation_manifest(&feed, &manifest_bytes).unwrap();
    assert_eq!(manifest.schema_version, 2);
    assert!(
        !String::from_utf8(bytes.clone())
            .unwrap()
            .contains("launcher_abi")
    );
    assert!(
        !String::from_utf8(manifest_bytes.clone())
            .unwrap()
            .contains("launcher_abi")
    );
    for schema in [1, 3] {
        let mut wrong = document.clone();
        wrong.schema_version = schema;
        let bytes = serde_json::to_vec(&wrong).unwrap();
        let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
        assert_eq!(
            verify_account_server_feed(
                &key,
                BuildChannel::Release,
                ServerPlatform::LinuxX86_64,
                &bytes,
                signature.as_bytes(),
                NOW
            )
            .err(),
            Some(DistributionFailure::UnsupportedSchema)
        );
    }
    for schema in [1, 2, 3] {
        let mut value: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
        value["schema_version"] = schema.into();
        if schema == 2 {
            value["launcher_abi"] = 2.into();
        }
        let wrong_manifest = serde_json::to_vec(&value).unwrap();
        let mut wrong = document.clone();
        wrong.archive.manifest_sha256 = Sha256::digest(&wrong_manifest).into();
        let bytes = serde_json::to_vec(&wrong).unwrap();
        let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
        let feed = verify_account_server_feed(
            &key,
            BuildChannel::Release,
            ServerPlatform::LinuxX86_64,
            &bytes,
            signature.as_bytes(),
            NOW,
        )
        .unwrap();
        assert!(verify_account_installation_manifest(&feed, &wrong_manifest).is_err());
    }
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["installation"]["launcher_abi"] = 2.into();
    let bytes = serde_json::to_vec(&value).unwrap();
    let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
    assert_eq!(
        verify_account_server_feed(
            &key,
            BuildChannel::Release,
            ServerPlatform::LinuxX86_64,
            &bytes,
            signature.as_bytes(),
            NOW
        )
        .err(),
        Some(DistributionFailure::InvalidRecord)
    );
}

#[test]
fn python_account_packaging_metadata_is_accepted_by_rust_verification() {
    use std::io::Read as _;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mise-tasks/lib");
    let output = std::process::Command::new("python3").args(["-I", "-c", r#"
import json, sys
sys.path.insert(0, sys.argv[1])
from spaceterm_tasks.server_artifacts import archive_name, entry_points
print(json.dumps([
    {"build": build, "platform": platform, "entry_points": entry_points(platform, build),
     "archive_name": archive_name("1.2.3" if build == "release" else "dev.010101010101", platform, build)}
    for build in ("release", "preflight", "development")
    for platform in ("darwin-arm64", "linux-x86_64")
]))
"#]).arg(root).output().unwrap();
    assert!(output.status.success(), "Python packager fixture failed");
    let documents: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    let signer = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    for document in documents {
        let build: BuildChannel = serde_json::from_value(document["build"].clone()).unwrap();
        let platform: ServerPlatform =
            serde_json::from_value(document["platform"].clone()).unwrap();
        let (key, bytes, _, archive) = artifact_fixture(build, platform);
        let decoder = flate2::read::GzDecoder::new(archive.as_slice());
        let mut archive = tar::Archive::new(decoder);
        let mut manifest_bytes = Vec::new();
        archive
            .entries()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .read_to_end(&mut manifest_bytes)
            .unwrap();
        let mut manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
        manifest["entry_points"] = document["entry_points"].clone();
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let mut feed: AccountServerFeedV2 = serde_json::from_slice(&bytes).unwrap();
        feed.archive.name = document["archive_name"].as_str().unwrap().into();
        feed.archive.manifest_sha256 = Sha256::digest(&manifest_bytes).into();
        let bytes = serde_json::to_vec(&feed).unwrap();
        let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
        let feed =
            verify_account_server_feed(&key, build, platform, &bytes, signature.as_bytes(), NOW)
                .unwrap();
        let manifest = verify_account_installation_manifest(&feed, &manifest_bytes).unwrap();
        assert_eq!(manifest.schema_version, 2);
    }
}

#[test]
fn optional_product_cli_is_bounded_and_earlier_account_manifests_keep_reading() {
    let (key, bytes, signature, archive) =
        artifact_fixture(BuildChannel::Development, ServerPlatform::LinuxX86_64);
    let feed = verify_account_server_feed(
        &key,
        BuildChannel::Development,
        ServerPlatform::LinuxX86_64,
        &bytes,
        &signature,
        NOW,
    )
    .unwrap();
    let artifact = ServerArchiveVerifier::new()
        .verify_account_owned(&key, feed, archive)
        .unwrap();
    let mut manifest = artifact.manifest().clone();
    let mut earlier = serde_json::to_value(&manifest).unwrap();
    earlier["entry_points"]
        .as_object_mut()
        .unwrap()
        .remove("cli");
    let earlier: AccountInstallationManifestV2 = serde_json::from_value(earlier).unwrap();
    earlier.validate().unwrap();
    assert!(earlier.entry_points.cli.is_none());
    manifest.entry_points.cli = Some("bin/spaceterm-cli".into());
    assert!(manifest.validate().is_err());
    manifest.files.push(InstallationFile {
        name: "bin/spaceterm-cli".into(),
        mode: 0o700,
        size: 1,
        sha256: Sha256::digest(b"x").into(),
    });
    manifest.files.sort_by(|a, b| a.name.cmp(&b.name));
    manifest.validate().unwrap();
    manifest.entry_points.cli = Some("bin/other".into());
    assert!(manifest.validate().is_err());
}

#[test]
fn retained_nonrelease_feed_names_remain_bound_to_the_exact_signed_build() {
    let signer = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let (key, bytes, _, _) =
        artifact_fixture(BuildChannel::Development, ServerPlatform::LinuxX86_64);
    let mut document: AccountServerFeedV2 = serde_json::from_slice(&bytes).unwrap();
    document.archive.name = ServerPlatform::LinuxX86_64.archive_name(document.version.as_str());
    let bytes = serde_json::to_vec(&document).unwrap();
    let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
    verify_account_server_feed(
        &key,
        BuildChannel::Development,
        ServerPlatform::LinuxX86_64,
        &bytes,
        signature.as_bytes(),
        NOW,
    )
    .unwrap();
    assert_eq!(
        verify_account_server_feed(
            &key,
            BuildChannel::Preflight,
            ServerPlatform::LinuxX86_64,
            &bytes,
            signature.as_bytes(),
            NOW
        )
        .err(),
        Some(DistributionFailure::IdentityMismatch)
    );
}

/// The packaging task (mise-tasks/lib/spaceterm_tasks/release_server.py) writes
/// exactly these descriptor fields. The strict Rust schema must accept every one
/// of them, or locally packaged signed servers are refused at install time.
#[test]
fn account_feed_schema_matches_the_packaging_producer_shape() {
    let (_, feed, _, _) = artifact_fixture(BuildChannel::Preflight, ServerPlatform::DarwinArm64);
    let document: serde_json::Value = serde_json::from_slice(&feed).unwrap();
    fn keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }
    assert_eq!(
        keys(&document),
        [
            "archive",
            "build_commit",
            "build_identity",
            "component",
            "installation",
            "platform",
            "protocol_majors",
            "published_at",
            "schema_version",
            "storage",
            "version",
        ]
    );
    assert_eq!(
        keys(&document["installation"]),
        ["journal_schema", "manifest_schema", "registration_schema"]
    );
    assert_eq!(
        keys(&document["archive"]),
        ["manifest_sha256", "name", "sha256", "signature", "size"]
    );
    assert_eq!(document["build_commit"], serde_json::json!("01".repeat(20)));
}
