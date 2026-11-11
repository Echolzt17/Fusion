use super::*;
struct NoCredentials;
impl pool_core::vault::Vault for NoCredentials {
    fn get(&self,_:&str)->Result<Option<String>> {panic!("configuration status must not read credentials")}
    fn set(&self,_:&str,_:&str)->Result<()> {panic!("configuration status must not write credentials")}
    fn delete(&self,_:&str)->Result<()> {panic!("configuration status must not delete credentials")}
}
#[tokio::test]
async fn idle_status_and_close_do_not_inspect_clients_or_credentials() {
    let root=tempfile::tempdir().unwrap();let path=root.path().join("config.toml");
    std::fs::write(&path,"model_provider='official'\n").unwrap();
    let store=Arc::new(Store::open(&root.path().join("pool.sqlite")).unwrap());
    let sources=Arc::new(Sources::new(store.clone(),Arc::new(NoCredentials)));
    let clients=ApiClients::new(store,sources,path.clone(),None);
    assert!(clients.statuses().await.unwrap().iter().all(|s|s.mode=="off"));
    assert!(!clients.has_connection(ClientKind::Codex).await.unwrap());
    clients.stop_all().await.unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(),"model_provider='official'\n");
}
#[tokio::test]
async fn retained_connection_requires_close_and_blocks_source_mutation_without_auto_start() {
    let root=tempfile::tempdir().unwrap();let path=root.path().join("config.toml");
    std::fs::write(&path,"model_provider='official'\n").unwrap();
    let store=Arc::new(Store::open(&root.path().join("pool.sqlite")).unwrap());
    store.set(state_key(ClientKind::ClaudeCode),r#"{"source_id":"fixture-source","source_name":"Public fixture"}"#).unwrap();
    let sources=Arc::new(Sources::new(store.clone(),Arc::new(NoCredentials)));
    let clients=ApiClients::new(store,sources,path,None);
    let status=clients.statuses().await.unwrap();
    assert_eq!(status[0].mode,"off");assert_eq!(status[1].mode,"recoveryRequired");
    assert!(clients.ensure_source_idle("fixture-source").await.is_err());
    clients.stop(ClientKind::Codex).await.unwrap();
    assert!(clients.has_connection(ClientKind::ClaudeCode).await.unwrap());
    clients.stop(ClientKind::ClaudeCode).await.unwrap();
    assert!(!clients.has_connection(ClientKind::ClaudeCode).await.unwrap());
    clients.ensure_source_idle("fixture-source").await.unwrap();
}
#[tokio::test]
async fn damaged_pending_identity_blocks_every_source_until_recovered() {
    let root=tempfile::tempdir().unwrap();let path=root.path().join("config.toml");
    std::fs::write(&path,"model_provider='official'\n").unwrap();
    let store=Arc::new(Store::open(&root.path().join("pool.sqlite")).unwrap());
    store.set(state_key(ClientKind::ClaudeCode),"invalid").unwrap();
    let sources=Arc::new(Sources::new(store.clone(),Arc::new(NoCredentials)));
    let clients=ApiClients::new(store,sources,path,None);
    assert!(clients.ensure_source_idle("any-source").await.is_err());
    clients.stop(ClientKind::ClaudeCode).await.unwrap();
    clients.ensure_source_idle("any-source").await.unwrap();
}
