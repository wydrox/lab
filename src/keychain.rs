use crate::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const ERR_DUPLICATE_ITEM: i32 = -25299;
const ERR_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::c_void;
    use std::ptr;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecKeychainAddGenericPassword(
            keychain: *mut c_void,
            service_name_length: u32,
            service_name: *const u8,
            account_name_length: u32,
            account_name: *const u8,
            password_length: u32,
            password_data: *const u8,
            item_ref: *mut *mut c_void,
        ) -> i32;
        fn SecKeychainFindGenericPassword(
            keychain_or_array: *mut c_void,
            service_name_length: u32,
            service_name: *const u8,
            account_name_length: u32,
            account_name: *const u8,
            password_length: *mut u32,
            password_data: *mut *mut u8,
            item_ref: *mut *mut c_void,
        ) -> i32;
        fn SecKeychainItemModifyContent(
            item_ref: *mut c_void,
            attr_list: *const c_void,
            length: u32,
            data: *const u8,
        ) -> i32;
        fn SecKeychainItemFreeContent(attr_list: *mut c_void, data: *mut c_void) -> i32;
        #[cfg(test)]
        fn SecKeychainItemDelete(item_ref: *mut c_void) -> i32;
        fn SecKeychainItemSetAccess(item_ref: *mut c_void, access: *mut c_void) -> i32;
        fn SecAccessCreate(
            descriptor: *mut c_void,
            trusted_list: *mut c_void,
            access: *mut *mut c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *mut c_void);
        fn CFStringCreateWithCString(
            alloc: *mut c_void,
            c_str: *const i8,
            encoding: u32,
        ) -> *mut c_void;
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    fn open_access() -> Option<*mut c_void> {
        let Ok(desc) = std::ffi::CString::new("lab-cli") else {
            return None;
        };
        let cf = unsafe {
            CFStringCreateWithCString(ptr::null_mut(), desc.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };
        if cf.is_null() {
            return None;
        }
        let mut access = ptr::null_mut::<c_void>();
        let status = unsafe { SecAccessCreate(cf, ptr::null_mut(), &mut access) };
        unsafe { CFRelease(cf) };
        if status != 0 || access.is_null() {
            None
        } else {
            Some(access)
        }
    }

    fn relax_item_access(item: *mut c_void) {
        if item.is_null() || cfg!(test) {
            return;
        }
        let Some(access) = open_access() else {
            return;
        };
        let _ = unsafe { SecKeychainItemSetAccess(item, access) };
        unsafe { CFRelease(access) };
    }

    fn u32_len(bytes: &[u8], what: &str) -> Result<u32> {
        u32::try_from(bytes.len()).map_err(|_| anyhow!("{what} przekracza limit Keychain"))
    }

    pub(super) fn get(service: &str, account: &str) -> Result<Option<String>> {
        let service = service.as_bytes();
        let account = account.as_bytes();
        let mut length = 0u32;
        let mut data = ptr::null_mut::<u8>();
        let mut item = ptr::null_mut::<c_void>();
        let status = unsafe {
            SecKeychainFindGenericPassword(
                ptr::null_mut(),
                u32_len(service, "usługa")?,
                service.as_ptr(),
                u32_len(account, "konto")?,
                account.as_ptr(),
                &mut length,
                &mut data,
                &mut item,
            )
        };
        if status == ERR_ITEM_NOT_FOUND {
            return Ok(None);
        }
        if status != 0 {
            return Err(anyhow!("Keychain: odczyt status {status}"));
        }
        let bytes = unsafe { std::slice::from_raw_parts(data, length as usize) }.to_vec();
        unsafe {
            let _ = SecKeychainItemFreeContent(ptr::null_mut(), data.cast());
        }
        relax_item_access(item);
        if !item.is_null() {
            unsafe { CFRelease(item) };
        }
        let raw = String::from_utf8(bytes).context("Keychain: sekret nie jest UTF-8")?;
        Ok(Some(super::decode_keychain_secret(&raw).unwrap_or(raw)))
    }

    pub(super) fn set(service: &str, account: &str, secret: &str) -> Result<bool> {
        let payload = super::encode_keychain_secret(secret);
        let service_b = service.as_bytes();
        let account_b = account.as_bytes();
        let password = payload.as_bytes();
        let status = unsafe {
            SecKeychainAddGenericPassword(
                ptr::null_mut(),
                u32_len(service_b, "usługa")?,
                service_b.as_ptr(),
                u32_len(account_b, "konto")?,
                account_b.as_ptr(),
                u32_len(password, "sekret")?,
                password.as_ptr(),
                ptr::null_mut(),
            )
        };
        if status == 0 {
            relax_named_item(service, account);
            return Ok(true);
        }
        if status != ERR_DUPLICATE_ITEM {
            return Err(anyhow!("Keychain: zapis status {status}"));
        }
        let mut item = ptr::null_mut::<c_void>();
        let find = unsafe {
            SecKeychainFindGenericPassword(
                ptr::null_mut(),
                u32_len(service_b, "usługa")?,
                service_b.as_ptr(),
                u32_len(account_b, "konto")?,
                account_b.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut item,
            )
        };
        if find != 0 || item.is_null() {
            return Err(anyhow!("Keychain: brak elementu do aktualizacji ({find})"));
        }
        let modify = unsafe {
            SecKeychainItemModifyContent(
                item,
                ptr::null(),
                u32_len(password, "sekret")?,
                password.as_ptr(),
            )
        };
        relax_item_access(item);
        unsafe { CFRelease(item) };
        if modify != 0 {
            return Err(anyhow!("Keychain: aktualizacja status {modify}"));
        }
        Ok(true)
    }

    fn relax_named_item(service: &str, account: &str) {
        let service_b = service.as_bytes();
        let account_b = account.as_bytes();
        let mut item = ptr::null_mut::<c_void>();
        let Ok(service_len) = u32_len(service_b, "usługa") else {
            return;
        };
        let Ok(account_len) = u32_len(account_b, "konto") else {
            return;
        };
        let find = unsafe {
            SecKeychainFindGenericPassword(
                ptr::null_mut(),
                service_len,
                service_b.as_ptr(),
                account_len,
                account_b.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut item,
            )
        };
        if find == 0 && !item.is_null() {
            relax_item_access(item);
            unsafe { CFRelease(item) };
        }
    }

    #[cfg(test)]
    pub(super) fn delete(service: &str, account: &str) -> Result<()> {
        let service_b = service.as_bytes();
        let account_b = account.as_bytes();
        let mut item = ptr::null_mut::<c_void>();
        let find = unsafe {
            SecKeychainFindGenericPassword(
                ptr::null_mut(),
                u32_len(service_b, "usługa")?,
                service_b.as_ptr(),
                u32_len(account_b, "konto")?,
                account_b.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut item,
            )
        };
        if find == ERR_ITEM_NOT_FOUND {
            return Ok(());
        }
        if find != 0 || item.is_null() {
            return Err(anyhow!("Keychain: usuwanie, odczyt status {find}"));
        }
        let status = unsafe { SecKeychainItemDelete(item) };
        unsafe { CFRelease(item) };
        if status != 0 {
            return Err(anyhow!("Keychain: usuwanie status {status}"));
        }
        Ok(())
    }
}

fn encode_keychain_secret(secret: &str) -> String {
    format!("base64:{}", STANDARD.encode(secret))
}

fn decode_keychain_secret(raw: &str) -> Option<String> {
    if let Some(encoded) = raw.strip_prefix("base64:") {
        return STANDARD
            .decode(encoded)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
    }
    if raw.len().is_multiple_of(2) && raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return hex::decode(raw)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
    }
    None
}

fn secret_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn keychain_get_secret(account: &str) -> Result<Option<String>> {
    if let Ok(cache) = secret_cache().lock()
        && let Some(value) = cache.get(account)
    {
        return Ok(value.clone());
    }
    #[cfg(target_os = "macos")]
    let value = macos::get(KEYCHAIN_SERVICE, account)?;
    #[cfg(not(target_os = "macos"))]
    let value = {
        let _ = account;
        None
    };
    if let Ok(mut cache) = secret_cache().lock() {
        cache.insert(account.to_string(), value.clone());
    }
    Ok(value)
}

pub(crate) fn keychain_set_secret(account: &str, secret: &str) -> Result<bool> {
    #[cfg(target_os = "macos")]
    let stored = macos::set(KEYCHAIN_SERVICE, account, secret)?;
    #[cfg(not(target_os = "macos"))]
    let stored = {
        let _ = (account, secret);
        false
    };
    if stored && let Ok(mut cache) = secret_cache().lock() {
        cache.insert(account.to_string(), Some(secret.to_string()));
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_encoded_and_never_raw_cli_password() {
        let payload = encode_keychain_secret("token-gmail");
        assert!(payload.starts_with("base64:"));
        assert!(!payload.contains("token-gmail"));
        assert_eq!(
            decode_keychain_secret(&payload).as_deref(),
            Some("token-gmail")
        );
        assert_eq!(
            decode_keychain_secret(&hex::encode("hex-secret")).as_deref(),
            Some("hex-secret")
        );
        assert!(include_str!("keychain.rs").contains("SecKeychainAddGenericPassword"));
        assert!(!include_str!("keychain.rs").contains("Command::new(\"security\")"));
        assert!(!include_str!("main.rs").contains("add-generic-password"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "tworzy i usuwa wpis w prawdziwym login Keychain; uruchom: cargo test -- --ignored roundtrip_uses_security_framework"]
    fn roundtrip_uses_security_framework() {
        let account = format!("lab-test-{}", std::process::id());
        macos::delete(KEYCHAIN_SERVICE, &account).unwrap();
        assert!(macos::set(KEYCHAIN_SERVICE, &account, "wartość z polskimi znakami żźć").unwrap());
        assert_eq!(
            macos::get(KEYCHAIN_SERVICE, &account).unwrap().as_deref(),
            Some("wartość z polskimi znakami żźć")
        );
        macos::delete(KEYCHAIN_SERVICE, &account).unwrap();
        assert_eq!(macos::get(KEYCHAIN_SERVICE, &account).unwrap(), None);
    }
}
