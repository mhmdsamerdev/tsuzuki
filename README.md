# Tsuzuki

*続き — "continuation."*

You watch an episode in [mpv](https://mpv.io), quit, and forget about it.
Tomorrow, you type:

```
tsuzuki
```

The right file opens, at the right timestamp. No searching folders, no
remembering episode numbers, no thinking.

That's the whole tool.

---

## What it isn't

Tsuzuki is not a media player, a library manager, or a MyAnimeList
replacement. Those are already solved. Tsuzuki answers one question, and
only one: *what was I watching, where did I stop, and can you take me
back there instantly?*

## How it works

Point it at the folders where your anime already lives:

```
tsuzuki add ~/Anime
```

Watch something in mpv, the way you always have. Quit whenever — mid
episode, end of episode, doesn't matter. Next time, just run:

```
tsuzuki
```

One series in progress? It resumes immediately, no prompt. A few
different things going at once? Pick one with the arrow keys. Nothing in
progress yet? It shows you what's available to start. With several
roots tracked, choices are grouped by root (and `tsuzuki list` tags each
series with its root).

Your filesystem is already your library. Tsuzuki never reorganizes it,
never renames anything, never guesses which files are "real" episodes —
it just remembers where you were and takes you back.

## A one-time setup step

Tsuzuki learns what you watched from a small mpv script,
[`contrib/mpv/tsuzuki.lua`](contrib/mpv/tsuzuki.lua). Install it once:

```
tsuzuki doctor --fix
```

That copies the script into mpv's own `scripts/` folder (or copy it there
yourself). Without it Tsuzuki can't see what you watch, so `tsuzuki`
won't start playback until it's installed. It records when you finish an
episode and where you stopped, so binging straight through a series
without ever pausing works.

Run `tsuzuki doctor` any time to check this — and everything else Tsuzuki
depends on — is set up correctly.

## Commands you'll actually use

| | |
|---|---|
| `tsuzuki` | Resume what you were watching. |
| `tsuzuki start` | Nothing in progress? See what's available and begin. |
| `tsuzuki start <root>` | Same, narrowed to one root by its folder name (`resume` takes it too). |
| `tsuzuki status` | What's currently in progress. |
| `tsuzuki list` | Everything you're tracking, at a glance. |
| `tsuzuki pause <series>...` | Set something aside without losing your place. |
| `tsuzuki drop <series>...` | Done with it, or starting fresh. |
| `tsuzuki add <path>...` | Start tracking a folder. |
| `tsuzuki exclude <path>...` | Keep specials/OVAs out of the episode order. |
| `tsuzuki config` | See and edit your settings. |
| `tsuzuki doctor` | Make sure everything's set up right. |

Every command is explicit and predictable. Tsuzuki never infers what you
meant — it does exactly what you told it to, every time.

## Handles the messy stuff for you

Season folders, release-group tags, missing episodes, oddly numbered
files — Tsuzuki figures out the correct watch order without you having
to rename anything or explain your folder structure. If a couple of
episodes are specials that shouldn't be counted, tell it once with
`tsuzuki exclude` and it remembers.

## Optional: MyAnimeList sync

If you keep a MAL list, Tsuzuki can keep it in sync — in both
directions, always trusting whichever side is further ahead, never
overwriting real progress. Starting an episode moves a planned title to
Watching and records the start date; finishing a series moves it to
Completed and records the finish date:

```
tsuzuki sync login
tsuzuki sync
```

Paused and dropped series are skipped, so pausing can temporarily keep a local
watch or rewatch separate from MAL. Records whose series folders are no longer
available are skipped too. `tsuzuki sync exclude <series|root>` skips a series,
or every series under a root; undo it by deleting its `[[sync_exclusions]]`
entry in `config.toml`.

Entirely optional. Everything above works with no account, no internet
connection, and no setup beyond pointing Tsuzuki at your files.

## Philosophy

- **Local-first.** Your machine is the source of truth. Nothing about
  resuming a show requires the internet.
- **Explicit, never inferred.** Tsuzuki doesn't guess what you meant —
  every folder it tracks, every file it skips, is something you told it.
- **Boring, on purpose.** No AI matching, no fuzzy logic deciding things
  behind your back. Simple, fixed rules you can predict every time.
- **Does one thing.** It doesn't try to be a player, a library, or a
  tracker replacement — it sits quietly between the tools you already
  use and removes one small daily annoyance.

## Installing

```
curl -LsSf https://github.com/mhmdsamerdev/tsuzuki/releases/latest/download/tsuzuki-installer.sh | sh
```

Downloads and installs a prebuilt `tsuzuki` binary — nothing else to set
up. Linux only, for now.

### Building from source instead

```
git clone https://github.com/mhmdsamerdev/tsuzuki
cd tsuzuki
cargo install --path .
```

## Contributing

Want to help build Tsuzuki? [`docs/development.md`](docs/development.md)
covers everything you need to set up the dev environment and get going.

## License

GPLv3 — see [LICENSE](LICENSE).
