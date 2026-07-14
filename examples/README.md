# snapref examples

## `agent-loop.sh` — a complete simulated session

Runs a self-contained three-turn "agent session" in a temp directory: each
turn edits files the way a coding agent would, snaps the result, and the
script then demonstrates `log`, `blame` (including an uncommitted working-tree
line) and `restore` with its automatic backup turn.

```bash
cargo build            # once, from the repo root
bash examples/agent-loop.sh
```

The script uses `SNAPREF_TIME` so its output — including snapshot ids — is
identical on every machine. It cleans up after itself.

## Wiring snapref into a real agent

The integration contract is one command per turn boundary:

```bash
snapref snap --label "<what the agent said it did>" --agent "<agent name>"
```

Where to put it:

- **End-of-turn / stop hooks.** Most coding agents (Claude Code, aider-style
  loops, custom harnesses) expose a hook or callback that fires when a turn
  finishes. Call `snapref snap` there, passing the turn's summary as
  `--label`. Export `SNAPREF_AGENT` once instead of repeating `--agent`.
- **Wrapper loops.** If you drive the agent yourself (one prompt per
  iteration), snap right after each iteration returns — see the loop in
  `agent-loop.sh`.
- **Aligning turn numbers.** If your transcript numbers turns itself, pass
  `--turn N` to keep snapref's numbering identical to the conversation's.
  Gaps are legal; going backwards is not.

Afterwards, the session is queryable:

```bash
snapref log                      # what happened, turn by turn
snapref blame src/main.rs        # which turn wrote each line
snapref diff 4 9 --path src/     # what turns 5..9 did to src/
snapref restore 4                # rewind (your dirty tree is backed up first)
```

Add `.snapref/` to the project's `.gitignore`, and put scratch patterns the
agent tends to produce (`*.log`, `tmp/`) into `.snaprefignore`.
