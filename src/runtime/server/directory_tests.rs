use super::tests::{host, size};
use super::*;

#[test]
fn explicit_directory_creation_is_recursive_idempotent_and_does_not_edit_workspaces() {
    use pane::directories::{CreationRequest, Progress, Value};
    let root = std::env::temp_dir().join(format!(
        "spaceterm-create-{:x}",
        u64::from_ne_bytes(nonce().unwrap()[..8].try_into().unwrap())
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("parent/child");
    let mut host = SharedWorkspaceHost::new(host());
    assert!(matches!(
        host.request([1; 32], [2; 32], Request::Capabilities),
        Reply::Capabilities {
            directory_creation: true,
            ..
        }
    ));
    let before = host.request([1; 32], [2; 32], Request::Inventory);
    for id in [1, 2] {
        assert!(matches!(
            host.request(
                [1; 32],
                [2; 32],
                Request::DirectoryCreate(CreationRequest::Begin {
                    id,
                    path: path.clone()
                })
            ),
            Reply::DirectoryCreate(Ok(Progress::Pending))
        ));
        let until = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match host.request(
                [1; 32],
                [2; 32],
                Request::DirectoryCreate(CreationRequest::Poll { id }),
            ) {
                Reply::DirectoryCreate(Ok(Progress::Ready(Value::Directory(actual)))) => {
                    assert_eq!(actual, path);
                    assert!(actual.is_dir());
                    break;
                }
                Reply::DirectoryCreate(Ok(Progress::Pending))
                    if std::time::Instant::now() < until =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => panic!("directory creation did not complete"),
            }
        }
    }
    let after = host.request([1; 32], [2; 32], Request::Inventory);
    let (
        Reply::Inventory {
            inventory: before, ..
        },
        Reply::Inventory {
            inventory: after, ..
        },
    ) = (before, after)
    else {
        panic!("inventory unavailable")
    };
    assert_eq!(before, after);
    host.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn directory_recovery_poll_retires_a_worker_directory_failure_before_any_tick() {
    let root = std::env::temp_dir().join(format!(
        "spaceterm-poll-directory-{:x}",
        u64::from_ne_bytes(nonce().unwrap()[..8].try_into().unwrap())
    ));
    std::fs::create_dir(&root).unwrap();
    let directory = local_filesystem().validate_directory(&root).unwrap();
    let mut host = host();
    let original = Organization::first(root.clone()).unwrap();
    host.organization = Some(original.clone());
    // The owning host validated this object, but it disappears before worker preparation.
    std::fs::remove_dir(&root).unwrap();
    let lease = host
        .start_directory([1; 32], [1; 32], size(), None, directory)
        .unwrap();
    let cleanup = host
        .pane
        .as_mut()
        .unwrap()
        .runtime
        .shutdown_and_wait(Duration::from_secs(5));
    assert!(matches!(
        host.request([1; 32], [1; 32], Request::Poll { lease }),
        Reply::Refused(Refusal::DirectoryUnavailable)
    ));
    assert!(
        cleanup.is_ok(),
        "preparation failed before any process was started"
    );
    assert!(host.pane.is_none());
    assert!(host.organization.as_ref() == Some(&original));
    assert!(matches!(
        host.request(
            [1; 32],
            [2; 32],
            Request::Select {
                create: false,
                size: size(),
                resume: None,
                style: None
            }
        ),
        Reply::Restored {
            directory_unavailable: true,
            recovery: Some(_),
            ..
        }
    ));
}

#[test]
fn directory_repair_preserves_organization_and_cannot_replace_a_running_shell() {
    let mut host = host();
    let mut original = Organization::first(host.directory.clone()).unwrap();
    original.pinned_directory = Some(host.directory.clone());
    original.confirmed_directory = Some(host.directory.join("missing-directory-repair"));
    host.organization = Some(original.clone());
    let target = pane::directories::Target {
        boot: host.boot,
        pane: original.pane,
        original: original.startup_directory().to_owned(),
    };
    let request = Request::RepairDirectory {
        target,
        replacement: Some(host.directory.clone()),
        size: size(),
        style: None,
    };
    let Reply::Attached { lease } = host.request([1; 32], [1; 32], request.clone()) else {
        panic!("valid replacement should start the retained Pane");
    };
    let mut expected = original;
    expected.confirmed_directory = Some(host.directory.clone());
    assert!(host.organization.as_ref() == Some(&expected));
    assert!(
        matches!(host.request([2; 32], [2; 32], request), Reply::Takeover { target, .. } if target == lease.into())
    );
    host.shutdown().unwrap();
}

#[test]
fn retry_revalidates_the_original_and_passive_reconnect_never_starts_it() {
    let root = std::env::temp_dir().join(format!(
        "spaceterm-retry-{}-{:x}",
        std::process::id(),
        u64::from_ne_bytes(nonce().unwrap()[..8].try_into().unwrap())
    ));
    let mut host = host();
    let mut original = Organization::first(host.directory.clone()).unwrap();
    original.confirmed_directory = Some(root.clone());
    host.organization = Some(original.clone());
    let target = pane::directories::Target {
        boot: host.boot,
        pane: original.pane,
        original: root.clone(),
    };
    let retry = Request::RepairDirectory {
        target,
        replacement: None,
        size: size(),
        style: None,
    };
    assert!(matches!(
        host.request([1; 32], [1; 32], retry.clone()),
        Reply::Refused(Refusal::DirectoryUnavailable)
    ));
    assert!(host.organization.as_ref() == Some(&original));
    std::fs::create_dir(&root).unwrap();
    for connection in [[1; 32], [2; 32]] {
        host.disconnected(connection);
        assert!(matches!(
            host.request(
                [1; 32],
                connection,
                Request::Select {
                    create: false,
                    size: size(),
                    resume: None,
                    style: None
                }
            ),
            Reply::Restored { .. }
        ));
        host.tick();
        assert!(host.pane.is_none());
    }
    let Reply::Attached { lease } = host.request([1; 32], [3; 32], retry.clone()) else {
        panic!("Retry should validate the original again")
    };
    assert!(
        matches!(host.request([2; 32], [4; 32], retry), Reply::Takeover { target, .. } if target == lease.into())
    );
    host.shutdown().unwrap();
    std::fs::remove_dir(&root).unwrap();
}

#[test]
fn stale_and_missing_replacements_cannot_change_or_create_directories() {
    let mut host = host();
    let org = Organization::first(host.directory.clone()).unwrap();
    host.organization = Some(org.clone());
    let missing = host.directory.join(format!(
        "missing-repair-{:x}",
        u64::from_ne_bytes(nonce().unwrap()[..8].try_into().unwrap())
    ));
    let target = pane::directories::Target {
        boot: host.boot,
        pane: org.pane,
        original: org.startup_directory().to_owned(),
    };
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::RepairDirectory {
                target: target.clone(),
                replacement: Some(missing.clone()),
                size: size(),
                style: None
            }
        ),
        Reply::Refused(Refusal::DirectoryUnavailable)
    ));
    assert!(!missing.exists());
    let mut stale = target;
    stale.boot = [8; 32];
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::RepairDirectory {
                target: stale,
                replacement: Some(host.directory.clone()),
                size: size(),
                style: None
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(host.organization.as_ref() == Some(&org));
    assert!(host.pane.is_none());
}
