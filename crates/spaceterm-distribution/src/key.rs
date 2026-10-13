use base64::{Engine as _, engine::general_purpose::STANDARD};

/// The Ed25519 public key installed releases trust.
#[derive(Clone, Eq, PartialEq)]
pub struct UpdateKey([u8; 32]);

impl UpdateKey {
    /// The release identity's bundle template is the one source of the update trust root.
    pub fn release() -> Option<Self> {
        Self::from_property_list(include_str!(
            "../../../packaging/macos/spaceterm/Info.plist"
        ))
    }

    fn from_property_list(plist: &str) -> Option<Self> {
        let (_, after) = plist.split_once("<key>SUPublicEDKey</key>")?;
        let value = after.trim_start().strip_prefix("<string>")?;
        let (value, _) = value.split_once("</string>")?;
        Self::from_base64(value.trim())
    }

    pub fn from_base64(value: &str) -> Option<Self> {
        STANDARD.decode(value).ok()?.try_into().ok().map(Self)
    }

    pub(crate) fn verifies(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &self.0)
            .verify(message, signature)
            .is_ok()
    }
}
