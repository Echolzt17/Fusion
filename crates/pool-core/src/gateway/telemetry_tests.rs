use super::tests::{ctx, serve};
use super::*;
use crate::tests::{add, fixture};

#[tokio::test]
async fn models_are_captured_for_stream_tools_compact_images_and_interruption() {
    let (_dir, accounts) = fixture();
    add(&accounts, "a");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let (upstream, up)=serve(Router::new()
        .route("/responses",post(move |Json(v):Json<Value>| { let count=count.clone(); async move {
            count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
            let created="data: {\"type\":\"response.created\",\"response\":{\"model\":\"returned-model\"}}\n\n";
            let done="data: {\"type\":\"response.completed\",\"response\":{\"model\":\"returned-model\",\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"name\":\"private-tool\",\"arguments\":\"private-arguments\"}],\"usage\":{\"input_tokens\":10,\"output_tokens\":2}}}\n\n";
            ([("content-type","text/event-stream")],if v["model"]=="interrupt" { created.to_owned() } else {format!("{created}{done}")})
        }}))
        .route("/responses/compact",post(|| async { Json(json!({"model":"compact-model","output":[],"usage":{"input_tokens":3,"output_tokens":1}})) }))
        .route("/images/edits",post(|| async { Json(json!({"model":"image-edit-model","data":[{"b64_json":"private-image"}]})) }))
        .route("/images/generations",post(|| async { Json(json!({"model":"image-model","data":[{"b64_json":"private-image"}],"usage":{"input_tokens":2,"output_tokens":1}})) }))
    ).await;
    let (base, gw) = serve(router(ctx(accounts.clone(), upstream))).await;
    let client = reqwest::Client::new();
    for (path, model, stream) in [
        ("responses", "requested-model", true),
        ("responses", "requested-model", false),
        ("responses/compact", "requested-model", false),
        ("images/generations", "requested-model", false),
        ("images/edits", "requested-model", false),
        ("responses", "interrupt", true),
    ] {
        let response=client.post(format!("{base}/v1/{path}")).bearer_auth("test-key").header("session_id","private-session")
            .json(&json!({"model":model,"input":"private-prompt","stream":stream,"images":[{"image_url":"data:image/png;base64,AA=="}]})).send().await;
        match response {
            Ok(response) => {
                let _ = response.bytes().await;
            }
            Err(_) if model == "interrupt" => {}
            Err(error) => panic!("unexpected fixture connection error: {error}"),
        }
    }
    let logs = accounts.store.logs().unwrap();
    assert_eq!(logs.len(), 6);
    assert!(logs.iter().all(|l| l.started_at.is_some()
        && l.requested_model.is_some()
        && l.response_model.is_some()));
    let interrupted = logs
        .iter()
        .find(|l| l.requested_model.as_deref() == Some("interrupt"))
        .unwrap();
    assert_eq!(
        interrupted.response_model.as_deref(),
        Some("returned-model")
    );
    assert_eq!(interrupted.status, "stream_interrupted");
    assert!(interrupted.usage.is_none());
    assert_eq!(
        logs.iter()
            .find(|l| l.operation == "compact")
            .unwrap()
            .response_model
            .as_deref(),
        Some("compact-model")
    );
    assert_eq!(
        logs.iter()
            .find(|l| l.operation == "image_generate")
            .unwrap()
            .response_model
            .as_deref(),
        Some("image-model")
    );
    assert_eq!(
        logs.iter()
            .find(|l| l.operation == "image_edit")
            .unwrap()
            .response_model
            .as_deref(),
        Some("image-edit-model")
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    let serialized = serde_json::to_string(&logs).unwrap();
    for secret in [
        "private-session",
        "private-tool",
        "private-arguments",
        "private-prompt",
        "private-image",
    ] {
        assert!(!serialized.contains(secret));
    }
    assert_eq!(accounts.store.conversations().unwrap()[0].requests, 6);
    gw.abort();
    up.abort();
}

#[test]
fn model_metadata_rejects_arbitrary_strings_and_survives_split_events() {
    use crate::usage::model_name;
    for v in [
        json!("private prompt"),
        json!("private\nheader"),
        json!("x".repeat(129)),
        json!(1),
        Value::Null,
    ] {
        assert!(model_name(&v).is_none());
    }
    let (_dir, a) = fixture();
    add(&a, "a");
    let c = ctx(a, String::new());
    let mut observer = Observer::default();
    for byte in
        b"data: {\"type\":\"response.created\",\"response\":{\"model\":\"gpt-test-1.2\"}}\r\n\r\n"
    {
        observer.push(&[*byte], &c, "a").unwrap();
    }
    assert_eq!(observer.response_model.as_deref(), Some("gpt-test-1.2"));
    assert!(observer.usage.is_none());
}
