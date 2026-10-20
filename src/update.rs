//! `tsuzuki update`: version check against GitHub and installer invocation.
//!
//! The GitHub "latest release" lookup is the only network call here, made
//! only from [`check_for_update`]; `doctor` and startup never reach it.

use std::fmt;
use std::process::ExitStatus;

use serde::Deserialize;

use crate::http::{HttpResponse, build_agent, map_ureq_result};

/// The compiled-in crate version. Shared with `doctor`'s version banner.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Owner and name from `Cargo.toml`'s `repository` field.
struct GithubRepo {
    owner: String,
    name: String,
}

/// Parses `https://github.com/<owner>/<name>`, allowing a trailing slash or
/// `.git`. Returns `None` for any other host or path shape.
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

/// Parses the compile-time repository URL. A failure means `Cargo.toml` is
/// wrong; `github_repo_matches_the_real_cargo_toml` catches it in tests.
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

/// GitHub's API can reject requests without a `User-Agent` header.
fn user_agent() -> String {
    format!(
        "tsuzuki/{} (+https://github.com/{}/{})",
        current_version(),
        github_repo().owner,
        github_repo().name
    )
}

/// Parses `vX.Y.Z` (the `v` is optional). Anything else, such as
/// `v1.2.3-beta` or `v1.2`, returns `None` rather than a guess.
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

/// The running version compared against the latest release tag.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate,
    UpdateAvailable {
        latest: String,
    },
    /// Newer than the latest release (a source build or an unfinished
    /// release); neither ok nor a problem, so it gets no symbol.
    Ahead {
        latest: String,
    },
}

impl UpdateStatus {
    /// `UpdateAvailable` states the transition because the install follows
    /// immediately.
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

/// `Transport`: GitHub was never reached. `Rejected`: non-2xx response.
/// `Parse`: 2xx body of an unexpected shape.
///
/// `Rejected` keeps only the status; GitHub's JSON error bodies would just
/// add noise to the error line.
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

/// GETs the latest-release endpoint, which already excludes drafts and
/// pre-releases.
fn fetch_latest_release(repo: &GithubRepo) -> Result<HttpResponse, UpdateCheckError> {
    let agent = build_agent().map_err(|e| UpdateCheckError::Transport(e.to_string()))?;
    let req = agent
        .get(&repo.latest_release_api_url())
        .header("User-Agent", user_agent());
    map_ureq_result(req.call()).map_err(|e| UpdateCheckError::Transport(e.to_string()))
}

/// Checks the status, then extracts the release tag.
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

/// Compares [`current_version`] against GitHub's latest release.
pub fn check_for_update() -> Result<UpdateStatus, UpdateCheckError> {
    let repo = github_repo();
    let resp = fetch_latest_release(&repo)?;
    let tag = parse_latest_release_tag(&resp)?;
    let current = parse_semver_tag(current_version())
        .expect("our own Cargo.toml version is always plain X.Y.Z");
    compare_to_latest_tag(current, &tag)
}

/// The install one-liner documented in `README.md`.
fn install_command_line() -> String {
    format!("curl -LsSf {} | sh", github_repo().install_script_url())
}

/// `sh` is assumed present, so a missing one is just `Spawn`.
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

/// Runs the installer through `sh`, blocking with inherited stdio so the
/// user sees its output. Prints nothing itself.
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
