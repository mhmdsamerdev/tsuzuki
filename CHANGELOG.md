# Changelog

All notable changes to Tsuzuki are documented here.

## [Unreleased]

- `add`, `exclude`, `pause` and `drop` accept multiple arguments.
- Fixed `sync` marking a manually dropped series Completed on MAL; dropped series are now skipped.
- A series whose final episode finishes is now `completed` (shown as "Rewatch?") instead of `dropped`; earlier ones stay `dropped`.

## [v0.1.2]

- Fixed correct episode_number when a completion chain spans multiple watched episodes.
- `tsuzuki doctor` now prints the tool's own version.
- Added `tsuzuki update`, which checks GitHub for a newer release and
  installs it directly (via the existing install script) when one is found.

## [v0.1.1]

- Fixed MAL sync pulls leaving Tsuzuki's local resume cursor on an older episode.
- Paused series are now excluded from MAL progress sync.
- Deleted or otherwise unavailable series folders are now excluded from MAL sync.
- Unchanged series are omitted from sync output so real actions and warnings stand out.
- MAL entries now move from Plan to Watch to Watching on progress and to Completed when finished, recording the transition dates.

## [v0.1.0]

- Tracked roots, natural-sort episode ordering, and exclusions.
- mpv `watch_later` tracking, with an optional companion script
  (`contrib/mpv/tsuzuki.lua`) for exact completion detection.
- The `in_progress`/`paused`/`dropped` state machine, including
  `auto_pause_days` idle auto-pause.
- `tsuzuki doctor` environment health checks.
- Two-way, non-destructive MyAnimeList sync.
- Prebuilt-binary distribution via a one-line install script.
