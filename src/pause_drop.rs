//! `pause` / `drop` state transitions.
//!
//! Allowed: `InProgress -> Paused`, `InProgress -> Dropped`,
//! `Paused -> InProgress` (via reconcile), `Paused -> Dropped`. `Completed`
//! is never touched, and `Dropped -> Paused` does not exist. A series with
//! no record is always an error.
//!
//! The caller reconciles first and persists `state` afterward; this module
//! only mutates the in-memory `State`.

use std::fmt;

use crate::state::{State, Status, now_unix};

#[derive(Debug, PartialEq, Eq)]
pub enum ActionError {
    /// No record exists for this (root, series) pair.
    NoRecord,
    /// The record's status can't start this action (`pause` on `Dropped`,
    /// either command on `Completed`).
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
    /// The status changed; the caller must persist `state`.
    Changed,
    /// Already in the target status; nothing changed.
    NoOp,
}

/// `InProgress -> Paused`. Already `Paused` is a no-op; `Dropped`,
/// `Completed` and no record error.
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

/// `InProgress`/`Paused -> Dropped`. Already `Dropped` is a no-op;
/// `Completed` and no record error.
///
/// Leaves `current_relative_path` and `position_seconds` untouched: reconcile
/// compares new observations against them, so resetting them would make an
/// unchanged `watch_later` entry look like new activity and reactivate the
/// series.
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
        // Every field except `status`/`updated_at` must stay identical.
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
        // Checks every non-status field, not just the position (see
        // `drop_series`).
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
        // Neither function may take a Dropped record to Paused.
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
