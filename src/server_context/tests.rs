use super::*;
use crate::platform::{
    app_directories::AppDirectoryEnvironment,
    testing::{RecordingFilesystem, resolve_app_paths},
};

fn storage() -> (Arc<AppPaths>, Arc<RecordingFilesystem>) {
    let filesystem = Arc::new(RecordingFilesystem::default());
    let paths = resolve_app_paths(
        &AppDirectoryEnvironment {
            home: Some("/home/test".into()),
            ..Default::default()
        },
        Some("/runtime".into()),
        200,
        filesystem.clone(),
    )
    .unwrap();
    (Arc::new(paths), filesystem)
}

fn facts(at: u64, status: ObservedServerStatus) -> LastKnownFacts {
    LastKnownFacts {
        observed_at: at,
        status,
        machine: Some("Penguin".into()),
        operating_system: Some("Linux".into()),
        version: Some("0.1.0".into()),
        ..Default::default()
    }
}

#[test]
fn selection_and_last_known_facts_survive_relaunch_without_connection_authority() {
    let (paths, filesystem) = storage();
    let key = ServerKey::Ssh("penguin".into());
    let expected = LastKnownFacts {
        worktree_location: Some("/owning-server-only/{repository}/{branch}".into()),
        ..facts(20, ObservedServerStatus::Running)
    };
    {
        let mut context = ServerContext::load(paths.clone()).unwrap();
        context.select(key.clone()).unwrap();
        context.remember(key.clone(), expected.clone()).unwrap();
    }
    let context = ServerContext::load(paths).unwrap();
    assert_eq!(context.selected(), &key);
    assert_eq!(context.facts(&key), Some(&expected));
    let files = filesystem.files.lock().unwrap();
    assert_eq!(files.values.len(), 1);
    assert!(
        files
            .values
            .keys()
            .all(|path| path.file_name() == Some(OsStr::new(FILE)))
    );
}

#[test]
fn malformed_worktree_location_observations_preserve_the_prior_retained_record() {
    let (paths, filesystem) = storage();
    let key = ServerKey::Ssh("penguin".into());
    let mut context = ServerContext::load(paths.clone()).unwrap();
    let expected = LastKnownFacts {
        worktree_location: Some("~/worktrees/{repository}/{branch}".into()),
        ..facts(20, ObservedServerStatus::Running)
    };
    context.remember(key.clone(), expected.clone()).unwrap();
    let prior = filesystem.files.lock().unwrap().values.clone();
    let oversized = format!("/{}{{branch}}", "a".repeat(1024));
    for template in [
        "",
        "~/worktrees/{unknown}/{branch}",
        "~/worktrees/{repository}",
        "worktrees/{branch}",
        "~/../{branch}",
        "~/\n/{branch}",
        "~/\0/{branch}",
        &oversized,
    ] {
        let invalid = LastKnownFacts {
            observed_at: 21,
            worktree_location: Some(template.into()),
            ..expected.clone()
        };
        assert_eq!(
            context.remember(key.clone(), invalid),
            Err(ContextError::Malformed)
        );
        assert_eq!(context.facts(&key), Some(&expected));
    }
    assert_eq!(filesystem.files.lock().unwrap().values, prior);
    assert_eq!(
        ServerContext::load(paths).unwrap().facts(&key),
        Some(&expected)
    );
}

#[test]
fn a_missing_retained_worktree_location_loads_as_unknown() {
    let (paths, filesystem) = storage();
    let key = ServerKey::Ssh("penguin".into());
    let mut context = ServerContext::load(paths.clone()).unwrap();
    context
        .remember(key.clone(), facts(20, ObservedServerStatus::Running))
        .unwrap();
    {
        let mut files = filesystem.files.lock().unwrap();
        let bytes = &mut files.values.values_mut().next().unwrap().0;
        let mut document: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        document["facts"][0]["facts"]
            .as_object_mut()
            .unwrap()
            .remove("worktree_location");
        *bytes = serde_json::to_vec(&document).unwrap();
    }
    let restored = ServerContext::load(paths).unwrap();
    assert_eq!(restored.facts(&key).unwrap().worktree_location, None);
}

#[test]
fn removing_the_selected_server_falls_back_to_local_and_forgets_its_facts() {
    let (paths, _) = storage();
    let key = ServerKey::Direct([2; 32]);
    let other = ServerKey::Ssh("penguin".into());
    let mut context = ServerContext::load(paths.clone()).unwrap();
    context.select(key.clone()).unwrap();
    context
        .remember(key.clone(), facts(20, ObservedServerStatus::Running))
        .unwrap();
    context
        .remember(other.clone(), facts(10, ObservedServerStatus::Stopped))
        .unwrap();
    context.reconcile(std::slice::from_ref(&other)).unwrap();
    let restored = ServerContext::load(paths).unwrap();
    assert_eq!(restored.selected(), &ServerKey::Local);
    assert!(restored.facts(&key).is_none());
    assert_eq!(
        restored.facts(&other).unwrap().status,
        ObservedServerStatus::Stopped
    );
}

#[test]
fn a_missing_remembered_server_falls_back_after_the_saved_inventory_loads() {
    let (paths, _) = storage();
    let mut context = ServerContext::load(paths.clone()).unwrap();
    context.select(ServerKey::Ssh("removed".into())).unwrap();
    let mut restored = ServerContext::load(paths.clone()).unwrap();
    restored.reconcile(&[]).unwrap();
    assert_eq!(restored.selected(), &ServerKey::Local);
    assert_eq!(
        ServerContext::load(paths).unwrap().selected(),
        &ServerKey::Local
    );
}

#[test]
fn delayed_observations_never_replace_newer_retained_facts() {
    let (paths, _) = storage();
    let key = ServerKey::Ssh("penguin".into());
    let mut first = ServerContext::load(paths.clone()).unwrap();
    let mut delayed = ServerContext::load(paths.clone()).unwrap();
    let expected = facts(20, ObservedServerStatus::Stopped);
    first.remember(key.clone(), expected.clone()).unwrap();
    delayed
        .remember(key.clone(), facts(10, ObservedServerStatus::Running))
        .unwrap();
    assert_eq!(
        ServerContext::load(paths).unwrap().facts(&key),
        Some(&expected)
    );
}

#[test]
fn remembering_facts_does_not_overwrite_another_windows_selection() {
    let (paths, _) = storage();
    let key = ServerKey::Ssh("penguin".into());
    let mut first = ServerContext::load(paths.clone()).unwrap();
    let mut second = ServerContext::load(paths.clone()).unwrap();
    first.select(key.clone()).unwrap();
    second
        .remember(ServerKey::Local, facts(20, ObservedServerStatus::Running))
        .unwrap();
    assert_eq!(ServerContext::load(paths).unwrap().selected(), &key);
}

#[test]
fn failed_retention_still_removes_a_selected_server_from_presentation() {
    let (paths, filesystem) = storage();
    let key = ServerKey::Ssh("penguin".into());
    let mut context = ServerContext::load(paths).unwrap();
    context.select(key).unwrap();
    filesystem.files.lock().unwrap().prepare_error = Some(SecureFilesystemError::Unavailable);
    assert_eq!(context.reconcile(&[]), Err(ContextError::Unavailable));
    assert_eq!(context.selected(), &ServerKey::Local);
}

#[test]
fn malformed_storage_is_preserved_when_using_the_inert_local_fallback() {
    let (paths, filesystem) = storage();
    let mut context = ServerContext::load(paths.clone()).unwrap();
    context.select(ServerKey::Ssh("penguin".into())).unwrap();
    {
        let mut files = filesystem.files.lock().unwrap();
        files.values.values_mut().next().unwrap().0 = b"malformed".to_vec();
    }
    let mut fallback = ServerContext::new(paths);
    assert_eq!(
        fallback.select(ServerKey::Direct([3; 32])),
        Err(ContextError::Malformed)
    );
    let files = filesystem.files.lock().unwrap();
    assert_eq!(files.values.values().next().unwrap().0, b"malformed");
}
