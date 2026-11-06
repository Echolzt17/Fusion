//! Uninstall-only deletion. Inspect target names, never credential blobs.
use crate::Result;

fn owned_target(target: &str) -> bool {
    let Some(key) = target.strip_suffix(".PersonalCodexPool") else { return false; };
    let rest = if let Some(rest) = key.strip_prefix("gateway-key") { rest }
        else if let Some(rest) = key.strip_prefix("account/") {
            let id = rest.split('/').next().unwrap_or_default();
            if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) { return false; }
            &rest[id.len()..]
        } else { return false; };
    if rest.is_empty() { return true; }
    let Some((generation, part)) = rest.strip_prefix('/').and_then(|s| s.split_once('/')) else { return false; };
    uuid::Uuid::parse_str(generation).is_ok() && part.parse::<usize>().is_ok_and(|n| n < 128)
}

#[cfg(windows)]
pub fn remove_app_credentials() -> Result<()> {
    // Win32 supports prefix filters only, followed by a literal service check.
    // https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credenumeratew
    remove_matching("account/*", owned_target)?;
    remove_matching("gateway-key*", owned_target)
}
#[cfg(windows)]
fn remove_matching(prefix: &str, owns: impl Fn(&str) -> bool) -> Result<()> {
    use windows_sys::Win32::{Foundation::{GetLastError, ERROR_NOT_FOUND}, Security::Credentials::*};
    // keyring 3.6.3 uses "{user}.{service}" as the generic TargetName.
    let filter: Vec<u16> = prefix.encode_utf16().chain(Some(0)).collect();
    let mut count = 0;
    let mut entries = std::ptr::null_mut();
    unsafe {
        if CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut entries) == 0 {
            return if GetLastError() == ERROR_NOT_FOUND { Ok(()) } else { Err("无法枚举本工具凭据，账号数据未清理".into()) };
        }
        let mut failed = false;
        for entry in std::slice::from_raw_parts(entries, count as usize) {
            let entry = &**entry;
            if entry.Type != CRED_TYPE_GENERIC || entry.TargetName.is_null() { continue; }
            let mut len = 0;
            while len < 32768 && *entry.TargetName.add(len) != 0 { len += 1; }
            if len == 32768 { failed = true; continue; }
            let target = String::from_utf16(std::slice::from_raw_parts(entry.TargetName, len));
            if target.as_deref().is_ok_and(&owns)
                && CredDeleteW(entry.TargetName, CRED_TYPE_GENERIC, 0) == 0
                && GetLastError() != ERROR_NOT_FOUND { failed = true; }
        }
        CredFree(entries.cast());
        if failed { Err("本工具凭据清理未完成，请重试卸载".into()) } else { Ok(()) }
    }
}
#[cfg(not(windows))]
pub fn remove_app_credentials() -> Result<()> { Err("仅支持 Windows 凭据管理器".into()) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_service_and_known_key_shapes_only() {
        let account = format!("account/{}.PersonalCodexPool", "a".repeat(64));
        assert!(owned_target(&account));
        assert!(owned_target("gateway-key.PersonalCodexPool"));
        assert!(owned_target("gateway-key/03ab09f0-eebd-4de2-9697-506e947e6ffc/0.PersonalCodexPool"));
        for target in ["other.PersonalCodexPool", "gateway-key.OtherTool", "gateway-key.PersonalCodexPool.extra", "account/short.PersonalCodexPool", "gateway-key/bad/0.PersonalCodexPool", "gateway-key/03ab09f0-eebd-4de2-9697-506e947e6ffc/128.PersonalCodexPool"] { assert!(!owned_target(target)); }
    }
    #[cfg(windows)]
    #[test]
    fn native_cleanup_removes_only_unique_fixture_target_without_reading_blob() {
        let prefix = format!("fusion-cleanup-fixture-{}", uuid::Uuid::new_v4());
        let owned = keyring::Entry::new("FusionCleanupFixture", &format!("{prefix}/owned")).unwrap();
        let keep = keyring::Entry::new("FusionCleanupFixture", &format!("{prefix}/keep")).unwrap();
        let target = format!("{prefix}/owned.FusionCleanupFixture");
        owned.set_password("public fixture only").unwrap(); keep.set_password("public fixture only").unwrap();
        let result = remove_matching(&format!("{prefix}*"), |name| name == target);
        let removed = matches!(owned.get_password(), Err(keyring::Error::NoEntry));
        let retained = keep.get_password().is_ok();
        let _ = owned.delete_credential(); let _ = keep.delete_credential();
        result.unwrap(); assert!(removed); assert!(retained);
    }
}
