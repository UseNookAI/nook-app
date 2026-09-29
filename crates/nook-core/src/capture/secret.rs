//! A stream key kept on this computer: encrypted with Windows' data protection (DPAPI) for the
//! person's account, so the settings file holds nothing another account, or a copy of the file
//! on another computer, could read. On a Mac the key goes into the person's login keychain
//! instead, and the settings file only says it is there.

use anyhow::{Context, Result};
use base64::Engine as _;

/// What the settings hold for a key a Mac keeps in its keychain.
pub const IN_KEYCHAIN: &str = "keychain";
/// The keychain's service name for Nook's keys.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const KEYCHAIN_SERVICE: &str = "ai.nook.app";

/// Keeps `plain` for `name` (the setting it belongs to) and returns what that setting stores:
/// the key encrypted for this Windows account, or on a Mac [`IN_KEYCHAIN`].
pub fn keep(name: &str, plain: &str) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        security_framework::passwords::set_generic_password(
            KEYCHAIN_SERVICE,
            name,
            plain.as_bytes(),
        )
        .context("The keychain did not take the key")?;
        Ok(IN_KEYCHAIN.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = name;
        protect(plain)
    }
}

/// What [`keep`] kept for `name`, from the setting's `stored` value.
pub fn read(name: &str, stored: &str) -> Result<String> {
    if stored.trim() == IN_KEYCHAIN {
        #[cfg(target_os = "macos")]
        {
            let bytes = security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, name)
                .context("The keychain does not have the key")?;
            return String::from_utf8(bytes).context("The saved key is not text");
        }
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("The key was kept in a Mac's keychain");
    }
    let _ = name;
    unprotect(stored)
}

/// Forgets what [`keep`] kept for `name` (a Mac's keychain item; the setting is emptied by the
/// caller).
pub fn forget(name: &str) {
    #[cfg(target_os = "macos")]
    let _ = security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, name);
    #[cfg(not(target_os = "macos"))]
    let _ = name;
}

/// `plain`, encrypted for this Windows account, as base64.
pub fn protect(plain: &str) -> Result<String> {
    let sealed = imp::protect(plain.as_bytes())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(sealed))
}

/// What [`protect`] sealed.
pub fn unprotect(sealed: &str) -> Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(sealed.trim())
        .context("The saved key is not readable")?;
    String::from_utf8(imp::unprotect(&bytes)?).context("The saved key is not text")
}

#[cfg(windows)]
mod imp {
    use anyhow::{anyhow, Result};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn run(input: &[u8], seal: bool) -> Result<Vec<u8>> {
        let blob = CRYPT_INTEGER_BLOB {
            cbData: input.len() as u32,
            pbData: input.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: the blobs point at memory alive for the call; Windows allocates the output,
        // freed below.
        let ok = unsafe {
            if seal {
                CryptProtectData(
                    &blob,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            } else {
                CryptUnprotectData(
                    &blob,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            }
        };
        if ok == 0 {
            return Err(anyhow!(
                "Windows could not {} the key: {}",
                if seal { "protect" } else { "read" },
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: Windows filled `out` with cbData bytes, freed right after the copy.
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe { LocalFree(out.pbData as *mut core::ffi::c_void) };
        Ok(bytes)
    }

    pub(super) fn protect(plain: &[u8]) -> Result<Vec<u8>> {
        run(plain, true)
    }

    pub(super) fn unprotect(sealed: &[u8]) -> Result<Vec<u8>> {
        run(sealed, false)
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::{bail, Result};

    pub(super) fn protect(_: &[u8]) -> Result<Vec<u8>> {
        bail!("Keys are sealed only on Windows (a Mac keeps them in its keychain)")
    }

    pub(super) fn unprotect(_: &[u8]) -> Result<Vec<u8>> {
        bail!("Keys are sealed only on Windows (a Mac keeps them in its keychain)")
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_key_comes_back_and_is_not_kept_as_it_was() {
        let sealed = protect("live_1234567890_abcdef").unwrap();
        assert!(!sealed.contains("live_1234567890"));
        assert_eq!(unprotect(&sealed).unwrap(), "live_1234567890_abcdef");
        assert!(
            unprotect("bm90IHNlYWxlZA==").is_err(),
            "not something Windows sealed"
        );
    }
}
