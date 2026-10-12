use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::*;
use std::io::Write;

pub(super) const CONTENT: &[u8] = b"authenticated fixture bytes, not executable";

pub(super) fn candidate(platform: ServerPlatform) -> VerifiedServerArchive {
    candidate_fault(platform, ArchiveFault::None)
}
#[derive(Clone, Copy)]
pub(super) enum ArchiveFault {
    None,
    Link,
    HardLink,
    Fifo,
    Traversal,
    Absolute,
    Unexpected,
    Mode,
    OversizeManifest,
    Missing,
    Extension(tar::EntryType, usize),
    ManifestLongName,
    ManifestOldLongName,
}
pub(super) fn candidate_fault(
    platform: ServerPlatform,
    fault: ArchiveFault,
) -> VerifiedServerArchive {
    candidate_contents(platform, fault, CONTENT)
}
pub(super) fn candidate_contents(
    platform: ServerPlatform,
    fault: ArchiveFault,
    contents: &[u8],
) -> VerifiedServerArchive {
    let server = match platform {
        ServerPlatform::LinuxX86_64 => "bin/spaceterm-server",
        ServerPlatform::DarwinArm64 => "SpaceTerm Server.app/Contents/MacOS/spaceterm-server",
    };
    let mut names = vec![
        server,
        "libexec/spaceterm-server-launcher",
        "libexec/spaceterm-server-manager",
        "share/sealed.conf",
        "lib/interpreter",
        "lib/libfixture.so",
        "providers/fixture.so",
        "share/native-evidence/fixture-target",
    ];
    if platform == ServerPlatform::LinuxX86_64 {
        names.push("libexec/credential-tools/systemd-creds");
    }
    names.sort_unstable();
    let manifest = InstallationManifestV1 {
        schema_version: 1,
        component: ServerComponent::Server,
        build_identity: BuildChannel::Release,
        platform,
        version: BuildVersion::parse("1.2.3").unwrap(),
        build_commit: [5; 20],
        protocol_majors: vec![1, 2],
        storage: StorageCompatibility {
            read: vec![1],
            write: 1,
            migrate_from: vec![],
        },
        native_identifier: BuildChannel::Release.native_identifier().into(),
        launcher_abi: 2,
        entry_points: ServerEntryPoints {
            server: server.into(),
            launcher: "libexec/spaceterm-server-launcher".into(),
            manager: "libexec/spaceterm-server-manager".into(),
            credential_tool: (platform == ServerPlatform::LinuxX86_64)
                .then(|| "libexec/credential-tools/systemd-creds".into()),
        },
        files: names
            .iter()
            .map(|name| InstallationFile {
                name: (*name).into(),
                size: contents.len() as u64,
                mode: if *name == server && platform == ServerPlatform::LinuxX86_64 {
                    0o501
                } else if *name == "share/sealed.conf" {
                    0o444
                } else {
                    0o555
                },
                sha256: Sha256::digest(contents).into(),
            })
            .collect(),
        dependencies: vec![],
    };
    let mut bytes = serde_json::to_vec(&manifest).unwrap();
    if matches!(fault, ArchiveFault::OversizeManifest) {
        bytes.resize(1024 * 1024 + 1, b' ');
    }
    let mut tar = tar::Builder::new(Vec::new());
    if let ArchiveFault::Extension(kind, size) = fault {
        let mut header = tar::Header::new_gnu();
        header.set_size(size as u64);
        header.set_mode(0o444);
        header.set_entry_type(kind);
        header.set_cksum();
        tar.append_data(&mut header, "././@LongLink", vec![0; size].as_slice())
            .unwrap();
    } else if matches!(
        fault,
        ArchiveFault::ManifestLongName | ArchiveFault::ManifestOldLongName
    ) {
        let name = b"spaceterm-server/installation.json\0";
        let mut header = if matches!(fault, ArchiveFault::ManifestOldLongName) {
            tar::Header::new_old()
        } else {
            tar::Header::new_gnu()
        };
        header.set_size(name.len() as u64);
        header.set_mode(0o444);
        header.set_entry_type(tar::EntryType::GNULongName);
        header.set_cksum();
        tar.append_data(&mut header, "././@LongLink", name.as_slice())
            .unwrap();
    }
    for (name, mut mode, contents) in
        std::iter::once(("installation.json", 0o444, bytes.as_slice())).chain(
            manifest
                .files
                .iter()
                .map(|f| (f.name.as_str(), f.mode, contents)),
        )
    {
        if matches!(fault, ArchiveFault::Missing) && name == "share/sealed.conf" {
            continue;
        }
        if matches!(fault, ArchiveFault::Mode) && name == server {
            mode = 0o400;
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        // The extension must supply the manifest name; repeating it in the next
        // header would hide a disagreement between discovery and copying.
        let path = if name == "installation.json"
            && matches!(
                fault,
                ArchiveFault::ManifestLongName | ArchiveFault::ManifestOldLongName
            ) {
            "spaceterm-server/short-manifest".into()
        } else {
            format!("spaceterm-server/{name}")
        };
        tar.append_data(&mut header, path, contents).unwrap();
    }
    if matches!(
        fault,
        ArchiveFault::Link
            | ArchiveFault::HardLink
            | ArchiveFault::Fifo
            | ArchiveFault::Traversal
            | ArchiveFault::Absolute
            | ArchiveFault::Unexpected
    ) {
        let mut header = tar::Header::new_gnu();
        let name = match fault {
            ArchiveFault::Traversal => "spaceterm-server/../outside",
            ArchiveFault::Absolute => "/outside",
            _ => "spaceterm-server/unexpected",
        };
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_size(0);
        header.set_mode(0o444);
        header.set_uid(0);
        header.set_gid(0);
        match fault {
            ArchiveFault::Link => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_link_name("share/sealed.conf").unwrap();
            }
            ArchiveFault::HardLink => {
                header.set_entry_type(tar::EntryType::Link);
                header.set_link_name("share/sealed.conf").unwrap();
            }
            ArchiveFault::Fifo => header.set_entry_type(tar::EntryType::Fifo),
            _ => header.set_entry_type(tar::EntryType::Regular),
        }
        header.set_cksum();
        tar.append(&header, &[][..]).unwrap();
    }
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&tar.into_inner().unwrap()).unwrap();
    let archive = gzip.finish().unwrap();
    // This deterministic distribution test key is not a native signing identity or a credential.
    let signer = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let key = UpdateKey::from_base64(&STANDARD.encode(signer.public_key().as_ref())).unwrap();
    let feed = ServerFeedV1 {
        schema_version: 1,
        component: ServerComponent::Server,
        build_identity: BuildChannel::Release,
        platform,
        version: StableVersion::parse("1.2.3").unwrap(),
        published_at: 1,
        protocol_majors: vec![1, 2],
        storage: manifest.storage,
        installation: InstallationCompatibility {
            manifest_schema: 1,
            registration_schema: 2,
            journal_schema: 1,
            launcher_abi: 2,
        },
        archive: ServerArchiveV1 {
            name: platform.archive_name("1.2.3"),
            size: archive.len() as u64,
            sha256: Sha256::digest(&archive).into(),
            signature: signer.sign(&archive).as_ref().try_into().unwrap(),
            manifest_sha256: Sha256::digest(&bytes).into(),
        },
    };
    let feed_bytes = serde_json::to_vec(&feed).unwrap();
    let verified_feed = verify_server_feed(
        &key,
        platform,
        &feed_bytes,
        STANDARD
            .encode(signer.sign(&feed_bytes).as_ref())
            .as_bytes(),
        1,
    )
    .unwrap();
    ServerArchiveVerifier::new()
        .verify_owned(&key, &verified_feed, archive)
        .unwrap()
}
