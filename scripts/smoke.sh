#!/usr/bin/env bash
# Smoke test: builds snapref, drives a realistic three-turn agent session
# end to end (init, snap, log, status, blame, diff, show, restore with
# auto-backup, verify + corruption detection, .snaprefignore), asserting on
# real output. Self-contained: temp dirs only, no network, idempotent.
# Prints "SMOKE OK" on success.
set -euo pipefail

cd "$(dirname "$0")/.."

fail() { echo "SMOKE FAIL: $*" >&2; exit 1; }

echo "[smoke] building..."
cargo build --quiet
BIN="$PWD/target/debug/snapref"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/snapref-smoke.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
PROJ="$WORK/proj"
mkdir -p "$PROJ"

# Reproducible timestamps: 2026-07-12T10:00:00Z + one minute per turn.
T0=1783850400
snapref() { local t=$1; shift; (cd "$PROJ" && SNAPREF_TIME=$t "$BIN" "$@"); }

# --- 1. version/help sanity ---------------------------------------------------
"$BIN" --version | grep -q '^snapref 0\.1\.0$' || fail "--version mismatch"
"$BIN" --help | grep -q 'COMMANDS:' || fail "--help missing COMMANDS section"
echo "[smoke] version/help OK"

# --- 2. init + three agent turns ----------------------------------------------
snapref $T0 init | grep -q 'initialized empty snapref store' || fail "init did not create a store"
snapref $T0 init | grep -q 'already initialized' || fail "re-init is not idempotent"

mkdir -p "$PROJ/src"
printf 'fn parse() {\n    todo!()\n}\n' > "$PROJ/src/parser.rs"
printf '# demo\n' > "$PROJ/README.md"
printf 'obsolete\n' > "$PROJ/old.txt"
snapref $T0 snap --label "scaffold parser" --agent demo-agent \
  | grep -q '^turn 1 snapped' || fail "turn 1 not snapped"

printf 'fn parse() {\n    lex();\n    todo!()\n}\n' > "$PROJ/src/parser.rs"
printf 'fn lex() {}\n' > "$PROJ/src/lexer.rs"
snapref $((T0+60)) snap --label "add lexer" --agent demo-agent \
  | grep -q '+1 ~1 -0 files' || fail "turn 2 stats wrong"

printf 'fn parse() {\n    lex();\n    build_ast()\n}\n' > "$PROJ/src/parser.rs"
rm "$PROJ/old.txt"
snapref $((T0+120)) snap --label "build the AST" --agent demo-agent \
  | grep -q -- '-1 files' || fail "turn 3 deletion not counted"
echo "[smoke] three turns snapped"

# --- 3. log + status -----------------------------------------------------------
snapref $T0 log > "$WORK/log.out"
[ "$(wc -l < "$WORK/log.out")" -eq 3 ] || fail "log should list 3 turns"
head -n 1 "$WORK/log.out" | grep -q '^turn 3' || fail "log not newest-first"
grep -q 'demo-agent: build the AST' "$WORK/log.out" || fail "log missing agent/label"
snapref $T0 status | grep -q 'working tree matches turn 3' || fail "status not clean after snap"
echo "[smoke] log + status OK"

# --- 4. blame: every line names its turn; uncommitted lines say wt -------------
printf 'fn parse() {\n    lex();\n    build_ast()\n}\n// wip\n' > "$PROJ/src/parser.rs"
snapref $T0 blame src/parser.rs > "$WORK/blame.out"
sed -n '1p' "$WORK/blame.out" | grep -q '^turn 1' || fail "blame line 1 should be turn 1"
sed -n '2p' "$WORK/blame.out" | grep -q '^turn 2' || fail "blame line 2 should be turn 2"
sed -n '3p' "$WORK/blame.out" | grep -q '^turn 3' || fail "blame line 3 should be turn 3"
sed -n '5p' "$WORK/blame.out" | grep -q '^wt' || fail "blame line 5 should be wt"
grep -q 'add lexer' "$WORK/blame.out" || fail "blame missing turn label"
echo "[smoke] blame attributes lines to turns"

# --- 5. diff between turns and against the working tree ------------------------
snapref $T0 diff 1 3 > "$WORK/diff.out"
grep -q -- '--- turn 1:src/parser.rs' "$WORK/diff.out" || fail "diff missing from-header"
grep -q -- '-    todo!()' "$WORK/diff.out" || fail "diff missing deletion"
grep -q -- '+    build_ast()' "$WORK/diff.out" || fail "diff missing insertion"
grep -q -- '+++ /dev/null' "$WORK/diff.out" || fail "diff missing deleted-file marker"
snapref $T0 diff | grep -q -- '+// wip' || fail "default diff (latest vs wt) missing wt edit"
echo "[smoke] diff between turns and worktree OK"

# --- 6. show: metadata with git-compatible blob ids + exact bytes ---------------
printf 'hello world\n' > "$PROJ/hello.txt"
snapref $((T0+180)) snap --label "hello blob" >/dev/null
snapref $T0 show 4 > "$WORK/show.out"
# `git hash-object` of "hello world\n" — snapref blob ids match git's.
grep -q '100644 3b18e512  hello.txt' "$WORK/show.out" || fail "blob id not git-compatible"
snapref $T0 show 2:src/parser.rs > "$WORK/v2.out"
printf 'fn parse() {\n    lex();\n    todo!()\n}\n' | cmp -s - "$WORK/v2.out" \
  || fail "show TURN:PATH bytes differ from what was snapped"
echo "[smoke] show + git-compatible blob ids OK"

# --- 7. restore: auto-backup, rewind, then rewind the rewind --------------------
printf 'fn parse() { /* uncommitted */ }\n' > "$PROJ/src/parser.rs"
snapref $((T0+240)) restore 1 > "$WORK/restore.out"
grep -q 'working tree backed up as turn 5' "$WORK/restore.out" || fail "no auto-backup turn"
grep -q 'restored working tree to turn 1' "$WORK/restore.out" || fail "restore summary missing"
cmp -s <(printf 'fn parse() {\n    todo!()\n}\n') "$PROJ/src/parser.rs" || fail "parser not at v1"
[ ! -e "$PROJ/src/lexer.rs" ] || fail "restore left a file turn 1 does not have"
[ -e "$PROJ/old.txt" ] || fail "restore did not resurrect a deleted file"
snapref $T0 show 5:src/parser.rs | grep -q 'uncommitted' || fail "backup turn lost the dirty edit"
# Rewinding back is itself guarded: the turn-1 state gets backed up as turn 6.
snapref $((T0+300)) restore 5 | grep -q 'backed up as turn 6' || fail "second restore skipped its backup"
grep -q 'uncommitted' "$PROJ/src/parser.rs" || fail "restoring the backup turn failed"
echo "[smoke] restore + auto-backup round trip OK"

# --- 8. verify: healthy store passes, a flipped byte is caught ------------------
snapref $T0 verify | grep -q '^verify OK:' || fail "verify failed on a healthy store"
VICTIM=$(find "$PROJ/.snapref/objects" -type f | sort | head -n 1)
cp "$VICTIM" "$WORK/victim.bak"
printf 'X' >> "$VICTIM"
if snapref $T0 verify >/dev/null 2>"$WORK/verify.err"; then
  fail "verify accepted a corrupt object"
fi
grep -q 'problem:' "$WORK/verify.err" || fail "verify did not name the problem"
cp "$WORK/victim.bak" "$VICTIM"
snapref $T0 verify >/dev/null || fail "verify still failing after repair"
echo "[smoke] verify catches corruption"

# --- 9. .snaprefignore prunes noise ---------------------------------------------
printf '*.log\nscratch/\n' > "$PROJ/.snaprefignore"
printf 'noisy\n' > "$PROJ/build.log"
mkdir -p "$PROJ/scratch"
printf 'tmp\n' > "$PROJ/scratch/tmp.txt"
TURN=$(snapref $((T0+360)) snap --label "with ignores" | sed -n 's/^turn \([0-9]*\) snapped.*/\1/p')
[ -n "$TURN" ] || fail "could not parse the snapped turn number"
snapref $T0 show "$TURN" > "$WORK/show-ignored.out"
grep -q 'build.log' "$WORK/show-ignored.out" && fail "ignored *.log leaked into the snapshot"
grep -q 'scratch' "$WORK/show-ignored.out" && fail "ignored directory leaked into the snapshot"
grep -q '.snaprefignore' "$WORK/show-ignored.out" || fail "the ignore file itself should be tracked"
echo "[smoke] .snaprefignore respected"

echo "SMOKE OK"
