//! Native local-socket path limits.

/// The longest local socket path, in bytes, that the host `sockaddr_un` accepts with its NUL
/// terminator: 103 on macOS and 107 on Linux.
pub(super) const LOCAL_IPC_PATH_MAXIMUM: usize = std::mem::size_of::<libc::sockaddr_un>()
    - std::mem::offset_of!(libc::sockaddr_un, sun_path)
    - 1;

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use super::*;

    #[test]
    fn local_ipc_path_maximum_matches_the_host_socket_address() {
        #[cfg(target_os = "macos")]
        assert_eq!(LOCAL_IPC_PATH_MAXIMUM, 103);
        #[cfg(target_os = "linux")]
        assert_eq!(LOCAL_IPC_PATH_MAXIMUM, 107);
    }
}
