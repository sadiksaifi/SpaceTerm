use std::sync::Mutex;

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use security_framework::item::{ItemAddOptions, ItemAddValue, ItemClass, Location};
use security_framework_sys::base::{errSecItemNotFound, errSecSuccess as ERR_SEC_SUCCESS};
use security_framework_sys::item::{
    kSecAttrAccount, kSecAttrService, kSecAttrSynchronizable, kSecClass, kSecClassGenericPassword,
    kSecReturnData, kSecUseAuthenticationUI,
};
use security_framework_sys::keychain::{
    SecKeychainGetUserInteractionAllowed, SecKeychainSetUserInteractionAllowed,
};
use security_framework_sys::keychain_item::{SecItemCopyMatching, SecItemDelete};
use zeroize::Zeroizing;

use super::{CredentialError, CredentialStore, Interaction, account};

mod security_tool;
mod tool_access;
pub(super) use security_tool::SecurityTool;

unsafe extern "C" {
    // This public Security constant is absent from security-framework-sys 2.17.
    static kSecUseAuthenticationUIFail: CFStringRef;
}

// The file-based Keychain's interaction switch is process-wide. Serialize our Requested and
// Quiet operations, and restore the previous setting before releasing this lock.
static INTERACTION: Mutex<()> = Mutex::new(());

pub(super) struct Keychain {
    service: String,
}

impl Keychain {
    pub(super) fn new(identity: &str) -> Self {
        Self {
            service: service(identity),
        }
    }

    fn query(&self, key: &[u8; 32], interaction: Interaction) -> Vec<(CFString, CFType)> {
        // SAFETY: Security exports these constants as permanent, nonnull CFString objects.
        unsafe {
            let mut pairs = vec![
                (
                    CFString::wrap_under_get_rule(kSecClass),
                    CFString::wrap_under_get_rule(kSecClassGenericPassword).into_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrService),
                    CFString::new(&self.service).into_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrAccount),
                    CFString::new(&account(key)).into_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrSynchronizable),
                    CFBoolean::false_value().into_CFType(),
                ),
            ];
            if interaction == Interaction::Quiet {
                pairs.push((
                    CFString::wrap_under_get_rule(kSecUseAuthenticationUI),
                    CFString::wrap_under_get_rule(kSecUseAuthenticationUIFail).into_CFType(),
                ));
            }
            pairs
        }
    }
}

impl CredentialStore for Keychain {
    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
        let _lock = INTERACTION
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        let _quiet = QuietGuard::new(interaction)?;
        let mut pairs = self.query(key, interaction);
        // SAFETY: The key is a permanent CFString, and the dictionary retains the value.
        pairs.push(unsafe {
            (
                CFString::wrap_under_get_rule(kSecReturnData),
                CFBoolean::true_value().into_CFType(),
            )
        });
        let query = CFDictionary::from_CFType_pairs(&pairs);
        let mut value = std::ptr::null();
        // SAFETY: The query and output pointer live throughout this synchronous call.
        let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut value) };
        if status == errSecItemNotFound {
            return Ok(None);
        }
        classify(status)?;
        if value.is_null() {
            return Err(CredentialError::Unavailable);
        }
        // SAFETY: A successful CopyMatching transfers ownership of the returned CF object.
        let value = unsafe { CFType::wrap_under_create_rule(value) };
        let data = value
            .downcast_into::<CFData>()
            .ok_or(CredentialError::Unavailable)?;
        Ok(Some(Zeroizing::new(data.bytes().to_vec())))
    }

    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
        let _lock = INTERACTION
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        let mut item = ItemAddOptions::new(ItemAddValue::Data {
            class: ItemClass::generic_password(),
            data: CFData::from_buffer(secret),
        });
        item.set_service(&self.service)
            .set_account_name(account(key))
            .set_location(Location::DefaultFileKeychain);
        // ItemAddOptions calls SecItemAdd without updating duplicate items.
        // Never use set_generic_password: it replaces a duplicate item's secret.
        item.add().map_err(|error| {
            classify(error.code())
                .err()
                .unwrap_or(CredentialError::Unavailable)
        })
    }

    fn remove(&self, key: &[u8; 32], interaction: Interaction) -> Result<(), CredentialError> {
        let _lock = INTERACTION
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        let _quiet = QuietGuard::new(interaction)?;
        let query = CFDictionary::from_CFType_pairs(&self.query(key, interaction));
        // SAFETY: The dictionary lives throughout this synchronous delete.
        let status = unsafe { SecItemDelete(query.as_concrete_TypeRef()) };
        if status == errSecItemNotFound {
            return Ok(());
        }
        classify(status)
    }
}

struct QuietGuard(Option<u8>);

impl QuietGuard {
    fn new(interaction: Interaction) -> Result<Self, CredentialError> {
        if interaction == Interaction::Requested {
            return Ok(Self(None));
        }
        let mut previous = 0;
        // SAFETY: The output points to a live Boolean, and INTERACTION serializes changes.
        classify(unsafe { SecKeychainGetUserInteractionAllowed(&mut previous) })?;
        // kSecUseAuthenticationUIFail alone does not suppress legacy file-Keychain UI.
        // SAFETY: This API accepts a Boolean and affects only this process.
        classify(unsafe { SecKeychainSetUserInteractionAllowed(0) })?;
        Ok(Self(Some(previous)))
    }
}

impl Drop for QuietGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            // SAFETY: INTERACTION is held until after this guard restores the saved Boolean.
            let _ = unsafe { SecKeychainSetUserInteractionAllowed(previous) };
        }
    }
}

fn service(identity: &str) -> String {
    format!("io.github.sadiksaifi.{identity}.direct-network")
}

fn classify(status: i32) -> Result<(), CredentialError> {
    match status {
        ERR_SEC_SUCCESS => Ok(()),
        // Authentication failed, interaction forbidden/required, or the user cancelled.
        -25293 | -25308 | -25315 | -128 => Err(CredentialError::InteractionRequired),
        _ => Err(CredentialError::Unavailable),
    }
}
