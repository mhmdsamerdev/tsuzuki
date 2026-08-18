//! The single shared "resolve `state.toml` against current mpv reality"
//! step every state-touching command runs first (CLAUDE.md's "Every
//! state-touching command reconciles against current mpv reality first").
//!
//! Implements, together, per CLAUDE.md's "Cursor movement" decision:
//! - the noise rule: a sub-`resume_seconds` reading is invisible to
//!   `state.toml` — it never creates a record and never overwrites an
//!   existing real one.
//! - two real, equally-weighted completion signals for the series' CURRENT
//!   tracked episode: a missing `watch_later` entry (mpv's own
//!   played-to-true-EOF signal), and a present entry `mpv::classify()`
//!   scores `Completed` (a real position within `completion_remaining_seconds`
//!   of the file's own duration). Neither is a landing candidate on its
//!   own file — both instead trigger `apply_completion_advance`, which
//!   resolves the cursor to the *next* episode (position 0), chaining past
//!   any further episode that's ALSO already completed, or transitions the
//!   record to `Dropped` once the walk runs out of episodes (CLAUDE.md's
//!   "When the CURRENT tracked episode itself completes" rule). An earlier
//!   version of this scan discarded a `Completed` reading outright instead
//!   (a real, shipped bug: the record's position permanently stopped
//!   updating the moment it crossed into the completion window); a later,
//!   still-incomplete fix recorded that reading's own raw position in
//!   place instead of actually advancing the cursor — CLAUDE.md's rule
//!   above is what settled the actual intended behavior.
//! - single-run chaining, two distinct forms: (1) ordinary — one call
//!   resolves however many steps happened since the last check when a real
//!   `Resume` position already exists further ahead (the forward scan
//!   always finds the *furthest* real `Resume` entry, unaffected by
//!   completion handling); (2) completion-driven — `apply_completion_advance`
//!   itself walks forward through any number of *consecutively* completed
//!   episodes in one call before landing or auto-`Drop`ping.
//! - a series whose folder no longer resolves on disk (moved/deleted) is
//!   left inert, never crashed on or auto-removed.
//! - opening any episode of a `paused`/`dropped` series directly in mpv is
//!   picked up here too, the same forward-scan mechanism, and flips status
//!   back to `in_progress` — this is the *only* mechanism by which a
//!   `dropped` series ever returns to `in_progress` (CLAUDE.md: no
//!   `dropped -> paused` transition exists).
//!
//! Phase 6 adds a second, independent candidate source: the optional mpv
//! companion script's log (`script_log.rs`, CLAUDE.md's "Optional mpv
//! companion script"). Candidate discovery is now a per-path union of both
//! sources ([`merge_candidates`]) — a script-log entry with no corresponding
//! `watch_later` entry at all (a clean binge, watched straight through with
//! the script installed) still surfaces as real activity, closing the
//! "clean binge leaves no trace" blind spot documented above for anyone who
//! has the script installed. When a path has entries from both sources, the
//! fresher one wins by the underlying entry file's own mtime (an exact tie
//! goes to the script entry, deterministically). Everything downstream of
//! source selection — the forward scan, `any_completed_found`, cursor
//! advancement, and auto-`Dropped`-on-no-next-episode — is unchanged; it
//! only now classifies through [`classify_candidate`], which fans out to
//! `mpv::classify()` unchanged for a `watch_later`-sourced reading, treats a
//! script-sourced `reason == "eof"` reading as an unconditional `Completed`
//! (a direct signal — no proximity-to-duration guessing needed), and treats
//! any other script-sourced reason with a real position exactly like a
//! `watch_later` reading (through the same `mpv::classify()` call), just fed
//! the script's own duration when it has one instead of a Symphonia/AVI
//! probe.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::anitomy;
use crate::config::Config;
use crate::duration::{self, DurationOutcome};
use crate::exclude;
use crate::mpv::{self, PlaybackAction, ScanWarning, WatchLaterEntry};
use crate::roots::{expand_tilde, normalize};
use crate::script_log::{self, ScriptLogEntry};
use crate::series;
use crate::state::{SeriesRecord, State, Status, now_unix};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconcileOutcome {
    pub warning: Option<ScanWarning>,
}

/// One merged, per-path candidate reading, tagged with which source it came
/// from — the winner of [`merge_candidates`]'s per-path source selection.
#[derive(Debug, Clone, PartialEq)]
enum Candidate {
    WatchLater(WatchLaterEntry),
    ScriptLog(ScriptLogEntry),
}

impl Candidate {
    fn source_path(&self) -> &Path {
        match self {
            Candidate::WatchLater(e) => &e.source_path,
            Candidate::ScriptLog(e) => &e.source_path,
        }
    }

    fn mtime(&self) -> SystemTime {
        match self {
            Candidate::WatchLater(e) => e.mtime,
            Candidate::ScriptLog(e) => e.mtime,
        }
    }

    /// Whether this candidate is real enough to mark its (root, series) pair
    /// as needing reconciliation at all (the top-level `pairs` collection in
    /// [`reconcile`], extending the existing sub-`resume_seconds`-is-noise
    /// rule to script-sourced entries). A script `eof` reading is always
    /// real activity regardless of its raw position — it's a direct
    /// completion signal, not a position to threshold-check; any other
    /// script reason still needs a real (>= `resume_seconds`) position, same
    /// as a `watch_later` reading.
    fn is_real_activity(&self, resume_seconds: u64) -> bool {
        match self {
            Candidate::WatchLater(e) => e.position_seconds >= resume_seconds as f64,
            Candidate::ScriptLog(e) if e.reason == "eof" => true,
            Candidate::ScriptLog(e) => e.position.is_some_and(|p| p >= resume_seconds as f64),
        }
    }
}

/// Merges `watch_later` and script-log entries into one per-path candidate
/// map, keyed by each path's normalized (canonicalized-with-fallback) form.
/// When a path has an entry from only one source, that entry wins
/// unconditionally. When both are present, the underlying entry files'
/// mtimes decide: strictly fresher wins, and an exact tie goes to the script
/// entry (CLAUDE.md's decision, deterministic like the OS-candidate merge's
/// own exact-mtime-tie precedent).
fn merge_candidates(
    watch_later: Vec<WatchLaterEntry>,
    script_log: Vec<ScriptLogEntry>,
    home: Option<&Path>,
) -> HashMap<PathBuf, Candidate> {
    let mut map: HashMap<PathBuf, Candidate> = HashMap::new();
    for entry in watch_later {
        let key = normalize(&entry.source_path, home);
        map.insert(key, Candidate::WatchLater(entry));
    }
    for entry in script_log {
        let key = normalize(&entry.source_path, home);
        let script_wins = match map.get(&key) {
            Some(existing) => entry.mtime >= existing.mtime(),
            None => true,
        };
        if script_wins {
            map.insert(key, Candidate::ScriptLog(entry));
        }
    }
    map
}

/// Classifies one merged candidate's reading against `file`'s thresholds,
/// per source (CLAUDE.md's per-source classification rules). Returns `None`
/// when the candidate carries no usable signal for this file — a
/// below-`resume_seconds` reading (either source), or a script entry with a
/// non-`eof` reason and no position at all — which the forward scan then
/// treats exactly like "no entry for this file."
fn classify_candidate(
    candidate: &Candidate,
    file: &Path,
    resume_seconds: u64,
    completion_remaining_seconds: u64,
) -> Option<PlaybackAction> {
    match candidate {
        Candidate::WatchLater(entry) => {
            if entry.position_seconds < resume_seconds as f64 {
                return None;
            }
            let duration_outcome = duration::probe_duration(file);
            let classification = mpv::classify(
                entry.position_seconds,
                duration_outcome,
                resume_seconds,
                completion_remaining_seconds,
            );
            Some(classification.action)
        }
        Candidate::ScriptLog(entry) if entry.reason == "eof" => Some(PlaybackAction::Completed),
        Candidate::ScriptLog(entry) => {
            let position = entry.position?;
            if position < resume_seconds as f64 {
                return None;
            }
            let duration_outcome = match entry.duration {
                Some(d) => DurationOutcome::Known(d),
                None => duration::probe_duration(file),
            };
            let classification = mpv::classify(
                position,
                duration_outcome,
                resume_seconds,
                completion_remaining_seconds,
            );
            Some(classification.action)
        }
    }
}

/// Runs the full reconcile pass over every series that either already has a
/// `state.toml` record or has fresh real activity (from either candidate
/// source) under a tracked root. Mutates `state` in place; the caller
/// decides whether/how to persist it (comparing before/after, same pattern
/// every other module in this codebase uses of leaving I/O to the caller).
pub fn reconcile(config: &Config, state: &mut State, home: Option<&Path>) -> ReconcileOutcome {
    let script_log_dir = script_log::mpv_log_dir().ok();
    reconcile_with_script_log_dir(config, state, home, script_log_dir.as_deref())
}

/// The actual reconcile implementation, parameterized on the script-log
/// directory to scan — split out from [`reconcile`] purely for testability:
/// unlike `watch_later_dir`, the script-log directory is deliberately not a
/// `config.toml` field (CLAUDE.md: "No new config field for the script-log
/// directory"), so tests can't redirect it the way `mpv.rs`'s tests redirect
/// `watch_later_dir`. This function stays private; `reconcile` is the only
/// public entry point, always resolving the real
/// `$XDG_DATA_HOME/tsuzuki/mpv-log` via [`script_log::mpv_log_dir`].
fn reconcile_with_script_log_dir(
    config: &Config,
    state: &mut State,
    home: Option<&Path>,
    script_log_dir: Option<&Path>,
) -> ReconcileOutcome {
    let scan = mpv::scan_watch_later(config, home);
    let script_entries = match script_log_dir {
        Some(dir) => script_log::scan_script_log(dir, &config.roots.paths, home),
        None => Vec::new(),
    };
    let resume_seconds = config.thresholds.resume_seconds;
    let completion_remaining_seconds = config.thresholds.completion_remaining_seconds;

    let candidates = merge_candidates(scan.entries, script_entries, home);

    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for record in &state.series {
        pairs.insert((record.root.clone(), record.name.clone()));
    }
    for candidate in candidates.values() {
        if !candidate.is_real_activity(resume_seconds) {
            continue;
        }
        if let Some(pair) = classify_root_and_series(config, candidate.source_path(), home) {
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

    ReconcileOutcome {
        warning: scan.warning,
    }
}

/// Finds which tracked root `source_path` falls under, and the series name
/// (the root's immediate child directory) it belongs to. When more than one
/// tracked root matches (composable roots, e.g. `~/Anime` and
/// `~/Anime/Rewatches`), the most specific (longest/most-nested) match
/// wins — otherwise a file under `Anime/Rewatches/Monster` would misresolve
/// to series name `"Rewatches"` under the `Anime` root instead of series
/// `"Monster"` under the `Anime/Rewatches` root.
///
/// Each root is checked two ways: a cheap whole-root prefix match first
/// (the common case), falling back to canonicalizing that root's own
/// depth-1 children individually only when the whole-root match fails. The
/// fallback matters for a real, confirmed reason, not a hypothetical: a
/// depth-1 series folder can itself be a symlink pointing entirely outside
/// the root's own real location (CLAUDE.md's "Symlinks are followed
/// everywhere... a symlinked series folder behaves exactly like a real
/// one"). `normalize` canonicalizes the *whole* entry path, which resolves
/// straight through such a symlink to wherever it actually points — a
/// plain root-vs-source prefix comparison can never match that, so the
/// series would never reconcile at all. Confirmed directly: a real tracked
/// root with one symlinked series folder never reconciled, while an
/// ordinary sibling series under the same root worked correctly.
fn classify_root_and_series(
    config: &Config,
    source_path: &Path,
    home: Option<&Path>,
) -> Option<(String, String)> {
    let real_source = normalize(source_path, home);
    let tracked_root_real_paths: Vec<_> = config
        .roots
        .paths
        .iter()
        .map(|r| normalize(Path::new(r), home))
        .collect();

    let mut best: Option<(PathBuf, String, String)> = None;
    for root in &config.roots.paths {
        let real_root = normalize(Path::new(root), home);
        if let Ok(rel) = real_source.strip_prefix(&real_root) {
            if let Some(first) = rel.components().next() {
                let name = first.as_os_str().to_string_lossy().into_owned();
                consider_match(&mut best, real_root, root, &name);
            }
            // Whole-root prefix match succeeded (even if it didn't yield a
            // usable child component) — the fallback below exists only for
            // when this comparison fails entirely, so it's skipped here.
            continue;
        }

        let expanded_root = expand_tilde(Path::new(root), home);
        let Ok(children) = series::series_in_root(&expanded_root, &tracked_root_real_paths) else {
            continue;
        };
        for child in children {
            let real_child = normalize(&child.path, home);
            if real_source == real_child || real_source.starts_with(&real_child) {
                consider_match(&mut best, real_child, root, &child.name);
            }
        }
    }
    best.map(|(_, root, name)| (root, name))
}

/// Updates `best` to `(matched_prefix, root, name)` when `matched_prefix`
/// is more specific (longer) than whatever `best` already holds — the
/// shared "most specific match wins" tie-break used by both the whole-root
/// and per-child-fallback matches in `classify_root_and_series`.
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

/// Reconciles one (root, series name) pair. See module docs for the full
/// algorithm; this is the per-series worker `reconcile` calls for every
/// candidate pair discovered.
#[allow(clippy::too_many_arguments)]
fn reconcile_one_series(
    config: &Config,
    state: &mut State,
    root: &str,
    name: &str,
    candidates: &HashMap<PathBuf, Candidate>,
    resume_seconds: u64,
    completion_remaining_seconds: u64,
    home: Option<&Path>,
) {
    let expanded_root = expand_tilde(Path::new(root), home);
    let series_path = expanded_root.join(name);
    // A moved/deleted series folder is indistinguishable from a brand-new
    // one (CLAUDE.md §3) — either way, if it doesn't resolve as a directory
    // right now, this record (if any) is left completely untouched.
    if !series_path.is_dir() {
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

    let start_index = match existing_index {
        Some(i) => {
            let record = &state.series[i];
            if record.status == Status::Dropped {
                // Drop resets progress; any episode picking it back up
                // (including episode 1) must count as new real evidence —
                // see CLAUDE.md's "Paused/dropped -> in_progress" note.
                0
            } else {
                let target = expanded_root.join(&record.current_relative_path);
                let real_target = normalize(&target, home);
                match ordered
                    .iter()
                    .position(|p| normalize(p, home) == real_target)
                {
                    Some(idx) => idx,
                    // The record's own file isn't in the current ordering
                    // (excluded since, or individually removed while the
                    // rest of the series remains) — degrade gracefully by
                    // skipping this series' reconciliation this run, rather
                    // than guessing where "forward" starts from.
                    None => return,
                }
            }
        }
        None => 0,
    };

    // Forward scan from `start_index`: for each ordered file, a real
    // (>= resume_seconds) watch_later entry is classified via
    // `mpv::classify()` (probing duration only for entries that already
    // have a real position, per CLAUDE.md — never for noise). Only `Resume`
    // is a landing candidate here — `Completed` never lands an episode on
    // its own raw position; per CLAUDE.md's "When the CURRENT tracked
    // episode itself completes" rule, a confirmed-complete episode instead
    // advances the cursor to the *next* episode (or auto-`Drop`s if there
    // is none), handled below once the scan finds nothing real anywhere.
    // `best` is still unconditionally overwritten as the scan moves
    // forward, so a later real `Resume` always wins over an earlier one —
    // this is what makes ordinary multi-step binge chaining (a real
    // position already exists further ahead) resolve in one pass,
    // unaffected by the completion-advance logic below.
    //
    // `any_completed_found` tracks whether a `Completed` classification
    // was observed *anywhere* in this scan (at `start_index` or later) —
    // deliberately not just at `start_index`. Absence of the current
    // episode's own entry is never, by itself, evidence of anything: mpv's
    // own `--resume-playback` (on by default) deletes a file's bookmark
    // the instant it's loaded, regardless of who launched mpv or what
    // happens afterward, so "no entry" is produced identically by a
    // genuine completion and by an ordinary abandon-mid-file-via-skip —
    // these are provably indistinguishable from watch_later state alone.
    // Treating absence as positive proof (an earlier version of this scan
    // did, via the stored record's own old position as a "baseline") was a
    // real, shipped bug: skipping to a later episode and watching under
    // `resume_seconds` of it silently discarded real progress on the
    // episode being left and could even auto-`Drop` the whole series. A
    // `Completed` classification, by contrast, is a positive reading
    // captured from a real, still-existing entry — unaffected by the
    // deletion-on-load problem — so it remains a valid, sufficient signal
    // on its own, wherever in the scan it's found.
    let mut best: Option<(usize, f64)> = None;
    let mut any_completed_found = false;
    for (idx, file) in ordered.iter().enumerate().skip(start_index) {
        let real_file = normalize(file, home);
        let Some(candidate) = candidates.get(&real_file) else {
            continue;
        };
        let Some(action) = classify_candidate(
            candidate,
            file,
            resume_seconds,
            completion_remaining_seconds,
        ) else {
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
        // Nothing real (Resume) exists anywhere from `start_index` on. If
        // a `Completed` reading was found anywhere in that same range,
        // resolve the cursor one or more steps further instead of
        // freezing (CLAUDE.md's "When the CURRENT tracked episode itself
        // completes" rule) — but only for a real InProgress/Paused record;
        // `Dropped`/never-started series have no "current episode" for
        // this rule to apply to. Absence alone (no `Completed` reading
        // anywhere) is deliberately never sufficient — see the scan loop's
        // own comment above.
        if let Some(i) = existing_index {
            let status = state.series[i].status;
            if status != Status::Dropped && any_completed_found {
                apply_completion_advance(
                    state,
                    i,
                    name,
                    &expanded_root,
                    &ordered,
                    candidates,
                    start_index,
                    resume_seconds,
                    completion_remaining_seconds,
                    home,
                );
            }
        }
        return;
    };

    let new_relative = ordered[found_index]
        .strip_prefix(&expanded_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| name.to_string());

    let changed = match existing_index {
        Some(i) => {
            let record = &state.series[i];
            record.current_relative_path != new_relative
                || (record.position_seconds - position).abs() > f64::EPSILON
        }
        None => true,
    };
    if !changed {
        return;
    }

    let identities = anitomy::episode_identity(&ordered);
    let episode_number = identities.get(found_index).map(|id| id.episode_number);
    let now = now_unix();

    match existing_index {
        Some(i) => {
            let record = &mut state.series[i];
            record.status = Status::InProgress;
            record.current_relative_path = new_relative;
            record.position_seconds = position;
            record.episode_number = episode_number;
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
            });
        }
    }
}

/// Called only once `reconcile_one_series` has established that
/// `state.series[i]`'s own currently-tracked episode is confirmed complete
/// and nothing real (`Resume`) exists anywhere in the series from there on.
/// Resolves the cursor as many steps further as the evidence actually
/// supports, in one pass — CLAUDE.md's "When the CURRENT tracked episode
/// itself completes" rule, including its own single-run-chaining case (the
/// newly-advanced-to episode having *already* separately completed too,
/// e.g. a binge that quit near the end of more than one episode in a row):
/// walks forward past every consecutive episode that also currently has a
/// real `Completed` reading, landing at position 0 of the first episode
/// found with no such evidence (a genuinely blank, never-yet-touched
/// episode) — or transitioning the record to `Dropped` if the walk runs
/// out of episodes entirely, i.e. the completed episode(s) were the actual
/// end of the series. `Dropped` is deliberately reached the same way a
/// manual `tsuzuki drop` reaches it (CLAUDE.md: no new state), and — like
/// manual `drop` — never mutates the stored `current_relative_path`/
/// `position_seconds` on the way there; only a landing on a real episode
/// does.
#[allow(clippy::too_many_arguments)]
fn apply_completion_advance(
    state: &mut State,
    i: usize,
    name: &str,
    expanded_root: &Path,
    ordered: &[PathBuf],
    candidates: &HashMap<PathBuf, Candidate>,
    start_index: usize,
    resume_seconds: u64,
    completion_remaining_seconds: u64,
    home: Option<&Path>,
) {
    let mut cursor = start_index;
    loop {
        if cursor + 1 >= ordered.len() {
            let record = &mut state.series[i];
            record.status = Status::Dropped;
            record.updated_at = now_unix();
            return;
        }
        cursor += 1;

        let real_file = normalize(&ordered[cursor], home);
        let still_completed = match candidates.get(&real_file) {
            Some(candidate) => matches!(
                classify_candidate(
                    candidate,
                    &ordered[cursor],
                    resume_seconds,
                    completion_remaining_seconds
                ),
                Some(PlaybackAction::Completed)
            ),
            // No entry at all — not completion evidence for this episode,
            // so the walk stops here rather than treating absence as a
            // signal. (`classify_candidate` itself already collapses a
            // below-threshold/no-position reading to `None`, so a present
            // but non-completing candidate falls through to `false` here
            // too, via the outer `matches!` on `Some(Completed)`.)
            None => false,
        };

        if !still_completed {
            let new_relative = ordered[cursor]
                .strip_prefix(expanded_root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| name.to_string());
            let identities = anitomy::episode_identity(ordered);
            let episode_number = identities.get(cursor).map(|id| id.episode_number);
            let record = &mut state.series[i];
            record.status = Status::InProgress;
            record.current_relative_path = new_relative;
            record.position_seconds = 0.0;
            record.episode_number = episode_number;
            record.updated_at = now_unix();
            return;
        }
        // Still completed — keep advancing past it without landing.
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

    /// Writes an mpv-style `watch_later` entry file for `source` at
    /// `position_seconds`, inside `watch_later_dir`, named distinctly from
    /// any other entry written in the same test (mpv itself names these by
    /// MD5 hash; the name doesn't matter to the reader, only its contents).
    fn write_entry(watch_later_dir: &Path, entry_name: &str, source: &Path, position_seconds: f64) {
        fs::create_dir_all(watch_later_dir).unwrap();
        fs::write(
            watch_later_dir.join(entry_name),
            format!("# {}\nstart={position_seconds}\n", source.display()),
        )
        .unwrap();
    }

    /// Sets an override-based single watch_later directory in `config`, so
    /// tests don't depend on real OS-default candidate paths / a real HOME.
    fn with_watch_later_override(config: &mut Config, dir: &Path) {
        config.mpv.watch_later_dir = dir.to_string_lossy().into_owned();
    }

    /// Writes a `tsuzuki.lua`-style script-log entry file for `source`
    /// inside `log_dir`, named distinctly from any other entry written in
    /// the same test (mpv itself names these by an 8-hex-char djb2 hash;
    /// the name doesn't matter to the reader, only its contents).
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

        let watch_later = dir.path().join("watch_later");
        // 45s is below the default 120s resume_seconds threshold — noise.
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 45.0);

        let mut config = default_config(&root);
        with_watch_later_override(&mut config, &watch_later);
        let mut state = State::default();

        reconcile_with_script_log_dir(&config, &mut state, None, None);

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

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 900.0,
                episode_number: Some(1),
                updated_at: 12345,
            }],
        };

        // mpv's own watch_later file now says 45s — a brief accidental
        // reopen — which would clobber the real 900s if propagated blindly.
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 45.0);

        reconcile_with_script_log_dir(&config, &mut state, None, None);

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.position_seconds, 900.0,
            "the real position must survive a noise reading unchanged"
        );
        assert_eq!(record.updated_at, 12345, "updated_at must not move either");
    }

    #[test]
    fn absence_equals_completion_only_fires_when_a_baseline_existed() {
        // No existing record, and no *real* entry anywhere in the series
        // (only a noise-level one) — must not fabricate a record out of
        // "absence" alone.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        // No entries at all for ep02 (absent) and nothing real for ep01.

        let mut state = State::default();
        reconcile_with_script_log_dir(&config, &mut state, None, None);

        assert!(state.series.is_empty());
    }

    #[test]
    fn single_run_chaining_resolves_a_multi_step_binge_in_one_call() {
        // Episode 1 was completed (its bookmark is gone — mpv deleted it on
        // clean EOF) and the user is already real minutes into episode 3,
        // all discovered in one reconcile() call starting from a record
        // still pointing at episode 1.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("ep03.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        // No entry for ep01 (completed, absent) or ep02 (never opened).
        write_entry(&watch_later, "entry3", &series_path.join("ep03.mkv"), 300.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 1100.0,
                episode_number: Some(1),
                updated_at: 1,
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);

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
        // "Monster" is never created on disk at all — simulates a
        // moved/deleted series folder.

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);

        let original = SeriesRecord {
            root: root.to_string_lossy().into_owned(),
            name: "Monster".to_string(),
            status: Status::InProgress,
            current_relative_path: "Monster/ep05.mkv".to_string(),
            position_seconds: 400.0,
            episode_number: Some(5),
            updated_at: 999,
        };
        let mut state = State {
            series: vec![original.clone()],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);

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

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        write_entry(&watch_later, "entry1", &series_path.join("ep02.mkv"), 500.0);

        let mut state = State::default();
        reconcile_with_script_log_dir(&config, &mut state, None, None);

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 500.0);
    }

    #[test]
    fn reconciles_own_write_is_stable_across_a_second_call_with_nothing_new_on_disk() {
        // Gap-audit required finding #1, third write path: reconcile's own
        // write (the "new activity"/"completion advances the cursor" path)
        // always stores exactly the real values it just found — never a
        // zeroed/cleared placeholder — so it must never manufacture a false
        // diff against itself on the very next call. Proven by calling
        // reconcile twice with nothing touched on disk in between and
        // asserting the second call is a true no-op (updated_at unchanged).
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 400.0);

        let mut state = State::default();
        reconcile_with_script_log_dir(&config, &mut state, None, None);
        let updated_at_after_first_call = state
            .find(&root.to_string_lossy(), "Monster")
            .unwrap()
            .updated_at;

        reconcile_with_script_log_dir(&config, &mut state, None, None);
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.updated_at, updated_at_after_first_call,
            "a second reconcile call with nothing new on disk must not treat \
             reconcile's own prior write as new activity"
        );
    }

    #[test]
    fn a_dropped_series_with_an_unchanged_bookmark_stays_dropped_across_reconcile_calls() {
        // Regression test for a real bug caught during Phase 5's dogfood
        // pass: `drop` used to zero `position_seconds` while leaving
        // `current_relative_path` unchanged. The very next reconcile then
        // compared its freshest real observation (the *same*, untouched
        // watch_later entry from before the drop) against that artificially
        // zeroed baseline, saw a "difference", and silently flipped the
        // series back to `in_progress` — undoing the drop with no user
        // action at all. `drop_series` no longer mutates the stored
        // position/path; this locks in that a genuinely unchanged bookmark
        // must never reactivate a dropped series, across any number of
        // reconcile calls, not just the first one.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("One Piece");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 300.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "One Piece".to_string(),
                status: Status::Dropped,
                current_relative_path: "One Piece/ep01.mkv".to_string(),
                position_seconds: 300.0,
                episode_number: Some(1),
                updated_at: 1,
            }],
        };

        // Two reconcile calls in a row, mpv's own file never touched again.
        reconcile_with_script_log_dir(&config, &mut state, None, None);
        reconcile_with_script_log_dir(&config, &mut state, None, None);

        let record = state.find(&root.to_string_lossy(), "One Piece").unwrap();
        assert_eq!(record.status, Status::Dropped);
        assert_eq!(record.updated_at, 1, "no genuine change occurred");
    }

    #[test]
    fn opening_an_episode_of_a_dropped_series_flips_it_back_to_in_progress() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        // Opens episode 1 directly (not even the last-watched one before drop).
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 400.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::Dropped,
                current_relative_path: "Monster/ep02.mkv".to_string(),
                position_seconds: 0.0,
                episode_number: None,
                updated_at: 1,
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 400.0);
    }

    #[test]
    fn an_unchanged_bookmark_never_flips_a_paused_series_back_to_in_progress() {
        // Pausing doesn't touch mpv's own watch_later file — its entry for
        // the paused episode still sits there completely unchanged. Every
        // command reconciles before acting, so if presence-alone were
        // enough to reactivate, `pause` would be immediately undone by the
        // very next command. Only a *change* may flip status.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 400.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::Paused,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 400.0,
                episode_number: Some(1),
                updated_at: 42,
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::Paused);
        assert_eq!(record.updated_at, 42);
    }

    #[test]
    fn a_paused_series_reactivates_when_a_new_real_bookmark_appears_on_a_later_episode() {
        // Gap-audit required finding #2: CLAUDE.md's "forward-scan is
        // strictly forward" note documents that a Paused record must NOT
        // reactivate from an EARLIER real bookmark. This is the other
        // direction, which is a genuinely different case and must not be
        // blocked the same way: the user opens a LATER episode directly,
        // bypassing tsuzuki entirely, while the series sits paused at
        // episode 1. That's forward motion from the paused baseline and
        // must correctly flip the record back to in_progress at the new,
        // later position — nothing about `Paused` status should suppress
        // forward movement, only backward movement is meant to be blocked.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("ep03.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        // ep01's own old bookmark is still there, completely unchanged...
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 200.0);
        // ...but the user has since opened ep03 directly and quit mid-file.
        write_entry(&watch_later, "entry3", &series_path.join("ep03.mkv"), 500.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::Paused,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 200.0,
                episode_number: Some(1),
                updated_at: 1,
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);

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
        // Real fixture: ~5.0s duration. With resume_seconds=1 and
        // completion_remaining_seconds=2, quitting at 4.0s (1.0s remaining)
        // classifies as Completed, not Resume — must be treated exactly
        // like an absent bookmark: scanning continues past it.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        let fixture = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/short.mkv"
        ));
        fs::create_dir_all(&series_path).unwrap();
        fs::copy(&fixture, series_path.join("ep01.mkv")).unwrap();
        fs::copy(&fixture, series_path.join("ep02.mkv")).unwrap();

        let mut config = default_config(&root);
        config.thresholds.resume_seconds = 1;
        config.thresholds.completion_remaining_seconds = 2;
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);

        // ep01: 4.0s into a ~5.0s file -> Completed (treated like absence).
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 4.0);
        // ep02: 1.5s into a ~5.0s file -> Resume (a real landing candidate).
        write_entry(&watch_later, "entry2", &series_path.join("ep02.mkv"), 1.5);

        let mut state = State::default();
        reconcile_with_script_log_dir(&config, &mut state, None, None);

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.current_relative_path, "Monster/ep02.mkv",
            "a Completed-classified entry on ep01 must not be the landing point"
        );
        assert_eq!(record.position_seconds, 1.5);
    }

    #[test]
    fn a_completed_reading_on_the_last_episode_auto_drops_instead_of_freezing_or_updating_in_place()
    {
        // Supersedes an earlier version of this test from the completion-
        // freeze fix, which expected a `Completed` reading on the current
        // file to just update that file's own stored position and stay
        // `InProgress`. CLAUDE.md's later "When the CURRENT tracked episode
        // itself completes" rule refines that: recording the fact alone
        // isn't the full behavior — a confirmed-complete episode must
        // resolve the cursor one step further. Here the series has only
        // one episode, so once it's confirmed complete there is no next
        // episode to advance to, and the correct outcome is `Dropped`, not
        // a same-file position update.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        let fixture = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/short.mkv"
        ));
        fs::create_dir_all(&series_path).unwrap();
        fs::copy(&fixture, series_path.join("ep01.mkv")).unwrap();

        let mut config = default_config(&root);
        config.thresholds.resume_seconds = 1;
        config.thresholds.completion_remaining_seconds = 2;
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);

        let mut state = State::default();

        // Call 1: 1.5s into a ~5.0s file -> 3.5s remaining -> Resume.
        // Ordinary update, establishes the real baseline this episode
        // needs before absence/Completed can ever mean anything.
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 1.5);
        reconcile_with_script_log_dir(&config, &mut state, None, None);
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.position_seconds, 1.5);
        assert_eq!(record.status, Status::InProgress);

        // Call 2: quit again, further in -> 4.0s -> 1.0s remaining ->
        // Completed. This is the actual last (only) episode in the series,
        // so the record must auto-Drop, not freeze and not just update its
        // own position in place.
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 4.0);
        reconcile_with_script_log_dir(&config, &mut state, None, None);
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.status,
            Status::Dropped,
            "completing the series' actual last episode must auto-Drop it"
        );
        // Per CLAUDE.md's "pause/drop never zero the stored position" rule
        // (extended to auto-drop for the same reconcile-diffing reason):
        // the stored position is left exactly as it was, not zeroed and
        // not updated to the Completed reading's own value.
        assert_eq!(record.position_seconds, 1.5);
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");

        // `overview.rs`'s display must show "Rewatch?" for this series
        // now, never "Resume" — the same word a manual `drop` produces,
        // since this is reached the same way.
        assert_eq!(
            crate::overview::start_display_word(Some(record.status)),
            "Rewatch?"
        );

        // Call 3: nothing changed on disk (mpv's own entry for ep01 is
        // still sitting there unchanged) — must stay stable, no error, no
        // further change. Dropped records use their own separate,
        // already-tested reconcile path (start_index = 0, whole-series
        // scan for genuinely new activity) — this confirms the auto-drop
        // transition doesn't leave anything that path would misinterpret.
        reconcile_with_script_log_dir(&config, &mut state, None, None);
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.status, Status::Dropped);
        assert_eq!(record.position_seconds, 1.5);
    }

    #[test]
    fn absence_alone_never_advances_or_mutates_the_record() {
        // Regression test for a real, reported data-loss bug: mpv's own
        // `--resume-playback` (on by default) deletes a file's watch_later
        // bookmark the instant it's loaded, regardless of who launched mpv
        // or what happens afterward — so "no entry for ep01" is produced
        // identically by a genuine completion and by simply reopening
        // ep01 and then abandoning it (e.g. skipping to ep02 mid-file via
        // mpv's own `playlist-next`, which never rewrites ep01's entry).
        // These are provably indistinguishable from watch_later state
        // alone, so absence must never, by itself, justify a mutation —
        // ep01's real stored position must survive completely untouched.
        // Nothing on ep02 either (the reported scenario's "watched under
        // resume_seconds" and "watched nothing at all" cases behave
        // identically here, since both fail to produce a `Resume` or
        // `Completed` reading).
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);

        // No entry for ep01 (deleted by mpv on load — not proof of
        // completion) and nothing at all for ep02.
        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 1100.0,
                episode_number: Some(1),
                updated_at: 1,
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 1100.0);
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.updated_at, 1);
    }

    #[test]
    fn a_sub_threshold_reading_on_the_next_episode_does_not_corroborate_the_current_ones_absence() {
        // The exact reported scenario: resuming ep01 at a real position,
        // skipping ahead to ep02 via mpv's `playlist-next` mid-file (which
        // never rewrites ep01's entry), then quitting after under
        // `resume_seconds` of ep02. ep01's entry is gone (deletion-on-load,
        // not completion) and ep02's own reading is real but noise-level —
        // neither is a `Resume` or `Completed` signal, so the record must
        // stay exactly on ep01 at its last real position, not jump to ep02
        // at position 0.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);

        // No entry for ep01. ep02: 60s watched, below the 120s default
        // resume_seconds threshold.
        write_entry(&watch_later, "entry2", &series_path.join("ep02.mkv"), 60.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 600.0,
                episode_number: Some(1),
                updated_at: 1,
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);
        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(record.current_relative_path, "Monster/ep01.mkv");
        assert_eq!(record.position_seconds, 600.0);
        assert_eq!(record.status, Status::InProgress);
        assert_eq!(record.updated_at, 1);
    }

    #[test]
    fn current_episode_completes_and_next_episode_already_has_its_own_real_resume_bookmark() {
        // The "binge" half of scope item 3/4: when the next episode
        // already has a genuine `Resume`-classified position of its own,
        // the ordinary forward-scan (unchanged by this feature) must still
        // find it directly — `apply_completion_advance` should never even
        // run, since `best` is already set by the normal scan before it's
        // consulted. Confirms the new auto-advance path doesn't shadow or
        // interfere with the existing binge-chaining mechanism.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        // No entry for ep01 (absence, completes it) — a real bookmark
        // already sitting on ep02 from the same binge session.
        write_entry(&watch_later, "entry2", &series_path.join("ep02.mkv"), 700.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 1100.0,
                episode_number: Some(1),
                updated_at: 1,
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);
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
        // Scope item 3's explicit multi-step case: the current episode
        // completes (absence), the newly-advanced-to episode has ALSO
        // already separately completed (a real `Completed` reading of its
        // own — a binge that quit near the end of more than one episode in
        // a row), and only the episode after THAT is genuinely blank. All
        // of this must resolve in a single reconcile() call.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        let fixture = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/short.mkv"
        ));
        touch(&series_path.join("ep01.mkv"));
        fs::create_dir_all(&series_path).unwrap();
        fs::copy(&fixture, series_path.join("ep02.mkv")).unwrap();
        touch(&series_path.join("ep03.mkv"));

        let mut config = default_config(&root);
        config.thresholds.resume_seconds = 1;
        config.thresholds.completion_remaining_seconds = 2;
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        // ep01: no entry -> absence. ep02: 4.0s into ~5.0s -> Completed.
        // ep03: no entry -> genuinely blank.
        write_entry(&watch_later, "entry2", &series_path.join("ep02.mkv"), 4.0);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 1100.0,
                episode_number: Some(1),
                updated_at: 1,
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);
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
        // Gap-audit required finding #3: absence and duration-based
        // Completed have each only ever been exercised independently (one
        // signal, nothing after it). Real usage can chain both in the same
        // sitting: ep01 completes cleanly (mpv writes no entry at all —
        // absence), ep02 is quit during the credits (a real position within
        // completion_remaining_seconds of a known duration — Completed),
        // and the user is already genuinely partway into ep03 (an ordinary
        // real Resume position). All three must resolve in a single
        // reconcile() call, landing on ep03 — not stopping at ep02 just
        // because it was the first entry actually present.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        let fixture = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/short.mkv"
        ));
        fs::create_dir_all(&series_path).unwrap();
        // ep01 has no watch_later entry at all (absence) — no real content
        // needed, duration is never probed for a file with no entry.
        touch(&series_path.join("ep01.mkv"));
        // ep02/ep03 need a real, probeable duration (~5.0s each).
        fs::copy(&fixture, series_path.join("ep02.mkv")).unwrap();
        fs::copy(&fixture, series_path.join("ep03.mkv")).unwrap();

        let mut config = default_config(&root);
        config.thresholds.resume_seconds = 1;
        config.thresholds.completion_remaining_seconds = 2;
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);

        // ep01: no entry at all -> absence.
        // ep02: 4.0s into ~5.0s -> 1.0s remaining -> Completed.
        write_entry(&watch_later, "entry2", &series_path.join("ep02.mkv"), 4.0);
        // ep03: 1.5s into ~5.0s -> 3.5s remaining -> ordinary Resume.
        write_entry(&watch_later, "entry3", &series_path.join("ep03.mkv"), 1.5);

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 3.0,
                episode_number: Some(1),
                updated_at: 1,
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, None);

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

        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        write_entry(&watch_later, "entry1", &series_path.join("ep01.mkv"), 300.0);

        let mut state = State::default();
        reconcile_with_script_log_dir(&config, &mut state, None, None);

        assert_eq!(state.series.len(), 1);
        let record = &state.series[0];
        assert_eq!(record.root, rewatches_root.to_string_lossy());
        assert_eq!(record.name, "Monster");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_series_folder_reconciles_correctly() {
        // Regression test for a real, reported bug, reproducing the exact
        // reported shape: a tracked root whose series folder is a symlink
        // pointing entirely outside the root's own real location (a
        // completely normal, expected library layout — CLAUDE.md's
        // "Symlinks are followed everywhere... a symlinked series folder
        // behaves exactly like a real one"). `classify_root_and_series`
        // used to canonicalize the *whole* watch_later entry path,
        // resolving straight through the symlink to a location that no
        // longer shared any prefix with the root's own canonical form —
        // silently dropping every real bookmark for that series,
        // permanently, on every single invocation. Confirmed directly
        // against a real (non-symlinked) sibling series continuing to
        // work throughout.
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let real_storage = dir.path().join("real_storage").join("Dungeon Meshi");
        let release_dir = real_storage.join("[1080p WEBRip][EMBER]");
        fs::create_dir_all(&release_dir).unwrap();
        touch(&release_dir.join("Dungeon Meshi - S01E01.mkv"));
        symlink(&real_storage, root.join("Dungeon Meshi")).unwrap();

        // An ordinary, non-symlinked sibling series under the same root —
        // must keep working throughout, isolating this as specifically a
        // symlink-resolution bug, not a wholesale regression.
        touch(&root.join("Shokei Shoujo").join("ep01.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later");
        with_watch_later_override(&mut config, &watch_later);
        write_entry(
            &watch_later,
            "entry-symlinked",
            &root
                .join("Dungeon Meshi")
                .join("[1080p WEBRip][EMBER]")
                .join("Dungeon Meshi - S01E01.mkv"),
            685.852,
        );
        write_entry(
            &watch_later,
            "entry-ordinary",
            &root.join("Shokei Shoujo").join("ep01.mkv"),
            551.259,
        );

        let mut state = State::default();
        reconcile_with_script_log_dir(&config, &mut state, None, None);

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

    #[test]
    fn scan_warning_is_surfaced_through_reconcile_outcome() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = default_config(&root);
        // A configured override that doesn't exist on disk.
        config.mpv.watch_later_dir = dir
            .path()
            .join("does-not-exist")
            .to_string_lossy()
            .into_owned();

        let mut state = State::default();
        let outcome = reconcile_with_script_log_dir(&config, &mut state, None, None);

        assert!(matches!(
            outcome.warning,
            Some(ScanWarning::WatchLaterDirNotFound(_))
        ));
    }

    // --- Phase 6: script-log candidate source ---
    //
    // `classify_candidate`/`merge_candidates` are exercised directly first
    // (mirroring `mpv.rs`'s own precedent of testing `merge_freshest_wins`
    // below the `scan_*` layer) before the full end-to-end `reconcile()`
    // behavior tests.

    #[test]
    fn classify_candidate_script_eof_is_completed_regardless_of_position_or_duration() {
        // Decision 3's central claim: an `eof` reading never goes through
        // the completion_remaining_seconds proximity check at all — not
        // even when the raw position/duration values would clearly say
        // otherwise (here: barely any time watched, duration unknown).
        let candidate = Candidate::ScriptLog(ScriptLogEntry {
            source_path: PathBuf::from("/Anime/Show/ep01.mkv"),
            duration: None,
            position: Some(3.0),
            reason: "eof".to_string(),
            mtime: SystemTime::now(),
        });

        let action = classify_candidate(&candidate, Path::new("/Anime/Show/ep01.mkv"), 120, 180);
        assert_eq!(action, Some(PlaybackAction::Completed));
    }

    #[test]
    fn classify_candidate_script_non_eof_below_resume_threshold_is_no_signal() {
        let candidate = Candidate::ScriptLog(ScriptLogEntry {
            source_path: PathBuf::from("/Anime/Show/ep01.mkv"),
            duration: None,
            position: Some(45.0),
            reason: "stop".to_string(),
            mtime: SystemTime::now(),
        });

        let action = classify_candidate(
            &candidate,
            Path::new("/definitely/does/not/exist/ep01.mkv"),
            120,
            180,
        );
        assert_eq!(action, None);
    }

    #[test]
    fn classify_candidate_script_non_eof_with_no_position_is_no_signal() {
        let candidate = Candidate::ScriptLog(ScriptLogEntry {
            source_path: PathBuf::from("/Anime/Show/ep01.mkv"),
            duration: None,
            position: None,
            reason: "error".to_string(),
            mtime: SystemTime::now(),
        });

        let action = classify_candidate(
            &candidate,
            Path::new("/definitely/does/not/exist/ep01.mkv"),
            120,
            180,
        );
        assert_eq!(action, None);
    }

    #[test]
    fn classify_candidate_script_non_eof_uses_its_own_duration_not_a_probe() {
        // duration=1000, position=900 -> remaining=100, under the 180s
        // completion threshold -> Completed. The file path given doesn't
        // exist on disk at all, proving the script's own `duration` field
        // was used directly rather than falling through to
        // `duration::probe_duration`, which would fail (Unknown) on a
        // nonexistent file and force a `Resume` classification instead.
        let candidate = Candidate::ScriptLog(ScriptLogEntry {
            source_path: PathBuf::from("/Anime/Show/ep01.mkv"),
            duration: Some(1000.0),
            position: Some(900.0),
            reason: "stop".to_string(),
            mtime: SystemTime::now(),
        });

        let action = classify_candidate(
            &candidate,
            Path::new("/definitely/does/not/exist/ep01.mkv"),
            120,
            180,
        );
        assert_eq!(action, Some(PlaybackAction::Completed));
    }

    #[test]
    fn merge_candidates_watch_later_only_is_selected() {
        let path = PathBuf::from("/Anime/Show/ep01.mkv");
        let watch_later = vec![WatchLaterEntry {
            source_path: path.clone(),
            position_seconds: 500.0,
            mtime: SystemTime::now(),
        }];

        let merged = merge_candidates(watch_later, Vec::new(), None);
        assert!(matches!(
            merged.get(&normalize(&path, None)),
            Some(Candidate::WatchLater(_))
        ));
    }

    #[test]
    fn merge_candidates_script_only_is_selected() {
        let path = PathBuf::from("/Anime/Show/ep01.mkv");
        let script_log = vec![ScriptLogEntry {
            source_path: path.clone(),
            duration: None,
            position: Some(500.0),
            reason: "stop".to_string(),
            mtime: SystemTime::now(),
        }];

        let merged = merge_candidates(Vec::new(), script_log, None);
        assert!(matches!(
            merged.get(&normalize(&path, None)),
            Some(Candidate::ScriptLog(_))
        ));
    }

    #[test]
    fn merge_candidates_watch_later_strictly_fresher_wins() {
        let path = PathBuf::from("/Anime/Show/ep01.mkv");
        let now = SystemTime::now();

        let watch_later = vec![WatchLaterEntry {
            source_path: path.clone(),
            position_seconds: 111.0,
            mtime: now,
        }];
        let script_log = vec![ScriptLogEntry {
            source_path: path.clone(),
            duration: None,
            position: Some(222.0),
            reason: "stop".to_string(),
            mtime: now - Duration::from_secs(60),
        }];

        let merged = merge_candidates(watch_later, script_log, None);
        match merged.get(&normalize(&path, None)) {
            Some(Candidate::WatchLater(e)) => assert_eq!(e.position_seconds, 111.0),
            other => {
                panic!("expected the strictly fresher watch_later entry to win, got {other:?}")
            }
        }
    }

    #[test]
    fn merge_candidates_script_strictly_fresher_wins() {
        let path = PathBuf::from("/Anime/Show/ep01.mkv");
        let now = SystemTime::now();

        let watch_later = vec![WatchLaterEntry {
            source_path: path.clone(),
            position_seconds: 111.0,
            mtime: now - Duration::from_secs(60),
        }];
        let script_log = vec![ScriptLogEntry {
            source_path: path.clone(),
            duration: None,
            position: Some(222.0),
            reason: "stop".to_string(),
            mtime: now,
        }];

        let merged = merge_candidates(watch_later, script_log, None);
        match merged.get(&normalize(&path, None)) {
            Some(Candidate::ScriptLog(e)) => assert_eq!(e.position, Some(222.0)),
            other => panic!("expected the strictly fresher script entry to win, got {other:?}"),
        }
    }

    #[test]
    fn merge_candidates_on_an_exact_mtime_tie_the_script_entry_wins() {
        let path = PathBuf::from("/Anime/Show/ep01.mkv");
        let now = SystemTime::now();

        let watch_later = vec![WatchLaterEntry {
            source_path: path.clone(),
            position_seconds: 111.0,
            mtime: now,
        }];
        let script_log = vec![ScriptLogEntry {
            source_path: path.clone(),
            duration: None,
            position: Some(222.0),
            reason: "stop".to_string(),
            mtime: now,
        }];

        let merged = merge_candidates(watch_later, script_log, None);
        match merged.get(&normalize(&path, None)) {
            Some(Candidate::ScriptLog(e)) => assert_eq!(e.position, Some(222.0)),
            other => panic!("expected the script entry to win an exact mtime tie, got {other:?}"),
        }
    }

    // --- End-to-end `reconcile()` behavior, both candidate sources ---

    #[test]
    fn script_only_eof_on_the_last_episode_auto_drops_flagship() {
        // The flagship test: a script-log-only `eof` reading, with zero
        // watch_later trace at all (mpv's own resume-playback deleted the
        // bookmark on load, exactly as it always does — CLAUDE.md's
        // documented blind spot), on a series' actual LAST episode. Before
        // Phase 6, this scenario was permanently unrecoverable: absence
        // alone is never proof of anything, so the record would freeze at
        // its old position forever with no way to ever confirm completion.
        // With the script installed, the direct `eof` signal now closes
        // that gap completely — auto-Drop fires exactly as it would have
        // for an ordinary duration-proximity `Completed` reading.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later"); // left empty
        with_watch_later_override(&mut config, &watch_later);
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
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, Some(&script_log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.status,
            Status::Dropped,
            "a script-only eof reading on the series' last episode must auto-Drop it"
        );
        // Per the pause/drop-never-zeroes-position rule (CLAUDE.md),
        // extended to auto-drop: the stored position/path are untouched.
        assert_eq!(record.current_relative_path, "Monster/ep02.mkv");
        assert_eq!(record.position_seconds, 900.0);
    }

    #[test]
    fn script_only_eof_on_a_non_last_episode_advances_the_cursor() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("ep03.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later"); // left empty
        with_watch_later_override(&mut config, &watch_later);
        let script_log_dir = dir.path().join("mpv-log");
        // ep01: script-only eof, zero watch_later trace. ep02/ep03: nothing
        // at all from either source (genuinely blank).
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
            }],
        };

        reconcile_with_script_log_dir(&config, &mut state, None, Some(&script_log_dir));

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

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later"); // left empty
        with_watch_later_override(&mut config, &watch_later);
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
        reconcile_with_script_log_dir(&config, &mut state, None, Some(&script_log_dir));

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

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later"); // left empty
        with_watch_later_override(&mut config, &watch_later);
        let script_log_dir = dir.path().join("mpv-log");
        // 45s is below the default 120s resume_seconds threshold.
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            None,
            Some(45.0),
            "stop",
        );

        let mut state = State::default();
        reconcile_with_script_log_dir(&config, &mut state, None, Some(&script_log_dir));

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

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later"); // left empty
        with_watch_later_override(&mut config, &watch_later);
        let script_log_dir = dir.path().join("mpv-log");
        // A large, real-looking position on a `redirect` entry must still
        // never surface as activity.
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(1000.0),
            Some(950.0),
            "redirect",
        );

        let mut state = State::default();
        reconcile_with_script_log_dir(&config, &mut state, None, Some(&script_log_dir));

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

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later"); // left empty
        with_watch_later_override(&mut config, &watch_later);
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
        reconcile_with_script_log_dir(&config, &mut state, None, Some(&script_log_dir));

        let record = state.find(&root.to_string_lossy(), "Monster").unwrap();
        assert_eq!(
            record.position_seconds, 400.0,
            "the valid entry must still be picked up despite a malformed sibling file"
        );
    }

    #[test]
    fn reconcile_twice_against_an_unchanged_script_log_entry_reflecting_a_prior_drop_is_a_no_op() {
        // Mirrors the existing watch_later regression test for the same
        // real bug class (drop never zeroing stored position, so a later
        // reconcile mustn't manufacture a false diff against its own
        // baseline) — but for a script-log-sourced reading. Script-log
        // entries persist forever on disk (mpv never deletes them, unlike
        // watch_later's deletion-on-load), so this is the more likely real
        // shape of the bug going forward: the same eof entry sitting there,
        // completely unchanged, on every single future invocation.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let series_path = root.join("One Piece");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let mut config = default_config(&root);
        let watch_later = dir.path().join("watch_later"); // left empty
        with_watch_later_override(&mut config, &watch_later);
        let script_log_dir = dir.path().join("mpv-log");
        write_script_entry(
            &script_log_dir,
            "entry1",
            &series_path.join("ep01.mkv"),
            Some(1420.0),
            Some(1418.0),
            "eof",
        );

        let mut state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "One Piece".to_string(),
                status: Status::Dropped,
                current_relative_path: "One Piece/ep01.mkv".to_string(),
                position_seconds: 300.0,
                episode_number: Some(1),
                updated_at: 1,
            }],
        };

        // Two reconcile calls in a row, the script-log entry never touched
        // again in between.
        reconcile_with_script_log_dir(&config, &mut state, None, Some(&script_log_dir));
        reconcile_with_script_log_dir(&config, &mut state, None, Some(&script_log_dir));

        let record = state.find(&root.to_string_lossy(), "One Piece").unwrap();
        assert_eq!(
            record.status,
            Status::Dropped,
            "the prior drop must not be undone"
        );
        assert_eq!(record.updated_at, 1, "no genuine change occurred");
        assert_eq!(record.position_seconds, 300.0);
    }

    #[test]
    fn set_mtime_helper_actually_changes_mtime() {
        // Sanity check for the `set_mtime` helper itself (used above by the
        // merge_candidates tie/freshness tests) — not a behavioral test of
        // reconcile.rs, just a guard against the helper silently no-oping.
        let dir = tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, b"x").unwrap();
        let when = SystemTime::now() - Duration::from_secs(3600);
        set_mtime(&path, when);
        let got = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(got <= SystemTime::now() - Duration::from_secs(3000));
    }
}
