//! Whether `security` can read a Keychain item without asking, judged in-process.
//!
//! Reading an item's access shows no UI. An item that an older Preflight build created through
//! the Security framework trusts only that build's cdhash, so `security` would ask for it.

use std::ffi::c_void;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, OSStatus, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::{CFData, CFDataRef};
use core_foundation::dictionary::CFDictionary;
use core_foundation::propertylist::{create_with_data, kCFPropertyListImmutable};
use core_foundation::string::{CFString, CFStringRef};
use security_framework::os::macos::keychain::SecKeychain;
use security_framework_sys::base::{errSecItemNotFound, errSecSuccess};
use security_framework_sys::item::{
    kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecMatchSearchList,
    kSecReturnRef, kSecUseAuthenticationUI,
};
use security_framework_sys::keychain_item::SecItemCopyMatching;

use super::super::{CredentialError, Interaction};
use super::{INTERACTION, QuietGuard, kSecUseAuthenticationUIFail};

/// The trusted application `security` records for the items it creates.
const TOOL: &[u8] = b"/usr/bin/security";
/// The partition of Apple's command-line tools, `security` among them.
const TOOL_PARTITION: &str = "apple-tool:";

unsafe extern "C" {
    // These public Security symbols are absent from security-framework-sys 2.17.
    static kSecACLAuthorizationDecrypt: CFStringRef;
    static kSecACLAuthorizationPartitionID: CFStringRef;
    fn SecKeychainItemCopyAccess(item: *const c_void, access: *mut *const c_void) -> OSStatus;
    fn SecAccessCopyACLList(access: *const c_void, list: *mut CFArrayRef) -> OSStatus;
    fn SecACLCopyAuthorizations(acl: *const c_void) -> CFArrayRef;
    fn SecACLCopyContents(
        acl: *const c_void,
        applications: *mut CFArrayRef,
        description: *mut CFStringRef,
        prompt: *mut u16,
    ) -> OSStatus;
    fn SecTrustedApplicationCopyData(application: *const c_void, data: *mut CFDataRef) -> OSStatus;
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ToolAccess {
    Absent,
    /// `security` reads the item without asking.
    Quiet,
    /// `security` would ask the person before it reads the item.
    Asks,
}

pub(super) fn tool_access(
    keychain: &SecKeychain,
    service: &str,
    account: &str,
) -> Result<ToolAccess, CredentialError> {
    let _lock = INTERACTION
        .lock()
        .map_err(|_| CredentialError::Unavailable)?;
    let _quiet = QuietGuard::new(Interaction::Quiet)?;
    // SAFETY: Security exports these constants as permanent, nonnull CFString objects.
    let query = unsafe {
        CFDictionary::from_CFType_pairs(&[
            (
                CFString::wrap_under_get_rule(kSecClass),
                CFString::wrap_under_get_rule(kSecClassGenericPassword).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecAttrService),
                CFString::new(service).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecAttrAccount),
                CFString::new(account).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecMatchSearchList),
                CFArray::from_CFTypes(std::slice::from_ref(keychain)).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecReturnRef),
                CFBoolean::true_value().into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecUseAuthenticationUI),
                CFString::wrap_under_get_rule(kSecUseAuthenticationUIFail).into_CFType(),
            ),
        ])
    };
    let mut item = std::ptr::null();
    // SAFETY: The query and output pointer live throughout this synchronous call.
    let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut item) };
    if status == errSecItemNotFound {
        return Ok(ToolAccess::Absent);
    }
    let item = created(status, item)?;
    let mut access = std::ptr::null();
    // SAFETY: The item is a live Keychain item reference returned by CopyMatching.
    let status = unsafe { SecKeychainItemCopyAccess(item.as_CFTypeRef(), &mut access) };
    let access = created(status, access)?;
    let mut list = std::ptr::null();
    // SAFETY: The access is a live SecAccess returned by CopyAccess.
    let status = unsafe { SecAccessCopyACLList(access.as_CFTypeRef(), &mut list) };
    let list = created(status, list.cast())?
        .downcast_into::<CFArray>()
        .ok_or(CredentialError::Unavailable)?;
    let mut decrypt = false;
    let mut partitions = None;
    for acl in list.iter() {
        let acl = *acl;
        // SAFETY: Each element of an ACL list is a live SecACL.
        let authorizations = unsafe { SecACLCopyAuthorizations(acl) };
        if authorizations.is_null() {
            return Err(CredentialError::Unavailable);
        }
        // SAFETY: A Copy function transfers ownership of the returned array.
        let authorizations = unsafe { CFArray::<CFString>::wrap_under_create_rule(authorizations) };
        // SAFETY: Security exports these constants as permanent, nonnull CFString objects.
        let (decrypts, partitioned) = unsafe {
            let has = |name: CFStringRef| {
                let name = CFString::wrap_under_get_rule(name);
                authorizations.iter().any(|entry| *entry == name)
            };
            (
                has(kSecACLAuthorizationDecrypt),
                has(kSecACLAuthorizationPartitionID),
            )
        };
        if !decrypts && !partitioned {
            continue;
        }
        let (applications, description) = contents(acl)?;
        if decrypts {
            decrypt |= match applications {
                None => true,
                Some(applications) => applications.iter().any(|application| is_tool(*application)),
            };
        }
        if partitioned {
            partitions = Some(description.ok_or(CredentialError::Unavailable)?);
        }
    }
    // An item without a partition list is not partitioned.
    let partition = match partitions {
        None => true,
        Some(description) => listed(&description.to_string())?,
    };
    Ok(if decrypt && partition {
        ToolAccess::Quiet
    } else {
        ToolAccess::Asks
    })
}

fn created(status: OSStatus, object: *const c_void) -> Result<CFType, CredentialError> {
    if status != errSecSuccess || object.is_null() {
        return Err(CredentialError::Unavailable);
    }
    // SAFETY: A successful Copy function transfers ownership of the returned object.
    Ok(unsafe { CFType::wrap_under_create_rule(object) })
}

/// The trusted applications, absent when any application is trusted, and the description.
fn contents(acl: *const c_void) -> Result<(Option<CFArray>, Option<CFString>), CredentialError> {
    let mut applications = std::ptr::null();
    let mut description = std::ptr::null();
    let mut prompt = 0;
    // SAFETY: The ACL is live, and the outputs live throughout this call.
    let status =
        unsafe { SecACLCopyContents(acl, &mut applications, &mut description, &mut prompt) };
    if status != errSecSuccess {
        return Err(CredentialError::Unavailable);
    }
    // SAFETY: CopyContents transfers ownership of each nonnull output.
    unsafe {
        Ok((
            (!applications.is_null()).then(|| CFArray::wrap_under_create_rule(applications)),
            (!description.is_null()).then(|| CFString::wrap_under_create_rule(description)),
        ))
    }
}

fn is_tool(application: *const c_void) -> bool {
    let mut data = std::ptr::null();
    // SAFETY: The application is a live SecTrustedApplication from an ACL's contents.
    if unsafe { SecTrustedApplicationCopyData(application, &mut data) } != errSecSuccess
        || data.is_null()
    {
        return false;
    }
    // SAFETY: CopyData transfers ownership of the returned data.
    let data = unsafe { CFData::wrap_under_create_rule(data) };
    let path = data.bytes();
    path.strip_suffix(&[0]).unwrap_or(path) == TOOL
}

/// Whether a partition ACL's description, a hexadecimal property list, lists `security`.
fn listed(description: &str) -> Result<bool, CredentialError> {
    let bytes = description.as_bytes();
    let (pairs, []) = bytes.as_chunks::<2>() else {
        return Err(CredentialError::Unavailable);
    };
    let digit = |c: u8| char::from(c).to_digit(16);
    let mut plist = Vec::with_capacity(pairs.len());
    for [high, low] in pairs {
        let (Some(high), Some(low)) = (digit(*high), digit(*low)) else {
            return Err(CredentialError::Unavailable);
        };
        plist.push(((high << 4) | low) as u8);
    }
    let (plist, _) = create_with_data(CFData::from_buffer(&plist), kCFPropertyListImmutable)
        .map_err(|_| CredentialError::Unavailable)?;
    // SAFETY: A successful CreateWithData transfers ownership of the property list.
    let plist = unsafe { CFType::wrap_under_create_rule(plist) };
    let partitions = plist
        .downcast_into::<CFDictionary>()
        .and_then(|plist| {
            plist
                .find(CFString::from_static_string("Partitions").as_CFTypeRef())
                // SAFETY: Dictionary values are live CF objects retained by the dictionary.
                .map(|value| unsafe { CFType::wrap_under_get_rule(*value) })
        })
        .and_then(|value| value.downcast_into::<CFArray>())
        .ok_or(CredentialError::Unavailable)?;
    Ok(partitions.iter().any(|partition| {
        // SAFETY: Array values are live CF objects retained by the array.
        unsafe { CFType::wrap_under_get_rule(*partition) }
            .downcast_into::<CFString>()
            .is_some_and(|partition| partition == TOOL_PARTITION)
    }))
}

#[cfg(test)]
mod tests {
    use super::listed;

    fn hex(text: &str) -> String {
        text.bytes().map(|byte| format!("{byte:02x}")).collect()
    }

    fn partitions(entries: &[&str]) -> String {
        let strings: String = entries
            .iter()
            .map(|entry| format!("<string>{entry}</string>"))
            .collect();
        hex(&format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>\
             <key>Partitions</key><array>{strings}</array></dict></plist>"
        ))
    }

    #[test]
    fn only_a_partition_list_naming_apple_tools_lets_security_read_quietly() {
        assert_eq!(listed(&partitions(&["apple-tool:"])), Ok(true));
        assert_eq!(
            listed(&partitions(&["cdhash:abc", "apple-tool:"])),
            Ok(true)
        );
        // An older Preflight build's item, as observed in the login Keychain.
        assert_eq!(
            listed(&partitions(&[
                "cdhash:e58280ac479d9d2cde3688c377fe68a6b11b83cf"
            ])),
            Ok(false)
        );
        assert_eq!(listed(&partitions(&[])), Ok(false));
    }

    #[test]
    fn unreadable_partition_descriptions_are_unavailable() {
        for description in ["", "0", "zz", &hex("not a property list")] {
            assert!(listed(description).is_err());
        }
    }
}
