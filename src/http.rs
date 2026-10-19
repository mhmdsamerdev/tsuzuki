//! Shared `ureq` HTTP plumbing: the one place that builds an `Agent` and
//! interprets its result.
//!
//! Always go through [`build_agent`], never the bare `ureq::get`/`ureq::post`
//! shortcuts. With only `native-tls` enabled, the shortcuts have no TLS
//! backend and every HTTPS request fails.

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

/// Builds a fresh `Agent` with an explicit native-tls connector. Not cached:
/// callers are rare, on-demand paths, so the setup cost is irrelevant.
pub fn build_agent() -> Result<ureq::Agent, HttpError> {
    let connector = native_tls::TlsConnector::new()
        .map_err(|e| HttpError(format!("could not initialize TLS: {e}")))?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(connector))
        .build())
}

/// Maps a `ureq` result to `HttpResponse`/`HttpError`. A non-2xx status is
/// still `Ok`; only a failure to get any HTTP response is an `Err`.
pub fn map_ureq_result(
    result: Result<ureq::Response, ureq::Error>,
) -> Result<HttpResponse, HttpError> {
    match result {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.into_string().unwrap_or_default();
            Ok(HttpResponse { status, body })
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Ok(HttpResponse { status: code, body })
        }
        Err(e) => Err(HttpError(transport_reason(&e).to_string())),
    }
}

/// Short, URL-free reason for a failure to get any HTTP response.
fn transport_reason(err: &ureq::Error) -> &'static str {
    let ureq::Error::Transport(t) = err else {
        return "network error";
    };
    match t.kind() {
        ureq::ErrorKind::Dns => "no internet connection (DNS lookup failed)",
        ureq::ErrorKind::ConnectionFailed => "connection failed",
        ureq::ErrorKind::Io => {
            let timed_out = std::error::Error::source(t)
                .and_then(|s| s.downcast_ref::<std::io::Error>())
                .is_some_and(|io| {
                    matches!(
                        io.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    )
                });
            if timed_out {
                "timed out"
            } else {
                "connection interrupted"
            }
        }
        _ => "network error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason_for(url: &str) -> String {
        let err = map_ureq_result(ureq::get(url).call())
            .err()
            .expect("request should fail");
        err.0
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
    fn map_ureq_result_read_timeout_yields_timed_out() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/");
        let agent = ureq::AgentBuilder::new()
            .timeout_read(std::time::Duration::from_millis(100))
            .build();
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
        let resp = map_ureq_result(ureq::get(&format!("http://127.0.0.1:{port}/")).call())
            .expect("a 503 is still a response");
        server.join().unwrap();
        assert_eq!((resp.status, resp.body.as_str()), (503, "no"));
    }
}
