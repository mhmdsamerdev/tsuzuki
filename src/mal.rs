//! MyAnimeList OAuth login, token storage/refresh, and the pieces
//! `doctor`'s MAL-auth check needs (design.md §7, CLAUDE.md's "Scope
//! boundary: episode progress only").
//!
//! Authorization, token exchange/refresh, `~/.local/share/tsuzuki/mal_token.toml`
//! persistence, and the small authenticated-HTTP surface (`get`/`patch_form`
//! on [`MalHttpClient`], [`ensure_access_token`]) that Phase 8b's `sync.rs`
//! builds on. Linking, matching, and the two-way episode-progress merge
//! themselves live in `sync.rs`, not here — this module never fetches a
//! user's MAL list or pushes/pulls episode numbers on its own.
//!
//! **Never print or log a full token/secret value anywhere.** `TokenRecord`
//! hand-writes its own `Debug` to redact both token fields even under an
//! accidental `{:?}`; every caller-facing status type in this module
//! (`TokenStatus`) carries only booleans/expiry seconds/opaque error
//! strings, never a raw credential.
//!
//! HTTP access goes through the injectable [`MalHttpClient`] trait — the
//! real [`UreqClient`] is the only thing in this module that ever touches
//! the network, so every other function here (PKCE, URL building, the local
//! callback listener, token-status logic) is testable against a fabricated
//! client with zero real network calls, matching this project's existing
//! `launch.rs`/`doctor.rs` injectable-closure convention.

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

/// Tsuzuki's own registered MAL client_id, committed here in the clear —
/// deliberately, not an oversight. MyAnimeList's PKCE/no-secret app type
/// (what this whole module implements — no `client_secret` field exists or
/// is ever sent) does not treat `client_id` as confidential; only
/// `client_secret` is, and Tsuzuki never has one to leak. Precedent: Taiga
/// (erengy/taiga, a long-running, widely-used open-source MAL client)
/// commits its own `client_id` as a plain constant in public source
/// (`src/sync/myanimelist.h`) the same way, with no secret alongside it —
/// this is the established, safe pattern for this app type, not something
/// unique to Tsuzuki. **Do not "helpfully" remove or redact this as a
/// leaked credential** — a future session flagging it without this context
/// would be a regression, not a fix; see CLAUDE.md's MAL-sync section for
/// the full reasoning. `[mal] client_id` in config.toml remains a genuine
/// override for anyone running their own fork against their own
/// registration (`resolve_client_id`, below) — this default only removes
/// the *requirement* to do that before `sync login` works at all.
///
/// **This is the second value used here, not the first — the swap matters.**
/// The original app was registered under MAL's "web" app type, which turned
/// out to issue a real `client_secret` MAL's token endpoint then requires;
/// every real token exchange failed with `invalid_client` regardless of
/// auth scheme tried (body param, HTTP Basic with an empty password, an
/// explicit empty `client_secret=`), confirmed live via a direct
/// `curl … -d client_id=… -d code=fake` request that failed at client
/// authentication itself, before the (deliberately fake) code was ever
/// evaluated. The app was recreated and this value verified the same way —
/// the same fake-code `curl` request against *this* client_id fails one
/// step later, at `"Cannot decrypt the authorization code"` (`invalid_request`,
/// not `invalid_client`), proving client authentication succeeds without any
/// secret. If this value is ever rotated again, verify it the same
/// zero-browser, zero-account-risk way before trusting it: a fake-code POST
/// to `TOKEN_URL` must fail with `invalid_request`/`invalid_grant`, never
/// `invalid_client` — `invalid_client` means the new registration also
/// issued a secret this flow doesn't send.
pub(crate) const DEFAULT_CLIENT_ID: &str = "1c0e2546c3b0f9e8cfd215ba17fd0b15";

/// Resolves the `client_id` to actually use: the user's own `[mal]
/// client_id` override when set, `DEFAULT_CLIENT_ID` otherwise. The single
/// seam every caller (`sync_login_command`, `doctor_command`) goes through —
/// nothing else in this crate reads `config.mal.client_id` directly, so
/// there's exactly one place this decision is made.
pub(crate) fn resolve_client_id(config: &Config) -> &str {
    if config.mal.client_id.is_empty() {
        DEFAULT_CLIENT_ID
    } else {
        &config.mal.client_id
    }
}

/// A stored token's own safety margin before treating it as needing refresh
/// — matches CLAUDE.md's "any MAL API call checks the stored token's expiry
/// first (with a ~60s safety margin)".
const EXPIRY_SAFETY_MARGIN_SECS: u64 = 60;

/// How long `sync login`'s local callback listener waits for MAL to redirect
/// back before giving up.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(120);

// --- Token storage: ~/.local/share/tsuzuki/mal_token.toml ---

/// A stored MAL OAuth token pair. Written atomically (via
/// [`crate::state::atomic_write`]) with `0600` permissions on Unix — this
/// file holds live credentials, unlike `state.toml`/`config.toml`.
///
/// `Debug` is hand-written to redact both token fields — a defensive
/// measure so an accidental `{:?}` anywhere (a stray `dbg!`, a future log
/// line) can never leak a real credential. `Serialize`/`Deserialize` are
/// untouched by this — they're what actually reads/writes the file.
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

/// Resolves `~/.local/share/tsuzuki/mal_token.toml` — the same XDG data
/// directory `state::state_path` uses, via the same `dirs::data_dir()` call.
pub fn mal_token_path() -> io::Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "could not resolve data directory")
    })?;
    Ok(base.join("tsuzuki").join("mal_token.toml"))
}

/// Reads a stored token. A missing file means "never logged in" — `Ok(None)`,
/// not an error, matching `state::read`'s own missing-file-is-not-an-error
/// convention.
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

/// Writes `token` to `path` atomically, `0600` on Unix — reuses
/// `state::atomic_write` rather than a second temp-file-then-rename path
/// (CLAUDE.md: "any new state-writing code path must go through the same
/// atomic-write helper").
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

/// Generates a fresh verifier/challenge pair for one login attempt. 64
/// characters from `[A-Za-z0-9]` — a subset of RFC 7636's unreserved
/// character set (`ALPHA / DIGIT / "-" / "." / "_" / "~"`), well within the
/// spec's 43–128 length bound.
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

/// Builds the URL `tsuzuki sync login` prints and opens — Authorization
/// Code grant with PKCE, `code_challenge_method=plain` (MAL doesn't support
/// S256).
pub fn authorize_url(client_id: &str, port: u16, code_challenge: &str, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={}&code_challenge={}&code_challenge_method=plain&state={}&redirect_uri={}",
        percent_encode(client_id),
        percent_encode(code_challenge),
        percent_encode(state),
        percent_encode(&redirect_uri(port)),
    )
}

/// RFC 3986 percent-encoding for a query-string value — everything outside
/// the unreserved set (`ALPHA / DIGIT / "-" / "." / "_" / "~"`) is escaped.
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

/// Decodes a percent-encoded query-string component. `+` is treated as a
/// literal space (the `application/x-www-form-urlencoded` convention) — the
/// only place this matters here is parsing the browser's own callback
/// request, which query strings normally use `%20` for, but treating `+`
/// this way is harmless and standard-compliant either way.
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
    /// The user declined authorization on MAL's page (`error=...` on the
    /// redirect) — a real, single callback, distinct from a state mismatch,
    /// so it stops listening immediately rather than waiting out the rest
    /// of the timeout for a callback that will never come.
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
    /// State mismatch, malformed request, or neither `code` nor `error`
    /// present — the CSRF guard: reject without accepting, keep listening.
    Ignore,
}

/// Accepts exactly one *valid* callback (matching `state`) on `listener`,
/// then returns — it never lingers listening afterward. Polls via a
/// non-blocking accept loop rather than blocking indefinitely, so the
/// `timeout` bound is enforceable without a second thread.
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
    // Drain the remaining request headers (never needed, but a GET has no
    // body worth reading) up to the blank line that ends them.
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

/// The MAL HTTP surface Tsuzuki needs, end to end: `post_form` for the
/// unauthenticated OAuth token endpoint (client_id-only, both authorization-
/// code exchange and refresh are `application/x-www-form-urlencoded`
/// POSTs — Phase 8a), plus `get`/`patch_form` for the authenticated data API
/// (`sync.rs`, Phase 8b) — fetching the user's anime list, looking up one
/// anime's list status, and updating episode progress/list status. One
/// trait, one real implementation ([`UreqClient`]), so every caller above it (PKCE, URL
/// building, token-status logic, linking, merge decisions) is testable
/// against a fabricated client with zero real network calls.
pub trait MalHttpClient {
    fn post_form(&self, url: &str, params: &[(&str, &str)]) -> Result<HttpResponse, HttpError>;

    /// An authenticated `GET`, `Authorization: Bearer {bearer_token}`, with
    /// `query` appended as query-string parameters. Used for both the
    /// paginated anime-list fetch and a single anime's list status.
    fn get(
        &self,
        url: &str,
        bearer_token: &str,
        query: &[(&str, &str)],
    ) -> Result<HttpResponse, HttpError>;

    /// An authenticated `PATCH` with a form-encoded body — MAL's
    /// `.../my_list_status` update endpoint is the only user of this.
    fn patch_form(
        &self,
        url: &str,
        bearer_token: &str,
        params: &[(&str, &str)],
    ) -> Result<HttpResponse, HttpError>;
}

/// The real client, backed by `ureq` — a blocking-only HTTP client with no
/// async runtime (CLAUDE.md §5: "MAL calls must stay lazy/on-demand", "don't
/// introduce anything at startup that reintroduces meaningful latency" — an
/// async executor pulled in for one on-demand code path would work directly
/// against the reason this project is Rust). This is the only place in the
/// whole binary that ever makes a real network call.
///
/// **Must use an explicit `Agent` with `.tls_connector(...)` set, never the
/// bare `ureq::post(...)`/`ureq::get(...)` shortcut functions.** Found the
/// hard way, against the real MAL API: `ureq`'s `native-tls` feature is
/// never picked up automatically by those shortcuts — per `ureq`'s own
/// source (`lib.rs`'s `default_tls_config`), only the `tls` (rustls)
/// feature wires a default TLS backend into them. With `default-features =
/// false` and only `native-tls` enabled (Cargo.toml, chosen to use the
/// pkg-config+openssl `shell.nix` already provisions), the shortcuts had
/// *no* TLS backend at all — every request failed with "Unknown Scheme:
/// cannot make HTTPS request because no TLS backend is configured". No unit
/// test in this module caught it: every one of them goes through the
/// injectable `MalHttpClient` trait's fake, never this real impl. Building
/// the connector fresh per call (rather than caching it) is deliberate
/// simplicity, not an oversight — this is an on-demand, low-frequency path
/// (a login once, a refresh roughly hourly at most), so the tiny, one-time
/// TLS-connector setup cost here is a complete non-issue.
pub struct UreqClient;

/// Builds a fresh `Agent` with the explicit native-tls connector every
/// `UreqClient` method needs (see the doc comment above) — factored out
/// once `get`/`patch_form` joined `post_form` in needing it, so the setup
/// exists in exactly one place rather than three copies.
fn build_agent() -> Result<ureq::Agent, HttpError> {
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
fn map_ureq_result(result: Result<ureq::Response, ureq::Error>) -> Result<HttpResponse, HttpError> {
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
    /// Could not reach MAL at all (DNS, connection refused, TLS, ...).
    Transport(String),
    /// MAL responded, but rejected the request (non-2xx) — a real,
    /// actionable outcome, e.g. an invalid/expired `refresh_token`.
    Rejected(String),
    /// MAL responded 2xx but the body wasn't the JSON shape expected.
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

/// Exchanges an authorization `code` (from the callback listener) for a
/// fresh token pair. `port` must match the `redirect_uri` actually used to
/// obtain `code` — MAL validates it's identical.
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

/// Refreshes an access token via `refresh_token_value`.
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

/// The MAL-auth check's result, computed once here and handed to
/// `doctor::check_mal_auth` for the pure OK/WARN/FAIL mapping — mirrors
/// `doctor.rs`'s existing convention of "every check is a pure function over
/// already-resolved inputs" (`MpvPresence`, `permissions_output`, ...): the
/// one place that actually reads the token file or calls the network is
/// this function, not `doctor.rs` itself.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenStatus {
    /// `client_id` passed in was empty. Not reachable through the real call
    /// path anymore — `resolve_client_id` always returns a non-empty value
    /// (the built-in `DEFAULT_CLIENT_ID` when no override is configured),
    /// and every real caller resolves through it before calling this
    /// function. Kept as a defensive guard, not a state real users can hit.
    NotConfigured,
    /// `client_id` is set, but no `mal_token.toml` exists (or it's
    /// unreadable/corrupt — treated the same as never-logged-in, since
    /// either way the fix is the same: run `sync login` again, which
    /// overwrites it).
    NotLoggedIn,
    Valid {
        expires_in_secs: u64,
    },
    Refreshed {
        expires_in_secs: u64,
    },
    /// MAL rejected the refresh — the stored `refresh_token` is dead.
    RefreshRejected(String),
    /// Couldn't verify either way — a transport-level problem (offline,
    /// DNS, ...), not evidence the token itself is broken.
    NetworkError(String),
}

/// Resolves the current MAL-auth status, refreshing an expiring token along
/// the way (and persisting the refreshed token) exactly like any other MAL
/// API call would (CLAUDE.md's "any MAL API call checks the stored token's
/// expiry first... and transparently refreshes"). This is a real network
/// call when (and only when) a token exists and is within
/// `EXPIRY_SAFETY_MARGIN_SECS` of expiring — never on every `doctor` run
/// regardless of token freshness, and never at all when no token has ever
/// been stored. `client_id` is expected to already be resolved (via
/// `resolve_client_id`) by the caller — this function's own empty-string
/// guard exists only for direct/defensive callers, not the real path.
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

/// Shared refresh-if-needed outcome — the one place the "is the stored
/// token expiring; if so, refresh and persist" logic lives. Both
/// `token_status` (doctor's OK/WARN/FAIL summary) and `ensure_access_token`
/// (Phase 8b's actual bearer-token accessor, below) resolve through this
/// single function, so a future change to refresh behavior can't drift
/// between the two callers.
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

/// Why a usable access token couldn't be produced — `sync.rs`'s top-level
/// error for "can't even start," distinct from a per-series sync failure.
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

/// Resolves a real, currently-valid access token — refreshing and
/// persisting first if the stored one is expiring, exactly like
/// `token_status` does, via the same shared `ensure_token`. This is what
/// `sync.rs` calls once at the start of a `sync`/`sync link` run to get the
/// bearer token every authenticated list/anime/PATCH request needs.
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

    /// Phase 8a-correction verification item 3: the real authorize URL
    /// through the actual zero-config default resolution path (an untouched
    /// `Config::default()` — no `[mal] client_id` set) must contain the real
    /// `DEFAULT_CLIENT_ID` and the exact registered redirect URL. Run with
    /// `cargo test print_real_default_authorize_url -- --nocapture` to see
    /// it printed.
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

        // Wrong state: must be ignored, not accepted.
        send_raw_callback(addr, "code=irrelevant&state=wrong-state-value");
        thread::sleep(Duration::from_millis(300));
        assert!(
            result.lock().unwrap().is_none(),
            "a state-mismatched callback must not resolve listen_for_callback"
        );

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

        // Every test in this module only ever exercises the token
        // endpoint (`post_form`) — `get`/`patch_form` exist to satisfy the
        // trait, deliberately panicking so a test that starts needing the
        // authenticated data API fails loudly instead of silently getting
        // a wrong/empty response.
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

    /// Regression test for a real bug hit dogfooding `sync login` against
    /// the live MAL API: `ureq`'s bare `ureq::post(...)` shortcut never
    /// picks up the `native-tls` feature (only `AgentBuilder::tls_connector`
    /// does — see `UreqClient`'s doc comment), so every real request failed
    /// with a transport-level "Unknown Scheme: no TLS backend configured"
    /// error before ever reaching MAL. No other test in this module could
    /// catch this class of bug — everything else goes through the
    /// injectable `MalHttpClient` trait's fake, never this real impl.
    ///
    /// Deliberately harmless: garbage credentials, no browser, no login
    /// attempt, no real account action — just proves the TLS handshake
    /// completes and a real HTTP response comes back from MAL (almost
    /// certainly a 4xx rejection of the bogus code/verifier), which is only
    /// possible if the TLS backend actually works. `#[ignore]`d since it's
    /// still a real network call — run with:
    /// `cargo test real_ureq_client_actually_reaches_mal_over_https --
    /// -- --ignored --nocapture`
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

    /// Regression test for the real, live-diagnosed root cause of a second
    /// failure hit dogfooding `sync login`: the *original* `DEFAULT_CLIENT_ID`
    /// was registered as a MAL "web" app, which turned out to carry a real
    /// `client_secret` that MAL's token endpoint requires — every real token
    /// exchange failed with `invalid_client` ("Client authentication
    /// failed"), regardless of auth scheme tried, confirmed live via `curl`
    /// against the real API with a deliberately fake code. This test encodes
    /// that exact zero-account-risk diagnostic permanently: POST a fake
    /// code/verifier to the real token endpoint using `DEFAULT_CLIENT_ID`.
    /// Client authentication happens *before* the code is evaluated, so a
    /// fake code still proves the point either way — if authentication
    /// succeeds, MAL gets as far as trying to decrypt the fake code and
    /// fails there instead (`invalid_request`/`invalid_grant`), never
    /// `invalid_client`. Seeing `invalid_client` here means whatever value
    /// `DEFAULT_CLIENT_ID` currently holds needs a `client_secret` this
    /// PKCE-only flow deliberately doesn't send — see its own doc comment
    /// for how to re-diagnose and replace it the same way.
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
             likely has a client_secret assigned that this PKCE-only flow doesn't send; see \
             DEFAULT_CLIENT_ID's own doc comment for how to re-diagnose and fix this. Got: {}",
            result.body
        );
    }
}
