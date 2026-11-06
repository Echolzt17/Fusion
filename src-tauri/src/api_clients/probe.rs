//! Before managed use, prove this installed binary honors no-replay controls.
//! Every request uses an isolated temporary Claude profile and a local fake API.
use super::process::{command,ManagedProcess};
use pool_core::Result;
use std::{path::Path,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use tokio::io::{AsyncReadExt,AsyncWriteExt};

pub(super) async fn verify(executable:&Path,model:&str)->Result<()> {
    for scenario in [Scenario::Error,Scenario::Disconnected,Scenario::PartialStream] {
        verify_scenario(executable,model,scenario).await?;
    }
    Ok(())
}
#[derive(Clone,Copy)]
enum Scenario {Error,Disconnected,PartialStream}
struct Serving(tokio::task::JoinHandle<()>);
impl Drop for Serving {fn drop(&mut self){self.0.abort();}}
async fn verify_scenario(executable:&Path,model:&str,scenario:Scenario)->Result<()> {
    let root=tempfile::tempdir().map_err(|_|"无法创建 Claude 隔离验证目录")?;
    let listener=tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).await.map_err(|_|"无法创建 Claude 本机验证服务")?;
    let port=listener.local_addr().map_err(|_|"无法读取 Claude 验证服务地址")?.port();
    let attempts=Arc::new(AtomicUsize::new(0));
    let seen=attempts.clone();
    let serving=Serving(tokio::spawn(async move {
        loop {
            let Ok((mut socket,_))=listener.accept().await else {return;};
            let seen=seen.clone();
            tokio::spawn(async move {
                let mut headers=Vec::new();let mut chunk=[0u8;4096];
                let header_end=loop {
                    let Ok(Ok(n))=tokio::time::timeout(Duration::from_secs(3),socket.read(&mut chunk)).await else {return;};
                    if n==0{return;}headers.extend_from_slice(&chunk[..n]);
                    if headers.len()>1024*1024{return;}
                    if let Some(index)=headers.windows(4).position(|w|w==b"\r\n\r\n"){break index+4;}
                };
                let header_text=String::from_utf8_lossy(&headers[..header_end]);
                let first=header_text.lines().next().unwrap_or_default();
                let messages=first.starts_with("POST ")&&first.contains("/v1/messages ");
                let content_length=header_text.lines().find_map(|line|line.split_once(':').filter(|(k,_)|k.eq_ignore_ascii_case("content-length")).and_then(|(_,v)|v.trim().parse::<usize>().ok())).unwrap_or(0);
                if content_length>1024*1024{return;}
                let mut remaining=content_length.saturating_sub(headers.len()-header_end);
                while remaining>0 {
                    let Ok(Ok(n))=tokio::time::timeout(Duration::from_secs(3),socket.read(&mut chunk)).await else {return;};
                    if n==0{return;}remaining=remaining.saturating_sub(n);
                }
                if !messages {
                    let body=if first.contains("count_tokens") {"{\"input_tokens\":1}"} else {"{\"data\":[{\"id\":\"claude-sonnet-4-5\",\"type\":\"model\",\"display_name\":\"Fixture\"}],\"has_more\":false}"};
                    let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await;return;
                }
                seen.fetch_add(1,Ordering::SeqCst);
                match scenario {
                    Scenario::Disconnected=>{},
                    Scenario::Error=>{
                        let body="{\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"Public Fusion fixture\"}}";
                        let _=socket.write_all(format!("HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nx-should-retry: false\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await;
                    },
                    Scenario::PartialStream=>{
                        let body=concat!("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fusion_fixture\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-5\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Public Fusion fixture\"}}\n\n");
                        // Incomplete Content-Length forces a genuine transport interruption.
                        let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len()+4096,body).as_bytes()).await;
                    }
                }
                let _=socket.shutdown().await;
            });
        }
    }));
    let mut command=command(executable,Some(&format!("http://127.0.0.1:{port}")),Some("fusion-public-probe-key"),Some(model));
    super::process::isolate(&mut command,root.path());
    command.args(["--print","Reply with the word fixture. This is a local public transport test."])
        .env("API_TIMEOUT_MS","3000").env("CLAUDE_STREAM_FIRST_BYTE_TIMEOUT_MS","3000");
    let mut process=ManagedProcess::spawn(command,true)?;
    let deadline=tokio::time::Instant::now()+Duration::from_secs(20);
    let mut completed=false;
    loop {
        if process.exited()? {completed=true;break;}
        if attempts.load(Ordering::SeqCst)>1 || tokio::time::Instant::now()>=deadline {break;}
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let count=attempts.load(Ordering::SeqCst);
    if !completed {let _=process.terminate();}
    // Own job is dropped here and cannot survive cancellation of this check.
    drop(serving);
    if count!=1 || !completed {
        return Err("当前 Claude Code 未通过隔离的不重放检查（错误、断连或部分输出）；未启用真实 API 接入。请更新客户端后重试".into());
    }
    // Read completion through the owned child handle only; no output is collected.
    let _=process.stop();
    Ok(())
}
