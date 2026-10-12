#![cfg(any(target_os = "macos", target_os = "linux"))]

use spaceterm_core::authority::ServerEpoch;
use spaceterm_core::identity::*;
use spaceterm_core::organization::ServerDirectory;
use spaceterm_core::server_directories::*;
use spaceterm_host::server_directories::native::NativeDirectoryProvider;
use std::os::unix::fs::symlink;

fn server() -> ServerId {
    ServerId::new([4; 16])
}
fn directory(path: &std::path::Path) -> ServerDirectory {
    ServerDirectory::new(server(), path.as_os_str().as_encoded_bytes().to_vec()).unwrap()
}
fn context(client: u64) -> DirectoryContext {
    DirectoryContext::new(DirectoryContextParts {
        server: server(),
        epoch: ServerEpoch::new(1),
        pairing: PairingId::new(server(), 1).unwrap(),
        client: ClientId::new(server(), client).unwrap(),
    })
    .unwrap()
}
fn browse(
    context: DirectoryContext,
    path: &std::path::Path,
    cursor: Option<DirectoryCursor>,
) -> DirectoryQuery {
    DirectoryQuery::new(DirectoryQueryParts {
        context,
        action: DirectoryQueryAction::Browse {
            directory: directory(path),
            cursor,
        },
    })
    .unwrap()
}

#[test]
fn browse_pages_bind_principal_epoch_scope_and_observation() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..140 {
        std::fs::create_dir(root.path().join(format!("dir-{index:03}"))).unwrap();
    }
    let provider = NativeDirectoryProvider::new(server(), root.path().to_path_buf()).unwrap();
    let first = provider
        .query(&browse(context(1), root.path(), None))
        .unwrap();
    let DirectoryQueryResult::Page(first) = first else {
        panic!("expected directory page")
    };
    assert_eq!(first.parts().entries.len(), 128);
    let next = first.parts().next.clone().unwrap();
    let mut forged = next.parts().clone();
    forged.context = context(2);
    let forged = DirectoryCursor::new(forged).unwrap();
    assert_eq!(
        provider.query(&browse(context(2), root.path(), Some(forged))),
        Err(DirectoryFailure::StaleCursor)
    );
    let DirectoryQueryResult::Page(second) = provider
        .query(&browse(context(1), root.path(), Some(next)))
        .unwrap()
    else {
        panic!("expected directory page")
    };
    assert_eq!(second.parts().entries.len(), 12);
    assert!(second.complete());
    assert_eq!(first.parts().observation, second.parts().observation);
}

#[test]
fn replaced_directory_invalidates_browse_cursor() {
    let root = tempfile::tempdir().unwrap();
    let scope = root.path().join("scope");
    std::fs::create_dir(&scope).unwrap();
    for index in 0..130 {
        std::fs::create_dir(scope.join(format!("d{index}"))).unwrap();
    }
    let provider = NativeDirectoryProvider::new(server(), root.path().to_path_buf()).unwrap();
    let DirectoryQueryResult::Page(page) =
        provider.query(&browse(context(1), &scope, None)).unwrap()
    else {
        panic!("expected page")
    };
    std::fs::rename(&scope, root.path().join("old")).unwrap();
    std::fs::create_dir(&scope).unwrap();
    assert_eq!(
        provider.query(&browse(context(1), &scope, page.parts().next.clone())),
        Err(DirectoryFailure::StaleCursor)
    );
}

#[test]
fn physical_alias_pin_is_inside_and_missing_is_unchecked() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let child = target.join("child");
    std::fs::create_dir(&child).unwrap();
    let alias = root.path().join("alias");
    symlink(&child, &alias).unwrap();
    let provider = NativeDirectoryProvider::new(server(), root.path().to_path_buf()).unwrap();
    assert_eq!(
        provider.containment(&directory(&target), &directory(&alias)),
        Ok(DirectoryContainment::Inside)
    );
    assert_eq!(
        provider.containment(&directory(&target), &directory(&root.path().join("gone"))),
        Ok(DirectoryContainment::Unchecked)
    );
}

#[test]
fn create_target_rejects_links_occupied_directory_and_replacement() {
    let root = tempfile::tempdir().unwrap();
    let provider = NativeDirectoryProvider::new(server(), root.path().to_path_buf()).unwrap();
    let target = root.path().join("new");
    let lease = provider.retain_create_target(&directory(&target)).unwrap();
    std::fs::create_dir(&target).unwrap();
    assert_eq!(lease.revalidate(), Err(DirectoryFailure::IdentityChanged));
    symlink(root.path().join("gone"), root.path().join("link")).unwrap();
    assert!(
        provider
            .retain_create_target(&directory(&root.path().join("link")))
            .is_err()
    );
    std::fs::write(target.join("file"), b"occupied").unwrap();
    assert_eq!(
        provider
            .validate(&directory(&target), DirectoryPurpose::WorktreeCreateTarget)
            .unwrap()
            .class,
        DirectoryValidationClass::Unchecked
    );
}

#[test]
fn home_is_owning_server_home_and_errors_are_content_free() {
    let root = tempfile::tempdir().unwrap();
    let provider = NativeDirectoryProvider::new(server(), root.path().to_path_buf()).unwrap();
    let query = DirectoryQuery::new(DirectoryQueryParts {
        context: context(1),
        action: DirectoryQueryAction::Home,
    })
    .unwrap();
    assert_eq!(
        provider.query(&query),
        Ok(DirectoryQueryResult::Home(directory(root.path())))
    );
    let bad = ServerDirectory::new(server(), b"relative/private-path".to_vec()).unwrap();
    assert_eq!(
        provider.validate(&bad, DirectoryPurpose::PaneRepair),
        Err(DirectoryFailure::InvalidDirectory)
    );
    assert!(!format!("{bad:?}").contains("private-path"));
}
