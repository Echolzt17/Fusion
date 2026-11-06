use super::*;
#[test]
fn detached_checks_effective_profile_and_ignores_retained_definitions() {
    let root = tempfile::tempdir().unwrap(); let path = root.path().join("config.toml");
    fs::write(&path,"model_provider='official'\n[model_providers.fusion_api_history]\nbase_url='http://127.0.0.1:1/fusion-api/old/v1'\n").unwrap();
    assert!(detached(&path).unwrap());
    fs::write(&path,"model_provider='official'\nprofile='work'\n[profiles.work]\nmodel_provider='fusion_api_active'\n").unwrap();
    assert!(!detached(&path).unwrap());
    fs::write(&path,"model_provider='renamed'\n[model_providers.renamed]\nbase_url='http://127.0.0.1:1/fusion-api/old/v1'\n").unwrap();
    assert!(!detached(&path).unwrap());
    fs::write(&path,"model_provider=2").unwrap(); assert!(detached(&path).is_err());
}
#[test]
fn provider_identity_scopes_source_and_revision() {
    assert_ne!(provider_id("source-a",1),provider_id("source-b",1));
    assert_ne!(provider_id("source-a",1),provider_id("source-a",2));
    assert!(!provider_id("source-a;password",1).contains(';'));
}
#[cfg(windows)]
#[test]
fn restore_preserves_unrelated_edits_and_protects_local_key() {
    let root = tempfile::tempdir().unwrap(); let path=root.path().join("config.toml");
    let store=Store::open(&root.path().join("pool.sqlite")).unwrap();
    fs::write(&path,"model='original'\nmodel_provider='official'\nprofile='work'\n[profiles.work]\nmodel_provider='profile-provider'\nmodel='profile-model'\n[features]\nimage_generation=true\nother=true\n").unwrap();
    apply(&store,&path,"a",1,"fixture-model","http://127.0.0.1:12345/fusion-api/source/1/v1","public-fixture-local-key").unwrap();
    assert!(!store.get(BACKUP).unwrap().unwrap().contains("public-fixture-local-key"));
    assert!(!detached(&path).unwrap()); verify_applied(&store,&path).unwrap();
    let current=read(&path).unwrap(); fs::write(&path,format!("# outside edit\n{current}\n[plugins.sample]\nenabled=true\n")).unwrap();
    assert!(restore(&store,&path).unwrap());
    assert!(pending(&store).unwrap());
    let restored=parse(&read(&path).unwrap()).unwrap();
    assert_eq!(restored["model"].as_str(),Some("original"));
    assert_eq!(restored["profiles"]["work"]["model_provider"].as_str(),Some("profile-provider"));
    assert_eq!(restored["profiles"]["work"]["model"].as_str(),Some("profile-model"));
    assert_eq!(restored["features"]["image_generation"].as_bool(),Some(true));
    assert!(restored["plugins"]["sample"]["enabled"].as_bool().unwrap());
    assert!(detached(&path).unwrap()); finish_close(&store,&path).unwrap(); assert!(!pending(&store).unwrap());
    // Same source identity can update only its registered provider when restarted.
    apply(&store,&path,"a",1,"fixture-model","http://127.0.0.1:23456/fusion-api/source/1/v1","public-fixture-next-key").unwrap();
    restore(&store,&path).unwrap();finish_close(&store,&path).unwrap();
}
#[cfg(windows)]
#[test]
fn external_provider_edits_and_post_exit_conflicts_keep_recovery() {
    let root=tempfile::tempdir().unwrap();let path=root.path().join("config.toml");
    let store=Store::open(&root.path().join("pool.sqlite")).unwrap();
    fs::write(&path,"model_provider='official'\n").unwrap();
    apply(&store,&path,"a",1,"fixture","http://127.0.0.1:12345/fusion-api/a/1/v1","public-fixture-local-key").unwrap();
    let expected=read(&path).unwrap();fs::write(&path,expected.replace("fixture\"","changed\"")).unwrap();
    assert!(restore(&store,&path).is_err()); assert!(pending(&store).unwrap());
    fs::write(&path,expected).unwrap();restore(&store,&path).unwrap();
    let current=read(&path).unwrap();fs::write(&path,current.replace("official","external")).unwrap();
    assert!(finish_close(&store,&path).is_err());assert!(pending(&store).unwrap());
}
