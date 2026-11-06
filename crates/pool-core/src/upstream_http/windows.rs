//! Use the active Windows connection, including per-connection/PAC settings.
//! API contracts: Microsoft WinHttpGetIEProxyConfigForCurrentUser / WinHttpGetProxyForUrl.
use super::*;
use windows_sys::Win32::{
    Foundation::{GetLastError, GlobalFree},
    Networking::WinHttp::*,
};

struct Config(WINHTTP_CURRENT_USER_IE_PROXY_CONFIG);
impl Drop for Config {
    fn drop(&mut self) {
        unsafe {
            GlobalFree(self.0.lpszProxy.cast());
            GlobalFree(self.0.lpszProxyBypass.cast());
            GlobalFree(self.0.lpszAutoConfigUrl.cast());
        }
    }
}
struct ProxyInfo(WINHTTP_PROXY_INFO);
impl Drop for ProxyInfo {
    fn drop(&mut self) {
        unsafe {
            GlobalFree(self.0.lpszProxy.cast());
            GlobalFree(self.0.lpszProxyBypass.cast());
        }
    }
}
struct Session(*mut std::ffi::c_void);
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}
// The pointers come from WinHTTP and remain owned by the enclosing RAII value.
unsafe fn text(ptr: *const u16) -> Result<Option<String>> {
    if ptr.is_null() || *ptr == 0 {
        return Ok(None);
    }
    let mut len = 0;
    while len < 32768 && *ptr.add(len) != 0 {
        len += 1;
    }
    if len == 32768 {
        return Err(INVALID_PROXY.into());
    }
    String::from_utf16(std::slice::from_raw_parts(ptr, len))
        .map(Some)
        .map_err(|_| INVALID_PROXY.into())
}
pub(super) fn resolve(target: &Url) -> Result<Option<Route>> {
    let mut config = Config(WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default());
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config.0) } == 0 {
        return if unsafe { GetLastError() } == 2 {
            Ok(None)
        } else {
            Err("Fusion 无法读取当前 Windows 用户代理，已阻止外部请求直连".into())
        };
    }
    if let Some(raw) = unsafe { text(config.0.lpszProxy) }? {
        return Ok(Some(Route {
            proxy: static_proxy(&raw, target.scheme())?,
            source: ProxySource::WindowsStaticConfigured,
        }));
    }
    let has_pac =
        !config.0.lpszAutoConfigUrl.is_null() && unsafe { *config.0.lpszAutoConfigUrl } != 0;
    // Explicit environment configuration precedes optional WPAD discovery, but not an explicit PAC.
    if !has_pac {
        if let Some(route) = environment_route(target.scheme(), |key| std::env::var(key).ok())? {
            return Ok(Some(route));
        }
        if config.0.fAutoDetect == 0 {
            return Ok(None);
        }
    }
    auto_route(
        target,
        has_pac.then_some(config.0.lpszAutoConfigUrl.cast_const()),
    )
    .map(Some)
}
fn auto_route(target: &Url, pac: Option<*const u16>) -> Result<Route> {
    let agent: Vec<_> = "Fusion proxy resolver\0".encode_utf16().collect();
    let session = Session(unsafe {
        WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    });
    if session.0.is_null() {
        return Err("Fusion 无法初始化 Windows 自动代理解析".into());
    }
    if unsafe { WinHttpSetTimeouts(session.0, 5000, 5000, 5000, 5000) } == 0 {
        return Err("Fusion 无法设置自动代理解析时限".into());
    }
    let mut options = WINHTTP_AUTOPROXY_OPTIONS::default();
    if let Some(pac) = pac {
        options.dwFlags = WINHTTP_AUTOPROXY_CONFIG_URL;
        options.lpszAutoConfigUrl = pac;
    } else {
        options.dwFlags = WINHTTP_AUTOPROXY_AUTO_DETECT;
        options.dwAutoDetectFlags = WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
    }
    // Never send the user's Windows credentials automatically to a PAC server.
    options.fAutoLogonIfChallenged = 0;
    let url: Vec<_> = target.as_str().encode_utf16().chain(Some(0)).collect();
    let mut info = ProxyInfo(WINHTTP_PROXY_INFO::default());
    if unsafe { WinHttpGetProxyForUrl(session.0, url.as_ptr(), &mut options, &mut info.0) } == 0 {
        return Err("Fusion 解析 Windows 自动代理失败，已阻止外部请求直连".into());
    }
    if info.0.dwAccessType != WINHTTP_ACCESS_TYPE_NAMED_PROXY {
        return Err("Windows 自动代理为此 AI 服务返回直连，Fusion 已阻止发送".into());
    }
    let raw = unsafe { text(info.0.lpszProxy) }?.ok_or(INVALID_PROXY)?;
    // WinHTTP returns an ordered server list. Select one before sending; never retry a request.
    Ok(Route {
        proxy: static_proxy(&raw, target.scheme())?,
        source: ProxySource::WindowsAutomaticConfigured,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_pac_is_resolved_per_target_and_direct_is_blocked() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pac_url = format!("http://{}/proxy.pac", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let router = axum::Router::new().route("/proxy.pac", axum::routing::get(|| async {
                ([("Content-Type", "application/x-ns-proxy-autoconfig")],
                "function FindProxyForURL(url, host) { if (host == 'auth.example.test') return 'PROXY 127.0.0.1:8123'; if (host == 'chat.example.test') return 'PROXY 127.0.0.1:8456'; return 'DIRECT'; }")
            }));
            let _ = axum::serve(listener, router).await;
        });
        // No registry writes and no external upstream requests; only a temporary local PAC server.
        let result = tokio::task::spawn_blocking(move || {
            let pac: Vec<_> = pac_url.encode_utf16().chain(Some(0)).collect();
            let auth = auto_route(
                &Url::parse("https://auth.example.test/oauth/token").unwrap(),
                Some(pac.as_ptr()),
            )?;
            let chat = auto_route(
                &Url::parse("https://chat.example.test/responses").unwrap(),
                Some(pac.as_ptr()),
            )?;
            let direct = auto_route(
                &Url::parse("https://direct.example.test/").unwrap(),
                Some(pac.as_ptr()),
            );
            Ok::<_, String>((auth, chat, direct.is_err()))
        });
        let (auth, chat, direct_blocked) = tokio::time::timeout(Duration::from_secs(20), result)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(auth.proxy, "http://127.0.0.1:8123/");
        assert_eq!(chat.proxy, "http://127.0.0.1:8456/");
        assert!(direct_blocked);
        server.abort();
    }
}
