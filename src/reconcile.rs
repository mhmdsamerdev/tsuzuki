//! Resolves `state.toml` against current mpv reality; every state-touching
//! command runs it first. Readings below `resume_seconds` are noise, and
//! absence of an entry is never evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::ani_cli;
use crate::anitomy;
use crate::config::Config;
use crate::exclude;
use crate::roots::{expand_tilde, normalize, tracked_real_paths};
use crate::script_log::{self, ScriptLogEntry};
use crate::series;
use crate::state::{SeriesRecord, State, Status, now_unix};

/// What a reading says about an episode.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PlaybackAction {
    /// At or above `resume_seconds`, and not within `completion_remaining_seconds` of the end.
    Resume(f64),
    /// A script `eof`, or within `completion_remaining_seconds` of the end.
    Completed,
}

/// Classifies one script-log reading. `None` for a reading below
/// `resume_seconds` or a non-`eof` one with no position: the scan treats that
/// like no entry. The position check comes first, so a short file never reads
/// as `Completed` from a position below it. Without a duration the completion
/// check is skipped.
fn classify_entry(
    entry: &ScriptLogEntry,
    resume_seconds: u64,
    completion_remaining_seconds: u64,
) -> Option<PlaybackAction> {
    if entry.reason == "eof" {
        return Some(PlaybackAction::Completed);
    }
    let position = entry.position?;
    if position < resume_seconds as f64 {
        return None;
    }
    match entry.duration {
        Some(total) if total - position <= completion_remaining_seconds as f64 => {
            Some(PlaybackAction::Completed)
        }
        _ => Some(PlaybackAction::Resume(position)),
    }
}

/// The reading for `key`. With a `cutoff` (unix seconds), a reading whose
/// entry file is not strictly newer is a leftover and counts as absent.
fn fresh_candidate<'a>(
    candidates: &'a BTreeMap<PathBuf, ScriptLogEntry>,
    key: &Path,
    cutoff: Option<u64>,
) -> Option<&'a ScriptLogEntry> {
    let candidate = candidates.get(key)?;
    is_newer_than(candidate, cutoff).then_some(candidate)
}

/// True without a `cutoff`, else when the entry file's mtime is strictly
/// newer than it (unix seconds).
fn is_newer_than(entry: &ScriptLogEntry, cutoff: Option<u64>) -> bool {
    let Some(cutoff) = cutoff else {
        return true;
    };
    let mtime_seconds = entry
        .mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    mtime_seconds > cutoff
}

/// Reconciles every series that has a record or real activity under a tracked
/// root. Mutates `state` in place; persisting it is the caller's job.
pub fn reconcile(config: &Config, state: &mut State, home: Option<&Path>) {
    let script_log_dirs = script_log::mpv_log_dirs(home);
    // An unreadable history claims nothing; ani-cli records stay as they are.
    let history = if config.ani_cli.enabled {
        ani_cli::history_path_here(&config.ani_cli, home)
            .and_then(|path| ani_cli::read_history(&path).ok())
            .unwrap_or_default()
    } else {
        ani_cli::History::default()
    };
    reconcile_with_inputs(config, state, home, &script_log_dirs, &history);
}

/// [`reconcile_with_inputs`] with no ani-cli history, for the local-folder
/// tests.
#[cfg(test)]
fn reconcile_with_script_log_dirs(
    config: &Config,
    state: &mut State,
    home: Option<&Path>,
    script_log_dirs: &[PathBuf],
) {
    reconcile_with_inputs(
        config,
        state,
        home,
        script_log_dirs,
        &ani_cli::History::default(),
    );
}

/// [`reconcile`] with the script-log directories and ani-cli history
/// injected, since they have no config field for tests to override.
fn reconcile_with_inputs(
    config: &Config,
    state: &mut State,
    home: Option<&Path>,
    script_log_dirs: &[PathBuf],
    history: &ani_cli::History,
) {
    let resume_seconds = config.thresholds.resume_seconds;
    let completion_remaining_seconds = config.thresholds.completion_remaining_seconds;

    let resolved = script_log::resolve_roots(&config.roots.paths, home);
    let scan = script_log::scan_script_log_full(script_log_dirs, &resolved, home);
    let candidates = scan.files;

    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for record in state.series.iter().filter(|r| r.root != ani_cli::ROOT) {
        pairs.insert((record.root.clone(), record.name.clone()));
    }
    for (real_source, candidate) in &candidates {
        if classify_entry(candidate, resume_seconds, completion_remaining_seconds).is_none() {
            continue;
        }
        if let Some(pair) = classify_root_and_series(&resolved, real_source) {
            pairs.insert(pair);
        }
    }

    for (root, name) in pairs {
        reconcile_one_series(
            config,
            state,
            &root,
            &name,
            &candidates,
            resume_seconds,
            completion_remaining_seconds,
            home,
        );
    }

    if config.ani_cli.enabled {
        let claims = ani_cli::claim_all(&scan.streams, history);
        reconcile_ani_cli(state, &claims, resume_seconds, completion_remaining_seconds);
    }

    apply_idle_auto_pause(state, config.thresholds.auto_pause_days, now_unix());
}

/// Reconciles every ani-cli show that has a claimed reading. A show without
/// one is untouched: absence proves nothing.
fn reconcile_ani_cli(
    state: &mut State,
    claims: &ani_cli::Claims,
    resume_seconds: u64,
    completion_remaining_seconds: u64,
) {
    let mut by_title: BTreeMap<&str, BTreeMap<u32, &ScriptLogEntry>> = BTreeMap::new();
    for (claim, entry) in &claims.claimed {
        by_title
            .entry(&claim.title)
            .or_default()
            .insert(claim.episode, entry);
    }
    for (title, episodes) in by_title {
        let total = state
            .find_mal_link(ani_cli::ROOT, title)
            .and_then(|link| link.total_episodes);
        reconcile_ani_cli_title(
            state,
            title,
            total,
            &episodes,
            resume_seconds,
            completion_remaining_seconds,
        );
    }
}

/// Reconciles one show from its readings, keyed by episode number. The
/// furthest episode with a usable reading decides: a `Resume` lands on that
/// episode at that position, a `Completed` lands on the next episode at 0.
/// A `Completed` reading of the final episode, `total` (MAL's count), makes
/// the show `Completed` instead; a reading above `total`, or an unknown
/// `total`, never does.
/// Like a folder series, a dropped or completed record reacts only to readings
/// newer than it, and an active one ignores episodes before its cursor.
fn reconcile_ani_cli_title(
    state: &mut State,
    title: &str,
    total: Option<u32>,
    episodes: &BTreeMap<u32, &ScriptLogEntry>,
    resume_seconds: u64,
    completion_remaining_seconds: u64,
) {
    let existing = state
        .series
        .iter()
        .position(|r| r.root == ani_cli::ROOT && r.name == title);

    let (cutoff, start_episode) = match existing.map(|i| &state.series[i]) {
        Some(r) if matches!(r.status, Status::Dropped | Status::Completed) => {
            (Some(r.updated_at), 0)
        }
        // An active record sits one past the final episode right after that
        // episode finished, so the scan reaches back to it.
        Some(r) => {
            let cursor = r.episode_number.unwrap_or(0);
            (None, total.map_or(cursor, |t| cursor.min(t)))
        }
        None => (None, 0),
    };

    let mut furthest: Option<(u32, PlaybackAction)> = None;
    for (&episode, entry) in episodes.range(start_episode..) {
        if !is_newer_than(entry, cutoff) {
            continue;
        }
        if let Some(action) = classify_entry(entry, resume_seconds, completion_remaining_seconds) {
            furthest = Some((episode, action));
        }
    }
    let Some((episode, action)) = furthest else {
        return;
    };
    let stored_position = existing.map_or(0.0, |i| state.series[i].position_seconds);
    let (episode, position, finished) = match action {
        PlaybackAction::Resume(position) => (episode, position, false),
        PlaybackAction::Completed if total == Some(episode) => (episode, stored_position, true),
        PlaybackAction::Completed => (episode.saturating_add(1), 0.0, false),
    };
    let status = if finished {
        Status::Completed
    } else {
        Status::InProgress
    };

    // Readings that pass the cutoff are newer than the record, so even an
    // identical one is new watching.
    let changed = match existing.map(|i| &state.series[i]) {
        Some(r) => {
            cutoff.is_some()
                || finished
                || r.episode_number != Some(episode)
                || (r.position_seconds - position).abs() > f64::EPSILON
        }
        None => true,
    };
    if !changed {
        return;
    }

    let now = now_unix();
    match existing {
        Some(i) => {
            let record = &mut state.series[i];
            record.status = status;
            record.position_seconds = position;
            record.episode_number = Some(episode);
            record.season_number = None;
            record.updated_at = now;
        }
        None => state.series.push(SeriesRecord {
            root: ani_cli::ROOT.to_string(),
            name: title.to_string(),
            status,
            current_relative_path: String::new(),
            position_seconds: position,
            episode_number: Some(episode),
            updated_at: now,
            mal_episode_awareness: None,
            season_number: None,
        }),
    }
}

/// Auto-pauses every `in_progress` record idle for at least `auto_pause_days`
/// days. 0 disables it. Runs after the per-series pass, so a series touched in
/// this same call (`updated_at == now`) can't idle-pause itself.
fn apply_idle_auto_pause(state: &mut State, auto_pause_days: u64, now: u64) {
    if auto_pause_days == 0 {
        return;
    }
    let idle_threshold_seconds = auto_pause_days.saturating_mul(86_400);
    for record in &mut state.series {
        if record.status == Status::InProgress
            && now.saturating_sub(record.updated_at) >= idle_threshold_seconds
        {
            record.status = Status::Paused;
            record.updated_at = now;
        }
    }
}

/// Finds the tracked root the canonical `real_source` falls under and its
/// series name (the root's depth-1 directory). With nested roots (`~/Anime` and
/// `~/Anime/Rewatches`) the most specific match wins.
///
/// Tries a whole-root prefix match first. If that fails, matches against the
/// series folders symlinked outside the root, since `normalize` resolves their
/// files to a location outside it.
fn classify_root_and_series(
    resolved: &[script_log::ResolvedRoot],
    real_source: &Path,
) -> Option<(String, String)> {
    let mut best: Option<(PathBuf, String, String)> = None;
    for r in resolved {
        if let Ok(rel) = real_source.strip_prefix(&r.real_root) {
            if let Some(first) = rel.components().next() {
                let name = first.as_os_str().to_string_lossy().into_owned();
                consider_match(&mut best, r.real_root.clone(), &r.root, &name);
            }
            // The child fallback below only applies when the prefix match fails.
            continue;
        }

        for (name, real_child) in &r.outside_children {
            if real_source.starts_with(real_child) {
                consider_match(&mut best, real_child.clone(), &r.root, name);
            }
        }
    }
    best.map(|(_, root, name)| (root, name))
}

/// Replaces `best` when `matched_prefix` is longer than its current prefix.
fn consider_match(
    best: &mut Option<(PathBuf, String, String)>,
    matched_prefix: PathBuf,
    root: &str,
    name: &str,
) {
    let is_more_specific = match best {
        Some((best_prefix, _, _)) => {
            matched_prefix.as_os_str().len() > best_prefix.as_os_str().len()
        }
        None => true,
    };
    if is_more_specific {
        *best = Some((matched_prefix, root.to_string(), name.to_string()));
    }
}

/// Reconciles one (root, series name) pair.
#[allow(clippy::too_many_arguments)]
fn reconcile_one_series(
    config: &Config,
    state: &mut State,
    root: &str,
    name: &str,
    candidates: &BTreeMap<PathBuf, ScriptLogEntry>,
    resume_seconds: u64,
    completion_remaining_seconds: u64,
    home: Option<&Path>,
) {
    let expanded_root = expand_tilde(Path::new(root), home);
    let series_path = expanded_root.join(name);
    // A moved or deleted folder leaves its record untouched.
    if !series::is_series_dir(&series_path, &tracked_real_paths(config, home)) {
        return;
    }

    let excluded = exclude::excluded_paths_for_root(config, root, home);
    let Ok(ordered) =
        series::ordered_episodes_excluding(&series_path, &config.roots.video_extensions, &excluded)
    else {
        return;
    };
    if ordered.is_empty() {
        return;
    }

    let existing_index = state
        .series
        .iter()
        .position(|r| r.root == root && r.name == name);

    // A dropped or completed record only reacts to readings written after it
    // last changed; older entries are leftovers from the earlier watching.
    let cutoff = existing_index.and_then(|i| {
        let record = &state.series[i];
        matches!(record.status, Status::Dropped | Status::Completed).then_some(record.updated_at)
    });

    let start_index = match existing_index {
        Some(i) => {
            let record = &state.series[i];
            if matches!(record.status, Status::Dropped | Status::Completed) {
                // Any episode, including the first, counts as new activity.
                0
            } else {
                let target = expanded_root.join(&record.current_relative_path);
                let real_target = normalize(&target, home);
                match ordered
                    .iter()
                    .position(|p| normalize(p, home) == real_target)
                {
                    Some(idx) => idx,
                    // The tracked file is gone or excluded; skip rather than
                    // guess where "forward" starts.
                    None => return,
                }
            }
        }
        None => 0,
    };

    // Forward scan from `start_index`. Only `Resume` is a landing candidate,
    // and a later one overwrites an earlier one. `Completed` is handled below
    // when no `Resume` exists.
    //
    // `any_completed_found` covers the whole range, not just `start_index`.
    // A missing entry proves nothing: an episode may simply not have been
    // opened since the script was installed. A `Completed` reading comes from
    // a real entry, so it is enough on its own.
    let mut best: Option<(usize, f64)> = None;
    let mut any_completed_found = false;
    for (idx, file) in ordered.iter().enumerate().skip(start_index) {
        let real_file = normalize(file, home);
        let Some(candidate) = fresh_candidate(candidates, &real_file, cutoff) else {
            continue;
        };
        let Some(action) = classify_entry(candidate, resume_seconds, completion_remaining_seconds)
        else {
            continue;
        };
        if matches!(action, PlaybackAction::Completed) {
            any_completed_found = true;
        }
        if let PlaybackAction::Resume(pos) = action {
            best = Some((idx, pos));
        }
    }

    let Some((found_index, position)) = best else {
        // No `Resume` from `start_index` on. A `Completed` reading anywhere in
        // that range advances the cursor; without one, nothing changes.
        //
        // A never-tracked series gets a placeholder record at the first
        // episode for `apply_completion_advance` to overwrite.
        if !any_completed_found {
            return;
        }
        let index = match existing_index {
            Some(i) => i,
            None => {
                let identities = anitomy::episode_identity(&ordered);
                let start_identity = identities.get(start_index);
                state.series.push(SeriesRecord {
                    root: root.to_string(),
                    name: name.to_string(),
                    status: Status::InProgress,
                    current_relative_path: ordered[start_index]
                        .strip_prefix(&expanded_root)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| name.to_string()),
                    position_seconds: 0.0,
                    episode_number: start_identity.map(|id| id.episode_number),
                    updated_at: now_unix(),
                    mal_episode_awareness: None,
                    season_number: start_identity.and_then(|id| id.season),
                });
                state.series.len() - 1
            }
        };
        apply_completion_advance(
            state,
            index,
            name,
            &expanded_root,
            &ordered,
            candidates,
            start_index,
            cutoff,
            resume_seconds,
            completion_remaining_seconds,
            home,
        );
        return;
    };

    let new_relative = ordered[found_index]
        .strip_prefix(&expanded_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| name.to_string());

    // Readings that reach here for a dropped or completed record are already
    // newer than it, so even an identical one is new watching.
    let changed = match existing_index {
        Some(i) => {
            let record = &state.series[i];
            cutoff.is_some()
                || record.current_relative_path != new_relative
                || (record.position_seconds - position).abs() > f64::EPSILON
        }
        None => true,
    };
    if !changed {
        return;
    }

    let identities = anitomy::episode_identity(&ordered);
    let episode_number = identities.get(found_index).map(|id| id.episode_number);
    let season_number = identities.get(found_index).and_then(|id| id.season);
    let now = now_unix();

    match existing_index {
        Some(i) => {
            let record = &mut state.series[i];
            record.status = Status::InProgress;
            record.current_relative_path = new_relative;
            record.position_seconds = position;
            record.episode_number = episode_number;
            record.season_number = season_number;
            record.updated_at = now;
        }
        None => {
            state.series.push(SeriesRecord {
                root: root.to_string(),
                name: name.to_string(),
                status: Status::InProgress,
                current_relative_path: new_relative,
                position_seconds: position,
                episode_number,
                updated_at: now,
                mal_episode_awareness: None,
                season_number,
            });
        }
    }
}

/// Advances the cursor of `state.series[i]` once its tracked episode is
/// confirmed complete and no `Resume` exists ahead. Skips every consecutive
/// episode that also reads `Completed` and lands at position 0 of the first
/// one that doesn't. If the walk runs out of episodes, the record becomes
/// `Completed`: the path and position stay as they were, but `episode_number`
/// is set to the final episode's.
#[allow(clippy::too_many_arguments)]
fn apply_completion_advance(
    state: &mut State,
    i: usize,
    name: &str,
    expanded_root: &Path,
    ordered: &[PathBuf],
    candidates: &BTreeMap<PathBuf, ScriptLogEntry>,
    start_index: usize,
    cutoff: Option<u64>,
    resume_seconds: u64,
    completion_remaining_seconds: u64,
    home: Option<&Path>,
) {
    let mut cursor = start_index;
    let identities = anitomy::episode_identity(ordered);
    loop {
        if cursor + 1 >= ordered.len() {
            let episode_number = identities.get(cursor).map(|id| id.episode_number);
            let season_number = identities.get(cursor).and_then(|id| id.season);
            let record = &mut state.series[i];
            record.status = Status::Completed;
            record.episode_number = episode_number;
            record.season_number = season_number;
            record.updated_at = now_unix();
            return;
        }
        cursor += 1;

        let real_file = normalize(&ordered[cursor], home);
        let still_completed = match fresh_candidate(candidates, &real_file, cutoff) {
            Some(candidate) => matches!(
                classify_entry(candidate, resume_seconds, completion_remaining_seconds),
                Some(PlaybackAction::Completed)
            ),
            // No entry is not completion evidence, so the walk stops.
            None => false,
        };

        if !still_completed {
            let new_relative = ordered[cursor]
                .strip_prefix(expanded_root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| name.to_string());
            let episode_number = identities.get(cursor).map(|id| id.episode_number);
            let season_number = identities.get(cursor).and_then(|id| id.season);
            let record = &mut state.series[i];
            record.status = Status::InProgress;
            record.current_relative_path = new_relative;
            record.position_seconds = 0.0;
            record.episode_number = episode_number;
            record.season_number = season_number;
            record.updated_at = now_unix();
            return;
        }
        // Completed too; keep walking.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::tempdir;

    fn default_config(root: &Path) -> Config {
        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::File::create(path).unwrap();
    }

    /// Writes a script-log entry for `source` that quit at `position_seconds`
    /// with no known duration, under a name distinct from other entries in the
    /// test.
    fn write_entry(log_dir: &Path, entry_name: &str, source: &Path, position_seconds: f64) {
        write_script_entry(
            log_dir,
            entry_name,
            source,
            None,
            Some(position_seconds),
            "quit",
        );
    }

    /// Writes a `tsuzuki.lua`-style script-log entry file for `source` in
    /// `log_dir`, under a name distinct from other entries in the test.
    fn write_script_entry(
        log_dir: &Path,
        entry_name: &str,
        source: &Path,
        duration: Option<f64>,
        position: Option<f64>,
        reason: &str,
    ) {
        fs::create_dir_all(log_dir).unwrap();
        fs::write(
            log_dir.join(entry_name),
            format!(
                "# {}\nduration={}\nposition={}\nreason={reason}\n",
                source.display(),
                duration.map(|d| d.to_string()).unwrap_or_default(),
                position.map(|p| p.to_string()).unwrap_or_default(),
            ),
        )
        .unwrap();
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn a_noise_reading_creates_no_record() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let log_dir = dir.path().join("mpv-log");
        // 45s is below the default 120s `resume_seconds`.
        write_entry(&log_dir, "entry1", &series_path.join("ep01.mkv"), 45.0);

        let config = default_config(&root);
        let mut state = State::default();

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        assert!(
            state.series.is_empty(),
            "a sub-resume_seconds reading must never create a record"
        );
    }

    #[test]
    fn a_noise_reading_leaves_an_existing_real_record_untouched() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 900.0,
                episode_number: Some(1),
                updated_at: 12345,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        // A brief reopen leaves a 45s entry that must not overwrite the real 900s.
        write_entry(&log_dir, "entry1", &series_path.join("ep01.mkv"), 45.0);

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.position_seconds, 900.0,
            "the real position must survive a noise reading unchanged"
        );
        assert_eq!(record.updated_at, 12345, "updated_at must not move either");
    }

    #[test]
    fn reconcile_with_no_entries_and_no_record_creates_no_record() {
        // Absence alone must not create a record.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");

        let mut state = State::default();
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        assert!(state.series.is_empty());
    }

    #[test]
    fn single_run_chaining_resolves_a_multi_step_binge_in_one_call() {
        // ep01 has no entry (completed) and ep03 has a real position; one call
        // resolves from a record still on ep01.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("ep03.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        // No entry for ep01 or ep02.
        write_entry(&log_dir, "entry3", &series_path.join("ep03.mkv"), 300.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 1100.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.current_relative_path, "Monster/ep03.mkv");
        assert_eq!(record.position_seconds, 300.0);
        assert_eq!(record.status, Status::InProgress);
        assert!(record.updated_at > 1);
    }

    #[test]
    fn a_record_whose_series_folder_no_longer_resolves_is_skipped_not_crashed_or_deleted() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();
        // "Monster" is never created on disk, like a moved or deleted folder.

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");

        let original = SeriesRecord {
            root: root.to_string_lossy().into_owned(),
            name: "Monster".to_string(),
            status: Status::InProgress,
            current_relative_path: "Monster/ep05.mkv".to_string(),
            position_seconds: 400.0,
            episode_number: Some(5),
            updated_at: 999,
            mal_episode_awareness: None,
            season_number: None,
        };
        let mut state = State {
            series: vec![original.clone()],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        assert_eq!(
            state.series,
            vec![original],
            "an orphaned record must be left completely inert, not crashed on or removed"
        );
    }

    #[test]
    fn a_real_bookmark_creates_a_brand_new_record_for_a_never_tracked_series() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        write_entry(&log_dir, "entry1", &series_path.join("ep02.mkv"), 500.0);

        let mut state = State::default();
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 500.0);
    }

    #[test]
    fn reconcile_own_write_is_stable_across_a_second_call_with_nothing_new_on_disk() {
        // `updated_at` is pinned to 1 so any rewrite shows up as a change.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        write_entry(&log_dir, "entry1", &series_path.join("ep01.mkv"), 400.0);

        let mut state = State::default();
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));
        state.series[0].updated_at = 1;
        let after_first_call = state.series[0].clone();

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        assert_eq!(state.series[0], after_first_call);
    }

    #[test]
    fn a_dropped_series_with_a_bookmark_older_than_the_drop_stays_dropped_across_reconcile_calls() {
        // A leftover bookmark must never reactivate a dropped series, however
        // many times reconcile runs.
        let dir = tempdir().unwrap();
        let (root, series_path, config, log_dir) = monster_library(dir.path(), 2);
        write_entry(&log_dir, "w1", &series_path.join("ep01.mkv"), 300.0);
        set_mtime(&log_dir.join("w1"), at(CUTOFF - 60));
        let original = record_at(
            &root,
            "Monster",
            Status::Dropped,
            "Monster/ep01.mkv",
            300.0,
            1,
        );
        let mut state = State {
            series: vec![original.clone()],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);
        run_reconcile(&config, &mut state, &log_dir);

        assert_eq!(state.series, vec![original]);
    }

    #[test]
    fn dropped_or_completed_record_reopened_at_the_stored_position_becomes_in_progress() {
        // Reopening and quitting right away rewrites the bookmark with the
        // same path and position; after the drop that is still new watching.
        for status in [Status::Dropped, Status::Completed] {
            let dir = tempdir().unwrap();
            let (root, series_path, config, log_dir) = monster_library(dir.path(), 2);
            write_entry(&log_dir, "w1", &series_path.join("ep01.mkv"), 300.0);
            set_mtime(&log_dir.join("w1"), at(CUTOFF + 60));
            let mut state = State {
                series: vec![record_at(
                    &root,
                    "Monster",
                    status,
                    "Monster/ep01.mkv",
                    300.0,
                    1,
                )],
                mal_links: Vec::new(),
            };

            run_reconcile(&config, &mut state, &log_dir);

            let record = &state.series[0];
            assert_eq!(record.status, Status::InProgress, "{status:?}");
            assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
            assert_eq!(record.position_seconds, 300.0);
            assert!(
                record.updated_at > CUTOFF,
                "{status:?} must record the change"
            );
        }
    }

    #[test]
    fn opening_an_episode_of_a_dropped_series_flips_it_back_to_in_progress() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        // Opens episode 1 directly.
        write_entry(&log_dir, "entry1", &series_path.join("ep01.mkv"), 400.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::Dropped,
                current_relative_path: "Monster/ep02.mkv".to_string(),
                position_seconds: 0.0,
                episode_number: None,
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 400.0);
    }

    #[test]
    fn a_completed_series_reopened_on_episode_one_restarts_as_in_progress() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        // Opens episode 1 directly.
        write_entry(&log_dir, "entry1", &series_path.join("ep01.mkv"), 400.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::Completed,
                current_relative_path: "Monster/ep02.mkv".to_string(),
                position_seconds: 1400.0,
                episode_number: Some(2),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 400.0);
    }

    #[test]
    fn an_unchanged_bookmark_never_flips_a_paused_series_back_to_in_progress() {
        // The paused episode's entry is unchanged; only a change may reactivate.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        write_entry(&log_dir, "entry1", &series_path.join("ep01.mkv"), 400.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::Paused,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 400.0,
                episode_number: Some(1),
                updated_at: 42,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::Paused);
        assert_eq!(record.updated_at, 42);
    }

    #[test]
    fn a_paused_series_reactivates_when_a_new_real_bookmark_appears_on_a_later_episode() {
        // Opening a later episode directly moves a paused series forward and
        // reactivates it.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("ep03.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        // ep01's own old bookmark is still there, completely unchanged...
        write_entry(&log_dir, "entry1", &series_path.join("ep01.mkv"), 200.0);
        // ...but the user has since opened ep03 directly and quit mid-file.
        write_entry(&log_dir, "entry3", &series_path.join("ep03.mkv"), 500.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::Paused,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 200.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.status,
            Status::InProgress,
            "a genuinely later real bookmark must reactivate a paused series"
        );
        assert_eq!(record.current_relative_path, "Monster/ep03.mkv");
        assert_eq!(record.position_seconds, 500.0);
        assert!(record.updated_at > 1);
    }

    #[test]
    fn a_duration_based_completed_reading_is_treated_like_absence_and_scanning_continues() {
        // 5.0s duration, `resume_seconds=1`, `completion_remaining_seconds=2`:
        // quitting at 4.0s classifies as Completed, so the scan continues past it.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        fs::create_dir_all(&series_path).unwrap();
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let mut config = default_config(&root);
        config.thresholds.resume_seconds = 1;
        config.thresholds.completion_remaining_seconds = 2;
        let log_dir = dir.path().join("mpv-log");

        // ep01: 4.0s of 5.0s -> Completed.
        write_script_entry(
            &log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(5.0),
            Some(4.0),
            "quit",
        );
        // ep02: 1.5s of 5.0s -> Resume.
        write_script_entry(
            &log_dir,
            "entry2",
            &series_path.join("ep02.mkv"),
            Some(5.0),
            Some(1.5),
            "quit",
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.current_relative_path, "Monster/ep02.mkv",
            "a Completed-classified entry on ep01 must not be the landing point"
        );
        assert_eq!(record.position_seconds, 1.5);
    }

    #[test]
    fn a_completed_reading_on_the_last_episode_marks_completed_instead_of_freezing_or_updating_in_place()
     {
        // With one episode, completing it has no next episode, so the series
        // becomes Completed.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        fs::create_dir_all(&series_path).unwrap();
        touch(&series_path.join("ep01.mkv"));

        let mut config = default_config(&root);
        config.thresholds.resume_seconds = 1;
        config.thresholds.completion_remaining_seconds = 2;
        let log_dir = dir.path().join("mpv-log");

        let mut state = State::default();

        // Call 1: 1.5s of 5.0s -> Resume.
        write_script_entry(
            &log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(5.0),
            Some(1.5),
            "quit",
        );
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.position_seconds, 1.5);
        assert_eq!(record.status, Status::InProgress);

        // Call 2: 4.0s of 5.0s -> Completed on the only episode.
        write_script_entry(
            &log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(5.0),
            Some(4.0),
            "quit",
        );
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.status,
            Status::Completed,
            "completing the series' actual last episode must mark it Completed"
        );
        // The stored position stays as it was.
        assert_eq!(record.position_seconds, 1.5);
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        // Sync reads the finale as fully watched.
        assert_eq!(crate::sync::local_effective_episode(record), 1);

        // Overview shows "Rewatch?", not "Resume".
        assert_eq!(
            crate::overview::start_display_word(Some(record.status)),
            "Rewatch?"
        );

        // Call 3: nothing changed on disk, so nothing changes.
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::Completed);
        assert_eq!(record.position_seconds, 1.5);
    }

    #[test]
    fn absence_alone_never_advances_or_mutates_the_record() {
        // Absence alone never justifies a mutation, so ep01's stored position
        // must survive.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");

        // No entry for ep01 and nothing for ep02.
        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 1100.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 1100.0);
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.updated_at, 1);
    }

    #[test]
    fn a_sub_threshold_reading_on_the_next_episode_does_not_corroborate_the_current_ones_absence() {
        // ep01's entry is gone and ep02 has only a sub-threshold reading, so the
        // record stays on ep01 at its last real position.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");

        // ep02: 60s, below the 120s default `resume_seconds`.
        write_entry(&log_dir, "entry2", &series_path.join("ep02.mkv"), 60.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 600.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 600.0);
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.updated_at, 1);
    }

    #[test]
    fn current_episode_completes_and_next_episode_already_has_its_own_real_resume_bookmark() {
        // A real `Resume` ahead is found by the scan; completion advance never runs.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        // No entry for ep01; a real bookmark on ep02.
        write_entry(&log_dir, "entry2", &series_path.join("ep02.mkv"), 700.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 1100.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(
            record.position_seconds, 700.0,
            "must land on ep02's own real position, not position 0"
        );
        assert_eq!(record.status, Status::InProgress);
    }

    #[test]
    fn current_episode_completes_then_next_also_completes_then_a_third_blank_episode_in_one_call() {
        // ep01 has no entry, ep02 is Completed and ep03 is blank, so one call lands
        // on ep03.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        fs::create_dir_all(&series_path).unwrap();
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("ep03.mkv"));

        let mut config = default_config(&root);
        config.thresholds.resume_seconds = 1;
        config.thresholds.completion_remaining_seconds = 2;
        let log_dir = dir.path().join("mpv-log");
        // ep01: no entry. ep02: 4.0s of 5.0s -> Completed.
        // ep03: no entry.
        write_script_entry(
            &log_dir,
            "entry2",
            &series_path.join("ep02.mkv"),
            Some(5.0),
            Some(4.0),
            "quit",
        );

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 1100.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.current_relative_path, "Monster/ep03.mkv",
            "must chain past both ep01's absence and ep02's own separate completion in one call"
        );
        assert_eq!(record.position_seconds, 0.0);
        assert_eq!(record.status, Status::InProgress);
    }

    #[test]
    fn single_run_chains_through_both_completion_signals_and_an_ordinary_resume() {
        // ep01 has no entry, ep02 is Completed and ep03 has a real position, so one
        // call lands on ep03.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        fs::create_dir_all(&series_path).unwrap();
        // ep01 has no entry, so no content is needed.
        touch(&series_path.join("ep01.mkv"));
        // ep02 and ep03 carry a 5.0s duration in their entries.
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("ep03.mkv"));

        let mut config = default_config(&root);
        config.thresholds.resume_seconds = 1;
        config.thresholds.completion_remaining_seconds = 2;
        let log_dir = dir.path().join("mpv-log");

        // ep01: no entry.
        // ep02: 4.0s of 5.0s -> Completed.
        write_script_entry(
            &log_dir,
            "entry2",
            &series_path.join("ep02.mkv"),
            Some(5.0),
            Some(4.0),
            "quit",
        );
        // ep03: 1.5s of 5.0s -> Resume.
        write_script_entry(
            &log_dir,
            "entry3",
            &series_path.join("ep03.mkv"),
            Some(5.0),
            Some(1.5),
            "quit",
        );

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 3.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.current_relative_path, "Monster/ep03.mkv",
            "must chain past both ep01's absence and ep02's Completed reading in one call"
        );
        assert_eq!(record.position_seconds, 1.5);
        assert_eq!(record.status, Status::InProgress);
    }

    #[test]
    fn composable_roots_resolve_to_the_most_specific_matching_root() {
        let dir = tempdir().unwrap();
        let anime_root = dir.path().join("Anime");
        let rewatches_root = anime_root.join("Rewatches");
        let series_path = rewatches_root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let mut config = Config::default();
        config
            .roots
            .paths
            .push(anime_root.to_string_lossy().into_owned());
        config
            .roots
            .paths
            .push(rewatches_root.to_string_lossy().into_owned());

        let log_dir = dir.path().join("mpv-log");
        write_entry(&log_dir, "entry1", &series_path.join("ep01.mkv"), 300.0);

        let mut state = State::default();
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        assert_eq!(state.series.len(), 1);
        let record = &state.series[0];
        assert_eq!(record.root, rewatches_root.to_string_lossy());
        assert_eq!(record.name, "Monster");
    }

    #[test]
    fn a_stale_record_for_a_nested_root_as_its_parents_series_is_left_untouched() {
        let dir = tempdir().unwrap();
        let anime_root = dir.path().join("Anime");
        let rewatches_root = anime_root.join("Rewatches");
        let series_path = rewatches_root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let mut config = Config::default();
        config
            .roots
            .paths
            .push(anime_root.to_string_lossy().into_owned());
        config
            .roots
            .paths
            .push(rewatches_root.to_string_lossy().into_owned());

        let log_dir = dir.path().join("mpv-log");
        write_entry(&log_dir, "entry1", &series_path.join("ep01.mkv"), 300.0);

        let stale = SeriesRecord {
            root: anime_root.to_string_lossy().into_owned(),
            name: "Rewatches".to_string(),
            status: Status::InProgress,
            current_relative_path: "Rewatches/Monster/ep01.mkv".to_string(),
            position_seconds: 10.0,
            episode_number: Some(1),
            updated_at: 5,
            mal_episode_awareness: None,
            season_number: None,
        };
        let mut state = State {
            series: vec![stale.clone()],
            mal_links: Vec::new(),
        };
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        assert_eq!(state.series.len(), 2);
        assert_eq!(state.series[0], stale);
        assert_eq!(state.series[1].root, rewatches_root.to_string_lossy());
        assert_eq!(state.series[1].name, "Monster");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_series_folder_reconciles_correctly() {
        // A symlinked series folder pointing outside the root must still
        // reconcile.
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let real_storage = dir.path().join("real_storage").join("Dungeon Meshi");
        let release_dir = real_storage.join("[1080p WEBRip][EMBER]");
        fs::create_dir_all(&release_dir).unwrap();
        touch(&release_dir.join("Dungeon Meshi - S01E01.mkv"));
        symlink(&real_storage, root.join("Dungeon Meshi")).unwrap();

        // A plain sibling series keeps working.
        touch(&root.join("Shokei Shoujo").join("ep01.mkv"));

        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        write_entry(
            &log_dir,
            "entry-symlinked",
            &root
                .join("Dungeon Meshi")
                .join("[1080p WEBRip][EMBER]")
                .join("Dungeon Meshi - S01E01.mkv"),
            685.852,
        );
        write_entry(
            &log_dir,
            "entry-ordinary",
            &root.join("Shokei Shoujo").join("ep01.mkv"),
            551.259,
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let symlinked = state.find(&root.to_string_lossy(), "Dungeon Meshi");
        assert!(
            symlinked.is_some(),
            "a real bookmark for a symlinked series folder must create a record, not be \
             silently dropped"
        );
        let symlinked = symlinked.unwrap();
        assert_eq!(symlinked.status, Status::InProgress);
        assert_eq!(symlinked.position_seconds, 685.852);

        let ordinary = state
            .find(&root.to_string_lossy(), "Shokei Shoujo")
            .expect("the ordinary, non-symlinked sibling series must still reconcile correctly");
        assert_eq!(ordinary.status, Status::InProgress);
        assert_eq!(ordinary.position_seconds, 551.259);
    }

    // --- reading classification ---

    fn reading(duration: Option<f64>, position: Option<f64>, reason: &str) -> ScriptLogEntry {
        ScriptLogEntry {
            source_path: PathBuf::from("/Anime/Show/ep01.mkv"),
            duration,
            position,
            reason: reason.to_string(),
            title: None,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn classify_entry_eof_is_completed_regardless_of_position_or_duration() {
        for entry in [
            reading(None, Some(3.0), "eof"),
            reading(None, None, "eof"),
            reading(Some(1000.0), Some(3.0), "eof"),
        ] {
            assert_eq!(
                classify_entry(&entry, 120, 180),
                Some(PlaybackAction::Completed),
                "{entry:?}"
            );
        }
    }

    #[test]
    fn classify_entry_position_below_resume_threshold_is_no_signal() {
        assert_eq!(
            classify_entry(&reading(None, Some(119.9), "stop"), 120, 180),
            None
        );
        assert_eq!(
            classify_entry(&reading(None, Some(0.0), "stop"), 120, 180),
            None
        );
    }

    #[test]
    fn classify_entry_position_at_and_above_resume_threshold_resumes() {
        assert_eq!(
            classify_entry(&reading(None, Some(120.0), "quit"), 120, 180),
            Some(PlaybackAction::Resume(120.0))
        );
        assert_eq!(
            classify_entry(&reading(None, Some(120.1), "quit"), 120, 180),
            Some(PlaybackAction::Resume(120.1))
        );
    }

    #[test]
    fn classify_entry_non_eof_with_no_position_is_no_signal() {
        assert_eq!(
            classify_entry(&reading(Some(1000.0), None, "quit"), 120, 180),
            None
        );
    }

    #[test]
    fn classify_entry_remaining_above_at_and_below_the_completion_threshold() {
        // Duration 1000, completion window 180s: remaining 180.1, 180.0, 179.9.
        assert_eq!(
            classify_entry(&reading(Some(1000.0), Some(819.9), "quit"), 120, 180),
            Some(PlaybackAction::Resume(819.9))
        );
        assert_eq!(
            classify_entry(&reading(Some(1000.0), Some(820.0), "quit"), 120, 180),
            Some(PlaybackAction::Completed)
        );
        assert_eq!(
            classify_entry(&reading(Some(1000.0), Some(820.1), "quit"), 120, 180),
            Some(PlaybackAction::Completed)
        );
    }

    #[test]
    fn classify_entry_short_file_position_below_resume_wins_over_completion() {
        // Duration 100, position 50: inside the completion window, but below `resume_seconds`.
        assert_eq!(
            classify_entry(&reading(Some(100.0), Some(50.0), "quit"), 120, 180),
            None
        );
    }

    #[test]
    fn classify_entry_without_a_duration_never_completes_from_position_alone() {
        assert_eq!(
            classify_entry(&reading(None, Some(99_999.0), "quit"), 120, 180),
            Some(PlaybackAction::Resume(99_999.0))
        );
    }

    #[test]
    fn classify_entry_non_eof_reasons_all_classify_by_position_and_duration() {
        for reason in ["stop", "quit", "error", "unknown"] {
            assert_eq!(
                classify_entry(&reading(Some(1000.0), Some(500.0), reason), 120, 180),
                Some(PlaybackAction::Resume(500.0)),
                "{reason}"
            );
        }
    }

    // --- end-to-end `reconcile()`, both candidate sources ---

    #[test]
    fn a_never_tracked_single_episode_series_completed_via_script_eof_marks_completed() {
        // A brand-new series whose only activity is a `Completed` reading still
        // gets a record.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        let record = state
            .find(&root.to_string_lossy(), "Monster")
            .expect("a never-tracked series completed on its only episode must get a record");
        assert_eq!(
            record.status,
            Status::Completed,
            "the series' one and only episode completing must mark it Completed, not leave it \
             invisible"
        );
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 0.0);
    }

    #[test]
    fn a_never_tracked_series_completed_on_a_non_last_episode_lands_on_the_next_one() {
        // ep01 completes with no prior record and a next episode exists: lands on
        // ep02 at position 0.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        let record = state
            .find(&root.to_string_lossy(), "Monster")
            .expect("a never-tracked series completed on a non-last episode must get a record");
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 0.0);
    }

    #[test]
    fn a_never_tracked_series_completed_via_a_duration_based_reading_also_gets_a_record() {
        // Same for a quit reading inside the completion window.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        fs::create_dir_all(&series_path).unwrap();
        touch(&series_path.join("ep01.mkv"));

        let mut config = default_config(&root);
        config.thresholds.resume_seconds = 1;
        config.thresholds.completion_remaining_seconds = 2;
        let log_dir = dir.path().join("mpv-log");
        // 4.0s of 5.0s -> Completed.
        write_script_entry(
            &log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(5.0),
            Some(4.0),
            "quit",
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").expect(
            "a never-tracked series completed via a duration-based reading must get a record",
        );
        assert_eq!(record.status, Status::Completed);
    }

    #[test]
    fn a_dropped_record_with_a_fresh_completed_reading_advances_to_the_next_episode() {
        // A finished episode read after the drop is new activity, like a `Resume`.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        // A fresh `eof` on ep01.
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
        );

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::Dropped,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 0.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 0.0);
        assert_eq!(record.episode_number, Some(2));
    }

    #[test]
    fn a_completed_record_with_a_fresh_completed_reading_advances_to_the_next_episode() {
        // Rewatching a finished series: a finished episode is new activity.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        // A fresh `eof` on ep01.
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
        );

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::Completed,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 0.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 0.0);
        assert_eq!(record.episode_number, Some(2));
    }

    #[test]
    fn script_only_eof_on_the_last_episode_marks_completed_flagship() {
        // A lone `eof` on the last episode completes the series, with no earlier
        // reading for it.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        write_script_entry(
            &script_log_dir,
            "entry-ep02",
            &series_path.join("ep02.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
        );

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep02.mkv".to_string(),
                position_seconds: 900.0,
                episode_number: Some(2),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.status,
            Status::Completed,
            "a script-only eof reading on the series' last episode must mark it Completed"
        );
        // The stored position and path are untouched.
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 900.0);
    }

    #[test]
    fn a_multi_hop_completion_chain_lands_episode_number_on_the_true_final_episode_before_completing()
     {
        // A walk chaining through several completed episodes to the end must set
        // `episode_number` to the final episode's.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("ep03.mkv"));
        touch(&series_path.join("ep04.mkv"));
        touch(&series_path.join("ep05.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        // ep02 through ep05 all classify Completed (script `eof`).
        for (file_name, entry_name) in [
            ("ep02.mkv", "entry-ep02"),
            ("ep03.mkv", "entry-ep03"),
            ("ep04.mkv", "entry-ep04"),
            ("ep05.mkv", "entry-ep05"),
        ] {
            write_script_entry(
                &script_log_dir,
                entry_name,
                &series_path.join(file_name),
                Some(1420.0),
                Some(1415.0),
                "eof",
            );
        }

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep02.mkv".to_string(),
                position_seconds: 1200.0,
                episode_number: Some(2),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.status,
            Status::Completed,
            "chaining through every remaining episode's completion must still mark it Completed"
        );
        assert_eq!(
            record.episode_number,
            Some(5),
            "episode_number must land on the true final episode (5), not freeze at the \
             starting cursor's episode (2), regardless of how many episodes were chained \
             through"
        );
        // Path and position are unchanged; only `episode_number` moves.
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 1200.0);
    }

    #[test]
    fn script_only_eof_on_a_non_last_episode_advances_the_cursor() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("ep03.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        // ep01: script-only `eof`. ep02 and ep03: nothing from either source.
        write_script_entry(
            &script_log_dir,
            "entry-ep01",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1418.0),
            "eof",
        );

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 1100.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 0.0);
        assert_eq!(record.status, Status::InProgress);
    }

    #[test]
    fn script_reason_stop_with_a_real_position_is_treated_as_an_ordinary_resume() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            None,
            Some(400.0),
            "stop",
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 400.0);
    }

    #[test]
    fn script_reason_stop_below_resume_threshold_creates_no_record() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        // 45s is below the default 120s `resume_seconds`.
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            None,
            Some(45.0),
            "stop",
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        assert!(
            state.series.is_empty(),
            "a sub-resume_seconds script reading must never create a record"
        );
    }

    #[test]
    fn script_reason_redirect_is_discarded_and_never_becomes_a_candidate() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        // A `redirect` entry never counts as activity, whatever its position.
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(1000.0),
            Some(950.0),
            "redirect",
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        assert!(
            state.series.is_empty(),
            "a redirect entry must never become a candidate, regardless of its position"
        );
    }

    #[test]
    fn a_malformed_script_log_file_alongside_a_valid_one_does_not_abort_the_scan() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        fs::create_dir_all(&script_log_dir).unwrap();
        fs::write(
            script_log_dir.join("malformed"),
            "garbage, not an entry at all\n",
        )
        .unwrap();
        write_script_entry(
            &script_log_dir,
            "valid",
            &series_path.join("ep01.mkv"),
            None,
            Some(400.0),
            "quit",
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.position_seconds, 400.0,
            "the valid entry must still be picked up despite a malformed sibling file"
        );
    }

    #[test]
    fn reconcile_twice_against_a_script_log_entry_older_than_the_drop_is_a_no_op() {
        // Script-log entries persist on disk, so an `eof` written before the
        // drop must not look like new activity on any later reconcile.
        let dir = tempdir().unwrap();
        let (root, series_path, config, script_log_dir) = monster_library(dir.path(), 2);
        script_entry_at(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1418.0),
            "eof",
            CUTOFF - 60,
        );
        let original = record_at(
            &root,
            "Monster",
            Status::Dropped,
            "Monster/ep01.mkv",
            300.0,
            1,
        );
        let mut state = State {
            series: vec![original.clone()],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &script_log_dir);
        run_reconcile(&config, &mut state, &script_log_dir);

        assert_eq!(state.series, vec![original]);
    }

    // --- auto_pause_days ---

    fn idle_in_progress_record(root: &Path, name: &str, updated_at: u64) -> SeriesRecord {
        SeriesRecord {
            root: root.to_string_lossy().into_owned(),
            name: name.to_string(),
            status: Status::InProgress,
            current_relative_path: format!("{name}/ep01.mkv"),
            position_seconds: 500.0,
            episode_number: Some(1),
            updated_at,
            mal_episode_awareness: None,
            season_number: None,
        }
    }

    #[test]
    fn auto_pause_days_zero_never_pauses_no_matter_how_old() {
        // 0 disables auto-pause entirely.
        let mut state = State {
            series: vec![idle_in_progress_record(Path::new("~/Anime"), "Monster", 0)],
            mal_links: Vec::new(),
        };
        apply_idle_auto_pause(&mut state, 0, 10_000_000_000);
        assert_eq!(state.series[0].status, Status::InProgress);
        assert_eq!(
            state.series[0].updated_at, 0,
            "a disabled pass must not touch updated_at either"
        );
    }

    #[test]
    fn a_nonzero_auto_pause_days_pauses_a_genuinely_idle_series() {
        let now = 10 * 86_400; // day 10
        let updated_at = 0; // idle for exactly 10 days
        let mut state = State {
            series: vec![idle_in_progress_record(
                Path::new("~/Anime"),
                "Monster",
                updated_at,
            )],
            mal_links: Vec::new(),
        };
        apply_idle_auto_pause(&mut state, 10, now);
        let record = &state.series[0];
        assert_eq!(record.status, Status::Paused);
        assert_eq!(
            record.updated_at, now,
            "the transition itself is a genuine, persisted change and must bump updated_at, \
             matching pause_drop::pause's own precedent"
        );
        // Idleness only reaches Paused; position and episode are unchanged.
        assert_eq!(record.position_seconds, 500.0);
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
    }

    #[test]
    fn a_series_idle_for_fewer_than_auto_pause_days_stays_in_progress() {
        let now = 5 * 86_400; // day 5
        let updated_at = 0;
        let mut state = State {
            series: vec![idle_in_progress_record(
                Path::new("~/Anime"),
                "Monster",
                updated_at,
            )],
            mal_links: Vec::new(),
        };
        // 10-day threshold, 5 days idle.
        apply_idle_auto_pause(&mut state, 10, now);
        assert_eq!(state.series[0].status, Status::InProgress);
        assert_eq!(state.series[0].updated_at, updated_at);
    }

    #[test]
    fn already_paused_and_dropped_records_are_left_alone_regardless_of_age() {
        let ancient = 0u64;
        let now = 100 * 86_400;
        let mut state = State {
            series: vec![
                SeriesRecord {
                    status: Status::Paused,
                    ..idle_in_progress_record(Path::new("~/Anime"), "AlreadyPaused", ancient)
                },
                SeriesRecord {
                    status: Status::Dropped,
                    ..idle_in_progress_record(Path::new("~/Anime"), "AlreadyDropped", ancient)
                },
                SeriesRecord {
                    status: Status::Completed,
                    ..idle_in_progress_record(Path::new("~/Anime"), "AlreadyCompleted", ancient)
                },
            ],
            mal_links: Vec::new(),
        };
        apply_idle_auto_pause(&mut state, 1, now);
        assert_eq!(state.series[0].status, Status::Paused);
        assert_eq!(
            state.series[0].updated_at, ancient,
            "no transition occurred, no bump"
        );
        assert_eq!(state.series[1].status, Status::Dropped);
        assert_eq!(
            state.series[1].updated_at, ancient,
            "Dropped -> Paused does not exist; idleness must never invent it"
        );
        assert_eq!(state.series[2].status, Status::Completed);
        assert_eq!(
            state.series[2].updated_at, ancient,
            "a finished series is never idle-paused"
        );
    }

    #[test]
    fn reconcile_wires_auto_pause_days_end_to_end_and_never_immediately_repauses_fresh_activity() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let idle_series = root.join("Monster");
        let fresh_series = root.join("Naruto");
        touch(&idle_series.join("ep01.mkv"));
        touch(&fresh_series.join("ep01.mkv"));

        let mut config = default_config(&root);
        config.thresholds.auto_pause_days = 7;
        let log_dir = dir.path().join("mpv-log");

        let long_ago = now_unix().saturating_sub(30 * 86_400);
        let mut state = State {
            series: vec![
                // No activity this run; idle 30 days against a 7-day threshold.
                idle_in_progress_record(&root, "Monster", long_ago),
            ],
            mal_links: Vec::new(),
        };

        // Naruto has a new bookmark in this very call, so it must not be
        // idle-paused.
        write_entry(&log_dir, "entry1", &fresh_series.join("ep01.mkv"), 300.0);

        reconcile_with_script_log_dirs(&config, &mut state, None, std::slice::from_ref(&log_dir));

        let monster = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            monster.status,
            Status::Paused,
            "a genuinely idle in_progress series must be auto-paused end to end"
        );

        let naruto = state.find(&root.to_string_lossy(), "Naruto").unwrap();
        assert_eq!(
            naruto.status,
            Status::InProgress,
            "fresh activity observed in this same reconcile pass must never be immediately \
             auto-paused"
        );
    }

    #[test]
    fn set_mtime_helper_actually_changes_mtime() {
        // Guards against `set_mtime` silently doing nothing.
        let dir = tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, b"x").unwrap();
        let when = SystemTime::now() - Duration::from_secs(3600);
        set_mtime(&path, when);
        let got = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(got <= SystemTime::now() - Duration::from_secs(3000));
    }

    fn in_progress_record(root: &Path, position: f64) -> SeriesRecord {
        SeriesRecord {
            root: root.to_string_lossy().into_owned(),
            name: "Monster".to_string(),
            status: Status::InProgress,
            current_relative_path: "Monster/ep01.mkv".to_string(),
            position_seconds: position,
            episode_number: Some(1),
            updated_at: 1,
            mal_episode_awareness: None,
            season_number: None,
        }
    }

    fn reconcile_with_one_script_stop(position: f64, record: SeriesRecord, root: &Path) -> State {
        let dir = tempdir().unwrap();
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        let config = default_config(root);
        let script_log_dir = dir.path().join("mpv-log");
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            None,
            Some(position),
            "stop",
        );
        let mut state = State {
            series: vec![record],
            mal_links: Vec::new(),
        };
        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );
        state
    }

    #[test]
    fn reconcile_script_stop_matching_an_in_progress_record_leaves_it_byte_identical() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let record = in_progress_record(&root, 400.0);

        let state = reconcile_with_one_script_stop(400.0, record.clone(), &root);

        assert_eq!(state.series, vec![record]);
    }

    #[test]
    fn reconcile_script_stop_just_below_resume_seconds_leaves_an_existing_record_untouched() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let record = in_progress_record(&root, 900.0);

        let state = reconcile_with_one_script_stop(119.9, record.clone(), &root);

        assert_eq!(state.series, vec![record]);
    }

    #[test]
    fn reconcile_script_stop_exactly_at_resume_seconds_counts_as_real_activity() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let record = in_progress_record(&root, 900.0);

        let state = reconcile_with_one_script_stop(120.0, record, &root);

        assert_eq!(state.series.len(), 1);
        assert_eq!(state.series[0].position_seconds, 120.0);
        assert_eq!(state.series[0].status, Status::InProgress);
        assert_eq!(state.series[0].current_relative_path, "Monster/ep01.mkv");
        assert_eq!(state.series[0].episode_number, Some(1));
    }

    #[test]
    fn reconcile_script_stop_exactly_at_resume_seconds_creates_a_record_for_an_untracked_series() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        let config = default_config(&root);
        let script_log_dir = dir.path().join("mpv-log");
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            None,
            Some(120.0),
            "stop",
        );

        let mut state = State::default();
        reconcile_with_script_log_dirs(
            &config,
            &mut state,
            None,
            std::slice::from_ref(&script_log_dir),
        );

        assert_eq!(state.series.len(), 1);
        assert_eq!(state.series[0].name, "Monster");
        assert_eq!(state.series[0].position_seconds, 120.0);
        assert_eq!(state.series[0].status, Status::InProgress);
    }

    // --- Dropped/completed records react to readings newer than `updated_at` ---

    /// A mid-2026 `updated_at` for records whose entries are aged relative to it.
    const CUTOFF: u64 = 1_790_000_000;

    fn at(unix: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(unix)
    }

    fn record_at(
        root: &Path,
        name: &str,
        status: Status,
        relative: &str,
        position_seconds: f64,
        episode_number: u32,
    ) -> SeriesRecord {
        SeriesRecord {
            root: root.to_string_lossy().into_owned(),
            name: name.to_string(),
            status,
            current_relative_path: relative.to_string(),
            position_seconds,
            episode_number: Some(episode_number),
            updated_at: CUTOFF,
            mal_episode_awareness: None,
            season_number: None,
        }
    }

    /// Writes a script-log entry and stamps its mtime at `mtime_unix`.
    fn script_entry_at(
        log_dir: &Path,
        entry_name: &str,
        source: &Path,
        duration: Option<f64>,
        position: Option<f64>,
        reason: &str,
        mtime_unix: u64,
    ) {
        write_script_entry(log_dir, entry_name, source, duration, position, reason);
        set_mtime(&log_dir.join(entry_name), at(mtime_unix));
    }

    /// A library of `Monster` with `episodes` files, and the path of its
    /// (not yet created) script-log dir.
    fn monster_library(dir: &Path, episodes: usize) -> (PathBuf, PathBuf, Config, PathBuf) {
        let root = dir.join("Anime");
        let series_path = root.join("Monster");
        for n in 1..=episodes {
            touch(&series_path.join(format!("ep{n:02}.mkv")));
        }
        let config = default_config(&root);
        (root, series_path, config, dir.join("mpv-log"))
    }

    fn run_reconcile(config: &Config, state: &mut State, log_dir: &Path) {
        reconcile_with_script_log_dirs(
            config,
            state,
            None,
            std::slice::from_ref(&log_dir.to_path_buf()),
        );
    }

    #[test]
    fn dropped_record_with_a_fresh_quit_reading_inside_the_completion_window_advances() {
        // The reported case: dropped on EP01 at 139.994s, later quit EP01 97s
        // before its end (completion window 180s).
        let dir = tempdir().unwrap();
        let (root, series_path, config, log_dir) = monster_library(dir.path(), 2);
        script_entry_at(
            &log_dir,
            "e1",
            &series_path.join("ep01.mkv"),
            Some(1551.551),
            Some(1454.474),
            "quit",
            CUTOFF + 14 * 86_400,
        );
        let mut state = State {
            series: vec![record_at(
                &root,
                "Monster",
                Status::Dropped,
                "Monster/ep01.mkv",
                139.994,
                1,
            )],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 0.0);
        assert_eq!(record.episode_number, Some(2));
    }

    #[test]
    fn dropped_record_with_a_fresh_ten_minute_bookmark_resumes_there() {
        let dir = tempdir().unwrap();
        let (root, series_path, config, log_dir) = monster_library(dir.path(), 2);
        write_entry(&log_dir, "w1", &series_path.join("ep01.mkv"), 600.0);
        set_mtime(&log_dir.join("w1"), at(CUTOFF + 100));
        let mut state = State {
            series: vec![record_at(
                &root,
                "Monster",
                Status::Dropped,
                "Monster/ep01.mkv",
                139.994,
                1,
            )],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 600.0);
    }

    #[test]
    fn dropped_record_ignores_readings_at_or_before_updated_at_and_takes_the_next_second() {
        // Boundary at the cutoff: strictly later counts, equal or earlier is
        // a leftover from before the drop.
        for (mtime_unix, reactivates) in [(CUTOFF - 1, false), (CUTOFF, false), (CUTOFF + 1, true)]
        {
            let dir = tempdir().unwrap();
            let (root, series_path, config, log_dir) = monster_library(dir.path(), 2);
            script_entry_at(
                &log_dir,
                "e1",
                &series_path.join("ep01.mkv"),
                Some(1420.0),
                Some(1415.0),
                "eof",
                mtime_unix,
            );
            let original = record_at(
                &root,
                "Monster",
                Status::Dropped,
                "Monster/ep01.mkv",
                139.994,
                1,
            );
            let mut state = State {
                series: vec![original.clone()],
                mal_links: Vec::new(),
            };

            run_reconcile(&config, &mut state, &log_dir);

            let record = &state.series[0];
            if reactivates {
                assert_eq!(record.status, Status::InProgress, "mtime {mtime_unix}");
                assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
            } else {
                assert_eq!(record, &original, "mtime {mtime_unix}");
            }
        }
    }

    #[test]
    fn dropped_record_ignores_a_stale_bookmark_on_a_later_episode() {
        // A bookmark from before the drop on EP03 must not revive the series.
        let dir = tempdir().unwrap();
        let (root, series_path, config, log_dir) = monster_library(dir.path(), 3);
        write_entry(&log_dir, "w3", &series_path.join("ep03.mkv"), 500.0);
        set_mtime(&log_dir.join("w3"), at(CUTOFF - 3600));
        let original = record_at(
            &root,
            "Monster",
            Status::Dropped,
            "Monster/ep01.mkv",
            139.994,
            1,
        );
        let mut state = State {
            series: vec![original.clone()],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        assert_eq!(state.series[0], original);
    }

    #[test]
    fn completed_record_rewatched_from_ep01_ignores_stale_eofs_on_later_episodes() {
        // Finished long ago: EP02..EP05 `eof` entries linger. Finishing EP01
        // now must land on EP02, not skip the stale entries to the end.
        let dir = tempdir().unwrap();
        let (root, series_path, config, log_dir) = monster_library(dir.path(), 5);
        for n in 2..=5 {
            script_entry_at(
                &log_dir,
                &format!("old{n}"),
                &series_path.join(format!("ep{n:02}.mkv")),
                Some(1420.0),
                Some(1415.0),
                "eof",
                CUTOFF - 86_400,
            );
        }
        script_entry_at(
            &log_dir,
            "new1",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
            CUTOFF + 86_400,
        );
        let mut state = State {
            series: vec![record_at(
                &root,
                "Monster",
                Status::Completed,
                "Monster/ep05.mkv",
                1400.0,
                5,
            )],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 0.0);
        assert_eq!(record.episode_number, Some(2));
    }

    #[test]
    fn completed_record_with_only_stale_eofs_is_left_untouched() {
        let dir = tempdir().unwrap();
        let (root, series_path, config, log_dir) = monster_library(dir.path(), 3);
        for n in 1..=3 {
            script_entry_at(
                &log_dir,
                &format!("old{n}"),
                &series_path.join(format!("ep{n:02}.mkv")),
                Some(1420.0),
                Some(1415.0),
                "eof",
                CUTOFF - 86_400,
            );
        }
        let original = record_at(
            &root,
            "Monster",
            Status::Completed,
            "Monster/ep03.mkv",
            1400.0,
            3,
        );
        let mut state = State {
            series: vec![original.clone()],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        assert_eq!(state.series[0], original);
    }

    #[test]
    fn dropped_single_episode_series_with_a_fresh_eof_becomes_completed() {
        let dir = tempdir().unwrap();
        let (root, series_path, config, log_dir) = monster_library(dir.path(), 1);
        script_entry_at(
            &log_dir,
            "e1",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
            CUTOFF + 1,
        );
        let mut state = State {
            series: vec![record_at(
                &root,
                "Monster",
                Status::Dropped,
                "Monster/ep01.mkv",
                139.994,
                1,
            )],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::Completed);
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.episode_number, Some(1));
    }

    #[test]
    fn eof_reading_outcome_by_status_and_freshness() {
        // Exhaustive over `Status`: a fresh `eof` on the tracked EP01 always
        // advances to EP02. A stale one only matters to records that have no
        // freshness cutoff.
        let statuses = [
            Status::InProgress,
            Status::Paused,
            Status::Dropped,
            Status::Completed,
        ];
        for status in statuses {
            let stale_is_inert = match status {
                Status::Dropped | Status::Completed => true,
                Status::InProgress | Status::Paused => false,
            };
            for fresh in [true, false] {
                let dir = tempdir().unwrap();
                let (root, series_path, config, log_dir) = monster_library(dir.path(), 3);
                script_entry_at(
                    &log_dir,
                    "e1",
                    &series_path.join("ep01.mkv"),
                    Some(1420.0),
                    Some(1415.0),
                    "eof",
                    if fresh { CUTOFF + 60 } else { CUTOFF - 60 },
                );
                let original = record_at(&root, "Monster", status, "Monster/ep01.mkv", 300.0, 1);
                let mut state = State {
                    series: vec![original.clone()],
                    mal_links: Vec::new(),
                };

                run_reconcile(&config, &mut state, &log_dir);

                let record = &state.series[0];
                if !fresh && stale_is_inert {
                    assert_eq!(record, &original, "{status:?} stale");
                } else {
                    assert_eq!(
                        record.status,
                        Status::InProgress,
                        "{status:?} fresh={fresh}"
                    );
                    assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
                    assert_eq!(record.position_seconds, 0.0);
                }
            }
        }
    }

    #[test]
    fn dropped_record_under_a_nested_root_reacts_only_to_its_own_fresh_reading() {
        // `Anime/Rewatch` is a root inside the root `Anime`; same series name
        // lives under both, but only the inner copy was dropped.
        let dir = tempdir().unwrap();
        let outer = dir.path().join("Anime");
        let inner = outer.join("Rewatch");
        for series in [outer.join("Monster"), inner.join("Monster")] {
            touch(&series.join("ep01.mkv"));
            touch(&series.join("ep02.mkv"));
        }
        let mut config = default_config(&outer);
        config
            .roots
            .paths
            .push(inner.to_string_lossy().into_owned());
        let log_dir = dir.path().join("mpv-log");
        fs::create_dir_all(&log_dir).unwrap();
        let log_dir = dir.path().join("mpv-log");
        script_entry_at(
            &log_dir,
            "e1",
            &inner.join("Monster/ep01.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
            CUTOFF + 60,
        );
        let outer_record = record_at(
            &outer,
            "Monster",
            Status::Dropped,
            "Monster/ep01.mkv",
            139.994,
            1,
        );
        let mut state = State {
            series: vec![
                outer_record.clone(),
                record_at(
                    &inner,
                    "Monster",
                    Status::Dropped,
                    "Monster/ep01.mkv",
                    139.994,
                    1,
                ),
            ],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        assert_eq!(state.series[0], outer_record);
        assert_eq!(state.series[1].status, Status::InProgress);
        assert_eq!(state.series[1].current_relative_path, "Monster/ep02.mkv");
        assert_eq!(state.series[1].position_seconds, 0.0);
    }

    #[cfg(unix)]
    #[test]
    fn dropped_record_under_a_symlinked_root_reacts_to_a_fresh_reading_on_the_real_path() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("real");
        touch(&real.join("Monster/ep01.mkv"));
        touch(&real.join("Monster/ep02.mkv"));
        let link = dir.path().join("Anime");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let config = default_config(&link);
        let log_dir = dir.path().join("mpv-log");
        fs::create_dir_all(&log_dir).unwrap();
        let log_dir = dir.path().join("mpv-log");
        script_entry_at(
            &log_dir,
            "e1",
            &real.join("Monster/ep01.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
            CUTOFF + 60,
        );
        let mut state = State {
            series: vec![record_at(
                &link,
                "Monster",
                Status::Dropped,
                "Monster/ep01.mkv",
                139.994,
                1,
            )],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        assert_eq!(state.series[0].status, Status::InProgress);
        assert_eq!(state.series[0].current_relative_path, "Monster/ep02.mkv");
    }

    #[test]
    fn dropped_record_with_a_future_dated_reading_counts_it_as_fresh() {
        // Pins current behavior: freshness is only "newer than `updated_at`",
        // so a clock-skewed entry from the future reactivates the record.
        let dir = tempdir().unwrap();
        let (root, series_path, config, log_dir) = monster_library(dir.path(), 2);
        script_entry_at(
            &log_dir,
            "e1",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
            CUTOFF + 10 * 365 * 86_400,
        );
        let mut state = State {
            series: vec![record_at(
                &root,
                "Monster",
                Status::Dropped,
                "Monster/ep01.mkv",
                139.994,
                1,
            )],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        assert_eq!(state.series[0].status, Status::InProgress);
        assert_eq!(state.series[0].current_relative_path, "Monster/ep02.mkv");
    }

    #[test]
    fn dropped_record_takes_the_fresher_entry_for_a_file_and_ignores_a_stale_one() {
        // (first entry mtime, second entry mtime, expected position on EP01).
        // Two entries for EP01: the first quit at 300s, the second at 600s of a long file.
        let cases: [(u64, u64, Option<f64>); 4] = [
            (CUTOFF + 10, CUTOFF + 20, Some(600.0)), // both fresh, second newer
            (CUTOFF + 20, CUTOFF + 10, Some(300.0)), // both fresh, first newer
            (CUTOFF - 20, CUTOFF + 10, Some(600.0)), // only the second is fresh
            (CUTOFF - 10, CUTOFF - 20, None),        // both stale
        ];
        for (first_mtime, second_mtime, expected) in cases {
            let dir = tempdir().unwrap();
            let (root, series_path, config, log_dir) = monster_library(dir.path(), 2);
            write_entry(&log_dir, "w1", &series_path.join("ep01.mkv"), 300.0);
            set_mtime(&log_dir.join("w1"), at(first_mtime));
            script_entry_at(
                &log_dir,
                "e1",
                &series_path.join("ep01.mkv"),
                Some(5000.0),
                Some(600.0),
                "quit",
                second_mtime,
            );
            let original = record_at(
                &root,
                "Monster",
                Status::Dropped,
                "Monster/ep01.mkv",
                139.994,
                1,
            );
            let mut state = State {
                series: vec![original.clone()],
                mal_links: Vec::new(),
            };

            run_reconcile(&config, &mut state, &log_dir);

            match expected {
                Some(position) => {
                    assert_eq!(
                        state.series[0].status,
                        Status::InProgress,
                        "{first_mtime} {second_mtime}"
                    );
                    assert_eq!(
                        state.series[0].position_seconds, position,
                        "{first_mtime} {second_mtime}"
                    );
                }
                None => assert_eq!(state.series[0], original, "{first_mtime} {second_mtime}"),
            }
        }
    }

    #[test]
    fn fresh_script_reading_on_a_dropped_record_by_reason_and_duration() {
        // Position 1454s. With a known 1551s duration the 97s remaining is
        // inside the completion window for every reason; without one only
        // `eof` is a finish.
        let reasons = ["eof", "quit", "stop", "error", "unknown"];
        for reason in reasons {
            for duration in [Some(1551.0), None] {
                let dir = tempdir().unwrap();
                let (root, series_path, config, log_dir) = monster_library(dir.path(), 2);
                script_entry_at(
                    &log_dir,
                    "e1",
                    &series_path.join("ep01.mkv"),
                    duration,
                    Some(1454.0),
                    reason,
                    CUTOFF + 60,
                );
                let mut state = State {
                    series: vec![record_at(
                        &root,
                        "Monster",
                        Status::Dropped,
                        "Monster/ep01.mkv",
                        139.994,
                        1,
                    )],
                    mal_links: Vec::new(),
                };

                run_reconcile(&config, &mut state, &log_dir);

                let record = &state.series[0];
                let finished = reason == "eof" || duration.is_some();
                let label = format!("{reason} {duration:?}");
                assert_eq!(record.status, Status::InProgress, "{label}");
                if finished {
                    assert_eq!(record.current_relative_path, "Monster/ep02.mkv", "{label}");
                    assert_eq!(record.position_seconds, 0.0, "{label}");
                } else {
                    assert_eq!(record.current_relative_path, "Monster/ep01.mkv", "{label}");
                    assert_eq!(record.position_seconds, 1454.0, "{label}");
                }
            }
        }
    }

    #[test]
    fn paused_record_with_a_reading_older_than_it_and_equal_to_its_position_is_unchanged() {
        // Paused has no freshness cutoff; the stored position protects it.
        let dir = tempdir().unwrap();
        let (root, series_path, config, log_dir) = monster_library(dir.path(), 2);
        write_entry(&log_dir, "w1", &series_path.join("ep01.mkv"), 300.0);
        set_mtime(&log_dir.join("w1"), at(CUTOFF - 60));
        let original = record_at(
            &root,
            "Monster",
            Status::Paused,
            "Monster/ep01.mkv",
            300.0,
            1,
        );
        let mut state = State {
            series: vec![original.clone()],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        assert_eq!(state.series, vec![original]);
    }

    #[test]
    fn reactivated_record_is_not_idle_paused_in_the_same_reconcile() {
        // Ancient dropped record, fresh reading, `auto_pause_days = 1`:
        // reactivation stamps `updated_at = now`, so the idle pass skips it.
        let dir = tempdir().unwrap();
        let (root, series_path, mut config, log_dir) = monster_library(dir.path(), 2);
        config.thresholds.auto_pause_days = 1;
        write_entry(&log_dir, "w1", &series_path.join("ep01.mkv"), 300.0);
        let mut ancient = record_at(
            &root,
            "Monster",
            Status::Dropped,
            "Monster/ep01.mkv",
            139.994,
            1,
        );
        ancient.updated_at = 1;
        let mut state = State {
            series: vec![ancient],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        assert_eq!(state.series[0].status, Status::InProgress);
        assert_eq!(state.series[0].position_seconds, 300.0);
    }

    #[test]
    fn dropped_record_walks_across_season_directories_on_a_fresh_eof() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        for file in [
            "Season 1/ep01.mkv",
            "Season 1/ep02.mkv",
            "Season 2/ep01.mkv",
        ] {
            touch(&series_path.join(file));
        }
        let config = default_config(&root);
        let log_dir = dir.path().join("mpv-log");
        fs::create_dir_all(&log_dir).unwrap();
        let log_dir = dir.path().join("mpv-log");
        script_entry_at(
            &log_dir,
            "e2",
            &series_path.join("Season 1/ep02.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
            CUTOFF + 60,
        );
        let mut state = State {
            series: vec![record_at(
                &root,
                "Monster",
                Status::Completed,
                "Monster/Season 2/ep01.mkv",
                1400.0,
                1,
            )],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        let record = &state.series[0];
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/Season 2/ep01.mkv");
        assert_eq!(record.position_seconds, 0.0);
    }

    #[test]
    fn dropped_record_ignores_a_fresh_reading_on_an_excluded_episode() {
        let dir = tempdir().unwrap();
        let (root, series_path, mut config, log_dir) = monster_library(dir.path(), 2);
        config.exclusions.push(crate::config::Exclusion {
            root: root.to_string_lossy().into_owned(),
            path: "Monster/ep02.mkv".to_string(),
        });
        script_entry_at(
            &log_dir,
            "e2",
            &series_path.join("ep02.mkv"),
            Some(1420.0),
            Some(1415.0),
            "eof",
            CUTOFF + 60,
        );
        let original = record_at(
            &root,
            "Monster",
            Status::Dropped,
            "Monster/ep01.mkv",
            139.994,
            1,
        );
        let mut state = State {
            series: vec![original.clone()],
            mal_links: Vec::new(),
        };

        run_reconcile(&config, &mut state, &log_dir);

        assert_eq!(state.series, vec![original]);
    }

    /// `(root, name)` as `classify_root_and_series` returns it.
    fn pair(root: &Path, name: &str) -> Option<(String, String)> {
        Some((root.to_string_lossy().into_owned(), name.to_string()))
    }

    fn roots_of(paths: &[&Path]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn classify_root_and_series_picks_the_most_specific_of_nested_roots() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("Anime");
        let rewatches = anime.join("Rewatches");
        touch(&anime.join("Monster").join("ep01.mkv"));
        touch(&rewatches.join("Monster").join("ep01.mkv"));
        let resolved = script_log::resolve_roots(&roots_of(&[&anime, &rewatches]), None);

        assert_eq!(
            classify_root_and_series(
                &resolved,
                &normalize(&rewatches.join("Monster/ep01.mkv"), None)
            ),
            pair(&rewatches, "Monster")
        );
        assert_eq!(
            classify_root_and_series(&resolved, &normalize(&anime.join("Monster/ep01.mkv"), None)),
            pair(&anime, "Monster")
        );
    }

    #[test]
    fn classify_root_and_series_keeps_same_named_sibling_roots_apart() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("A").join("Anime");
        let b = dir.path().join("B").join("Anime");
        touch(&a.join("Show").join("ep01.mkv"));
        touch(&b.join("Show").join("ep01.mkv"));
        let resolved = script_log::resolve_roots(&roots_of(&[&a, &b]), None);

        assert_eq!(
            classify_root_and_series(&resolved, &normalize(&b.join("Show/ep01.mkv"), None)),
            pair(&b, "Show")
        );
        assert_eq!(
            classify_root_and_series(&resolved, &normalize(&a.join("Show/ep01.mkv"), None)),
            pair(&a, "Show")
        );
    }

    #[cfg(unix)]
    #[test]
    fn classify_root_and_series_matches_a_symlinked_series_folder_under_a_nested_root() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let anime = dir.path().join("Anime");
        let rewatches = anime.join("Rewatches");
        fs::create_dir_all(&rewatches).unwrap();
        let storage = dir.path().join("storage").join("Monster");
        touch(&storage.join("ep01.mkv"));
        symlink(&storage, rewatches.join("Monster")).unwrap();
        let resolved = script_log::resolve_roots(&roots_of(&[&anime, &rewatches]), None);

        assert_eq!(
            classify_root_and_series(
                &resolved,
                &normalize(&rewatches.join("Monster/ep01.mkv"), None)
            ),
            pair(&rewatches, "Monster")
        );
    }

    #[test]
    fn classify_root_and_series_is_none_outside_every_root_and_matches_a_missing_root_by_prefix() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("Anime");
        let missing = dir.path().join("Unmounted");
        touch(&anime.join("Show").join("ep01.mkv"));
        touch(&dir.path().join("Downloads").join("clip.mkv"));
        let resolved = script_log::resolve_roots(&roots_of(&[&anime, &missing]), None);

        assert_eq!(
            classify_root_and_series(
                &resolved,
                &normalize(&dir.path().join("Downloads/clip.mkv"), None)
            ),
            None
        );
        assert_eq!(
            classify_root_and_series(&resolved, &normalize(&missing.join("Show/ep01.mkv"), None)),
            pair(&missing, "Show")
        );
    }

    // --- ani-cli ---

    fn ani_config() -> Config {
        let mut config = Config::default();
        config.ani_cli.enabled = true;
        config
    }

    fn history_of(titles: &[&str]) -> ani_cli::History {
        ani_cli::History::parse(
            &titles
                .iter()
                .enumerate()
                .map(|(i, t)| format!("1\tid-{i}\t{t}\n"))
                .collect::<String>(),
        )
    }

    /// Writes a stream entry for `title` ("<show> Episode <n>"), named by the
    /// title like the script does, with its file mtime set to `mtime`.
    fn write_stream(
        log_dir: &Path,
        title: &str,
        duration: Option<f64>,
        position: Option<f64>,
        reason: &str,
        mtime: SystemTime,
    ) {
        fs::create_dir_all(log_dir).unwrap();
        let path = log_dir.join(title.replace(' ', "_"));
        fs::write(
            &path,
            format!(
                "# https://cdn.example/{}/index.m3u8?token=abc\ntitle={title}\nduration={}\nposition={}\nreason={reason}\n",
                title.replace(' ', "-"),
                duration.map(|d| d.to_string()).unwrap_or_default(),
                position.map(|p| p.to_string()).unwrap_or_default(),
            ),
        )
        .unwrap();
        set_mtime(&path, mtime);
    }

    fn quit_at(log_dir: &Path, title: &str, position: f64, mtime: SystemTime) {
        write_stream(log_dir, title, Some(1400.0), Some(position), "quit", mtime);
    }

    fn run_ani(config: &Config, state: &mut State, log_dir: &Path, history: &ani_cli::History) {
        reconcile_with_inputs(
            config,
            state,
            None,
            std::slice::from_ref(&log_dir.to_path_buf()),
            history,
        );
    }

    /// The record with its volatile `updated_at` blanked, for whole-value comparison.
    fn ani_record(
        status: Status,
        title: &str,
        position_seconds: f64,
        episode_number: u32,
        updated_at: u64,
    ) -> SeriesRecord {
        SeriesRecord {
            root: ani_cli::ROOT.to_string(),
            name: title.to_string(),
            status,
            current_relative_path: String::new(),
            position_seconds,
            episode_number: Some(episode_number),
            updated_at,
            mal_episode_awareness: None,
            season_number: None,
        }
    }

    /// `state.series` with every `updated_at` replaced by `stamp`.
    fn stamped(state: &State, stamp: u64) -> Vec<SeriesRecord> {
        state
            .series
            .iter()
            .map(|r| SeriesRecord {
                updated_at: stamp,
                ..r.clone()
            })
            .collect()
    }

    #[test]
    fn ani_cli_resume_reading_creates_a_record_at_that_episode_and_position() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        quit_at(&log, "Blue Lock Episode 9", 300.0, at(CUTOFF));
        let mut state = State::default();

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 300.0, 9, 0)]
        );
    }

    #[test]
    fn ani_cli_reading_below_resume_seconds_creates_nothing_at_the_boundary() {
        for (position, creates) in [
            (0.0, false),
            (119.0, false),
            (119.9, false),
            (120.0, true),
            (121.0, true),
        ] {
            let dir = tempdir().unwrap();
            let log = dir.path().join("mpv-log");
            quit_at(&log, "Blue Lock Episode 9", position, at(CUTOFF));
            let mut state = State::default();

            run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

            assert_eq!(
                state.series.len(),
                usize::from(creates),
                "position {position}"
            );
        }
    }

    #[test]
    fn ani_cli_eof_moves_the_cursor_to_the_next_episode_at_zero() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        write_stream(
            &log,
            "Blue Lock Episode 9",
            Some(1400.0),
            Some(1399.0),
            "eof",
            at(CUTOFF),
        );
        let mut state = State::default();

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 0.0, 10, 0)]
        );
    }

    #[test]
    fn ani_cli_stop_within_the_completion_window_counts_as_finished() {
        // Duration 1400: remaining 180 is completed, remaining 181 is a resume.
        for (position, expected_episode, expected_position) in
            [(1220.0, 10, 0.0), (1219.0, 9, 1219.0)]
        {
            let dir = tempdir().unwrap();
            let log = dir.path().join("mpv-log");
            quit_at(&log, "Blue Lock Episode 9", position, at(CUTOFF));
            let mut state = State::default();

            run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

            assert_eq!(
                stamped(&state, 0),
                vec![ani_record(
                    Status::InProgress,
                    "Blue Lock",
                    expected_position,
                    expected_episode,
                    0
                )],
                "position {position}"
            );
        }
    }

    #[test]
    fn ani_cli_short_stream_below_resume_seconds_is_not_completed_by_the_window() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        // 60s total, stopped at 45s: remaining 15s is inside the window, but
        // the position check wins.
        write_stream(
            &log,
            "Blue Lock Episode 9",
            Some(60.0),
            Some(45.0),
            "quit",
            at(CUTOFF),
        );
        let mut state = State::default();

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(state.series, Vec::new());
    }

    #[test]
    fn ani_cli_reading_for_a_title_missing_from_history_creates_nothing() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        quit_at(&log, "Some Video Episode 3", 500.0, at(CUTOFF));
        let mut state = State::default();

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(state.series, Vec::new());
    }

    #[test]
    fn ani_cli_reading_creates_nothing_when_the_source_is_not_enabled() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        quit_at(&log, "Blue Lock Episode 9", 300.0, at(CUTOFF));
        let mut state = State::default();

        run_ani(
            &Config::default(),
            &mut state,
            &log,
            &history_of(&["Blue Lock"]),
        );

        assert_eq!(state.series, Vec::new());
    }

    #[test]
    fn ani_cli_reading_for_an_ambiguous_or_fractional_episode_creates_nothing() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        quit_at(&log, "Twin Episode 2", 500.0, at(CUTOFF));
        quit_at(&log, "Blue Lock Episode 12.5", 500.0, at(CUTOFF));
        let mut state = State::default();

        run_ani(
            &ani_config(),
            &mut state,
            &log,
            &history_of(&["Twin", "Twin", "Blue Lock"]),
        );

        assert_eq!(state.series, Vec::new());
    }

    #[test]
    fn ani_cli_unchanged_reading_does_not_bump_updated_at() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        quit_at(&log, "Blue Lock Episode 9", 300.0, at(CUTOFF));
        let history = history_of(&["Blue Lock"]);
        let mut state = State::default();
        run_ani(&ani_config(), &mut state, &log, &history);
        state.series[0].updated_at = 5;

        run_ani(&ani_config(), &mut state, &log, &history);

        assert_eq!(
            state.series,
            vec![ani_record(Status::InProgress, "Blue Lock", 300.0, 9, 5)]
        );
    }

    #[test]
    fn ani_cli_furthest_episode_with_a_usable_reading_wins() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        quit_at(&log, "Blue Lock Episode 3", 300.0, at(CUTOFF));
        quit_at(&log, "Blue Lock Episode 5", 700.0, at(CUTOFF));
        quit_at(&log, "Blue Lock Episode 4", 200.0, at(CUTOFF));
        let mut state = State::default();

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 700.0, 5, 0)]
        );
    }

    #[test]
    fn ani_cli_consecutive_finished_episodes_skip_to_the_one_after_the_last() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        for ep in 1..=3 {
            write_stream(
                &log,
                &format!("Blue Lock Episode {ep}"),
                Some(1400.0),
                Some(1399.0),
                "eof",
                at(CUTOFF),
            );
        }
        let mut state = State::default();

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 0.0, 4, 0)]
        );
    }

    #[test]
    fn ani_cli_a_gap_before_a_finished_episode_does_not_block_the_advance() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        write_stream(
            &log,
            "Blue Lock Episode 7",
            Some(1400.0),
            Some(1399.0),
            "eof",
            at(CUTOFF),
        );
        let mut state = State::default();

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 0.0, 8, 0)]
        );
    }

    #[test]
    fn ani_cli_active_record_ignores_readings_for_episodes_before_its_cursor() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        let history = history_of(&["Blue Lock"]);
        quit_at(&log, "Blue Lock Episode 5", 300.0, at(CUTOFF));
        let mut state = State::default();
        run_ani(&ani_config(), &mut state, &log, &history);
        state.series[0].updated_at = 5;
        quit_at(&log, "Blue Lock Episode 3", 900.0, at(CUTOFF + 1000));

        run_ani(&ani_config(), &mut state, &log, &history);

        assert_eq!(
            state.series,
            vec![ani_record(Status::InProgress, "Blue Lock", 300.0, 5, 5)]
        );
    }

    #[test]
    fn ani_cli_each_status_reacts_to_a_reading_older_than_the_record_as_for_folders() {
        for status in [
            Status::InProgress,
            Status::Paused,
            Status::Completed,
            Status::Dropped,
        ] {
            let dir = tempdir().unwrap();
            let log = dir.path().join("mpv-log");
            let history = history_of(&["Blue Lock"]);
            quit_at(&log, "Blue Lock Episode 5", 300.0, at(CUTOFF - 100));
            let mut state = State::default();
            run_ani(&ani_config(), &mut state, &log, &history);
            state.series[0].status = status;
            state.series[0].updated_at = CUTOFF;
            // A different position for the same episode, still older than the record.
            quit_at(&log, "Blue Lock Episode 5", 900.0, at(CUTOFF - 50));

            run_ani(&ani_config(), &mut state, &log, &history);

            let expected = match status {
                // Active records take the reading and become InProgress.
                Status::InProgress | Status::Paused => {
                    ani_record(Status::InProgress, "Blue Lock", 900.0, 5, 0)
                }
                // Dropped and completed records ignore leftovers.
                Status::Completed | Status::Dropped => ani_record(status, "Blue Lock", 300.0, 5, 0),
            };
            assert_eq!(stamped(&state, 0), vec![expected], "{status:?}");
        }
    }

    #[test]
    fn ani_cli_dropped_or_completed_record_reactivates_only_on_a_strictly_newer_reading() {
        for status in [Status::Completed, Status::Dropped] {
            for (mtime, reactivates) in [(CUTOFF - 1, false), (CUTOFF, false), (CUTOFF + 1, true)] {
                let dir = tempdir().unwrap();
                let log = dir.path().join("mpv-log");
                let history = history_of(&["Blue Lock"]);
                quit_at(&log, "Blue Lock Episode 5", 300.0, at(CUTOFF - 100));
                let mut state = State::default();
                run_ani(&ani_config(), &mut state, &log, &history);
                state.series[0].status = status;
                state.series[0].updated_at = CUTOFF;
                quit_at(&log, "Blue Lock Episode 2", 400.0, at(mtime));

                run_ani(&ani_config(), &mut state, &log, &history);

                let expected = if reactivates {
                    ani_record(Status::InProgress, "Blue Lock", 400.0, 2, 0)
                } else {
                    ani_record(status, "Blue Lock", 300.0, 5, 0)
                };
                assert_eq!(stamped(&state, 0), vec![expected], "{status:?} at {mtime}");
            }
        }
    }

    #[test]
    fn ani_cli_paused_record_advances_when_its_episode_is_finished() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        let history = history_of(&["Blue Lock"]);
        quit_at(&log, "Blue Lock Episode 5", 300.0, at(CUTOFF));
        let mut state = State::default();
        run_ani(&ani_config(), &mut state, &log, &history);
        state.series[0].status = Status::Paused;
        write_stream(
            &log,
            "Blue Lock Episode 5",
            Some(1400.0),
            Some(1399.0),
            "eof",
            at(CUTOFF + 10),
        );

        run_ani(&ani_config(), &mut state, &log, &history);

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 0.0, 6, 0)]
        );
    }

    #[test]
    fn ani_cli_absence_of_readings_leaves_a_record_untouched_even_when_history_lost_it() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        quit_at(&log, "Blue Lock Episode 5", 300.0, at(CUTOFF));
        let mut state = State::default();
        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));
        state.series[0].updated_at = 5;
        fs::remove_dir_all(&log).unwrap();

        run_ani(
            &ani_config(),
            &mut state,
            &log,
            &ani_cli::History::default(),
        );

        assert_eq!(
            state.series,
            vec![ani_record(Status::InProgress, "Blue Lock", 300.0, 5, 5)]
        );
    }

    #[test]
    fn ani_cli_and_folder_series_with_the_same_name_keep_separate_records() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let folder_ep = root.join("Blue Lock").join("ep01.mkv");
        touch(&folder_ep);
        touch(&root.join("Blue Lock").join("ep02.mkv"));
        let log = dir.path().join("mpv-log");
        write_entry(&log, "folder", &folder_ep, 500.0);
        quit_at(&log, "Blue Lock Episode 9", 300.0, at(CUTOFF));
        let mut config = default_config(&root);
        config.ani_cli.enabled = true;
        let mut state = State::default();

        run_ani(&config, &mut state, &log, &history_of(&["Blue Lock"]));

        let mut records: Vec<(String, String, Option<u32>, f64)> = state
            .series
            .iter()
            .map(|r| {
                (
                    r.root.clone(),
                    r.name.clone(),
                    r.episode_number,
                    r.position_seconds,
                )
            })
            .collect();
        records.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            records,
            vec![
                (
                    root.to_string_lossy().into_owned(),
                    "Blue Lock".to_string(),
                    Some(1),
                    500.0
                ),
                (
                    ani_cli::ROOT.to_string(),
                    "Blue Lock".to_string(),
                    Some(9),
                    300.0
                ),
            ]
        );
    }

    #[test]
    fn ani_cli_records_are_subject_to_idle_auto_pause() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        let history = history_of(&["Blue Lock"]);
        quit_at(&log, "Blue Lock Episode 5", 300.0, at(CUTOFF));
        let mut config = ani_config();
        config.thresholds.auto_pause_days = 1;
        let mut state = State::default();
        run_ani(&config, &mut state, &log, &history);
        state.series[0].updated_at = 1;

        run_ani(&config, &mut state, &log, &history);

        assert_eq!(state.series[0].status, Status::Paused);
    }

    #[test]
    fn ani_cli_active_record_updates_when_only_the_episode_or_only_the_position_changes() {
        // (new episode, new position) against a record at episode 5, position 300.
        for (episode, position) in [(6, 300.0), (5, 900.0)] {
            let dir = tempdir().unwrap();
            let log = dir.path().join("mpv-log");
            let history = history_of(&["Blue Lock"]);
            quit_at(&log, "Blue Lock Episode 5", 300.0, at(CUTOFF));
            let mut state = State::default();
            run_ani(&ani_config(), &mut state, &log, &history);
            state.series[0].updated_at = 5;
            quit_at(
                &log,
                &format!("Blue Lock Episode {episode}"),
                position,
                at(CUTOFF + 10),
            );

            run_ani(&ani_config(), &mut state, &log, &history);

            assert_eq!(
                stamped(&state, 0),
                vec![ani_record(
                    Status::InProgress,
                    "Blue Lock",
                    position,
                    episode,
                    0
                )],
                "episode {episode} position {position}"
            );
        }
    }

    #[test]
    fn ani_cli_dropped_record_reactivates_on_a_newer_reading_identical_to_the_stored_one() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        let history = history_of(&["Blue Lock"]);
        quit_at(&log, "Blue Lock Episode 5", 300.0, at(CUTOFF - 100));
        let mut state = State::default();
        run_ani(&ani_config(), &mut state, &log, &history);
        state.series[0].status = Status::Dropped;
        state.series[0].updated_at = CUTOFF;
        quit_at(&log, "Blue Lock Episode 5", 300.0, at(CUTOFF + 1));

        run_ani(&ani_config(), &mut state, &log, &history);

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 300.0, 5, 0)]
        );
    }

    // --- ani-cli: completion from MAL's total ---

    fn with_total(state: &mut State, title: &str, total: Option<u32>) {
        state.mal_links.push(crate::state::MalLink {
            root: ani_cli::ROOT.to_string(),
            name: title.to_string(),
            mal_id: 7,
            total_episodes: total,
        });
    }

    fn finished(log: &Path, episode: u32, mtime: SystemTime) {
        write_stream(
            log,
            &format!("Blue Lock Episode {episode}"),
            Some(1400.0),
            Some(1399.0),
            "eof",
            mtime,
        );
    }

    #[test]
    fn ani_cli_finishing_the_final_episode_completes_only_at_exactly_the_total() {
        // Total 12: below advances, equal completes, above never completes.
        for (episode, status, expected_episode, expected_position) in [
            (11, Status::InProgress, 12, 0.0),
            (12, Status::Completed, 12, 0.0),
            (13, Status::InProgress, 14, 0.0),
        ] {
            let dir = tempdir().unwrap();
            let log = dir.path().join("mpv-log");
            finished(&log, episode, at(CUTOFF));
            let mut state = State::default();
            with_total(&mut state, "Blue Lock", Some(12));

            run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

            assert_eq!(
                stamped(&state, 0),
                vec![ani_record(
                    status,
                    "Blue Lock",
                    expected_position,
                    expected_episode,
                    0
                )],
                "finished episode {episode}"
            );
        }
    }

    #[test]
    fn ani_cli_finishing_the_last_episode_never_completes_when_the_total_is_unknown() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        finished(&log, 12, at(CUTOFF));
        let mut state = State::default();
        with_total(&mut state, "Blue Lock", None);

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 0.0, 13, 0)]
        );
    }

    #[test]
    fn ani_cli_a_total_learned_after_the_cursor_passed_the_last_episode_completes_it() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        let history = history_of(&["Blue Lock"]);
        finished(&log, 12, at(CUTOFF));
        let mut state = State::default();
        run_ani(&ani_config(), &mut state, &log, &history);
        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 0.0, 13, 0)]
        );

        with_total(&mut state, "Blue Lock", Some(12));
        run_ani(&ani_config(), &mut state, &log, &history);

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::Completed, "Blue Lock", 0.0, 12, 0)]
        );
    }

    #[test]
    fn ani_cli_a_final_episode_resume_reading_stays_in_progress_at_that_episode() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        quit_at(&log, "Blue Lock Episode 12", 300.0, at(CUTOFF));
        let mut state = State::default();
        with_total(&mut state, "Blue Lock", Some(12));

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 300.0, 12, 0)]
        );
    }

    #[test]
    fn ani_cli_finishing_the_final_episode_completes_a_paused_record_and_keeps_its_position() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        let history = history_of(&["Blue Lock"]);
        quit_at(&log, "Blue Lock Episode 12", 300.0, at(CUTOFF));
        let mut state = State::default();
        with_total(&mut state, "Blue Lock", Some(12));
        run_ani(&ani_config(), &mut state, &log, &history);
        state.series[0].status = Status::Paused;
        state.series[0].updated_at = 5;
        finished(&log, 12, at(CUTOFF + 10));

        run_ani(&ani_config(), &mut state, &log, &history);

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::Completed, "Blue Lock", 300.0, 12, 0)]
        );
    }

    #[test]
    fn ani_cli_a_completed_record_ignores_the_old_final_reading_and_is_not_bumped_again() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        let history = history_of(&["Blue Lock"]);
        finished(&log, 12, at(CUTOFF));
        let mut state = State::default();
        with_total(&mut state, "Blue Lock", Some(12));
        run_ani(&ani_config(), &mut state, &log, &history);
        state.series[0].updated_at = CUTOFF + 100;

        run_ani(&ani_config(), &mut state, &log, &history);

        assert_eq!(
            state.series,
            vec![ani_record(
                Status::Completed,
                "Blue Lock",
                0.0,
                12,
                CUTOFF + 100
            )]
        );
    }

    #[test]
    fn ani_cli_a_completed_show_reactivates_on_newer_playback_as_for_folders() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        let history = history_of(&["Blue Lock"]);
        finished(&log, 12, at(CUTOFF));
        let mut state = State::default();
        with_total(&mut state, "Blue Lock", Some(12));
        run_ani(&ani_config(), &mut state, &log, &history);
        state.series[0].updated_at = CUTOFF + 100;
        quit_at(&log, "Blue Lock Episode 3", 500.0, at(CUTOFF + 200));

        run_ani(&ani_config(), &mut state, &log, &history);

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 500.0, 3, 0)]
        );
    }

    #[test]
    fn ani_cli_another_titles_total_does_not_complete_this_show() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("mpv-log");
        finished(&log, 12, at(CUTOFF));
        let mut state = State::default();
        with_total(&mut state, "Chained Soldier", Some(12));

        run_ani(&ani_config(), &mut state, &log, &history_of(&["Blue Lock"]));

        assert_eq!(
            stamped(&state, 0),
            vec![ani_record(Status::InProgress, "Blue Lock", 0.0, 13, 0)]
        );
    }
}
