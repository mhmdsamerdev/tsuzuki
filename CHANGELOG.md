# Changelog

All notable changes to Tsuzuki are documented here.

## [Unreleased]

- Fixed MAL sync pulls leaving Tsuzuki's local resume cursor on an older episode.
- Paused series are now excluded from MAL progress sync.
- Deleted or otherwise unavailable series folders are now excluded from MAL sync.

## [v0.1.0]

- Tracked roots, natural-sort episode ordering, and exclusions.
- mpv `watch_later` tracking, with an optional companion script
  (`contrib/mpv/tsuzuki.lua`) for exact completion detection.
- The `in_progress`/`paused`/`dropped` state machine, including
  `auto_pause_days` idle auto-pause.
- `tsuzuki doctor` environment health checks.
- Two-way, non-destructive MyAnimeList sync.
- Prebuilt-binary distribution via a one-line install script.
