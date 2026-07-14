# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-07-12

### Added

- Shadow store under `.snapref/`: content-addressed `blob`/`tree`/`snapshot` objects with a dependency-free SHA-1, fan-out layout, temp-file-then-rename writes, and append-only per-turn refs. Blob ids are byte-identical to `git hash-object` output.
- `snapref snap`: one snapshot per agent turn with label/agent metadata, explicit `--turn` pinning, content deduplication, per-turn change statistics (`+A ~M -D` files, `+x -y` lines), and no-op turns recorded on purpose so turn numbers stay aligned with the conversation.
- `snapref blame`: line → turn attribution over the full file history via an incremental Myers-diff fold; uncommitted lines are reported as `wt (not snapped)`; `--at TURN` blames past versions; binary files are refused instead of garbled.
- `snapref diff`: unified diff between any two turns or against the working tree (`wt`), with `/dev/null` markers for added/deleted files, `\ No newline at end of file` handling, binary detection, mode-change reporting and `--path` scoping.
- `snapref restore`: whole-tree rewind that rewrites, deletes and resurrects files, prunes emptied directories, and snaps a dirty tree as an automatic backup turn first; `--path` restores files or directory prefixes without deleting; discarding work requires `--no-backup --force`.
- `snapref log`, `status`, `show TURN`, `show TURN:PATH`: queryable history, content-hash working-tree comparison, snapshot metadata with parent links, and exact byte extraction of any file at any turn.
- `snapref verify`: full store audit — every object rehashed, every ref/tree/parent edge walked, each problem named individually.
- Deterministic walker with sorted traversal, `.git`/`.snapref` always skipped, heavy build directories excluded by default, symlinks never followed, executable bits preserved, and gitignore-flavored `.snaprefignore` patterns.
- Machine-readable `--json` output on snap/log/status/blame/diff/show with a stable `tool`/`version` envelope; `SNAPREF_TIME` and `SNAPREF_AGENT` environment hooks for reproducible, wrapper-driven use; broken-pipe-safe stdout.
- Test suite: 77 unit tests, 13 CLI integration tests, and `scripts/smoke.sh`.

[0.1.0]: https://github.com/JaydenCJ/snapref/releases/tag/v0.1.0
