use super::*;

const MINIMAL: &str =
    "# Fusion 救援：原配置已保留，请正常重开 Codex。\nmodel_provider = \"openai\"\n";
fn source(path: &Path) -> Result<(String, bool, String)> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => return Err("无法读取配置，请处理文件占用或权限后使用恢复入口".into()),
    };
    let valid = std::str::from_utf8(&bytes).is_ok();
    Ok((
        String::from_utf8_lossy(&bytes).into_owned(),
        valid,
        format!("{:x}", Sha256::digest(&bytes)),
    ))
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStrategy {
    Normal,
    Official,
    Rescue,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryPlan {
    pub path: String,
    pub before: String,
    pub after: String,
    pub fingerprint: String,
    pub strategy: RecoveryStrategy,
    pub archive_path: Option<String>,
    pub warning: Option<String>,
    pub can_apply: bool,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u8,
    archive: String,
    source_hash: String,
}
fn journal_path(path: &Path) -> PathBuf {
    path.with_file_name("personal-codex-pool-recovery.json")
}
fn journal(path: &Path) -> Result<Option<Journal>> {
    let raw = read(&journal_path(path))?;
    if raw.is_empty() {
        return Ok(None);
    }
    let j: Journal = serde_json::from_str(&raw)
        .map_err(|_| "救援事务记录损坏，请保留该文件后使用独立恢复入口")?;
    if j.version != 1
        || !j.archive.starts_with("config.fusion-recovery-")
        || !j.archive.ends_with(".toml")
        || j.archive.contains(['/', '\\', ':'])
        || j.source_hash.len() != 64
    {
        return Err("救援事务路径无效，未移动任何文件".into());
    }
    Ok(Some(j))
}
pub(super) fn active(doc: &DocumentMut) -> Result<Option<(&str, &Item)>> {
    if doc
        .get("profiles")
        .is_some_and(|p| p.as_table_like().is_none())
    {
        return Err("profiles 表格式无效".into());
    }
    let Some(p) = doc.get("profile") else {
        return Ok(None);
    };
    let name = p.as_str().ok_or("profile 类型无效")?;
    let result = doc
        .get("profiles")
        .and_then(|p| p.get(name))
        .map(|p| (name, p));
    if result.is_some_and(|(_, p)| p.as_table_like().is_none()) {
        return Err("当前 profile 表格式无效".into());
    }
    Ok(result)
}
pub(super) fn provider_name(doc: &DocumentMut) -> Result<&str> {
    let v = active(doc)?
        .and_then(|(_, p)| p.get("model_provider"))
        .or_else(|| doc.get("model_provider"));
    Ok(v.map(|v| v.as_str().ok_or("provider 类型无效"))
        .unwrap_or(Ok("openai"))?)
}
fn local_pool_url(v: Option<&str>, port: u16) -> bool {
    v.and_then(|s| url::Url::parse(s).ok()).is_some_and(|u| {
        matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) && u.port() == Some(port)
    })
}
pub fn detached(path: &Path, exe: &Path, port: u16) -> Result<bool> {
    detached_doc(&parse(&read(path)?)?, exe, port)
}
pub(super) fn detached_doc(doc: &DocumentMut, exe: &Path, port: u16) -> Result<bool> {
    let name = provider_name(doc)?;
    if name == PROVIDER {
        return Ok(false);
    }
    let p = doc.get("model_providers").and_then(|p| p.get(name));
    let command = p
        .and_then(|p| p.get("auth"))
        .and_then(|a| a.get("command"))
        .and_then(Item::as_str);
    if command.is_some_and(|c| {
        c.eq_ignore_ascii_case(&exe.to_string_lossy())
            || c.replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/personal-codex-pool.exe")
    }) {
        return Ok(false);
    }
    if local_pool_url(
        p.and_then(|p| p.get("base_url")).and_then(Item::as_str),
        port,
    ) {
        return Ok(false);
    }
    if name == "openai"
        && (local_pool_url(doc.get("openai_base_url").and_then(Item::as_str), port)
            || local_pool_url(
                active(doc)?
                    .and_then(|(_, p)| p.get("openai_base_url"))
                    .and_then(Item::as_str),
                port,
            ))
    {
        return Ok(false);
    }
    Ok(true)
}
// Only hashes leave this scope. Never expose auth commands, headers, or an entire user config in a preview.
fn scope(doc: &DocumentMut) -> String {
    let v: serde_json::Value = toml_edit::de::from_str(&doc.to_string()).unwrap_or_default();
    let active = v["profile"].as_str().map(|name| &v["profiles"][name]);
    serde_json::json!([
        v["model_provider"],
        v["features"]["image_generation"],
        v["model_providers"][PROVIDER],
        v["profile"],
        active.map(|p| &p["model_provider"]),
        active.map(|p| &p["openai_base_url"]),
        v["openai_base_url"],
        v["model_providers"]["openai"]
    ])
    .to_string()
}

fn summary(doc: &DocumentMut) -> String {
    let name = provider_name(doc).unwrap_or("无法解析");
    // Provider identifiers are not echoed: an edited config could contain arbitrary sensitive text.
    let state = match name {
        "openai" => "官方",
        PROVIDER => "Fusion 账号池",
        _ => "其他自定义接入",
    };
    format!(
        "有效接入：{state}\n当前 profile：{}",
        if active(doc).ok().flatten().is_some() {
            "已设置"
        } else {
            "无"
        }
    )
}
fn official(doc: &mut DocumentMut, b: Option<&Backup>) -> Result<()> {
    active(doc)?;
    doc["model_provider"] = value("openai");
    if let Some(name) = doc.get("profile").and_then(Item::as_str).map(str::to_owned) {
        if let Some(profile) = doc.get_mut("profiles").and_then(|p| p.get_mut(&name)) {
            let p = profile
                .as_table_like_mut()
                .ok_or("当前 profile 格式无法安全修改")?;
            p.insert("model_provider", value("openai"));
            p.remove("openai_base_url");
        }
    }
    doc.remove("openai_base_url");
    let remove_pool = b.is_some_and(|b| {
        equivalent(&provider(doc), &b.applied_provider)
            || b.previous
                .as_ref()
                .is_some_and(|p| equivalent(&provider(doc), &p.provider))
    });
    if let Some(providers) = doc.get_mut("model_providers") {
        let p = providers
            .as_table_like_mut()
            .ok_or("provider 表格式无法安全修改")?;
        p.remove("openai"); // Remove an endpoint/auth override of the built-in official provider.
        if remove_pool {
            p.remove(PROVIDER);
        }
    }
    Ok(())
}
fn port(store: Option<&Store>) -> u16 {
    store
        .and_then(|s| s.settings().ok())
        .map_or(18765, |s| s.port)
}
fn archive(path: &Path, source: &str) -> Result<PathBuf> {
    if let Some(j) = journal(path).ok().flatten() {
        let prior = path.with_file_name(j.archive);
        if prior.exists() && (!path.exists() || source == MINIMAL) {
            return Ok(prior);
        }
        if !prior.exists() && self::source(path)?.2 == j.source_hash {
            return Ok(prior);
        }
    }
    let stem = format!("config.fusion-recovery-{}", &hash(source)[..16]);
    for i in 0..1000 {
        let p = path.with_file_name(format!("{stem}-{i}.toml"));
        if !p.exists() {
            return Ok(p);
        }
    }
    Err("保留配置文件名已用尽，未覆盖任何文件".into())
}
pub fn recovery_preview(
    store: Option<&Store>,
    path: &Path,
    exe: &Path,
    requested: Option<RecoveryStrategy>,
) -> RecoveryPlan {
    let failed = |e: String| RecoveryPlan {
        path: path.display().to_string(),
        before: "无法读取接入配置".into(),
        after: "恢复官方接入".into(),
        fingerprint: String::new(),
        strategy: RecoveryStrategy::Rescue,
        archive_path: None,
        warning: Some(e),
        can_apply: false,
    };
    let (source, valid, source_hash) = match self::source(path) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let b = store.and_then(|s| backup(s).ok().flatten());
    let doc = if valid { parse(&source).ok() } else { None };
    let normal = doc
        .as_ref()
        .and(b.as_ref())
        .and_then(|b| restored(&source, b).ok())
        .filter(|d| detached_doc(d, exe, port(store)).unwrap_or(false))
        .or_else(|| {
            doc.as_ref()
                .filter(|d| store.is_some() && detached_doc(d, exe, port(store)).unwrap_or(false))
                .cloned()
        });
    let pending = journal(path).ok().flatten().is_some() && (!path.exists() || source == MINIMAL);
    let mut chosen = requested.unwrap_or({
        if pending {
            RecoveryStrategy::Rescue
        } else if normal.is_some() {
            RecoveryStrategy::Normal
        } else if doc.is_some() {
            RecoveryStrategy::Official
        } else {
            RecoveryStrategy::Rescue
        }
    });
    let mut target = doc.clone();
    if chosen == RecoveryStrategy::Normal && normal.is_none() {
        chosen = if doc.is_some() {
            RecoveryStrategy::Official
        } else {
            RecoveryStrategy::Rescue
        };
    }
    if chosen == RecoveryStrategy::Normal {
        target = normal;
    }
    if chosen == RecoveryStrategy::Official
        && target
            .as_mut()
            .is_none_or(|d| official(d, b.as_ref()).is_err())
    {
        chosen = RecoveryStrategy::Rescue;
    }
    let archived = if chosen == RecoveryStrategy::Rescue {
        match archive(path, &source) {
            Ok(p) => Some(p),
            Err(e) => return failed(e),
        }
    } else {
        None
    };
    let raw_backup = store.and_then(|s| s.get("codex_config_backup").ok().flatten());
    let basis = if chosen == RecoveryStrategy::Rescue {
        source_hash
    } else {
        scope(doc.as_ref().unwrap())
    };
    let fingerprint = hash(
        &serde_json::json!([
            chosen,
            path.to_string_lossy(),
            exe.to_string_lossy(),
            port(store),
            basis,
            raw_backup,
            read(&journal_path(path)).unwrap_or_default(),
            archived
        ])
        .to_string(),
    );
    RecoveryPlan {
        path: path.display().to_string(), before: doc.as_ref().map(summary).unwrap_or("现有配置无法解析；将原位改名保留，不展示文件内容".into()),
        after: if chosen == RecoveryStrategy::Rescue { "使用最小官方配置；自定义设置暂时停用。原登录、聊天和账号池数据保留。".into() } else { format!("{}\n保留无关设置；关闭后正常重开 Codex。", summary(target.as_ref().unwrap())) },
        fingerprint, strategy: chosen, archive_path: archived.map(|p| p.display().to_string()), can_apply: true,
        warning: match chosen { RecoveryStrategy::Normal => None, RecoveryStrategy::Official => Some("原恢复记录不可直接使用。将切回官方 provider，并解除当前 profile、官方端点覆盖对 Fusion 的依赖。".into()), RecoveryStrategy::Rescue => Some("救援会暂时停用当前配置中的自定义设置。原文件原位保留，不读取或修改 auth.json。".into()) },
    }
}
pub fn apply_recovery(
    store: Option<&Store>,
    path: &Path,
    exe: &Path,
    strategy: RecoveryStrategy,
    fingerprint: &str,
) -> Result<()> {
    apply_recovery_inner(store, path, exe, strategy, fingerprint, true)
}
fn apply_recovery_inner(
    store: Option<&Store>,
    path: &Path,
    exe: &Path,
    strategy: RecoveryStrategy,
    fingerprint: &str,
    retire: bool,
) -> Result<()> {
    let _lock = locked(path)?;
    let (source, _, source_hash) = self::source(path)?;
    let p = recovery_preview(store, path, exe, Some(strategy));
    if !p.can_apply {
        return Err(p.warning.unwrap_or("无法准备恢复".into()));
    }
    if p.strategy != strategy || p.fingerprint != fingerprint {
        return Err("接入字段在确认后变化，请重新查看恢复方案；无关设置不会被覆盖".into());
    }
    if self::source(path)?.2 != source_hash {
        return Err("配置在恢复期间被修改，请重新确认".into());
    }
    match strategy {
        RecoveryStrategy::Normal => {
            let restored = store
                .and_then(|s| backup(s).ok().flatten())
                .and_then(|b| super::restored(&source, &b).ok())
                .filter(|d| detached_doc(d, exe, port(store)).unwrap_or(false));
            if let Some(d) = restored {
                if d.to_string() != source {
                    write(path, &source, &d.to_string())?;
                }
                if let Some(store) = store {
                    // Ownership survives retirement of the restore record. A later
                    // enable may update exactly this table, never an edited one.
                    store.set("codex_retained_provider", &protected::seal(&provider(&d))?)?;
                }
            } else if !detached_doc(&parse(&source)?, exe, port(store))? {
                return Err("接入字段已变化，请重新确认恢复".into());
            }
        }
        RecoveryStrategy::Official => {
            let mut d = parse(&source)?;
            official(
                &mut d,
                store.and_then(|s| backup(s).ok().flatten()).as_ref(),
            )?;
            write(path, &source, &d.to_string())?;
        }
        RecoveryStrategy::Rescue => rescue(
            path,
            &source,
            &source_hash,
            &PathBuf::from(p.archive_path.ok_or("救援保留路径缺失")?),
        )?,
    }
    if !detached(path, exe, port(store))? {
        return Err("恢复后仍检测到有效的账号池依赖，请使用救援恢复".into());
    }
    // A broken metadata DB must not undo a successful official configuration repair.
    if let Some(store) = store {
        let changed = self::source(path)?.2 != source_hash || has_backup(store).unwrap_or(false);
        if retire {
            store.set("codex_config_backup", "")?;
        }
        if changed {
            store.set("codex_client_reload_required", "original")?;
        }
    }
    let jp = journal_path(path);
    if jp.exists() {
        let _ = fs::remove_file(jp);
    }
    Ok(())
}

/// User-requested close restores only the recorded origin; never downgrade to official/rescue.
/// Keep the recovery record until the desktop has drained and reloaded successfully.
pub fn restore_for_close(store: &Store, path: &Path, exe: &Path) -> Result<bool> {
    let b = backup(store)?;
    if recovery_transaction_pending(path) {
        return Err("配置救援尚未完成，请先使用独立恢复入口完成修复".into());
    }
    let safe = detached(path, exe, port(Some(store)))?;
    if b.is_none() && !safe {
        return Err("原接入恢复记录缺失，无法自动恢复；未停止服务".into());
    }
    if b.is_none() {
        // Legacy versions never retired reload hints after a manual desktop restart.
        // A hint alone must not restart the user's current desktop on an ordinary exit.
        return Ok(false);
    }
    let plan = recovery_preview(Some(store), path, exe, Some(RecoveryStrategy::Normal));
    if !plan.can_apply || plan.strategy != RecoveryStrategy::Normal {
        return Err("接入字段已被修改，无法安全恢复原接入；未停止服务".into());
    }
    apply_recovery_inner(
        Some(store),
        path,
        exe,
        RecoveryStrategy::Normal,
        &plan.fingerprint,
        false,
    )?;
    Ok(true)
}

pub fn verify_closed(store: &Store, path: &Path, exe: &Path) -> Result<()> {
    if recovery_transaction_pending(path) || !detached(path, exe, port(Some(store)))? {
        return Err("当前接入仍依赖账号池，已取消退出并保留恢复记录".into());
    }
    Ok(())
}
fn rescue(path: &Path, source: &str, source_hash: &str, archived: &Path) -> Result<()> {
    let prior = journal(path).ok().flatten();
    let resuming = prior.is_some() && archived.exists() && (!path.exists() || source == MINIMAL);
    if !resuming {
        let j = Journal {
            version: 1,
            archive: archived
                .file_name()
                .ok_or("保留路径无效")?
                .to_string_lossy()
                .to_string(),
            source_hash: source_hash.into(),
        };
        let jp = journal_path(path);
        write(
            &jp,
            &read(&jp)?,
            &serde_json::to_string(&j).map_err(|_| "救援事务编码失败")?,
        )?;
        if self::source(path)?.2 != source_hash {
            return Err("救援期间配置已变化，未移动文件，请重新确认".into());
        }
        if path.exists() {
            rename_new(path, archived)?;
        }
    }
    if path.exists() {
        if read(path)? == MINIMAL {
            return Ok(());
        }
        return Err("救援期间其他程序创建了新配置；原文件已保留，未覆盖新配置".into());
    }
    use std::io::Write;
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|_| "无法准备救援配置；原文件保留，重新运行恢复入口可继续")?;
        file.write_all(MINIMAL.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| "救援配置保存失败；原文件与恢复事务保留")?;
        drop(file);
        rename_new(&tmp, path)
    })();
    let _ = fs::remove_file(tmp);
    result
}
fn rename_new(source: &Path, target: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        }
        let from: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 8) } == 0 {
            return Err(
                "文件被占用、无写权限或目标已存在；未覆盖文件，可退出并使用独立恢复入口重试".into(),
            );
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(source, target).map_err(|_| "无法移动配置，未覆盖目标")?;
        fs::remove_file(source).map_err(|_| "无法结束配置移动".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::fixture;
    #[test]
    fn normal_close_merges_unrelated_edits_but_never_accepts_changed_owned_fields() {
        let (d, a) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        fs::write(&path, "model='keep'\n").unwrap();
        let p = preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
        apply(&a.store, &path, &exe, &a.api_key().unwrap(), &p.fingerprint).unwrap();
        let p = recovery_preview(Some(&a.store), &path, &exe, None);
        assert_eq!(p.strategy, RecoveryStrategy::Normal);
        let applied = read(&path).unwrap();
        fs::write(
            &path,
            format!(
                "# concurrent unrelated edit\n{}",
                applied.replace("model_provider =", "model_provider    =")
            ),
        )
        .unwrap();
        apply_recovery(Some(&a.store), &path, &exe, p.strategy, &p.fingerprint).unwrap();
        assert!(read(&path).unwrap().contains("concurrent unrelated edit"));
        assert!(read(&path).unwrap().contains("model='keep'"));
        assert!(detached(&path, &exe, 18765).unwrap());
        assert!(!has_backup(&a.store).unwrap());
    }
    #[test]
    fn conflict_and_corrupt_backup_offer_official_fix_including_active_profile() {
        let (d, a) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        a.store
            .set("codex_config_backup", "corrupt fixture")
            .unwrap();
        let original="model='keep'\nprofile='work'\nmodel_provider='personal_codex_pool'\n[profiles.work]\nmodel_provider='personal_codex_pool'\nother=true\n[model_providers.personal_codex_pool]\nbase_url='http://127.0.0.1:18765/v1'\nsecret='fixture-sensitive-must-not-appear'\n";
        fs::write(&path, original).unwrap();
        let p = recovery_preview(Some(&a.store), &path, &exe, None);
        assert!(p.can_apply);
        assert_eq!(p.strategy, RecoveryStrategy::Official);
        assert!(!serde_json::to_string(&p)
            .unwrap()
            .contains("fixture-sensitive"));
        apply_recovery(Some(&a.store), &path, &exe, p.strategy, &p.fingerprint).unwrap();
        let doc = parse(&read(&path).unwrap()).unwrap();
        assert_eq!(
            doc["profiles"]["work"]["model_provider"].as_str(),
            Some("openai")
        );
        assert_eq!(doc["profiles"]["work"]["other"].as_bool(), Some(true));
        assert!(fs::read_to_string(&path).unwrap().contains(PROVIDER)); // unused altered table is preserved
        assert!(detached(&path, &exe, 18765).unwrap());
    }
    #[test]
    fn rescue_preserves_invalid_toml_and_non_utf8_exactly_without_touching_login() {
        for original in [b"invalid = [\n".to_vec(), vec![0xff, 0xfe, 0x00, 0xaa]] {
            let (d, a) = fixture();
            let path = d.path().join("config.toml");
            let exe = d.path().join("pool.exe");
            fs::write(&path, &original).unwrap();
            let auth = d.path().join("auth.json");
            fs::write(&auth, "public fixture login").unwrap();
            let p = recovery_preview(Some(&a.store), &path, &exe, None);
            assert_eq!(p.strategy, RecoveryStrategy::Rescue);
            assert!(p.can_apply);
            let archive = PathBuf::from(p.archive_path.clone().unwrap());
            assert!(!archive.exists());
            apply_recovery(Some(&a.store), &path, &exe, p.strategy, &p.fingerprint).unwrap();
            assert_eq!(fs::read(archive).unwrap(), original);
            assert_eq!(read(&path).unwrap(), MINIMAL);
            assert_eq!(fs::read_to_string(auth).unwrap(), "public fixture login");
            assert!(!journal_path(&path).exists());
        }
    }
    #[test]
    fn interrupted_rescue_resumes_and_never_replaces_a_new_external_file() {
        let (d, _) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        let original = "broken = [";
        fs::write(&path, original).unwrap();
        let archive = path.with_file_name("config.fusion-recovery-fixture.toml");
        let j = Journal {
            version: 1,
            archive: archive.file_name().unwrap().to_string_lossy().into(),
            source_hash: hash(original),
        };
        fs::write(journal_path(&path), serde_json::to_string(&j).unwrap()).unwrap();
        rename_new(&path, &archive).unwrap();
        let p = recovery_preview(None, &path, &exe, None);
        assert_eq!(p.strategy, RecoveryStrategy::Rescue);
        assert_eq!(p.archive_path.as_deref(), Some(archive.to_str().unwrap()));
        apply_recovery(None, &path, &exe, p.strategy, &p.fingerprint).unwrap();
        assert_eq!(read(&archive).unwrap(), original);
        let other = d.path().join("other");
        fs::write(&other, "public external configuration").unwrap();
        assert!(rename_new(&archive, &other).is_err());
        assert_eq!(read(&other).unwrap(), "public external configuration");
        assert!(archive.exists());
    }
    #[test]
    fn recovery_rejects_changed_routing_and_ignores_untrusted_journal_paths() {
        let (d, _) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        fs::write(&path, "model_provider='personal_codex_pool'").unwrap();
        let p = recovery_preview(None, &path, &exe, None);
        fs::write(&path, "model_provider='other'").unwrap();
        assert!(apply_recovery(None, &path, &exe, p.strategy, &p.fingerprint).is_err());
        fs::write(&path, "bad = [").unwrap();
        fs::write(
            journal_path(&path),
            r#"{"version":1,"archive":"../../do-not-touch.toml","source_hash":"x"}"#,
        )
        .unwrap();
        let p = recovery_preview(None, &path, &exe, None);
        assert!(p.can_apply);
        assert_eq!(
            PathBuf::from(p.archive_path.clone().unwrap()).parent(),
            path.parent()
        );
        apply_recovery(None, &path, &exe, p.strategy, &p.fingerprint).unwrap();
        assert!(detached(&path, &exe, 18765).unwrap());
    }
    #[test]
    fn malformed_profile_shape_uses_rescue_instead_of_claiming_a_working_official_config() {
        let (d, _) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        fs::write(&path, "profile='work'\nprofiles=1\n").unwrap();
        let p = recovery_preview(None, &path, &exe, None);
        assert_eq!(p.strategy, RecoveryStrategy::Rescue);
        apply_recovery(None, &path, &exe, p.strategy, &p.fingerprint).unwrap();
        assert!(detached(&path, &exe, 18765).unwrap());
    }
    #[test]
    fn ordinary_exit_without_pool_dependency_does_not_rewrite_user_configuration() {
        let (d, a) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        let original = "model_provider='other'\nmodel='keep'\n";
        fs::write(&path, original).unwrap();
        let p = recovery_preview(Some(&a.store), &path, &exe, None);
        assert_eq!(p.strategy, RecoveryStrategy::Normal);
        apply_recovery(Some(&a.store), &path, &exe, p.strategy, &p.fingerprint).unwrap();
        assert_eq!(read(&path).unwrap(), original);
        assert_eq!(
            recovery_preview(None, &path, &exe, None).strategy,
            RecoveryStrategy::Official
        );
    }
    #[cfg(windows)]
    #[test]
    fn locked_file_returns_an_actionable_plan_without_mutation() {
        use std::os::windows::fs::OpenOptionsExt;
        let (d, _) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        fs::write(&path, "public fixture").unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let p = recovery_preview(None, &path, &exe, None);
        assert!(!p.can_apply);
        assert!(p.warning.is_some());
        assert!(apply_recovery(None, &path, &exe, p.strategy, &p.fingerprint).is_err());
        drop(held);
        assert_eq!(read(&path).unwrap(), "public fixture");
    }
}
