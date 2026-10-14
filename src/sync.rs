//! MyAnimeList linking and the two-way episode-progress merge.
//!
//! Series → MAL-id links live in `state.toml` (`state::MalLink`), since
//! Tsuzuki writes them itself. The local side of the merge is
//! [`local_effective_episode`], never the raw `episode_number`.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use chrono::Local;
use serde::Deserialize;

use crate::anitomy;
use crate::config::{Config, SyncExclusion};
use crate::exclude;
use crate::mal::{HttpResponse, MalHttpClient};
use crate::roots::{expand_tilde, normalize, tracked_real_paths};
use crate::series::{self, is_series_dir};
use crate::state::{MalLink, SeriesRecord, State, Status, now_unix};

pub const API_BASE: &str = "https://api.myanimelist.net/v2";

/// Pause between consecutive MAL API calls within one `sync` run. Not a
/// config field; tests pass `Duration::ZERO`.
pub const API_CALL_DELAY: Duration = Duration::from_millis(300);

/// Sleeps `delay` before every call except the first of a call sequence;
/// `is_first` is threaded through the sequence.
fn throttle(is_first: &mut bool, delay: Duration) {
    if *is_first {
        *is_first = false;
    } else {
        std::thread::sleep(delay);
    }
}

// --- Title normalization, exact-match linking, similarity ranking ---

/// Strips bracketed content, lowercases, and collapses whitespace so local
/// folder names and MAL titles compare equal.
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
    pub num_episodes: u32,
    pub status: MalListStatus,
}

/// MAL's anime-list statuses. `Unknown` absorbs unrecognized API values,
/// which sync leaves untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MalListStatus {
    Watching,
    Completed,
    OnHold,
    Dropped,
    PlanToWatch,
    #[default]
    #[serde(other)]
    Unknown,
}

impl MalListStatus {
    fn api_value(self) -> Option<&'static str> {
        match self {
            MalListStatus::Watching => Some("watching"),
            MalListStatus::Completed => Some("completed"),
            MalListStatus::OnHold => Some("on_hold"),
            MalListStatus::Dropped => Some("dropped"),
            MalListStatus::PlanToWatch => Some("plan_to_watch"),
            MalListStatus::Unknown => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            MalListStatus::Watching => "Watching",
            MalListStatus::Completed => "Completed",
            MalListStatus::OnHold => "On Hold",
            MalListStatus::Dropped => "Dropped",
            MalListStatus::PlanToWatch => "Plan to Watch",
            MalListStatus::Unknown => "Unknown",
        }
    }
}

/// Every known title of an entry: main, English, Japanese (when present),
/// and synonyms.
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

/// The entry with a title equal to `local_name` after normalization. Never
/// fuzzy.
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

/// Character-based Levenshtein distance. Only ranks displayed candidates.
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

/// The `top_n` entries closest to `local_name` by title similarity.
/// Display-only; never decides a link.
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

/// The closest candidate, only if its similarity reaches
/// `fuzzy_match_threshold`. A display-only suggestion; `find_exact_match`
/// alone decides links.
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
    status: MalListStatus,
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
    #[serde(default)]
    num_episodes: u32,
}

#[derive(Debug, Deserialize)]
struct AnimeListEntryJson {
    node: AnimeNodeJson,
    /// The user-list endpoint returns this beside `node`; the single-anime
    /// endpoint instead returns `my_list_status` inside the anime object.
    #[serde(default)]
    list_status: Option<MyListStatusJson>,
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

fn to_anime_list_entry(entry: AnimeListEntryJson) -> AnimeListEntry {
    let node = entry.node;
    let (en, ja, synonyms) = match node.alternative_titles {
        Some(t) => (t.en, t.ja, t.synonyms),
        None => (String::new(), String::new(), Vec::new()),
    };
    let (num_episodes_watched, status) = entry
        .list_status
        .or(node.my_list_status)
        .map(|s| (s.num_episodes_watched, s.status))
        .unwrap_or((0, MalListStatus::Unknown));
    AnimeListEntry {
        id: node.id,
        titles: AnimeTitles {
            main: node.title,
            en,
            ja,
            synonyms,
        },
        num_episodes_watched,
        num_episodes: node.num_episodes,
        status,
    }
}

/// Page cap against a looping `paging.next` chain.
const MAX_LIST_PAGES: usize = 200;

/// Fetches the user's full anime list, following `paging.next`. The requested
/// fields cover linking, merging and status transitions, so a freshly linked
/// series needs no second lookup.
pub fn fetch_anime_list(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    delay: Duration,
) -> Result<Vec<AnimeListEntry>, SyncApiError> {
    let mut entries = Vec::new();
    let mut url = format!("{API_BASE}/users/@me/animelist");
    let mut query: Vec<(&str, &str)> = vec![
        ("fields", "alternative_titles,num_episodes,list_status"),
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
            entries.push(to_anime_list_entry(item));
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
    /// Whether `my_list_status` was present, i.e. the anime is on the user's
    /// list.
    pub on_list: bool,
    pub num_episodes_watched: u32,
    pub num_episodes: u32,
    pub status: MalListStatus,
}

/// One anime's list status by ID. Validates `sync link --mal-id` and reads
/// MAL's current progress for a linked series.
pub fn fetch_anime_status(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    mal_id: u32,
) -> Result<AnimeStatus, SyncApiError> {
    let url = format!("{API_BASE}/anime/{mal_id}");
    let resp = client
        .get(
            &url,
            bearer_token,
            &[("fields", "num_episodes,my_list_status")],
        )
        .map_err(|e| SyncApiError::Transport(e.to_string()))?;
    require_success(&resp)?;
    let parsed: AnimeNodeJson =
        serde_json::from_str(&resp.body).map_err(|e| SyncApiError::Parse(e.to_string()))?;
    let on_list = parsed.my_list_status.is_some();
    let (num_episodes_watched, status) = parsed
        .my_list_status
        .map(|s| (s.num_episodes_watched, s.status))
        .unwrap_or((0, MalListStatus::Unknown));
    Ok(AnimeStatus {
        on_list,
        num_episodes_watched,
        num_episodes: parsed.num_episodes,
        status,
    })
}

/// Updates MAL via `PATCH /v2/anime/{id}/my_list_status`.
///
/// The request field is `num_watched_episodes`, while responses report
/// `num_episodes_watched`; MAL silently ignores the wrong name. Episode count,
/// status and dates go in one request so a failure can't apply half. Dates are
/// sent only with a forward status transition.
fn update_mal_entry(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    mal_id: u32,
    num_watched_episodes: Option<u32>,
    status: Option<MalListStatus>,
    start_date: Option<&str>,
    finish_date: Option<&str>,
) -> Result<(), SyncApiError> {
    let url = format!("{API_BASE}/anime/{mal_id}/my_list_status");
    let episode_value = num_watched_episodes.map(|value| value.to_string());
    let mut params = Vec::new();
    if let Some(value) = episode_value.as_deref() {
        params.push(("num_watched_episodes", value));
    }
    if let Some(value) = status.and_then(MalListStatus::api_value) {
        params.push(("status", value));
    }
    if let Some(value) = start_date {
        params.push(("start_date", value));
    }
    if let Some(value) = finish_date {
        params.push(("finish_date", value));
    }
    if params.is_empty() {
        return Ok(());
    }
    let resp = client
        .patch_form(&url, bearer_token, &params)
        .map_err(|e| SyncApiError::Transport(e.to_string()))?;
    require_success(&resp)?;
    Ok(())
}

// --- Merge decision ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeAction {
    /// Local is ahead — push this number to MAL.
    Push(u32),
    /// MAL is ahead — update local awareness only, to this number.
    Pull(u32),
    /// Episode counts already agree, but MAL's list status changed.
    RemoteStatus(MalListStatus),
    NoOp,
}

/// Takes the higher number in either direction; never overwrites down.
pub fn decide_merge(local: u32, mal: u32) -> MergeAction {
    match local.cmp(&mal) {
        std::cmp::Ordering::Greater => MergeAction::Push(local),
        std::cmp::Ordering::Less => MergeAction::Pull(mal),
        std::cmp::Ordering::Equal => MergeAction::NoOp,
    }
}

/// The automatic forward status transition, if any. Never changes
/// `on_hold`/`dropped` or moves a status backward. Finished means MAL's total
/// is reached or the local series is `Completed`.
fn desired_status(
    current: MalListStatus,
    watched: u32,
    total_episodes: u32,
    local_finished: bool,
) -> Option<MalListStatus> {
    if watched == 0 {
        return None;
    }
    let finished = local_finished || (total_episodes > 0 && watched >= total_episodes);
    match (current, finished) {
        (MalListStatus::PlanToWatch, true) | (MalListStatus::Watching, true) => {
            Some(MalListStatus::Completed)
        }
        (MalListStatus::PlanToWatch, false) => Some(MalListStatus::Watching),
        _ => None,
    }
}

/// Start/finish dates for a forward status transition, using the local date
/// captured once per run. A series completed in one run gets both.
fn transition_dates(
    current: MalListStatus,
    next: Option<MalListStatus>,
    sync_date: &str,
) -> (Option<&str>, Option<&str>) {
    match (current, next) {
        (MalListStatus::PlanToWatch, Some(MalListStatus::Watching)) => (Some(sync_date), None),
        (MalListStatus::Watching, Some(MalListStatus::Completed)) => (None, Some(sync_date)),
        (MalListStatus::PlanToWatch, Some(MalListStatus::Completed)) => {
            (Some(sync_date), Some(sync_date))
        }
        _ => (None, None),
    }
}

/// Watched-episode count on the local side of the merge, including
/// `mal_episode_awareness`.
///
/// `episode_number` is a cursor: the episode being watched or just advanced
/// to, not yet finished. MAL counts watched episodes, so the count is
/// `episode_number - 1`. A `Completed` record's cursor stays on the finished
/// finale, so there it is the count itself.
pub fn local_effective_episode(record: &SeriesRecord) -> u32 {
    let cursor = record.episode_number.unwrap_or(0);
    let watched = if record.status == Status::Completed {
        cursor
    } else {
        cursor.saturating_sub(1)
    };
    watched.max(record.mal_episode_awareness.unwrap_or(0))
}

/// Moves the local cursor to the first on-disk episode after
/// `watched_episodes`, at position zero.
///
/// The ordered file list is authoritative; anitomy only finds the first entry
/// numbered above the watched count. The cursor never moves backward. Dropped
/// and completed records are left alone, and so is a record whose next episode
/// isn't on disk yet (`mal_episode_awareness` lets a later sync retry).
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
    if matches!(
        state.series[record_index].status,
        Status::Dropped | Status::Completed
    ) {
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
    record.season_number = identities[next_index].season;
    record.updated_at = now_unix();
    true
}

/// Advances the local cursor for every merged outcome and returns
/// `(root, name, watched)` for each cursor that moved.
///
/// `NoOp` and `RemoteStatus` are included so state left by an older release
/// (awareness raised, cursor not advanced) is repaired.
pub fn apply_synced_progress(
    config: &Config,
    state: &mut State,
    outcomes: &[SeriesOutcome],
    home: Option<&Path>,
) -> Vec<(String, String, u32)> {
    let mut advanced = Vec::new();
    for outcome in outcomes {
        let OutcomeKind::Merged { action, .. } = &outcome.kind else {
            continue;
        };
        let watched_episodes = match *action {
            MergeAction::Push(n) | MergeAction::Pull(n) => n,
            MergeAction::RemoteStatus(_) | MergeAction::NoOp => state
                .find(&outcome.root, &outcome.name)
                .map(local_effective_episode)
                .unwrap_or(0),
        };
        if advance_local_cursor(
            config,
            state,
            &outcome.root,
            &outcome.name,
            watched_episodes,
            home,
        ) {
            advanced.push((outcome.root.clone(), outcome.name.clone(), watched_episodes));
        }
    }
    advanced
}

/// Decides the merge and sends one PATCH covering any episode push and status
/// change. `Pull` and `NoOp` send no episode count.
fn merge_and_maybe_update(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    mal_id: u32,
    local: u32,
    remote: &AnimeStatus,
    local_finished: bool,
    sync_date: &str,
) -> Result<MergeAction, SyncApiError> {
    let merge_action = decide_merge(local, remote.num_episodes_watched);
    let agreed = local.max(remote.num_episodes_watched);
    let status_update = desired_status(remote.status, agreed, remote.num_episodes, local_finished);
    let (start_date, finish_date) = transition_dates(remote.status, status_update, sync_date);
    let episode_update = match merge_action {
        MergeAction::Push(n) => Some(n),
        _ => None,
    };
    update_mal_entry(
        client,
        bearer_token,
        mal_id,
        episode_update,
        status_update,
        start_date,
        finish_date,
    )?;
    if merge_action == MergeAction::NoOp
        && let Some(status) = status_update
    {
        return Ok(MergeAction::RemoteStatus(status));
    }
    Ok(merge_action)
}

/// Records the agreed episode count in `mal_episode_awareness` for
/// `Push`/`Pull`. Other actions leave state untouched. Cursor movement is
/// [`advance_local_cursor`]'s job.
fn apply_merge_result(state: &mut State, root: &str, name: &str, action: MergeAction) {
    let new_awareness = match action {
        MergeAction::Push(n) | MergeAction::Pull(n) => n,
        MergeAction::RemoteStatus(_) | MergeAction::NoOp => return,
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

/// Links `name` to `mal_id` after checking the id is on the user's MAL list,
/// so a mistyped id is rejected here instead of failing later or patching an
/// unrelated anime.
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
    /// Not yet linked, and no exact title match was found. `suggestion` is set
    /// only when the best candidate cleared `fuzzy_match_threshold`.
    NoExactMatch { suggestion: Option<Candidate> },
    /// Not yet linked, and the user's MAL list is completely empty.
    EmptyList,
    /// Any failure for this series; the rest of the run continues.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesOutcome {
    pub root: String,
    pub name: String,
    pub kind: OutcomeKind,
}

/// Outcome of `add_sync_exclusion`.
#[derive(Debug, PartialEq, Eq)]
pub enum SyncExclusionOutcome {
    Added,
    AlreadyExcluded,
}

/// Records that `series` (or, with `None`, the whole `root`) is kept out of
/// MAL sync. `root` is the stored form. Repeating is a no-op.
pub fn add_sync_exclusion(
    config: &mut Config,
    root: &str,
    series: Option<&str>,
) -> SyncExclusionOutcome {
    let entry = SyncExclusion {
        root: root.to_string(),
        series: series.map(str::to_string),
    };
    if config.sync_exclusions.contains(&entry) {
        return SyncExclusionOutcome::AlreadyExcluded;
    }
    config.sync_exclusions.push(entry);
    SyncExclusionOutcome::Added
}

/// True when `name` under `root` (stored form) is excluded directly or
/// through its whole root.
pub fn is_sync_excluded(config: &Config, root: &str, name: &str) -> bool {
    config
        .sync_exclusions
        .iter()
        .any(|e| e.root == root && e.series.as_deref().is_none_or(|s| s == name))
}

/// Records eligible for a MAL merge: not paused or dropped, not sync-excluded,
/// root still tracked, and series directory present. Completed records are
/// included so they reach MAL.
pub fn eligible_targets(
    state: &State,
    config: &Config,
    home: Option<&Path>,
) -> Vec<(String, String)> {
    let real_roots = tracked_real_paths(config, home);
    state
        .series
        .iter()
        .filter(|record| {
            !matches!(record.status, Status::Paused | Status::Dropped)
                && !is_sync_excluded(config, &record.root, &record.name)
                && config.roots.paths.contains(&record.root)
                && is_series_dir(
                    &expand_tilde(Path::new(&record.root), home).join(&record.name),
                    &real_roots,
                )
        })
        .map(|record| (record.root.clone(), record.name.clone()))
        .collect()
}

/// Links (if needed) and merges each `(root, name)` in `targets`; one series
/// failing doesn't abort the rest. `targets` should come from
/// [`eligible_targets`].
///
/// The bulk list is fetched at most once per run, and only if some target is
/// unlinked. If that fetch fails, every unlinked series fails with its error;
/// linked series are unaffected.
pub fn run_sync(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    state: &mut State,
    targets: &[(String, String)],
    delay: Duration,
    fuzzy_match_threshold: f64,
) -> Vec<SeriesOutcome> {
    let sync_date = Local::now().format("%Y-%m-%d").to_string();
    run_sync_on_date(
        client,
        bearer_token,
        state,
        targets,
        delay,
        fuzzy_match_threshold,
        &sync_date,
    )
}

/// [`run_sync`] with the date injected for tests.
fn run_sync_on_date(
    client: &dyn MalHttpClient,
    bearer_token: &str,
    state: &mut State,
    targets: &[(String, String)],
    delay: Duration,
    fuzzy_match_threshold: f64,
    sync_date: &str,
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
    let mut is_first_call = list.is_none(); // the list fetch, if any, used the first slot

    for (root, name) in targets {
        let local = state
            .find(root, name)
            .map(local_effective_episode)
            .unwrap_or(0);
        let local_finished = state
            .find(root, name)
            .is_some_and(|record| record.status == Status::Completed);

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
                            // The list entry already carries the watched
                            // count; no extra GET.
                            let remote = AnimeStatus {
                                on_list: true,
                                num_episodes_watched: entry.num_episodes_watched,
                                num_episodes: entry.num_episodes,
                                status: entry.status,
                            };
                            throttle(&mut is_first_call, delay);
                            match merge_and_maybe_update(
                                client,
                                bearer_token,
                                id,
                                local,
                                &remote,
                                local_finished,
                                sync_date,
                            ) {
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

        // Already linked: read MAL's current progress for this one anime.
        throttle(&mut is_first_call, delay);
        let result = fetch_anime_status(client, bearer_token, mal_id).and_then(|status| {
            merge_and_maybe_update(
                client,
                bearer_token,
                mal_id,
                local,
                &status,
                local_finished,
                sync_date,
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

    /// `fuzzy_match_threshold` for end-to-end `run_sync` tests that aren't about
    /// the threshold boundary.
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
            num_episodes: 0,
            status: MalListStatus::Unknown,
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
        // A top-ranked candidate is not an exact match.
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

    // --- Merge decision table ---

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

    #[test]
    fn mal_status_transitions_are_forward_only_and_progress_driven() {
        assert_eq!(
            desired_status(MalListStatus::PlanToWatch, 1, 12, false),
            Some(MalListStatus::Watching)
        );
        assert_eq!(
            desired_status(MalListStatus::Watching, 12, 12, false),
            Some(MalListStatus::Completed)
        );
        assert_eq!(
            desired_status(MalListStatus::PlanToWatch, 12, 12, false),
            Some(MalListStatus::Completed),
            "a fully watched planned title should not require two sync runs"
        );
        assert_eq!(
            desired_status(MalListStatus::Watching, 3, 0, true),
            Some(MalListStatus::Completed),
            "Tsuzuki's confirmed-finale signal works when MAL's total is unknown"
        );

        for status in [
            MalListStatus::OnHold,
            MalListStatus::Dropped,
            MalListStatus::Completed,
            MalListStatus::Unknown,
        ] {
            assert_eq!(desired_status(status, 12, 12, true), None);
        }
        assert_eq!(
            desired_status(MalListStatus::PlanToWatch, 0, 12, false),
            None,
            "zero progress must not move a planned title to Watching"
        );

        let date = "2026-09-23";
        assert_eq!(
            transition_dates(
                MalListStatus::PlanToWatch,
                Some(MalListStatus::Watching),
                date
            ),
            (Some(date), None)
        );
        assert_eq!(
            transition_dates(
                MalListStatus::Watching,
                Some(MalListStatus::Completed),
                date
            ),
            (None, Some(date))
        );
        assert_eq!(
            transition_dates(
                MalListStatus::PlanToWatch,
                Some(MalListStatus::Completed),
                date
            ),
            (Some(date), Some(date))
        );
        assert_eq!(
            transition_dates(MalListStatus::Watching, None, date),
            (None, None),
            "dates must remain untouched without a status transition"
        );
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
            season_number: None,
        }
    }

    #[test]
    fn local_effective_uses_episode_number_minus_one_when_no_prior_mal_awareness() {
        // base_record's cursor is episode 9, still being watched: 8 watched.
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
    fn local_effective_uses_cursor_as_is_when_completed_on_the_finale() {
        // No next episode to advance to, so the cursor stays on the finished
        // finale and counts as watched.
        let mut record = base_record();
        record.status = Status::Completed;
        record.position_seconds = 1380.0;
        assert_eq!(local_effective_episode(&record), 9);
    }

    #[test]
    fn local_effective_subtracts_one_when_dropped_mid_episode_one() {
        // A manual drop two minutes into ep1 never watched ep1.
        let mut record = base_record();
        record.status = Status::Dropped;
        record.episode_number = Some(1);
        record.position_seconds = 120.0;
        assert_eq!(local_effective_episode(&record), 0);
    }

    #[test]
    fn local_effective_subtracts_one_when_dropped_late_in_an_episode() {
        // Position alone never proves the cursor episode finished.
        let mut record = base_record();
        record.status = Status::Dropped;
        record.position_seconds = 1380.0;
        assert_eq!(local_effective_episode(&record), 8);
    }

    #[test]
    fn local_effective_subtracts_one_when_dropped_at_a_fresh_untouched_cursor() {
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
        // Awareness is 9 but the cursor stayed on 7, as an older release left it.
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
        let advanced = apply_synced_progress(&config, &mut state, &outcomes, Some(dir.path()));
        assert_eq!(
            advanced,
            vec![(
                root_string.clone(),
                "Shokei Shoujo no Virgin Road".to_string(),
                9
            )]
        );

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
        for name in ["Watching", "Local Rewatch", "Abandoned", "Finished"] {
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
        dropped.name = "Abandoned".to_string();
        dropped.status = Status::Dropped;

        let mut completed = base_record();
        completed.root.clone_from(&root_string);
        completed.name = "Finished".to_string();
        completed.status = Status::Completed;

        let mut orphaned = base_record();
        orphaned.root.clone_from(&root_string);
        orphaned.name = "Deleted".to_string();

        let state = State {
            series: vec![in_progress, paused, dropped, completed, orphaned],
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

    // --- Non-destructive invariant ---
    //
    // A stale raw episode_number (15) below established awareness (20) must
    // never be pushed over MAL's higher count; the merge sees the effective
    // number.

    #[test]
    fn never_pushes_a_smaller_number_when_a_stale_local_read_is_lower_than_established_awareness() {
        let effective_local = local_effective_episode(&{
            let mut r = base_record();
            r.episode_number = Some(15); // stale/lower raw local read
            r.mal_episode_awareness = Some(20); // already-established higher number
            r
        });
        // Comparing the raw 15 would wrongly pull for MAL counts in 16..=19;
        // the effective 20 pushes 20 instead.
        assert_eq!(effective_local, 20);
        assert_eq!(decide_merge(effective_local, 18), MergeAction::Push(20));
        assert_eq!(decide_merge(effective_local, 20), MergeAction::NoOp);
        assert_eq!(decide_merge(effective_local, 25), MergeAction::Pull(25));
    }

    #[test]
    fn run_sync_never_patches_a_smaller_number_than_mal_already_has() {
        // End-to-end: stale local 15 below MAL's 18 pulls; the fake panics on
        // any PATCH below 18.
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

        let outcomes = run_sync_on_date(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
            "2026-09-23",
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

    #[test]
    fn progressed_plan_to_watch_entry_moves_to_watching_in_the_same_patch() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":42,"title":"Test Anime","num_episodes":12,"my_list_status":{"status":"plan_to_watch","num_episodes_watched":0}}"#,
        );

        let mut state = State {
            series: vec![base_record()], // cursor 9 -> 8 watched locally
            mal_links: vec![MalLink {
                root: "~/Anime".to_string(),
                name: "One Piece".to_string(),
                mal_id: 42,
            }],
        };
        let outcomes = run_sync_on_date(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
            "2026-09-23",
        );

        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::Push(8)
            }
        );
        assert_eq!(
            client.patch_calls.borrow().as_slice(),
            &[(
                format!("{API_BASE}/anime/42/my_list_status"),
                "num_watched_episodes=8&status=watching&start_date=2026-09-23".to_string()
            )]
        );
    }

    #[test]
    fn status_only_transition_is_reported_as_a_real_remote_change() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":42,"title":"Test Anime","num_episodes":12,"my_list_status":{"status":"plan_to_watch","num_episodes_watched":8}}"#,
        );

        let mut state = State {
            series: vec![base_record()], // cursor 9 -> 8 watched locally
            mal_links: vec![MalLink {
                root: "~/Anime".to_string(),
                name: "One Piece".to_string(),
                mal_id: 42,
            }],
        };
        let outcomes = run_sync_on_date(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
            "2026-09-23",
        );

        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::RemoteStatus(MalListStatus::Watching)
            }
        );
        assert_eq!(
            client.patch_calls.borrow()[0].1,
            "status=watching&start_date=2026-09-23".to_string()
        );
    }

    #[test]
    fn reaching_mals_episode_total_moves_watching_entry_to_completed() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":42,"title":"Test Anime","num_episodes":12,"my_list_status":{"status":"watching","num_episodes_watched":11}}"#,
        );

        let mut record = base_record();
        record.episode_number = Some(13); // cursor 13 -> 12 watched locally
        let mut state = State {
            series: vec![record],
            mal_links: vec![MalLink {
                root: "~/Anime".to_string(),
                name: "One Piece".to_string(),
                mal_id: 42,
            }],
        };
        let outcomes = run_sync_on_date(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
            "2026-09-23",
        );

        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::Push(12)
            }
        );
        assert_eq!(
            client.patch_calls.borrow()[0].1,
            "num_watched_episodes=12&status=completed&finish_date=2026-09-23".to_string()
        );
    }

    #[test]
    fn completing_a_planned_entry_sets_both_transition_dates() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":42,"title":"Test Anime","num_episodes":12,"my_list_status":{"status":"plan_to_watch","num_episodes_watched":0}}"#,
        );

        let mut record = base_record();
        record.episode_number = Some(13); // cursor 13 -> 12 watched locally
        let mut state = State {
            series: vec![record],
            mal_links: vec![MalLink {
                root: "~/Anime".to_string(),
                name: "One Piece".to_string(),
                mal_id: 42,
            }],
        };
        let outcomes = run_sync_on_date(
            &client,
            "token",
            &mut state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
            "2026-09-23",
        );

        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::Push(12)
            }
        );
        assert_eq!(
            client.patch_calls.borrow()[0].1,
            "num_watched_episodes=12&status=completed&start_date=2026-09-23&finish_date=2026-09-23"
        );
    }

    // --- Manual link (`sync link`) ---

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
        // A valid MAL id with no my_list_status: not on this user's list.
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
        // "One Piece" vs "One Piece Special" scores ~0.53, under the
        // threshold, so no suggestion is shown.
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
        // "One Piece" vs "One Piece!" scores 0.9, above the 0.8 threshold.
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
        // A suggestion never auto-links; `sync link` is still required.
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
            r#"{"data":[{"node":{"id":7,"title":"One Piece","num_episodes":12,"alternative_titles":{"en":"","ja":"","synonyms":[]}},"list_status":{"status":"watching","num_episodes_watched":3}}]}"#,
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
        // Only the list fetch; no per-anime GET for a series linked this run.
        assert_eq!(client.get_calls.borrow().len(), 1);
        assert_eq!(client.patch_calls.borrow().len(), 1);
    }

    // --- Partial-failure continuation ---

    #[test]
    fn one_series_failing_does_not_abort_or_corrupt_the_rest_of_the_run() {
        let client = FakeClient::new();
        // Both linked. The status lookup for id 1 fails; id 2 succeeds.
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

        // The failing series' state is untouched.
        let failing = state.find("~/Anime", "Failing Show").unwrap();
        assert_eq!(failing.mal_episode_awareness, None);
        // The working series persisted its result.
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

    fn run_one_linked(client: &FakeClient, state: &mut State) -> Vec<SeriesOutcome> {
        run_sync_on_date(
            client,
            "token",
            state,
            &[("~/Anime".to_string(), "One Piece".to_string())],
            Duration::ZERO,
            TEST_FUZZY_MATCH_THRESHOLD,
            "2026-09-23",
        )
    }

    fn linked_state(record: SeriesRecord) -> State {
        State {
            series: vec![record],
            mal_links: vec![MalLink {
                root: "~/Anime".to_string(),
                name: "One Piece".to_string(),
                mal_id: 42,
            }],
        }
    }

    fn one_patch(body: &str) -> Vec<(String, String)> {
        vec![(
            format!("{API_BASE}/anime/42/my_list_status"),
            body.to_string(),
        )]
    }

    #[test]
    fn run_sync_stale_cursor_below_awareness_pushes_awareness_not_cursor_minus_one() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":42,"title":"T","my_list_status":{"status":"watching","num_episodes_watched":18}}"#,
        );
        let mut record = base_record();
        record.episode_number = Some(15);
        record.mal_episode_awareness = Some(20);
        let mut state = linked_state(record);

        let outcomes = run_one_linked(&client, &mut state);

        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::Push(20)
            }
        );
        assert_eq!(
            *client.patch_calls.borrow(),
            one_patch("num_watched_episodes=20")
        );
        assert_eq!(state.series[0].mal_episode_awareness, Some(20));
    }

    #[test]
    fn run_sync_completed_finale_pushes_cursor_and_completes_the_mal_entry() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":42,"title":"T","my_list_status":{"status":"watching","num_episodes_watched":5}}"#,
        );
        let mut record = base_record();
        record.status = Status::Completed;
        record.episode_number = Some(12);
        record.position_seconds = 1400.0;
        let mut state = linked_state(record);

        let outcomes = run_one_linked(&client, &mut state);

        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::Push(12)
            }
        );
        assert_eq!(
            *client.patch_calls.borrow(),
            one_patch("num_watched_episodes=12&status=completed&finish_date=2026-09-23")
        );
        assert_eq!(state.series[0].mal_episode_awareness, Some(12));
    }

    #[test]
    fn run_sync_dropped_mid_episode_one_never_completes_or_pushes_to_mal() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":42,"title":"T","num_episodes":12,"my_list_status":{"status":"plan_to_watch","num_episodes_watched":0}}"#,
        );
        let mut record = base_record();
        record.status = Status::Dropped;
        record.episode_number = Some(1);
        record.position_seconds = 120.0;
        let mut state = linked_state(record);

        let outcomes = run_one_linked(&client, &mut state);

        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::NoOp
            }
        );
        assert_eq!(*client.patch_calls.borrow(), Vec::new());
    }

    #[test]
    fn run_sync_rejected_patch_leaves_awareness_unset_and_retry_pushes_same_number() {
        let client = FakeClient::new();
        client.push_status_response(
            200,
            r#"{"id":42,"title":"T","my_list_status":{"status":"watching","num_episodes_watched":18}}"#,
        );
        client.patch_status.set(500);
        let mut record = base_record();
        record.episode_number = Some(21);
        let mut state = linked_state(record.clone());

        let outcomes = run_one_linked(&client, &mut state);

        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Failed("MyAnimeList rejected the request: HTTP 500: {}".to_string())
        );
        assert_eq!(state.series[0], record);

        client.push_status_response(
            200,
            r#"{"id":42,"title":"T","my_list_status":{"status":"watching","num_episodes_watched":18}}"#,
        );
        client.patch_status.set(200);
        let outcomes = run_one_linked(&client, &mut state);

        assert_eq!(
            outcomes[0].kind,
            OutcomeKind::Merged {
                mal_id: 42,
                action: MergeAction::Push(20)
            }
        );
        assert_eq!(
            *client.patch_calls.borrow(),
            [
                one_patch("num_watched_episodes=20")[0].clone(),
                one_patch("num_watched_episodes=20")[0].clone()
            ]
        );
        assert_eq!(state.series[0].mal_episode_awareness, Some(20));
    }

    #[test]
    fn desired_status_unfinished_progress_only_moves_plan_to_watch_forward() {
        assert_eq!(
            desired_status(MalListStatus::PlanToWatch, 3, 12, false),
            Some(MalListStatus::Watching)
        );
        for status in [
            MalListStatus::Watching,
            MalListStatus::OnHold,
            MalListStatus::Dropped,
            MalListStatus::Completed,
            MalListStatus::Unknown,
        ] {
            assert_eq!(desired_status(status, 3, 12, false), None, "{status:?}");
        }
    }

    #[test]
    fn desired_status_watched_count_reaching_or_passing_total_completes() {
        assert_eq!(desired_status(MalListStatus::Watching, 11, 12, false), None);
        assert_eq!(
            desired_status(MalListStatus::Watching, 12, 12, false),
            Some(MalListStatus::Completed)
        );
        assert_eq!(
            desired_status(MalListStatus::Watching, 13, 12, false),
            Some(MalListStatus::Completed)
        );
    }

    // --- Fakeable HTTP client for this module's own tests ---

    /// Queue-based fake: each `push_*_response` queues one response for the
    /// next matching call. Records every call so tests can assert call counts
    /// and PATCH bodies.
    struct FakeClient {
        get_responses: RefCell<Vec<(u16, String)>>,
        get_calls: RefCell<Vec<String>>,
        patch_calls: RefCell<Vec<(String, String)>>,
        patch_status: std::cell::Cell<u16>,
    }

    impl FakeClient {
        fn new() -> Self {
            FakeClient {
                get_responses: RefCell::new(Vec::new()),
                get_calls: RefCell::new(Vec::new()),
                patch_calls: RefCell::new(Vec::new()),
                patch_status: std::cell::Cell::new(200),
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
                status: self.patch_status.get(),
                body: "{}".to_string(),
            })
        }
    }

    // --- Status-matrix / incidental-field guards ---

    /// Every `Status`, with a wildcard-free `match` so a new variant fails
    /// to compile until it is added here and given an expectation below.
    fn all_statuses() -> [Status; 4] {
        fn guard(status: Status) {
            match status {
                Status::InProgress | Status::Paused | Status::Dropped | Status::Completed => {}
            }
        }
        let all = [
            Status::InProgress,
            Status::Paused,
            Status::Dropped,
            Status::Completed,
        ];
        all.into_iter().for_each(guard);
        all
    }

    fn record_with(status: Status, cursor: u32, position_seconds: f64) -> SeriesRecord {
        SeriesRecord {
            status,
            episode_number: Some(cursor),
            position_seconds,
            ..base_record()
        }
    }

    const IRRELEVANT_POSITIONS: [f64; 4] = [0.0, 1.0, 120.0, 1380.0];
    const CURSORS: [u32; 3] = [1, 5, 12];

    #[test]
    fn local_effective_episode_ignores_position_seconds_for_every_status() {
        for status in all_statuses() {
            for cursor in CURSORS {
                let expected = match status {
                    Status::Completed => cursor,
                    Status::InProgress | Status::Paused | Status::Dropped => cursor - 1,
                };
                for position in IRRELEVANT_POSITIONS {
                    assert_eq!(
                        local_effective_episode(&record_with(status, cursor, position)),
                        expected,
                        "{status:?} cursor {cursor} position {position}"
                    );
                }
            }
        }
    }

    #[test]
    fn eligible_targets_by_status_matches_the_hand_written_table() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(root.join("One Piece")).unwrap();
        let root_string = root.to_string_lossy().into_owned();
        let mut config = Config::default();
        config.roots.paths.push(root_string.clone());

        for status in all_statuses() {
            let expected = match status {
                Status::InProgress | Status::Completed => true,
                Status::Paused | Status::Dropped => false,
            };
            let mut record = record_with(status, 3, 120.0);
            record.root.clone_from(&root_string);
            let state = State {
                series: vec![record],
                mal_links: Vec::new(),
            };
            assert_eq!(
                !eligible_targets(&state, &config, Some(dir.path())).is_empty(),
                expected,
                "{status:?}"
            );
        }
    }

    #[test]
    fn eligible_targets_skip_sync_excluded_series_and_roots_at_every_status() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("Anime");
        let movies = dir.path().join("Movies");
        for series in [
            anime.join("Monster"),
            anime.join("Naruto"),
            movies.join("Monster"),
        ] {
            fs::create_dir_all(series).unwrap();
        }
        let anime_s = anime.to_string_lossy().into_owned();
        let movies_s = movies.to_string_lossy().into_owned();
        let mut config = Config::default();
        config.roots.paths = vec![anime_s.clone(), movies_s.clone()];
        add_sync_exclusion(&mut config, &anime_s, Some("Monster"));
        add_sync_exclusion(&mut config, &movies_s, None);

        for status in all_statuses() {
            let records = [
                (&anime_s, "Monster"),
                (&anime_s, "Naruto"),
                (&movies_s, "Monster"),
            ]
            .map(|(root, name)| SeriesRecord {
                root: root.clone(),
                name: name.to_string(),
                ..record_with(status, 3, 120.0)
            });
            let state = State {
                series: records.to_vec(),
                mal_links: Vec::new(),
            };
            let expected = match status {
                Status::InProgress | Status::Completed => {
                    vec![(anime_s.clone(), "Naruto".to_string())]
                }
                Status::Paused | Status::Dropped => Vec::new(),
            };
            assert_eq!(
                eligible_targets(&state, &config, Some(dir.path())),
                expected,
                "{status:?}"
            );
        }
    }

    #[test]
    fn is_sync_excluded_matches_by_stored_root_and_series_or_whole_root() {
        let mut config = Config::default();
        add_sync_exclusion(&mut config, "~/Anime", Some("Monster"));
        add_sync_exclusion(&mut config, "~/Movies", None);

        let table = [
            ("~/Anime", "Monster", true),
            ("~/Anime", "Naruto", false),
            ("~/Anime/", "Monster", false),
            ("~/Other", "Monster", false),
            ("~/Movies", "Akira", true),
            ("~/Movies", "Monster", true),
            ("~/Movie", "Akira", false),
        ];
        for (root, name, expected) in table {
            assert_eq!(
                is_sync_excluded(&config, root, name),
                expected,
                "{root} {name}"
            );
        }
    }

    #[test]
    fn add_sync_exclusion_is_idempotent_and_keeps_series_and_root_entries_distinct() {
        let mut config = Config::default();
        assert_eq!(
            add_sync_exclusion(&mut config, "~/Anime", Some("Monster")),
            SyncExclusionOutcome::Added
        );
        assert_eq!(
            add_sync_exclusion(&mut config, "~/Anime", Some("Monster")),
            SyncExclusionOutcome::AlreadyExcluded
        );
        assert_eq!(
            add_sync_exclusion(&mut config, "~/Anime", None),
            SyncExclusionOutcome::Added
        );
        assert_eq!(
            add_sync_exclusion(&mut config, "~/Anime", None),
            SyncExclusionOutcome::AlreadyExcluded
        );
        assert_eq!(
            config.sync_exclusions,
            vec![
                SyncExclusion {
                    root: "~/Anime".to_string(),
                    series: Some("Monster".to_string()),
                },
                SyncExclusion {
                    root: "~/Anime".to_string(),
                    series: None,
                },
            ]
        );
    }

    #[test]
    fn eligible_targets_apply_sync_exclusions_by_stored_root_under_aliased_roots() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("real/Monster")).unwrap();
        fs::create_dir_all(home.join("real/Naruto")).unwrap();
        std::os::unix::fs::symlink(home.join("real"), home.join("link")).unwrap();
        fs::create_dir_all(home.join("anime/Monster")).unwrap();
        fs::create_dir_all(home.join("anime/rewatch/Monster")).unwrap();

        let real = home.join("real").to_string_lossy().into_owned();
        let link = home.join("link").to_string_lossy().into_owned();
        let anime = home.join("anime").to_string_lossy().into_owned();
        let nested = home.join("anime/rewatch").to_string_lossy().into_owned();
        let mut config = Config::default();
        config.roots.paths = vec![real.clone(), link.clone(), anime.clone(), nested.clone()];
        // One series excluded under the symlink root, the whole nested root excluded.
        add_sync_exclusion(&mut config, &link, Some("Monster"));
        add_sync_exclusion(&mut config, &nested, None);

        let records = [
            (&real, "Monster"),
            (&real, "Naruto"),
            (&link, "Monster"),
            (&link, "Naruto"),
            (&anime, "Monster"),
            (&nested, "Monster"),
        ]
        .map(|(root, name)| SeriesRecord {
            root: root.clone(),
            name: name.to_string(),
            ..record_with(Status::InProgress, 3, 120.0)
        });
        let state = State {
            series: records.to_vec(),
            mal_links: Vec::new(),
        };

        let mut got = eligible_targets(&state, &config, Some(home));
        got.sort();
        let mut want = vec![
            (real.clone(), "Monster".to_string()),
            (real, "Naruto".to_string()),
            (link, "Naruto".to_string()),
            (anime, "Monster".to_string()),
        ];
        want.sort();
        assert_eq!(got, want);
    }

    fn pushed_count(body: &str) -> Option<u32> {
        let rest = body.strip_prefix("num_watched_episodes=")?;
        rest.split('&').next()?.parse().ok()
    }

    #[test]
    fn sync_never_completes_or_overpushes_unless_the_record_is_completed() {
        let mal_states = [
            r#"{"id":42,"title":"T","num_episodes":12,"my_list_status":{"status":"plan_to_watch","num_episodes_watched":0}}"#,
            r#"{"id":42,"title":"T","num_episodes":12,"my_list_status":{"status":"watching","num_episodes_watched":5}}"#,
        ];
        for status in all_statuses() {
            for cursor in CURSORS {
                for position in IRRELEVANT_POSITIONS {
                    for mal in mal_states {
                        let client = FakeClient::new();
                        client.push_status_response(200, mal);
                        let mut state = linked_state(record_with(status, cursor, position));

                        run_one_linked(&client, &mut state);

                        for (_, body) in client.patch_calls.borrow().iter() {
                            let context =
                                format!("{status:?} cursor {cursor} pos {position}: {body}");
                            if status != Status::Completed {
                                assert!(!body.contains("status=completed"), "{context}");
                                assert!(pushed_count(body).unwrap_or(0) < cursor, "{context}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn sync_completes_the_mal_entry_for_a_completed_record_at_every_position() {
        // Positive control: the grid above isn't vacuous.
        for position in IRRELEVANT_POSITIONS {
            let client = FakeClient::new();
            client.push_status_response(
                200,
                r#"{"id":42,"title":"T","num_episodes":12,"my_list_status":{"status":"watching","num_episodes_watched":5}}"#,
            );
            let mut state = linked_state(record_with(Status::Completed, 12, position));

            run_one_linked(&client, &mut state);

            assert_eq!(
                *client.patch_calls.borrow(),
                one_patch("num_watched_episodes=12&status=completed&finish_date=2026-09-23"),
                "position {position}"
            );
        }
    }

    #[test]
    fn a_series_dropped_by_the_real_drop_command_is_never_a_sync_target_or_pushes_ahead() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(root.join("One Piece")).unwrap();
        let root_string = root.to_string_lossy().into_owned();
        let mut config = Config::default();
        config.roots.paths.push(root_string.clone());

        for position in IRRELEVANT_POSITIONS {
            let mut record = record_with(Status::InProgress, 1, position);
            record.root.clone_from(&root_string);
            let mut state = State {
                series: vec![record],
                mal_links: Vec::new(),
            };
            crate::pause_drop::drop_series(&mut state, &root_string, "One Piece").unwrap();

            assert_eq!(
                eligible_targets(&state, &config, Some(dir.path())),
                Vec::<(String, String)>::new(),
                "position {position}"
            );
            assert_eq!(local_effective_episode(&state.series[0]), 0);
        }
    }

    #[test]
    fn eligible_targets_skips_a_record_for_a_directory_that_is_itself_a_tracked_root() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("Anime");
        let rewatch = anime.join("Rewatch");
        fs::create_dir_all(rewatch.join("Bleach")).unwrap();
        let mut config = Config::default();
        config
            .roots
            .paths
            .push(anime.to_string_lossy().into_owned());
        config
            .roots
            .paths
            .push(rewatch.to_string_lossy().into_owned());

        let mut stale = record_with(Status::InProgress, 3, 120.0);
        stale.root = anime.to_string_lossy().into_owned();
        stale.name = "Rewatch".to_string();
        let mut live = record_with(Status::InProgress, 3, 120.0);
        live.root = rewatch.to_string_lossy().into_owned();
        live.name = "Bleach".to_string();
        let state = State {
            series: vec![stale, live],
            mal_links: Vec::new(),
        };

        assert_eq!(
            eligible_targets(&state, &config, Some(dir.path())),
            vec![(rewatch.to_string_lossy().into_owned(), "Bleach".to_string())]
        );
    }

    #[test]
    fn eligible_targets_follow_the_stored_root_under_every_root_layout() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("real/Monster")).unwrap();
        std::os::unix::fs::symlink(home.join("real"), home.join("link")).unwrap();
        fs::create_dir_all(home.join("anime/Monster")).unwrap();
        fs::create_dir_all(home.join("anime/rewatch/Monster")).unwrap();
        fs::create_dir_all(home.join("tilde/Monster")).unwrap();
        fs::create_dir_all(home.join("target_root/Other")).unwrap();
        std::os::unix::fs::symlink(home.join("target_root"), home.join("anime/Aliased")).unwrap();

        let stored = [
            format!("{}/", home.join("real").display()),
            home.join("link").to_string_lossy().into_owned(),
            home.join("anime").to_string_lossy().into_owned(),
            home.join("anime/rewatch").to_string_lossy().into_owned(),
            "~/tilde".to_string(),
            home.join("target_root").to_string_lossy().into_owned(),
        ];
        let mut config = Config::default();
        config.roots.paths.extend(stored.iter().cloned());

        let mut records = Vec::new();
        for (root, name) in [
            (&stored[0], "Monster"),
            (&stored[1], "Monster"),
            (&stored[2], "Monster"),
            (&stored[3], "Monster"),
            (&stored[4], "Monster"),
            (&stored[2], "rewatch"),
            (&stored[2], "Aliased"),
            (&stored[5], "Other"),
        ] {
            let mut record = record_with(Status::InProgress, 3, 120.0);
            record.root.clone_from(root);
            record.name = name.to_string();
            records.push(record);
        }
        let state = State {
            series: records,
            mal_links: Vec::new(),
        };

        let mut got = eligible_targets(&state, &config, Some(home));
        got.sort();
        let mut want: Vec<(String, String)> = [
            (&stored[0], "Monster"),
            (&stored[1], "Monster"),
            (&stored[2], "Monster"),
            (&stored[3], "Monster"),
            (&stored[4], "Monster"),
            (&stored[5], "Other"),
        ]
        .iter()
        .map(|(r, n)| ((*r).clone(), (*n).to_string()))
        .collect();
        want.sort();
        assert_eq!(got, want);
    }
}
