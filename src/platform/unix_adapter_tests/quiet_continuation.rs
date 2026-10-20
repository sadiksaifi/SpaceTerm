use super::*;
#[test]
fn dependency_changes_pause_the_original_route() {
    let path = std::env::temp_dir().join(format!("st-proof-{}", std::process::id()));
    std::fs::write(&path, b"original").unwrap();
    let proof = Dependency::read(
        &crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem,
        path.clone(),
    )
    .unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    assert!(
        !proof.unchanged(&crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem)
    );
    std::fs::remove_file(path).unwrap();
}
