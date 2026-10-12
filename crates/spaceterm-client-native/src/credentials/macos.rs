//! Login Keychain storage through Security.framework, without desktop linkage.
//! Background reads/deletion never show authorization UI. Deliberate enrollment
//! and additional-Client actions permit native Keychain authorization of the
//! actual signed executable. Security retains its code-identity access rules;
//! sharing a locator does not authorize a different executable automatically.

use super::adapter::{DeviceCredentials, NativeKeyStore};
use spaceterm_client::pairing::credentials::{
    CredentialScope, NativeCredentialAccess, PairingFuture,
};
use spaceterm_core::pairing::PairingFailure;
use std::{ffi::c_void, ptr};
use zeroize::Zeroizing;

type CfRef = *const c_void;
pub(super) struct Keychain;
const SERVICE: &[u8] = b"SpaceTerm native client credentials v1";
const NOT_FOUND: i32 = -25300;
const DUPLICATE: i32 = -25299;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: CfRef;
    static kSecClassGenericPassword: CfRef;
    static kSecAttrService: CfRef;
    static kSecAttrAccount: CfRef;
    static kSecMatchLimit: CfRef;
    static kSecMatchLimitOne: CfRef;
    static kSecMatchItemList: CfRef;
    static kSecReturnData: CfRef;
    static kSecReturnRef: CfRef;
    static kSecValueData: CfRef;
    static kSecValueRef: CfRef;
    static kSecUseAuthenticationUI: CfRef;
    static kSecUseAuthenticationUIFail: CfRef;
    static kSecUseAuthenticationUIAllow: CfRef;
    fn SecItemCopyMatching(query: CfRef, result: *mut CfRef) -> i32;
    fn SecItemDelete(query: CfRef) -> i32;
    fn SecKeychainAddGenericPassword(
        keychain: CfRef,
        service_len: u32,
        service: *const u8,
        account_len: u32,
        account: *const u8,
        password_len: u32,
        password: CfRef,
        item: *mut CfRef,
    ) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CfRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFTypeArrayCallBacks: u8;
    fn CFRelease(object: CfRef);
    fn CFGetTypeID(object: CfRef) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDictionaryCreate(
        allocator: CfRef,
        keys: *const CfRef,
        values: *const CfRef,
        count: isize,
        key_callbacks: CfRef,
        value_callbacks: CfRef,
    ) -> CfRef;
    fn CFDictionaryGetValue(dictionary: CfRef, key: CfRef) -> CfRef;
    fn CFStringCreateWithBytes(
        allocator: CfRef,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: bool,
    ) -> CfRef;
    fn CFDataGetLength(data: CfRef) -> isize;
    fn CFDataGetBytePtr(data: CfRef) -> *const u8;
    fn CFArrayCreate(
        allocator: CfRef,
        values: *const CfRef,
        count: isize,
        callbacks: CfRef,
    ) -> CfRef;
}

struct Object(CfRef);
impl Object {
    fn new(value: CfRef) -> Result<Self, PairingFailure> {
        if value.is_null() {
            return Err(PairingFailure::CredentialUnavailable);
        }
        Ok(Self(value))
    }
    fn text(bytes: &[u8]) -> Result<Self, PairingFailure> {
        // SAFETY: the live bounded ASCII buffer is valid UTF-8. CoreFoundation
        // copies it and returns one owned reference.
        Self::new(unsafe {
            CFStringCreateWithBytes(
                ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                0x08000100,
                false,
            )
        })
    }
    fn dictionary(entries: &[(CfRef, CfRef)]) -> Result<Self, PairingFailure> {
        let (keys, values): (Vec<_>, Vec<_>) = entries.iter().copied().unzip();
        // SAFETY: keys/values are live CF references. Native callbacks retain
        // them for the dictionary and release them with its owned reference.
        Self::new(unsafe {
            CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                entries.len() as isize,
                ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
                ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
            )
        })
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}

struct Item {
    result: Object,
    data: CfRef,
    item: CfRef,
}
fn find(account: &str, access: NativeCredentialAccess) -> Result<Option<Item>, PairingFailure> {
    let service = Object::text(SERVICE)?;
    let account = Object::text(account.as_bytes())?;
    // Per-query UI policy avoids mutating Security's process-global interaction
    // setting, which would interfere with unrelated desktop credential use.
    let query = Object::dictionary(&unsafe {
        [
            (kSecClass, kSecClassGenericPassword),
            (kSecAttrService, service.0),
            (kSecAttrAccount, account.0),
            (kSecMatchLimit, kSecMatchLimitOne),
            (
                kSecUseAuthenticationUI,
                match access {
                    NativeCredentialAccess::Noninteractive => kSecUseAuthenticationUIFail,
                    NativeCredentialAccess::Foreground => kSecUseAuthenticationUIAllow,
                },
            ),
            (kSecReturnData, kCFBooleanTrue),
            (kSecReturnRef, kCFBooleanTrue),
        ]
    })?;
    let mut result = ptr::null();
    let status = unsafe { SecItemCopyMatching(query.0, &mut result) };
    if status == NOT_FOUND {
        return Ok(None);
    }
    if status != 0 {
        return Err(PairingFailure::CredentialUnavailable);
    }
    let result = Object::new(result)?;
    // SAFETY: CopyMatching owns this returned native object. Type and lengths
    // are checked before interpreting its dictionary/data pointers.
    let (data, item) = unsafe {
        if CFGetTypeID(result.0) != CFDictionaryGetTypeID() {
            return Err(PairingFailure::CredentialIntegrity);
        }
        (
            CFDictionaryGetValue(result.0, kSecValueData),
            CFDictionaryGetValue(result.0, kSecValueRef),
        )
    };
    if data.is_null()
        || item.is_null()
        || unsafe {
            CFGetTypeID(data) != CFDataGetTypeID()
                || CFDataGetLength(data) != 65
                || CFDataGetBytePtr(data).is_null()
        }
    {
        return Err(PairingFailure::CredentialIntegrity);
    }
    Ok(Some(Item { result, data, item }))
}
fn copy(item: &Item) -> Zeroizing<Vec<u8>> {
    // Native copies are Security-owned. The application-owned copy is zeroizing.
    // Item retains the containing native dictionary throughout this access.
    let _owner = &item.result;
    Zeroizing::new(unsafe { std::slice::from_raw_parts(CFDataGetBytePtr(item.data), 65) }.to_vec())
}
fn delete(item: &Item) -> Result<bool, PairingFailure> {
    // Retain the exact native item in a one-item search list. A replaced slot
    // cannot redirect this deletion to a newer native object.
    let list = Object::new(unsafe {
        CFArrayCreate(
            ptr::null(),
            &item.item,
            1,
            ptr::addr_of!(kCFTypeArrayCallBacks).cast(),
        )
    })?;
    let query = Object::dictionary(&unsafe {
        [
            (kSecClass, kSecClassGenericPassword),
            (kSecMatchItemList, list.0),
            (kSecUseAuthenticationUI, kSecUseAuthenticationUIFail),
        ]
    })?;
    match unsafe { SecItemDelete(query.0) } {
        0 => Ok(true),
        NOT_FOUND => Ok(false),
        _ => Err(PairingFailure::CredentialUnavailable),
    }
}
impl NativeKeyStore for Keychain {
    fn read<'a>(
        &'a self,
        scope: &'a CredentialScope,
        access: NativeCredentialAccess,
    ) -> PairingFuture<'a, Result<Option<Zeroizing<Vec<u8>>>, PairingFailure>> {
        let account = DeviceCredentials::locator(scope);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                find(&account, access).map(|item| item.map(|item| copy(&item)))
            })
            .await
            .map_err(|_| PairingFailure::CredentialUnavailable)?
        })
    }
    fn insert<'a>(
        &'a self,
        scope: &'a CredentialScope,
        secret: &'a [u8],
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        let account = DeviceCredentials::locator(scope);
        let secret = Zeroizing::new(secret.to_vec());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                // Explicit foreground creation never updates an existing item.
                let status = unsafe {
                    SecKeychainAddGenericPassword(
                        ptr::null(),
                        SERVICE.len() as u32,
                        SERVICE.as_ptr(),
                        account.len() as u32,
                        account.as_ptr(),
                        secret.len() as u32,
                        secret.as_ptr().cast(),
                        ptr::null_mut(),
                    )
                };
                match status {
                    0 => Ok(()),
                    DUPLICATE => Err(PairingFailure::CredentialIntegrity),
                    _ => Err(PairingFailure::CredentialUnavailable),
                }
            })
            .await
            .map_err(|_| PairingFailure::CredentialUnavailable)?
        })
    }
    fn remove<'a>(
        &'a self,
        scope: &'a CredentialScope,
        expected: &'a [u8],
    ) -> PairingFuture<'a, Result<bool, PairingFailure>> {
        let account = DeviceCredentials::locator(scope);
        let expected = Zeroizing::new(expected.to_vec());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let Some(item) = find(&account, NativeCredentialAccess::Noninteractive)? else {
                    return Ok(false);
                };
                if copy(&item).as_slice() != expected.as_slice() {
                    return Err(PairingFailure::CredentialIntegrity);
                }
                delete(&item)
            })
            .await
            .map_err(|_| PairingFailure::CredentialUnavailable)?
        })
    }
}
