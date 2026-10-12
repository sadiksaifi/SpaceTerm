use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::*;

const NOW: u64 = 1_800_000_000;
struct Signer(Ed25519KeyPair);
impl Signer {
    fn new() -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap())
    }
    fn key(&self) -> UpdateKey {
        UpdateKey::from_base64(&STANDARD.encode(self.0.public_key().as_ref())).unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Vec<u8> {
        format!("{}\n", STANDARD.encode(self.0.sign(bytes).as_ref())).into_bytes()
    }
    fn feed(&self, archive: &[u8], manifest: &[u8]) -> ServerFeedV1 {
        ServerFeedV1 {
            schema_version: 1,
            component: ServerComponent::Server,
            build_identity: BuildChannel::Release,
            platform: ServerPlatform::LinuxX86_64,
            version: StableVersion::parse("1.2.3").unwrap(),
            published_at: NOW,
            protocol_majors: vec![1, 2],
            storage: StorageCompatibility {
                read: vec![1],
                write: 1,
                migrate_from: vec![1],
            },
            installation: InstallationCompatibility {
                manifest_schema: 1,
                registration_schema: 2,
                journal_schema: 1,
                launcher_abi: 2,
            },
            archive: ServerArchiveV1 {
                name: ServerPlatform::LinuxX86_64.archive_name("1.2.3"),
                size: archive.len() as u64,
                sha256: Sha256::digest(archive).into(),
                signature: self.0.sign(archive).as_ref().try_into().unwrap(),
                manifest_sha256: Sha256::digest(manifest).into(),
            },
        }
    }
    fn verified(&self, document: &ServerFeedV1) -> VerifiedServerFeed {
        let bytes = serde_json::to_vec(document).unwrap();
        verify_server_feed(
            &self.key(),
            document.platform,
            &bytes,
            &self.sign(&bytes),
            NOW,
        )
        .unwrap()
    }
}
#[test]
fn client_feed_bytes_names_bounds_and_urls_are_unchanged() {
    let signer = Signer::new();
    let bytes = b"archive";
    let json = serde_json::json!({ "version":"1.2.3", "published_at": NOW,
        "archive": {"name":"SpaceTerm-1.2.3-linux-x86_64.tar.gz", "size":7, "sha256":format!("{:x}",Sha256::digest(bytes)), "signature": STANDARD.encode(signer.0.sign(bytes).as_ref())} });
    let feed = serde_json::to_vec(&json).unwrap();
    let platform = client_feed::ReleasePlatform::new("linux-x86_64");
    let accepted =
        client_feed::ReleaseFeed::verify(&signer.key(), platform, &feed, &signer.sign(&feed), NOW)
            .unwrap();
    assert_eq!(
        accepted.archive.url,
        "https://github.com/sadiksaifi/SpaceTerm/releases/download/v1.2.3/SpaceTerm-1.2.3-linux-x86_64.tar.gz"
    );
    assert_eq!(accepted.archive.verify(&signer.key(), bytes), Ok(()));
    assert_eq!(
        (
            client_feed::MAX_FEED_BYTES,
            client_feed::MAX_FEED_SIGNATURE_BYTES,
            client_feed::MAX_ARCHIVE_BYTES
        ),
        (4096, 256, 512 * 1024 * 1024)
    );
    assert!(
        verify_server_feed(
            &signer.key(),
            ServerPlatform::LinuxX86_64,
            &feed,
            &signer.sign(&feed),
            NOW
        )
        .is_err()
    );
}
#[test]
fn authenticated_server_feed_accepts_exact_signed_bytes_and_never_downgrades() {
    let signer = Signer::new();
    let feed = signer.verified(&signer.feed(b"archive", b"manifest"));
    assert_eq!(
        feed.archive_url(),
        "https://github.com/sadiksaifi/SpaceTerm/releases/download/v1.2.3/SpaceTerm-Server-1.2.3-linux-x86_64.tar.gz"
    );
    assert!(feed.supersedes("1.2.2"));
    assert!(!feed.supersedes("1.2.4"));
    assert!(!feed.supersedes("dev.abcdefabcdef"));
}
#[test]
fn server_feed_signature_precedes_field_interpretation() {
    let signer = Signer::new();
    assert!(matches!(
        verify_server_feed(
            &signer.key(),
            ServerPlatform::LinuxX86_64,
            b"not JSON",
            &signer.sign(b"different"),
            NOW
        ),
        Err(DistributionFailure::Verification)
    ));
    assert!(matches!(
        verify_server_feed(
            &signer.key(),
            ServerPlatform::LinuxX86_64,
            b"not JSON",
            &signer.sign(b"not JSON"),
            NOW
        ),
        Err(DistributionFailure::InvalidRecord)
    ));
}
#[test]
fn server_feed_rejects_duplicate_unknown_wrong_identity_and_target() {
    let signer = Signer::new();
    let valid = signer.feed(b"archive", b"manifest");
    for change in [
        "schema", "identity", "name", "protocol", "storage", "future",
    ] {
        let mut feed = valid.clone();
        match change {
            "schema" => feed.installation.launcher_abi = 1,
            "identity" => feed.build_identity = BuildChannel::Preflight,
            "name" => feed.archive.name = "SpaceTerm-1.2.3-linux-x86_64.tar.gz".into(),
            "protocol" => feed.protocol_majors = vec![2, 1],
            "storage" => feed.storage.read = vec![1, 1],
            "future" => feed.published_at = NOW + 301,
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&feed).unwrap();
        assert!(
            verify_server_feed(
                &signer.key(),
                ServerPlatform::LinuxX86_64,
                &bytes,
                &signer.sign(&bytes),
                NOW
            )
            .is_err(),
            "{change}"
        );
    }
    let original = serde_json::to_string(&valid).unwrap();
    for bytes in [
        format!("{{\"schema_version\":1,{}", &original[1..]),
        format!("{{\"url\":\"ignored\",{}", &original[1..]),
    ] {
        assert!(
            verify_server_feed(
                &signer.key(),
                ServerPlatform::LinuxX86_64,
                bytes.as_bytes(),
                &signer.sign(bytes.as_bytes()),
                NOW
            )
            .is_err()
        );
    }
    let bytes = original.as_bytes();
    assert!(
        verify_server_feed(
            &signer.key(),
            ServerPlatform::DarwinArm64,
            bytes,
            &signer.sign(bytes),
            NOW
        )
        .is_err()
    );
    assert!(
        client_feed::ReleaseFeed::verify(
            &signer.key(),
            client_feed::ReleasePlatform::new("linux-x86_64"),
            bytes,
            &signer.sign(bytes),
            NOW
        )
        .is_err()
    );
}

fn installation() -> (InstallationManifestV1, Vec<(String, u32, Vec<u8>)>) {
    let files: Vec<_> = [
        ("bin/spaceterm-server", 0o501),
        ("libexec/spaceterm-server-launcher", 0o555),
        ("libexec/spaceterm-server-manager", 0o555),
        ("libexec/credential-tools/systemd-creds", 0o555),
    ]
    .into_iter()
    .map(|(n, m)| (n.to_owned(), m, n.as_bytes().to_vec()))
    .collect();
    let mut listed: Vec<_> = files
        .iter()
        .map(|(name, mode, bytes)| InstallationFile {
            name: name.clone(),
            mode: *mode,
            size: bytes.len() as u64,
            sha256: Sha256::digest(bytes).into(),
        })
        .collect();
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    (
        InstallationManifestV1 {
            schema_version: 1,
            component: ServerComponent::Server,
            build_identity: BuildChannel::Release,
            platform: ServerPlatform::LinuxX86_64,
            version: BuildVersion::parse("1.2.3").unwrap(),
            build_commit: [1; 20],
            protocol_majors: vec![1, 2],
            storage: StorageCompatibility {
                read: vec![1],
                write: 1,
                migrate_from: vec![1],
            },
            native_identifier: BuildChannel::Release.native_identifier().into(),
            launcher_abi: 2,
            entry_points: ServerEntryPoints {
                server: "bin/spaceterm-server".into(),
                launcher: "libexec/spaceterm-server-launcher".into(),
                manager: "libexec/spaceterm-server-manager".into(),
                credential_tool: Some("libexec/credential-tools/systemd-creds".into()),
            },
            files: listed,
            dependencies: vec![],
        },
        files,
    )
}
fn package(
    manifest: &[u8],
    files: &[(String, u32, Vec<u8>)],
    extra: Option<(&str, tar::EntryType)>,
) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |name: &str, mode, bytes: &[u8], kind| {
        let mut h = tar::Header::new_ustar();
        h.set_size(bytes.len() as u64);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        h.set_mode(mode);
        h.set_entry_type(kind);
        h.set_cksum();
        if name.contains("..") || name.starts_with('/') {
            h.as_mut_bytes()[..100].fill(0);
            h.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            h.set_cksum();
            tar.append(&h, bytes).unwrap();
        } else {
            tar.append_data(&mut h, name, bytes).unwrap();
        }
    };
    add(
        "spaceterm-server/installation.json",
        0o444,
        manifest,
        tar::EntryType::Regular,
    );
    for (n, m, b) in files {
        add(
            &format!("spaceterm-server/{n}"),
            *m,
            b,
            tar::EntryType::Regular,
        );
    }
    if let Some((name, kind)) = extra {
        add(name, 0o555, b"", kind);
    }
    let tar = tar.into_inner().unwrap();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gzip, &tar).unwrap();
    gzip.finish().unwrap()
}
#[test]
fn archive_rejects_wrong_size_digest_signature_and_manifest() {
    let signer = Signer::new();
    let (manifest, files) = installation();
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let archive = package(&bytes, &files, None);
    let document = signer.feed(&archive, &bytes);
    let feed = signer.verified(&document);
    let verified = verify_server_archive(&signer.key(), &feed, &archive).unwrap();
    assert_eq!(verify_installation_manifest(&verified, &bytes).err(), None);
    assert!(verify_installation_manifest(&verified, b"other").is_err());
    for field in ["size", "digest", "signature", "manifest"] {
        let mut changed = document.clone();
        match field {
            "size" => changed.archive.size += 1,
            "digest" => changed.archive.sha256[0] ^= 1,
            "signature" => changed.archive.signature[0] ^= 1,
            "manifest" => changed.archive.manifest_sha256[0] ^= 1,
            _ => unreachable!(),
        }
        let feed = signer.verified(&changed);
        match verify_server_archive(&signer.key(), &feed, &archive) {
            Err(_) => assert_ne!(field, "manifest"),
            Ok(verified) => assert!(verify_installation_manifest(&verified, &bytes).is_err()),
        }
    }
}
#[test]
fn extraction_rejects_links_devices_traversal_duplicates_and_extra_files() {
    let signer = Signer::new();
    let (manifest, files) = installation();
    let bytes = serde_json::to_vec(&manifest).unwrap();
    for extra in [
        ("spaceterm-server/extra", tar::EntryType::Regular),
        (
            "spaceterm-server/bin/spaceterm-server",
            tar::EntryType::Regular,
        ),
        ("spaceterm-server/link", tar::EntryType::Symlink),
        ("spaceterm-server/device", tar::EntryType::Char),
        ("spaceterm-server/fifo", tar::EntryType::Fifo),
        ("other-root/file", tar::EntryType::Regular),
        ("spaceterm-server/../escape", tar::EntryType::Regular),
        ("/spaceterm-server/absolute", tar::EntryType::Regular),
    ] {
        let archive = package(&bytes, &files, Some(extra));
        let feed = signer.verified(&signer.feed(&archive, &bytes));
        let archive = verify_server_archive(&signer.key(), &feed, &archive).unwrap();
        assert!(
            verify_installation_manifest(&archive, &bytes).is_err(),
            "{extra:?}"
        );
    }
    let mut bad = files.clone();
    bad[0].2.push(0);
    let archive = package(&bytes, &bad, None);
    let feed = signer.verified(&signer.feed(&archive, &bytes));
    let archive = verify_server_archive(&signer.key(), &feed, &archive).unwrap();
    assert!(verify_installation_manifest(&archive, &bytes).is_err());
}
#[test]
fn manifest_rejects_unsafe_paths_modes_and_incompatible_metadata() {
    let signer = Signer::new();
    let (valid, files) = installation();
    for field in [
        "traversal",
        "absolute",
        "backslash",
        "mode",
        "identifier",
        "version",
        "protocol",
        "unknown",
        "duplicate",
    ] {
        let mut manifest = valid.clone();
        match field {
            "traversal" => manifest.files[0].name = "bin/../spaceterm-server".into(),
            "absolute" => manifest.files[0].name = "/bin/server".into(),
            "backslash" => manifest.files[0].name = "bin\\server".into(),
            "mode" => manifest.files[0].mode = 0o4755,
            "identifier" => manifest.native_identifier = "io.github.sadiksaifi.spaceterm".into(),
            "version" => manifest.version = BuildVersion::parse("1.2.4").unwrap(),
            "protocol" => manifest.protocol_majors = vec![1],
            _ => {}
        }
        let mut bytes = serde_json::to_vec(&manifest).unwrap();
        if field == "unknown" || field == "duplicate" {
            bytes = format!(
                "{{\"{}\":1,{}",
                if field == "unknown" {
                    "untrusted"
                } else {
                    "schema_version"
                },
                &std::str::from_utf8(&bytes).unwrap()[1..]
            )
            .into_bytes();
        }
        let archive = package(&bytes, &files, None);
        let feed = signer.verified(&signer.feed(&archive, &bytes));
        let archive = verify_server_archive(&signer.key(), &feed, &archive).unwrap();
        assert!(
            verify_installation_manifest(&archive, &bytes).is_err(),
            "{field}"
        );
    }
}

#[test]
fn archive_verification_controller_retains_only_one_archive_budget() {
    let signer = Signer::new();
    let feed = signer.verified(&signer.feed(b"archive", b"manifest"));
    let controller = ServerArchiveVerifier::new();
    let retained = controller.verify(&signer.key(), &feed, b"archive").unwrap();
    assert!(matches!(
        controller.verify(&signer.key(), &feed, b"archive"),
        Err(DistributionFailure::LimitExceeded)
    ));
    drop(retained);
    assert!(controller.verify(&signer.key(), &feed, b"archive").is_ok());
    assert!(controller.verify(&signer.key(), &feed, b"bad").is_err());
    assert!(controller.verify(&signer.key(), &feed, b"archive").is_ok());
}

#[test]
fn macos_manifest_requires_explicit_null_credential_tool() {
    let signer = Signer::new();
    let (mut manifest, mut files) = installation();
    manifest.platform = ServerPlatform::DarwinArm64;
    manifest.entry_points.credential_tool = None;
    manifest.entry_points.server = "SpaceTerm Server.app/Contents/MacOS/spaceterm-server".into();
    files.retain(|(n, _, _)| n != "libexec/credential-tools/systemd-creds");
    files[0].0 = manifest.entry_points.server.clone();
    files[0].1 = 0o755;
    manifest.files = files
        .iter()
        .map(|(n, m, b)| InstallationFile {
            name: n.clone(),
            mode: *m,
            size: b.len() as u64,
            sha256: Sha256::digest(b).into(),
        })
        .collect();
    manifest.files.sort_by(|a, b| a.name.cmp(&b.name));
    for missing in [false, true] {
        let mut value = serde_json::to_value(&manifest).unwrap();
        if missing {
            value["entry_points"]
                .as_object_mut()
                .unwrap()
                .remove("credential_tool");
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        let archive = package(&bytes, &files, None);
        let mut feed = signer.feed(&archive, &bytes);
        feed.platform = ServerPlatform::DarwinArm64;
        feed.archive.name = feed.platform.archive_name("1.2.3");
        let verified = signer.verified(&feed);
        let archive = verify_server_archive(&signer.key(), &verified, &archive).unwrap();
        assert_eq!(
            verify_installation_manifest(&archive, &bytes).is_ok(),
            !missing
        );
    }
}
