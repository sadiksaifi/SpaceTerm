//! Native Adapter integration evidence.
use super::*;
use std::os::unix::ffi::OsStringExt;

#[test]
fn capture_should_preserve_raw_non_utf8_agent_socket_bytes() {
    let raw = OsString::from_vec(vec![b'/', b'p', b'r', b'i', b'v', b'a', b't', b'e', 0xff]);

    let captured = StartupSshEnvironment::from_environment(
        |key| (key == OsStr::new(SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE)).then(|| raw.clone()),
        FALLBACK_PATH.into(),
    )
    .unwrap();

    assert_eq!(captured.agent_socket(), Some(raw.as_os_str()));
}
