//! API keys at rest are encrypted with Windows DPAPI (current user scope).
//! On disk a secret looks like `"dpapi:<base64>"`; plain strings are accepted on load
//! (e.g. a hand-edited config) and re-encrypted on the next save.

use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

const PREFIX: &str = "dpapi:";

/// A secret value (API key / token). `Debug` and `Display` never print the value.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(s: impl Into<String>) -> Self {
        SecretString(s.into().trim().to_string())
    }
    /// The plain secret. Only call this right before putting it in a request header.
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// `sk-ant-…9fQx` style preview safe to show in the UI.
    pub fn masked(&self) -> String {
        mask(&self.0)
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretString({})", self.masked())
    }
}
impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.masked())
    }
}

/// Mask a secret for display: keep a recognizable prefix and the last 4 chars.
pub fn mask(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    if n <= 10 {
        return "•".repeat(n.min(8));
    }
    // keep prefix up to the last '-' or '_' within the first 8 chars (e.g. "sk-ant-", "gsk_")
    let mut keep = 0;
    for (i, c) in chars.iter().take(8).enumerate() {
        if *c == '-' || *c == '_' {
            keep = i + 1;
        }
    }
    if keep == 0 {
        keep = 3.min(n);
    }
    let prefix: String = chars[..keep].iter().collect();
    let suffix: String = chars[n - 4..].iter().collect();
    format!("{prefix}••••{suffix}")
}

impl Serialize for SecretString {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.0.is_empty() {
            return s.serialize_str("");
        }
        match protect(&self.0) {
            Ok(enc) => s.serialize_str(&enc),
            Err(_) => s.serialize_str(&self.0),
        }
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(SecretString(unprotect(&raw).unwrap_or_default()))
    }
}

/// Encrypt with DPAPI → `"dpapi:<base64>"`.
pub fn protect(plain: &str) -> anyhow::Result<String> {
    #[cfg(windows)]
    {
        let bytes = dpapi::protect(plain.as_bytes())?;
        Ok(format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }
    #[cfg(not(windows))]
    {
        Ok(format!(
            "b64:{}",
            base64::engine::general_purpose::STANDARD.encode(plain.as_bytes())
        ))
    }
}

/// Decrypt a stored secret. Plain (unprefixed) strings are returned as-is.
pub fn unprotect(stored: &str) -> anyhow::Result<String> {
    if let Some(b64) = stored.strip_prefix(PREFIX) {
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())?;
        #[cfg(windows)]
        {
            let plain = dpapi::unprotect(&bytes)?;
            return Ok(String::from_utf8(plain)?);
        }
        #[cfg(not(windows))]
        {
            let _ = bytes;
            anyhow::bail!("DPAPI secrets can only be read on Windows");
        }
    }
    if let Some(b64) = stored.strip_prefix("b64:") {
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())?;
        return Ok(String::from_utf8(bytes)?);
    }
    Ok(stored.to_string())
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    pub fn protect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        unsafe {
            let input = CRYPT_INTEGER_BLOB {
                cbData: data.len() as u32,
                pbData: data.as_ptr() as *mut u8,
            };
            let mut out = CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            };
            let ok = CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 {
                anyhow::bail!("CryptProtectData failed");
            }
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData as HLOCAL);
            Ok(v)
        }
    }

    pub fn unprotect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        unsafe {
            let input = CRYPT_INTEGER_BLOB {
                cbData: data.len() as u32,
                pbData: data.as_ptr() as *mut u8,
            };
            let mut out = CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            };
            let ok = CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 {
                anyhow::bail!("CryptUnprotectData failed (secret was encrypted by another Windows user?)");
            }
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData as HLOCAL);
            Ok(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let enc = protect("sk-ant-api03-abcdefghijklmnop").unwrap();
        assert_ne!(enc, "sk-ant-api03-abcdefghijklmnop");
        assert_eq!(unprotect(&enc).unwrap(), "sk-ant-api03-abcdefghijklmnop");
        assert_eq!(unprotect("plain").unwrap(), "plain");
    }
    #[test]
    fn masking() {
        assert_eq!(mask("sk-ant-api03-abcdefghijklmnop"), "sk-ant-••••mnop");
        assert_eq!(mask("gsk_1234567890abcdef"), "gsk_••••cdef");
        assert_eq!(mask("AIzaSyD1234567890abcd"), "AIz••••abcd");
    }
}
