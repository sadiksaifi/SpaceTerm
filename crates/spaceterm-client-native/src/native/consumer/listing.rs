//! Authenticated server pairing inventory. The local journal selects the paired
//! connection; it is not the server's device inventory.

use super::{NativeConsumer, classify_client};
use crate::cli::bindings::{InventoryCursor, PairingInventoryPage, PublicPairedDevice};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use spaceterm_client::management_cli::ManagementFailure;
use spaceterm_core::{
    authority::FieldVersion,
    identity::{PairingId, ServerId},
};

// The cursor contains only a format tag and public comparison facts. The server
// revalidates current authorization and the original revision on every page.
const CURSOR_SCHEMA: u8 = 1;
const CURSOR_BYTES: usize = 1 + 16 + 8 + 8;

impl NativeConsumer {
    pub(super) async fn list_registered_devices(
        &self,
        cursor: Option<InventoryCursor>,
    ) -> Result<PairingInventoryPage, ManagementFailure> {
        self.mutable()?;
        let opened = self.opened().await?;
        let server = opened.server.identity();
        let (after, version) = match cursor {
            Some(cursor) => {
                let (after, version) = decode_cursor(&cursor, server)?;
                (Some(after), Some(version))
            }
            None => (None, None),
        };
        let queries = opened
            .server
            .queries(opened.pairing.journal().clone())
            .map_err(classify_client)?;
        let page = queries.paired_devices(after, version).await?;
        let next = page
            .next
            .map(|after| encode_cursor(after, page.version))
            .transpose()?;
        Ok(PairingInventoryPage {
            devices: page
                .devices
                .into_iter()
                .map(|target| {
                    let parts = target.parts();
                    PublicPairedDevice {
                        server: parts.server,
                        device: parts.device,
                        pairing: parts.pairing,
                    }
                })
                .collect(),
            next,
        })
    }
}

fn encode_cursor(
    after: PairingId,
    version: FieldVersion,
) -> Result<InventoryCursor, ManagementFailure> {
    let mut bytes = [0; CURSOR_BYTES];
    bytes[0] = CURSOR_SCHEMA;
    bytes[1..17].copy_from_slice(&after.server().bytes());
    bytes[17..25].copy_from_slice(&version.get().to_be_bytes());
    bytes[25..].copy_from_slice(&after.local().to_be_bytes());
    InventoryCursor::new(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(
    cursor: &InventoryCursor,
    server: ServerId,
) -> Result<(PairingId, FieldVersion), ManagementFailure> {
    let mut bytes = [0; CURSOR_BYTES];
    let length = URL_SAFE_NO_PAD
        .decode_slice(cursor.as_str(), &mut bytes)
        .map_err(|_| ManagementFailure::Usage)?;
    if length != CURSOR_BYTES || bytes[0] != CURSOR_SCHEMA || bytes[1..17] != server.bytes() {
        return Err(ManagementFailure::Usage);
    }
    let version = u64::from_be_bytes(
        bytes[17..25]
            .try_into()
            .map_err(|_| ManagementFailure::Usage)?,
    );
    if version == 0 {
        return Err(ManagementFailure::Usage);
    }
    let local = u64::from_be_bytes(
        bytes[25..]
            .try_into()
            .map_err(|_| ManagementFailure::Usage)?,
    );
    let after = PairingId::new(server, local).map_err(|_| ManagementFailure::Usage)?;
    Ok((after, FieldVersion::new(version)))
}
