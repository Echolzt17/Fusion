//! Finite metadata categories only. Never retain raw headers, network errors or proxy addresses.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseType {
    Sse,
    Html,
    Json,
    Missing,
    Other,
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    IngressQueue,
    Upload,
    RequestValidation,
    Queue,
    ProxyResolution,
    Connect,
    ResponseHeaders,
    ResponseRead,
    EventValidation,
    UpstreamRejected,
    Cancelled,
    ServiceStopped,
    Unknown,
}
/// Only finite categories leave the transport stack; never retain its error text or URLs.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkErrorKind {
    Dns,
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    ConnectionClosed,
    Connect,
    Tls,
    ProxyAuthentication,
    ProxyTunnel,
    Timeout,
    HttpProtocol,
    BodyRead,
    UnexpectedEof,
    StreamEnded,
    Unknown,
}
impl NetworkErrorKind {
    pub(crate) fn classify(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            return Self::Timeout;
        }
        network_source(error).unwrap_or_else(|| {
            if error.is_connect() {
                Self::Connect
            } else if error.is_body() || error.is_decode() {
                Self::BodyRead
            } else {
                Self::Unknown
            }
        })
    }
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Dns => "名称解析失败",
            Self::ConnectionRefused => "连接被拒绝",
            Self::ConnectionReset => "连接被重置",
            Self::ConnectionAborted => "连接被中止",
            Self::ConnectionClosed => "连接已关闭",
            Self::Connect => "建立连接失败，具体原因未确认",
            Self::Tls => "TLS 校验或握手失败",
            Self::ProxyAuthentication => "代理要求身份验证",
            Self::ProxyTunnel => "代理隧道建立失败",
            Self::Timeout => "网络等待超时",
            Self::HttpProtocol => "HTTP 传输协议异常",
            Self::BodyRead => "响应体传输读取失败",
            Self::UnexpectedEof => "传输数据尚未完整接收即结束",
            Self::StreamEnded => "响应流提前结束，未收到完成事件",
            Self::Unknown => "网络错误，具体原因未确认",
        }
    }
}
fn network_source(error: &(dyn std::error::Error + 'static)) -> Option<NetworkErrorKind> {
    use NetworkErrorKind as Kind;
    let mut current = Some(error);
    let mut fallback = None;
    // Bound traversal even if a third-party error ever contains a cyclic source.
    for _ in 0..32 {
        let Some(cause) = current else { break };
        if cause.is::<rustls::Error>() {
            return Some(Kind::Tls);
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind;
            #[cfg(windows)]
            if matches!(io.raw_os_error(), Some(11001..=11004)) {
                return Some(Kind::Dns);
            }
            let kind = match io.kind() {
                ErrorKind::ConnectionRefused => Some(Kind::ConnectionRefused),
                ErrorKind::ConnectionReset => Some(Kind::ConnectionReset),
                ErrorKind::ConnectionAborted => Some(Kind::ConnectionAborted),
                ErrorKind::BrokenPipe | ErrorKind::NotConnected => Some(Kind::ConnectionClosed),
                ErrorKind::UnexpectedEof => Some(Kind::UnexpectedEof),
                ErrorKind::TimedOut => Some(Kind::Timeout),
                _ => None,
            };
            if let Some(kind) = kind {
                return Some(kind);
            }
            // io::Error::source can skip the wrapped error itself, including rustls::Error.
            current = io
                .get_ref()
                .map(|e| e as &(dyn std::error::Error + 'static))
                .or_else(|| cause.source());
            continue;
        }
        if let Some(http) = cause.downcast_ref::<hyper::Error>() {
            fallback = if http.is_incomplete_message() {
                Some(Kind::UnexpectedEof)
            } else if http.is_closed() {
                Some(Kind::ConnectionClosed)
            } else if http.is_parse() {
                Some(Kind::HttpProtocol)
            } else {
                fallback
            };
        }
        if cause.source().is_none() {
            // hyper-util 0.1.21 does not export TunnelError. Match only its exact,
            // fixed leaf messages; never copy or save arbitrary Display output.
            if message_is(cause, "tunnel error: proxy authorization required") {
                return Some(Kind::ProxyAuthentication);
            }
            if [
                "tunnel error: unsuccessful",
                "tunnel error: unexpected end of file",
                "tunnel error: proxy response headers too long",
            ]
            .iter()
            .any(|message| message_is(cause, message))
            {
                return Some(Kind::ProxyTunnel);
            }
        }
        current = cause.source();
    }
    fallback
}
fn message_is(error: &dyn std::fmt::Display, expected: &str) -> bool {
    struct Exact<'a>(&'a str);
    impl std::fmt::Write for Exact<'_> {
        fn write_str(&mut self, part: &str) -> std::fmt::Result {
            self.0 = self.0.strip_prefix(part).ok_or(std::fmt::Error)?;
            Ok(())
        }
    }
    let mut exact = Exact(expected);
    std::fmt::write(&mut exact, format_args!("{error}")).is_ok() && exact.0.is_empty()
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProxySource {
    ManagedKernel,
    EnvironmentConfigured,
    WindowsStaticConfigured,
    WindowsAutomaticConfigured,
    LoopbackDirect,
    NoneConfigured,
    Unknown,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RequestDiagnostic {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<fusion_report_protocol::FailureDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<fusion_report_protocol::Execution>,
    pub app_version: String,
    pub upstream_status: Option<u16>,
    /// Content-Type declaration only; preserved even when the body proves another format.
    pub response_type: Option<ResponseType>,
    /// Format established by the existing body parser, never inferred from the route or status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_response_type: Option<ResponseType>,
    pub failure_stage: Option<FailureStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_error_kind: Option<NetworkErrorKind>,
    /// Selected configuration for this request, not proof of the proxy's outbound route.
    pub proxy_source: ProxySource,
}
pub(crate) fn failure_origin(status: &str, d: &RequestDiagnostic) -> &'static str {
    if matches!(status, "reauthorization_required" | "upstream_rate_limited")
        || matches!(d.upstream_status, Some(401 | 429))
        || d.details.as_ref().and_then(|v| v.upstream_failure.as_deref()).is_some_and(|v| matches!(v, "authorization" | "rate_limit")) { return "account"; }
    if matches!(d.failure_stage, Some(FailureStage::ProxyResolution | FailureStage::Connect | FailureStage::ResponseHeaders | FailureStage::ResponseRead))
        && (d.network_error_kind.is_some() || matches!(status, "proxy_configuration" | "upstream_header_timeout" | "upstream_read_timeout" | "stream_interrupted")) { return "transport"; }
    if d.upstream_status.is_some_and(|n| n >= 500) || d.details.as_ref().and_then(|v| v.upstream_failure.as_deref()) == Some("server") { return "upstream_service"; }
    if matches!(d.failure_stage, Some(FailureStage::IngressQueue | FailureStage::Upload | FailureStage::RequestValidation | FailureStage::Queue))
        || matches!(status, "storage_error" | "service_stopped") { return "local"; }
    "unknown"
}
#[cfg(test)]
mod tests;
pub fn proxy_source() -> ProxySource {
    if [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .iter()
    .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
    {
        return ProxySource::EnvironmentConfigured;
    }
    windows_proxy_source()
}
#[cfg(windows)]
fn windows_proxy_source() -> ProxySource {
    // Read only the enabled DWORD and string lengths, never proxy URLs or credentials.
    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut std::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings");
    let flag = wide("ProxyEnable");
    let mut enabled = 0u32;
    let mut size = 4u32;
    let root = 0x80000001u32 as i32 as isize;
    let result = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            flag.as_ptr(),
            0x10,
            std::ptr::null_mut(),
            (&mut enabled as *mut u32).cast(),
            &mut size,
        )
    };
    if result != 0 && result != 2 {
        return ProxySource::Unknown;
    }
    if enabled != 0 {
        return ProxySource::WindowsStaticConfigured;
    }
    let pac = wide("AutoConfigURL");
    let mut size = 0;
    let result = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            pac.as_ptr(),
            2,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if result == 2 || result == 0 && size <= 2 {
        ProxySource::NoneConfigured
    } else {
        ProxySource::Unknown
    }
}
#[cfg(not(windows))]
fn windows_proxy_source() -> ProxySource {
    ProxySource::NoneConfigured
}
