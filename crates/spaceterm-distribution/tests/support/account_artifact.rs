//! Shared signed account-archive fixtures. Never compiled in production.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::*;

pub fn artifact_fixture_with_payload(
    build: BuildChannel,
    platform: ServerPlatform,
    payload: &[u8],
    published_at: u64,
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
    let resource_root = match platform {
        ServerPlatform::LinuxX86_64 => "share/spaceterm-server".to_owned(),
        ServerPlatform::DarwinArm64 => format!("{}/Contents/Resources", build.bundle_name()),
    };
    let mut files = vec![(name.clone(), 0o755, payload)];
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
            cli: None,
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
        build_commit: [1; 20],
        published_at,
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
            name: platform.account_archive_name(build, version),
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
