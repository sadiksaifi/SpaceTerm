//! Native evidence that a Worktree's launches keep a Pinned Directory or a shell directory
//! reached through a symbolic link, although git names the Worktree root by its physical path.
use std::fs;
use std::os::unix::fs::symlink;
use std::rc::Rc;

use super::{local_filesystem, short_temporary_root};
use crate::domain::PinnedDirectory;
use crate::terminal::WorkspaceTerminalSessionFactory;
use crate::terminal::metadata::CurrentDirectory;
use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};

#[test]
fn unix_worktree_launches_should_follow_links_into_the_worktree() {
    let root =
        short_temporary_root().join(format!("spaceterm-worktree-launch-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("repository");
    let inside = worktree.join("sub");
    let outside = root.join("elsewhere");
    let alias = root.join("alias");
    fs::create_dir_all(&inside).unwrap();
    fs::create_dir_all(&outside).unwrap();
    symlink(&worktree, &alias).unwrap();
    let linked = alias.join("sub");
    let authority = local_filesystem();
    let mut factory = WorkspaceTerminalSessionFactory::new_local_with_authority(
        Rc::new(TestTerminalSessionFactory::new(
            TestTerminalSessionRecords::default(),
        )),
        authority.validate_directory(&root).unwrap(),
        authority.clone(),
    );
    factory.set_worktree_root(Some(worktree.clone()));
    let selected = |factory: &WorkspaceTerminalSessionFactory, source: &std::path::Path| {
        factory
            .for_source_directory(Some(CurrentDirectory::Local(source.to_path_buf())))
            .unwrap()
            .local_working_directory()
            .map(std::path::Path::to_path_buf)
    };

    let from_linked_shell = selected(&factory, &linked);
    factory.set_pinned_directory(Some(PinnedDirectory::Local(
        authority.validate_directory(&linked).unwrap(),
    )));
    let from_linked_pin = selected(&factory, &outside);
    let _ = fs::remove_dir_all(&root);

    assert_eq!(
        (from_linked_shell, from_linked_pin),
        (Some(linked.clone()), Some(linked)),
        "a directory reached through a link into the Worktree stays its starting directory"
    );
}

#[test]
fn unix_worktree_launches_should_reject_a_symbolic_link_to_an_outside_directory() {
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    let home = fixture.path();
    let worktree = home.join("shell-integration");
    let escape = worktree.join("shared");
    symlink(home, &escape).unwrap();
    let authority = local_filesystem();
    let mut factory = WorkspaceTerminalSessionFactory::new_local_with_authority(
        Rc::new(TestTerminalSessionFactory::new(
            TestTerminalSessionRecords::default(),
        )),
        authority.validate_directory(home).unwrap(),
        authority.clone(),
    );
    factory.set_worktree_root(Some(worktree.clone()));

    let source = factory
        .for_source_directory(Some(CurrentDirectory::Local(escape.clone())))
        .unwrap();
    assert_eq!(source.local_working_directory(), Some(worktree.as_path()));

    factory.set_pinned_directory(Some(PinnedDirectory::Local(
        authority.validate_directory(&escape).unwrap(),
    )));
    let pinned = factory.for_source_directory(None).unwrap();
    assert_eq!(pinned.local_working_directory(), Some(worktree.as_path()));
}
