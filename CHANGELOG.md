# Changelog

All notable changes to Tsuzuki are documented here.

## [v0.1.10]

- Fix the `resume`/`start` pickers garbling the screen when the terminal is small or resized.
- Fix progress not updating when a later episode is finished after an earlier one was stopped mid-way.

## [v0.1.9]

- The `resume` picker shows the episode and saved position like `status`: `Name — ep. 9 (12:34)`.
- `list` shows the saved position for in-progress and paused series.
- Resuming an ani-cli show returns to the shell once playback starts, and its next episode no longer starts at the resumed position.
- `start` now offers paused ani-cli shows.
- An ani-cli show completes when ani-cli has no next episode, instead of failing on resume.

## [v0.1.8]

- Fix progress not updating when a series contains symlinked season folders or episode files.
- `doctor` warns when Flatpak mpv can't see a folder your symlinks point to.

## [v0.1.7]

- `status` shows the saved position when it is past 0:00.
- ani-cli support: `tsuzuki add ani-cli`, then `tsuzuki` resumes what you watched through ani-cli. 

## [v0.1.6]

- Faster commands with a large mpv-log.
- The install script now installs to `~/.local/bin` instead of `~/.cargo/bin`.

## [v0.1.5]

- Breaking: mpv's `watch_later` data support is dropped, the mpv script is required. Install it with `tsuzuki doctor --fix`; `tsuzuki`/`start` refuse to launch without it.
- New `[mpv] install` setting (`auto`, `native`, `flatpak`, `custom`) picks which mpv to use; `custom` takes `command` and `script_path`.
- `doctor` checks and fixes only the mpv in use, explains why it was chosen, and warns about future-dated mpv entries.
- mpv's output goes to `mpv-launch.log` next to `state.toml`; the launch line starts with `→` instead of `✓`.
- Watching a dropped or completed series again reactivates it.
- `sync exclude <series|root>` keeps a series, or a whole root, out of MAL sync.
- `sync` refreshes the MAL token on 401, and stops with one short message when MAL is unreachable.
- `doctor` asks Flatpak whether mpv can see each tracked root, fixing a false permissions warning (e.g. with `host:ro`).
- `doctor` reports moved or deleted series as a `⚠ stale records` warning with one name per line (first 3, then "… and N more"); `--forget-orphans` lists every record it removes.
- Faster, leaner build: rustls instead of OpenSSL, ureq 3 (~45 fewer crates); `sync login` opens the browser with `xdg-open`.

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
