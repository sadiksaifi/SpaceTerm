use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::*;

pub fn fixture(
    version: &str,
    seed: u8,
    machine: u32,
) -> (UpdateKey, VerifiedAccountServerArtifact) {
    let platform = if cfg!(target_os = "macos") {
        ServerPlatform::DarwinArm64
    } else {
        ServerPlatform::LinuxX86_64
    };
    let mut payload = vec![0; 64];
    if platform == ServerPlatform::DarwinArm64 {
        payload[..4].copy_from_slice(&0xfeedfacf_u32.to_le_bytes());
        payload[4..8].copy_from_slice(&machine.to_le_bytes());
        payload[12..16].copy_from_slice(&2_u32.to_le_bytes());
    } else {
        payload[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        payload[16..18].copy_from_slice(&2_u16.to_le_bytes());
        payload[18..20].copy_from_slice(&(machine as u16).to_le_bytes());
    }
    payload[63] = version.as_bytes()[version.len() - 1];
    fixture_with_binary(version, seed, &payload)
}

/// Signs the supplied executable bytes with the same authenticated archive and
/// installation manifest used by the custody fixtures.
pub fn fixture_with_binary(
    version: &str,
    seed: u8,
    payload: &[u8],
) -> (UpdateKey, VerifiedAccountServerArtifact) {
    fixture_with_binary_and_commit(version, seed, payload, [1; 20])
}

pub fn fixture_with_binary_and_commit(
    version: &str,
    seed: u8,
    payload: &[u8],
    build_commit: [u8; 20],
) -> (UpdateKey, VerifiedAccountServerArtifact) {
    let build = BuildChannel::Development;
    let platform = if cfg!(target_os = "macos") {
        ServerPlatform::DarwinArm64
    } else {
        ServerPlatform::LinuxX86_64
    };
    let signer = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap();
    let key = UpdateKey::from_base64(&STANDARD.encode(signer.public_key().as_ref())).unwrap();
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
        build_commit,
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
    let mut tar = tar::Builder::new(Vec::new());
    for (name, mode, bytes) in
        std::iter::once(("installation.json".to_owned(), 0o644, manifest.as_slice())).chain(files)
    {
        let mut header = tar::Header::new_ustar();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append_data(&mut header, format!("spaceterm-server/{name}"), bytes)
            .unwrap();
    }
    let tar = tar.into_inner().unwrap();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gzip, &tar).unwrap();
    let archive = gzip.finish().unwrap();
    let feed = AccountServerFeedV2 {
        schema_version: 2,
        component: ServerComponent::Server,
        build_identity: build,
        platform,
        version: BuildVersion::parse(version).unwrap(),
        build_commit,
        published_at: 1,
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
    let bytes = serde_json::to_vec(&feed).unwrap();
    let signature = STANDARD.encode(signer.sign(&bytes).as_ref());
    let feed =
        verify_account_server_feed(&key, build, platform, &bytes, signature.as_bytes(), 1).unwrap();
    let artifact = ServerArchiveVerifier::new()
        .verify_account_owned(&key, feed, archive)
        .unwrap();
    (key, artifact)
}
pub fn native_machine() -> u32 {
    if cfg!(target_os = "macos") {
        0x0100000c
    } else {
        62
    }
}
