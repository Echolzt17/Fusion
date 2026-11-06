use pool_core::{config, vault, Result};
use std::path::Path;

fn check_tree(path: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("无法检查应用数据目录，清理已停止".into()),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 { return Err("应用数据包含重解析点，未清理".into()); }
    }
    if metadata.file_type().is_symlink() { return Err("应用数据包含链接，未清理".into()); }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|_| "无法检查应用数据目录")? {
            check_tree(&entry.map_err(|_| "无法检查应用数据文件")?.path())?;
        }
    }
    Ok(())
}
fn clean(local: &Path, remove_credentials: impl FnOnce() -> Result<()>) -> Result<()> {
    // Only the exact application directory under the resolved user data root.
    let local = local.canonicalize().map_err(|_| "无法定位本机用户数据目录")?;
    let target = local.join("PersonalCodexPool");
    check_tree(&target)?;
    remove_credentials()?;
    if target.exists() { std::fs::remove_dir_all(&target).map_err(|_| "账号数据目录清理未完成，请重试卸载")?; }
    Ok(())
}
pub fn run() -> Result<()> {
    let path = config::codex_config_path()?;
    let exe = std::env::current_exe().map_err(|_| "找不到程序路径")?;
    if !pool_core::api_config::detached(&path)? { return Err("请先关闭 API 接入再删除应用数据".into()); }
    if !config::detached(&path, &exe, 18765)? { return Err("请先恢复原接入再删除应用数据".into()); }
    let local = std::env::var_os("LOCALAPPDATA").ok_or("无法找到本机用户数据目录")?;
    clean(Path::new(&local), vault::cleanup::remove_app_credentials)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_is_exact_retryable_and_retains_data_on_credential_failure() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("PersonalCodexPool");
        let other = d.path().join("OtherTool");
        std::fs::create_dir_all(&app).unwrap(); std::fs::create_dir_all(&other).unwrap();
        std::fs::write(app.join("pool.sqlite"), "fixture").unwrap();
        std::fs::write(other.join("keep"), "keep").unwrap();
        assert!(clean(d.path(), || Err("fixture vault failure".into())).is_err());
        assert!(app.join("pool.sqlite").exists());
        clean(d.path(), || Ok(())).unwrap(); clean(d.path(), || Ok(())).unwrap();
        assert!(!app.exists()); assert!(other.join("keep").exists());
    }
    #[test]
    fn links_are_rejected_before_credentials_are_touched() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("PersonalCodexPool");
        // A directory junction requires no developer-mode symlink privilege.
        let other = d.path().join("keep"); std::fs::create_dir(&other).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            assert!(std::process::Command::new("cmd").args(["/d", "/c", "mklink", "/J"])
                .arg(&target).arg(&other).creation_flags(0x08000000)
                .stdout(std::process::Stdio::null()).status().unwrap().success());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, &target).unwrap();
        assert!(clean(d.path(), || panic!("must reject the link first")).is_err());
        std::fs::remove_dir(&target).unwrap();
        assert!(other.exists());
    }
}
