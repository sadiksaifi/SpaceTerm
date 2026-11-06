//! Bound physical tar records before tar interprets or allocates extension metadata.
use super::*;

const EXTENSION_BYTES: u64 = 4096;
const PHYSICAL_RECORDS: usize = 256;

pub(super) fn decoder(bytes: &[u8]) -> std::io::Take<flate2::read::MultiGzDecoder<&[u8]>> {
    flate2::read::MultiGzDecoder::new(bytes).take(MAX_EXPANDED + 1)
}

pub(super) fn validate(bytes: &[u8]) -> Result<(), CoreError> {
    let mut archive = tar::Archive::new(decoder(bytes));
    let mut physical = 0u64;
    let mut logical = 0;
    for (count, entry) in archive
        .entries()
        .map_err(|_| CoreError::RuntimeUnavailable)?
        .raw(true)
        .enumerate()
    {
        let mut entry = entry.map_err(|_| CoreError::RuntimeUnavailable)?;
        let size = entry.size();
        physical = physical
            .checked_add(512)
            .and_then(|n| {
                size.checked_add(511)
                    .and_then(|size| n.checked_add(size / 512 * 512))
            })
            .ok_or(CoreError::RuntimeUnavailable)?;
        if count >= PHYSICAL_RECORDS || physical > MAX_EXPANDED {
            return Err(CoreError::RuntimeUnavailable);
        }
        let kind = entry.header().entry_type();
        let extension = kind.is_gnu_longname()
            || kind.is_gnu_longlink()
            || kind.is_pax_local_extensions()
            || kind.is_pax_global_extensions();
        if extension {
            if size > EXTENSION_BYTES {
                return Err(CoreError::RuntimeUnavailable);
            }
            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .map_err(|_| CoreError::RuntimeUnavailable)?;
            if data.len() as u64 != size {
                return Err(CoreError::RuntimeUnavailable);
            }
            if kind.is_pax_local_extensions() || kind.is_pax_global_extensions() {
                for field in tar::PaxExtensions::new(&data) {
                    let field = field.map_err(|_| CoreError::RuntimeUnavailable)?;
                    if !matches!(field.key(), Ok("path" | "mtime" | "atime" | "ctime")) {
                        return Err(CoreError::RuntimeUnavailable);
                    }
                }
            }
        } else {
            logical += 1;
            if logical > 128 || !(kind.is_file() || kind.is_dir()) {
                return Err(CoreError::RuntimeUnavailable);
            }
            if std::io::copy(&mut entry, &mut std::io::sink())
                .map_err(|_| CoreError::RuntimeUnavailable)?
                != size
            {
                return Err(CoreError::RuntimeUnavailable);
            }
        }
    }
    let mut reader = archive.into_inner();
    std::io::copy(&mut reader, &mut std::io::sink()).map_err(|_| CoreError::RuntimeUnavailable)?;
    if reader.limit() == 0 {
        return Err(CoreError::RuntimeUnavailable);
    }
    Ok(())
}
