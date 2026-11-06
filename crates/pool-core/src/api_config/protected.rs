//! Protect the complete recovery record, including its local-only access key.
use crate::Result;
use base64::Engine;
const PREFIX: &str = "fusion-api-dpapi-v1:";
pub(super) fn seal(text: &str) -> Result<String> {
    Ok(format!("{PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(crypt(text.as_bytes(), true)?)))
}
pub(super) fn open(text: &str) -> Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(text.strip_prefix(PREFIX).ok_or("API 恢复记录格式无效")?)
        .map_err(|_| "API 恢复记录编码损坏")?;
    String::from_utf8(crypt(&bytes, false)?).map_err(|_| "API 恢复记录编码损坏".into())
}
#[cfg(windows)]
fn crypt(bytes: &[u8], protect: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN}};
    if bytes.len() > 1024 * 1024 { return Err("API 恢复记录过大".into()); }
    let input = CRYPT_INTEGER_BLOB { cbData:bytes.len() as u32, pbData:bytes.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB { cbData:0, pbData:std::ptr::null_mut() };
    let success = unsafe { if protect {
        CryptProtectData(&input,std::ptr::null(),std::ptr::null(),std::ptr::null(),std::ptr::null(),CRYPTPROTECT_UI_FORBIDDEN,&mut output)
    } else { CryptUnprotectData(&input,std::ptr::null_mut(),std::ptr::null(),std::ptr::null(),std::ptr::null(),CRYPTPROTECT_UI_FORBIDDEN,&mut output) } };
    if success == 0 { return Err("无法保护 API 恢复记录，请使用原 Windows 用户重试".into()); }
    let result = unsafe { std::slice::from_raw_parts(output.pbData,output.cbData as usize).to_vec() };
    unsafe { std::ptr::write_bytes(output.pbData,0,output.cbData as usize); LocalFree(output.pbData.cast()); }
    Ok(result)
}
#[cfg(not(windows))]
fn crypt(_: &[u8],_:bool)->Result<Vec<u8>> { Err("API 恢复记录仅支持 Windows 当前用户".into()) }
