//! Runtime form of scripts/verify-native-images.mjs. Only public fixture data.
use super::*;
use crate::gateway::continuity::{identities, output_keys, required};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use serde_json::{json, Value};
use std::{process::Stdio, sync::Arc};
use toml_edit::{value, DocumentMut, Item, Table};
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
#[derive(Default)]
struct Fixture {
    reject_image: bool,
    responses: usize,
    generated: usize,
    edited: usize,
    error: Option<&'static str>,
    bindings: std::collections::HashSet<String>,
}
type Shared = Arc<Mutex<Fixture>>;
pub(crate) fn isolated_command(engine: &Path, root: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(engine);
    command.env_clear().current_dir(root);
    // No inherited API keys, proxies, real homes, config or plugins.
    for name in ["SYSTEMROOT", "WINDIR", "SYSTEMDRIVE", "COMSPEC", "PATH"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for name in [
        "CODEX_HOME",
        "USERPROFILE",
        "HOME",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
    ] {
        command.env(name, root);
    }
    command.env("OTEL_SDK_DISABLED", "true");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
fn authorized(h: &HeaderMap) -> bool {
    h.get("x-fusion-client-key").and_then(|v| v.to_str().ok()) == Some("fusion-public-fixture-key")
        && h.get("x-openai-actor-authorization")
            .and_then(|v| v.to_str().ok())
            == Some("fusion-local")
}
fn fail(s: &mut Fixture, reason: &'static str) -> Response {
    s.error.get_or_insert(reason);
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":{"message":"fixture rejected"}})),
    )
        .into_response()
}
fn bind_request(s: &mut Fixture, h: &HeaderMap, v: &Value) -> bool {
    let Ok(ids) = identities(h, v) else {
        return false;
    };
    if ids.is_empty() || required(h, v).iter().any(|key| !s.bindings.contains(key)) {
        return false;
    }
    s.bindings.extend(ids);
    true
}
async fn models(h: HeaderMap) -> Response {
    if !authorized(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"models":[],"data":[]})).into_response()
}
async fn responses(State(shared): State<Shared>, h: HeaderMap, Json(v): Json<Value>) -> Response {
    let mut s = shared.lock().unwrap();
    if !authorized(&h) {
        return fail(&mut s, "凭据命令或本机识别头协议不兼容");
    }
    if !bind_request(&mut s, &h, &v) {
        return fail(&mut s, "客户端会话或轮次不能由网关安全绑定");
    }
    let tools = json!([
        v["tools"],
        v["input"].as_array().map(|a| a
            .iter()
            .filter(|v| v["type"] == "additional_tools")
            .collect::<Vec<_>>())
    ])
    .to_string();
    if !tools.contains("image_gen") || !tools.contains("imagegen") {
        return fail(&mut s, "客户端未开放原生生图工具");
    }
    let n = s.responses;
    s.responses += 1;
    let last = if s.reject_image { 1 } else { 2 };
    if n > last {
        return fail(&mut s, "客户端重复发送模型请求");
    }
    if (n >= 1 && s.generated != 1) || (n == 2 && s.edited != 1) {
        return fail(&mut s, "图片工具调用或改图续接不兼容");
    }
    let item = if n < last {
        let args = if n == 1 {
            json!({"prompt":"Fixture only","num_last_images_to_include":1})
        } else {
            json!({"prompt":"Fixture only"})
        };
        json!({"type":"function_call","id":format!("fc-{n}"),"call_id":format!("image-{n}"),
            "namespace":"image_gen","name":"imagegen","arguments":args.to_string()})
    } else {
        json!({"type":"message","id":"msg-fixture","role":"assistant","status":"completed",
            "content":[{"type":"output_text","text":"fixture-ok","annotations":[]}]})
    };
    let events = [
        json!({"type":"response.created","response":{"id":format!("resp-{n}"),"object":"response","status":"in_progress"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":item}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"id":format!("resp-{n}"),"object":"response","status":"completed",
            "output":[item],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ];
    s.bindings.extend(output_keys(&json!({"output":[item]})));
    s.bindings.insert(crate::scheduler::binding_key(
        "response",
        &format!("resp-{n}"),
    ));
    let text = events
        .iter()
        .map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap()))
        .collect::<String>();
    ([(http::header::CONTENT_TYPE, "text/event-stream")], text).into_response()
}
async fn image(shared: Shared, h: HeaderMap, v: Value, edit: bool) -> Response {
    let mut s = shared.lock().unwrap();
    if !authorized(&h) || h.get("x-codex-image-turn-id").is_none() {
        return fail(&mut s, "图片鉴权或轮次绑定协议不兼容");
    }
    if !bind_request(&mut s, &h, &v) {
        return fail(&mut s, "图片轮次或引用不能绑定到原账号");
    }
    if !v.is_object() || v["stream"] == true {
        return fail(&mut s, "图片请求不再使用受支持的 JSON 协议");
    }
    if edit {
        s.edited += 1;
        if s.reject_image || s.edited != 1 || !v["images"].as_array().is_some_and(|v| v.len() == 1)
        {
            return fail(&mut s, "图片编辑协议不兼容或客户端重复发送");
        }
    } else {
        s.generated += 1;
        if s.generated != 1 {
            return fail(&mut s, "客户端重放了图片请求");
        }
    }
    if s.reject_image {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":{"message":"fixture failure","type":"server_error"}})),
        )
            .into_response();
    }
    s.bindings.extend(output_keys(
        &json!({"data":[{"generation_id":"fixture-generation"}]}),
    ));
    ([(http::header::CONTENT_TYPE, "application/json"),
        (http::HeaderName::from_static("x-codex-imagegen-request-id"), "fixture-image-request")],
        Json(json!({"created":1,"background":"opaque","data":[{"b64_json":PNG,"generation_id":"fixture-generation"}]}))).into_response()
}
fn router(shared: Shared) -> Router {
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/responses", post(responses))
        .route(
            "/v1/images/generations",
            post(
                |State(s): State<Shared>, h: HeaderMap, Json(v): Json<Value>| image(s, h, v, false),
            ),
        )
        .route(
            "/v1/images/edits",
            post(
                |State(s): State<Shared>, h: HeaderMap, Json(v): Json<Value>| image(s, h, v, true),
            ),
        )
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .fallback(|| async { Json(json!({"plan_type":"plus","rate_limit":{"allowed":true,"limit_reached":false}})) })
        .with_state(shared)
}
pub(crate) fn configure(root: &Path, address: std::net::SocketAddr) -> Result<()> {
    let mut d = DocumentMut::new();
    d["model"] = value("gpt-6.1-sol");
    d["model_provider"] = value("fusion_fixture");
    d["cli_auth_credentials_store"] = value("file");
    d["chatgpt_base_url"] = value(format!("http://{address}"));
    d["features"] = Item::Table(Table::new());
    d["features"]["image_generation"] = value(true);
    d["analytics"] = Item::Table(Table::new());
    d["analytics"]["enabled"] = value(false);
    d["model_providers"] = Item::Table(Table::new());
    d["model_providers"]["fusion_fixture"] = Item::Table(config::provider_config(
        &format!("http://{address}/v1"),
        "fusion-public-fixture-key",
    )?);
    let claims = json!({"exp":4102444800u64,"sub":"fixture-user","email":"fixture@example.invalid",
        "https://api.openai.com/auth":{"chatgpt_plan_type":"plus","chatgpt_account_id":"fixture-account","chatgpt_user_id":"fixture-user","user_id":"fixture-user"}});
    let jwt = format!("eyJhbGciOiJub25lIn0.{}.c2ln", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string()));
    std::fs::write(root.join("auth.json"), json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
        "tokens":{"id_token":jwt,"access_token":jwt,"refresh_token":"public-fixture","account_id":"fixture-account"},
        "last_refresh":"2099-01-01T00:00:00Z"}).to_string()).map_err(|_| "无法写入公开假登录")?;
    std::fs::write(root.join("config.toml"), d.to_string())
        .map_err(|_| "无法写入临时测试配置".into())
}
fn saved_images(root: &Path, count: &mut usize, remaining: &mut usize) -> Result<()> {
    for entry in std::fs::read_dir(root).map_err(|_| "无法核验临时图片输出")? {
        if *remaining == 0 {
            return Err("临时测试输出超出限制".into());
        }
        *remaining -= 1;
        let entry = entry.map_err(|_| "无法核验临时图片输出")?;
        let kind = entry.file_type().map_err(|_| "无法核验临时图片输出")?;
        if kind.is_symlink() {
            return Err("临时测试产生了不支持的文件链接".into());
        }
        if kind.is_dir() {
            saved_images(&entry.path(), count, remaining)?;
        } else if entry.path().extension().is_some_and(|v| v == "png")
            && entry
                .path()
                .components()
                .any(|p| p.as_os_str() == "generated_images")
        {
            if entry.metadata().map_err(|_| "无法核验临时图片输出")?.len() > 1024 {
                return Err("测试图片保存格式不兼容".into());
            }
            let bytes = std::fs::read(entry.path()).map_err(|_| "无法核验临时图片输出")?;
            if bytes
                != base64::engine::general_purpose::STANDARD
                    .decode(PNG)
                    .unwrap()
            {
                return Err("测试图片保存格式不兼容".into());
            }
            *count += 1;
        }
    }
    Ok(())
}
fn completed(s: &Fixture, success: bool, files: usize) -> Result<()> {
    if let Some(error) = s.error {
        return Err(error.into());
    }
    if !success {
        return Err("测试内核提前退出，客户端参数、运行依赖或凭据命令检查未完成".into());
    }
    if s.responses != (if s.reject_image { 2 } else { 3 })
        || s.generated != 1
        || s.edited != (if s.reject_image { 0 } else { 1 })
    {
        return Err("原生生图/改图或失败不重放检查未完成".into());
    }
    if files != (if s.reject_image { 0 } else { 2 }) {
        return Err("客户端未正确保存原生生图与改图结果".into());
    }
    Ok(())
}
async fn scenario(engine: &Path, reject_image: bool) -> Result<()> {
    let root = tempfile::Builder::new()
        .prefix("fusion-protocol-")
        .tempdir()
        .map_err(|_| "无法创建隔离测试目录")?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "无法启动本机兼容检查接口")?;
    let address = listener
        .local_addr()
        .map_err(|_| "无法读取本机兼容检查端口")?;
    configure(root.path(), address)?;
    let login = std::fs::read(root.path().join("auth.json")).map_err(|_| "无法核对公开假登录")?;
    let shared = Arc::new(Mutex::new(Fixture {
        reject_image,
        ..Default::default()
    }));
    let app = router(shared.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let result = async {
        verify_desktop_identity(engine, root.path()).await?;
        let mut command = tokio::process::Command::from(isolated_command(engine, root.path()));
        command
            .args([
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-rules",
                "--color",
                "never",
                "-C",
            ])
            .arg(root.path())
            .arg("Fixture")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| "无法启动独立测试内核")?;
        let success = match tokio::time::timeout(Duration::from_secs(40), child.wait()).await {
            Ok(Ok(code)) => code.success(),
            Ok(Err(_)) => return Err("无法读取测试内核退出状态".into()),
            Err(_) => {
                let _ = child.kill().await;
                return Err(
                    "本机协议检查超时；请检查安全软件或内核启动状态，未使用真实账号".into(),
                );
            }
        };
        let mut files = 0;
        saved_images(root.path(), &mut files, &mut 1024)?;
        if std::fs::read(root.path().join("auth.json")).ok().as_deref() != Some(login.as_slice()) {
            return Err("客户端改变了隔离登录，未应用账号池接入".into());
        }
        completed(&shared.lock().unwrap(), success, files)
    }
    .await;
    server.abort();
    let _ = server.await;
    result
}
async fn verify_desktop_identity(engine: &Path, root: &Path) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut child = tokio::process::Command::from(isolated_command(engine, root))
        .arg("app-server").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .kill_on_drop(true).spawn().map_err(|_| "无法核对客户端登录识别")?;
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        let mut input = child.stdin.take().ok_or("隔离检查输入不可用")?;
        let mut output = BufReader::new(child.stdout.take().ok_or("隔离检查输出不可用")?).lines();
        input.write_all(b"{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"fusion-fixture\",\"version\":\"1\"}}}\n").await.map_err(|_| "无法核对客户端登录识别")?;
        let mut total = 0;
        while let Some(line) = output.next_line().await.map_err(|_| "无法读取隔离登录状态")? {
            total += line.len();
            if total > 65536 { return Err("隔离登录检查输出超出限制".into()); }
            let v: Value = serde_json::from_str(&line).map_err(|_| "隔离登录检查格式不兼容")?;
            if v["id"] == 1 {
                if v.get("error").is_some() { return Err("客户端接入配置不兼容".into()); }
                input.write_all(b"{\"method\":\"initialized\"}\n{\"id\":2,\"method\":\"getAuthStatus\",\"params\":{\"includeToken\":false,\"refreshToken\":false}}\n").await.map_err(|_| "无法核对客户端登录识别")?;
            } else if v["id"] == 2 {
                return if v["result"]["authMethod"] == "chatgpt" { Ok(()) }
                    else { Err("账号池接入会改变客户端个人登录识别，未应用接入".into()) };
            }
        }
        Err("客户端登录识别检查提前结束".into())
    }).await.unwrap_or_else(|_| Err("客户端登录识别检查超时".into()));
    let _ = child.kill().await;
    result
}
pub(super) fn run(engine: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "无法创建隔离检查运行环境")?;
    runtime.block_on(async {
        scenario(engine, false).await?;
        scenario(engine, true).await
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fixture_rejects_missing_tool_unknown_auth_and_replayed_images() {
        let shared = Arc::new(Mutex::new(Fixture::default()));
        let mut h = HeaderMap::new();
        h.insert("x-fusion-client-key", "fusion-public-fixture-key".parse().unwrap());
        h.insert(
            "x-openai-actor-authorization",
            "fusion-local".parse().unwrap(),
        );
        h.insert(
            "x-codex-turn-metadata",
            r#"{"thread_id":"fixture-thread","turn_id":"fixture-turn"}"#
                .parse()
                .unwrap(),
        );
        assert_eq!(
            responses(State(shared.clone()), h.clone(), Json(json!({})))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            shared.lock().unwrap().error,
            Some("客户端未开放原生生图工具")
        );
        *shared.lock().unwrap() = Fixture::default();
        h.insert("x-codex-image-turn-id", "fixture-turn".parse().unwrap());
        assert_eq!(
            image(shared.clone(), h.clone(), json!({}), false)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            shared.lock().unwrap().error,
            Some("图片轮次或引用不能绑定到原账号")
        );
        *shared.lock().unwrap() = Fixture::default();
        shared
            .lock()
            .unwrap()
            .bindings
            .insert(crate::scheduler::binding_key("turn", "fixture-turn"));
        assert_eq!(
            image(shared.clone(), h.clone(), json!({}), false)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            image(shared.clone(), h.clone(), json!({}), false)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert!(completed(&shared.lock().unwrap(), true, 2).is_err());
        h.remove("x-fusion-client-key");
        assert_eq!(
            image(shared, h, json!({}), false).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    #[test]
    fn success_requires_both_images_and_failure_requires_no_replay_or_saved_image() {
        let mut s = Fixture {
            responses: 3,
            generated: 1,
            edited: 1,
            ..Default::default()
        };
        assert!(completed(&s, true, 2).is_ok());
        assert!(completed(&s, true, 1).is_err());
        s = Fixture {
            reject_image: true,
            responses: 2,
            generated: 1,
            ..Default::default()
        };
        assert!(completed(&s, true, 0).is_ok());
        assert!(completed(&s, true, 1).is_err());
        s.generated = 2;
        assert!(completed(&s, true, 0).is_err());
    }
}
