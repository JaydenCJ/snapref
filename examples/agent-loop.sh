#!/usr/bin/env bash
# A complete, self-contained snapref session: simulates three agent turns
# against a scratch project, then walks the history with log/blame/restore.
# Deterministic output via SNAPREF_TIME; leaves nothing behind.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [ -x "$ROOT/target/debug/snapref" ]; then
  BIN="$ROOT/target/debug/snapref"
elif command -v snapref >/dev/null 2>&1; then
  BIN="$(command -v snapref)"
else
  echo "build snapref first: cargo build" >&2
  exit 1
fi

PROJ=$(mktemp -d "${TMPDIR:-/tmp}/snapref-example.XXXXXX")
trap 'rm -rf "$PROJ"' EXIT
cd "$PROJ"

export SNAPREF_AGENT=demo-agent
T0=1783850400 # 2026-07-12T10:00:00Z, so ids match on every machine

# One "agent turn": apply the edits the agent made, then snap them.
turn() {
  local offset=$1 label=$2
  SNAPREF_TIME=$((T0 + offset)) "$BIN" snap --label "$label"
}

echo "\$ snapref init"
SNAPREF_TIME=$T0 "$BIN" init
echo

# --- turn 1: the agent scaffolds a parser -----------------------------------
mkdir -p src
cat > src/parser.rs <<'EOF'
fn parse(input: &str) -> Ast {
    todo!()
}
EOF
printf '# todo-cli\n' > README.md
turn 0 "scaffold the parser"

# --- turn 2: the agent adds a lexer and calls it ------------------------------
cat > src/parser.rs <<'EOF'
fn parse(input: &str) -> Ast {
    let toks = lex(input);
    todo!()
}
EOF
cat > src/lexer.rs <<'EOF'
pub fn lex(s: &str) -> Vec<Tok> {
    s.split_whitespace().map(Tok::from).collect()
}
EOF
turn 73 "add the lexer"

# --- turn 3: the agent finishes parse() ---------------------------------------
cat > src/parser.rs <<'EOF'
fn parse(input: &str) -> Ast {
    let toks = lex(input);
    Ast::from_tokens(toks)
}
EOF
turn 148 "build the AST"

echo
echo "\$ snapref log"
"$BIN" log

# A stray edit nobody has snapped yet — blame calls it out as 'wt'.
cat > src/parser.rs <<'EOF'
fn parse(input: &str) -> Ast {
    let toks = lex(input);
    dbg!(&toks);
    Ast::from_tokens(toks)
}
EOF

echo
echo "\$ snapref blame src/parser.rs"
"$BIN" blame src/parser.rs

echo
echo "\$ snapref restore 1"
SNAPREF_TIME=$((T0 + 300)) "$BIN" restore 1

echo
echo "\$ snapref log   # the dirty tree survived as the auto-backup turn 4"
"$BIN" log
