//! MyAnimeList linking and the two-way episode-progress merge (design.md
//! §7, Phase 8b). Builds directly on Phase 8a's `mal.rs` — the authenticated
//! HTTP client (`MalHttpClient::get`/`patch_form`) and `ensure_access_token`
//! — without touching any OAuth/PKCE/token-refresh internals; this module
//! only ever calls into that existing interface.
//!
//! ## Where a series → MAL-id link is stored, and why
//!
//! In `state.toml`, as `state::MalLink` (a new `[[mal_links]]` array-of-
//! tables, keyed by the same `(root, name)` composite key `SeriesRecord`
//! already uses) — not in `config.toml`. The two files' actual distinction
//! (CLAUDE.md §3: "config is meant to be hand-edited and versioned in
//! dotfiles repos; state is not") is about *authorship*, not portability: a
//! MAL link is always Tsuzuki-derived/written, either by exact-match
//! auto-linking during `sync` or by the explicit `sync link` command
//! recording its own result — never something a user directly hand-types
//! into a TOML file the way a tracked root or an exclusion is. It belongs
//! in the same category, and the same file, as `SeriesRecord` itself, and
//! gets the same atomic-write guarantee for free by reusing `state::write`
//! rather than needing a third persisted file with its own write path.
//!
//! ## The non-destructive invariant (CLAUDE.md §2)
//!
//! "Local best-known episode number" for the merge comparison
//! ([`local_effective_episode`]) is **not** the raw, `reconcile.rs`-owned
//! `episode_number` alone, for two separate reasons:
//!
//! - `episode_number` is a cursor (the episode currently being watched, or
//!   just-advanced-to at position 0), not a watched count — MAL's field
//!   means "episodes already watched," so the number fed into the merge is
//!   `episode_number - 1` in the ordinary case (see
//!   [`local_effective_episode`]'s own doc comment for the one narrow
//!   exception, a `Dropped` record left sitting on a just-completed finale).
//! - `reconcile.rs` unconditionally overwrites `episode_number` from local
//!   file position on every reconcile pass, so a MAL-driven pull has to be
//!   remembered somewhere `reconcile.rs` never touches
//!   (`SeriesRecord::mal_episode_awareness`) — and that remembered number
//!   has to keep counting as "local" in every future comparison, or a merge
//!   could re-read a stale, lower local number and silently push it back
//!   down over a higher number MAL (or a previous pull) already established.
//!
//! [`decide_merge`] itself is a pure three-way comparison with no special
//! cases — the safety comes entirely from never feeding it a number that
//! regresses what's already been confirmed on either side.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::anitomy;
use crate::config::Config;
use crate::exclude;
use crate::mal::{HttpResponse, MalHttpClient};
use crate::roots::{expand_tilde, normalize};
use crate::series;
use crate::state::{MalLink, SeriesRecord, State, Status, now_unix};

pub const API_BASE: &str = "https://api.myanimelist.net/v2";

/// A light, deliberate pause between consecutive MAL API calls within one
/// `sync` run — MAL doesn't publish a hard documented rate limit, but this
/// project doesn't fire requests at it in an unthrottled burst across a
/// large tracked library either. Not a `config.toml` field: this is an
/// internal politeness delay, not a user-facing tunable in the design doc's
/// sense (contrast `resume_seconds`/`completion_remaining_seconds`, which
/// change observable behavior) — tests pass `Duration::ZERO` directly to
/// every function that takes a delay parameter instead.
pub const API_CALL_DELAY: Duration = Duration::from_millis(300);

/// Sleeps `delay` before every call after the first in one loop — call once
/// per HTTP request, threading the same `is_first` through one logical
/// sequence of calls (one per pagination loop, one per the per-series sync
/// loop). Never sleeps before the very first call in a run.
fn throttle(is_first: &mut bool, delay: Duration) {
    if *is_first {
        *is_first = false;
    } else {
        std::thread::sleep(delay);
    }
}

// --- Title normalization, exact-match linking, similarity ranking ---

/// Normalizes a title for comparison (design.md §7: "Normalize both sides
/// (case, brackets, whitespace)"): strips any parenthesized/bracketed
/// content entirely (annotations like a year or source tag that wouldn't be
/// worded identically on both a local folder name and MAL's title fields),
/// lowercases, and collapses whitespace.
pub fn normalize_title(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut depth: i32 = 0;
    for c in input.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnimeTitles {
    pub main: String,
    pub en: String,
    pub ja: String,
    pub synonyms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnimeListEntry {
    pub id: u32,
    pub titles: AnimeTitles,
    pub num_episodes_watched: u32,
}

/// Every known title for one entry — main title, English, Japanese (when
/// present), and every synonym. This is the full candidate set design.md
/// §7 means by "any of an entry's known titles."
fn candidate_titles(entry: &AnimeListEntry) -> Vec<&str> {
    let mut titles = vec![entry.titles.main.as_str()];
    if !entry.titles.en.is_empty() {
        titles.push(&entry.titles.en);
    }
    if !entry.titles.ja.is_empty() {
        titles.push(&entry.titles.ja);
    }
    titles.extend(entry.titles.synonyms.iter().map(|s| s.as_str()));
    titles
}

/// An **exact** match (post-normalization) between `local_name` and any
/// known title of any entry — never fuzzy, never picks a "closest" result.
pub fn find_exact_match<'a>(
    local_name: &str,
    entries: &'a [AnimeListEntry],
) -> Option<&'a AnimeListEntry> {
    let target = normalize_title(local_name);
    entries.iter().find(|e| {
        candidate_titles(e)
            .iter()
            .any(|t| normalize_title(t) == target)
    })
}

/// Levenshtein edit distance, character-based — small and hand-written
/// rather than a new dependency, since this only ever ranks what's
/// *displayed* to the user (design.md §7: "similarity ranks what's
/// displayed, never decides a match") and never feeds a linking decision.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// Normalized similarity in `[0.0, 1.0]` (`1.0` = identical after
/// normalization), for ranking display candidates only.
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (normalize_title(a), normalize_title(b));
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein(&a, &b) as f64 / max_len as f64)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub mal_id: u32,
    pub title: String,
    pub similarity: f64,
}

/// The `top_n` closest entries by title similarity — display-only (design.md
/// §7: "show the closest candidates by title... user picks one" — per this
/// project's own list-only, non-interactive precedent for exactly this
/// shape of ambiguity (CLAUDE.md's "Anitomy-candidate confirmation stays
/// list-only, not interactive"), "picks one" here means re-invoking
/// `sync link` with an explicit `--mal-id`, not an interactive prompt).
pub fn rank_candidates(
    local_name: &str,
    entries: &[AnimeListEntry],
    top_n: usize,
) -> Vec<Candidate> {
    let mut scored: Vec<Candidate> = entries
        .iter()
        .map(|e| {
            let similarity = candidate_titles(e)
                .iter()
                .map(|t| title_similarity(local_name, t))
                .fold(0.0_f64, f64::max);
            Candidate {
                mal_id: e.id,
                title: e.titles.main.clone(),
                similarity,
            }
        })
        .collect();
    scored.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap());
    scored.truncate(top_n);
    scored
}

/// The single best-matching candidate for a "did you mean...?" suggestion —
/// but only when it's actually good enough to be worth showing.
/// `fuzzy_match_threshold` (`[mal] fuzzy_match_threshold` in config.toml,
/// CLAUDE.md's "sync output only ever suggests a confident fuzzy match")
/// is the bar: below it, `rank_candidates`' own top result is still just
/// "the least-bad of whatever's on the list" — often a completely unrelated
/// series — and surfacing it as a suggestion is noise, not help. This never
/// changes what counts as a *match*; `find_exact_match` alone still decides
/// that. It only decides what's worth *displaying* as a manual-link hint,
/// same list-only, never-auto-links precedent `rank_candidates` itself
/// already documents.
pub fn best_candidate(
    local_name: &str,
    entries: &[AnimeListEntry],
    fuzzy_match_threshold: f64,
) -> Option<Candidate> {
    rank_candidates(local_name, entries, 1)
        .into_iter()
        .next()
        .filter(|c| c.similarity >= fuzzy_match_threshold)
}

// --- MAL data API: fetch list, fetch one anime's status, push a count ---

#[derive(Debug)]
pub enum SyncApiError {
    /// Could not reach MAL at all.
    Transport(String),
    /// MAL responded, but rejected the request (non-2xx).
    Rejected(String),
    /// MAL responded 2xx but the body wasn't the JSON shape expected.
    Parse(String),
}

impl fmt::Display for SyncApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncApiError::Transport(m) => write!(f, "network error contacting MyAnimeList: {m}"),
            SyncApiError::Rejected(m) => write!(f, "MyAnimeList rejected the request: {m}"),
            SyncApiError::Parse(m) => write!(f, "could not parse MyAnimeList's response: {m}"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AlternativeTitlesJson {
    #[serde(default)]
    en: String,
    #[serde(default)]
    ja: String,
    #[serde(default)]
    synonyms: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MyListStatusJson {
    #[serde(default)]
    num_episodes_watched: u32,
}

#[derive(Debug, Deserialize)]
struct AnimeNodeJson {
    id: u32,
    title: String,
    #[serde(default)]
    alternative_titles: Option<AlternativeTitlesJson>,
    #[serde(default)]
    my_list_status: Option<MyListStatusJson>,
}

#[derive(Debug, Deserialize)]
struct AnimeListEntryJson {
    node: AnimeNodeJson,
}

#[derive(Debug, Deserialize)]
struct PagingJson {
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnimeListResponseJson {
    data: Vec<AnimeListEntryJson>,
    #[serde(default)]
    paging: Option<PagingJson>,
}

fn to_anime_list_entry(node: AnimeNodeJson) -> AnimeListEntry {
    let (en, ja, synonyms) = match node.alternative_titles {
        Some(t) => (t.en, t.ja, t.synonyms),
        None => (String::new(), String::new(), Vec::new()),
    };
    AnimeListEntry {
        id: node.id,
        titles: AnimeTitles {
            main: node.title,
            en,
            ja,
            synonyms,
        },
        num_episodes_watched: node
            .my_list_status
            .map(|s| s.num_episodes_watched)
            .unwrap_or(0),
    }
}

/// A hard cap on pages followed via `paging.next`, purely defensive against
/// a pathological/looping response — 200 pages at `limit=1000` is 200,000
/// entries, far beyond any real MAL list.
const MAX_LIST_PAGES: usize = 200;

/// Fetches the authenticated user's full anime list (design.md §7's
/// "fetch the user's MAL list"), following `paging.next` until exhausted.
/// `fields=alternative_titles,my_list_status` so every entry already
/// carries what both linking (titles) and merging (`num_episodes_watched`)
/// need — a freshly-linked series never needs a second lookup this same run.
pub fn fetch_anime_list(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    delay: Duration,
) -> Result<Vec<AnimeListEntry>, SyncApiError> {
    let mut entries = Vec::new();
    let mut url = format!("{API_BASE}/users/@me/animelist");
    let mut query: Vec<(&str, &str)> = vec![
        ("fields", "alternative_titles,my_list_status"),
        ("limit", "1000"),
        ("nsfw", "true"),
    ];
    let mut is_first = true;

    for _ in 0..MAX_LIST_PAGES {
        throttle(&mut is_first, delay);
        let resp = client
            .get(&url, bearer_token, &query)
            .map_err(|e| SyncApiError::Transport(e.to_string()))?;
        require_success(&resp)?;
        let parsed: AnimeListResponseJson =
            serde_json::from_str(&resp.body).map_err(|e| SyncApiError::Parse(e.to_string()))?;

        for item in parsed.data {
            entries.push(to_anime_list_entry(item.node));
        }

        match parsed.paging.and_then(|p| p.next) {
            Some(next) if next != url => {
                url = next;
                query = Vec::new(); // the next URL already embeds every query param
            }
            _ => break,
        }
    }

    Ok(entries)
}

fn require_success(resp: &HttpResponse) -> Result<(), SyncApiError> {
    if !(200..300).contains(&resp.status) {
        return Err(SyncApiError::Rejected(format!(
            "HTTP {}: {}",
            resp.status, resp.body
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnimeStatus {
    /// Whether the anime is genuinely on the authenticated user's list —
    /// `my_list_status` present in the response. `false` means a valid MAL
    /// anime ID that simply isn't on this user's list at all.
    pub on_list: bool,
    pub num_episodes_watched: u32,
}

/// A single anime's current list status by ID — used both to validate a
/// manual `sync link --mal-id` (does it actually appear on the user's list)
/// and, for an already-linked series, to learn MAL's current
/// `num_episodes_watched` for the merge comparison, without re-fetching or
/// re-scanning the entire list.
pub fn fetch_anime_status(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    mal_id: u32,
) -> Result<AnimeStatus, SyncApiError> {
    let url = format!("{API_BASE}/anime/{mal_id}");
    let resp = client
        .get(&url, bearer_token, &[("fields", "my_list_status")])
        .map_err(|e| SyncApiError::Transport(e.to_string()))?;
    require_success(&resp)?;
    let parsed: AnimeNodeJson =
        serde_json::from_str(&resp.body).map_err(|e| SyncApiError::Parse(e.to_string()))?;
    Ok(AnimeStatus {
        on_list: parsed.my_list_status.is_some(),
        num_episodes_watched: parsed
            .my_list_status
            .map(|s| s.num_episodes_watched)
            .unwrap_or(0),
    })
}

/// Pushes a new episode count to MAL (design.md §7: `PATCH
/// /v2/anime/{id}/my_list_status`). **The request body field is
/// `num_watched_episodes`** — MAL's API is asymmetric here: the *response*
/// (`fetch_anime_status`/the list fetch) reports `num_episodes_watched`,
/// but the *update* endpoint's own request-body field is
/// `num_watched_episodes` (words swapped), confirmed independently against
/// Taiga's own real request-building code and the unofficial MAL API
/// specification (which explicitly notes the two field names differ). This
/// asymmetry is easy to get wrong silently — using the response field name
/// here wouldn't error, MAL would just ignore the unrecognized field and
/// the episode count would silently never update.
pub fn push_episode_count(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    mal_id: u32,
    num_watched_episodes: u32,
) -> Result<(), SyncApiError> {
    let url = format!("{API_BASE}/anime/{mal_id}/my_list_status");
    let value = num_watched_episodes.to_string();
    let resp = client
        .patch_form(
            &url,
            bearer_token,
            &[("num_watched_episodes", value.as_str())],
        )
        .map_err(|e| SyncApiError::Transport(e.to_string()))?;
    require_success(&resp)?;
    Ok(())
}

// --- Merge decision (CLAUDE.md §2: never write a smaller number) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeAction {
    /// Local is ahead — push this number to MAL.
    Push(u32),
    /// MAL is ahead — update local awareness only, to this number.
    Pull(u32),
    NoOp,
}

/// The entire two-way merge rule (design.md §7): take the higher number, in
/// either direction, never overwrite down. Pure and total — every input
/// pair resolves to exactly one of three outcomes, no partial states.
pub fn decide_merge(local: u32, mal: u32) -> MergeAction {
    match local.cmp(&mal) {
        std::cmp::Ordering::Greater => MergeAction::Push(local),
        std::cmp::Ordering::Less => MergeAction::Pull(mal),
        std::cmp::Ordering::Equal => MergeAction::NoOp,
    }
}

/// The local side of a merge comparison — see this module's own doc comment
/// for why this folds in `mal_episode_awareness`, not just the raw,
/// `reconcile.rs`-owned `episode_number` alone.
///
/// **`episode_number` is a cursor, not a watched count.** It names the
/// episode a record is currently sitting on — the one being watched (a real
/// mid-episode position), or the one just advanced to at position 0 after
/// the previous episode completed (CLAUDE.md's "When the CURRENT tracked
/// episode itself completes") — not an episode confirmed finished. MAL's own
/// field means "episodes already watched," so pushing the raw cursor number
/// overstates progress by exactly one: the cursor's own episode hasn't been
/// counted as watched yet. The fix is the episode immediately before it,
/// `episode_number.saturating_sub(1)`.
///
/// **One exception:** `apply_completion_advance` (`reconcile.rs`) has no
/// next episode to advance a completed series to, so it leaves the cursor
/// sitting on the finale it just confirmed complete and transitions straight
/// to `Dropped` — the only case where the cursor's own episode genuinely
/// *is* the watched count, not one short of it. That state is identifiable
/// without guessing: `Dropped` with a real (nonzero) `position_seconds`, the
/// same nonzero near-the-end position `classify()` used to confirm
/// completion. A `Dropped` record with `position_seconds == 0.0` never
/// touched its cursor episode at all (advanced to a fresh next episode by
/// completion, then dropped before opening it — or dropped right at the
/// index-0 baseline) and still gets the ordinary `-1`.
pub fn local_effective_episode(record: &SeriesRecord) -> u32 {
    let cursor = record.episode_number.unwrap_or(0);
    let watched = if record.status == Status::Dropped && record.position_seconds > 0.0 {
        cursor
    } else {
        cursor.saturating_sub(1)
    };
    watched.max(record.mal_episode_awareness.unwrap_or(0))
}

/// Advances Tsuzuki's local playback cursor to the first locally available
/// episode after `watched_episodes`.
///
/// MAL reports a watched count, while Tsuzuki stores the episode to resume.
/// A successful sync therefore has a local consequence when MAL is ahead:
/// if MAL says episode 9 is watched, the local cursor should land on episode
/// 10 at position zero instead of continuing to resume an older episode.
///
/// The ordered file list remains authoritative. Anitomy only identifies the
/// first entry whose episode number is greater than MAL's watched count; it
/// never reorders the list. The cursor moves only when that entry is later
/// than the currently stored file, so sync can never regress local progress.
/// Paused records stay paused, and dropped records remain deliberate dead
/// ends. If the next episode is not on disk yet, the record is left alone;
/// the retained `mal_episode_awareness` lets a later sync try again after new
/// files arrive.
fn advance_local_cursor(
    config: &Config,
    state: &mut State,
    root: &str,
    name: &str,
    watched_episodes: u32,
    home: Option<&Path>,
) -> bool {
    let Some(record_index) = state
        .series
        .iter()
        .position(|record| record.root == root && record.name == name)
    else {
        return false;
    };
    if state.series[record_index].status == Status::Dropped {
        return false;
    }

    let expanded_root = expand_tilde(Path::new(root), home);
    let series_path = expanded_root.join(name);
    let excluded = exclude::excluded_paths_for_root(config, root, home);
    let Ok(ordered) =
        series::ordered_episodes_excluding(&series_path, &config.roots.video_extensions, &excluded)
    else {
        return false;
    };

    let current_path = expanded_root.join(&state.series[record_index].current_relative_path);
    let current_real = normalize(&current_path, home);
    let Some(current_index) = ordered
        .iter()
        .position(|path| normalize(path, home) == current_real)
    else {
        return false;
    };

    let identities = anitomy::episode_identity(&ordered);
    let Some(next_index) = identities
        .iter()
        .position(|identity| identity.episode_number > watched_episodes)
    else {
        return false;
    };
    if next_index <= current_index {
        return false;
    }

    let next_relative = ordered[next_index]
        .strip_prefix(&expanded_root)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| name.to_string());
    let record = &mut state.series[record_index];
    record.current_relative_path = next_relative;
    record.position_seconds = 0.0;
    record.episode_number = Some(identities[next_index].episode_number);
    record.updated_at = now_unix();
    true
}

/// Applies the local-cursor side of every successful sync result.
///
/// `NoOp` is intentionally included: releases before v0.1.1 could persist
/// the higher MAL awareness without advancing the cursor, so the next sync
/// must repair that stale state even though the episode-count merge itself
/// now has nothing left to change.
pub fn apply_synced_progress(
    config: &Config,
    state: &mut State,
    outcomes: &[SeriesOutcome],
    home: Option<&Path>,
) {
    for outcome in outcomes {
        let OutcomeKind::Merged { action, .. } = &outcome.kind else {
            continue;
        };
        let watched_episodes = match *action {
            MergeAction::Push(n) | MergeAction::Pull(n) => n,
            MergeAction::NoOp => state
                .find(&outcome.root, &outcome.name)
                .map(local_effective_episode)
                .unwrap_or(0),
        };
        advance_local_cursor(
            config,
            state,
            &outcome.root,
            &outcome.name,
            watched_episodes,
            home,
        );
    }
}

/// Runs the decision and, only for `Push`, the actual PATCH — a merge that
/// resolves to `Pull`/`NoOp` never calls the network a second time (the
/// `mal` number was already learned via whichever GET the caller used to
/// resolve it).
fn merge_and_maybe_push(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    mal_id: u32,
    local: u32,
    mal: u32,
) -> Result<MergeAction, SyncApiError> {
    let action = decide_merge(local, mal);
    if let MergeAction::Push(n) = action {
        push_episode_count(client, bearer_token, mal_id, n)?;
    }
    Ok(action)
}

/// Applies a resolved merge action to `state` — updates
/// `mal_episode_awareness` to the now-agreed-upon number and bumps
/// `updated_at`, but **only** for `Push`/`Pull` (design.md's "equal →
/// no-op" means no-op all the way down to storage: a genuine no-op never
/// touches `state.toml` at all, matching this project's existing "never
/// touch saved state on a no-op" precedent everywhere else). Cursor
/// alignment is a separate post-merge step: [`advance_local_cursor`] may
/// move to the first unwatched local episode at position zero, but this
/// function never invents a playback position for a watched episode.
fn apply_merge_result(state: &mut State, root: &str, name: &str, action: MergeAction) {
    let new_awareness = match action {
        MergeAction::Push(n) | MergeAction::Pull(n) => n,
        MergeAction::NoOp => return,
    };
    if let Some(record) = state.find_mut(root, name) {
        record.mal_episode_awareness = Some(new_awareness);
        record.updated_at = now_unix();
    }
}

// --- Manual link: `tsuzuki sync link <series> --mal-id <id>` ---

#[derive(Debug)]
pub enum ManualLinkError {
    /// The given ID doesn't appear on the user's own MAL list at all.
    NotOnList,
    Api(SyncApiError),
}

impl fmt::Display for ManualLinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManualLinkError::NotOnList => write!(
                f,
                "that MAL id is not on your MyAnimeList list — add it on MAL first, or double-check the id"
            ),
            ManualLinkError::Api(e) => write!(f, "{e}"),
        }
    }
}

/// Judgment call (flagged per this phase's own instructions, not silently
/// resolved): **yes, verify the given id against the user's real MAL list
/// before persisting it.** Matches this project's existing precedent for
/// explicit user input (CLAUDE.md's "`tsuzuki add` validation and
/// idempotency": `add` validates a path exists at add-time rather than
/// deferring to `doctor`) — a manual link is exactly this shape of input,
/// and a typo'd id would otherwise persist silently and only surface much
/// later as a confusing per-series merge failure (or worse, a real PATCH
/// against a wrong, unrelated anime). One extra GET at link time (rare —
/// this is a manual override path, not the daily reflex command) buys a
/// link the merge step can trust unconditionally forever after.
pub fn manual_link(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    state: &mut State,
    root: &str,
    name: &str,
    mal_id: u32,
) -> Result<(), ManualLinkError> {
    let status = fetch_anime_status(client, bearer_token, mal_id).map_err(ManualLinkError::Api)?;
    if !status.on_list {
        return Err(ManualLinkError::NotOnList);
    }
    upsert_link(state, root, name, mal_id);
    Ok(())
}

fn upsert_link(state: &mut State, root: &str, name: &str, mal_id: u32) {
    if let Some(link) = state.find_mal_link_mut(root, name) {
        link.mal_id = mal_id;
    } else {
        state.mal_links.push(MalLink {
            root: root.to_string(),
            name: name.to_string(),
            mal_id,
        });
    }
}

// --- Bare `tsuzuki sync`: link-if-needed then merge, per tracked series ---

#[derive(Debug, Clone, PartialEq)]
pub enum OutcomeKind {
    /// Linked (just now, or already) and merged.
    Merged { mal_id: u32, action: MergeAction },
    /// Not yet linked, and no exact title match was found. `suggestion` is
    /// `Some` only when the best candidate cleared `fuzzy_match_threshold`
    /// — see [`best_candidate`]. Never more than one: a list of weak
    /// candidates is noise, not a useful "did you mean" prompt.
    NoExactMatch { suggestion: Option<Candidate> },
    /// Not yet linked, and the user's MAL list is completely empty.
    EmptyList,
    /// Any failure — network, API rejection, parse error — for this series
    /// specifically. Never aborts the rest of the run.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesOutcome {
    pub root: String,
    pub name: String,
    pub kind: OutcomeKind,
}

/// Returns the state records eligible for a MAL progress merge.
///
/// Pausing is an explicit request to preserve a series' local progress
/// without MAL changing it (and without that stale local cursor changing
/// MAL). An orphaned record is inert everywhere outside `doctor`, so its
/// series directory must also exist at sync time. Dropped records remain
/// eligible when their directory is still present.
pub fn eligible_targets(
    state: &State,
    config: &Config,
    home: Option<&Path>,
) -> Vec<(String, String)> {
    state
        .series
        .iter()
        .filter(|record| {
            record.status != Status::Paused
                && config.roots.paths.contains(&record.root)
                && expand_tilde(Path::new(&record.root), home)
                    .join(&record.name)
                    .is_dir()
        })
        .map(|record| (record.root.clone(), record.name.clone()))
        .collect()
}

/// Runs `sync` across every `(root, name)` in `targets` — design.md §7 +
/// this phase's own instructions: attempt linking first if not already
/// linked, then merge if linked; one series failing never aborts the rest.
/// `targets` is expected to come from [`eligible_targets`]: a never-started
/// series has no record, while paused and orphaned records are deliberately
/// excluded before this network-facing layer runs.
///
/// The bulk list (needed only for not-yet-linked series) is fetched **at
/// most once** per run, shared across every series that needs it — not
/// once per series. If that one fetch fails, every not-yet-linked series in
/// this run fails with that same underlying reason, but already-linked
/// series are entirely unaffected (they never touch the bulk list at all).
pub fn run_sync(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    state: &mut State,
    targets: &[(String, String)],
    delay: Duration,
    fuzzy_match_threshold: f64,
) -> Vec<SeriesOutcome> {
    if targets.is_empty() {
        return Vec::new();
    }

    let needs_list = targets
        .iter()
        .any(|(root, name)| state.find_mal_link(root, name).is_none());
    let list: Option<Result<Vec<AnimeListEntry>, SyncApiError>> = if needs_list {
        Some(fetch_anime_list(client, bearer_token, delay))
    } else {
        None
    };

    let mut outcomes = Vec::with_capacity(targets.len());
    let mut is_first_call = list.is_none(); // the list fetch (if any) already used the first slot

    for (root, name) in targets {
        let local = state
            .find(root, name)
            .map(local_effective_episode)
            .unwrap_or(0);

        let mal_id = match state.find_mal_link(root, name).map(|l| l.mal_id) {
            Some(id) => id,
            None => match &list {
                Some(Ok(entries)) => {
                    if entries.is_empty() {
                        outcomes.push(SeriesOutcome {
                            root: root.clone(),
                            name: name.clone(),
                            kind: OutcomeKind::EmptyList,
                        });
                        continue;
                    }
                    match find_exact_match(name, entries) {
                        Some(entry) => {
                            let id = entry.id;
                            upsert_link(state, root, name, id);
                            // A freshly-linked entry already carries
                            // num_episodes_watched from the same list
                            // fetch — merge immediately, no extra GET.
                            let mal = entry.num_episodes_watched;
                            throttle(&mut is_first_call, delay);
                            match merge_and_maybe_push(client, bearer_token, id, local, mal) {
                                Ok(action) => {
                                    apply_merge_result(state, root, name, action);
                                    outcomes.push(SeriesOutcome {
                                        root: root.clone(),
                                        name: name.clone(),
                                        kind: OutcomeKind::Merged { mal_id: id, action },
                                    });
                                }
                                Err(e) => outcomes.push(SeriesOutcome {
                                    root: root.clone(),
                                    name: name.clone(),
                                    kind: OutcomeKind::Failed(e.to_string()),
                                }),
                            }
                            continue;
                        }
                        None => {
                            outcomes.push(SeriesOutcome {
                                root: root.clone(),
                                name: name.clone(),
                                kind: OutcomeKind::NoExactMatch {
                                    suggestion: best_candidate(
                                        name,
                                        entries,
                                        fuzzy_match_threshold,
                                    ),
                                },
                            });
                            continue;
                        }
                    }
                }
                Some(Err(e)) => {
                    outcomes.push(SeriesOutcome {
                        root: root.clone(),
                        name: name.clone(),
                        kind: OutcomeKind::Failed(e.to_string()),
                    });
                    continue;
                }
                None => unreachable!(
                    "needs_list is true whenever any target is unlinked, so `list` is Some here"
                ),
            },
        };

        // Already linked (from a prior run, or a manual `sync link`) —
        // learn MAL's current count via a single per-anime lookup, never
        // the bulk list.
        throttle(&mut is_first_call, delay);
        let result = fetch_anime_status(client, bearer_token, mal_id).and_then(|status| {
            merge_and_maybe_push(
                client,
                bearer_token,
                mal_id,
                local,
                status.num_episodes_watched,
            )
        });
        match result {
            Ok(action) => {
                apply_merge_result(state, root, name, action);
                outcomes.push(SeriesOutcome {
                    root: root.clone(),
                    name: name.clone(),
                    kind: OutcomeKind::Merged { mal_id, action },
                });
            }
            Err(e) => outcomes.push(SeriesOutcome {
                root: root.clone(),
                name: name.clone(),
                kind: OutcomeKind::Failed(e.to_string()),
            }),
        }
    }

    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mal::HttpError;
    use std::cell::RefCell;
    use std::fs;
    use tempfile::tempdir;

    /// A representative `fuzzy_match_threshold`, used everywhere `run_sync`
    /// is exercised end-to-end in this module's tests unless a test is
    /// specifically about the threshold boundary itself.
    const TEST_FUZZY_MATCH_THRESHOLD: f64 = 0.8;

    // --- Title normalization ---

    #[test]
    fn normalize_strips_brackets_case_and_whitespace() {
        assert_eq!(normalize_title("One Piece"), "one piece");
        assert_eq!(normalize_title("  One   Piece  "), "one piece");
        assert_eq!(normalize_title("ONE PIECE"), "one piece");
        assert_eq!(normalize_title("One Piece (1999)"), "one piece");
        assert_eq!(normalize_title("[SubsPlease] One Piece"), "one piece");
        assert_eq!(normalize_title("One Piece {TV}"), "one piece");
        assert_eq!(
            normalize_title("One (nested (deep) stuff) Piece"),
            "one piece"
        );
    }

    #[test]
    fn normalize_handles_unbalanced_brackets_without_panicking() {
        // Defensive: a stray closing bracket must not underflow/panic.
        assert_eq!(normalize_title("One Piece)"), "one piece");
        assert_eq!(normalize_title("(One Piece"), "");
    }

    // --- Exact-match linking ---

    fn entry(id: u32, main: &str, en: &str, ja: &str, synonyms: &[&str]) -> AnimeListEntry {
        AnimeListEntry {
            id,
            titles: AnimeTitles {
                main: main.to_string(),
                en: en.to_string(),
                ja: ja.to_string(),
                synonyms: synonyms.iter().map(|s| s.to_string()).collect(),
            },
            num_episodes_watched: 0,
        }
    }

    #[test]
    fn exact_match_on_main_title_case_and_whitespace_insensitive() {
        let entries = vec![entry(1, "One Piece", "", "", &[])];
        assert_eq!(
            find_exact_match("  ONE piece ", &entries).map(|e| e.id),
            Some(1)
        );
    }

    #[test]
    fn exact_match_on_english_title() {
        let entries = vec![entry(1, "Shingeki no Kyojin", "Attack on Titan", "", &[])];
        assert_eq!(
            find_exact_match("Attack on Titan", &entries).map(|e| e.id),
            Some(1)
        );
    }

    #[test]
    fn exact_match_on_japanese_title() {
        let entries = vec![entry(1, "One Piece", "", "ワンピース", &[])];
        assert_eq!(
            find_exact_match("ワンピース", &entries).map(|e| e.id),
            Some(1)
        );
    }

    #[test]
    fn exact_match_on_a_synonym() {
        let entries = vec![entry(1, "Cowboy Bebop", "", "", &["Kaubōi Bibappu"])];
        assert_eq!(
            find_exact_match("Kaubōi Bibappu", &entries).map(|e| e.id),
            Some(1)
        );
    }

    #[test]
    fn exact_match_ignores_bracketed_annotations_on_the_local_side() {
        let entries = vec![entry(1, "One Piece", "", "", &[])];
        assert_eq!(
            find_exact_match("[Erai-raws] One Piece (2023)", &entries).map(|e| e.id),
            Some(1)
        );
    }

    #[test]
    fn no_exact_match_returns_none_not_a_fuzzy_guess() {
        let entries = vec![entry(1, "One Piece", "", "", &[])];
        assert_eq!(find_exact_match("One Piece: Special", &entries), None);
        assert_eq!(find_exact_match("Two Piece", &entries), None);
    }

    // --- Similarity ranking (display-only) ---

    #[test]
    fn identical_titles_have_similarity_one() {
        assert_eq!(title_similarity("One Piece", "one piece"), 1.0);
    }

    #[test]
    fn rank_candidates_orders_by_similarity_and_truncates() {
        let entries = vec![
            entry(1, "One Piece", "", "", &[]),
            entry(2, "One Punch Man", "", "", &[]),
            entry(3, "Totally Unrelated Show", "", "", &[]),
        ];
        let ranked = rank_candidates("One Piece:", &entries, 2);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].mal_id, 1, "the near-exact title must rank first");
        assert!(ranked[0].similarity > ranked[1].similarity);
    }

    #[test]
    fn rank_candidates_never_auto_selects_only_displays() {
        // rank_candidates has no way to "decide" anything — it's a plain
        // Vec, the caller (run_sync) never reads a ranked list as a link
        // decision anywhere in this module. This test just pins the
        // contract: the top-ranked candidate is not automatically equal to
        // an exact match outcome.
        let entries = vec![entry(1, "One Piece Special", "", "", &[])];
        assert_eq!(find_exact_match("One Piece", &entries), None);
        let ranked = rank_candidates("One Piece", &entries, 5);
        assert_eq!(ranked.len(), 1);
    }

    // --- best_candidate: the threshold gate on what's worth suggesting ---

    #[test]
    fn best_candidate_is_none_when_the_top_result_is_below_threshold() {
        let entries = vec![entry(1, "Totally Unrelated Show", "", "", &[])];
        assert_eq!(best_candidate("One Piece", &entries, 0.8), None);
    }

    #[test]
    fn best_candidate_is_some_when_the_top_result_clears_threshold() {
        let entries = vec![entry(1, "One Piece!", "", "", &[])];
        let candidate = best_candidate("One Piece", &entries, 0.8).unwrap();
        assert_eq!(candidate.mal_id, 1);
    }

    #[test]
    fn best_candidate_boundary_is_inclusive() {
        let entries = vec![entry(1, "abcde", "", "", &[])];
        // Exactly 0.8 similarity to "abcd" (1 edit / 5 chars) — must pass a
        // threshold of exactly 0.8, not require strictly greater than.
        assert_eq!(title_similarity("abcd", "abcde"), 0.8);
        assert!(best_candidate("abcd", &entries, 0.8).is_some());
    }

    // --- Merge decision table (CLAUDE.md §2's core invariant) ---

    #[test]
    fn merge_local_greater_than_mal_pushes_local() {
        assert_eq!(decide_merge(10, 5), MergeAction::Push(10));
        assert_eq!(decide_merge(1, 0), MergeAction::Push(1));
    }

    #[test]
    fn merge_mal_greater_than_local_pulls_mal() {
        assert_eq!(decide_merge(5, 10), MergeAction::Pull(10));
        assert_eq!(decide_merge(0, 1), MergeAction::Pull(1));
    }

    #[test]
    fn merge_equal_is_noop() {
        assert_eq!(decide_merge(0, 0), MergeAction::NoOp);
        assert_eq!(decide_merge(42, 42), MergeAction::NoOp);
    }

    #[test]
    fn merge_boundary_values() {
        assert_eq!(
            decide_merge(u32::MAX, u32::MAX - 1),
            MergeAction::Push(u32::MAX)
        );
        assert_eq!(
            decide_merge(u32::MAX - 1, u32::MAX),
            MergeAction::Pull(u32::MAX)
        );
        assert_eq!(decide_merge(u32::MAX, u32::MAX), MergeAction::NoOp);
    }

    // --- local_effective_episode: mal_episode_awareness participates as "local" ---

    fn base_record() -> SeriesRecord {
        SeriesRecord {
            root: "~/Anime".to_string(),
            name: "One Piece".to_string(),
            status: Status::InProgress,
            current_relative_path: "One Piece/ep09.mkv".to_string(),
            position_seconds: 100.0,
            episode_number: Some(9),
            updated_at: 1,
            mal_episode_awareness: None,
        }
    }

    #[test]
    fn local_effective_uses_episode_number_minus_one_when_no_prior_mal_awareness() {
        // base_record is InProgress, cursor = episode 9 — that episode
        // itself is still being watched, so only 8 count as watched.
        let record = base_record();
        assert_eq!(local_effective_episode(&record), 8);
    }

    #[test]
    fn local_effective_uses_the_higher_of_episode_number_minus_one_and_mal_awareness() {
        let mut record = base_record();
        record.mal_episode_awareness = Some(20);
        assert_eq!(
            local_effective_episode(&record),
            20,
            "a prior MAL-driven pull must not be forgotten just because the local file position is lower"
        );
    }

    #[test]
    fn local_effective_subtracts_one_for_a_paused_cursor_too() {
        let mut record = base_record();
        record.status = Status::Paused;
        assert_eq!(local_effective_episode(&record), 8);
    }

    #[test]
    fn local_effective_uses_cursor_as_is_when_dropped_on_a_completed_finale() {
        // The apply_completion_advance terminal case: no next episode, so
        // reconcile leaves the cursor sitting on the finale it just
        // confirmed complete (a real, nonzero position) and transitions
        // straight to Dropped. That finale genuinely was watched in full.
        let mut record = base_record();
        record.status = Status::Dropped;
        record.position_seconds = 1380.0; // near-the-end position, not 0
        assert_eq!(local_effective_episode(&record), 9);
    }

    #[test]
    fn local_effective_still_subtracts_one_when_dropped_at_a_fresh_untouched_cursor() {
        // Dropped right after an auto-advance to the next (unwatched)
        // episode, or dropped before ever really opening the cursor episode
        // — position_seconds == 0.0 means the cursor episode was never
        // touched, so it still doesn't count as watched.
        let mut record = base_record();
        record.status = Status::Dropped;
        record.position_seconds = 0.0;
        assert_eq!(local_effective_episode(&record), 8);
    }

    #[test]
    fn synced_mal_progress_advances_the_local_resume_cursor() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_dir = root.join("Shokei Shoujo no Virgin Road");
        fs::create_dir_all(&series_dir).unwrap();
        for episode in 1..=12 {
            fs::File::create(series_dir.join(format!(
                "Shokei Shoujo no Virgin Road - S01E{episode:02}.mkv"
            )))
            .unwrap();
        }

        let root_string = root.to_string_lossy().into_owned();
        let mut config = Config::default();
        config.roots.paths.push(root_string.clone());
        let mut record = base_record();
        record.root.clone_from(&root_string);
        record.name = "Shokei Shoujo no Virgin Road".to_string();
        record.current_relative_path =
            format!("{}/Shokei Shoujo no Virgin Road - S01E07.mkv", record.name);
        record.position_seconds = 600.0;
        record.episode_number = Some(7);
        // This reproduces an already-synced state written by v0.1.0: MAL
        // awareness moved to 9, but the local cursor incorrectly stayed 7.
        record.mal_episode_awareness = Some(9);
        let mut state = State {
            series: vec![record],
            mal_links: Vec::new(),
        };

        let outcomes = vec![SeriesOutcome {
            root: root_string.clone(),
            name: "Shokei Shoujo no Virgin Road".to_string(),
            kind: OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::NoOp,
            },
        }];
        apply_synced_progress(&config, &mut state, &outcomes, Some(dir.path()));

        let record = state
            .find(&root_string, "Shokei Shoujo no Virgin Road")
            .unwrap();
        assert_eq!(
            record.current_relative_path,
            "Shokei Shoujo no Virgin Road/Shokei Shoujo no Virgin Road - S01E10.mkv"
        );
        assert_eq!(record.position_seconds, 0.0);
        assert_eq!(record.episode_number, Some(10));
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.mal_episode_awareness, Some(9));
    }

    #[test]
    fn synced_mal_progress_never_moves_the_local_cursor_backward() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_dir = root.join("One Piece");
        fs::create_dir_all(&series_dir).unwrap();
        for episode in 1..=10 {
            fs::File::create(series_dir.join(format!("One Piece - {episode:02}.mkv"))).unwrap();
        }

        let root_string = root.to_string_lossy().into_owned();
        let mut config = Config::default();
        config.roots.paths.push(root_string.clone());
        let mut record = base_record();
        record.root.clone_from(&root_string);
        record.current_relative_path = "One Piece/One Piece - 10.mkv".to_string();
        record.position_seconds = 500.0;
        record.episode_number = Some(10);
        let before = record.clone();
        let mut state = State {
            series: vec![record],
            mal_links: Vec::new(),
        };

        assert!(!advance_local_cursor(
            &config,
            &mut state,
            &root_string,
            "One Piece",
            5,
            Some(dir.path()),
        ));
        assert_eq!(state.series[0], before);
    }

    #[test]
    fn paused_and_orphaned_series_are_excluded_from_sync_targets() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        for name in ["Watching", "Local Rewatch", "Finished"] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        let root_string = root.to_string_lossy().into_owned();
        let mut config = Config::default();
        config.roots.paths.push(root_string.clone());

        let mut in_progress = base_record();
        in_progress.root.clone_from(&root_string);
        in_progress.name = "Watching".to_string();

        let mut paused = base_record();
        paused.root.clone_from(&root_string);
        paused.name = "Local Rewatch".to_string();
        paused.status = Status::Paused;

        let mut dropped = base_record();
        dropped.root.clone_from(&root_string);
        dropped.name = "Finished".to_string();
        dropped.status = Status::Dropped;

        let mut orphaned = base_record();
        orphaned.root.clone_from(&root_string);
        orphaned.name = "Deleted".to_string();

        let state = State {
            series: vec![in_progress, paused, dropped, orphaned],
            mal_links: Vec::new(),
        };

        assert_eq!(
            eligible_targets(&state, &config, Some(dir.path())),
            vec![
                (root_string.clone(), "Watching".to_string()),
                (root_string, "Finished".to_string()),
            ]
        );
    }

    // --- THE non-destructive invariant test (verification item 5) ---
    //
    // Fabricates exactly the case a naive implementation gets wrong: a
    // series was already pulled up to episode 20 via a prior MAL sync
    // (mal_episode_awareness = 20), but the raw local anitomy-derived
    // episode_number still reads only 15 (e.g. reconcile hasn't observed
    // any newer local playback since). A naive implementation comparing
    // raw episode_number (15) against MAL's current count would read
    // "MAL ahead" or "local behind" incorrectly and could push 15 back to
    // MAL, silently overwriting the real, higher, already-confirmed 20.
    // This must never happen — decide_merge always sees the *effective*
    // local number, so the fake would-push-15 case can't arise, and the
    // FakeClient below would panic if a PATCH were ever attempted.

    #[test]
    fn never_pushes_a_smaller_number_when_a_stale_local_read_is_lower_than_established_awareness() {
        let effective_local = local_effective_episode(&{
            let mut r = base_record();
            r.episode_number = Some(15); // stale/lower raw local read
            r.mal_episode_awareness = Some(20); // already-established higher number
            r
        });
        // The naive bug this guards against: comparing raw episode_number
        // (15) directly against MAL's number would look like "local < MAL"
        // for a MAL count anywhere in 16..=19, incorrectly triggering a
        // *pull* that discards the already-known 20. Using the effective
        // number instead correctly reads "local ahead" and would push 20,
        // never a smaller number, for a MAL count in that same range.
        assert_eq!(effective_local, 20);
        assert_eq!(decide_merge(effective_local, 18), MergeAction::Push(20));
        assert_eq!(decide_merge(effective_local, 20), MergeAction::NoOp);
        assert_eq!(decide_merge(effective_local, 25), MergeAction::Pull(25));
    }

    #[test]
    fn run_sync_never_patches_a_smaller_number_than_mal_already_has() {
        // End-to-end version of the same invariant, through run_sync
        // itself: a linked series whose stale local number (15) is below
        // MAL's real current count (18) must PULL, never push — the fake
        // client panics if patch_form is ever called with anything below
        // 18, proving the push path is genuinely never taken here.
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":42,"title":"Test Anime","my_list_status":{"num_episodes_watched":18}}"#,
        );

        let mut state = State {
            series: vec![{
                let mut r = base_record();
                r.episode_number = Some(15);
                r.mal_episode_awareness = None;
                r
            }],
            mal_links: vec![MalLink {
                root: "~/Anime".to_string(),
                name: "One Piece".to_string(),
                mal_id: 42,
            }],
        };

        let outcomes = run_sync(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
        );

        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::Pull(18)
            }
        );
        assert!(
            client.patch_calls.borrow().is_empty(),
            "must never PATCH — MAL was already ahead, so nothing may be pushed"
        );
        assert_eq!(
            state
                .find("~/Anime", "One Piece")
                .unwrap()
                .mal_episode_awareness,
            Some(18)
        );
    }

    // --- Manual link (`sync link`), including the validate-before-persist judgment call ---

    #[test]
    fn manual_link_persists_when_the_id_is_on_the_users_list() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":100,"title":"Test Anime","my_list_status":{"num_episodes_watched":3}}"#,
        );

        let mut state = State::default();
        manual_link(&client, "token", &mut state, "~/Anime", "Monster", 100).unwrap();

        assert_eq!(
            state.find_mal_link("~/Anime", "Monster").unwrap().mal_id,
            100
        );
    }

    #[test]
    fn manual_link_rejects_and_does_not_persist_an_id_not_on_the_list() {
        // MAL returns the anime, but with no my_list_status at all — a
        // real, valid MAL id that simply isn't on this user's list.
        let client = FakeClient::new();
        client.push_status_response(200, r#"{"id":999,"title":"Some Anime"}"#);

        let mut state = State::default();
        let result = manual_link(&client, "token", &mut state, "~/Anime", "Monster", 999);

        assert!(matches!(result, Err(ManualLinkError::NotOnList)));
        assert!(
            state.find_mal_link("~/Anime", "Monster").is_none(),
            "an unverified id must never be persisted"
        );
    }

    #[test]
    fn manual_link_overwrites_an_existing_link_for_the_same_series() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":200,"title":"Test Anime","my_list_status":{"num_episodes_watched":0}}"#,
        );

        let mut state = State {
            series: Vec::new(),
            mal_links: vec![MalLink {
                root: "~/Anime".to_string(),
                name: "Monster".to_string(),
                mal_id: 100,
            }],
        };
        manual_link(&client, "token", &mut state, "~/Anime", "Monster", 200).unwrap();

        assert_eq!(
            state.mal_links.len(),
            1,
            "must update in place, not duplicate"
        );
        assert_eq!(
            state.find_mal_link("~/Anime", "Monster").unwrap().mal_id,
            200
        );
    }

    // --- run_sync: no-match / empty-list outcomes ---

    #[test]
    fn run_sync_reports_no_exact_match_with_no_suggestion_when_nothing_clears_the_threshold() {
        let client = FakeClient::new();
        // "One Piece" vs "One Piece Special" — related-looking, but the
        // similarity a naive reader might expect is well under a reasonable
        // confidence bar (~0.53 here). This is exactly the noisy-suggestion
        // shape this project's own dogfooding flagged: a weak candidate is
        // worse than no candidate at all, so it must not surface.
        client.push_list_response(
            200,
            r#"{"data":[{"node":{"id":5,"title":"One Piece Special","alternative_titles":{"en":"","ja":"","synonyms":[]},"my_list_status":{"num_episodes_watched":0}}}]}"#,
        );

        let mut state = State {
            series: vec![base_record()],
            mal_links: Vec::new(),
        };
        let outcomes = run_sync(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
        );

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0].kind {
            OutcomeKind::NoExactMatch { suggestion } => {
                assert_eq!(*suggestion, None, "a weak candidate must not be suggested")
            }
            other => panic!("expected NoExactMatch, got {other:?}"),
        }
        assert!(state.find_mal_link("~/Anime", "One Piece").is_none());
    }

    #[test]
    fn run_sync_reports_no_exact_match_with_a_confident_suggestion() {
        let client = FakeClient::new();
        // "One Piece" vs "One Piece!" — a single extra character, similarity
        // 0.9, comfortably above TEST_FUZZY_MATCH_THRESHOLD (0.8).
        client.push_list_response(
            200,
            r#"{"data":[{"node":{"id":5,"title":"One Piece!","alternative_titles":{"en":"","ja":"","synonyms":[]},"my_list_status":{"num_episodes_watched":0}}}]}"#,
        );

        let mut state = State {
            series: vec![base_record()],
            mal_links: Vec::new(),
        };
        let outcomes = run_sync(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
        );

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0].kind {
            OutcomeKind::NoExactMatch { suggestion } => {
                let suggestion = suggestion
                    .as_ref()
                    .expect("expected a confident suggestion");
                assert_eq!(suggestion.mal_id, 5);
                assert_eq!(suggestion.title, "One Piece!");
            }
            other => panic!("expected NoExactMatch, got {other:?}"),
        }
        // Never auto-linked — a suggestion is display-only, `sync link`
        // is still required to actually confirm it.
        assert!(state.find_mal_link("~/Anime", "One Piece").is_none());
    }

    #[test]
    fn run_sync_reports_empty_list_plainly() {
        let client = FakeClient::new();
        client.push_list_response(200, r#"{"data":[]}"#);

        let mut state = State {
            series: vec![base_record()],
            mal_links: Vec::new(),
        };
        let outcomes = run_sync(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
        );

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].kind, OutcomeKind::EmptyList);
    }

    // --- run_sync: auto-linking + immediate merge from the same list fetch ---

    #[test]
    fn run_sync_auto_links_on_exact_match_and_merges_without_a_second_lookup() {
        let client = FakeClient::new();
        client.push_list_response(
            200,
            r#"{"data":[{"node":{"id":7,"title":"One Piece","alternative_titles":{"en":"","ja":"","synonyms":[]},"my_list_status":{"num_episodes_watched":3}}}]}"#,
        );

        let mut record = base_record();
        record.episode_number = Some(9); // cursor 9, still in progress -> watched 8, ahead of MAL's 3
        let mut state = State {
            series: vec![record],
            mal_links: Vec::new(),
        };
        let outcomes = run_sync(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
        );

        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 7,
                action: MergeAction::Push(8)
            }
        );
        assert_eq!(
            state.find_mal_link("~/Anime", "One Piece").unwrap().mal_id,
            7
        );
        // Only the list fetch happened — no extra per-anime GET for a
        // series linked this same run.
        assert_eq!(client.get_calls.borrow().len(), 1);
        assert_eq!(client.patch_calls.borrow().len(), 1);
    }

    // --- Partial-failure continuation (verification item: one bad entry doesn't abort the rest) ---

    #[test]
    fn one_series_failing_does_not_abort_or_corrupt_the_rest_of_the_run() {
        let client = FakeClient::new();
        // Both already linked. The first anime-status lookup (id 1) fails
        // outright; the second (id 2) succeeds.
        client.push_status_response(500, "boom");
        client.push_status_response(
            200,
            r#"{"id":2,"title":"Test Anime","my_list_status":{"num_episodes_watched":50}}"#,
        );

        let mut first = base_record();
        first.name = "Failing Show".to_string();
        first.episode_number = Some(5);
        let mut second = base_record();
        second.name = "Working Show".to_string();
        second.episode_number = Some(5);

        let mut state = State {
            series: vec![first, second],
            mal_links: vec![
                MalLink {
                    root: "~/Anime".to_string(),
                    name: "Failing Show".to_string(),
                    mal_id: 1,
                },
                MalLink {
                    root: "~/Anime".to_string(),
                    name: "Working Show".to_string(),
                    mal_id: 2,
                },
            ],
        };

        let outcomes = run_sync(
            &client,
            "token",
            &mut state,
            &[
                ("~/Anime".to_string(), "Failing Show".to_string()),
                ("~/Anime".to_string(), "Working Show".to_string()),
            ],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
        );

        assert_eq!(
            outcomes.len(),
            2,
            "both series must be reported, not just the successful one"
        );
        assert!(matches!(outcomes[0].kind, OutcomeKind::Failed(_)));
        assert_eq!(
            outcomes[1].kind,
            OutcomeKind::Merged {
                mal_id: 2,
                action: MergeAction::Pull(50)
            }
        );

        // The failing series' local state must be completely untouched —
        // no partial/corrupt write from the aborted attempt.
        let failing = state.find("~/Anime", "Failing Show").unwrap();
        assert_eq!(failing.mal_episode_awareness, None);
        // The working series must have genuinely persisted its result.
        let working = state.find("~/Anime", "Working Show").unwrap();
        assert_eq!(working.mal_episode_awareness, Some(50));
    }

    #[test]
    fn list_fetch_failure_fails_every_unlinked_series_but_never_touches_already_linked_ones() {
        let client = FakeClient::new();
        client.push_list_response(500, "server error");
        client.push_status_response(
            200,
            r#"{"id":9,"title":"Test Anime","my_list_status":{"num_episodes_watched":7}}"#,
        );

        let mut unlinked = base_record();
        unlinked.name = "Unlinked Show".to_string();
        let mut linked = base_record();
        linked.name = "Linked Show".to_string();
        linked.episode_number = Some(2); // local behind MAL's 7 -> Pull

        let mut state = State {
            series: vec![unlinked, linked],
            mal_links: vec![MalLink {
                root: "~/Anime".to_string(),
                name: "Linked Show".to_string(),
                mal_id: 9,
            }],
        };

        let outcomes = run_sync(
            &client,
            "token",
            &mut state,
            &[
                ("~/Anime".to_string(), "Unlinked Show".to_string()),
                ("~/Anime".to_string(), "Linked Show".to_string()),
            ],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
        );

        assert_eq!(outcomes.len(), 2);
        assert!(matches!(outcomes[0].kind, OutcomeKind::Failed(_)));
        assert_eq!(
            outcomes[1].kind,
            OutcomeKind::Merged {
                mal_id: 9,
                action: MergeAction::Pull(7)
            }
        );
        assert!(state.find_mal_link("~/Anime", "Unlinked Show").is_none());
        assert_eq!(
            state
                .find("~/Anime", "Linked Show")
                .unwrap()
                .mal_episode_awareness,
            Some(7)
        );
    }

    #[test]
    fn a_genuine_noop_merge_touches_no_state_at_all() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":3,"title":"Test Anime","my_list_status":{"num_episodes_watched":9}}"#,
        );

        let mut record = base_record();
        record.episode_number = Some(10); // cursor 10, still in progress -> watched 9
        record.mal_episode_awareness = Some(3); // irrelevant: max(9,3) == 9 == mal
        let before = record.clone();
        let mut state = State {
            series: vec![record],
            mal_links: vec![MalLink {
                root: "~/Anime".to_string(),
                name: "One Piece".to_string(),
                mal_id: 3,
            }],
        };

        run_sync(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
        );

        assert_eq!(
            state.find("~/Anime", "One Piece").unwrap(),
            &before,
            "a no-op merge must leave the record byte-for-byte unchanged, including updated_at"
        );
        assert!(client.patch_calls.borrow().is_empty());
    }

    #[test]
    fn run_sync_on_empty_targets_makes_no_network_call_at_all() {
        let client = FakeClient::new();
        let mut state = State::default();
        let outcomes = run_sync(
            &client,
            "token",
            &mut state,
            &[],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
        );
        assert!(outcomes.is_empty());
        assert!(client.get_calls.borrow().is_empty());
    }

    // --- Fakeable HTTP client for this module's own tests ---

    /// A small, queue-based fake: each `push_*_response` call queues one
    /// response for the *next* matching call. Also records every call made,
    /// so tests can assert on call counts (e.g. "no second lookup for a
    /// freshly-linked series") and on exactly what a PATCH would have sent
    /// (the non-destructive-invariant tests rely on this never containing a
    /// smaller number than expected).
    struct FakeClient {
        get_responses: RefCell<Vec<(u16, String)>>,
        get_calls: RefCell<Vec<String>>,
        patch_calls: RefCell<Vec<(String, String)>>,
    }

    impl FakeClient {
        fn new() -> Self {
            FakeClient {
                get_responses: RefCell::new(Vec::new()),
                get_calls: RefCell::new(Vec::new()),
                patch_calls: RefCell::new(Vec::new()),
            }
        }

        fn push_list_response(&self, status: u16, body: &str) {
            self.get_responses
                .borrow_mut()
                .push((status, body.to_string()));
        }

        fn push_status_response(&self, status: u16, body: &str) {
            self.get_responses
                .borrow_mut()
                .push((status, body.to_string()));
        }
    }

    impl MalHttpClient for FakeClient {
        fn post_form(
            &self,
            _url: &str,
            _params: &[(&str, &str)],
        ) -> Result<HttpResponse, HttpError> {
            unimplemented!("sync.rs never calls post_form — that's mal.rs's OAuth token endpoint")
        }

        fn get(
            &self,
            url: &str,
            _bearer_token: &str,
            _query: &[(&str, &str)],
        ) -> Result<HttpResponse, HttpError> {
            self.get_calls.borrow_mut().push(url.to_string());
            let mut responses = self.get_responses.borrow_mut();
            if responses.is_empty() {
                panic!("FakeClient: no queued response for get({url})");
            }
            let (status, body) = responses.remove(0);
            Ok(HttpResponse { status, body })
        }

        fn patch_form(
            &self,
            url: &str,
            _bearer_token: &str,
            params: &[(&str, &str)],
        ) -> Result<HttpResponse, HttpError> {
            let body = params
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            self.patch_calls.borrow_mut().push((url.to_string(), body));
            Ok(HttpResponse {
                status: 200,
                body: "{}".to_string(),
            })
        }
    }
}
