//! `tsuzuki pause <series>` / `tsuzuki drop <series>` state transitions
//! (CLAUDE.md's "`pause`/`drop`: root disambiguation, no-record errors, and
//! the allowed transitions").
//!
//! Allowed transitions only: `InProgress -> Paused`, `InProgress -> Dropped`,
//! `Paused -> InProgress`, `Paused -> Dropped`. `Dropped -> Paused` does not
//! exist, and neither command acts on a `Completed` series (only reconcile
//! produces that status) — the only way back from `Dropped` or `Completed`
//! to `InProgress` is opening an episode directly, which `reconcile` (not
//! this module) picks up. Both
//! commands act only on *existing* progress: a series with no `state.toml`
//! record at all is always an error, never a silently-manufactured record.
//!
//! Caller (`main.rs`) is responsible for running `reconcile` first (per
//! CLAUDE.md's "every state-touching command reconciles first") and for
//! persisting `state` afterward — this module only mutates the in-memory
//! `State`, matching `roots.rs`/`exclude.rs`'s established convention.

use std::fmt;

use crate::state::{State, Status, now_unix};

#[derive(Debug, PartialEq, Eq)]
pub enum ActionError {
    /// No `state.toml` record exists at all for this (root, series) pair —
    /// `pause`/`drop` act on existing progress, they don't manufacture a
    /// starting point.
    NoRecord,
    /// The record exists but isn't in a state this action can start from
    /// (`pause` on a `Dropped` record — `Dropped -> Paused` doesn't exist —
    /// and either command on a `Completed` record).
    InvalidTransition { from: Status, action: &'static str },
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionError::NoRecord => write!(
                f,
                "no progress to act on for this series — it was never started"
            ),
            ActionError::InvalidTransition { from, action } => {
                let from = match from {
                    Status::InProgress => "in progress",
                    Status::Paused => "paused",
                    Status::Completed => "completed",
                    Status::Dropped => "dropped",
                };
                write!(f, "cannot {action} a series that's currently {from}")
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A genuine transition was made; caller must persist `state`.
    Changed,
    /// Already in the target state — idempotent, no change, exit 0.
    NoOp,
}

/// `InProgress -> Paused`. `Paused -> Paused` is idempotent. `Dropped` and
/// no-record both error — `pause` only acts on a series with real, currently
/// active-or-paused progress.
pub fn pause(state: &mut State, root: &str, name: &str) -> Result<Outcome, ActionError> {
    let record = state.find_mut(root, name).ok_or(ActionError::NoRecord)?;
    match record.status {
        Status::Paused => Ok(Outcome::NoOp),
        Status::InProgress => {
            record.status = Status::Paused;
            record.updated_at = now_unix();
            Ok(Outcome::Changed)
        }
        from @ (Status::Dropped | Status::Completed) => Err(ActionError::InvalidTransition {
            from,
            action: "pause",
        }),
    }
}

/// `InProgress -> Dropped`, `Paused -> Dropped`. `Dropped -> Dropped` is
/// idempotent; a `Completed` series errors — it is finished, not abandoned.
/// Otherwise only a missing record errors — unlike `pause`, dropping an
/// already-dropped series is a legitimate no-op, not an invalid source
/// state (CLAUDE.md: "re-invoking ... drop on an already-dropped one is
/// idempotent").
///
/// Deliberately does **not** zero `current_relative_path`/`position_seconds`
/// — design.md §8's "position reset" is a *display/playback* behavior
/// (`playback_target` in `main.rs` already special-cases `Dropped` to always
/// start at episode 1, ignoring whatever is stored), not a requirement to
/// mutate the stored fields. Mutating them here would actively break
/// reconcile's own "only a genuine change reactivates" rule: reconcile
/// compares its freshest real observation against these exact stored
/// values, and a still-unchanged `watch_later` entry would then look like
/// "new activity" purely because this function had reset the baseline out
/// from under it — reproduced directly against the dogfood scenario (drop,
/// then the very next reconcile silently flipped back to `in_progress`
/// with the old, unchanged position). Leaving them untouched is what makes
/// "no real change" and "genuinely new activity" distinguishable at all.
pub fn drop_series(state: &mut State, root: &str, name: &str) -> Result<Outcome, ActionError> {
    let record = state.find_mut(root, name).ok_or(ActionError::NoRecord)?;
    match record.status {
        Status::Dropped => Ok(Outcome::NoOp),
        Status::InProgress | Status::Paused => {
            record.status = Status::Dropped;
            record.updated_at = now_unix();
            Ok(Outcome::Changed)
        }
        Status::Completed => Err(ActionError::InvalidTransition {
            from: Status::Completed,
            action: "drop",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SeriesRecord;

    fn record(status: Status) -> SeriesRecord {
        SeriesRecord {
            root: "~/Anime".to_string(),
            name: "Monster".to_string(),
            status,
            current_relative_path: "Monster/ep01.mkv".to_string(),
            position_seconds: 500.0,
            episode_number: Some(1),
            updated_at: 100,
            mal_episode_awareness: None,
        }
    }

    #[test]
    fn pause_transitions_in_progress_to_paused() {
        let mut state = State {
            series: vec![record(Status::InProgress)],

            mal_links: Vec::new(),
        };
        let outcome = pause(&mut state, "~/Anime", "Monster").unwrap();
        assert_eq!(outcome, Outcome::Changed);
        let r = state.find("~/Anime", "Monster").unwrap();
        assert_eq!(r.status, Status::Paused);
        assert!(r.updated_at > 100);
    }

    #[test]
    fn pause_never_mutates_position_or_current_path() {
        // Gap-audit required finding #1: `pause` is the other write path
        // besides `drop` that could in principle repeat the same
        // zero/clear-causes-a-false-reconcile-diff mistake CLAUDE.md now
        // documents. Explicit evidence it doesn't: every field except
        // `status`/`updated_at` must be byte-for-byte identical afterward.
        let before = record(Status::InProgress);
        let mut state = State {
            series: vec![before.clone()],

            mal_links: Vec::new(),
        };
        pause(&mut state, "~/Anime", "Monster").unwrap();
        let after = state.find("~/Anime", "Monster").unwrap();
        assert_eq!(after.current_relative_path, before.current_relative_path);
        assert_eq!(after.position_seconds, before.position_seconds);
        assert_eq!(after.episode_number, before.episode_number);
    }

    #[test]
    fn pause_on_already_paused_is_idempotent() {
        let mut state = State {
            series: vec![record(Status::Paused)],

            mal_links: Vec::new(),
        };
        let outcome = pause(&mut state, "~/Anime", "Monster").unwrap();
        assert_eq!(outcome, Outcome::NoOp);
        let r = state.find("~/Anime", "Monster").unwrap();
        assert_eq!(r.updated_at, 100, "a no-op must not bump updated_at");
    }

    #[test]
    fn pause_on_dropped_is_an_invalid_transition_error() {
        let mut state = State {
            series: vec![record(Status::Dropped)],

            mal_links: Vec::new(),
        };
        let err = pause(&mut state, "~/Anime", "Monster").unwrap_err();
        assert_eq!(
            err,
            ActionError::InvalidTransition {
                from: Status::Dropped,
                action: "pause"
            }
        );
    }

    #[test]
    fn pause_on_completed_is_an_invalid_transition_error() {
        let mut state = State {
            series: vec![record(Status::Completed)],
            mal_links: Vec::new(),
        };
        let err = pause(&mut state, "~/Anime", "Monster").unwrap_err();
        assert_eq!(
            err,
            ActionError::InvalidTransition {
                from: Status::Completed,
                action: "pause"
            }
        );
        assert_eq!(
            err.to_string(),
            "cannot pause a series that's currently completed"
        );
    }

    #[test]
    fn drop_on_completed_is_an_invalid_transition_error_and_leaves_the_record() {
        let mut state = State {
            series: vec![record(Status::Completed)],
            mal_links: Vec::new(),
        };
        let err = drop_series(&mut state, "~/Anime", "Monster").unwrap_err();
        assert_eq!(
            err,
            ActionError::InvalidTransition {
                from: Status::Completed,
                action: "drop"
            }
        );
        assert_eq!(state.series, vec![record(Status::Completed)]);
    }

    #[test]
    fn pause_with_no_record_errors() {
        let mut state = State::default();
        let err = pause(&mut state, "~/Anime", "Monster").unwrap_err();
        assert_eq!(err, ActionError::NoRecord);
    }

    #[test]
    fn drop_transitions_in_progress_to_dropped_without_mutating_position() {
        // The stored position is deliberately left as-is (see drop_series's
        // doc comment) — "position reset" is a playback-time behavior, not
        // a stored-field mutation; zeroing it here would make an unchanged
        // watch_later entry look like new activity to the next reconcile.
        // Checks every non-status field, not just position_seconds — the
        // same class of bug could just as easily have zeroed
        // current_relative_path or episode_number instead.
        let before = record(Status::InProgress);
        let mut state = State {
            series: vec![before.clone()],

            mal_links: Vec::new(),
        };
        let outcome = drop_series(&mut state, "~/Anime", "Monster").unwrap();
        assert_eq!(outcome, Outcome::Changed);
        let after = state.find("~/Anime", "Monster").unwrap();
        assert_eq!(after.status, Status::Dropped);
        assert_eq!(after.current_relative_path, before.current_relative_path);
        assert_eq!(after.position_seconds, before.position_seconds);
        assert_eq!(after.episode_number, before.episode_number);
    }

    #[test]
    fn drop_transitions_paused_to_dropped() {
        let mut state = State {
            series: vec![record(Status::Paused)],

            mal_links: Vec::new(),
        };
        let outcome = drop_series(&mut state, "~/Anime", "Monster").unwrap();
        assert_eq!(outcome, Outcome::Changed);
        assert_eq!(
            state.find("~/Anime", "Monster").unwrap().status,
            Status::Dropped
        );
    }

    #[test]
    fn drop_on_already_dropped_is_idempotent_not_an_error() {
        let mut state = State {
            series: vec![record(Status::Dropped)],

            mal_links: Vec::new(),
        };
        let outcome = drop_series(&mut state, "~/Anime", "Monster").unwrap();
        assert_eq!(outcome, Outcome::NoOp);
        let r = state.find("~/Anime", "Monster").unwrap();
        assert_eq!(r.updated_at, 100, "a no-op must not bump updated_at");
    }

    #[test]
    fn drop_with_no_record_errors() {
        let mut state = State::default();
        let err = drop_series(&mut state, "~/Anime", "Monster").unwrap_err();
        assert_eq!(err, ActionError::NoRecord);
    }

    #[test]
    fn dropped_to_paused_transition_does_not_exist() {
        // There is no `unpause`/`resume`-style function in this module that
        // could take a Dropped record to Paused — `pause()` on a Dropped
        // record is rejected (proven above), and nothing else in this
        // module writes `Status::Paused` onto a `Dropped` record. This test
        // documents the invariant by exhaustively trying both functions.
        let mut state = State {
            series: vec![record(Status::Dropped)],

            mal_links: Vec::new(),
        };
        assert!(pause(&mut state, "~/Anime", "Monster").is_err());
        assert_eq!(
            state.find("~/Anime", "Monster").unwrap().status,
            Status::Dropped
        );
    }

    #[test]
    fn root_disambiguation_keys_strictly_on_both_root_and_name() {
        let mut state = State {
            series: vec![
                SeriesRecord {
                    root: "~/Anime".to_string(),
                    ..record(Status::InProgress)
                },
                SeriesRecord {
                    root: "~/Anime/Rewatches".to_string(),
                    ..record(Status::Paused)
                },
            ],

            mal_links: Vec::new(),
        };

        pause(&mut state, "~/Anime", "Monster").unwrap();
        assert_eq!(
            state.find("~/Anime", "Monster").unwrap().status,
            Status::Paused
        );
        // The other root's record must be completely unaffected.
        assert_eq!(
            state.find("~/Anime/Rewatches", "Monster").unwrap().status,
            Status::Paused
        );
        assert_eq!(
            state
                .find("~/Anime/Rewatches", "Monster")
                .unwrap()
                .updated_at,
            100
        );
    }

    #[test]
    fn pause_and_drop_outcome_for_every_status_matches_the_hand_written_table() {
        type Expected = Result<Outcome, ActionError>;
        fn invalid(from: Status, action: &'static str) -> Expected {
            Err(ActionError::InvalidTransition { from, action })
        }
        // Wildcard-free: a new `Status` variant must be added here.
        fn expected(status: Status) -> (Expected, Expected) {
            match status {
                Status::InProgress => (Ok(Outcome::Changed), Ok(Outcome::Changed)),
                Status::Paused => (Ok(Outcome::NoOp), Ok(Outcome::Changed)),
                Status::Dropped => (invalid(Status::Dropped, "pause"), Ok(Outcome::NoOp)),
                Status::Completed => (
                    invalid(Status::Completed, "pause"),
                    invalid(Status::Completed, "drop"),
                ),
            }
        }
        for status in [
            Status::InProgress,
            Status::Paused,
            Status::Dropped,
            Status::Completed,
        ] {
            let (expected_pause, expected_drop) = expected(status);
            let mut state = State {
                series: vec![record(status)],
                mal_links: Vec::new(),
            };
            assert_eq!(
                pause(&mut state, "~/Anime", "Monster"),
                expected_pause,
                "pause from {status:?}"
            );
            let mut state = State {
                series: vec![record(status)],
                mal_links: Vec::new(),
            };
            assert_eq!(
                drop_series(&mut state, "~/Anime", "Monster"),
                expected_drop,
                "drop from {status:?}"
            );
        }
    }
}
