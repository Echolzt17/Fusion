//! Private stdio connection to the verified desktop engine. No protocol bodies are logged.
use crate::Result;
use serde_json::{json, Value};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};

pub(super) struct Rpc {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    id: u64,
    terminal: Option<Value>,
    _copy: Option<tempfile::TempDir>,
}
impl Rpc {
    pub async fn start(engine: &Path, home: &Path) -> Result<Self> {
        let mut copy = None;
        let mut command = Self::command(engine, home);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                // MSIX files may be readable but not executable by an unpackaged process.
                let source = engine.to_owned();
                let root = tokio::task::spawn_blocking(move || -> Result<tempfile::TempDir> {
                    let before = std::fs::metadata(&source).map_err(|_| "无法核验桌面内核")?;
                    let root = tempfile::tempdir().map_err(|_| "无法准备桌面内核临时副本")?;
                    std::fs::copy(&source, root.path().join("codex.exe"))
                        .map_err(|_| "无法准备桌面内核副本")?;
                    let after = std::fs::metadata(&source).map_err(|_| "无法复核桌面内核")?;
                    if before.len() != after.len()
                        || before.modified().ok() != after.modified().ok()
                    {
                        return Err("桌面内核在复制期间发生变化，请重试".into());
                    }
                    Ok(root)
                })
                .await
                .map_err(|_| "桌面内核准备任务中断")??;
                let child = Self::command(&root.path().join("codex.exe"), home)
                    .spawn()
                    .map_err(|_| "无法启动桌面内核续接服务")?;
                copy = Some(root);
                child
            }
            Err(_) => return Err("无法启动桌面内核续接服务".into()),
        };
        let mut child = child;
        let input = child.stdin.take().ok_or("桌面内核输入不可用")?;
        let output = BufReader::new(child.stdout.take().ok_or("桌面内核输出不可用")?);
        let mut rpc = Self {
            child,
            input,
            output,
            id: 0,
            terminal: None,
            _copy: copy,
        };
        rpc.call("initialize", json!({"clientInfo":{"name":"fusion_handoff","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        Ok(rpc)
    }
    fn command(engine: &Path, home: &Path) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(engine);
        c.args(["app-server", "--listen", "stdio://"])
            .env("CODEX_HOME", home)
            .current_dir(home)
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY")
            .env("OTEL_SDK_DISABLED", "true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        c.creation_flags(0x08000000);
        c
    }
    async fn next(&mut self) -> Result<Value> {
        let mut bytes = Vec::new();
        (&mut self.output)
            .take(64 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|_| "读取续接服务失败")?;
        if bytes.is_empty() {
            return Err("续接服务已退出；执行状态可能未确认，请查看已创建聊天".into());
        }
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("聊天历史超出单次读取限制，请缩小范围".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "续接服务返回格式不兼容".into())
    }
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.id += 1;
        let id = self.id;
        let mut data = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))
            .map_err(|_| "无法准备续接操作")?;
        data.push(b'\n');
        // A write failure is deliberately not retried: the server may have accepted it.
        tokio::time::timeout(Duration::from_secs(15), self.input.write_all(&data))
            .await
            .map_err(|_| "续接操作发送超时，未自动重发")?
            .map_err(|_| "续接操作发送中断，未自动重发")?;
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let value = self.next().await?;
                if value["method"] == "turn/completed" {
                    self.terminal = Some(value.clone());
                }
                if value["id"].as_u64() == Some(id) {
                    if value.get("error").is_some() {
                        return Err(
                            "客户端拒绝本次续接操作，请检查版本、接入及聊天状态；未自动重发".into(),
                        );
                    }
                    return Ok(value["result"].clone());
                }
                // Never grant a tool/approval request while preparing a handoff.
                if value.get("id").is_some() && value.get("method").is_some() {
                    return Err("续接准备触发了额外授权，已停止；请在客户端查看目标聊天".into());
                }
            }
        })
        .await
        .map_err(|_| "等待续接操作结果超时，未自动重发")?
    }
    pub async fn completed(&mut self, thread_id: &str) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(600), async {
            loop {
                let v = match self.terminal.take() {
                    Some(value) => value,
                    None => self.next().await?,
                };
                if v.get("id").is_some() && v.get("method").is_some() {
                    return Err("交接要求额外授权，未代为批准；请查看目标聊天".into());
                }
                if v["params"]["threadId"].as_str() != Some(thread_id) {
                    continue;
                }
                if v["method"] == "turn/completed" {
                    return if v["params"]["turn"]["status"] == "completed" {
                        Ok(())
                    } else {
                        Err("交接发送未正常完成；请查看目标聊天，未自动重发".into())
                    };
                }
            }
        })
        .await
        .map_err(|_| "交接发送结果未确认，请查看目标聊天；未自动重发".to_owned())?
    }
    pub async fn close(mut self) {
        let _ = self.input.shutdown().await;
        if !matches!(
            tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await,
            Ok(Ok(_))
        ) {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        // The child is reaped before its temporary executable is removed.
    }
}
