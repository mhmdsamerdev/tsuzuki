//! Shared `ureq` HTTP plumbing: the one place that builds an `Agent` and
//! interprets its result.
//!
//! Always go through [`build_agent`], the one place an `Agent` is built.

use std::fmt;

pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug)]
pub struct HttpError(pub String);

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The shared agent settings: rustls with the system CA store, and non-2xx
/// statuses returned as responses rather than errors.
fn base_config() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .tls_config(tls)
}

/// The default resolver, except any lookup I/O failure is reported as
/// `HostNotFound`, which tells a DNS failure apart from a failed connection.
#[derive(Debug, Default)]
struct DnsResolver(ureq::unversioned::resolver::DefaultResolver);

impl ureq::unversioned::resolver::Resolver for DnsResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        match self.0.resolve(uri, config, timeout) {
            Err(ureq::Error::Io(_)) => Err(ureq::Error::HostNotFound),
            other => other,
        }
    }
}

fn agent_with(config: ureq::config::Config) -> ureq::Agent {
    ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        DnsResolver::default(),
    )
}

/// Builds a fresh `Agent`. Not cached: callers are rare, on-demand paths, so
/// the setup cost is irrelevant.
pub fn build_agent() -> Result<ureq::Agent, HttpError> {
    Ok(agent_with(base_config().build()))
}

/// Maps a `ureq` result to `HttpResponse`/`HttpError`. A non-2xx status is
/// still `Ok`; only a failure to get any HTTP response is an `Err`.
pub fn map_ureq_result(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<HttpResponse, HttpError> {
    match result {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            Ok(HttpResponse { status, body })
        }
        Err(e) => Err(HttpError(transport_reason(&e).to_string())),
    }
}

/// Short, URL-free reason for a failure to get any HTTP response.
fn transport_reason(err: &ureq::Error) -> &'static str {
    match err {
        ureq::Error::HostNotFound => "no internet connection (DNS lookup failed)",
        ureq::Error::ConnectionFailed | ureq::Error::Tls(_) => "connection failed",
        ureq::Error::Timeout(_) => "timed out",
        ureq::Error::Io(io) => match io.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => "timed out",
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::HostUnreachable
            | std::io::ErrorKind::NetworkUnreachable
            | std::io::ErrorKind::InvalidData => "connection failed",
            _ => "connection interrupted",
        },
        _ => "network error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason_for(url: &str) -> String {
        let agent = build_agent().unwrap();
        let err = map_ureq_result(agent.get(url).call())
            .err()
            .expect("request should fail");
        err.0
    }

    #[test]
    fn transport_reason_maps_each_error_to_its_short_message() {
        use std::io::{Error as IoError, ErrorKind};
        let cases: Vec<(ureq::Error, &str)> = vec![
            (
                ureq::Error::HostNotFound,
                "no internet connection (DNS lookup failed)",
            ),
            (ureq::Error::ConnectionFailed, "connection failed"),
            (ureq::Error::Tls("bad certificate"), "connection failed"),
            (ureq::Error::Timeout(ureq::Timeout::Global), "timed out"),
            (
                ureq::Error::Io(IoError::from(ErrorKind::TimedOut)),
                "timed out",
            ),
            (
                ureq::Error::Io(IoError::from(ErrorKind::WouldBlock)),
                "timed out",
            ),
            (
                ureq::Error::Io(IoError::from(ErrorKind::ConnectionRefused)),
                "connection failed",
            ),
            (
                ureq::Error::Io(IoError::from(ErrorKind::InvalidData)),
                "connection failed",
            ),
            (
                ureq::Error::Io(IoError::from(ErrorKind::UnexpectedEof)),
                "connection interrupted",
            ),
            (ureq::Error::TooManyRedirects, "network error"),
        ];
        for (err, expected) in cases {
            assert_eq!(transport_reason(&err), expected, "{err:?}");
        }
    }

    #[test]
    fn map_ureq_result_connection_refused_yields_short_reason_without_url() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert_eq!(
            reason_for(&format!("http://127.0.0.1:{port}/secret/path")),
            "connection failed"
        );
    }

    #[test]
    fn map_ureq_result_dns_failure_yields_short_reason_without_url() {
        // `.invalid` is reserved (RFC 6761) and never resolves.
        assert_eq!(
            reason_for("http://nonexistent.invalid/secret/path"),
            "no internet connection (DNS lookup failed)"
        );
    }

    #[test]
    fn build_agent_https_to_plaintext_server_fails_tls_handshake() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        });
        let agent = build_agent().expect("agent builds");
        let err = map_ureq_result(agent.get(&format!("https://127.0.0.1:{port}/")).call())
            .err()
            .expect("handshake should fail");
        server.join().unwrap();
        assert_eq!(err.0, "connection failed");
    }

    #[test]
    fn map_ureq_result_read_timeout_yields_timed_out() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/");
        let agent = agent_with(
            base_config()
                .timeout_recv_response(Some(std::time::Duration::from_millis(100)))
                .build(),
        );
        // The listener accepts into its backlog but never answers.
        let err = map_ureq_result(agent.get(&url).call())
            .err()
            .expect("should time out");
        drop(listener);
        assert_eq!(err.0, "timed out");
    }

    #[test]
    fn map_ureq_result_non_2xx_is_ok_with_status() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            s.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\n\r\nno")
                .unwrap();
        });
        let agent = build_agent().unwrap();
        let resp = map_ureq_result(agent.get(&format!("http://127.0.0.1:{port}/")).call())
            .expect("a 503 is still a response");
        server.join().unwrap();
        assert_eq!((resp.status, resp.body.as_str()), (503, "no"));
    }
}
