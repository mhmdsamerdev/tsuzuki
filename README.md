# Tsuzuki

*続き — "continuation."*

Resume what you were watching in [mpv](https://mpv.io) with one command.
Linux only.

You watch an episode, quit, and forget about it. Tomorrow, you type:

```
tsuzuki
```

The right file opens, at the right timestamp. No searching folders, no
remembering episode numbers.

https://github.com/user-attachments/assets/7a0397e2-bf20-4575-9a69-845e8710100d

## Quick start

**1. Install**

```
curl -LsSf https://github.com/mhmdsamerdev/tsuzuki/releases/latest/download/tsuzuki-installer.sh | sh
```

**2. One-time setup.** Tsuzuki learns what you watched from a small mpv
script. This installs it:

```
tsuzuki doctor --fix
```

**3. Point it at your anime folder**

```
tsuzuki add ~/Anime
```

Now watch in mpv the way you always have, and quit whenever you like. Next
time, run `tsuzuki`.

## How it works

Your folders are your library. Tsuzuki never renames or reorganizes
anything. It works out the watch order from your file names (season
folders, release tags and odd numbering are fine), and the mpv script
records where you stopped. One series in progress resumes immediately.
Several? Pick one with the arrow keys.

## Commands

| | |
|---|---|
| `tsuzuki` | Resume what you were watching. |
| `tsuzuki start` | Nothing in progress? See what's available and begin. |
| `tsuzuki status` | What's currently in progress. |
| `tsuzuki list` | Everything you're tracking. |
| `tsuzuki pause <series>` | Set something aside without losing your place. |
| `tsuzuki drop <series>` | Done with it, or starting fresh. |
| `tsuzuki add <path>` | Start tracking a folder. |
| `tsuzuki doctor` | Check that everything is set up right. |

Also: `tsuzuki exclude <path>` keeps specials and OVAs out of the episode
order, and `tsuzuki config` shows your settings.

## Optional: MyAnimeList sync

Keep your MAL list up to date. Tsuzuki only ever moves your progress
forward, never back.

```
tsuzuki sync login
tsuzuki sync
```

Everything else works with no account and no internet connection.

## Optional: ani-cli

Watch with [ani-cli](https://github.com/pystardust/ani-cli)? Turn it on
once:

```
tsuzuki add ani-cli
```

Keep watching through ani-cli in mpv, and `tsuzuki` picks those shows up
too.

## What it is, and isn't

- **Local-first.** Resuming never needs the internet.
- **Explicit.** Tsuzuki doesn't guess. Every folder it tracks and every
  file it skips is something you told it.
- **One job.** It's not a player, a library manager, or a MyAnimeList
  replacement. It answers one question: what was I watching, and can you
  take me back there?

## Contributing

Want to help? See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

GPLv3. See [LICENSE](LICENSE).
