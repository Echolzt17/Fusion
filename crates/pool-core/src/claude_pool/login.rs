use super::*;
use axum::{
    extract::{Query, State},
    response::Html,
    routing::get,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
pub struct Status {
    pub phase: String,
    pub message: String,
}
pub struct Login {
    pool: Arc<Pool>,
    status: Mutex<Status>,
    active: Mutex<Option<CancellationToken>>,
    idle: Arc<tokio::sync::Semaphore>,
}
#[derive(Clone)]
struct Callback {
    state: String,
    sender: Arc<Mutex<Option<oneshot::Sender<Result<String>>>>>,
}
async fn callback(
    State(s): State<Callback>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Html<&'static str> {
    if q.get("state") != Some(&s.state) {
        return Html("<meta charset=utf-8>授权状态不匹配，请返回 Fusion 重新登录。");
    }
    let result = if q.contains_key("error") {
        Err("Claude 登录已取消或被拒绝".into())
    } else {
        q.get("code")
            .filter(|s| !s.is_empty() && s.len() < 8192)
            .cloned()
            .ok_or("Claude 回调缺少授权代码".into())
    };
    if let Ok(mut tx) = s.sender.lock() {
        if let Some(tx) = tx.take() {
            let _ = tx.send(result);
        }
    }
    Html("<meta charset=utf-8>已收到授权，请返回 Fusion 查看结果。")
}
impl Login {
    pub fn new(pool: Arc<Pool>) -> Arc<Self> {
        Arc::new(Self {
            pool,
            status: Mutex::new(Status {
                phase: "idle".into(),
                message: String::new(),
            }),
            active: Mutex::new(None),
            idle: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn active(&self) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
    pub fn cancel(&self) {
        if let Some(t) = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            t.cancel();
        }
    }
    pub async fn wait_idle(&self) {
        let _permit = self.idle.acquire().await;
    }
    pub async fn begin(self: &Arc<Self>, expected: Option<String>) -> Result<String> {
        let permit = self
            .idle
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Claude 登录正在进行")?;
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 54545))
            .await
            .map_err(|_| "Claude 登录回调端口 54545 被占用，请先完成其他登录")?;
        let cancel = CancellationToken::new();
        *self.active.lock().map_err(|_| "Claude 登录锁异常")? = Some(cancel.clone());
        let state = uuid::Uuid::new_v4().to_string();
        let verifier = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let redirect = "http://localhost:54545/callback";
        let mut url = url::Url::parse("https://claude.ai/oauth/authorize")
            .map_err(|_| "Claude 登录地址无效")?;
        url.query_pairs_mut().extend_pairs([("client_id",CLIENT_ID),("response_type","code"),("redirect_uri",redirect),("scope","user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"),("code_challenge",&challenge),("code_challenge_method","S256"),("state",&state)]);
        let (tx, rx) = oneshot::channel();
        let router = Router::new()
            .route("/callback", get(callback))
            .with_state(Callback {
                state: state.clone(),
                sender: Arc::new(Mutex::new(Some(tx))),
            });
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await;
        });
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = Status {
            phase: "pending".into(),
            message: "请在浏览器登录 Claude 订阅账号；添加下一个前请核对浏览器身份。".into(),
        };
        let this = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result: Result<()> = tokio::select! {
                _=cancel.cancelled()=>Err("Claude 登录已取消".into()),
                r=tokio::time::timeout(Duration::from_secs(600),async {
                    let code=rx.await.map_err(|_|"Claude 登录回调已关闭")??;
                    let response=this.pool.http.post(TOKEN_URL).await?.timeout(Duration::from_secs(30)).json(&json!({"grant_type":"authorization_code","client_id":CLIENT_ID,"code":code,"state":state,"redirect_uri":redirect,"code_verifier":verifier})).send().await.map_err(|_|"Claude 授权交换网络失败，未自动重试")?;
                    if !response.status().is_success(){return Err("Claude 授权交换失败，未自动重试；请重新登录".into());}
                    let value=response.json::<Value>().await.map_err(|_|"Claude 授权响应格式错误")?;
                    this.pool.accept(value,expected).await
                })=>r.unwrap_or_else(|_|Err("Claude 登录已超时".into())),
            };
            stop.cancel();
            server.abort();
            *this.status.lock().unwrap_or_else(|e| e.into_inner()) = match result {
                Ok(()) => Status {
                    phase: "complete".into(),
                    message: "Claude 账号已添加；额度将单独查询。".into(),
                },
                Err(e) => Status {
                    phase: "failed".into(),
                    message: e,
                },
            };
            *this.active.lock().unwrap_or_else(|e| e.into_inner()) = None;
        });
        Ok(url.to_string())
    }
}
