use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::*;

const NOW: u64 = 1_800_000_000;

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

fn artifact_fixture(
    build: BuildChannel,
    platform: ServerPlatform,
) -> (UpdateKey, Vec<u8>, Vec<u8>, Vec<u8>) {
    let signer = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let key = UpdateKey::from_base64(&STANDARD.encode(signer.public_key().as_ref())).unwrap();
    let version = if build == BuildChannel::Release {
        "1.2.3"
    } else {
        "dev.010101010101"
    };
    let name = match platform {
        ServerPlatform::LinuxX86_64 => "bin/spaceterm-server".to_owned(),
        ServerPlatform::DarwinArm64 => {
            format!("{}/Contents/MacOS/spaceterm-server", build.bundle_name())
        }
    };
    let payload = b"signed server bytes";
    let resource_root = match platform {
        ServerPlatform::LinuxX86_64 => "share/spaceterm-server".to_owned(),
        ServerPlatform::DarwinArm64 => format!("{}/Contents/Resources", build.bundle_name()),
    };
    let mut files = vec![(name.clone(), 0o755, payload.as_slice())];
    for resource in [
        "shell-integration/VERSION",
        "shell-integration/bash/spaceterm.bash",
        "shell-integration/sh/spaceterm.sh",
        "shell-integration/zsh/.zshenv",
        "shell-integration/zsh/spaceterm-integration",
        "terminfo/x/xterm-spaceterm",
        "THIRD-PARTY-NOTICES.txt",
    ] {
        files.push((
            format!("{resource_root}/{resource}"),
            0o644,
            b"bundled resource".as_slice(),
        ));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let manifest = AccountInstallationManifestV2 {
        schema_version: 2,
        component: ServerComponent::Server,
        build_identity: build,
        platform,
        version: BuildVersion::parse(version).unwrap(),
        build_commit: [1; 20],
        protocol_majors: vec![1, 2],
        storage: StorageCompatibility {
            read: vec![1],
            write: 1,
            migrate_from: vec![],
        },
        native_identifier: build.native_identifier().into(),
        entry_points: AccountEntryPoints {
            server: name.clone(),
        },
        files: files
            .iter()
            .map(|(name, mode, bytes)| InstallationFile {
                name: name.clone(),
                mode: *mode,
                size: bytes.len() as u64,
                sha256: Sha256::digest(bytes).into(),
            })
            .collect(),
        dependencies: vec![],
    };
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    for (name, mode, bytes) in
        std::iter::once(("installation.json".to_owned(), 0o644, manifest.as_slice())).chain(files)
    {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("spaceterm-server/{name}"), bytes)
            .unwrap();
    }
    let tar = builder.into_inner().unwrap();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gzip, &tar).unwrap();
    let archive = gzip.finish().unwrap();
    let feed = AccountServerFeedV2 {
        schema_version: 2,
        component: ServerComponent::Server,
        build_identity: build,
        platform,
        version: BuildVersion::parse(version).unwrap(),
        published_at: NOW,
        protocol_majors: vec![1, 2],
        storage: StorageCompatibility {
            read: vec![1],
            write: 1,
            migrate_from: vec![],
        },
        installation: AccountInstallationCompatibility {
            manifest_schema: 2,
            registration_schema: 1,
            journal_schema: 1,
        },
        archive: ServerArchiveV1 {
            name: platform.archive_name(version),
            size: archive.len() as u64,
            sha256: Sha256::digest(&archive).into(),
            signature: signer.sign(&archive).as_ref().try_into().unwrap(),
            manifest_sha256: Sha256::digest(&manifest).into(),
        },
    };
    let feed = serde_json::to_vec(&feed).unwrap();
    let signature = STANDARD.encode(signer.sign(&feed).as_ref()).into_bytes();
    (key, feed, signature, archive)
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
