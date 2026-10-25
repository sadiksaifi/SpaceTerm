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
                    let result = service.inspect_candidate(
                        archive,
                        signature,
                        &pin,
                        1,
                        &["workspace_policy"],
                    );
                    if case == "clean" || (identity != "spaceterm" && case == "dirty") {
                        assert_eq!(result.unwrap().version, version);
                    } else {
                        assert_eq!(result, Err(ServerError::RuntimeUnavailable));
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
        "../../../../../packaging/server-nonproduction-trust.json"
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
            || service.inspect_candidate(&archive, &signature, &pin, 1, &["workspace_policy"]);
        if identity == "spaceterm" {
            assert_eq!(inspect(), Err(ServerError::RuntimeUnavailable));
            let parent = root.join("release-installations");
            assert_eq!(
                provision_candidate(
                    &parent,
                    identity,
                    &archive,
                    &signature,
                    &pin,
                    1,
                    &["workspace_policy"]
                ),
                Err(ServerError::RuntimeUnavailable)
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
            files::create_file(&directory, Path::new("artifact.tar.gz"), &bytes, 0o644).unwrap();
            files::create_file(
                &directory,
                Path::new("artifact.sig"),
                &std::fs::read(&signature).unwrap(),
                0o644,
            )
            .unwrap();
            assert_eq!(
                verify(&installation, identity),
                Err(ServerError::RuntimeUnavailable)
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
                other.inspect_candidate(&archive, &signature, &pin, 1, &["workspace_policy"]),
                Err(ServerError::RuntimeUnavailable)
            );
            std::fs::write(&signature, [0; 64]).unwrap();
            assert_eq!(inspect(), Err(ServerError::RuntimeUnavailable));
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
                entries
                    .iter_mut()
                    .find(|(name, _, _)| name == "server/bin/spaceterm-server")
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
                    1,
                    &["workspace_policy"],
                )
                .unwrap();
            assert_eq!(metadata.version, VERSION);
            assert_eq!(
                service.inspect_candidate_for_target(
                    archive,
                    signature,
                    &pin,
                    target(),
                    1,
                    &["workspace_policy"]
                ),
                Err(ServerError::RuntimeUnavailable)
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
                    1,
                    &["workspace_policy"],
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
fn retained_candidate_revalidates_declared_compatibility_and_signature() {
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
                verify_candidate_installation(
                    &installation.directory,
                    "spaceterm-development",
                    1,
                    &["workspace_policy"]
                ),
                Ok(())
            );
            assert_eq!(
                verify_candidate_installation(
                    &installation.directory,
                    "spaceterm-development",
                    2,
                    &["workspace_policy"]
                ),
                Err(ServerError::IncompatibleProtocol)
            );
            assert_eq!(
                verify_candidate_installation(
                    &installation.directory,
                    "spaceterm-development",
                    1,
                    &["future_required"]
                ),
                Err(ServerError::MissingRequiredCapability)
            );
            std::fs::write(installation.directory.join("artifact.sig"), [0; 64]).unwrap();
            assert_eq!(
                verify_candidate_installation(
                    &installation.directory,
                    "spaceterm-development",
                    1,
                    &["workspace_policy"]
                ),
                Err(ServerError::RuntimeUnavailable)
            );
        },
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn signed_candidate_discovery_distinguishes_required_major_and_capability() {
    let root = temporary();
    with_artifact(
        &root.join("source"),
        "spaceterm-development",
        |archive, signature| {
            for (major, required, expected) in [
                (
                    2,
                    vec!["workspace_policy"],
                    ServerError::IncompatibleProtocol,
                ),
                (
                    1,
                    vec!["future_required"],
                    ServerError::MissingRequiredCapability,
                ),
            ] {
                let pin = digest(&std::fs::read(archive).unwrap());
                let result = candidate(
                    "spaceterm-development",
                    archive,
                    signature,
                    &pin,
                    major,
                    &required,
                );
                assert!(matches!(result, Err(error) if error == expected));
            }
            let service = Service::for_identity("spaceterm-development").unwrap();
            let pin = digest(&std::fs::read(archive).unwrap());
            let metadata = service
                .inspect_candidate(archive, signature, &pin, 1, &["workspace_policy"])
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
                1,
                &["workspace_policy"],
            )
            .unwrap();
            let second = provision_candidate(
                &parent,
                "spaceterm-development",
                archive,
                signature,
                &pin,
                1,
                &["workspace_policy"],
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
                1,
                &["workspace_policy"],
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
            for change in 0..4 {
                rewrite(archive, signature, |entries| {
                    let mut info: serde_json::Value =
                        serde_json::from_slice(&entries[0].1).unwrap();
                    match change {
                        0 => info["protocol_major"] = 2.into(),
                        1 => {
                            info["protocol_major"] = 1.into();
                            info["capabilities"] = serde_json::json!([]);
                        }
                        2 => {
                            info["capabilities"] =
                                serde_json::json!(["workspace_policy", "workspace_policy"])
                        }
                        3 => info
                            .as_object_mut()
                            .unwrap()
                            .remove("protocol_major")
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
                        1,
                        &["workspace_policy"]
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
                    1,
                    &["workspace_policy"]
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
                    1,
                    &["workspace_policy"]
                ),
                Err(ServerError::CandidateChanged)
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
                    Err(ServerError::RuntimeUnavailable)
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
                Err(ServerError::RuntimeUnavailable)
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
                .join("share/spaceterm/shell-integration/zsh/.zshenv");
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
                Err(ServerError::RuntimeUnavailable)
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
                Err(ServerError::UnsafeStorage)
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
                .join("share/spaceterm/shell-integration/zsh/extra");
            std::fs::write(&extra, b"unverified code").unwrap();
            std::fs::set_permissions(&extra, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                verify(&installed.directory, "spaceterm-development"),
                Err(ServerError::RuntimeUnavailable)
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
        Err(ServerError::UnsafeStorage)
    );
    drop(prepared);
    assert_eq!(std::fs::read(path.join("witness")).unwrap(), b"preserve");
    assert!(root.join("original").is_dir());
    std::fs::remove_dir_all(root).unwrap();
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
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        header.set_cksum();
        tar.append(&header, data.as_slice()).unwrap();
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
                        entries.push(("server/../escaped".into(), b"unsafe".to_vec(), 0o644));
                    }
                    2 => {
                        entries.push(("server//ambiguous".into(), b"unsafe".to_vec(), 0o644));
                    }
                    3 => {
                        entries.push(entries[0].clone());
                    }
                    4 => {
                        entries.push(("server/extra".into(), b"unsafe".to_vec(), 0o644));
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
                    Err(ServerError::RuntimeUnavailable),
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
            rewrite(archive, signature, |entries| {
                let mut info: serde_json::Value = serde_json::from_slice(&entries[0].1).unwrap();
                info["version"] = "1.2.3".into();
                entries[0].1 = serde_json::to_vec(&info).unwrap();
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
                Err(ServerError::RuntimeUnavailable)
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
        Err(ServerError::UnsafeStorage)
    );
    drop(prepared);
    assert_eq!(std::fs::metadata(destination).unwrap().ino(), inode);
    assert!(!preparation_path.exists());
    std::fs::remove_dir_all(root).unwrap();
}
