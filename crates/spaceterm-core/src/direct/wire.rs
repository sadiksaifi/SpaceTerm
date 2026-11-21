use super::DirectError;
pub(super) use spaceterm_protocol::direct::wire::*;

pub(super) fn now() -> Result<u64, DirectError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| DirectError::StorageUnavailable)
}
