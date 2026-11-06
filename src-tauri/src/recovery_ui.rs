use pool_core::config;
// Standalone before database/vault/Tauri initialization. No unattended configuration write is allowed.
pub fn run() {
    let result = (|| -> pool_core::Result<()> {
        let path = config::codex_config_path()?;
        let exe = std::env::current_exe().map_err(|_| "找不到程序路径")?;
        let plan = config::recovery_preview(None, &path, &exe, None);
        if !plan.can_apply {
            return Err(plan.warning.unwrap_or("无法准备恢复".into()));
        }
        let text=format!("恢复 Codex 官方接入\n\n修改前：{}\n修改后：{}\n配置位置：{}\n{}\n{}\n\n点击确定应用；取消不会修改任何配置。",plan.before,plan.after,plan.path,plan.warning.as_deref().unwrap_or(""),plan.archive_path.as_ref().map(|p|format!("原文件保留位置：{p}")).unwrap_or_default());
        if !confirm(&text) {
            return Ok(());
        }
        config::apply_recovery(None, &path, &exe, plan.strategy, &plan.fingerprint)?;
        if let Ok(dir) = crate::data_dir() {
            let _ =
                pool_core::storage::Store::retire_recovery_if_readable(&dir.join("pool.sqlite"));
        }
        show("已切回官方接入配置。请正常退出并重新打开 Codex，再新建官方会话。原登录和聊天数据保持原样。");
        Ok(())
    })();
    if let Err(e) = result {
        show(&format!("尚未恢复官方接入：{e}\n\n没有删除原配置或登录数据。处理文件占用或写权限后，可重新运行本入口。Fusion 主程序无需保持运行。"));
    }
}
#[cfg(windows)]
fn message(text: &str, kind: u32) -> i32 {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            w: *mut std::ffi::c_void,
            text: *const u16,
            title: *const u16,
            kind: u32,
        ) -> i32;
    }
    let text: Vec<_> = text.encode_utf16().chain(Some(0)).collect();
    let title: Vec<_> = "Fusion · 恢复官方接入"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), kind) }
}
#[cfg(windows)]
fn confirm(text: &str) -> bool {
    message(text, 0x31) == 1
}
#[cfg(windows)]
fn show(text: &str) {
    message(text, 0x40);
}
#[cfg(not(windows))]
fn confirm(_: &str) -> bool {
    false
}
#[cfg(not(windows))]
fn show(_: &str) {}
