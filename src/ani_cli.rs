//! ani-cli as a second source: its history file (read-only) and the rule that
//! decides which mpv-script log entries are ani-cli playback.
//!
//! A history line is `episode<TAB>id<TAB>title`. History never proves
//! watching; it only says which titles are ani-cli's, so an unrelated stream
//! can't be mistaken for one.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::config::AniCli;
use crate::roots::expand_tilde;
use crate::script_log::ScriptLogEntry;

/// The word that names the source on the command line (`add ani-cli`,
/// `--root ani-cli`). A folder of that name is written `./ani-cli`.
pub const SOURCE_ARG: &str = "ani-cli";

/// The reserved `SeriesRecord.root` of every ani-cli series. Not a path, so it
/// can never equal a tracked root.
pub const ROOT: &str = "ani-cli://";

/// One parsed history line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The last episode ani-cli launched, as written (may be decimal).
    pub episode: String,
    pub id: String,
    pub title: String,
}

/// ani-cli's history, read once per command.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct History {
    pub entries: Vec<HistoryEntry>,
}

impl History {
    /// Lines that aren't three tab-separated, non-empty fields are skipped.
    pub fn parse(text: &str) -> History {
        let entries = text
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\t');
                let (episode, id, title) = (fields.next()?, fields.next()?, fields.next()?);
                let (episode, id, title) = (episode.trim(), id.trim(), title.trim());
                if episode.is_empty() || id.is_empty() || title.is_empty() {
                    return None;
                }
                Some(HistoryEntry {
                    episode: episode.to_string(),
                    id: id.to_string(),
                    title: title.to_string(),
                })
            })
            .collect();
        History { entries }
    }

    /// How many history lines carry each title.
    fn title_counts(&self) -> BTreeMap<&str, usize> {
        let mut counts = BTreeMap::new();
        for entry in &self.entries {
            *counts.entry(entry.title.as_str()).or_insert(0) += 1;
        }
        counts
    }
}

/// Reads and parses the history file.
pub fn read_history(path: &Path) -> io::Result<History> {
    Ok(History::parse(&std::fs::read_to_string(path)?))
}

/// ani-cli's own lookup order, with `configured` (a `history_path` from
/// config, `~` allowed) taking precedence. `None` when nothing resolves.
pub fn history_path(
    configured: &str,
    hist_dir_env: Option<&str>,
    xdg_state_env: Option<&str>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if !configured.is_empty() {
        return Some(expand_tilde(Path::new(configured), home));
    }
    let dir = if let Some(dir) = hist_dir_env.filter(|s| !s.is_empty()) {
        PathBuf::from(dir)
    } else if let Some(state) = xdg_state_env.filter(|s| !s.is_empty()) {
        Path::new(state).join("ani-cli")
    } else {
        home?.join(".local").join("state").join("ani-cli")
    };
    Some(dir.join("ani-hsts"))
}

/// [`history_path`] against the real environment.
pub fn history_path_here(config: &AniCli, home: Option<&Path>) -> Option<PathBuf> {
    history_path(
        &config.history_path,
        std::env::var("ANI_CLI_HIST_DIR").ok().as_deref(),
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
        home,
    )
}

/// An ani-cli playback: the show's title and the integer episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub title: String,
    pub episode: u32,
}

/// What one log entry is, as far as ani-cli is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Claimed(Claim),
    /// Right shape but a fractional episode (`12.5`); episodes are integers.
    Decimal,
    /// The title matches several history lines, so the show can't be told.
    Ambiguous(String),
    /// Not ani-cli's: a local file, no title, no episode suffix, or a title
    /// that isn't in history.
    NotAniCli,
}

fn is_stream_url(source: &Path) -> bool {
    let s = source.to_string_lossy();
    match s.split_once("://") {
        Some((scheme, _)) => {
            scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        }
        None => false,
    }
}

/// Splits `<title> Episode <n>` at its last ` Episode `. `Err(())` marks a
/// fractional number.
fn split_title(media_title: &str) -> Option<Result<(&str, u32), ()>> {
    let (title, number) = media_title.rsplit_once(" Episode ")?;
    if title.is_empty() || number.is_empty() {
        return None;
    }
    if number.bytes().all(|b| b.is_ascii_digit()) {
        return number.parse::<u32>().ok().map(|n| Ok((title, n)));
    }
    let fractional = number.split_once('.').is_some_and(|(whole, frac)| {
        !whole.is_empty()
            && !frac.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && frac.bytes().all(|b| b.is_ascii_digit())
    });
    fractional.then_some(Err(()))
}

/// Decides whether `entry` is ani-cli playback. All of these must hold: a
/// stream URL source, a `title=` ending in ` Episode <n>`, and the rest equal
/// to exactly one history title.
#[cfg(test)]
pub fn claim(entry: &ScriptLogEntry, history: &History) -> Verdict {
    classify(entry, &history.title_counts())
}

fn classify(entry: &ScriptLogEntry, counts: &BTreeMap<&str, usize>) -> Verdict {
    let Some(media_title) = &entry.title else {
        return Verdict::NotAniCli;
    };
    if !is_stream_url(&entry.source_path) {
        return Verdict::NotAniCli;
    }
    let Some(split) = split_title(media_title) else {
        return Verdict::NotAniCli;
    };
    let (title, number) = match split {
        Ok(parts) => parts,
        Err(()) => {
            let title = media_title
                .rsplit_once(" Episode ")
                .map_or("", |(title, _)| title);
            return match counts.get(title) {
                Some(_) => Verdict::Decimal,
                None => Verdict::NotAniCli,
            };
        }
    };
    match counts.get(title) {
        None => Verdict::NotAniCli,
        Some(1) => Verdict::Claimed(Claim {
            title: title.to_string(),
            episode: number,
        }),
        Some(_) => Verdict::Ambiguous(title.to_string()),
    }
}

/// The result of claiming a whole scan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Claims<'a> {
    pub claimed: Vec<(Claim, &'a ScriptLogEntry)>,
    /// Entries skipped for a fractional episode.
    pub decimal: usize,
    /// Titles skipped because history lists them more than once.
    pub ambiguous: BTreeSet<String>,
}

/// Claims every entry against one history read.
pub fn claim_all<'a>(
    entries: impl IntoIterator<Item = &'a ScriptLogEntry>,
    history: &History,
) -> Claims<'a> {
    let counts = history.title_counts();
    let mut claims = Claims::default();
    for entry in entries {
        match classify(entry, &counts) {
            Verdict::Claimed(claim) => claims.claimed.push((claim, entry)),
            Verdict::Decimal => claims.decimal += 1,
            Verdict::Ambiguous(title) => {
                claims.ambiguous.insert(title);
            }
            Verdict::NotAniCli => {}
        }
    }
    claims
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn history() -> History {
        History::parse(
            "9\tblue-lock-964\tBlue Lock\n\
             4\tchained-soldier-3873\tChained Soldier\n\
             1\tdup-a-1\tTwin\n\
             2\tdup-b-2\tTwin\n",
        )
    }

    fn stream(title: Option<&str>) -> ScriptLogEntry {
        ScriptLogEntry {
            source_path: PathBuf::from("https://cdn.example/x/index.m3u8?t=abc"),
            duration: Some(1400.0),
            position: Some(300.0),
            reason: "quit".to_string(),
            title: title.map(str::to_string),
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    fn entry_at(source: &str, title: Option<&str>) -> ScriptLogEntry {
        ScriptLogEntry {
            source_path: PathBuf::from(source),
            ..stream(title)
        }
    }

    // --- history parsing ---

    #[test]
    fn history_parse_reads_three_tab_separated_fields_per_line() {
        let parsed = History::parse("9\tblue-lock-964\tBlue Lock\n1\tyasuke-3298\tYasuke\n");

        assert_eq!(
            parsed.entries,
            vec![
                HistoryEntry {
                    episode: "9".to_string(),
                    id: "blue-lock-964".to_string(),
                    title: "Blue Lock".to_string(),
                },
                HistoryEntry {
                    episode: "1".to_string(),
                    id: "yasuke-3298".to_string(),
                    title: "Yasuke".to_string(),
                },
            ]
        );
    }

    #[test]
    fn history_parse_skips_malformed_lines_without_aborting() {
        let parsed = History::parse(
            "garbage\n\
             3\tonly-two-fields\n\
             \t\t\n\
             4\t\tNo Id\n\
             5\tgood-1\tGood\n\
             \n",
        );

        assert_eq!(
            parsed.entries,
            vec![HistoryEntry {
                episode: "5".to_string(),
                id: "good-1".to_string(),
                title: "Good".to_string(),
            }]
        );
    }

    #[test]
    fn history_parse_keeps_a_title_containing_tabs_whole_and_trims_crlf() {
        let parsed = History::parse("2\tid-1\tA\tB\r\n");

        assert_eq!(
            parsed.entries,
            vec![HistoryEntry {
                episode: "2".to_string(),
                id: "id-1".to_string(),
                title: "A\tB".to_string(),
            }]
        );
    }

    #[test]
    fn history_parse_of_empty_text_has_no_entries() {
        assert_eq!(History::parse("").entries, Vec::new());
    }

    // --- history path ---

    #[test]
    fn history_path_prefers_the_configured_path_and_expands_tilde() {
        let home = Path::new("/home/u");

        assert_eq!(
            history_path("~/h/ani-hsts", Some("/env"), Some("/state"), Some(home)),
            Some(PathBuf::from("/home/u/h/ani-hsts"))
        );
    }

    #[test]
    fn history_path_follows_ani_cli_lookup_order() {
        let home = Path::new("/home/u");

        assert_eq!(
            history_path("", Some("/hist"), Some("/state"), Some(home)),
            Some(PathBuf::from("/hist/ani-hsts"))
        );
        assert_eq!(
            history_path("", None, Some("/state"), Some(home)),
            Some(PathBuf::from("/state/ani-cli/ani-hsts"))
        );
        assert_eq!(
            history_path("", Some(""), Some(""), Some(home)),
            Some(PathBuf::from("/home/u/.local/state/ani-cli/ani-hsts"))
        );
    }

    #[test]
    fn history_path_without_any_base_is_none() {
        assert_eq!(history_path("", None, None, None), None);
    }

    #[test]
    fn read_history_of_a_missing_file_is_a_not_found_error() {
        let dir = tempfile::tempdir().unwrap();

        let err = read_history(&dir.path().join("ani-hsts")).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn read_history_parses_the_file_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ani-hsts");
        std::fs::write(&path, "9\tblue-lock-964\tBlue Lock\n").unwrap();

        assert_eq!(
            read_history(&path).unwrap().entries,
            vec![HistoryEntry {
                episode: "9".to_string(),
                id: "blue-lock-964".to_string(),
                title: "Blue Lock".to_string(),
            }]
        );
    }

    // --- claiming ---

    #[test]
    fn claim_accepts_a_stream_whose_title_matches_one_history_title() {
        assert_eq!(
            claim(&stream(Some("Blue Lock Episode 9")), &history()),
            Verdict::Claimed(Claim {
                title: "Blue Lock".to_string(),
                episode: 9,
            })
        );
    }

    #[test]
    fn claim_splits_at_the_last_episode_word_so_titles_may_contain_it() {
        let history = History::parse("1\tx-1\tEpisode Zero Episode One\n");

        assert_eq!(
            claim(
                &stream(Some("Episode Zero Episode One Episode 3")),
                &history
            ),
            Verdict::Claimed(Claim {
                title: "Episode Zero Episode One".to_string(),
                episode: 3,
            })
        );
    }

    #[test]
    fn claim_ignores_a_title_that_is_not_in_history() {
        assert_eq!(
            claim(&stream(Some("Some YouTube Video Episode 3")), &history()),
            Verdict::NotAniCli
        );
    }

    #[test]
    fn claim_requires_an_exact_title_not_a_prefix_or_case_variant() {
        for title in [
            "Blue Lock Extra Episode 9",
            "blue lock Episode 9",
            "Blue Loc Episode 9",
        ] {
            assert_eq!(
                claim(&stream(Some(title)), &history()),
                Verdict::NotAniCli,
                "{title}"
            );
        }
    }

    #[test]
    fn claim_ignores_entries_without_a_title_or_an_episode_suffix() {
        for title in [
            None,
            Some("Blue Lock"),
            Some("Blue Lock Episode "),
            Some(" Episode 9"),
        ] {
            assert_eq!(
                claim(&stream(title), &history()),
                Verdict::NotAniCli,
                "{title:?}"
            );
        }
    }

    #[test]
    fn claim_ignores_a_local_file_even_with_a_matching_title() {
        assert_eq!(
            claim(
                &entry_at(
                    "/home/u/Anime/Blue Lock/ep09.mkv",
                    Some("Blue Lock Episode 9")
                ),
                &history()
            ),
            Verdict::NotAniCli
        );
    }

    #[test]
    fn claim_accepts_any_url_scheme_but_not_a_path_with_a_colon() {
        for source in ["http://h/x", "https://h/x", "rtmp://h/x"] {
            assert_eq!(
                claim(&entry_at(source, Some("Blue Lock Episode 9")), &history()),
                Verdict::Claimed(Claim {
                    title: "Blue Lock".to_string(),
                    episode: 9,
                }),
                "{source}"
            );
        }
        for source in ["/home/u/a:b/x.mkv", "://x", "1http://x"] {
            assert_eq!(
                claim(&entry_at(source, Some("Blue Lock Episode 9")), &history()),
                Verdict::NotAniCli,
                "{source}"
            );
        }
    }

    #[test]
    fn claim_reports_a_fractional_episode_of_a_known_show_as_decimal() {
        assert_eq!(
            claim(&stream(Some("Blue Lock Episode 12.5")), &history()),
            Verdict::Decimal
        );
    }

    #[test]
    fn claim_ignores_a_fractional_episode_of_an_unknown_show() {
        assert_eq!(
            claim(&stream(Some("Unknown Episode 12.5")), &history()),
            Verdict::NotAniCli
        );
    }

    #[test]
    fn claim_ignores_non_numeric_and_malformed_episode_numbers() {
        for title in [
            "Blue Lock Episode x",
            "Blue Lock Episode 9a",
            "Blue Lock Episode .5",
            "Blue Lock Episode 5.",
            "Blue Lock Episode -1",
            "Blue Lock Episode 99999999999",
        ] {
            assert_eq!(
                claim(&stream(Some(title)), &history()),
                Verdict::NotAniCli,
                "{title}"
            );
        }
    }

    #[test]
    fn claim_reports_a_title_listed_twice_as_ambiguous() {
        assert_eq!(
            claim(&stream(Some("Twin Episode 2")), &history()),
            Verdict::Ambiguous("Twin".to_string())
        );
    }

    #[test]
    fn split_title_rejects_an_empty_title_or_an_empty_number() {
        assert_eq!(split_title(" Episode 9"), None);
        assert_eq!(split_title("Show Episode "), None);
        assert_eq!(split_title("Show Episode 9"), Some(Ok(("Show", 9))));
    }

    #[test]
    fn claim_accepts_episode_zero() {
        assert_eq!(
            claim(&stream(Some("Blue Lock Episode 0")), &history()),
            Verdict::Claimed(Claim {
                title: "Blue Lock".to_string(),
                episode: 0,
            })
        );
    }

    #[test]
    fn claim_all_splits_a_scan_into_claimed_decimal_and_ambiguous() {
        let entries = [
            stream(Some("Blue Lock Episode 9")),
            stream(Some("Chained Soldier Episode 4")),
            stream(Some("Blue Lock Episode 12.5")),
            stream(Some("Twin Episode 1")),
            stream(Some("Twin Episode 2")),
            stream(Some("Elsewhere Episode 1")),
            entry_at("/a/b.mkv", None),
        ];

        let claims = claim_all(&entries, &history());

        assert_eq!(
            claims
                .claimed
                .iter()
                .map(|(claim, _)| claim.clone())
                .collect::<Vec<_>>(),
            vec![
                Claim {
                    title: "Blue Lock".to_string(),
                    episode: 9,
                },
                Claim {
                    title: "Chained Soldier".to_string(),
                    episode: 4,
                },
            ]
        );
        assert_eq!(claims.decimal, 1);
        assert_eq!(claims.ambiguous, BTreeSet::from(["Twin".to_string()]));
    }

    #[test]
    fn claim_all_against_an_empty_history_claims_nothing() {
        let entries = [stream(Some("Blue Lock Episode 9"))];

        assert_eq!(claim_all(&entries, &History::default()), Claims::default());
    }
}
