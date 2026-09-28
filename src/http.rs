//! Shared `ureq`-backed HTTP plumbing — the one place any network caller in
//! this crate builds an `Agent` and interprets its result. Originally lived
//! only in `mal.rs`; extracted once `update.rs`'s GitHub release check
//! became a second real caller, so the native-tls gotcha below only has to
//! be gotten right in one place.
//!
//! **Must use an explicit `Agent` with `.tls_connector(...)` set, never the
//! bare `ureq::get(...)`/`ureq::post(...)` shortcut functions.** Found the
//! hard way, against the real MAL API: `ureq`'s `native-tls` feature is
//! never picked up automatically by those shortcuts — per `ureq`'s own
//! source (`lib.rs`'s `default_tls_config`), only the `tls` (rustls)
//! feature wires a default TLS backend into them. With `default-features =
//! false` and only `native-tls` enabled (`Cargo.toml`, chosen to use the
//! pkg-config+openssl `shell.nix` already provisions), the shortcuts had
//! *no* TLS backend at all — every request failed with "Unknown Scheme:
//! cannot make HTTPS request because no TLS backend is configured". A
//! future third caller must go through [`build_agent`], never the bare
//! shortcuts, or this bug resurfaces.

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

/// Builds a fresh `Agent` with the explicit native-tls connector every real
/// network call in this crate needs (see the module doc comment). Building
/// the connector fresh per call (rather than caching it) is deliberate
/// simplicity, not an oversight — every caller of this crate is an
/// on-demand, low-frequency path (a login, a refresh, an update check), so
/// the tiny one-time TLS-connector setup cost is a complete non-issue.
pub fn build_agent() -> Result<ureq::Agent, HttpError> {
    let connector = native_tls::TlsConnector::new()
        .map_err(|e| HttpError(format!("could not initialize TLS: {e}")))?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(connector))
        .build())
}

/// Maps a `ureq` call result to `HttpResponse`/`HttpError` — a non-2xx
/// status is still `Ok(HttpResponse { status, .. })` (the caller decides
/// what a given status means; this layer only distinguishes "got a real
/// HTTP response" from "never reached the server at all").
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
