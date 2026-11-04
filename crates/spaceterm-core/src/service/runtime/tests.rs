use super::*;
use std::os::unix::fs::PermissionsExt;
use test_support::{COMMIT, VERSION, target, with_artifact};

#[test]
fn signed_dirty_source_versions_are_admitted_only_for_matching_nonproduction_builds() {
    let root = temporary();
    for identity in ["spaceterm-development", "spaceterm-preflight", "spaceterm"] {
        for (case, version) in [
            ("clean", "dev.aaaaaaaaaaaa"),
            ("dirty", "dev.aaaaaaaaaaaa.dirty"),
            ("other-commit", "dev.bbbbbbbbbbbb.dirty"),
            ("extra-suffix", "dev.aaaaaaaaaaaa.dirty.dirty"),
        ] {
            with_artifact(
                &root.join(identity).join(case),
                identity,
                |archive, signature| {
                    rewrite(archive, signature, |entries| {
                        let mut manifest: serde_json::Value =
                            serde_json::from_slice(&entries[0].1).unwrap();
                        manifest["version"] = version.into();
                        entries[0].1 = serde_json::to_vec(&manifest).unwrap();
                    });
                    if identity == "spaceterm" {
                        test_support::sign_with_production_trust(archive, signature);
                    }
                    let service = Service::for_identity(identity).unwrap();
                    let pin = digest(&std::fs::read(archive).unwrap());
                    let result =
                        service.inspect_candidate(archive, signature, &pin, crate::protocol::MAJOR);
                    if case == "clean" || (identity != "spaceterm" && case == "dirty") {
                        assert_eq!(result.unwrap().version, version);
                    } else {
                        assert_eq!(result, Err(CoreError::RuntimeUnavailable));
                    }
                },
            );
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn nonproduction_signatures_never_authorize_release_even_when_the_manifest_claims_release() {
    let root = temporary();
    let trust: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../packaging/core-nonproduction-trust.json"
    ))
    .unwrap();
    let seed = base64::engine::general_purpose::STANDARD
        .decode(trust["seed"].as_str().unwrap())
        .unwrap();
    let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    for identity in ["spaceterm-preflight", "spaceterm-development", "spaceterm"] {
        let (archive, signature) =
            with_artifact(&root.join(identity), identity, |archive, signature| {
                std::fs::write(
                    signature,
                    pair.sign(&std::fs::read(archive).unwrap()).as_ref(),
                )
                .unwrap();
                (archive.to_owned(), signature.to_owned())
            });
        // Outside fixture trust: use the compiled trust selected by the receiving Service.
        let service = Service::for_identity(identity).unwrap();
        let pin = digest(&std::fs::read(&archive).unwrap());
        let inspect =
            || service.inspect_candidate(&archive, &signature, &pin, crate::protocol::MAJOR);
        if identity == "spaceterm" {
            assert_eq!(inspect(), Err(CoreError::RuntimeUnavailable));
            let parent = root.join("release-installations");
            assert_eq!(
                provision_candidate(
                    &parent,
                    identity,
                    &archive,
                    &signature,
                    &pin,
                    Some(crate::protocol::MAJOR),
                ),
                Err(CoreError::RuntimeUnavailable)
            );
            assert!(!parent.exists());
            // A retained-looking installation cannot turn the public nonproduction key
            // into Release authority, even when its signed manifest claims Release.
            let bytes = std::fs::read(&archive).unwrap();
            let installation = root.join("retained-release").join(&pin);
            let directory = files::store(&installation).unwrap();
            contents(&bytes, identity, |path, is_directory, mode, data| {
                if is_directory {
                    files::create_directories(&directory, path)?;
                } else {
                    files::create_file(&directory, path, data, mode)?;
                }
                Ok(())
            })
            .unwrap();
            let mut buffer = vec![0; CHUNK];
            for (name, bytes) in [
                ("artifact.tar.gz", bytes),
                ("artifact.sig", std::fs::read(&signature).unwrap()),
            ] {
                files::create_file(
                    &directory,
                    Path::new(name),
                    &mut Member::new(&mut bytes.as_slice(), bytes.len() as u64, &mut buffer),
                    0o644,
                )
                .unwrap();
            }
            assert_eq!(
                verify(&installation, identity),
                Err(CoreError::RuntimeUnavailable)
            );
        } else {
            assert_eq!(inspect().unwrap().version, VERSION);
            let other = Service::for_identity(if identity == "spaceterm-preflight" {
                "spaceterm-development"
            } else {
                "spaceterm-preflight"
            })
            .unwrap();
            assert_eq!(
                other.inspect_candidate(&archive, &signature, &pin, crate::protocol::MAJOR,),
                Err(CoreError::RuntimeUnavailable)
            );
            std::fs::write(&signature, [0; 64]).unwrap();
            assert_eq!(inspect(), Err(CoreError::RuntimeUnavailable));
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn remote_candidate_inspection_verifies_the_selected_target_without_executing_it() {
    let root = temporary();
    let remote_target = if target() == "aarch64-apple-darwin" {
        "x86_64-unknown-linux-gnu"
    } else {
        "aarch64-apple-darwin"
    };
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            rewrite(archive, signature, |entries| {
                let mut manifest: serde_json::Value =
                    serde_json::from_slice(&entries[0].1).unwrap();
                manifest["target"] = remote_target.into();
                entries[0].1 = serde_json::to_vec(&manifest).unwrap();
                relayout(entries, target(), remote_target);
                let executable = format!(
                    "core/{}",
                    Layout::for_target(remote_target, "spaceterm-development")
                        .unwrap()
                        .executable()
                        .display()
                );
                entries
                    .iter_mut()
                    .find(|(name, _, _)| *name == executable)
                    .unwrap()
                    .1 = b"must never execute".to_vec();
            });
            let pin = digest(&std::fs::read(archive).unwrap());
            let service = Service::for_identity("spaceterm-development").unwrap();
            let metadata = service
                .inspect_candidate_for_target(
                    archive,
                    signature,
                    &pin,
                    remote_target,
                    crate::protocol::MAJOR,
                )
                .unwrap();
            assert_eq!(metadata.version, VERSION);
            assert_eq!(
                service.inspect_candidate_for_target(
                    archive,
                    signature,
                    &pin,
                    target(),
                    crate::protocol::MAJOR,
                ),
                Err(CoreError::RuntimeUnavailable)
            );
            let bytes = std::fs::read(archive).unwrap();
            let signature_bytes = std::fs::read(signature).unwrap();
            std::fs::remove_file(archive).unwrap();
            std::fs::remove_file(signature).unwrap();
            assert_eq!(
                crate::service::verify_candidate_bytes(
                    "spaceterm-development",
                    &bytes,
                    &signature_bytes,
                    &pin,
                    remote_target,
                    crate::protocol::MAJOR,
                ),
                Ok(metadata),
                "verification must use captured bytes without reopening or executing the source"
            );
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retained_candidate_revalidates_its_signature() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let installation = provision(
                &root.join("installations"),
                "spaceterm-development",
                archive,
                signature,
                target(),
                COMMIT,
                VERSION,
            )
            .unwrap();
            assert_eq!(
                verify(&installation.directory, "spaceterm-development"),
                Ok(())
            );
            std::fs::write(installation.directory.join("artifact.sig"), [0; 64]).unwrap();
            assert_eq!(
                verify(&installation.directory, "spaceterm-development"),
                Err(CoreError::RuntimeUnavailable)
            );
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn signed_candidate_discovery_refuses_another_protocol_major() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let pin = digest(&std::fs::read(archive).unwrap());
            let result = candidate(
                "spaceterm-development",
                archive,
                signature,
                &pin,
                Some(crate::protocol::MAJOR + 1),
            );
            assert!(matches!(result, Err(CoreError::IncompatibleProtocol(_))));
            let service = Service::for_identity("spaceterm-development").unwrap();
            let pin = digest(&std::fs::read(archive).unwrap());
            let metadata = service
                .inspect_candidate(archive, signature, &pin, crate::protocol::MAJOR)
                .unwrap();
            assert_eq!(metadata.version, VERSION);
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn signed_candidate_can_reuse_an_independent_release_and_deduplicates_whole_archive() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let pin = digest(&std::fs::read(archive).unwrap());
            let parent = root.join("installations");
            let first = provision_candidate(
                &parent,
                "spaceterm-development",
                archive,
                signature,
                &pin,
                Some(crate::protocol::MAJOR),
            )
            .unwrap();
            let second = provision_candidate(
                &parent,
                "spaceterm-development",
                archive,
                signature,
                &pin,
                Some(crate::protocol::MAJOR),
            )
            .unwrap();
            assert_eq!(first, second);
            rewrite(archive, signature, |entries| {
                entries
                    .iter_mut()
                    .find(|(name, _, _)| name.ends_with(".zshenv"))
                    .unwrap()
                    .1 = b"another signed resource".to_vec();
            });
            let changed_pin = digest(&std::fs::read(archive).unwrap());
            let changed = provision_candidate(
                &parent,
                "spaceterm-development",
                archive,
                signature,
                &changed_pin,
                Some(crate::protocol::MAJOR),
            )
            .unwrap();
            assert_eq!(changed.version, first.version);
            assert_ne!(changed.directory, first.directory);
            assert_eq!(std::fs::read_dir(parent).unwrap().count(), 2);
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn matching_digest_never_bypasses_signature_or_signed_compatibility_validation() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let parent = root.join("installations");
            for change in 0..3 {
                rewrite(archive, signature, |entries| {
                    let mut info: serde_json::Value =
                        serde_json::from_slice(&entries[0].1).unwrap();
                    match change {
                        0 => {
                            info["protocols"]["work"]["major"] = (crate::protocol::MAJOR + 1).into()
                        }
                        1 => {
                            info["protocols"]["work"]["major"] = crate::protocol::MAJOR.into();
                            info["capabilities"] = serde_json::json!([]);
                        }
                        2 => info
                            .as_object_mut()
                            .unwrap()
                            .remove("protocols")
                            .map(|_| ())
                            .unwrap(),
                        _ => unreachable!(),
                    }
                    entries[0].1 = serde_json::to_vec(&info).unwrap();
                });
                let pin = digest(&std::fs::read(archive).unwrap());
                assert!(
                    provision_candidate(
                        &parent,
                        "spaceterm-development",
                        archive,
                        signature,
                        &pin,
                        Some(crate::protocol::MAJOR),
                    )
                    .is_err()
                );
                assert!(!parent.exists());
            }
            std::fs::write(signature, [0; 64]).unwrap();
            let pin = digest(&std::fs::read(archive).unwrap());
            assert!(
                candidate(
                    "spaceterm-development",
                    archive,
                    signature,
                    &pin,
                    Some(crate::protocol::MAJOR),
                )
                .is_err()
            );
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn candidate_pin_covers_resources_and_build_metadata_before_retention() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let pin = digest(&std::fs::read(archive).unwrap());
            rewrite(archive, signature, |entries| {
                let resource = entries
                    .iter_mut()
                    .find(|(name, _, _)| name.ends_with(".zshenv"))
                    .unwrap();
                resource.1 = b"changed signed resource".to_vec();
            });
            let parent = root.join("installations");
            assert_eq!(
                provision_candidate(
                    &parent,
                    "spaceterm-development",
                    archive,
                    signature,
                    &pin,
                    Some(crate::protocol::MAJOR),
                ),
                Err(CoreError::CandidateChanged)
            );
            assert!(!parent.exists());
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn provision_retains_exact_signed_bytes_and_reuses_verified_installation() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let parent = root.join("installations");
            let first = provision(
                &parent,
                "spaceterm-development",
                archive,
                signature,
                target(),
                COMMIT,
                VERSION,
            )
            .unwrap();
            assert_eq!(
                std::fs::read(first.directory.join("artifact.tar.gz")).unwrap(),
                std::fs::read(archive).unwrap()
            );
            assert_eq!(
                describe(&first.directory, "spaceterm-development").unwrap(),
                (VERSION.to_owned(), COMMIT.to_owned())
            );
            let inode = std::fs::metadata(&first.directory).unwrap().ino();
            let second = provision(
                &parent,
                "spaceterm-development",
                archive,
                signature,
                target(),
                COMMIT,
                VERSION,
            )
            .unwrap();
            assert_eq!(first, second);
            assert_eq!(std::fs::metadata(&second.directory).unwrap().ino(), inode);
            assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn a_client_links_the_launcher_the_installer_selects_and_keeps_other_files() {
    let home = temporary();
    let service = Service::for_paths(crate::host::Paths {
        identity: "spaceterm-development".into(),
        data: home.join(".local/share/spaceterm-development/core"),
        runtime: home.join("runtime"),
    })
    .unwrap();
    with_artifact(
        &home.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let launcher = service.cli_launcher_path().unwrap();
            assert_eq!(launcher, home.join(".local/bin/spaceterm-development"));
            assert_eq!(service.cli_launcher(), Ok(CliLauncher::Missing));
            // Without an approved runtime, the launcher follows the installation just retained.
            let mut retained = None;
            assert_eq!(
                service.link_cli(|| {
                    let installation =
                        service.provision(archive, signature, target(), COMMIT, VERSION)?;
                    retained = Some(installation.directory.clone());
                    Ok(installation.directory)
                }),
                Ok(CliLink::Linked)
            );
            let installation = retained.unwrap();
            let executable =
                installation.join(Layout::host("spaceterm-development").unwrap().executable());
            assert_eq!(std::fs::read_link(&launcher).unwrap(), executable);
            assert_eq!(service.cli_launcher(), Ok(CliLauncher::Linked));

            // An approved runtime is selected without retaining anything.
            Directory::open(&service.paths.data, true)
                .unwrap()
                .write_json(
                    c"service.json",
                    &State {
                        installation: installation
                            .file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                        stopped: false,
                        automatic_start: true,
                        installed: true,
                        removing: false,
                        run: None,
                        activation: None,
                    },
                )
                .unwrap();
            std::fs::remove_file(&launcher).unwrap();
            let unused = || -> Result<PathBuf, CoreError> {
                panic!("an approved runtime needs no provisioning")
            };
            assert_eq!(service.link_cli(unused), Ok(CliLink::Linked));
            assert_eq!(std::fs::read_link(&launcher).unwrap(), executable);

            // A managed link whose installation is gone is replaced.
            std::fs::remove_file(&launcher).unwrap();
            let gone = service.installations_directory().join("b".repeat(64));
            std::os::unix::fs::symlink(
                gone.join(Layout::host("spaceterm-development").unwrap().executable()),
                &launcher,
            )
            .unwrap();
            assert_eq!(service.cli_launcher(), Ok(CliLauncher::Stale));
            assert_eq!(service.link_cli(unused), Ok(CliLink::Linked));
            assert_eq!(std::fs::read_link(&launcher).unwrap(), executable);

            // Another tool's link and a file that is not a link stay in place.
            std::fs::remove_file(&launcher).unwrap();
            std::os::unix::fs::symlink("/another/tool", &launcher).unwrap();
            assert_eq!(service.cli_launcher(), Ok(CliLauncher::Foreign));
            assert_eq!(service.link_cli(unused), Ok(CliLink::Kept));
            assert_eq!(
                std::fs::read_link(&launcher).unwrap(),
                Path::new("/another/tool")
            );
            std::fs::remove_file(&launcher).unwrap();
            std::fs::write(&launcher, b"local tool").unwrap();
            assert_eq!(service.cli_launcher(), Ok(CliLauncher::NotLink));
            assert_eq!(service.link_cli(unused), Ok(CliLink::Kept));
            assert_eq!(std::fs::read(&launcher).unwrap(), b"local tool");
        },
    );
    std::fs::remove_dir_all(home).unwrap();
}

fn temporary() -> PathBuf {
    let mut random = [0; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random).unwrap();
    let name: String = random.iter().map(|v| format!("{v:02x}")).collect();
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("spaceterm-provision-{name}"));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

#[test]
fn exact_build_mismatch_and_tampering_cannot_create_installations() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let parent = root.join("installations");
            for (identity, target, commit, version) in [
                ("spaceterm-preflight", target(), COMMIT, VERSION),
                ("spaceterm-development", "wrong-target", COMMIT, VERSION),
                (
                    "spaceterm-development",
                    target(),
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    VERSION,
                ),
                ("spaceterm-development", target(), COMMIT, "1.0.0"),
            ] {
                assert_eq!(
                    provision(
                        &parent, identity, archive, signature, target, commit, version
                    ),
                    Err(CoreError::RuntimeUnavailable)
                );
                assert!(!parent.exists());
            }
            std::fs::write(signature, [0; 64]).unwrap();
            assert_eq!(
                provision(
                    &parent,
                    "spaceterm-development",
                    archive,
                    signature,
                    target(),
                    COMMIT,
                    VERSION
                ),
                Err(CoreError::RuntimeUnavailable)
            );
            assert!(!parent.exists());
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn existing_tampered_or_substituted_installations_are_left_unchanged() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let parent = root.join("installations");
            let installed = provision(
                &parent,
                "spaceterm-development",
                archive,
                signature,
                target(),
                COMMIT,
                VERSION,
            )
            .unwrap();
            let resource = installed
                .directory
                .join(Layout::host("spaceterm-development").unwrap().resources())
                .join("shell-integration/zsh/.zshenv");
            std::fs::write(&resource, b"tampered").unwrap();
            assert_eq!(
                provision(
                    &parent,
                    "spaceterm-development",
                    archive,
                    signature,
                    target(),
                    COMMIT,
                    VERSION
                ),
                Err(CoreError::RuntimeUnavailable)
            );
            assert_eq!(std::fs::read(&resource).unwrap(), b"tampered");
            let saved = parent.join("saved");
            std::fs::rename(&installed.directory, &saved).unwrap();
            std::os::unix::fs::symlink(&saved, &installed.directory).unwrap();
            assert!(
                provision(
                    &parent,
                    "spaceterm-development",
                    archive,
                    signature,
                    target(),
                    COMMIT,
                    VERSION
                )
                .is_err()
            );
            assert!(installed.directory.is_symlink());
            assert_eq!(std::fs::read(&resource).unwrap(), b"tampered");
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsafe_store_and_unverified_extra_retained_resources_are_refused() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let parent = root.join("installations");
            std::fs::create_dir(&parent).unwrap();
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                provision(
                    &parent,
                    "spaceterm-development",
                    archive,
                    signature,
                    target(),
                    COMMIT,
                    VERSION
                ),
                Err(CoreError::UnsafeStorage)
            );
            assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
            let installed = provision(
                &parent,
                "spaceterm-development",
                archive,
                signature,
                target(),
                COMMIT,
                VERSION,
            )
            .unwrap();
            let extra = installed
                .directory
                .join(Layout::host("spaceterm-development").unwrap().resources())
                .join("shell-integration/zsh/extra");
            std::fs::write(&extra, b"unverified code").unwrap();
            std::fs::set_permissions(&extra, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                verify(&installed.directory, "spaceterm-development"),
                Err(CoreError::RuntimeUnavailable)
            );
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn changed_preparation_directory_survives_cleanup_and_publication_refusal() {
    let root = temporary();
    let parent = owned_directory(&root).unwrap();
    let prepared = files::Preparation::new(&parent).unwrap();
    let path = prepared.path(&root);
    std::fs::rename(&path, root.join("original")).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(path.join("witness"), b"preserve").unwrap();
    assert_eq!(
        prepared.publish(c"destination"),
        Err(CoreError::UnsafeStorage)
    );
    drop(prepared);
    assert_eq!(std::fs::read(path.join("witness")).unwrap(), b"preserve");
    assert!(root.join("original").is_dir());
    std::fs::remove_dir_all(root).unwrap();
}

/// Moves a fixture's members from `from`'s layout into `to`'s, as a package built for `to` lays
/// them out.
fn relayout(entries: &mut Vec<(String, Vec<u8>, u32)>, from: &str, to: &str) {
    let from = Layout::for_target(from, "spaceterm-development").unwrap();
    let to = Layout::for_target(to, "spaceterm-development").unwrap();
    let placed = |path: &Path| {
        if path == from.executable() {
            return Some(to.executable());
        }
        path.strip_prefix(from.resources())
            .ok()
            .map(|relative| to.resources().join(relative))
    };
    let bundle = from.bundle_files();
    entries.retain(|(name, _, _)| {
        !bundle
            .iter()
            .any(|file| Path::new(name) == Path::new("core").join(file))
    });
    for (name, _, _) in entries.iter_mut() {
        if let Some(path) = Path::new(name.as_str())
            .strip_prefix("core")
            .ok()
            .and_then(placed)
        {
            *name = format!("core/{}", path.display());
        }
    }
    for file in to.bundle_files() {
        entries.push((
            format!("core/{}", file.display()),
            b"signed bundle file".to_vec(),
            0o644,
        ));
    }
}

fn rewrite(
    archive: &Path,
    signature: &Path,
    mut edit: impl FnMut(&mut Vec<(String, Vec<u8>, u32)>),
) {
    let bytes = std::fs::read(archive).unwrap();
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
    let mut entries: Vec<_> = tar
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_str().unwrap().to_owned();
            let mode = entry.header().mode().unwrap();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            (path, data, mode)
        })
        .collect();
    edit(&mut entries);
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    for (path, data, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(mode);
        header.set_size(data.len() as u64);
        if path.len() < 100 {
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            tar.append(&header, data.as_slice()).unwrap();
        } else {
            tar.append_data(&mut header, &path, data.as_slice())
                .unwrap();
        }
    }
    std::fs::write(archive, tar.into_inner().unwrap().finish().unwrap()).unwrap();
    test_support::sign(archive, signature);
}

#[test]
fn signed_missing_resources_unsafe_layout_and_malformed_build_are_refused_before_retention() {
    for change in 0..8 {
        let root = temporary();
        with_artifact(
            &root.join("source"),
            "spaceterm-development",
            |archive, signature| {
                rewrite(archive, signature, |entries| match change {
                    0 => {
                        entries.retain(|(name, _, _)| !name.ends_with(".zshenv"));
                    }
                    1 => {
                        entries.push(("core/../escaped".into(), b"unsafe".to_vec(), 0o644));
                    }
                    2 => {
                        entries.push(("core//ambiguous".into(), b"unsafe".to_vec(), 0o644));
                    }
                    3 => {
                        entries.push(entries[0].clone());
                    }
                    4 => {
                        entries.push(("core/extra".into(), b"unsafe".to_vec(), 0o644));
                    }
                    5 => {
                        entries[0].1 = b"{\"schema\":1,\"schema\":1}".to_vec();
                    }
                    6 => {
                        entries[1].2 = 0o6755;
                    }
                    7 => {
                        let mut info: serde_json::Value =
                            serde_json::from_slice(&entries[0].1).unwrap();
                        info["version"] = "01.2.3".into();
                        entries[0].1 = serde_json::to_vec(&info).unwrap();
                    }
                    _ => unreachable!(),
                });
                let parent = root.join("installations");
                assert_eq!(
                    provision(
                        &parent,
                        "spaceterm-development",
                        archive,
                        signature,
                        target(),
                        COMMIT,
                        VERSION
                    ),
                    Err(CoreError::RuntimeUnavailable),
                    "change {change}"
                );
                assert!(!parent.exists());
            },
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn signed_manifest_cannot_claim_a_different_compiled_build() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            let main = format!(
                "core/{}",
                Layout::host("spaceterm-development")
                    .unwrap()
                    .executable()
                    .display()
            );
            rewrite(archive, signature, |entries| {
                let compiled = entries[0].1.clone();
                let mut info: serde_json::Value = serde_json::from_slice(&compiled).unwrap();
                info["version"] = "1.2.3".into();
                entries[0].1 = serde_json::to_vec(&info).unwrap();
                entries
                    .iter_mut()
                    .find(|(name, _, _)| *name == main)
                    .unwrap()
                    .1 = compiled;
            });
            let parent = root.join("installations");
            assert_eq!(
                provision(
                    &parent,
                    "spaceterm-development",
                    archive,
                    signature,
                    target(),
                    COMMIT,
                    "1.2.3"
                ),
                Err(CoreError::RuntimeUnavailable)
            );
            assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_provisioning_publishes_one_exact_installation() {
    let root = temporary();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|index| {
            let root = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                with_artifact(
                    &root.join(format!("source-{index}")),
                    "spaceterm-development",
                    |archive, signature| {
                        barrier.wait();
                        provision(
                            &root.join("installations"),
                            "spaceterm-development",
                            archive,
                            signature,
                            target(),
                            COMMIT,
                            VERSION,
                        )
                        .unwrap()
                    },
                )
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results[0], results[1]);
    assert_eq!(
        std::fs::read_dir(root.join("installations"))
            .unwrap()
            .count(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn publication_refuses_an_existing_empty_destination() {
    let root = temporary();
    let parent = owned_directory(&root).unwrap();
    let prepared = files::Preparation::new(&parent).unwrap();
    let preparation_path = prepared.path(&root);
    let destination = root.join("destination");
    std::fs::create_dir(&destination).unwrap();
    let inode = std::fs::metadata(&destination).unwrap().ino();
    assert_eq!(
        prepared.publish(c"destination"),
        Err(CoreError::UnsafeStorage)
    );
    drop(prepared);
    assert_eq!(std::fs::metadata(destination).unwrap().ino(), inode);
    assert!(!preparation_path.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_update_inspection_accepts_a_signed_new_major_without_weakening_pins() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            rewrite(archive, signature, |entries| {
                let mut manifest: serde_json::Value =
                    serde_json::from_slice(&entries[0].1).unwrap();
                manifest["protocols"]["work"]["major"] = (crate::protocol::MAJOR + 1).into();
                entries[0].1 = serde_json::to_vec(&manifest).unwrap();
            });
            let service = Service::for_identity("spaceterm-development").unwrap();
            let pin = digest(&std::fs::read(archive).unwrap());
            assert_eq!(
                service
                    .inspect_update_candidate(archive, signature, &pin)
                    .unwrap()
                    .protocols
                    .work
                    .major,
                crate::protocol::MAJOR + 1
            );
            assert_eq!(
                service.inspect_candidate(archive, signature, &pin, crate::protocol::MAJOR,),
                Err(CoreError::IncompatibleProtocol(
                    crate::protocol::version::Incompatible::CallerTooOld
                ))
            );
            assert_eq!(
                service.inspect_update_candidate(archive, signature, &"0".repeat(64)),
                Err(CoreError::CandidateChanged)
            );
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Counts the heap bytes a measured thread holds, so a test can read the peak a call adds.
struct Measured;
thread_local! {
    static MEASURING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LIVE: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static PEAK: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}
fn record(change: isize) {
    let _ = MEASURING.try_with(|measuring| {
        if measuring.get() {
            let live = LIVE.get() + change;
            LIVE.set(live);
            PEAK.set(PEAK.get().max(live));
        }
    });
}
// SAFETY: every call forwards to the system allocator unchanged and only counts sizes.
unsafe impl std::alloc::GlobalAlloc for Measured {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let pointer = unsafe { std::alloc::System.alloc(layout) };
        if !pointer.is_null() {
            record(layout.size() as isize);
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        let pointer = unsafe { std::alloc::System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record(layout.size() as isize);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(pointer, layout) };
        record(-(layout.size() as isize));
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        let moved = unsafe { std::alloc::System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            record(size as isize - layout.size() as isize);
        }
        moved
    }
}
#[global_allocator]
static MEASURED: Measured = Measured;

/// The most heap `run` holds at once on this thread beyond what it started with.
fn peak_transient<T>(run: impl FnOnce() -> T) -> (T, usize) {
    LIVE.set(0);
    PEAK.set(0);
    MEASURING.set(true);
    let result = run();
    MEASURING.set(false);
    (result, PEAK.get().max(0) as usize)
}

/// Bytes that deflate can't shrink, so the archive grows with the executable.
fn incompressible(size: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    (0..size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// Retains a signed fixture after `edit` changes its archive entries.
fn installation_with(root: &Path, edit: impl FnMut(&mut Vec<(String, Vec<u8>, u32)>)) -> PathBuf {
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            rewrite(archive, signature, edit);
            provision(
                &root.join("installations"),
                "spaceterm-development",
                archive,
                signature,
                target(),
                COMMIT,
                VERSION,
            )
            .unwrap()
            .directory
        },
    )
}

/// Retains a signed fixture whose native executable holds `executable` bytes.
fn installation_with_executable(root: &Path, executable: Vec<u8>) -> PathBuf {
    let main = format!(
        "core/{}",
        Layout::host("spaceterm-development")
            .unwrap()
            .executable()
            .display()
    );
    installation_with(root, |entries| {
        entries
            .iter_mut()
            .find(|(name, _, _)| *name == main)
            .unwrap()
            .1 = executable.clone();
    })
}

/// The installed path of the fixture's zsh environment resource.
fn zshenv(installation: &Path) -> PathBuf {
    installation
        .join(Layout::host("spaceterm-development").unwrap().resources())
        .join("shell-integration/zsh/.zshenv")
}

#[test]
fn verification_refuses_a_retained_file_with_a_second_name() {
    let root = temporary();
    let installation = installation_with_executable(&root, b"signed".to_vec());
    let check =
        || test_support::with_fixture_trust(|| verify(&installation, "spaceterm-development"));
    assert_eq!(check(), Ok(()));
    for retained in [zshenv(&installation), installation.join("artifact.tar.gz")] {
        let outside = root.join("second-name");
        std::fs::hard_link(&retained, &outside).unwrap();
        assert_eq!(
            check(),
            Err(CoreError::RuntimeUnavailable),
            "{} passed verification with a second name",
            retained.display()
        );
        std::fs::remove_file(&outside).unwrap();
        assert_eq!(check(), Ok(()));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn verification_refuses_a_fifo_in_place_of_an_empty_installed_file() {
    // An empty entry leaves the file type as the only difference: a FIFO with no writer has
    // length zero and reads as immediately ended.
    let root = temporary();
    let installation = installation_with(&root, |entries| {
        entries
            .iter_mut()
            .find(|(name, _, _)| name.ends_with("/shell-integration/zsh/.zshenv"))
            .unwrap()
            .1
            .clear();
    });
    let check =
        || test_support::with_fixture_trust(|| verify(&installation, "spaceterm-development"));
    assert_eq!(check(), Ok(()));
    let resource = zshenv(&installation);
    std::fs::remove_file(&resource).unwrap();
    let name = CString::new(resource.as_os_str().as_bytes()).unwrap();
    // SAFETY: the terminated path stays live through the call.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o644) }, 0);
    std::fs::set_permissions(&resource, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(check(), Err(CoreError::RuntimeUnavailable));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn verification_refuses_an_installed_file_with_another_mode() {
    let root = temporary();
    let installation = installation_with_executable(&root, b"signed".to_vec());
    let check =
        || test_support::with_fixture_trust(|| verify(&installation, "spaceterm-development"));
    assert_eq!(check(), Ok(()));
    std::fs::set_permissions(
        zshenv(&installation),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(check(), Err(CoreError::RuntimeUnavailable));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn verification_refuses_an_installation_named_for_another_digest() {
    let root = temporary();
    let installation = installation_with_executable(&root, b"signed".to_vec());
    let renamed = installation.with_file_name("0".repeat(64));
    std::fs::rename(&installation, &renamed).unwrap();
    assert_eq!(
        test_support::with_fixture_trust(|| verify(&renamed, "spaceterm-development")),
        Err(CoreError::RuntimeUnavailable)
    );
    std::fs::rename(&renamed, &installation).unwrap();
    assert_eq!(
        test_support::with_fixture_trust(|| verify(&installation, "spaceterm-development")),
        Ok(())
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn snapshot_refuses_an_archive_that_grows_while_it_is_read() {
    /// Appends a byte to the file at `path` once the first read of `file` returns.
    struct Growing {
        file: File,
        path: PathBuf,
        grown: bool,
    }
    impl Read for Growing {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.file.read(buffer)?;
            if !self.grown {
                self.grown = true;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&self.path)?
                    .write_all(&[0])?;
            }
            Ok(read)
        }
    }
    let root = temporary();
    let path = root.join("artifact.tar.gz");
    std::fs::write(&path, incompressible(4 * CHUNK)).unwrap();
    let source = |grown| Growing {
        file: File::open(&path).unwrap(),
        path: path.clone(),
        grown,
    };
    let length = 4 * CHUNK;
    assert!(files::Snapshot::copy(source(true), length).is_ok());
    assert_eq!(
        files::Snapshot::copy(source(false), length).map(|_| ()),
        Err(CoreError::RuntimeUnavailable)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn verification_holds_the_same_transient_memory_for_any_package_size() {
    const SMALL: usize = 1024 * 1024;
    const LARGE: usize = 8 * SMALL;
    let mut peaks = Vec::new();
    for size in [SMALL, LARGE] {
        let root = temporary();
        let installation = installation_with_executable(&root, incompressible(size));
        let (result, peak) = test_support::with_fixture_trust(|| {
            peak_transient(|| verify(&installation, "spaceterm-development"))
        });
        assert_eq!(result, Ok(()));
        eprintln!("verification of a {size}-byte executable held at most {peak} heap bytes");
        peaks.push(peak);
        std::fs::remove_dir_all(root).unwrap();
    }
    let (small, large) = (peaks[0], peaks[1]);
    assert!(
        small < SMALL,
        "verification held {small} bytes, as much as a whole {SMALL}-byte file"
    );
    assert!(
        large <= small + 4096,
        "verification held {large} bytes for an {LARGE}-byte executable and {small} for {SMALL}"
    );
}

#[test]
fn installing_and_staging_hold_the_same_transient_memory_for_any_package_size() {
    const SMALL: usize = 1024 * 1024;
    const LARGE: usize = 8 * SMALL;
    const OPERATIONS: [&str; 4] = [
        "provision",
        "provision_candidate",
        "stage_candidate",
        "retain_cli",
    ];
    let main = format!(
        "core/{}",
        Layout::host("spaceterm-development")
            .unwrap()
            .executable()
            .display()
    );
    let mut peaks = Vec::new();
    for size in [SMALL, LARGE] {
        let root = temporary();
        let executable = incompressible(size);
        // Each operation has its own account, so each extracts and writes the whole package.
        let service = |name: &str| {
            Service::for_paths(crate::host::Paths {
                identity: "spaceterm-development".into(),
                data: root
                    .join(name)
                    .join(".local/share/spaceterm-development/core"),
                runtime: root.join(name).join("runtime"),
            })
            .unwrap()
        };
        let measured = with_artifact(
            &root.join("source"),
            "spaceterm-development",
            |archive, signature| {
                rewrite(archive, signature, |entries| {
                    entries
                        .iter_mut()
                        .find(|(name, _, _)| *name == main)
                        .unwrap()
                        .1 = executable.clone();
                });
                let pin = digest(&std::fs::read(archive).unwrap());
                let package = root.join("package");
                std::fs::create_dir(&package).unwrap();
                std::fs::copy(archive, package.join("artifact.tar.gz")).unwrap();
                std::fs::copy(signature, package.join("artifact.sig")).unwrap();
                let services = OPERATIONS.map(service);
                let peak = |result: Result<(), CoreError>, peak| {
                    assert_eq!(result, Ok(()));
                    peak
                };
                [
                    peak_transient(|| {
                        services[0]
                            .provision(archive, signature, target(), COMMIT, VERSION)
                            .map(drop)
                    }),
                    peak_transient(|| {
                        services[1]
                            .provision_candidate(archive, signature, &pin, crate::protocol::MAJOR)
                            .map(drop)
                    }),
                    peak_transient(|| {
                        services[2]
                            .stage_candidate(archive, signature, &pin)
                            .map(drop)
                    }),
                    peak_transient(|| services[3].retain_cli(&package).map(drop)),
                ]
                .map(|(result, measured)| peak(result, measured))
            },
        );
        peaks.push(measured);
        std::fs::remove_dir_all(root).unwrap();
    }
    for (index, operation) in OPERATIONS.iter().enumerate() {
        let (small, large) = (peaks[0][index], peaks[1][index]);
        eprintln!(
            "{operation} held at most {small} heap bytes for {SMALL} and {large} for {LARGE}"
        );
        assert!(
            small < SMALL,
            "{operation} held {small} bytes, as much as a whole {SMALL}-byte file"
        );
        assert!(
            large <= small + 4096,
            "{operation} held {large} bytes for an {LARGE}-byte executable and {small} for {SMALL}"
        );
    }
}

#[test]
fn verification_compares_every_piece_of_an_installed_file() {
    let root = temporary();
    let signed = incompressible(16 * CHUNK + 1);
    let installation = installation_with_executable(&root, signed.clone());
    let executable = installation.join(Layout::host("spaceterm-development").unwrap().executable());
    let check =
        || test_support::with_fixture_trust(|| verify(&installation, "spaceterm-development"));
    assert_eq!(check(), Ok(()));
    for (piece, index) in [
        ("first", 0),
        ("middle", 8 * CHUNK + CHUNK / 2),
        ("final", signed.len() - 1),
    ] {
        let mut changed = signed.clone();
        changed[index] ^= 1;
        std::fs::write(&executable, &changed).unwrap();
        assert_eq!(
            check(),
            Err(CoreError::RuntimeUnavailable),
            "a changed byte in the {piece} piece passed verification"
        );
        std::fs::write(&executable, &signed).unwrap();
        assert_eq!(check(), Ok(()));
    }
    let mut longer = signed.clone();
    longer.push(0);
    std::fs::write(&executable, &longer).unwrap();
    assert_eq!(check(), Err(CoreError::RuntimeUnavailable));
    std::fs::write(&executable, &signed[..signed.len() - 1]).unwrap();
    assert_eq!(check(), Err(CoreError::RuntimeUnavailable));
    std::fs::write(&executable, &signed).unwrap();
    assert_eq!(check(), Ok(()));
    std::fs::remove_dir_all(root).unwrap();
}

/// Retains a fixture, then builds a second signed archive of the same length whose executable
/// differs in one byte. Returns the installation, the second archive and its executable.
fn installation_with_same_length_substitute(root: &Path) -> (PathBuf, Vec<u8>, Vec<u8>) {
    let signed = incompressible(4 * CHUNK);
    let installation = installation_with_executable(root, signed.clone());
    let main = format!(
        "core/{}",
        Layout::host("spaceterm-development")
            .unwrap()
            .executable()
            .display()
    );
    let mut substitute = signed;
    substitute[CHUNK] ^= 1;
    let (archive, signature) = (root.join("substitute.tar.gz"), root.join("substitute.sig"));
    std::fs::copy(installation.join("artifact.tar.gz"), &archive).unwrap();
    rewrite(&archive, &signature, |entries| {
        entries
            .iter_mut()
            .find(|(name, _, _)| *name == main)
            .unwrap()
            .1 = substitute.clone();
    });
    let replacement = std::fs::read(&archive).unwrap();
    assert_eq!(
        replacement.len() as u64,
        std::fs::metadata(installation.join("artifact.tar.gz"))
            .unwrap()
            .len(),
        "the substitute archive must have the retained archive's length"
    );
    (installation, replacement, substitute)
}

#[test]
fn verification_compares_installed_files_to_the_authenticated_archive_after_an_in_place_rewrite() {
    let root = temporary();
    let (installation, replacement, substitute) = installation_with_same_length_substitute(&root);
    let identity = "spaceterm-development";
    let executable = installation.join(Layout::host(identity).unwrap().executable());
    test_support::with_fixture_trust(|| {
        let directory = owned_directory(&installation).unwrap();
        let bytes =
            files::Snapshot::read(&directory, Path::new("artifact.tar.gz"), MAX_ARCHIVE).unwrap();
        let signature = std::fs::read(installation.join("artifact.sig")).unwrap();
        assert_eq!(authenticate(&bytes, &signature, identity), Ok(()));
        assert_eq!(
            authenticate(&replacement, &signature, identity),
            Err(CoreError::RuntimeUnavailable)
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(installation.join("artifact.tar.gz"))
            .unwrap()
            .write_all(&replacement)
            .unwrap();
        std::fs::write(&executable, &substitute).unwrap();
        assert_eq!(
            verify_contents(&directory, &bytes, identity).map(|_| ()),
            Err(CoreError::RuntimeUnavailable)
        );
    });
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn verification_survives_a_truncated_archive_after_authentication() {
    let root = temporary();
    let installation = installation_with_executable(&root, incompressible(4 * CHUNK));
    let identity = "spaceterm-development";
    let archive = installation.join("artifact.tar.gz");
    test_support::with_fixture_trust(|| {
        let directory = owned_directory(&installation).unwrap();
        let bytes =
            files::Snapshot::read(&directory, Path::new("artifact.tar.gz"), MAX_ARCHIVE).unwrap();
        let signature = std::fs::read(installation.join("artifact.sig")).unwrap();
        assert_eq!(authenticate(&bytes, &signature, identity), Ok(()));
        std::fs::OpenOptions::new()
            .write(true)
            .open(&archive)
            .unwrap()
            .set_len(1)
            .unwrap();
        assert!(verify_contents(&directory, &bytes, identity).is_ok());
        assert_eq!(
            verify(&installation, identity),
            Err(CoreError::RuntimeUnavailable)
        );
    });
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "macos")]
const RELEASE_REQUIREMENT: &str = "identifier \"io.github.sadiksaifi.spaceterm.core\" and anchor \
    apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] and certificate \
    leaf[field.1.2.840.113635.100.6.1.13] and certificate leaf[subject.OU] = \"ABCDE12345\"";

#[cfg(target_os = "macos")]
#[test]
fn release_requirement_pins_the_identifier_developer_id_and_build_time_team() {
    assert_eq!(
        native_requirement("spaceterm", Some("ABCDE12345")).as_deref(),
        Some(RELEASE_REQUIREMENT)
    );
    for identity in ["spaceterm-preflight", "spaceterm-development"] {
        assert_eq!(
            native_requirement(identity, Some("ABCDE12345")),
            Some(format!(
                "identifier \"io.github.sadiksaifi.{identity}.core\""
            ))
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn release_requirement_fails_closed_without_a_valid_build_time_team() {
    for team in [
        None,
        Some(""),
        Some("abcde12345"),
        Some("ABCDE1234"),
        Some("ABCDE123456"),
        Some("ABCDE1234\""),
        Some("ABCDE 1234"),
    ] {
        assert_eq!(native_requirement("spaceterm", team), None, "{team:?}");
    }
}

/// A disposable bundle of `identity` holding an ad hoc signed copy of a system executable.
#[cfg(target_os = "macos")]
fn adhoc_package(identity: &str) -> PathBuf {
    let root = temporary();
    let layout = Layout::host(identity).unwrap();
    let executable = root.join(layout.executable());
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::copy("/usr/bin/true", &executable).unwrap();
    let bundle = root.join(layout.signed());
    let identifier = bundle_identifier(identity);
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>{identifier}</string>\
             <key>CFBundleExecutable</key><string>spaceterm</string>\
             <key>CFBundlePackageType</key><string>APPL</string></dict></plist>\n"
        ),
    )
    .unwrap();
    let signed = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--identifier", &identifier])
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(signed.status.success(), "{signed:?}");
    root
}

#[cfg(target_os = "macos")]
#[test]
fn release_native_verification_rejects_an_adhoc_bundle_with_the_release_identifier() {
    assert!(!test_support::active());
    let release = adhoc_package("spaceterm");
    let bundle = release.join(Layout::host("spaceterm").unwrap().signed());
    // The bundle is valid and carries the Release identifier, so only the Team ID part rejects it.
    verify_signature(
        &bundle,
        "identifier \"io.github.sadiksaifi.spaceterm.core\"",
    )
    .unwrap();
    assert!(matches!(
        verify_signature(&bundle, RELEASE_REQUIREMENT),
        Err(CoreError::RuntimeUnavailable)
    ));
    // Without the fixture bypass, the compiled Release check rejects it with or without a team.
    assert!(matches!(
        verify_native(&release, "spaceterm"),
        Err(CoreError::RuntimeUnavailable)
    ));
    let preflight = adhoc_package("spaceterm-preflight");
    verify_native(&preflight, "spaceterm-preflight").unwrap();
    std::fs::remove_dir_all(release).unwrap();
    std::fs::remove_dir_all(preflight).unwrap();
}
