use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;

use crate::paths::ssh_credentials_path;

const ENTROPY_LABEL: &str = "Network Manager SSH";

fn load_raw() -> HashMap<String, String> {
    let path = ssh_credentials_path();
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    map.into_iter()
        .map(|(key, value)| (key, value.as_str().unwrap_or("").to_string()))
        .collect()
}

fn save_raw(values: &HashMap<String, String>) {
    let path = ssh_credentials_path();
    if let Ok(text) = serde_json::to_string_pretty(values) {
        let temporary = path.with_extension("tmp");
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(temporary, path);
        }
    }
}

pub fn has(profile_id: &str) -> bool {
    load_raw().contains_key(profile_id)
}

/// Encrypt with the current Windows account (DPAPI) and persist.
pub fn set(profile_id: &str, password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Ok(());
    }
    let protected = protect(password)?;
    let mut values = load_raw();
    values.insert(profile_id.to_string(), protected);
    save_raw(&values);
    Ok(())
}

pub fn get(profile_id: &str) -> Result<String, String> {
    let values = load_raw();
    let protected = values
        .get(profile_id)
        .ok_or_else(|| "未找到 SSH 密码".to_string())?;
    unprotect(protected)
}

pub fn delete(profile_id: &str) {
    let mut values = load_raw();
    if values.remove(profile_id).is_some() {
        save_raw(&values);
    }
}

#[cfg(windows)]
type Blob = windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB;

#[cfg(windows)]
fn blob_from(bytes: &mut Vec<u8>) -> Blob {
    Blob {
        cbData: bytes.len() as u32,
        pbData: bytes.as_mut_ptr(),
    }
}

#[cfg(windows)]
unsafe fn free_blob(blob: &Blob) {
    if !blob.pbData.is_null() {
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            blob.pbData as *mut _,
        )));
    }
}

#[cfg(windows)]
fn protect(value: &str) -> Result<String, String> {
    use windows::Win32::Security::Cryptography::CryptProtectData;
    let mut bytes = value.as_bytes().to_vec();
    let input = blob_from(&mut bytes);
    let mut output = Blob::default();
    let label = wide(ENTROPY_LABEL);
    unsafe {
        CryptProtectData(
            &input,
            windows::core::PCWSTR::from_raw(label.as_ptr()),
            None,
            None,
            None,
            0,
            &mut output,
        )
        .map_err(|err| format!("Windows 凭据加密失败：{err}"))?;
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let encoded = base64::engine::general_purpose::STANDARD.encode(slice);
        free_blob(&output);
        Ok(encoded)
    }
}

#[cfg(windows)]
fn unprotect(encoded: &str) -> Result<String, String> {
    use windows::Win32::Security::Cryptography::CryptUnprotectData;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "SSH 凭据内容损坏")?;
    let mut bytes = raw;
    let input = blob_from(&mut bytes);
    let mut output = Blob::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
            .map_err(|_| "SSH 凭据解密失败")?;
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let text = String::from_utf8_lossy(slice).to_string();
        free_blob(&output);
        Ok(text)
    }
}

#[cfg(not(windows))]
fn protect(value: &str) -> Result<String, String> {
    Ok(base64::engine::general_purpose::STANDARD.encode(value))
}

#[cfg(not(windows))]
fn unprotect(encoded: &str) -> Result<String, String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "SSH 凭据内容损坏")?;
    String::from_utf8(raw).map_err(|_| "SSH 凭据内容损坏".into())
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
