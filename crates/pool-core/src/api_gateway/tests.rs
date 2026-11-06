use super::*;
use crate::{
    api_sources::{NetworkMode, SourceInput},
    storage::Store,
    vault::Vault,
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
};
#[derive(Default)]
struct FakeVault(Mutex<BTreeMap<String, String>>);
impl Vault for FakeVault {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
fn sources() -> (tempfile::TempDir, Arc<Sources>) {
    let temp = tempfile::tempdir().unwrap();
    let sources = Arc::new(Sources::new(
        Arc::new(Store::open(&temp.path().join("gateway.db")).unwrap()),
        Arc::new(FakeVault::default()),
    ));
    (temp, sources)
}
fn add(sources: &Sources, base: &str, protocol: Protocol) -> String {
    sources
        .save(SourceInput {
            id: None,
            name: "Fixture".into(),
            protocol,
            base_url: base.into(),
            model: "fixture-model".into(),
            network: NetworkMode::Direct,
            enabled: true,
            api_key: Some("public-upstream-fixture-key".into()),
        })
        .unwrap()
        .id
}
async fn server(app: Router) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (
        format!("http://{addr}/custom/v1"),
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .unwrap()
}
fn call(client: &reqwest::Client, gateway: &ApiGateway, body: Value) -> reqwest::RequestBuilder {
    client
        .post(format!("{}/responses", gateway.endpoint().base_url()))
        .header("x-fusion-client-key", gateway.endpoint().local_key())
        .json(&body)
}
#[tokio::test]
async fn native_json_credentials_paths_and_logs_are_isolated() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    let app=Router::new().route("/custom/v1/responses",post(move |headers:HeaderMap,Json(value):Json<Value>|{let attempts=observed.clone();async move {
        attempts.fetch_add(1,Ordering::SeqCst);
        assert_eq!(headers["authorization"],"Bearer public-upstream-fixture-key");
        assert!(headers.get("x-api-key").is_none()); assert!(headers.get("chatgpt-account-id").is_none());assert!(headers.get("x-fusion-client-key").is_none());
        assert_eq!(value["store"],true);assert_eq!(value["instructions"],"public-test-instructions");
        Json(json!({"object":"response","id":"resp_native_fixture","status":"completed","output":[],"usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}))
    }}));
    let (base, task) = server(app).await;
    let (_temp, sources) = sources();
    let id = add(&sources, &base, Protocol::Responses);
    let gateway = ApiGateway::start(sources.clone(), ClientKind::Codex, &id)
        .await
        .unwrap();
    let http = client();
    let body = json!({"model":"fixture-model","input":"public-user-fixture","instructions":"public-test-instructions","store":true,"stream":false});
    let response = call(&http, &gateway, body.clone())
        .header("authorization", "Bearer public-client-personal-token")
        .header("chatgpt-account-id", "public-client-account")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["id"],
        "resp_native_fixture"
    );
    gateway.wait_drained().await;
    let logs = gateway.logs();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].usage.as_ref().unwrap().total_tokens, 3);
    let serialized = serde_json::to_string(&logs).unwrap();
    for secret in [
        "public-user-fixture",
        "public-test-instructions",
        "public-client-personal-token",
        "public-upstream-fixture-key",
    ] {
        assert!(!serialized.contains(secret));
    }
    let wrong = http
        .post(format!("{}/responses", gateway.endpoint().base_url()))
        .bearer_auth("wrong-local-key")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    let wrong_route = http
        .post(format!("{}/messages", gateway.endpoint().base_url()))
        .bearer_auth(gateway.endpoint().local_key())
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_route.status(), 404);
    let other = ApiGateway::start(sources, ClientKind::Codex, &id)
        .await
        .unwrap();
    assert_ne!(gateway.endpoint().base_url(), other.endpoint().base_url());
    assert_ne!(gateway.endpoint().local_key(), other.endpoint().local_key());
    let cross = http
        .post(format!("{}/responses", other.endpoint().base_url()))
        .bearer_auth(gateway.endpoint().local_key())
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), 401);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    other.stop().await;
    gateway.stop().await;
    task.abort();
}
#[tokio::test]
async fn stream_tool_calls_pass_through_and_truncation_is_not_replayed() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    let complete="data: {\"type\":\"response.created\",\"response\":{\"object\":\"response\",\"id\":\"resp_stream_fixture\"}}\n\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_fixture\",\"call_id\":\"call_fixture\",\"name\":\"fixture_tool\",\"arguments\":\"{}\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"object\":\"response\",\"id\":\"resp_stream_fixture\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5}}}\n\n";
    let app=Router::new().route("/custom/v1/responses",post(move |Json(value):Json<Value>|{let attempts=observed.clone();async move {
        attempts.fetch_add(1,Ordering::SeqCst);let text=if value["input"]=="truncate" {"data: {\"type\":\"response.output_text.delta\",\"delta\":\"public-fixture-text\"}\n\n"}else{complete};
        Response::builder().header("content-type","text/event-stream").body(Body::from(text)).unwrap()
    }}));
    let (base, task) = server(app).await;
    let (_temp, sources) = sources();
    let id = add(&sources, &base, Protocol::Responses);
    let gateway = ApiGateway::start(sources, ClientKind::Codex, &id)
        .await
        .unwrap();
    let http = client();
    let response = call(
        &http,
        &gateway,
        json!({"model":"fixture-model","input":"tool","stream":true}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.text().await.unwrap(), complete);
    gateway.wait_drained().await;
    let logs = gateway.logs();
    assert_eq!(logs[0].status, "success");
    assert!(logs[0].tool_call_observed);
    assert_eq!(logs[0].usage.as_ref().unwrap().total_tokens, 5);
    if let Ok(response) = call(
        &http,
        &gateway,
        json!({"model":"fixture-model","input":"truncate","stream":true}),
    )
    .send()
    .await
    {
        assert!(response.bytes().await.is_err());
    }
    gateway.wait_drained().await;
    assert_eq!(gateway.logs()[0].status, "stream_interrupted");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    gateway.stop().await;
    task.abort();
}
#[tokio::test]
async fn unknown_or_other_source_response_context_is_rejected_before_dispatch() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    let (base,task)=server(Router::new().route("/custom/v1/responses",post(move ||{let attempts=observed.clone();async move{
        attempts.fetch_add(1,Ordering::SeqCst);Json(json!({"object":"response","id":"resp_owned_fixture","status":"completed","output":[]}))
    }}))).await;
    let (_temp, sources) = sources();
    let a = add(&sources, &base, Protocol::Responses);
    let b = add(&sources, &base, Protocol::Responses);
    let first = ApiGateway::start(sources.clone(), ClientKind::Codex, &a)
        .await
        .unwrap();
    let second = ApiGateway::start(sources, ClientKind::Codex, &b)
        .await
        .unwrap();
    let http = client();
    let first_response = call(
        &http,
        &first,
        json!({"model":"fixture-model","input":"public-fixture"}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(first_response.status(), 200);
    first_response.bytes().await.unwrap();
    for previous in ["resp_owned_fixture", "resp_unknown_fixture"] {
        let blocked=call(&http,&second,json!({"model":"fixture-model","previous_response_id":previous,"input":"public-fixture"})).send().await.unwrap();
        assert_eq!(blocked.status(), 409);
    }
    let allowed=call(&http,&first,json!({"model":"fixture-model","previous_response_id":"resp_owned_fixture","input":"public-fixture"})).send().await.unwrap();
    assert_eq!(allowed.status(), 200);
    allowed.bytes().await.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    first.stop().await;
    second.stop().await;
    task.abort();
}
#[tokio::test]
async fn drain_waits_for_inflight_and_does_not_stop_other_client() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Semaphore::new(0));
    let entered_upstream = entered.clone();
    let release_upstream = release.clone();
    let app=Router::new().route("/custom/v1/responses",post(move ||{let entered=entered_upstream.clone();let release=release_upstream.clone();async move{
        entered.notify_one();let _permit=release.acquire().await.unwrap();Json(json!({"object":"response","id":"resp_drain_fixture","status":"completed","output":[]}))
    }})).route("/custom/v1/messages",post(|headers:HeaderMap|async move {
        assert_eq!(headers["x-api-key"],"public-upstream-fixture-key");assert!(headers.get("authorization").is_none());
        Json(json!({"type":"message","id":"msg_fixture","content":[],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}))
    }));
    let (base, task) = server(app).await;
    let (_temp, sources) = sources();
    let a = add(&sources, &base, Protocol::Responses);
    let b = add(&sources, &base, Protocol::Messages);
    let first = ApiGateway::start(sources.clone(), ClientKind::Codex, &a)
        .await
        .unwrap();
    let second = ApiGateway::start(sources, ClientKind::ClaudeCode, &b)
        .await
        .unwrap();
    let http = client();
    let request = call(
        &http,
        &first,
        json!({"model":"fixture-model","input":"public-fixture"}),
    );
    let flight = tokio::spawn(async move { request.send().await.unwrap().bytes().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert_eq!(first.status().active_requests, 1);
    first.begin_drain();
    let blocked = call(
        &http,
        &first,
        json!({"model":"fixture-model","input":"public-fixture"}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(blocked.status(), 503);
    let other=http.post(format!("{}/messages",second.endpoint().base_url())).header("x-api-key",second.endpoint().local_key()).header("authorization","Bearer public-client-token").json(&json!({"model":"fixture-model","max_tokens":16,"messages":[{"role":"user","content":"public-fixture"}]})).send().await.unwrap();
    assert_eq!(other.status(), 200);
    other.bytes().await.unwrap();
    release.add_permits(1);
    flight.await.unwrap();
    first.wait_drained().await;
    first.stop().await;
    assert!(second.status().running);
    second.stop().await;
    task.abort();
}

#[tokio::test]
async fn terminal_event_releases_upstream_keepalive_and_logs_survive_restart() {
    let app=Router::new().route("/custom/v1/responses",post(||async {
        let stream=async_stream::stream! {
            yield Ok::<_,std::io::Error>("data: {\"type\":\"response.completed\",\"response\":{\"object\":\"response\",\"id\":\"resp_keepalive_fixture\",\"status\":\"completed\",\"model\":\"fixture-returned\",\"output\":[]}}\n\n");
            std::future::pending::<()>().await;
        };
        Response::builder().header("content-type","text/event-stream").body(Body::from_stream(stream)).unwrap()
    }));
    let (base, task) = server(app).await;
    let (temp, sources) = sources();
    let id = add(&sources, &base, Protocol::Responses);
    let gateway = ApiGateway::start(sources.clone(), ClientKind::Codex, &id)
        .await
        .unwrap();
    let http = client();
    let response = call(
        &http,
        &gateway,
        json!({"model":"fixture-model","input":"public-fixture","stream":true}),
    )
    .send()
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), response.text())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), gateway.wait_drained())
        .await
        .unwrap();
    assert_eq!(
        gateway.logs()[0].response_model.as_deref(),
        Some("fixture-returned")
    );
    gateway.stop().await;
    drop(gateway);
    drop(sources);
    let reopened = Sources::new(
        Arc::new(Store::open(&temp.path().join("gateway.db")).unwrap()),
        Arc::new(FakeVault::default()),
    );
    let logs = reopened.logs(ClientKind::Codex).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].status, "success");
    assert!(reopened.logs(ClientKind::ClaudeCode).unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn anthropic_signature_fragments_bind_only_complete_signature_and_usage_is_explicit() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    let app=Router::new().route("/custom/v1/messages",post(move |Json(value):Json<Value>|{let attempts=observed.clone();async move{
        attempts.fetch_add(1,Ordering::SeqCst);
        let partial=value["messages"][0]["content"]=="partial";
        let mut events=String::from("data: {\"type\":\"message_start\",\"message\":{\"type\":\"message\",\"id\":\"msg_signature_fixture\",\"model\":\"fixture-returned\",\"usage\":{\"input_tokens\":4}}}\n\n");
        events.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"fixture-part-one\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"-part-two\"}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
        if !partial { events.push_str("data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":2}}\n\n"); }
        events.push_str("data: {\"type\":\"message_stop\"}\n\n");
        Response::builder().header("content-type","text/event-stream").body(Body::from(events)).unwrap()
    }}));
    let (base, task) = server(app).await;
    let (_temp, sources) = sources();
    let id = add(&sources, &base, Protocol::Messages);
    let gateway = ApiGateway::start(sources.clone(), ClientKind::ClaudeCode, &id)
        .await
        .unwrap();
    let http = client();
    let call = |body| {
        http.post(format!("{}/messages", gateway.endpoint().base_url()))
            .header("x-api-key", gateway.endpoint().local_key())
            .json(&body)
    };
    call(json!({"model":"fixture-model","messages":[{"role":"user","content":"normal"}]}))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    gateway.wait_drained().await;
    assert!(!gateway.logs()[0].usage_partial);
    assert_eq!(gateway.logs()[0].usage.as_ref().unwrap().total_tokens, 6);
    let full=call(json!({"model":"fixture-model","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"public-fixture","signature":"fixture-part-one-part-two"}]}]})).send().await.unwrap();
    assert_eq!(full.status(), 200);
    full.bytes().await.unwrap();
    let fragment=call(json!({"model":"fixture-model","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"public-fixture","signature":"fixture-part-one"}]}]})).send().await.unwrap();
    assert_eq!(fragment.status(), 409);
    assert_eq!(fragment.headers()["x-should-retry"], "false");
    call(json!({"model":"fixture-model","messages":[{"role":"user","content":"partial"}]}))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    gateway.wait_drained().await;
    assert!(gateway.logs()[0].usage_partial);
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    gateway.stop().await;
    task.abort();
}
