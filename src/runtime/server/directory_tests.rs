use super::tests::{host, size};
use super::*;

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
        matches!(host.request([2; 32], [2; 32], request), Reply::Takeover { target } if target == lease.into())
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
        matches!(host.request([2; 32], [4; 32], retry), Reply::Takeover { target } if target == lease.into())
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
