//! Only anonymous, read-only update GETs may retry. No AI requests use this policy.
use super::{validate_manifest, Result, MAX_PACKAGE};
use reqwest::{Client, Response, StatusCode};
use serde_json::Value;
use std::{future::Future, path::Path, time::Duration};

const ATTEMPTS: usize = 3;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(8);

#[derive(Debug)]
pub(super) struct Failure {
    message: String,
    retry_after: Option<Duration>,
}
impl Failure {
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_after: None,
        }
    }
    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_after: Some(Duration::ZERO),
        }
    }
}

fn status_failure(response: &Response, not_found: &str) -> Failure {
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Failure::permanent(not_found);
    }
    let message = format!("GitHub 更新服务返回 HTTP {}；请稍后重试", status.as_u16());
    if !matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504) {
        return Failure::permanent(message);
    }
    // Do not ignore a server-requested longer wait or guess at an HTTP date.
    // Such responses finish this operation instead of retrying too early.
    let retry_after = match response.headers().get(reqwest::header::RETRY_AFTER) {
        None => Some(Duration::ZERO),
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
            .filter(|wait| *wait <= MAX_RETRY_WAIT),
    };
    Failure {
        message,
        retry_after,
    }
}

async fn retry_get<T, F, Fut>(budget: Duration, mut attempt: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, Failure>>,
{
    tokio::time::timeout(budget, async {
        for number in 0..ATTEMPTS {
            match attempt().await {
                Ok(value) => return Ok(value),
                Err(failure) => {
                    if number + 1 == ATTEMPTS {
                        return Err(failure.message);
                    }
                    let Some(wait) = failure.retry_after else {
                        return Err(failure.message);
                    };
                    let backoff = Duration::from_millis(750 * (1 << number));
                    tokio::time::sleep(wait.max(backoff)).await;
                }
            }
        }
        unreachable!()
    })
    .await
    .map_err(|_| "更新网络等待超时；请稍后重试".to_owned())?
}

pub(super) async fn manifest(client: &Client, url: &str) -> Result<Value> {
    retry_get(Duration::from_secs(60), || async {
        let mut response = client
            .get(url)
            .timeout(Duration::from_secs(45))
            .send()
            .await
            .map_err(|_| Failure::transient("无法连接 GitHub 更新服务；当前版本可继续使用"))?;
        if !response.status().is_success() {
            return Err(status_failure(
                &response,
                "尚未发布此版本的稳定更新清单；当前版本可继续使用",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Failure::transient("更新清单下载中断"))?
        {
            if bytes.len() + chunk.len() > 64 * 1024 {
                return Err(Failure::permanent("更新清单超过允许大小"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let manifest =
            serde_json::from_slice(&bytes).map_err(|_| Failure::permanent("更新清单格式错误"))?;
        validate_manifest(&manifest).map_err(Failure::permanent)?;
        Ok(manifest)
    })
    .await
}

pub(super) async fn package(
    client: &Client,
    url: &str,
    directory: &Path,
    progress: impl Fn(u64, Option<u64>) + Copy,
) -> Result<tempfile::NamedTempFile> {
    retry_get(Duration::from_secs(30 * 60), || async {
        let response = tokio::time::timeout(
            Duration::from_secs(45),
            client.get(url).timeout(Duration::from_secs(30 * 60)).send(),
        )
        .await
        .map_err(|_| Failure::transient("更新服务长时间未响应，可稍后重试"))?
        .map_err(|_| Failure::transient("更新包下载失败；请检查 GitHub 网络连接"))?;
        if !response.status().is_success() {
            return Err(status_failure(&response, "更新包下载被拒绝或文件尚未发布"));
        }
        // A failed attempt drops its partial tempfile. Only complete bytes are
        // returned to the caller for signature verification and atomic commit.
        download_attempt_to_file(response, directory, progress).await
    })
    .await
}

async fn download_attempt_to_file(
    mut response: Response,
    directory: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> std::result::Result<tempfile::NamedTempFile, Failure> {
    let total = response.content_length();
    if total.is_some_and(|n| n > MAX_PACKAGE) {
        return Err(Failure::permanent("更新包超过允许大小"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)
        .map_err(|_| Failure::permanent("无法创建更新缓存，请检查磁盘空间和权限"))?;
    let mut downloaded = 0;
    progress(downloaded, total);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Failure::transient("更新包下载中断，可稍后重试"))?
    {
        downloaded += chunk.len() as u64;
        if downloaded > MAX_PACKAGE {
            return Err(Failure::permanent("更新包超过允许大小"));
        }
        std::io::Write::write_all(&mut temporary, &chunk)
            .map_err(|_| Failure::permanent("更新包保存失败，请检查磁盘空间和权限"))?;
        progress(downloaded, total);
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| Failure::permanent("更新包写入磁盘失败"))?;
    Ok(temporary)
}

#[cfg(test)]
pub(super) async fn download_to_file(
    response: Response,
    directory: &Path,
    progress: impl FnMut(u64, Option<u64>),
) -> Result<tempfile::NamedTempFile> {
    download_attempt_to_file(response, directory, progress)
        .await
        .map_err(|failure| failure.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn server(
        replies: Vec<Vec<u8>>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/public", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let size = stream.read(&mut request).await.unwrap();
                let request = std::str::from_utf8(&request[..size]).unwrap();
                assert!(request.starts_with("GET /public "));
                assert!(!request.to_ascii_lowercase().contains("authorization:"));
                let index = count.fetch_add(1, Ordering::SeqCst);
                let reply = &replies[index.min(replies.len() - 1)];
                stream.write_all(reply).await.unwrap();
            }
        });
        (url, calls, task)
    }
    fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        [
            format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                body.len()
            )
            .into_bytes(),
            body.to_vec(),
        ]
        .concat()
    }
    fn client() -> Client {
        Client::builder().no_proxy().build().unwrap()
    }
    fn manifest_bytes() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": "99.0.0", "notes": "public fixture",
            "platforms": {"windows-x86_64": {"signature": "public-fixture", "url":
                "https://github.com/Echolzt17/Fusion/releases/download/v99.0.0/Fusion_99.0.0_x64-setup.exe"}}
        })).unwrap()
    }

    #[tokio::test]
    async fn metadata_recovers_from_service_error_and_interrupted_body() {
        let replies = vec![
            response("503 Service Unavailable", "Retry-After: 0\r\n", b""),
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort".to_vec(),
            response("200 OK", "", &manifest_bytes()),
        ];
        let (url, calls, task) = server(replies).await;
        assert_eq!(
            manifest(&client(), &url).await.unwrap()["version"],
            "99.0.0"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        task.abort();
    }

    #[tokio::test]
    async fn missing_invalid_and_long_retry_after_metadata_are_not_retried() {
        for reply in [
            response("404 Not Found", "", b""),
            response("200 OK", "", b"invalid"),
            response("429 Too Many Requests", "Retry-After: 120\r\n", b""),
            response(
                "503 Service Unavailable",
                "Retry-After: Fri, 06 Nov 2026 12:00:00 GMT\r\n",
                b"",
            ),
        ] {
            let (url, calls, task) = server(vec![reply]).await;
            assert!(manifest(&client(), &url).await.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            task.abort();
        }
    }

    #[tokio::test]
    async fn repeated_transient_failure_stops_after_three_attempts() {
        let (url, calls, task) = server(vec![response("502 Bad Gateway", "", b"")]).await;
        assert!(manifest(&client(), &url)
            .await
            .unwrap_err()
            .contains("HTTP 502"));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        task.abort();
    }

    #[tokio::test]
    async fn interrupted_package_restarts_cleanly_and_keeps_existing_package() {
        let directory = tempfile::tempdir().unwrap();
        let previous = directory.path().join("setup.exe");
        std::fs::write(&previous, b"previous").unwrap();
        let (url, calls, task) = server(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort".to_vec(),
            response("200 OK", "", b"complete-public-fixture"),
        ])
        .await;
        let file = package(&client(), &url, directory.path(), |_, _| {})
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(file.path()).unwrap(),
            b"complete-public-fixture"
        );
        assert_eq!(std::fs::read(previous).unwrap(), b"previous");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        drop(file);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn disk_failure_is_terminal_and_retry_wait_is_cancellable() {
        let directory = tempfile::tempdir().unwrap();
        let (url, calls, task) = server(vec![response("200 OK", "", b"public-fixture")]).await;
        assert!(package(
            &client(),
            &url,
            &directory.path().join("missing"),
            |_, _| {}
        )
        .await
        .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort();
        let (url, calls, task) = server(vec![response(
            "503 Service Unavailable",
            "Retry-After: 8\r\n",
            b"",
        )])
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(150), manifest(&client(), &url))
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort();
    }
}
