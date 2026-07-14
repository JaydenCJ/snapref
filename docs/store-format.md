# The snapref store format (version 1)

This document specifies everything on disk under `.snapref/`. The format is
an interface: tools may read it directly, and any change to it must bump the
`format` marker and be described here.

## Layout

```text
.snapref/
├── format            # the single ASCII line "1"
├── objects/
│   └── xx/yyyy…      # content-addressed objects, fan-out on the first 2 hex chars
└── refs/
    └── turns/
        ├── 1         # one file per turn: 40 hex chars + newline
        ├── 2
        └── …
```

There is no index, no lockfile and no daemon. Writers create objects via a
temp file in the fan-out directory followed by `rename(2)`, so a crashed
`snap` can leave at most an orphaned temp file — never a truncated object
under a content-addressed name. Turn refs are append-only: snapref refuses
to overwrite an existing ref.

## Object encoding

Every object is stored as:

```text
<kind> <payload-length>\0<payload>
```

where `<kind>` is `blob`, `tree` or `snapshot`, the length is decimal ASCII,
and the object id is the SHA-1 of those bytes, written in lowercase hex. The
file lives at `objects/<id[0..2]>/<id[2..]>`.

### Blobs — git-compatible by construction

A blob's payload is the file's raw bytes, so the id is the SHA-1 of
`blob <len>\0<bytes>` — exactly how git computes blob ids. Consequence:

```bash
$ printf 'hello world\n' | git hash-object --stdin
3b18e512dba79e4c8300dd08aeb37f8e728b8dad     # the same id snapref stores
```

Any file in any snapshot can be verified against git without snapref's help.
SHA-1 is used here the way git uses it: content addressing inside a local,
trusted store — not a security boundary against adversarial collisions.

### Trees

A tree's payload is UTF-8 text, one entry per line, entries sorted by name:

```text
<mode> <id> <name>\n
```

`mode` is `100644` (file), `100755` (executable file) or `040000`
(directory, whose `id` names another tree). Names are single path
components; they may contain spaces but not `/`, newlines, or non-UTF-8
bytes (the walker rejects such names at snap time). Unlike git's binary
tree encoding, this is deliberately `cat`-readable; tree and snapshot ids
are therefore snapref-specific even though blob ids match git's.

### Snapshots

A snapshot's payload is seven fixed-order text lines:

```text
tree <40-hex tree id>
parent <40-hex snapshot id, or "-" for the first snapshot>
turn <decimal turn number>
time <unix epoch seconds, decimal>
stats <files-added> <files-modified> <files-deleted> <lines-added> <lines-removed>
agent <free text, no newlines>
label <free text, no newlines>
```

`stats` is computed against the parent at snap time (line counts skip
binary files), so `snapref log` never has to re-diff history. Newlines in
labels/agents are flattened to spaces before encoding.

## Turn refs

`refs/turns/<n>` contains the snapshot id for turn `n` plus a newline. Turn
numbers are decimal, strictly increasing, and may have gaps (`snap --turn 7`
after turn 3 is legal — useful when aligning with an external conversation
transcript). The latest turn is simply the largest filename.

## Invariants checked by `snapref verify`

1. Every object file's content rehashes to its own filename.
2. Every object parses: known kind, declared length matches the payload.
3. Every turn ref resolves to an existing `snapshot` object.
4. Every snapshot's `tree` exists, and — recursively — every tree entry
   points at an existing object of the right kind.
5. Every non-`-` `parent` points at an existing snapshot object.

A violation of any invariant is reported individually and fails the command
with exit code 1.
