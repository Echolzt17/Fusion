use super::tests::{ctx, serve};
use super::*;
use crate::tests::{add, fixture};
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn service_and_stream_failures_keep_preferred_account_and_capture_no_payloads() {
    let (_d, accounts) = fixture();
    add(&accounts, "a");
    add(&accounts, "b");
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let (upstream,server)=serve(Router::new().route("/responses",post(move |headers:HeaderMap,Json(_):Json<Value>| {
        let n=count.fetch_add(1,Ordering::SeqCst);
        assert_eq!(headers["chatgpt-account-id"],"chatgpt-a");
        async move {
            if n<2 { return (StatusCode::SERVICE_UNAVAILABLE,"private-upstream-body").into_response(); }
            let mut wire=concat!("data: {\"type\":\"response.output_text.delta\",\"delta\":\"private-text\"}\n\n",
                "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"arguments\":\"private-arguments\"}}\n\n").to_string();
            if n>=4 { wire.push_str("data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"); }
            ([("content-type","text/event-stream")],wire).into_response()
        }
    }))).await;
    let (base, gateway) = serve(router(ctx(accounts.clone(), upstream))).await;
    for n in 0..6 {
        let response = accounts
            .local_http
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .json(&json!({"input":"private-prompt","stream":n==5}))
            .send()
            .await
            .unwrap();
        let _ = response.bytes().await;
    }
    for _ in 0..100 {
        if accounts.store.logs().unwrap().len() == 6 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(hits.load(Ordering::SeqCst), 6);
    let logs = accounts.store.logs().unwrap();
    assert_eq!(logs.len(), 6);
    assert!(logs.iter().all(|l| l.account.as_deref() == Some("a")));
    for log in &logs {
        let d = log.diagnostic.as_ref().unwrap();
        let t = d.execution.as_ref().unwrap().transport.as_ref().unwrap();
        assert_eq!(t.attempts.len(), 1);
        assert!(t.request_bytes.unwrap() > 0);
        assert_eq!(t.attempts[0].body_bytes_yielded, t.request_bytes.unwrap());
        if d.upstream_status == Some(200) {
            assert!(t.output_event_observed && t.tool_call_observed && t.response_bytes > 0);
        }
        if d.upstream_status == Some(503) {
            assert_eq!(
                d.details.as_ref().unwrap().origin.as_deref(),
                Some("upstream_service")
            );
        }
        let safe = serde_json::to_string(log).unwrap();
        assert!(!safe.contains("private-"));
    }
    let summary = accounts
        .store
        .reliability_summary(crate::now())
        .unwrap()
        .unwrap();
    assert_eq!(summary.buckets.iter().map(|b| b.total()).sum::<u64>(), 6);
    assert_eq!(summary.buckets.iter().map(|b| b.success).sum::<u64>(), 2);
    assert_eq!(summary.buckets.iter().map(|b| b.failure).sum::<u64>(), 4);
    assert!(summary.valid(crate::now()));
    gateway.abort();
    server.abort();
}
