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
        Err(e) => Err(HttpError(e.to_string())),
    }
}
