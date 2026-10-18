use super::*;
use std::os::unix::fs::PermissionsExt;
use test_support::{COMMIT, VERSION, target, with_artifact};

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
