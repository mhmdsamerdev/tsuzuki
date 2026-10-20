# Changelog

All notable changes to Tsuzuki are documented here.

## [Unreleased]

- Leaner build: HTTP client upgraded to ureq 3 (~45 fewer crates); `sync login` opens the browser with `xdg-open`.
- TLS now uses rustls instead of a vendored OpenSSL, so builds are much faster.
- `tsuzuki` and `start` refuse to launch mpv until the mpv script is installed (an empty script counts as not installed).
- New `[mpv] install` setting (`auto`, `native`, `flatpak`, `custom`) picks the mpv to use; `custom` takes `command` and `script_path`. `command_override` still works.
- `doctor` checks and fixes only the mpv in use, and says why it was chosen.
- Snap mpv is no longer mistaken for native mpv.
- Flatpak mpv now counts as installed only when the `flatpak` command is on `PATH`.
- Native mpv honors `MPV_HOME` when `doctor` and launch look for the mpv script.
- `doctor --fix` updates a symlinked mpv script in place instead of replacing the link.
- mpv's output now goes to `mpv-launch.log` next to `state.toml` (overwritten each launch), and the launch line starts with `→` instead of `✓` since Tsuzuki can't know mpv succeeded.
- Breaking: mpv's `watch_later` data is no longer read. Install the mpv script with `tsuzuki doctor --fix`; without it nothing is tracked.
- Removed the `[mpv] watch_later_dir` setting (old configs still load) and the mpv.conf check from `doctor`.
- Dropped video-header probing; completion now comes only from the script's `eof` and recorded duration.
- `doctor` warns when mpv entries are dated in the future.
- Watching a dropped or completed series again now reactivates it, including finishing an episode.
- `sync exclude <series|root>` keeps a series, or every series under a root, out of MAL sync.
- `sync` refreshes the MAL token and retries when MAL answers 401 instead of failing every series.
- `sync` stops with one short message when MyAnimeList is unreachable instead of repeating a long error per series.

## [v0.1.4]

- `start` and `resume` accept a root's folder name to narrow the picker to that root.
- Episodes past season 1 show their season (`S02E01`, `Season 2, Episode 1`) when the filename names one.
- A folder that is both a root and a folder inside another root is no longer treated as a series of the other root.

## [v0.1.3]

- `start`/`resume` choices are grouped by root, and `list` tags each series with its root, when several roots are tracked.
- `add`, `exclude`, `pause` and `drop` accept multiple arguments.
- Fixed `sync` marking a manually dropped series Completed on MAL; dropped series are now skipped.
- A series whose final episode finishes is now `completed` (own group in `list`, "Rewatch?" in `start`) instead of `dropped`; earlier ones stay `dropped`.

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
