use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[test]
fn operation_records_never_persist_the_history_and_cancelled_or_unknown_work_is_distinct() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("pool.sqlite")).unwrap();
    let r = Record {
        id: "operation".into(),
        source_id: "source".into(),
        target: "pool".into(),
        account_id: Some("b".into()),
        target_id: Some("target".into()),
        recent_turns: 0,
        phase: "sending".into(),
        message: "发送结果未确认".into(),
        at: 0,
    };
    save(&store, &r).unwrap();
    assert_eq!(find(&store, "operation").unwrap().phase, "sending");
    let raw = store.get(RECORDS).unwrap().unwrap();
    assert!(!raw.contains("history") && !raw.contains("encrypted_content"));
    assert!(find(&store, "unrelated").is_err());
}

#[tokio::test]
#[ignore = "uses the installed desktop engine with an isolated temporary home and a loopback fake upstream"]
async fn actual_engine_readable_handoff_crosses_providers_without_old_ciphertext() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let stop = running.clone();
    let observed = Arc::new(Mutex::new(Vec::<(String, bool, bool)>::new()));
    let seen = observed.clone();
    let server = std::thread::spawn(move || {
        while stop.load(Ordering::SeqCst) {
            let (mut stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut raw = Vec::new();
            let mut byte = [0u8; 1];
            while !raw.ends_with(b"\r\n\r\n") {
                if stream.read_exact(&mut byte).is_err() {
                    break;
                }
                raw.push(byte[0]);
                if raw.len() > 16 * 1024 {
                    break;
                }
            }
            let header = String::from_utf8_lossy(&raw);
            let path = header
                .lines()
                .next()
                .unwrap_or("")
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_owned();
            let size = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|n| n.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            assert!(size < 1024 * 1024);
            let mut body = vec![0u8; size];
            if stream.read_exact(&mut body).is_err() {
                continue;
            }
            let (kind, output) = if size == 0 {
                (
                    "application/json",
                    json!({"data":[],"models":[]}).to_string(),
                )
            } else {
                let v: Value = serde_json::from_slice(&body).unwrap();
                let flat = v.to_string();
                seen.lock().unwrap().push((
                    path.clone(),
                    flat.contains("old-opaque-fixture"),
                    flat.contains("原聊天"),
                ));
                let msg = json!({"type":"message","id":"msg_fixture","role":"assistant","phase":"final_answer","status":"completed","content":[{"type":"output_text","text":"public fixture reply","annotations":[]}]});
                let reason = json!({"type":"reasoning","id":"rs_fixture","summary":[],"encrypted_content":"old-opaque-fixture"});
                let items = vec![reason, msg];
                let mut events = vec![
                    json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress"}}),
                ];
                for (i, item) in items.iter().enumerate() {
                    events.push(
                        json!({"type":"response.output_item.added","output_index":i,"item":item}),
                    );
                    events.push(
                        json!({"type":"response.output_item.done","output_index":i,"item":item}),
                    );
                }
                events.push(json!({"type":"response.completed","response":{"id":"resp_fixture","status":"completed","output":items,"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}));
                (
                    "text/event-stream",
                    events
                        .iter()
                        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
                        .collect::<String>(),
                )
            };
            let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{output}",output.len());
        }
    });
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let guard = Stop(running);
    let root = tempfile::tempdir().unwrap();
    let engine = crate::codex_window::installed_client().unwrap().engine();
    let config = |provider: &str| {
        let mut s = format!("model='fixture-model'\nmodel_provider='{provider}'\n");
        for p in ["fixture_a", "fixture_b", "fixture_official"] {
            s+=&format!("\n[model_providers.{p}]\nname='{p}'\nbase_url='http://{address}/{p}/v1'\nwire_api='responses'\nrequires_openai_auth=false\n");
        }
        s
    };
    std::fs::write(root.path().join("config.toml"), config("fixture_a")).unwrap();
    let mut rpc = Rpc::start(&engine, root.path()).await.unwrap();
    let created = rpc
        .call(
            "thread/start",
            json!({"cwd":root.path(),"approvalPolicy":"never","sandbox":"read-only"}),
        )
        .await
        .unwrap();
    let mut source = created["thread"]["id"].as_str().unwrap().to_owned();
    rpc.call("turn/start",json!({"threadId":source,"input":[{"type":"text","text":"Public fixture: acknowledge only. Do not call tools."}]})).await.unwrap();
    rpc.completed(&source).await.unwrap();
    rpc.close().await;
    // Exercise multiple hops, including back from the independent target.
    for provider in ["fixture_b", "fixture_official", "fixture_a"] {
        std::fs::write(root.path().join("config.toml"), config(provider)).unwrap();
        let mut rpc = Rpc::start(&engine, root.path()).await.unwrap();
        let h = history(&mut rpc, &source, 0).await.unwrap();
        let prepared = handoff::prepare(&h, 0).unwrap();
        assert!(!prepared.message.contains("old-opaque-fixture"));
        let next=rpc.call("thread/start",json!({"modelProvider":provider,"cwd":root.path(),"approvalPolicy":"never","sandbox":"read-only"})).await.unwrap();
        source = next["thread"]["id"].as_str().unwrap().to_owned();
        rpc.call(
            "turn/start",
            json!({"threadId":source,"input":[{"type":"text","text":prepared.message}]}),
        )
        .await
        .unwrap();
        rpc.completed(&source).await.unwrap();
        let restored = history(&mut rpc, &source, 0).await.unwrap();
        assert!(!restored["turns"].as_array().unwrap().is_empty());
        rpc.close().await;
    }
    let observed = observed.lock().unwrap();
    assert_eq!(
        observed.len(),
        4,
        "one source plus exactly three user-triggered handoffs"
    );
    assert!(
        observed.iter().all(|(_, opaque, _)| !opaque),
        "no request may carry the previous account's encrypted state"
    );
    drop(observed);
    drop(guard);
    server.join().unwrap();
}
