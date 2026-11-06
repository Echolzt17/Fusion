//! Protect owned provider snapshots containing the local gateway key in SQLite.
//! Codex still needs the local key in its user config; no OpenAI token is copied.
use crate::Result;
use base64::Engine;
use serde::{Deserialize, Deserializer, Serializer};
const PREFIX: &str = "fusion-dpapi-v1:";

pub(super) fn seal(value: &str) -> Result<String> {
    if !value.contains("x-fusion-client-key") { return Ok(value.into()); }
    Ok(format!("{PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(crypt(value.as_bytes(), true)?)))
}
pub(super) fn open(value: &str) -> Result<String> {
    let Some(encoded) = value.strip_prefix(PREFIX) else { return Ok(value.into()); };
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)
        .map_err(|_| "接入恢复记录编码损坏")?;
    String::from_utf8(crypt(&bytes, false)?).map_err(|_| "接入恢复记录编码无效".into())
}
pub(super) fn serialize<S: Serializer>(value: &str, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&seal(value).map_err(serde::ser::Error::custom)?)
}
pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<String, D::Error> {
    open(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}
#[cfg(windows)]
fn crypt(bytes: &[u8], protect: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    }};
    if bytes.len() > 1024 * 1024 { return Err("接入恢复记录超出限制".into()); }
    let input = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe { if protect {
        CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
    } else {
        CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
    }};
    if ok == 0 { return Err("无法保护或读取接入恢复记录，请使用原 Windows 用户重试".into()); }
    let result = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { std::ptr::write_bytes(output.pbData, 0, output.cbData as usize); LocalFree(output.pbData.cast()); }
    Ok(result)
}
#[cfg(not(windows))]
fn crypt(_: &[u8], _: bool) -> Result<Vec<u8>> { Err("接入恢复记录仅支持当前 Windows 用户".into()) }
