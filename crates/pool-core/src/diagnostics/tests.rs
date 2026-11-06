use super::*;
use std::io::{Error, ErrorKind};

#[test]
fn transport_sources_classify_typed_io_and_tls_without_retaining_details() {
    for (io, expected) in [
        (
            ErrorKind::ConnectionRefused,
            NetworkErrorKind::ConnectionRefused,
        ),
        (
            ErrorKind::ConnectionReset,
            NetworkErrorKind::ConnectionReset,
        ),
        (
            ErrorKind::ConnectionAborted,
            NetworkErrorKind::ConnectionAborted,
        ),
        (ErrorKind::BrokenPipe, NetworkErrorKind::ConnectionClosed),
        (ErrorKind::UnexpectedEof, NetworkErrorKind::UnexpectedEof),
        (ErrorKind::TimedOut, NetworkErrorKind::Timeout),
    ] {
        let wrapped = Error::other(Error::new(io, "private-error-detail"));
        let actual = network_source(&wrapped).unwrap();
        assert_eq!(actual, expected);
        assert!(!serde_json::to_string(&actual)
            .unwrap()
            .contains("private-error-detail"));
    }
    let tls = Error::new(
        ErrorKind::InvalidData,
        rustls::Error::General("private-tls-detail".into()),
    );
    assert_eq!(network_source(&tls), Some(NetworkErrorKind::Tls));
    // A message mentioning TLS or an error name is not evidence of that error type.
    assert_eq!(
        network_source(&Error::other("TLS ConnectionReset private-detail")),
        None
    );
    #[cfg(windows)]
    assert_eq!(
        network_source(&Error::from_raw_os_error(11001)),
        Some(NetworkErrorKind::Dns)
    );
}

#[test]
fn legacy_diagnostics_deserialize_without_inventing_a_network_cause() {
    let old = serde_json::json!({"appVersion":"0.1.14","upstreamStatus":200,
        "responseType":"missing","failureStage":"response_read","proxySource":"windows_static_configured"});
    let diagnostic: RequestDiagnostic = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(diagnostic.network_error_kind, None);
    assert_eq!(diagnostic.detected_response_type, None);
    assert_eq!(serde_json::to_value(diagnostic).unwrap(), old);
}

#[test]
fn private_tunnel_signatures_require_exact_match_without_capturing_other_text() {
    #[derive(Debug)]
    struct Leaf(&'static str);
    impl std::fmt::Display for Leaf {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Leaf {}
    assert_eq!(
        network_source(&Leaf("tunnel error: unsuccessful")),
        Some(NetworkErrorKind::ProxyTunnel)
    );
    assert_eq!(
        network_source(&Leaf("tunnel error: proxy authorization required")),
        Some(NetworkErrorKind::ProxyAuthentication)
    );
    assert_eq!(
        network_source(&Leaf("private-url tunnel error: unsuccessful")),
        None
    );
    assert_eq!(
        network_source(&Leaf("tunnel error: unsuccessful private-secret")),
        None
    );
}
