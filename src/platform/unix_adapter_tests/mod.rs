//! Native corpus oracles shared by the POSIX hosts, separate from the portable release gate.
fn require(
    condition: bool,
    field: &'static str,
    detail: impl std::fmt::Display,
) -> Result<(), String> {
    condition
        .then_some(())
        .ok_or_else(|| format!("step 1 `{field}` mismatch: {detail}"))
}
fn require_eq<T>(field: &'static str, actual: T, expected: T) -> Result<(), String>
where
    T: std::fmt::Debug + PartialEq,
{
    require(
        actual == expected,
        field,
        format!("expected {expected:?}, observed {actual:?}"),
    )
}

fn check_pty_initialization() -> Result<(), String> {
    let observation = crate::platform::unix_pty::conformance_initialization_observation();
    for expected in [
        "argv=[\"/bin/zsh\", \"-l\"]",
        "cwd=/tmp",
        "term=xterm-256color",
        "colorterm=truecolor",
        "program=ghostty",
        "spaceterm=1",
        "controlling-tty=true",
    ] {
        require(
            observation.contains(expected),
            "pty-initialization",
            format!("expected `{expected}` in `{observation}`"),
        )?;
    }
    Ok(())
}
fn check_pty_shutdown() -> Result<(), String> {
    require_eq(
        "pty-shutdown",
        crate::platform::unix_pty::conformance_shutdown_observation(),
        "first=true duplicate=true signals=1 disposition=Graceful revoked=true".to_owned(),
    )
}
#[test]
fn pty_initialization_oracle() {
    check_pty_initialization().unwrap();
}
#[test]
fn pty_shutdown_oracle() {
    check_pty_shutdown().unwrap();
}

pub(crate) fn local_filesystem() -> crate::platform::local_filesystem::LocalFilesystemAuthority {
    crate::platform::local_filesystem::LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        std::sync::Arc::new(super::unix_local_identity::UnixLocalIdentity),
    )
}

mod selected_file;
