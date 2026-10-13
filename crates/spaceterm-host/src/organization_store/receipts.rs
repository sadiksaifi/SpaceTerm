//! Exact-byte pairing receipt reader shared by initial installation and server recovery.

use spaceterm_core::{pairing::*, storage::StoreFailure};

/// Verifies retained receipts against the registered server pin. It owns no signer.
pub struct SignedReceiptReader(spaceterm_protocol::pairing_adapter::ReceiptCodec);
impl SignedReceiptReader {
    /// Binds decoding to the already validated original registration pin.
    pub fn new(pin: spaceterm_protocol::authentication::ServerPin) -> Result<Self, StoreFailure> {
        if pin.identity == [0; 16] || pin.public_key == [0; 32] {
            return Err(StoreFailure::IdentityMismatch);
        }
        Ok(Self(
            spaceterm_protocol::pairing_adapter::ReceiptCodec::new(pin),
        ))
    }
}
impl PairingReceiptValidator for SignedReceiptReader {
    fn validate_receipt(&self, receipt: &SignedPairingReceipt) -> Result<(), PairingContractError> {
        self.0.validate_receipt(receipt)
    }
}
impl super::pairing::PairingReceiptReader for SignedReceiptReader {
    fn read_receipt(
        &self,
        schema: u16,
        body: &[u8],
        signature: &[u8],
    ) -> Result<SignedPairingReceipt, StoreFailure> {
        if schema != 1 {
            return Err(StoreFailure::UnknownSchema);
        }
        if signature.len() != 64 || body.len() > MAX_SIGNED_PAIRING_RECEIPT_BYTES - 64 {
            return Err(StoreFailure::InvalidDurableState);
        }
        self.0
            .from_wire(&spaceterm_protocol::wire::pairing::SignedReceipt {
                canonical_body: body.to_vec(),
                signature: signature.to_vec(),
            })
            .map_err(|_| StoreFailure::InvalidDurableState)
    }
}
