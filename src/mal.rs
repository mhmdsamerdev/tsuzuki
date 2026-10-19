//! MyAnimeList OAuth login, token storage and refresh, and the
//! authenticated-HTTP surface `sync.rs` builds on.
//!
//! Linking, matching and episode merging live in `sync.rs`; this module
//! never fetches the user's list or pushes episode numbers.
//!
//! Tokens are never printed or logged: `TokenRecord` redacts both fields in
//! `Debug`, and `TokenStatus` carries no credential.
//!
//! All network access goes through the [`MalHttpClient`] trait; only
//! [`UreqClient`] touches the network, so everything else is testable with a
//! fake client.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rand::Rng;
use rand::distributions::Alphanumeric;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::state::atomic_write;

pub const AUTHORIZE_URL: &str = "https://myanimelist.net/v1/oauth2/authorize";
pub const TOKEN_URL: &str = "https://myanimelist.net/v1/oauth2/token";

/// Tsuzuki's registered MAL client_id. Public on purpose: this is a
/// PKCE/no-secret app type, so `client_id` is not confidential and no
/// `client_secret` exists. Do not remove or redact it.
///
/// The app must be registered as a type that issues no secret. After
/// rotating, POST a fake code to `TOKEN_URL`: it must fail with
/// `invalid_request`/`invalid_grant`, never `invalid_client`.
pub(crate) const DEFAULT_CLIENT_ID: &str = "1c0e2546c3b0f9e8cfd215ba17fd0b15";

/// The `[mal] client_id` override when set, `DEFAULT_CLIENT_ID` otherwise.
/// Callers go through this rather than reading the config field.
pub(crate) fn resolve_client_id(config: &Config) -> &str {
    if config.mal.client_id.is_empty() {
        DEFAULT_CLIENT_ID
    } else {
        &config.mal.client_id
    }
}

/// A token expiring within this many seconds is refreshed first.
const EXPIRY_SAFETY_MARGIN_SECS: u64 = 60;

/// How long `sync login`'s local callback listener waits for MAL to redirect
/// back before giving up.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(120);

// --- Token storage: ~/.local/share/tsuzuki/mal_token.toml ---

/// A stored MAL OAuth token pair, written atomically with `0600` permissions
/// on Unix since it holds live credentials.
///
/// `Debug` is hand-written to redact both tokens; serde still writes them.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenRecord {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds at which `access_token` expires.
    pub expires_at: u64,
}

impl fmt::Debug for TokenRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenRecord")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// `mal_token.toml` in the same data directory as the state file.
pub fn mal_token_path() -> io::Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "could not resolve data directory")
    })?;
    Ok(base.join("tsuzuki").join("mal_token.toml"))
}

/// Reads the stored token. A missing file is `Ok(None)`: never logged in.
pub fn read_token(path: &Path) -> io::Result<Option<TokenRecord>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let record = toml::from_str(&contents)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(record))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Writes `token` to `path` through the shared atomic write, `0600` on Unix.
pub fn write_token(path: &Path, token: &TokenRecord) -> io::Result<()> {
    let contents =
        toml::to_string_pretty(token).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(path, contents.as_bytes(), Some(0o600))
}

// --- PKCE (RFC 7636), plain method — MAL does not support S256 ---

pub struct Pkce {
    pub code_verifier: String,
    /// Identical to `code_verifier` under `code_challenge_method=plain`.
    pub code_challenge: String,
}

/// A fresh verifier/challenge pair for one login attempt: 64 characters from
/// `[A-Za-z0-9]`, within RFC 7636's charset and 43–128 length bound.
pub fn generate_pkce() -> Pkce {
    let verifier = random_alphanumeric(64);
    Pkce {
        code_challenge: verifier.clone(),
        code_verifier: verifier,
    }
}

/// Generates a fresh CSRF `state` value for one login attempt.
pub fn generate_state() -> String {
    random_alphanumeric(32)
}

fn random_alphanumeric(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

// --- Authorize URL ---

pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback")
}

/// The authorization URL `sync login` opens: code grant with PKCE `plain`.
pub fn authorize_url(client_id: &str, port: u16, code_challenge: &str, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={}&code_challenge={}&code_challenge_method=plain&state={}&redirect_uri={}",
        percent_encode(client_id),
        percent_encode(code_challenge),
        percent_encode(state),
        percent_encode(&redirect_uri(port)),
    )
}

/// RFC 3986 percent-encoding: everything but unreserved characters is escaped.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Decodes a percent-encoded query component; `+` is a space, as in
/// `application/x-www-form-urlencoded`.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            let key = it.next()?;
            let value = it.next().unwrap_or("");
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

// --- Local OAuth-callback listener ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOutcome {
    Code(String),
    /// The user declined on MAL's page (`error=...` on the redirect). Ends
    /// the wait immediately, unlike a state mismatch.
    Denied(String),
}

#[derive(Debug)]
pub enum ListenError {
    Bind(io::Error),
    Timeout,
}

impl fmt::Display for ListenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListenError::Bind(e) => write!(f, "could not bind local callback listener: {e}"),
            ListenError::Timeout => write!(
                f,
                "timed out after {}s waiting for MyAnimeList to redirect back",
                LOGIN_TIMEOUT.as_secs()
            ),
        }
    }
}

enum HandleOutcome {
    Code(String),
    Denied(String),
    /// State mismatch, malformed request, or neither `code` nor `error`:
    /// reject and keep listening (CSRF guard).
    Ignore,
}

/// Waits for one valid callback (matching `state`) on `listener`. Polls a
/// non-blocking accept so `timeout` holds without a second thread.
pub fn listen_for_callback(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<CallbackOutcome, ListenError> {
    listener.set_nonblocking(true).map_err(ListenError::Bind)?;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(ListenError::Timeout);
        }
        match listener.accept() {
            Ok((stream, _)) => match handle_connection(stream, expected_state) {
                HandleOutcome::Code(code) => return Ok(CallbackOutcome::Code(code)),
                HandleOutcome::Denied(reason) => return Ok(CallbackOutcome::Denied(reason)),
                HandleOutcome::Ignore => continue,
            },
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn handle_connection(stream: TcpStream, expected_state: &str) -> HandleOutcome {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let Ok(reader_stream) = stream.try_clone() else {
        return HandleOutcome::Ignore;
    };
    let mut writer_stream = stream;
    let mut reader = BufReader::new(reader_stream);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return HandleOutcome::Ignore;
    }
    // Skip the remaining headers up to the blank line; a GET has no body.
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let params = parse_query(query);

    let state_ok = params
        .get("state")
        .map(|s| s == expected_state)
        .unwrap_or(false);
    if !state_ok {
        let _ = write_http_response(
            &mut writer_stream,
            400,
            "State mismatch — ignored. Still waiting for a valid callback.",
        );
        return HandleOutcome::Ignore;
    }

    if let Some(code) = params.get("code") {
        let _ = write_http_response(
            &mut writer_stream,
            200,
            "You can close this tab and return to your terminal.",
        );
        return HandleOutcome::Code(code.clone());
    }

    if let Some(error) = params.get("error") {
        let _ = write_http_response(
            &mut writer_stream,
            200,
            "Authorization was not granted. You can close this tab.",
        );
        return HandleOutcome::Denied(error.clone());
    }

    let _ = write_http_response(&mut writer_stream, 400, "Missing code parameter.");
    HandleOutcome::Ignore
}

fn write_http_response(stream: &mut TcpStream, status: u16, message: &str) -> io::Result<()> {
    let status_text = if status == 200 { "OK" } else { "Bad Request" };
    let html = format!("<html><body><p>{message}</p></body></html>");
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

// --- HTTP client (injectable — the one seam that ever touches the network) ---

// Defined in `crate::http`; re-exported for `sync.rs`.
pub use crate::http::{HttpError, HttpResponse};
use crate::http::{build_agent, map_ureq_result};

/// The MAL HTTP surface: `post_form` for the OAuth token endpoint, `get` and
/// `patch_form` for the authenticated data API.
pub trait MalHttpClient {
    fn post_form(&self, url: &str, params: &[(&str, &str)]) -> Result<HttpResponse, HttpError>;

    /// An authenticated `GET` with `query` as query-string parameters.
    fn get(
        &self,
        url: &str,
        bearer_token: &str,
        query: &[(&str, &str)],
    ) -> Result<HttpResponse, HttpError>;

    /// An authenticated `PATCH` with a form-encoded body.
    fn patch_form(
        &self,
        url: &str,
        bearer_token: &str,
        params: &[(&str, &str)],
    ) -> Result<HttpResponse, HttpError>;
}

/// The real client: blocking `ureq` through `crate::http::build_agent`. The
/// bare `ureq::post`/`ureq::get` shortcuts must not be used; see `crate::http`.
pub struct UreqClient;

impl MalHttpClient for UreqClient {
    fn post_form(&self, url: &str, params: &[(&str, &str)]) -> Result<HttpResponse, HttpError> {
        let agent = build_agent()?;
        map_ureq_result(agent.post(url).send_form(params))
    }

    fn get(
        &self,
        url: &str,
        bearer_token: &str,
        query: &[(&str, &str)],
    ) -> Result<HttpResponse, HttpError> {
        let agent = build_agent()?;
        let mut req = agent
            .get(url)
            .set("Authorization", &format!("Bearer {bearer_token}"));
        for (key, value) in query {
            req = req.query(key, value);
        }
        map_ureq_result(req.call())
    }

    fn patch_form(
        &self,
        url: &str,
        bearer_token: &str,
        params: &[(&str, &str)],
    ) -> Result<HttpResponse, HttpError> {
        let agent = build_agent()?;
        let req = agent
            .patch(url)
            .set("Authorization", &format!("Bearer {bearer_token}"));
        map_ureq_result(req.send_form(params))
    }
}

// --- Token exchange / refresh ---

#[derive(Debug)]
pub enum TokenExchangeError {
    /// MAL was unreachable (DNS, connection, TLS).
    Transport(String),
    /// MAL answered non-2xx, e.g. an invalid or expired `refresh_token`.
    Rejected(String),
    /// MAL answered 2xx with an unexpected body.
    Parse(String),
}

impl fmt::Display for TokenExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenExchangeError::Transport(m) => {
                write!(f, "network error contacting MyAnimeList: {m}")
            }
            TokenExchangeError::Rejected(m) => write!(f, "MyAnimeList rejected the request: {m}"),
            TokenExchangeError::Parse(m) => {
                write!(f, "could not parse MyAnimeList's response: {m}")
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

fn parse_token_response(resp: &HttpResponse, now: u64) -> Result<TokenRecord, TokenExchangeError> {
    if !(200..300).contains(&resp.status) {
        return Err(TokenExchangeError::Rejected(format!(
            "HTTP {}: {}",
            resp.status, resp.body
        )));
    }
    let parsed: TokenResponse =
        serde_json::from_str(&resp.body).map_err(|e| TokenExchangeError::Parse(e.to_string()))?;
    Ok(TokenRecord {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_at: now + parsed.expires_in,
    })
}

/// Exchanges an authorization `code` for a token pair. `port` must match the
/// redirect URI the code was issued for.
pub fn exchange_code(
    client: &dyn MalHttpClient,
    client_id: &str,
    code: &str,
    code_verifier: &str,
    port: u16,
    now: u64,
) -> Result<TokenRecord, TokenExchangeError> {
    let redirect = redirect_uri(port);
    let params = [
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", code_verifier),
        ("redirect_uri", redirect.as_str()),
    ];
    let resp = client
        .post_form(TOKEN_URL, &params)
        .map_err(|e| TokenExchangeError::Transport(e.to_string()))?;
    parse_token_response(&resp, now)
}

/// Gets a new token pair from `refresh_token_value`.
pub fn refresh_with_token(
    client: &dyn MalHttpClient,
    client_id: &str,
    refresh_token_value: &str,
    now: u64,
) -> Result<TokenRecord, TokenExchangeError> {
    let params = [
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token_value),
    ];
    let resp = client
        .post_form(TOKEN_URL, &params)
        .map_err(|e| TokenExchangeError::Transport(e.to_string()))?;
    parse_token_response(&resp, now)
}

// --- Token status (feeds doctor's MAL-auth check) ---

/// The MAL-auth check result, mapped to a verdict by `doctor::check_mal_auth`.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenStatus {
    /// The `client_id` was empty. Defensive: `resolve_client_id` never
    /// returns an empty value.
    NotConfigured,
    /// No `mal_token.toml`, or an unreadable one; `sync login` fixes both.
    NotLoggedIn,
    Valid {
        expires_in_secs: u64,
    },
    Refreshed {
        expires_in_secs: u64,
    },
    /// MAL rejected the refresh; the stored `refresh_token` is dead.
    RefreshRejected(String),
    /// Transport problem (offline, DNS); says nothing about the token.
    NetworkError(String),
}

/// Resolves the MAL-auth status, refreshing and persisting an expiring
/// token. Touches the network only when a token exists and is within
/// `EXPIRY_SAFETY_MARGIN_SECS` of expiring. `client_id` must already be
/// resolved by the caller.
pub fn token_status(
    client: &dyn MalHttpClient,
    token_path: &Path,
    client_id: &str,
    now: u64,
) -> TokenStatus {
    if client_id.is_empty() {
        return TokenStatus::NotConfigured;
    }
    match ensure_token(client, token_path, client_id, now) {
        EnsureOutcome::Valid(t) => TokenStatus::Valid {
            expires_in_secs: t.expires_at - now,
        },
        EnsureOutcome::Refreshed(t) => TokenStatus::Refreshed {
            expires_in_secs: t.expires_at.saturating_sub(now),
        },
        EnsureOutcome::NotLoggedIn => TokenStatus::NotLoggedIn,
        EnsureOutcome::RefreshRejected(msg) => TokenStatus::RefreshRejected(msg),
        EnsureOutcome::NetworkError(msg) => TokenStatus::NetworkError(msg),
    }
}

/// Outcome of `ensure_token`, the single refresh-if-expiring path shared by
/// `token_status` and `ensure_access_token`.
enum EnsureOutcome {
    Valid(TokenRecord),
    Refreshed(TokenRecord),
    NotLoggedIn,
    RefreshRejected(String),
    NetworkError(String),
}

fn ensure_token(
    client: &dyn MalHttpClient,
    token_path: &Path,
    client_id: &str,
    now: u64,
) -> EnsureOutcome {
    let token = match read_token(token_path) {
        Ok(Some(t)) => t,
        Ok(None) => return EnsureOutcome::NotLoggedIn,
        Err(_) => return EnsureOutcome::NotLoggedIn,
    };

    if token.expires_at > now + EXPIRY_SAFETY_MARGIN_SECS {
        return EnsureOutcome::Valid(token);
    }

    match refresh_with_token(client, client_id, &token.refresh_token, now) {
        Ok(refreshed) => {
            let _ = write_token(token_path, &refreshed);
            EnsureOutcome::Refreshed(refreshed)
        }
        Err(TokenExchangeError::Rejected(msg)) => EnsureOutcome::RefreshRejected(msg),
        Err(TokenExchangeError::Transport(msg)) => EnsureOutcome::NetworkError(msg),
        Err(TokenExchangeError::Parse(msg)) => EnsureOutcome::NetworkError(msg),
    }
}

/// Wraps a client so a `401` from the data API (MAL's `expires_in` is not
/// trustworthy, so the stored expiry can claim a dead token is valid)
/// refreshes the token once and retries the call. Later calls use the
/// refreshed token; a failed refresh is not retried.
pub struct RefreshOn401Client<'a> {
    inner: &'a dyn MalHttpClient,
    token_path: PathBuf,
    client_id: String,
    state: std::cell::RefCell<RefreshState>,
}

enum RefreshState {
    Untried,
    Refreshed(String),
    Failed,
}

impl<'a> RefreshOn401Client<'a> {
    pub fn new(
        inner: &'a dyn MalHttpClient,
        token_path: PathBuf,
        client_id: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            token_path,
            client_id: client_id.into(),
            state: std::cell::RefCell::new(RefreshState::Untried),
        }
    }

    /// Whether MAL answered 401 and the stored refresh token could not fix it.
    pub fn refresh_failed(&self) -> bool {
        matches!(*self.state.borrow(), RefreshState::Failed)
    }

    fn refresh(&self) -> Option<String> {
        let stored = read_token(&self.token_path).ok().flatten()?;
        let refreshed = refresh_with_token(
            self.inner,
            &self.client_id,
            &stored.refresh_token,
            crate::state::now_unix(),
        )
        .ok()?;
        let _ = write_token(&self.token_path, &refreshed);
        Some(refreshed.access_token)
    }

    fn authorized(
        &self,
        bearer_token: &str,
        call: &dyn Fn(&str) -> Result<HttpResponse, HttpError>,
    ) -> Result<HttpResponse, HttpError> {
        let current = match &*self.state.borrow() {
            RefreshState::Refreshed(token) => token.clone(),
            _ => bearer_token.to_string(),
        };
        let resp = call(&current)?;
        if resp.status != 401 || !matches!(*self.state.borrow(), RefreshState::Untried) {
            return Ok(resp);
        }
        match self.refresh() {
            Some(token) => {
                let retried = call(&token);
                *self.state.borrow_mut() = RefreshState::Refreshed(token);
                retried
            }
            None => {
                *self.state.borrow_mut() = RefreshState::Failed;
                Ok(resp)
            }
        }
    }
}

impl MalHttpClient for RefreshOn401Client<'_> {
    fn post_form(&self, url: &str, params: &[(&str, &str)]) -> Result<HttpResponse, HttpError> {
        self.inner.post_form(url, params)
    }

    fn get(
        &self,
        url: &str,
        bearer_token: &str,
        query: &[(&str, &str)],
    ) -> Result<HttpResponse, HttpError> {
        self.authorized(bearer_token, &|t| self.inner.get(url, t, query))
    }

    fn patch_form(
        &self,
        url: &str,
        bearer_token: &str,
        params: &[(&str, &str)],
    ) -> Result<HttpResponse, HttpError> {
        self.authorized(bearer_token, &|t| self.inner.patch_form(url, t, params))
    }
}

/// Why no usable access token could be produced; fatal to a whole sync run.
#[derive(Debug)]
pub enum AccessTokenError {
    NotLoggedIn,
    RefreshRejected(String),
    NetworkError(String),
}

impl fmt::Display for AccessTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccessTokenError::NotLoggedIn => {
                write!(
                    f,
                    "not logged in to MyAnimeList — run `tsuzuki sync login` first"
                )
            }
            AccessTokenError::RefreshRejected(msg) => write!(
                f,
                "MyAnimeList rejected the stored refresh token — run `tsuzuki sync login` again ({msg})"
            ),
            AccessTokenError::NetworkError(msg) => {
                write!(f, "network error contacting MyAnimeList: {msg}")
            }
        }
    }
}

/// A currently-valid access token, refreshing and persisting first if the
/// stored one is expiring.
pub fn ensure_access_token(
    client: &dyn MalHttpClient,
    token_path: &Path,
    client_id: &str,
    now: u64,
) -> Result<String, AccessTokenError> {
    if client_id.is_empty() {
        return Err(AccessTokenError::NotLoggedIn);
    }
    match ensure_token(client, token_path, client_id, now) {
        EnsureOutcome::Valid(t) | EnsureOutcome::Refreshed(t) => Ok(t.access_token),
        EnsureOutcome::NotLoggedIn => Err(AccessTokenError::NotLoggedIn),
        EnsureOutcome::RefreshRejected(msg) => Err(AccessTokenError::RefreshRejected(msg)),
        EnsureOutcome::NetworkError(msg) => Err(AccessTokenError::NetworkError(msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{SocketAddr, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    // --- PKCE generation (RFC 7636 charset/length) ---

    #[test]
    fn pkce_verifier_is_64_chars_within_rfc7636_bounds() {
        let pkce = generate_pkce();
        assert_eq!(pkce.code_verifier.len(), 64);
        assert!((43..=128).contains(&pkce.code_verifier.len()));
    }

    #[test]
    fn pkce_verifier_uses_only_unreserved_characters() {
        let pkce = generate_pkce();
        assert!(
            pkce.code_verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')),
            "verifier contains a character outside RFC 7636's unreserved set: {}",
            pkce.code_verifier
        );
    }

    #[test]
    fn pkce_challenge_equals_verifier_under_plain_method() {
        let pkce = generate_pkce();
        assert_eq!(pkce.code_challenge, pkce.code_verifier);
    }

    #[test]
    fn successive_pkce_pairs_and_states_are_not_identical() {
        let a = generate_pkce();
        let b = generate_pkce();
        assert_ne!(a.code_verifier, b.code_verifier);
        assert_ne!(generate_state(), generate_state());
    }

    #[test]
    fn state_value_is_32_alphanumeric_chars() {
        let state = generate_state();
        assert_eq!(state.len(), 32);
        assert!(state.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    // --- Authorize URL (item 5: sanity-checkable before any real login) ---

    #[test]
    fn authorize_url_contains_the_expected_query_parameters() {
        let url = authorize_url(
            "test-client-id-not-real",
            8734,
            "test-challenge-value",
            "test-state-value",
        );
        assert!(url.starts_with(AUTHORIZE_URL));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=test-client-id-not-real"));
        assert!(url.contains("code_challenge=test-challenge-value"));
        assert!(url.contains("code_challenge_method=plain"));
        assert!(url.contains("state=test-state-value"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A8734%2Fcallback"));
    }

    /// A default `Config` yields an authorize URL with `DEFAULT_CLIENT_ID`
    /// and the registered redirect URL.
    #[test]
    fn print_real_default_authorize_url_from_zero_config() {
        let config = Config::default();
        let client_id = resolve_client_id(&config);
        let pkce = generate_pkce();
        let state = generate_state();
        let url = authorize_url(
            client_id,
            config.mal.redirect_port,
            &pkce.code_challenge,
            &state,
        );
        println!("real, zero-config default authorize URL:\n{url}");

        assert!(url.contains("client_id=1c0e2546c3b0f9e8cfd215ba17fd0b15"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A8734%2Fcallback"));
    }

    #[test]
    fn percent_encode_round_trips_through_percent_decode() {
        let input = "http://127.0.0.1:8734/callback";
        assert_eq!(percent_decode(&percent_encode(input)), input);
    }

    // --- Local callback listener: state-mismatch rejection (CSRF guard) ---

    fn send_raw_callback(addr: SocketAddr, query: &str) {
        if let Ok(mut stream) = TcpStream::connect(addr) {
            let _ = stream.write_all(
                format!("GET /callback?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes(),
            );
            // Drain the response so the connection closes cleanly.
            let mut buf = [0u8; 512];
            use std::io::Read;
            let _ = stream.read(&mut buf);
        }
    }

    #[test]
    fn state_mismatch_is_rejected_and_listener_keeps_waiting() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_state = "correct-state-value";

        let result: Arc<Mutex<Option<Result<CallbackOutcome, ListenError>>>> =
            Arc::new(Mutex::new(None));
        let result_clone = Arc::clone(&result);
        let expected_state_owned = expected_state.to_string();
        let handle = thread::spawn(move || {
            let outcome =
                listen_for_callback(&listener, &expected_state_owned, Duration::from_secs(5));
            *result_clone.lock().unwrap() = Some(outcome);
        });

        // Wrong state first: if it were accepted, the outcome below would
        // carry its code instead of the real one.
        send_raw_callback(addr, "code=irrelevant&state=wrong-state-value");

        // Correct state: now it should resolve, with the real code.
        send_raw_callback(addr, "code=real-code-value&state=correct-state-value");
        handle.join().unwrap();

        let outcome = result.lock().unwrap().take().unwrap();
        assert_eq!(
            outcome.unwrap(),
            CallbackOutcome::Code("real-code-value".to_string())
        );
    }

    #[test]
    fn denied_callback_stops_listening_immediately_with_the_error_reason() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = thread::spawn(move || {
            listen_for_callback(&listener, "expected-state", Duration::from_secs(5))
        });

        send_raw_callback(addr, "error=access_denied&state=expected-state");
        let outcome = handle.join().unwrap().unwrap();
        assert_eq!(
            outcome,
            CallbackOutcome::Denied("access_denied".to_string())
        );
    }

    #[test]
    fn listener_times_out_with_no_callback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let start = Instant::now();
        let outcome = listen_for_callback(&listener, "state", Duration::from_millis(200));
        assert!(matches!(outcome, Err(ListenError::Timeout)));
        assert!(start.elapsed() >= Duration::from_millis(200));
    }

    // --- Token storage: atomic write, 0600 permissions ---

    #[test]
    fn write_then_read_token_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        let token = TokenRecord {
            access_token: "fake-access-token-not-real".to_string(),
            refresh_token: "fake-refresh-token-not-real".to_string(),
            expires_at: 1_700_003_600,
        };
        write_token(&path, &token).unwrap();
        let reread = read_token(&path).unwrap().unwrap();
        assert_eq!(reread, token);
    }

    #[test]
    fn read_missing_token_file_returns_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        assert_eq!(read_token(&path).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn write_token_sets_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        write_token(
            &path,
            &TokenRecord {
                access_token: "fake-access-token-not-real".to_string(),
                refresh_token: "fake-refresh-token-not-real".to_string(),
                expires_at: 0,
            },
        )
        .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected owner-only read/write, got {mode:o}");
    }

    #[test]
    fn write_leaves_no_stray_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        write_token(
            &path,
            &TokenRecord {
                access_token: "a".to_string(),
                refresh_token: "b".to_string(),
                expires_at: 1,
            },
        )
        .unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn write_replaces_via_rename_not_in_place_mutation() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        let make = |suffix: &str| TokenRecord {
            access_token: format!("a-{suffix}"),
            refresh_token: format!("r-{suffix}"),
            expires_at: 1,
        };
        write_token(&path, &make("first")).unwrap();
        let inode_before = std::fs::metadata(&path).unwrap().ino();
        write_token(&path, &make("second")).unwrap();
        let inode_after = std::fs::metadata(&path).unwrap().ino();
        assert_ne!(inode_before, inode_after);
    }

    // --- Token-expiry-triggers-refresh logic (mockable HTTP layer) ---

    struct FakeClient {
        response: Result<HttpResponse, String>,
    }

    impl MalHttpClient for FakeClient {
        fn post_form(
            &self,
            _url: &str,
            _params: &[(&str, &str)],
        ) -> Result<HttpResponse, HttpError> {
            match &self.response {
                Ok(HttpResponse { status, body }) => Ok(HttpResponse {
                    status: *status,
                    body: body.clone(),
                }),
                Err(msg) => Err(HttpError(msg.clone())),
            }
        }

        // Tests here only use `post_form`; the data-API methods panic so
        // an unexpected call fails loudly.
        fn get(
            &self,
            _url: &str,
            _bearer_token: &str,
            _query: &[(&str, &str)],
        ) -> Result<HttpResponse, HttpError> {
            unimplemented!(
                "FakeClient (mal.rs tests) does not support get — see sync.rs's own fakes"
            )
        }

        fn patch_form(
            &self,
            _url: &str,
            _bearer_token: &str,
            _params: &[(&str, &str)],
        ) -> Result<HttpResponse, HttpError> {
            unimplemented!(
                "FakeClient (mal.rs tests) does not support patch_form — see sync.rs's own fakes"
            )
        }
    }

    fn fake_success_body(access: &str, refresh: &str, expires_in: u64) -> String {
        format!(
            r#"{{"token_type":"Bearer","expires_in":{expires_in},"access_token":"{access}","refresh_token":"{refresh}"}}"#
        )
    }

    // --- client_id resolution: built-in default vs. configured override ---

    #[test]
    fn resolve_client_id_uses_the_built_in_default_when_config_is_empty() {
        let config = Config::default();
        assert_eq!(
            config.mal.client_id, "",
            "sanity: default config has no override"
        );
        assert_eq!(resolve_client_id(&config), DEFAULT_CLIENT_ID);
    }

    #[test]
    fn resolve_client_id_uses_the_configured_override_when_present() {
        let mut config = Config::default();
        config.mal.client_id = "users-own-registered-client-id".to_string();
        assert_eq!(resolve_client_id(&config), "users-own-registered-client-id");
    }

    #[test]
    fn not_configured_when_client_id_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        let client = FakeClient {
            response: Ok(HttpResponse {
                status: 200,
                body: String::new(),
            }),
        };
        let status = token_status(&client, &path, "", 1_000);
        assert_eq!(status, TokenStatus::NotConfigured);
    }

    #[test]
    fn not_logged_in_when_no_token_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        let client = FakeClient {
            response: Ok(HttpResponse {
                status: 200,
                body: String::new(),
            }),
        };
        let status = token_status(&client, &path, "some-client-id", 1_000);
        assert_eq!(status, TokenStatus::NotLoggedIn);
    }

    #[test]
    fn valid_unexpired_token_needs_no_refresh_and_makes_no_network_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        let now = 1_000;
        write_token(
            &path,
            &TokenRecord {
                access_token: "still-good".to_string(),
                refresh_token: "unused".to_string(),
                expires_at: now + EXPIRY_SAFETY_MARGIN_SECS + 500,
            },
        )
        .unwrap();
        // A client that errors if ever called — proves no network call
        // happens for a token that isn't near expiry.
        let client = FakeClient {
            response: Err("must not be called".to_string()),
        };
        let status = token_status(&client, &path, "client-id", now);
        assert_eq!(
            status,
            TokenStatus::Valid {
                expires_in_secs: EXPIRY_SAFETY_MARGIN_SECS + 500
            }
        );
    }

    #[test]
    fn expiring_token_triggers_refresh_and_persists_the_new_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        let now = 1_000;
        write_token(
            &path,
            &TokenRecord {
                access_token: "about-to-expire".to_string(),
                refresh_token: "old-refresh".to_string(),
                expires_at: now + 10, // within the 60s safety margin
            },
        )
        .unwrap();
        let client = FakeClient {
            response: Ok(HttpResponse {
                status: 200,
                body: fake_success_body("new-access", "new-refresh", 3600),
            }),
        };
        let status = token_status(&client, &path, "client-id", now);
        assert_eq!(
            status,
            TokenStatus::Refreshed {
                expires_in_secs: 3600
            }
        );
        let persisted = read_token(&path).unwrap().unwrap();
        assert_eq!(persisted.access_token, "new-access");
        assert_eq!(persisted.refresh_token, "new-refresh");
        assert_eq!(persisted.expires_at, now + 3600);
    }

    #[test]
    fn rejected_refresh_is_distinguished_from_a_network_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        let now = 1_000;
        write_token(
            &path,
            &TokenRecord {
                access_token: "expired".to_string(),
                refresh_token: "dead-refresh-token".to_string(),
                expires_at: now,
            },
        )
        .unwrap();
        let client = FakeClient {
            response: Ok(HttpResponse {
                status: 400,
                body: r#"{"error":"invalid_grant"}"#.to_string(),
            }),
        };
        let status = token_status(&client, &path, "client-id", now);
        assert!(matches!(status, TokenStatus::RefreshRejected(_)));

        // The stored (dead) token must be untouched — a rejected refresh
        // never overwrites it, so the exact failure reason stays
        // reproducible and `sync login` remains the one way out.
        let persisted = read_token(&path).unwrap().unwrap();
        assert_eq!(persisted.refresh_token, "dead-refresh-token");
    }

    #[test]
    fn transport_failure_is_a_network_error_not_a_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mal_token.toml");
        let now = 1_000;
        write_token(
            &path,
            &TokenRecord {
                access_token: "expired".to_string(),
                refresh_token: "some-refresh-token".to_string(),
                expires_at: now,
            },
        )
        .unwrap();
        let client = FakeClient {
            response: Err("connection refused".to_string()),
        };
        let status = token_status(&client, &path, "client-id", now);
        assert!(matches!(status, TokenStatus::NetworkError(_)));
    }

    // --- 401 recovery: the clock says valid, MAL says expired ---

    /// Data API accepts only `valid_bearer`; the token endpoint answers
    /// `refresh_response`. Records every bearer seen and the post count.
    struct StaleTokenClient {
        valid_bearer: &'static str,
        refresh_response: Result<HttpResponse, String>,
        bearers_seen: std::cell::RefCell<Vec<String>>,
        posts: std::cell::Cell<u32>,
    }

    impl StaleTokenClient {
        fn new(valid_bearer: &'static str, refresh_response: Result<HttpResponse, String>) -> Self {
            Self {
                valid_bearer,
                refresh_response,
                bearers_seen: Default::default(),
                posts: Default::default(),
            }
        }

        fn data_response(&self, bearer: &str) -> Result<HttpResponse, HttpError> {
            self.bearers_seen.borrow_mut().push(bearer.to_string());
            if bearer == self.valid_bearer {
                Ok(HttpResponse {
                    status: 200,
                    body: "{}".to_string(),
                })
            } else {
                Ok(HttpResponse {
                    status: 401,
                    body: r#"{"error":"invalid_token"}"#.to_string(),
                })
            }
        }
    }

    impl MalHttpClient for StaleTokenClient {
        fn post_form(
            &self,
            _url: &str,
            _params: &[(&str, &str)],
        ) -> Result<HttpResponse, HttpError> {
            self.posts.set(self.posts.get() + 1);
            match &self.refresh_response {
                Ok(r) => Ok(HttpResponse {
                    status: r.status,
                    body: r.body.clone(),
                }),
                Err(m) => Err(HttpError(m.clone())),
            }
        }

        fn get(
            &self,
            _url: &str,
            bearer_token: &str,
            _query: &[(&str, &str)],
        ) -> Result<HttpResponse, HttpError> {
            self.data_response(bearer_token)
        }

        fn patch_form(
            &self,
            _url: &str,
            bearer_token: &str,
            _params: &[(&str, &str)],
        ) -> Result<HttpResponse, HttpError> {
            self.data_response(bearer_token)
        }
    }

    fn stale_token_file(dir: &Path) -> PathBuf {
        let path = dir.join("mal_token.toml");
        write_token(
            &path,
            &TokenRecord {
                access_token: "stale".to_string(),
                refresh_token: "old-refresh".to_string(),
                expires_at: u64::MAX / 2, // the clock claims it is valid
            },
        )
        .unwrap();
        path
    }

    fn refresh_ok() -> Result<HttpResponse, String> {
        Ok(HttpResponse {
            status: 200,
            body: fake_success_body("fresh", "new-refresh", 3600),
        })
    }

    #[test]
    fn data_call_with_stale_token_refreshes_retries_and_persists_the_new_pair() {
        let dir = tempfile::tempdir().unwrap();
        let path = stale_token_file(dir.path());
        let inner = StaleTokenClient::new("fresh", refresh_ok());
        let client = RefreshOn401Client::new(&inner, path.clone(), "client-id");

        let resp = client.get("https://x/list", "stale", &[]).unwrap();

        assert_eq!(resp.status, 200);
        assert_eq!(*inner.bearers_seen.borrow(), vec!["stale", "fresh"]);
        assert_eq!(inner.posts.get(), 1);
        let persisted = read_token(&path).unwrap().unwrap();
        assert_eq!(persisted.access_token, "fresh");
        assert_eq!(persisted.refresh_token, "new-refresh");
        assert!(!client.refresh_failed());
    }

    #[test]
    fn later_calls_reuse_the_refreshed_token_without_another_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = stale_token_file(dir.path());
        let inner = StaleTokenClient::new("fresh", refresh_ok());
        let client = RefreshOn401Client::new(&inner, path, "client-id");

        client.get("https://x/a", "stale", &[]).unwrap();
        let resp = client.patch_form("https://x/b", "stale", &[]).unwrap();

        assert_eq!(resp.status, 200);
        assert_eq!(
            *inner.bearers_seen.borrow(),
            vec!["stale", "fresh", "fresh"]
        );
        assert_eq!(inner.posts.get(), 1);
    }

    #[test]
    fn rejected_refresh_returns_the_401_and_leaves_the_stored_token_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = stale_token_file(dir.path());
        let inner = StaleTokenClient::new(
            "fresh",
            Ok(HttpResponse {
                status: 400,
                body: r#"{"error":"invalid_grant"}"#.to_string(),
            }),
        );
        let client = RefreshOn401Client::new(&inner, path.clone(), "client-id");

        let first = client.get("https://x/a", "stale", &[]).unwrap();
        let second = client.get("https://x/b", "stale", &[]).unwrap();

        assert_eq!((first.status, second.status), (401, 401));
        assert!(client.refresh_failed());
        assert_eq!(inner.posts.get(), 1, "a dead refresh token is tried once");
        assert_eq!(read_token(&path).unwrap().unwrap().access_token, "stale");
    }

    #[test]
    fn still_unauthorized_after_a_refresh_is_returned_without_a_second_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = stale_token_file(dir.path());
        let inner = StaleTokenClient::new("never-issued", refresh_ok());
        let client = RefreshOn401Client::new(&inner, path, "client-id");

        let resp = client.get("https://x/a", "stale", &[]).unwrap();

        assert_eq!(resp.status, 401);
        assert_eq!(inner.posts.get(), 1);
        assert_eq!(*inner.bearers_seen.borrow(), vec!["stale", "fresh"]);
    }

    #[test]
    fn non_401_failures_never_trigger_a_refresh() {
        struct Forbidden;
        impl MalHttpClient for Forbidden {
            fn post_form(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse, HttpError> {
                panic!("must not refresh on 403");
            }
            fn get(&self, _: &str, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse, HttpError> {
                Ok(HttpResponse {
                    status: 403,
                    body: "forbidden".to_string(),
                })
            }
            fn patch_form(
                &self,
                _: &str,
                _: &str,
                _: &[(&str, &str)],
            ) -> Result<HttpResponse, HttpError> {
                unreachable!()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = stale_token_file(dir.path());
        let client = RefreshOn401Client::new(&Forbidden, path, "client-id");

        let resp = client.get("https://x/a", "stale", &[]).unwrap();

        assert_eq!(resp.status, 403);
    }

    #[test]
    fn token_debug_output_never_contains_the_raw_secret_values() {
        let token = TokenRecord {
            access_token: "super-secret-access-token".to_string(),
            refresh_token: "super-secret-refresh-token".to_string(),
            expires_at: 1,
        };
        let debug = format!("{token:?}");
        assert!(!debug.contains("super-secret-access-token"));
        assert!(!debug.contains("super-secret-refresh-token"));
        assert!(debug.contains("<redacted>"));
    }

    // --- Real network smoke test: UreqClient's TLS backend is actually wired up ---

    /// The real client's TLS backend works: garbage credentials still get an
    /// HTTP rejection from MAL rather than a transport error. The fake
    /// client used elsewhere can't catch this. Harmless, but a live call.
    #[test]
    #[ignore = "makes a real (but harmless — garbage credentials, no login) network call to MyAnimeList"]
    fn real_ureq_client_actually_reaches_mal_over_https() {
        let client = UreqClient;
        let result = client.post_form(
            TOKEN_URL,
            &[
                ("client_id", "not-a-real-client-id"),
                ("grant_type", "authorization_code"),
                ("code", "not-a-real-code"),
                ("code_verifier", "not-a-real-verifier"),
                ("redirect_uri", &redirect_uri(8734)),
            ],
        );
        match result {
            Ok(resp) => {
                println!("real MAL response: HTTP {} — {}", resp.status, resp.body);
                assert!(
                    resp.status >= 400,
                    "expected MAL to reject garbage credentials, got HTTP {}",
                    resp.status
                );
            }
            Err(e) => panic!(
                "expected a real HTTP response (even a rejection) from MAL, got a \
                 transport-level failure instead — the TLS backend may be broken again: {e}"
            ),
        }
    }

    /// `DEFAULT_CLIENT_ID` authenticates without a secret. MAL checks the
    /// client before the code, so a fake code must fail on the code
    /// (`invalid_request`/`invalid_grant`), never `invalid_client`.
    #[test]
    #[ignore = "makes a real (but harmless — fake code, no login) network call to MyAnimeList"]
    fn default_client_id_authenticates_without_a_client_secret() {
        let client = UreqClient;
        let result = client
            .post_form(
                TOKEN_URL,
                &[
                    ("client_id", DEFAULT_CLIENT_ID),
                    ("grant_type", "authorization_code"),
                    ("code", "deliberately-fake-code-value"),
                    (
                        "code_verifier",
                        "deliberately-fake-verifier-value-1234567890123",
                    ),
                    ("redirect_uri", &redirect_uri(8734)),
                ],
            )
            .expect("expected a real HTTP response from MAL, not a transport failure");

        println!(
            "real MAL response: HTTP {} — {}",
            result.status, result.body
        );
        assert!(
            !result.body.contains("invalid_client"),
            "DEFAULT_CLIENT_ID was rejected at client authentication (invalid_client) — it \
             likely has a client_secret assigned that this PKCE-only flow doesn't send. Got: {}",
            result.body
        );
    }
}
