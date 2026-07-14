# Contributing to snapref

Thanks for your interest in improving snapref. Issues, discussions and pull requests are all welcome.

## Getting started

Prerequisites: Rust 1.75 or newer (stable toolchain). No other dependencies — the crate is std-only.

```bash
git clone https://github.com/JaydenCJ/snapref.git
cd snapref
cargo build
cargo test
bash scripts/smoke.sh
```

`scripts/smoke.sh` builds the binary and drives a full three-turn agent session end to end (init, snap, log, status, blame, diff, show, restore with auto-backup, verify with corruption detection, `.snaprefignore`). It finishes in well under a minute and must print `SMOKE OK`.

## Before you open a pull request

1. `cargo fmt` — formatting is enforced.
2. `cargo clippy --all-targets -- -D warnings` — clippy must be clean.
3. `cargo test` — the 77 unit tests and 13 CLI integration tests must pass.
4. `bash scripts/smoke.sh` — the smoke test must print `SMOKE OK`.
5. Add tests for behavior changes. Hashing, diffing, blame attribution, glob matching and the object model live in pure modules (`sha1`, `diff`, `blame`, `glob`, `object`) that are easy to unit-test; please keep it that way.

## Ground rules

- Keep dependencies at zero. snapref is deliberately std-only; adding a crate needs a very strong justification in the PR description.
- No network calls, ever. Snapshots, blame and restore are offline and deterministic; `SNAPREF_TIME` must keep producing byte-identical stores for identical inputs.
- The store format is an interface: anything that changes how objects are encoded or ids are computed must bump the `format` marker, update `docs/store-format.md` in the same PR, and be called out in the changelog. Blob ids staying git-compatible is a documented guarantee.
- Restore must stay non-destructive by default: no code path may overwrite or delete unsnapped work without an automatic backup turn or an explicit `--no-backup --force`.
- Output lines are an interface too: scripts and hooks parse them. Treat format changes as breaking.
- Code comments and doc comments are written in English.

## Reporting bugs

Please include the `snapref --version` output, the exact command line, and a minimal reproduction (a short shell script that builds a working tree and snaps a few turns is ideal — see `scripts/smoke.sh` for the pattern). For store issues, attach the output of `snapref verify`; for blame/diff issues, the two file versions involved.

## Security

If you find a security issue (e.g. anything involving path handling during restore or object parsing), please do not open a public issue. Use GitHub's private vulnerability reporting on this repository instead.
