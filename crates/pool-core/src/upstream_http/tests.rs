use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[test]
fn windows_proxy_maps_use_connect_http_transport_and_reject_unusable_maps() {
    let map = "http=127.0.0.1:8123; HTTPS=127.0.0.1:8456;ftp=ignored;socks=127.0.0.1:1080";
    assert_eq!(
        static_proxy(map, "https").unwrap(),
        "http://127.0.0.1:8456/"
    );
    assert_eq!(static_proxy(map, "http").unwrap(), "http://127.0.0.1:8123/");
    assert_eq!(
        static_proxy("127.0.0.1:7890", "https").unwrap(),
        "http://127.0.0.1:7890/"
    );
    assert_eq!(
        static_proxy("socks=socks5://127.0.0.1:1080", "https").unwrap(),
        "socks5h://127.0.0.1:1080"
    );
    assert_eq!(
        static_proxy("socks=127.0.0.1:1080", "https").unwrap(),
        "socks4://127.0.0.1:1080"
    );
    for raw in [
        "",
        "https=",
        "http=127.0.0.1:1234",
        "https=direct://",
        "ftp=127.0.0.1:1234",
        "https=127.0.0.1:0",
        "https=http://127.0.0.1:1/path",
        "https=user:fake-secret@bad:port",
    ] {
        let error = static_proxy(raw, "https").err().unwrap();
        assert_eq!(error, INVALID_PROXY);
        assert!(!error.contains("fake-secret"));
    }
}
#[test]
fn full_windows_proxy_lists_select_protocol_then_first_candidate() {
    for map in ["http=127.0.0.1:8123;https=127.0.0.1:8456", "http=127.0.0.1:8123 https=127.0.0.1:8456", "http=127.0.0.1:8123\tHTTPS=127.0.0.1:8456;https=127.0.0.1:9999"] {
        assert_eq!(static_proxy(map, "https").unwrap(), "http://127.0.0.1:8456/");
    }
    assert_eq!(static_proxy("127.0.0.1:8123;127.0.0.1:8456", "https").unwrap(), "http://127.0.0.1:8123/");
    assert!(static_proxy("http=127.0.0.1:8123", "https").is_err());
    assert!(static_proxy("DIRECT", "https").is_err());
}

#[test]
fn environment_selection_is_scheme_specific_and_cannot_bypass_external_requests() {
    let read = |name: &str| match name {
        "HTTPS_PROXY" => Some("socks5://127.0.0.1:1080".to_owned()),
        "ALL_PROXY" => Some("http://127.0.0.1:7890".to_owned()),
        "NO_PROXY" => Some("*".to_owned()),
        _ => None,
    };
    let route = environment_route("https", read).unwrap().unwrap();
    assert_eq!(route.proxy, "socks5h://127.0.0.1:1080");
    assert_eq!(
        environment_route("http", read).unwrap().unwrap().proxy,
        "http://127.0.0.1:7890/"
    );
    assert!(environment_route("https", |name| (name == "HTTP_PROXY")
        .then(|| "127.0.0.1:7890".into()))
    .unwrap()
    .is_none());
    assert!(environment_route("https", |name| (name == "HTTPS_PROXY")
        .then(|| "direct://".into()))
    .is_err());
}

async fn proxy(status: &str) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let hits = count.clone();
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\nLocation: http://must-not-be-followed.invalid/\r\n\r\n");
    let handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let hits = hits.clone();
            let response = response.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 8192];
                if stream.read(&mut buffer).await.unwrap_or(0) > 0 {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            });
        }
    });
    (address, count, handle)
}
fn with_proxy(current: Arc<Mutex<String>>) -> UpstreamHttp {
    let mut http = UpstreamHttp::new().unwrap();
    http.resolver = Arc::new(move |_| {
        Ok(Route {
            proxy: current.lock().unwrap().clone(),
            source: ProxySource::WindowsStaticConfigured,
        })
    });
    http
}

#[tokio::test]
async fn proxy_that_drops_reused_connections_gets_a_fresh_transport_for_each_request() {
    // Some proxies accept a keep-alive response then silently discard the next
    // request on that socket. Never send that second business request twice.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let (accepted, sent) = (connections.clone(), requests.clone());
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            accepted.fetch_add(1, Ordering::SeqCst);
            let sent = sent.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 8192];
                for n in 0..2 {
                    let size = stream.read(&mut buffer).await.unwrap_or(0);
                    if size == 0 { break; }
                    sent.fetch_add(1, Ordering::SeqCst);
                    if n == 1 { break; }
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await.unwrap();
                }
            });
        }
    });
    let http = with_proxy(Arc::new(Mutex::new(address)));
    for _ in 0..4 {
        assert_eq!(http.post("http://fixture.invalid/responses").await.unwrap()
            .timeout(Duration::from_secs(2)).send().await
            .expect("do not reuse a proxy connection left over from the preceding conversation")
            .text().await.unwrap(), "ok");
    }
    assert_eq!(requests.load(Ordering::SeqCst), 4, "exactly one send per request");
    assert_eq!(connections.load(Ordering::SeqCst), 4);
    server.abort();
}

#[tokio::test]
async fn new_requests_follow_changed_proxy_without_moving_existing_requests() {
    let (first, first_hits, first_task) = proxy("403 Forbidden").await;
    let (second, second_hits, second_task) = proxy("200 OK").await;
    let current = Arc::new(Mutex::new(first));
    let http = with_proxy(current.clone());
    let old = http
        .post("http://unresolvable.example.test/oauth/token")
        .await
        .unwrap();
    *current.lock().unwrap() = second;
    let new = http
        .post("http://unresolvable.example.test/responses")
        .await
        .unwrap();
    assert_eq!(old.send().await.unwrap().status(), 403);
    assert_eq!(new.send().await.unwrap().status(), 200);
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
    first_task.abort();
    second_task.abort();
}

#[tokio::test]
async fn https_uses_proxy_connect_once_and_never_replays_after_rejection() {
    let (address, hits, task) = proxy("403 Forbidden").await;
    let http = with_proxy(Arc::new(Mutex::new(address)));
    let request = http
        .post("https://unresolvable.example.test/oauth/token")
        .await
        .unwrap();
    assert!(request
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .is_err());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn redirects_and_proxy_failures_do_not_fall_back_to_direct() {
    let (address, hits, task) = proxy("302 Found").await;
    let current = Arc::new(Mutex::new(address));
    let http = with_proxy(current.clone());
    let response = http
        .get("http://unresolvable.example.test/models")
        .await
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 302);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    task.abort();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    *current.lock().unwrap() = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(http
        .get("http://unresolvable.example.test/models")
        .await
        .unwrap()
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .is_err());
}

#[tokio::test]
async fn loopback_never_uses_proxy_and_missing_proxy_blocks_external_requests() {
    let (address, hits, task) = proxy("200 OK").await;
    let mut http = UpstreamHttp::new().unwrap();
    http.resolver = Arc::new(|_| Err(MISSING_PROXY.into()));
    let (request, source) = http.request(Method::GET, &address).await.unwrap();
    assert_eq!(source, ProxySource::LoopbackDirect);
    assert_eq!(request.send().await.unwrap().status(), 200);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    for target in [
        "https://auth.openai.com/oauth/token",
        "https://chatgpt.com/backend-api/wham/usage",
        "https://localhost.evil.test/",
        "http://127.0.0.1.evil.test/",
        "https://[::ffff:192.0.2.1]/",
    ] {
        assert_eq!(http.get(target).await.err().unwrap(), MISSING_PROXY);
    }
    for target in [
        "http://localhost:1455/",
        "http://127.1.2.3:1457/",
        "http://[::1]:18765/",
    ] {
        assert!(loopback(&Url::parse(target).unwrap()));
    }
    task.abort();
}
