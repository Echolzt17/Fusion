// PKCE protocol/reference: Cockpit Tools v1.3.75 codex_oauth.rs; see THIRD_PARTY_NOTICES.md.
use crate::{
    account::{tokens_from_response, Accounts, CLIENT_ID},
    Result,
};
use axum::{
    extract::{Query, State},
    response::Html,
    routing::get,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStatus {
    pub phase: String,
    pub message: String,
}
pub struct OAuth {
    status: Arc<Mutex<LoginStatus>>,
    active: Mutex<Option<CancellationToken>>,
    accounts: Arc<Accounts>,
}
type CallbackResult = std::result::Result<String, String>;
type CallbackSender = Arc<Mutex<Option<oneshot::Sender<CallbackResult>>>>;
#[derive(Clone)]
struct Callback {
    state: String,
    tx: CallbackSender,
}

async fn callback(
    State(s): State<Callback>,
    Query(q): Query<HashMap<String, String>>,
) -> Html<&'static str> {
    if q.get("state") != Some(&s.state) {
        return Html("<meta charset=utf-8>授权状态不匹配，请回到账号池重新登录。");
    }
    let result = if q.contains_key("error") {
        Err("用户取消授权，或授权服务器拒绝了请求".into())
    } else {
        q.get("code")
            .filter(|s| !s.is_empty() && s.len() < 8192)
            .cloned()
            .ok_or("授权回调缺少有效代码".into())
    };
    if let Some(tx) = s.tx.lock().unwrap().take() {
        let _ = tx.send(result);
    }
    Html("<meta charset=utf-8><title>Codex 账号池</title>已收到授权回调，请回到账号池查看结果。可以关闭此页面。")
}

impl OAuth {
    pub fn new(accounts: Arc<Accounts>) -> Arc<Self> {
        Arc::new(Self {
            accounts,
            status: Arc::new(Mutex::new(LoginStatus {
                phase: "idle".into(),
                message: "尚未开始登录".into(),
            })),
            active: Mutex::new(None),
        })
    }
    pub fn status(&self) -> LoginStatus {
        self.status.lock().unwrap().clone()
    }
    pub fn cancel(&self) {
        if let Some(t) = self.active.lock().unwrap().as_ref() {
            t.cancel();
        }
    }
    pub fn active(&self) -> bool { self.active.lock().unwrap().is_some() }
    pub async fn start(self: &Arc<Self>, expected: Option<String>) -> Result<String> {
        // Reserve before any await; concurrent login clicks cannot race listener ownership.
        let cancel = CancellationToken::new();
        {
            let mut active = self.active.lock().unwrap();
            if active.is_some() {
                return Err("已有登录进行中，请完成或取消后重试".into());
            }
            *active = Some(cancel.clone());
        }
        let listener = match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 1455)).await {
            Ok(l) => l,
            Err(_) => match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 1457)).await {
                Ok(l) => l,
                Err(_) => {
                    *self.active.lock().unwrap() = None;
                    return Err(
                        "授权回调端口 1455 和 1457 均被占用，请结束其他登录流程后再试".into(),
                    );
                }
            },
        };
        let port = listener
            .local_addr()
            .map_err(|_| "无法读取授权回调端口")?
            .port();
        let verifier = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = uuid::Uuid::new_v4().to_string();
        let redirect = format!("http://localhost:{port}/auth/callback");
        let mut url = url::Url::parse("https://auth.openai.com/oauth/authorize").unwrap();
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", &redirect),
            ("scope", "openid profile email offline_access"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "codex_cli_rs"),
        ]);
        let (tx, rx) = oneshot::channel();
        let router = Router::new()
            .route("/auth/callback", get(callback))
            .with_state(Callback {
                state,
                tx: Arc::new(Mutex::new(Some(tx))),
            });
        *self.status.lock().unwrap() = LoginStatus {
            phase: "pending".into(),
            message: "请在浏览器完成登录。添加下一个账号前请确认浏览器选择的是该账号。".into(),
        };
        let this = self.clone();
        tokio::spawn(async move {
            let server_cancel = CancellationToken::new();
            let stop = server_cancel.clone();
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, router)
                    .with_graceful_shutdown(stop.cancelled_owned())
                    .await;
            });
            let result = tokio::select! {
                _=cancel.cancelled()=>Err("登录已取消".into()),
                r=tokio::time::timeout(Duration::from_secs(600),async {
                    let code=rx.await.map_err(|_|"登录回调已关闭")??;
                    let r=this.accounts.http.post("https://auth.openai.com/oauth/token").await?.timeout(Duration::from_secs(30))
                        .form(&[("grant_type","authorization_code"),("client_id",CLIENT_ID),("redirect_uri",&redirect),("code_verifier",&verifier),("code",&code)])
                        .send().await.map_err(|_|"授权交换网络失败，请检查网络后重试")?;
                    if !r.status().is_success(){return Err(exchange_error(r.status().as_u16()));}
                    let t=tokens_from_response(&r.json().await.map_err(|_|"授权响应格式错误")?,None)?;
                    this.accounts.accept_login(t,expected.as_deref()).await?;
                    Ok::<(),String>(())
                })=>r.unwrap_or_else(|_|Err("登录超过 10 分钟，请重新开始".into()))
            };
            server_cancel.cancel();
            // Force-close malicious/stalled local callback connections after a short grace period.
            let abort = server.abort_handle();
            if tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .is_err()
            {
                abort.abort();
            }
            *this.status.lock().unwrap() = match result {
                Ok(()) => LoginStatus {
                    phase: "success".into(),
                    message: "账号授权成功，已安全保存凭据".into(),
                },
                Err(e) => LoginStatus {
                    phase: "error".into(),
                    message: e,
                },
            };
            *this.active.lock().unwrap() = None;
        });
        Ok(url.into())
    }
}

// Never include the token endpoint's response body or the callback code in errors.
fn exchange_error(status: u16) -> String {
    match status {
        403 => "授权交换被拒绝（HTTP 403）。Fusion 已使用本次读取到的代理发送请求，未回退直连，也未自动重试；账号尚未添加。拒绝来源仍未确认，请重新发起授权。".into(),
        400 | 401 => format!("授权交换失败（HTTP {status}），请重新发起授权"),
        _ => format!("授权交换失败（HTTP {status}），请检查网络或稍后重新授权"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn callback_does_not_consume_login_on_wrong_state() {
        let (tx, mut rx) = oneshot::channel();
        let cb = Callback {
            state: "correct".into(),
            tx: Arc::new(Mutex::new(Some(tx))),
        };
        let _ = callback(
            State(cb.clone()),
            Query(HashMap::from([
                ("state".into(), "wrong".into()),
                ("code".into(), "secret-code".into()),
            ])),
        )
        .await;
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let _ = callback(
            State(cb),
            Query(HashMap::from([
                ("state".into(), "correct".into()),
                ("error".into(), "denied".into()),
            ])),
        )
        .await;
        assert!(rx.await.unwrap().is_err());
    }
}
