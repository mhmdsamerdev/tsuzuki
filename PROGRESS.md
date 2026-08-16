# PROGRESS.md — Tsuzuki build status

Status file, not a spec. Decisions and reasoning live in `CLAUDE.md` and
`docs/design.md` — this just tracks where the build currently stands.

**Current phase:** Phase 0 — Scaffolding & data model (done)

**Done:**
- Cargo binary crate scaffolded at repo root (edition 2024), deps: clap
  (derive), serde (derive), toml, dirs, tempfile
- Full clap CLI skeleton (`src/cli.rs`) — every subcommand from
  design.md §9 stubbed via `src/main.rs`, `--help` verified for all
- `config.toml` read/write (`src/config.rs`) — XDG path via
  `dirs::config_dir()`, writes defaults on first read, schema matches
  design.md §8 exactly
- `state.toml` read/write (`src/state.rs`) — XDG path via
  `dirs::data_dir()`, atomic temp-file-then-rename write, empty state on
  missing file
- Unit tests for both (8 total, all passing), including an inode-based
  test proving the write path is rename-based, not in-place

**Next up:**
- Phase 1 — Tracked roots & episode ordering (design.md §11)

**Open questions blocking further work:**
- (none)
