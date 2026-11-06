use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct FakeVault {
    values: Mutex<BTreeMap<String, String>>,
    fail_delete: AtomicBool,
}
impl Vault for FakeVault {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.values.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.values.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err("fixture deletion unavailable".into());
        }
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
}
fn input(id: Option<String>, key: Option<&str>) -> SourceInput {
    SourceInput {
        id,
        name: "Fixture".into(),
        protocol: Protocol::Responses,
        base_url: "http://127.0.0.1:12345/custom/v1/".into(),
        model: "fixture-model".into(),
        network: NetworkMode::Direct,
        enabled: true,
        api_key: key.map(str::to_owned),
    }
}
#[test]
fn credentials_are_not_persisted_and_replacements_are_retryable() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&temp.path().join("source.db")).unwrap());
    let vault = Arc::new(FakeVault::default());
    let sources = Sources::new(store.clone(), vault.clone());
    let first = sources
        .save(input(None, Some("public-fixture-key-one")))
        .unwrap();
    assert_eq!(first.base_url, "http://127.0.0.1:12345/custom/v1");
    assert!(!store
        .get(DIRECTORY)
        .unwrap()
        .unwrap()
        .contains("public-fixture-key-one"));
    let unchanged = sources.save(input(Some(first.id.clone()), None)).unwrap();
    assert_eq!(unchanged.revision, 1);
    vault.fail_delete.store(true, Ordering::SeqCst);
    let second = sources
        .save(input(
            Some(first.id.clone()),
            Some("public-fixture-key-two"),
        ))
        .unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(
        sources.snapshot(&second.id).unwrap().key,
        "public-fixture-key-two"
    );
    assert_eq!(vault.values.lock().unwrap().len(), 2);
    assert_eq!(sources.directory().unwrap().cleanup.len(), 1);
    assert!(sources.remove(&second.id).is_err());
    assert!(sources.get(&second.id).is_err());
    assert_eq!(sources.directory().unwrap().cleanup.len(), 2);
    vault.fail_delete.store(false, Ordering::SeqCst);
    sources.remove(&second.id).unwrap();
    assert!(vault.values.lock().unwrap().is_empty());
    assert!(sources
        .save(input(Some(second.id), Some("public-fixture-key-three")))
        .is_err());
    let fresh = sources
        .save(input(None, Some("public-fixture-key-three")))
        .unwrap();
    assert_ne!(fresh.id, first.id);
}
#[test]
fn url_validation_preserves_paths_and_rejects_credential_leaks() {
    assert_eq!(
        validate_base_url("https://example.test/team/custom/v1/").unwrap(),
        "https://example.test/team/custom/v1"
    );
    for raw in [
        "https://key@example.test/v1",
        "https://example.test/v1?key=x",
        "https://example.test/v1#x",
        "http://example.test/v1",
        "file:///C:/secret",
        "https://example.test/a%2fb",
    ] {
        assert!(validate_base_url(raw).is_err(), "{raw}");
    }
    assert!(validate_base_url("http://[::1]:123/v1").is_ok());
}
#[test]
fn context_ownership_is_durable_and_never_rebound() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("contexts.db");
    let store = Arc::new(Store::open(&path).unwrap());
    let vault = Arc::new(FakeVault::default());
    let sources = Sources::new(store.clone(), vault.clone());
    let key = context_hash("response", "public-fixture-response");
    assert!(sources
        .contexts("source-a/1", std::slice::from_ref(&key), false)
        .is_err());
    sources
        .contexts("source-a/1", std::slice::from_ref(&key), true)
        .unwrap();
    assert!(sources
        .contexts("source-b/1", std::slice::from_ref(&key), true)
        .is_err());
    drop(sources);
    drop(store);
    let sources = Sources::new(Arc::new(Store::open(&path).unwrap()), vault);
    sources
        .contexts("source-a/1", std::slice::from_ref(&key), false)
        .unwrap();
    assert!(sources.contexts("source-a/2", &[key], false).is_err());
}
#[tokio::test]
async fn checks_are_explicit_and_do_not_certify_stream_or_tools() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app=axum::Router::new().route("/custom/v1/models",axum::routing::get(||async {axum::Json(json!({"data":[{"id":"fixture-model"}]}))}))
        .route("/custom/v1/responses",axum::routing::post(|axum::Json(value):axum::Json<Value>|async move {
            assert_eq!(value["input"],"Reply OK."); assert_eq!(value["stream"],false);
            axum::Json(json!({"object":"response","id":"resp_fixture_check","status":"completed","output":[]}))
        }));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let sources = Sources::new(
        Arc::new(Store::open(&temp.path().join("check.db")).unwrap()),
        Arc::new(FakeVault::default()),
    );
    let mut source_input = input(None, Some("public-fixture-key"));
    source_input.base_url = format!("http://{addr}/custom/v1");
    let source = sources.save(source_input).unwrap();
    assert_eq!(
        sources.basic_check(&source.id).await.unwrap().status,
        "passed"
    );
    assert_eq!(
        sources.test_request(&source.id).await.unwrap().status,
        "passed"
    );
    let verification = sources.get(&source.id).unwrap().verification;
    assert_eq!(verification.stream.status, "unknown");
    assert_eq!(verification.tools.status, "unknown");
    task.abort();
}
