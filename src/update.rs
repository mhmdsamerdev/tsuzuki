//! `tsuzuki update`'s version check and installer invocation (CLAUDE.md's
//! "doctor's version line is local-only; update alone touches the
//! network" decision).
//!
//! The only network call this module makes is the GitHub "latest release"
//! lookup, and only when [`check_for_update`] is called directly — never
//! implicitly, never from `doctor` or any startup path. This is the sole
//! place in the crate other than `mal.rs` that ever touches the network;
//! both go through `crate::http`'s shared `Agent`/result-mapping plumbing
//! so the native-tls gotcha documented there is only solved once.
//!
//! [`current_version`] also backs `doctor`'s version banner (`main.rs`) —
//! the two must report the exact same compiled-in value by definition, so
//! it's shared rather than each site calling `env!("CARGO_PKG_VERSION")`
//! independently.

use std::fmt;
use std::process::ExitStatus;

use serde::Deserialize;

use crate::http::{HttpResponse, build_agent, map_ureq_result};

/// The compiled-in crate version (`Cargo.toml`'s `version` field). Never
/// itself makes a network call — safe to call from `doctor` as well as
/// `update`.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `owner`/`name` from `Cargo.toml`'s `repository` field, parsed once.
struct GithubRepo {
    owner: String,
    name: String,
}

/// Parses a `github.com` repository URL into its owner/name, tolerating an
/// optional trailing slash and an optional `.git` suffix. Returns `None`
/// for anything that isn't exactly `https://github.com/<owner>/<name>`
/// (a different host, too few/many path segments) — this project's own
/// `Cargo.toml` is the only input this ever sees, but a wrong parse should
/// fail loudly (via `github_repo`'s `.expect()`) rather than silently
/// resolve to a nonsense API URL.
fn parse_repository_url(url: &str) -> Option<GithubRepo> {
    let rest = url.strip_prefix("https://github.com/")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut segments = rest.split('/');
    let owner = segments.next()?;
    let name = segments.next()?;
    if owner.is_empty() || name.is_empty() || segments.next().is_some() {
        return None;
    }
    Some(GithubRepo {
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

/// Parses `env!("CARGO_PKG_REPOSITORY")` — a compile-time constant this
/// project's own `Cargo.toml` controls, not user/runtime input, so a
/// parse failure here means `Cargo.toml` itself is wrong and should fail
/// the build's test suite (see the `github_repo_matches_the_real_cargo_toml`
/// unit test), not degrade gracefully at runtime.
fn github_repo() -> GithubRepo {
    parse_repository_url(env!("CARGO_PKG_REPOSITORY")).expect(
        "Cargo.toml's repository field must be a github.com URL of the form https://github.com/<owner>/<repo>",
    )
}

impl GithubRepo {
    fn latest_release_api_url(&self) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            self.owner, self.name
        )
    }

    fn install_script_url(&self) -> String {
        format!(
            "https://github.com/{}/{}/releases/latest/download/tsuzuki-installer.sh",
            self.owner, self.name
        )
    }
}

/// GitHub's REST API requires a `User-Agent` header on every request (an
/// anonymous one can be rejected with 403) — unlike MAL, which needs none.
fn user_agent() -> String {
    format!(
        "tsuzuki/{} (+https://github.com/{}/{})",
        current_version(),
        github_repo().owner,
        github_repo().name
    )
}

/// Strips an optional leading `v`, then requires exactly three dot-
/// separated `u32` segments. A tag like `v1.2.3-beta` or `v1.2` returns
/// `None` — an honest "can't compare" result, never a coerced guess.
/// This project's own release process (`docs/development.md` §8) only
/// ever produces plain `vX.Y.Z` tags, so this hand-rolled comparison is a
/// safe, dependency-free stand-in for a full semver parser.
fn parse_semver_tag(tag: &str) -> Option<(u32, u32, u32)> {
    let rest = tag.strip_prefix('v').unwrap_or(tag);
    let mut parts = rest.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// The result of comparing the running binary's version against GitHub's
/// latest published release tag.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate,
    UpdateAvailable {
        latest: String,
    },
    /// Running ahead of the latest published release — a source build, or
    /// a tag pushed whose release build hasn't finished yet. Neither a
    /// pass nor a problem, so `render` gives it no ✓/⚠/✗ symbol.
    Ahead {
        latest: String,
    },
}

impl UpdateStatus {
    /// `UpdateAvailable`'s message states the transition, not a command to
    /// run — `update_command` installs immediately after printing this,
    /// with no separate flag to wait for.
    pub fn render(&self, current: &str) -> String {
        match self {
            UpdateStatus::UpToDate => format!("✓ up to date — v{current}"),
            UpdateStatus::UpdateAvailable { latest } => {
                format!("⚠ update available — v{current} → {latest}")
            }
            UpdateStatus::Ahead { latest } => {
                format!("v{current} is ahead of the latest published release ({latest})")
            }
        }
    }
}

/// Same three-variant shape as `mal.rs`'s `TokenExchangeError` and
/// `sync.rs`'s `SyncApiError`: `Transport` never reached GitHub at all,
/// `Rejected` is a real non-2xx response, `Parse` is a 2xx body that
/// wasn't the JSON shape expected.
///
/// **`Rejected` carries only the numeric status, not the response body.**
/// Unlike `mal.rs`/`sync.rs` (whose `Rejected` embeds the raw body — a
/// deliberate MAL OAuth-troubleshooting call, since that's a rare,
/// interactive action where the exact `invalid_grant`-style JSON is real
/// debug signal), a `tsuzuki update` failure is a plain command error a
/// user may see routinely. GitHub's own error bodies (e.g. a 404 when this
/// repository has no releases yet) are JSON blobs that would otherwise get
/// dumped verbatim into `error: ...` output — exactly the noise this
/// command's `Display` is meant to stay clean of.
#[derive(Debug)]
pub enum UpdateCheckError {
    Transport(String),
    Rejected(u16),
    Parse(String),
}

impl fmt::Display for UpdateCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateCheckError::Transport(m) => write!(f, "network error contacting GitHub: {m}"),
            UpdateCheckError::Rejected(status) => write!(f, "GitHub responded with HTTP {status}"),
            UpdateCheckError::Parse(m) => write!(f, "could not parse GitHub's response: {m}"),
        }
    }
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
}

/// The only real-network function in this module — GET GitHub's "latest
/// release" endpoint via `crate::http`'s shared agent. Note GitHub's
/// `/releases/latest` already excludes draft and pre-release releases
/// server-side, so no client-side filtering is needed here.
fn fetch_latest_release(repo: &GithubRepo) -> Result<HttpResponse, UpdateCheckError> {
    let agent = build_agent().map_err(|e| UpdateCheckError::Transport(e.to_string()))?;
    let req = agent
        .get(&repo.latest_release_api_url())
        .set("User-Agent", &user_agent());
    map_ureq_result(req.call()).map_err(|e| UpdateCheckError::Transport(e.to_string()))
}

/// Extracts the release tag from a response, folding the status check in
/// here (rather than a separate helper) so the `Rejected` path is
/// unit-testable from a fabricated `HttpResponse`, matching `mal.rs`'s
/// `parse_token_response(resp: &HttpResponse, ...)` shape.
fn parse_latest_release_tag(resp: &HttpResponse) -> Result<String, UpdateCheckError> {
    if !(200..300).contains(&resp.status) {
        return Err(UpdateCheckError::Rejected(resp.status));
    }
    serde_json::from_str::<ReleaseJson>(&resp.body)
        .map(|parsed| parsed.tag_name)
        .map_err(|e| UpdateCheckError::Parse(e.to_string()))
}

fn compare_to_latest_tag(
    current: (u32, u32, u32),
    latest_tag: &str,
) -> Result<UpdateStatus, UpdateCheckError> {
    let latest = parse_semver_tag(latest_tag).ok_or_else(|| {
        UpdateCheckError::Parse(format!("unrecognized release tag format: {latest_tag}"))
    })?;
    Ok(match current.cmp(&latest) {
        std::cmp::Ordering::Less => UpdateStatus::UpdateAvailable {
            latest: latest_tag.to_string(),
        },
        std::cmp::Ordering::Equal => UpdateStatus::UpToDate,
        std::cmp::Ordering::Greater => UpdateStatus::Ahead {
            latest: latest_tag.to_string(),
        },
    })
}

/// Checks GitHub for the latest published release and compares it against
/// [`current_version`]. The only function in this crate other than
/// `mal.rs`'s token/list calls that ever makes a real network request —
/// only reached because `tsuzuki update` was explicitly invoked.
pub fn check_for_update() -> Result<UpdateStatus, UpdateCheckError> {
    let repo = github_repo();
    let resp = fetch_latest_release(&repo)?;
    let tag = parse_latest_release_tag(&resp)?;
    let current = parse_semver_tag(current_version())
        .expect("our own Cargo.toml version is always plain X.Y.Z");
    compare_to_latest_tag(current, &tag)
}

/// The exact one-liner `README.md` documents manually — pure, so it can be
/// checked by string equality without ever spawning a shell. Not printed
/// to the user by `main.rs` (the status line already names the version
/// being installed); kept only for `run_install` and its own test.
fn install_command_line() -> String {
    format!("curl -LsSf {} | sh", github_repo().install_script_url())
}

/// Two variants, not `launch.rs`'s three-way `NotResolvable`/`Spawn`
/// split — there's no "resolution" step here (`sh` is assumed present, the
/// same floor `config_edit_command`'s `$EDITOR` spawn already assumes), so
/// a missing `sh` is just an ordinary `Spawn(io::Error)`.
#[derive(Debug)]
pub enum InstallError {
    Spawn(std::io::Error),
    NonZeroExit(ExitStatus),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::Spawn(e) => write!(f, "could not launch the installer: {e}"),
            InstallError::NonZeroExit(status) => {
                write!(f, "installer exited with {status}")
            }
        }
    }
}

/// Spawns `sh -c "<install_command_line()>"`, blocking until it exits with
/// real stdio inherited (mirrors `config_edit_command`'s `$EDITOR`
/// pattern, not `launch.rs`'s fire-and-forget mpv spawn — the user needs
/// to see curl/the installer's own output and get a real exit code back).
/// Prints nothing itself; `main.rs` owns all `println!`/`eprintln!`
/// narration, matching every other domain module's boundary in this crate.
pub fn run_install() -> Result<(), InstallError> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(install_command_line())
        .status()
        .map_err(InstallError::Spawn)?;
    if status.success() {
        Ok(())
    } else {
        Err(InstallError::NonZeroExit(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_repository_url() {
        let repo = parse_repository_url("https://github.com/mhmdsamerdev/tsuzuki").unwrap();
        assert_eq!(repo.owner, "mhmdsamerdev");
        assert_eq!(repo.name, "tsuzuki");
    }

    #[test]
    fn parses_a_repository_url_with_trailing_slash() {
        let repo = parse_repository_url("https://github.com/mhmdsamerdev/tsuzuki/").unwrap();
        assert_eq!(repo.owner, "mhmdsamerdev");
        assert_eq!(repo.name, "tsuzuki");
    }

    #[test]
    fn parses_a_repository_url_with_git_suffix() {
        let repo = parse_repository_url("https://github.com/mhmdsamerdev/tsuzuki.git").unwrap();
        assert_eq!(repo.owner, "mhmdsamerdev");
        assert_eq!(repo.name, "tsuzuki");
    }

    #[test]
    fn rejects_a_non_github_host() {
        assert!(parse_repository_url("https://gitlab.com/mhmdsamerdev/tsuzuki").is_none());
    }

    #[test]
    fn rejects_the_wrong_segment_count() {
        assert!(parse_repository_url("https://github.com/mhmdsamerdev").is_none());
        assert!(parse_repository_url("https://github.com/mhmdsamerdev/tsuzuki/extra").is_none());
    }

    #[test]
    fn github_repo_matches_the_real_cargo_toml() {
        let repo = github_repo();
        assert_eq!(repo.owner, "mhmdsamerdev");
        assert_eq!(repo.name, "tsuzuki");
    }

    #[test]
    fn builds_the_expected_urls() {
        let repo = GithubRepo {
            owner: "someone".to_string(),
            name: "project".to_string(),
        };
        assert_eq!(
            repo.latest_release_api_url(),
            "https://api.github.com/repos/someone/project/releases/latest"
        );
        assert_eq!(
            repo.install_script_url(),
            "https://github.com/someone/project/releases/latest/download/tsuzuki-installer.sh"
        );
    }

    #[test]
    fn parses_semver_tags() {
        assert_eq!(parse_semver_tag("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver_tag("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver_tag("v1.2"), None);
        assert_eq!(parse_semver_tag("v1.2.3.4"), None);
        assert_eq!(parse_semver_tag("v1.2.3-beta"), None);
        assert_eq!(parse_semver_tag("garbage"), None);
    }

    #[test]
    fn parses_a_valid_release_tag() {
        let resp = HttpResponse {
            status: 200,
            body: r#"{"tag_name":"v9.9.9"}"#.to_string(),
        };
        assert_eq!(parse_latest_release_tag(&resp).unwrap(), "v9.9.9");
    }

    #[test]
    fn missing_tag_name_is_a_parse_error() {
        let resp = HttpResponse {
            status: 200,
            body: r#"{"name":"nothing useful"}"#.to_string(),
        };
        assert!(matches!(
            parse_latest_release_tag(&resp),
            Err(UpdateCheckError::Parse(_))
        ));
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let resp = HttpResponse {
            status: 200,
            body: "not json".to_string(),
        };
        assert!(matches!(
            parse_latest_release_tag(&resp),
            Err(UpdateCheckError::Parse(_))
        ));
    }

    #[test]
    fn a_non_2xx_status_is_rejected_carrying_only_the_status_code() {
        let resp = HttpResponse {
            status: 404,
            body: r#"{"message":"Not Found","documentation_url":"https://example.com"}"#
                .to_string(),
        };
        assert!(matches!(
            parse_latest_release_tag(&resp),
            Err(UpdateCheckError::Rejected(404))
        ));
    }

    #[test]
    fn rejected_display_never_leaks_the_raw_response_body() {
        let rendered = UpdateCheckError::Rejected(404).to_string();
        assert_eq!(rendered, "GitHub responded with HTTP 404");
        assert!(!rendered.contains('{'), "must not echo a raw JSON body");
    }

    #[test]
    fn compares_versions() {
        assert_eq!(
            compare_to_latest_tag((0, 1, 0), "v0.2.0").unwrap(),
            UpdateStatus::UpdateAvailable {
                latest: "v0.2.0".to_string()
            }
        );
        assert_eq!(
            compare_to_latest_tag((0, 2, 0), "v0.2.0").unwrap(),
            UpdateStatus::UpToDate
        );
        assert_eq!(
            compare_to_latest_tag((0, 3, 0), "v0.2.0").unwrap(),
            UpdateStatus::Ahead {
                latest: "v0.2.0".to_string()
            }
        );
        assert!(matches!(
            compare_to_latest_tag((0, 1, 0), "not-a-version"),
            Err(UpdateCheckError::Parse(_))
        ));
    }

    #[test]
    fn renders_each_status() {
        assert_eq!(
            UpdateStatus::UpToDate.render("0.1.1"),
            "✓ up to date — v0.1.1"
        );
        assert_eq!(
            UpdateStatus::UpdateAvailable {
                latest: "v0.2.0".to_string()
            }
            .render("0.1.1"),
            "⚠ update available — v0.1.1 → v0.2.0"
        );
        assert_eq!(
            UpdateStatus::Ahead {
                latest: "v0.1.0".to_string()
            }
            .render("0.1.1"),
            "v0.1.1 is ahead of the latest published release (v0.1.0)"
        );
    }

    #[test]
    fn install_command_line_matches_the_readme() {
        assert_eq!(
            install_command_line(),
            "curl -LsSf https://github.com/mhmdsamerdev/tsuzuki/releases/latest/download/tsuzuki-installer.sh | sh"
        );
    }
}
