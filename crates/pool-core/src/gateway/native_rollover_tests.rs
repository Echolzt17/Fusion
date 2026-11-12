//! Actual desktop engine, public fake login and two fake subscriptions only.
//! No real client state or upstream service is read or changed.
use super::*;
use crate::tests::{add, fixture};
use std::{path::Path, process::Stdio, sync::Mutex as StdMutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Native {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
    completed: Option<Value>,
}
impl Native {
    async fn start(engine: &Path, root: &Path) -> Self {
        let mut child = tokio::process::Command::from(crate::compatibility::probe::isolated_command(engine, root))
            .arg("app-server").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .kill_on_drop(true).spawn().expect("start isolated actual native engine");
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut native = Self { child, input, output, id: 0, completed: None };
        native.call("initialize", json!({"clientInfo":{"name":"fusion-native-rollover","version":"1"},"capabilities":{"experimentalApi":true}})).await;
        native.input.write_all(b"{\"method\":\"initialized\"}\n").await.unwrap();
        native
    }
    async fn next(&mut self) -> Value {
        let line = self.output.next_line().await.unwrap().expect("native engine must remain available");
        assert!(line.len() <= 64 * 1024 * 1024, "bounded isolated native RPC output");
        serde_json::from_str(&line).expect("native JSON-RPC response")
    }
    async fn raw_call(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        let mut request = serde_json::to_vec(&json!({"id":id,"method":method,"params":params})).unwrap();
        request.push(b'\n');
        self.input.write_all(&request).await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let message = self.next().await;
                if message["method"] == "turn/completed" { self.completed = Some(message.clone()); }
                if message["id"] == id {
                    return message;
                }
                assert!(!(message.get("id").is_some() && message.get("method").is_some()), "no tool or approval action may be granted by this fixture");
            }
        }).await.expect("bounded isolated native RPC call")
    }
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let message = self.raw_call(method, params).await;
        assert!(message.get("error").is_none(), "isolated native RPC rejected {method}");
        message["result"].clone()
    }
    async fn completed(&mut self, thread: &str, turn: &str, stage: &str, diagnostic: Option<&StdMutex<Vec<Value>>>) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let message = match self.completed.take() { Some(message) => message, None => self.next().await };
                if message["method"] == "turn/completed" && message["params"]["threadId"] == thread && message["params"]["turn"]["id"] == turn {
                    if message["params"]["turn"]["status"] != "completed" {
                        if let Some(inputs) = diagnostic { eprintln!("actual-native-input-structure: {}", serde_json::to_string(&*inputs.lock().unwrap()).unwrap()); }
                    }
                    assert_eq!(message["params"]["turn"]["status"], "completed", "{stage}: actual native turn must complete without a manual retry; public-fake error={}", message["params"]["turn"]["error"]);
                    return;
                }
                assert!(!(message.get("id").is_some() && message.get("method").is_some()), "fixture must not execute extra native tools");
            }
        }).await.expect("actual native turn must complete through the pool");
    }
    async fn compacted(&mut self, thread: &str) {
        self.completed = None;
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut item_done = false;
            loop {
                let message = self.next().await;
                if message["params"]["threadId"] != thread { continue; }
                if message["method"] == "item/completed" && message["params"]["item"]["type"] == "contextCompaction" { item_done = true; }
                if item_done && message["method"] == "turn/completed" {
                    assert_eq!(message["params"]["turn"]["status"], "completed");
                    return;
                }
            }
        }).await.expect("actual local readable compaction must complete");
    }
    async fn close(mut self) {
        self.child.kill().await.unwrap();
        let _ = self.child.wait().await;
    }
}
#[derive(Clone)]
struct Seen {
    account: String,
    thread: String,
    old_cipher: bool,
    additional_tools: bool,
    readable_summary: bool,
    opaque_compaction: bool,
}
fn fixture_response(n: usize, summary: bool) -> String {
    let reasoning = json!({"type":"reasoning","id":format!("fixture-reason-{n}"),"summary":[],
        "encrypted_content":if n == 0 { "OLD_ACCOUNT_PRIVATE_REASONING" } else { "NEW_ACCOUNT_PRIVATE_REASONING" }});
    let message = json!({"type":"message","id":format!("fixture-message-{n}"),"role":"assistant","phase":"final_answer","status":"completed",
        "content":[{"type":"output_text","text":if summary { "PUBLIC LOCAL COMPACTION SUMMARY".into() } else { format!("fixture-native-turn-{n}") },"annotations":[]}]});
    let items = if summary { vec![message] } else { vec![reasoning, message] };
    let mut events = vec![json!({"type":"response.created","response":{"id":format!("fixture-response-{n}"),"object":"response","status":"in_progress"}})];
    for (index, item) in items.iter().enumerate() {
        events.push(json!({"type":"response.output_item.added","output_index":index,"item":item}));
        events.push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
    }
    events.push(json!({"type":"response.completed","response":{"id":format!("fixture-response-{n}"),"status":"completed","output":items,
        "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}));
    events.iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect()
}

async fn native_rollover_scenario(engine: &Path, local_compaction: bool) {
    let (_database, accounts) = fixture();
    add(&accounts, "a"); add(&accounts, "b");
    accounts.vault.set("gateway-key", "fusion-public-fixture-key").unwrap();
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserved.local_addr().unwrap().port(); drop(reserved);
    let mut settings = accounts.store.settings().unwrap(); settings.port = port;
    accounts.store.set("settings", &serde_json::to_string(&settings).unwrap()).unwrap();
    let seen = Arc::new(StdMutex::new(Vec::<Seen>::new()));
    let compact_request = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let compact_expected = compact_request.clone();
    let captured = seen.clone();
    let upstream = Router::new().route("/models", get(|| async { Json(json!({"models":[],"data":[]})) }))
        .route("/responses", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let captured = captured.clone(); let compact_expected = compact_expected.clone(); async move {
                assert_eq!(headers["authorization"], "Bearer fake-access-token", "personal fixture login must never reach the upstream");
                let metadata: Value = serde_json::from_str(headers["x-codex-turn-metadata"].to_str().unwrap()).unwrap();
                let mut requests = captured.lock().unwrap();
                let n = requests.len();
                requests.push(Seen {
                    account: headers["chatgpt-account-id"].to_str().unwrap().to_owned(),
                    thread: metadata["thread_id"].as_str().unwrap().to_owned(),
                    old_cipher: body.to_string().contains("OLD_ACCOUNT_PRIVATE_REASONING"),
                    additional_tools: body["input"].as_array().unwrap().iter().any(|item| item["type"] == "additional_tools"),
                    readable_summary: body.to_string().contains("PUBLIC LOCAL COMPACTION SUMMARY"),
                    opaque_compaction: body["input"].as_array().unwrap().iter().any(|item| matches!(item["type"].as_str(), Some("compaction" | "compaction_trigger"))),
                });
                ([("content-type", "text/event-stream")], fixture_response(n, compact_expected.swap(false, std::sync::atomic::Ordering::SeqCst)))
            }
        }));
    let (upstream, up) = super::tests::serve(upstream).await;
    let gateway = Gateway::new(accounts.clone());
    *gateway.test_upstream.lock().unwrap() = Some(upstream);
    gateway.scheduler.select_account(Some("a".into())).unwrap();
    gateway.start().await.unwrap();
    let inbound = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let observed_input = inbound.clone();
    let proxy = Router::new().fallback(axum::routing::any(move |request: axum::extract::Request| {
        let observed = observed_input.clone(); async move {
            let (parts, body) = request.into_parts();
            let body = axum::body::to_bytes(body, 8 * 1024 * 1024).await.unwrap();
            if parts.method == http::Method::POST && parts.uri.path() == "/v1/responses" {
                let parsed: Value = serde_json::from_slice(&body).unwrap();
                let metadata: Value = serde_json::from_str(parts.headers["x-codex-turn-metadata"].to_str().unwrap()).unwrap();
                observed.lock().unwrap().push(json!({"portable":portable::project(&parsed).is_some(),
                    "previousResponseId":!parsed["previous_response_id"].is_null(),"conversation":!parsed["conversation"].is_null(),
                    "turn":metadata["turn_id"],"input":parsed["input"].as_array().map(|items|items.iter().map(|item|json!({
                        "type":item["type"],"role":item["role"],"contentTypes":item["content"].as_array().map(|parts|parts.iter().map(|p|p["type"].clone()).collect::<Vec<_>>())})).collect::<Vec<_>>())}));
            }
            let mut headers = parts.headers; headers.remove(http::header::HOST); headers.remove(http::header::CONNECTION);
            let response = reqwest::Client::builder().no_proxy().build().unwrap()
                .request(parts.method, format!("http://127.0.0.1:{port}{}",parts.uri)).headers(headers).body(body).send().await.unwrap();
            let status = response.status(); let headers = response.headers().clone();
            (status,headers,Body::from_stream(response.bytes_stream().map(|chunk|chunk.map_err(std::io::Error::other))))
        }
    }));
    let (proxy_base, proxy_task) = super::tests::serve(proxy).await;
    let official_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let official_count = official_calls.clone();
    let (auxiliary, auxiliary_task) = super::tests::serve(Router::new()
        .route("/v1/responses", post(move || {
            let count = official_count.clone(); async move {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                StatusCode::BAD_REQUEST
            }
        })).fallback(|| async {
        Json(json!({"models":[],"data":[],"plan_type":"plus","rate_limit":{"allowed":false,"limit_reached":true,
            "primary_window":{"used_percent":100,"limit_window_seconds":18000,"reset_after_seconds":3600,"reset_at":2000000000}}}))
    })).await;
    let root = tempfile::Builder::new().prefix("fusion-native-rollover-").tempdir().unwrap();
    crate::compatibility::probe::configure(root.path(), proxy_base.trim_start_matches("http://").parse().unwrap()).unwrap();
    let path = root.path().join("config.toml");
    let mut config: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(config["model_provider"].as_str(), Some("personal_codex_pool"));
    assert_eq!(config["model_providers"]["personal_codex_pool"]["name"].as_str(), Some("OpenAI"));
    config["chatgpt_base_url"] = toml_edit::value(&auxiliary);
    std::fs::write(&path, config.to_string()).unwrap();
    let login = std::fs::read(root.path().join("auth.json")).unwrap();
    let mut native = Native::start(engine, root.path()).await;
    let auth = native.call("getAuthStatus", json!({"includeToken":false,"refreshToken":false})).await;
    assert_eq!(auth["authMethod"], "chatgpt");
    let rates = native.call("account/rateLimits/read", json!({})).await;
    assert_eq!(rates["rateLimits"]["primary"]["usedPercent"], 100, "personal fixture is exhausted while pool subscriptions remain independent");
    let created = native.call("thread/start", json!({"cwd":root.path(),"model":"gpt-6.1-sol","approvalPolicy":"never","sandbox":"read-only"})).await;
    let thread = created["thread"]["id"].as_str().unwrap().to_owned();
    let first_turn = native.call("turn/start", json!({"threadId":thread,"input":[{"type":"text","text":"Public fixture first turn. Reply only."}]})).await;
    native.completed(&thread, first_turn["turn"]["id"].as_str().unwrap(), "first A", None).await;
    assert_eq!(seen.lock().unwrap().len(), 1, "first native turn sends exactly once");
    assert_eq!(seen.lock().unwrap()[0].account, "chatgpt-a");
    if local_compaction {
        compact_request.store(true, std::sync::atomic::Ordering::SeqCst);
        native.call("thread/compact/start", json!({"threadId":thread})).await;
        native.compacted(&thread).await;
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2, "local compaction uses one ordinary Responses request");
        assert_eq!(requests[1].account, "chatgpt-a");
        assert!(!requests[1].opaque_compaction, "unsupported remote compaction must use readable local compaction");
    }
    let mut exhausted = accounts.store.account("a").unwrap();
    exhausted.quota = Some(json!({"updatedAt":crate::now(),"rateLimit":{"allowed":false,"limit_reached":true,
        "primary_window":{"used_percent":100,"reset_at":crate::now()+3600}}}));
    accounts.store.save_account(&exhausted).unwrap();
    let second_turn = native.call("turn/start", json!({"threadId":thread,"input":[{"type":"text","text":"Public fixture second turn in this same chat. Continue."}]})).await;
    // A failure reports only structural public-fixture metadata, never schemas,
    // instructions, request content or encrypted values.
    native.completed(&thread, second_turn["turn"]["id"].as_str().unwrap(), "second B", Some(&inbound)).await;
    let read = native.call("thread/read", json!({"threadId":thread,"includeTurns":false})).await;
    assert_eq!(read["thread"]["id"], thread, "original local native thread remains unchanged");
    native.close().await;
    assert_eq!(std::fs::read(root.path().join("auth.json")).unwrap(), login, "personal native login is untouched");
    let requests = seen.lock().unwrap().clone();
    let expected = if local_compaction { 3 } else { 2 };
    assert_eq!(requests.len(), expected, "actual native turns and no upstream replay");
    let last = requests.last().unwrap();
    assert_eq!(last.account, "chatgpt-b", "second native turn must actually reach the backup account");
    assert!(requests.iter().all(|request| request.thread == thread));
    assert!(requests[0].additional_tools);
    assert!(!last.old_cipher, "old account reasoning cannot be sent to the backup");
    assert!(!last.opaque_compaction);
    if local_compaction { assert!(last.readable_summary, "actual native readable compaction summary must reach B"); }
    assert_eq!(accounts.store.binding(&binding_key("thread", &thread)).unwrap().as_deref(), Some("b"));
    assert_eq!(accounts.store.binding(&binding_key("opaque", "OLD_ACCOUNT_PRIVATE_REASONING")).unwrap().as_deref(), Some("a"));
    let logs = accounts.store.logs().unwrap();
    let response_accounts: Vec<_> = logs.iter().filter(|log| log.operation == "responses").map(|log| log.account.as_deref()).collect();
    assert_eq!(response_accounts.len(), expected);
    assert!(response_accounts.contains(&Some("a")) && response_accounts.contains(&Some("b")));
    let serialized = serde_json::to_string(&logs).unwrap();
    assert!(!serialized.contains("PRIVATE_REASONING") && !serialized.contains("Public fixture"));
    gateway.stop().await;
    // Restoring official access must not make this pool-owned chat send B's
    // encrypted history through the user's unrelated official account.
    config["model_provider"] = toml_edit::value("openai");
    config["openai_base_url"] = toml_edit::value(format!("{auxiliary}/v1"));
    std::fs::write(&path, config.to_string()).unwrap();
    let mut restored = Native::start(engine, root.path()).await;
    let denied = restored.raw_call("thread/resume", json!({"threadId":thread})).await;
    assert_eq!(denied["error"]["data"]["type"], "modelProviderMismatch", "official access must refuse this pool-owned history before a business request");
    assert_eq!(official_calls.load(std::sync::atomic::Ordering::SeqCst), 0, "restored official account must receive no pool business request or ciphertext");
    restored.close().await;
    assert_eq!(std::fs::read(root.path().join("auth.json")).unwrap(), login);
    proxy_task.abort(); up.abort(); auxiliary_task.abort();
    println!("actual-native-rollover: local_compaction={local_compaction}; same local thread; upstream A then B; original reasoning binding retained; restored official access rejects pool history; login unchanged");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "FUSION_NATIVE_ENGINE must be the actual desktop engine; local fake services and temporary HOME only"]
async fn actual_native_same_thread_rolls_from_exhausted_a_to_b_without_foreign_reasoning() {
    let engine = std::env::var_os("FUSION_NATIVE_ENGINE").expect("FUSION_NATIVE_ENGINE must identify the verified actual desktop engine");
    native_rollover_scenario(Path::new(&engine), false).await;
    native_rollover_scenario(Path::new(&engine), true).await;
}
