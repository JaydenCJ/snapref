//! Command-line interface: argument parsing, dispatch, and all output
//! formatting (both the human tables and the machine-readable JSON).
//!
//! Exit codes: `0` success, `1` operational failure (unknown turn, dirty
//! restore refused, corrupt store, …), `2` usage error.

use crate::blame::{self, Origin};
use crate::object::Snapshot;
use crate::store::{self, Store};
use crate::{diff, restore, snapshot, timefmt, VERSION};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

/// Write to stdout, treating a closed pipe as success: `snapref log | head`
/// and `… | grep -q` must not panic or report an error when the consumer
/// stops reading early.
fn stdout_write(s: &str) {
    let mut so = std::io::stdout().lock();
    if so.write_all(s.as_bytes()).is_err() || so.flush().is_err() {
        std::process::exit(0);
    }
}

/// `println!` for command output, tolerant of broken pipes.
macro_rules! out {
    () => {
        stdout_write("\n")
    };
    ($($arg:tt)*) => {
        stdout_write(&format!("{}\n", format_args!($($arg)*)))
    };
}

struct Fail {
    code: i32,
    msg: String,
}

type R<T> = Result<T, Fail>;

impl From<String> for Fail {
    fn from(msg: String) -> Fail {
        Fail { code: 1, msg }
    }
}

fn usage<T>(msg: impl Into<String>) -> R<T> {
    Err(Fail {
        code: 2,
        msg: msg.into(),
    })
}

fn op<T>(msg: impl Into<String>) -> R<T> {
    Err(Fail {
        code: 1,
        msg: msg.into(),
    })
}

fn op_err(msg: String) -> Fail {
    Fail { code: 1, msg }
}

/// Run the CLI; returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    match dispatch(args) {
        Ok(code) => code,
        Err(f) => {
            eprintln!("snapref: {}", f.msg);
            f.code
        }
    }
}

fn dispatch(args: &[String]) -> R<i32> {
    let mut i = 0;
    let mut cdir: Option<String> = None;
    while i < args.len() {
        match args[i].as_str() {
            "-C" => {
                i += 1;
                let Some(dir) = args.get(i) else {
                    return usage("-C requires a directory argument");
                };
                cdir = Some(dir.clone());
                i += 1;
            }
            "-V" | "--version" => {
                out!("snapref {VERSION}");
                return Ok(0);
            }
            "-h" | "--help" | "help" => {
                stdout_write(&main_help());
                return Ok(0);
            }
            _ => break,
        }
    }
    let Some(cmd) = args.get(i) else {
        eprint!("{}", main_help());
        return Ok(2);
    };
    let rest = &args[i + 1..];
    let base = base_dir(cdir.as_deref())?;
    match cmd.as_str() {
        "init" => cmd_init(&base, rest),
        "snap" => cmd_snap(&base, rest),
        "log" => cmd_log(&base, rest),
        "status" => cmd_status(&base, rest),
        "blame" => cmd_blame(&base, rest),
        "diff" => cmd_diff(&base, rest),
        "show" => cmd_show(&base, rest),
        "restore" => cmd_restore(&base, rest),
        "verify" => cmd_verify(&base, rest),
        other => usage(format!("unknown command '{other}' (see 'snapref --help')")),
    }
}

fn main_help() -> String {
    format!(
        "snapref {VERSION}\n\
         Shadow snapshots of your working tree per agent turn: blame lines to turns, restore any state.\n\
         \n\
         USAGE:\n\
         \x20   snapref [-C DIR] <COMMAND> [OPTIONS]\n\
         \n\
         COMMANDS:\n\
         \x20   init                    Create the shadow store (.snapref/) for this working tree\n\
         \x20   snap                    Record the current working tree as the next turn\n\
         \x20   log                     List recorded turns, newest first\n\
         \x20   status                  Compare the working tree against the latest turn\n\
         \x20   blame <PATH>            Attribute each line of a file to the turn that wrote it\n\
         \x20   diff [FROM] [TO]        Unified diff between two turns (or the working tree, 'wt')\n\
         \x20   show <TURN[:PATH]>      Show snapshot metadata, or a file's exact bytes at a turn\n\
         \x20   restore <TURN>          Restore the working tree (or --path files) to a turn\n\
         \x20   verify                  Check object-store integrity (rehash everything)\n\
         \n\
         OPTIONS:\n\
         \x20   -C <DIR>                Run as if snapref was started in DIR\n\
         \x20   -h, --help              Print this help\n\
         \x20   -V, --version           Print version\n\
         \n\
         ENVIRONMENT:\n\
         \x20   SNAPREF_AGENT           Default value for 'snap --agent'\n\
         \x20   SNAPREF_TIME            Fixed unix timestamp, for reproducible snapshots\n\
         \n\
         Run 'snapref <COMMAND> --help' for command options.\n"
    )
}

fn base_dir(cdir: Option<&str>) -> R<PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|e| op_err(format!("cannot determine the current directory: {e}")))?;
    let base = match cdir {
        Some(dir) => cwd.join(dir),
        None => cwd,
    };
    std::fs::canonicalize(&base)
        .map_err(|e| op_err(format!("cannot open directory {}: {e}", base.display())))
}

/// Resolve a user-supplied path to a `/`-separated repo-relative path.
fn rel_path(store: &Store, base: &Path, raw: &str) -> R<String> {
    let p = Path::new(raw);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    let norm = normalize(&abs);
    let rel = norm
        .strip_prefix(&store.work)
        .map_err(|_| op_err(format!("path is outside the working tree: {raw}")))?;
    let s = rel
        .to_str()
        .ok_or_else(|| op_err(format!("path is not valid UTF-8: {raw}")))?;
    if s.is_empty() {
        return op("path resolves to the working-tree root; name a file or subdirectory");
    }
    Ok(s.replace(std::path::MAIN_SEPARATOR, "/"))
}

/// Lexically normalize a path (resolve `.` and `..` without touching the fs).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

fn describe(snap: &Snapshot) -> String {
    match (snap.agent.is_empty(), snap.label.is_empty()) {
        (true, true) => "(no label)".to_string(),
        (true, false) => snap.label.clone(),
        (false, true) => format!("{}: (no label)", snap.agent),
        (false, false) => format!("{}: {}", snap.agent, snap.label),
    }
}

fn agent_from_env() -> String {
    std::env::var("SNAPREF_AGENT").unwrap_or_default()
}

fn check_help(rest: &[String], help: &str) -> Option<i32> {
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        stdout_write(help);
        return Some(0);
    }
    None
}

// ---------------------------------------------------------------- init ----

fn cmd_init(base: &Path, rest: &[String]) -> R<i32> {
    if let Some(code) = check_help(
        rest,
        "USAGE:\n    snapref init\n\nCreates .snapref/ in the current directory (idempotent).\n",
    ) {
        return Ok(code);
    }
    if let Some(extra) = rest.first() {
        return usage(format!("init takes no arguments (got '{extra}')"));
    }
    let (store, created) = Store::init(base)?;
    if created {
        out!("initialized empty snapref store in {}", store.dir.display());
    } else {
        out!(
            "snapref store already initialized in {}",
            store.dir.display()
        );
    }
    Ok(0)
}

// ---------------------------------------------------------------- snap ----

fn cmd_snap(base: &Path, rest: &[String]) -> R<i32> {
    if let Some(code) = check_help(
        rest,
        "USAGE:\n    snapref snap [--label TEXT] [--agent NAME] [--turn N] [--json]\n\n\
         Records the working tree as the next turn. --turn pins an explicit\n\
         turn number (must be greater than the latest); --agent defaults to\n\
         the SNAPREF_AGENT environment variable.\n",
    ) {
        return Ok(code);
    }
    let mut label = String::new();
    let mut agent: Option<String> = None;
    let mut turn: Option<u64> = None;
    let mut json = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--label" => label = take_value(&mut it, "--label")?,
            "--agent" => agent = Some(take_value(&mut it, "--agent")?),
            "--turn" => turn = Some(take_number(&mut it, "--turn")?),
            "--json" => json = true,
            other => return usage(format!("unknown snap option '{other}'")),
        }
    }
    let store = Store::discover(base)?;
    let agent = agent.unwrap_or_else(agent_from_env);
    let previous = store.latest()?;
    let snap = snapshot::take(&store, &label, &agent, turn, timefmt::now())?;
    if json {
        out!(
            "{{\"tool\":\"snapref\",\"version\":{},\"turn\":{},\"id\":{},\"parent_turn\":{},\
             \"files_added\":{},\"files_modified\":{},\"files_deleted\":{},\
             \"lines_added\":{},\"lines_removed\":{},\"agent\":{},\"label\":{},\"time\":{}}}",
            json_str(VERSION),
            snap.turn,
            json_str(&snap.id),
            previous.map_or("null".to_string(), |n| n.to_string()),
            snap.stats.files_added,
            snap.stats.files_modified,
            snap.stats.files_deleted,
            snap.stats.lines_added,
            snap.stats.lines_removed,
            json_str(&snap.agent),
            json_str(&snap.label),
            json_str(&timefmt::utc(snap.time)),
        );
    } else if snap.stats.is_empty() && previous.is_some() {
        out!(
            "turn {} snapped {}: no changes since turn {}",
            snap.turn,
            short(&snap.id),
            previous.unwrap()
        );
    } else {
        out!(
            "turn {} snapped {}: +{} ~{} -{} files, +{} -{} lines",
            snap.turn,
            short(&snap.id),
            snap.stats.files_added,
            snap.stats.files_modified,
            snap.stats.files_deleted,
            snap.stats.lines_added,
            snap.stats.lines_removed
        );
    }
    Ok(0)
}

// ----------------------------------------------------------------- log ----

fn cmd_log(base: &Path, rest: &[String]) -> R<i32> {
    if let Some(code) = check_help(
        rest,
        "USAGE:\n    snapref log [--limit N] [--json]\n\nLists recorded turns, newest first.\n",
    ) {
        return Ok(code);
    }
    let mut limit: Option<usize> = None;
    let mut json = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--limit" => limit = Some(take_number(&mut it, "--limit")? as usize),
            "--json" => json = true,
            other => return usage(format!("unknown log option '{other}'")),
        }
    }
    let store = Store::discover(base)?;
    let mut turns = store.turns()?;
    turns.reverse();
    if let Some(n) = limit {
        turns.truncate(n);
    }
    let snaps: Vec<Snapshot> = turns
        .iter()
        .map(|&n| store.snapshot(n))
        .collect::<Result<_, _>>()?;

    if json {
        let rows: Vec<String> = snaps
            .iter()
            .map(|s| {
                format!(
                    "{{\"turn\":{},\"id\":{},\"time\":{},\"files_added\":{},\
                     \"files_modified\":{},\"files_deleted\":{},\"lines_added\":{},\
                     \"lines_removed\":{},\"agent\":{},\"label\":{}}}",
                    s.turn,
                    json_str(&s.id),
                    json_str(&timefmt::utc(s.time)),
                    s.stats.files_added,
                    s.stats.files_modified,
                    s.stats.files_deleted,
                    s.stats.lines_added,
                    s.stats.lines_removed,
                    json_str(&s.agent),
                    json_str(&s.label),
                )
            })
            .collect();
        out!(
            "{{\"tool\":\"snapref\",\"version\":{},\"turns\":[{}]}}",
            json_str(VERSION),
            rows.join(",")
        );
        return Ok(0);
    }
    if snaps.is_empty() {
        out!("no turns recorded yet (run 'snapref snap')");
        return Ok(0);
    }
    let turn_width = snaps
        .iter()
        .map(|s| s.turn.to_string().len())
        .max()
        .unwrap_or(1);
    for s in &snaps {
        out!(
            "turn {:>tw$}  {}  {}  +{} ~{} -{}  {}",
            s.turn,
            short(&s.id),
            timefmt::utc(s.time),
            s.stats.files_added,
            s.stats.files_modified,
            s.stats.files_deleted,
            describe(s),
            tw = turn_width,
        );
    }
    Ok(0)
}

// -------------------------------------------------------------- status ----

fn cmd_status(base: &Path, rest: &[String]) -> R<i32> {
    if let Some(code) = check_help(
        rest,
        "USAGE:\n    snapref status [--json]\n\nCompares the working tree against the latest turn.\n",
    ) {
        return Ok(code);
    }
    let mut json = false;
    for arg in rest {
        match arg.as_str() {
            "--json" => json = true,
            other => return usage(format!("unknown status option '{other}'")),
        }
    }
    let store = Store::discover(base)?;
    let Some(latest) = store.latest()? else {
        return op("no snapshots yet (run 'snapref snap')");
    };
    let snap = store.snapshot(latest)?;
    let changes = restore::worktree_changes(&store, &snap)?;

    if json {
        let list = |v: &[String]| v.iter().map(|p| json_str(p)).collect::<Vec<_>>().join(",");
        out!(
            "{{\"tool\":\"snapref\",\"version\":{},\"against_turn\":{},\"clean\":{},\
             \"added\":[{}],\"modified\":[{}],\"deleted\":[{}]}}",
            json_str(VERSION),
            latest,
            changes.is_empty(),
            list(&changes.added),
            list(&changes.modified),
            list(&changes.deleted),
        );
        return Ok(0);
    }
    if changes.is_empty() {
        out!("working tree matches turn {} ({})", latest, short(&snap.id));
        return Ok(0);
    }
    out!(
        "comparing working tree against turn {} ({})",
        latest,
        short(&snap.id)
    );
    let mut rows: Vec<(&str, &str)> = Vec::new();
    rows.extend(changes.added.iter().map(|p| (p.as_str(), "added")));
    rows.extend(changes.modified.iter().map(|p| (p.as_str(), "modified")));
    rows.extend(changes.deleted.iter().map(|p| (p.as_str(), "deleted")));
    rows.sort();
    for (path, tag) in rows {
        out!("  {tag:<8}  {path}");
    }
    out!(
        "{} change(s): {} added, {} modified, {} deleted",
        changes.total(),
        changes.added.len(),
        changes.modified.len(),
        changes.deleted.len()
    );
    Ok(0)
}

// --------------------------------------------------------------- blame ----

fn cmd_blame(base: &Path, rest: &[String]) -> R<i32> {
    if let Some(code) = check_help(
        rest,
        "USAGE:\n    snapref blame <PATH> [--at TURN] [--json]\n\n\
         Attributes each line of PATH (working-tree version by default) to\n\
         the turn that wrote it. --at blames the file as of a past turn.\n",
    ) {
        return Ok(code);
    }
    let mut path: Option<String> = None;
    let mut at: Option<u64> = None;
    let mut json = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--at" => at = Some(take_number(&mut it, "--at")?),
            "--json" => json = true,
            other if other.starts_with('-') => {
                return usage(format!("unknown blame option '{other}'"))
            }
            other => {
                if path.is_some() {
                    return usage("blame takes exactly one PATH");
                }
                path = Some(other.to_string());
            }
        }
    }
    let Some(raw_path) = path else {
        return usage("blame requires a PATH (see 'snapref blame --help')");
    };
    let store = Store::discover(base)?;
    let rel = rel_path(&store, base, &raw_path)?;
    if let Some(turn) = at {
        // Ensure the turn exists before scanning history.
        store.snapshot(turn)?;
    }

    let history = blame::file_versions(&store, &rel, at)?;
    let mut versions: Vec<(Origin, String)> = history
        .iter()
        .map(|(turn, text)| (Origin::Turn(*turn), text.clone()))
        .collect();

    if at.is_none() {
        let abs = store.work.join(&rel);
        match std::fs::read(&abs) {
            Ok(bytes) => {
                if diff::is_binary(&bytes) {
                    return op(format!("cannot blame binary file: {rel}"));
                }
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if versions.last().map(|(_, t)| t.as_str()) != Some(text.as_str()) {
                    versions.push((Origin::Working, text));
                }
            }
            Err(_) => {
                if versions.is_empty() {
                    return op(format!(
                        "no history for {rel} (never snapped, not in the working tree)"
                    ));
                }
                return op(format!(
                    "{rel} is not in the working tree; use --at TURN to blame a past version"
                ));
            }
        }
    }
    if versions.is_empty() {
        return op(format!(
            "no history for {rel} before or at turn {}",
            at.unwrap_or(0)
        ));
    }

    let lines = blame::attribute(&versions);
    let mut meta: BTreeMap<u64, Snapshot> = BTreeMap::new();
    for (turn, _) in &history {
        if !meta.contains_key(turn) {
            meta.insert(*turn, store.snapshot(*turn)?);
        }
    }

    if json {
        let rows: Vec<String> = lines
            .iter()
            .map(|l| {
                let (turn, time, label) = match l.origin {
                    Origin::Turn(t) => {
                        let s = &meta[&t];
                        (
                            t.to_string(),
                            json_str(&timefmt::utc(s.time)),
                            json_str(&s.label),
                        )
                    }
                    Origin::Working => ("null".to_string(), "null".to_string(), "null".to_string()),
                };
                format!(
                    "{{\"line\":{},\"turn\":{turn},\"time\":{time},\"label\":{label},\"text\":{}}}",
                    l.line,
                    json_str(&l.text)
                )
            })
            .collect();
        out!(
            "{{\"tool\":\"snapref\",\"version\":{},\"path\":{},\"lines\":[{}]}}",
            json_str(VERSION),
            json_str(&rel),
            rows.join(",")
        );
        return Ok(0);
    }

    const WT_LABEL: &str = "(not snapped)";
    let turn_cell = |o: Origin| match o {
        Origin::Turn(t) => format!("turn {t}"),
        Origin::Working => "wt".to_string(),
    };
    let label_cell = |o: Origin| match o {
        Origin::Turn(t) => truncate(&meta[&t].label, 28),
        Origin::Working => WT_LABEL.to_string(),
    };
    let tw = lines
        .iter()
        .map(|l| turn_cell(l.origin).len())
        .max()
        .unwrap_or(2);
    // Column widths in chars, not bytes: labels may be CJK or other
    // multi-byte text, and `{:<width$}` pads by char count.
    let lw = lines
        .iter()
        .map(|l| label_cell(l.origin).chars().count())
        .max()
        .unwrap_or(1);
    let nw = lines.last().map_or(1, |l| l.line.to_string().len());
    for l in &lines {
        let time = match l.origin {
            Origin::Turn(t) => timefmt::utc(meta[&t].time),
            Origin::Working => " ".repeat(20),
        };
        out!(
            "{:<tw$} | {time} | {:<lw$} | {:>nw$} | {}",
            turn_cell(l.origin),
            label_cell(l.origin),
            l.line,
            l.text,
        );
    }
    Ok(0)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 2).collect();
        format!("{cut}..")
    }
}

// ---------------------------------------------------------------- diff ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Turn(u64),
    Working,
}

fn side_name(side: Side) -> String {
    match side {
        Side::Turn(t) => format!("turn {t}"),
        Side::Working => "wt".to_string(),
    }
}

fn parse_side(spec: &str) -> R<Side> {
    match spec {
        "wt" | "@" | "worktree" => Ok(Side::Working),
        other => match other.parse::<u64>() {
            Ok(n) => Ok(Side::Turn(n)),
            Err(_) => usage(format!(
                "'{other}' is not a turn number or 'wt' (see 'snapref diff --help')"
            )),
        },
    }
}

enum SrcFile {
    Blob { id: String, exec: bool },
    Disk { abs: PathBuf, exec: bool },
}

fn side_files(store: &Store, side: Side) -> R<BTreeMap<String, SrcFile>> {
    let mut out = BTreeMap::new();
    match side {
        Side::Turn(turn) => {
            let snap = store.snapshot(turn)?;
            for entry in snapshot::files_of(store, &snap)? {
                out.insert(
                    entry.rel,
                    SrcFile::Blob {
                        id: entry.id,
                        exec: entry.exec,
                    },
                );
            }
        }
        Side::Working => {
            let patterns = store.ignore_patterns();
            let (files, _) = crate::walker::walk(&store.work, &patterns)?;
            for f in files {
                out.insert(
                    f.rel,
                    SrcFile::Disk {
                        abs: f.abs,
                        exec: f.exec,
                    },
                );
            }
        }
    }
    Ok(out)
}

fn src_bytes(store: &Store, src: &SrcFile) -> R<Vec<u8>> {
    match src {
        SrcFile::Blob { id, .. } => {
            let (_, bytes) = crate::object::read(&store.objects(), id)?;
            Ok(bytes)
        }
        SrcFile::Disk { abs, .. } => {
            std::fs::read(abs).map_err(|e| op_err(format!("cannot read {}: {e}", abs.display())))
        }
    }
}

fn src_exec(src: &SrcFile) -> bool {
    match src {
        SrcFile::Blob { exec, .. } | SrcFile::Disk { exec, .. } => *exec,
    }
}

fn cmd_diff(base: &Path, rest: &[String]) -> R<i32> {
    if let Some(code) = check_help(
        rest,
        "USAGE:\n    snapref diff [FROM] [TO] [--path PATH ...] [--json]\n\n\
         FROM/TO are turn numbers or 'wt' (the working tree). With no\n\
         arguments, diffs the latest turn against the working tree; with one\n\
         argument, diffs that turn against the working tree.\n",
    ) {
        return Ok(code);
    }
    let mut specs: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut json = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--path" => paths.push(take_value(&mut it, "--path")?),
            "--json" => json = true,
            other if other.starts_with("--") => {
                return usage(format!("unknown diff option '{other}'"))
            }
            other => specs.push(other.to_string()),
        }
    }
    if specs.len() > 2 {
        return usage("diff takes at most two revision arguments");
    }
    let store = Store::discover(base)?;
    let (from, to) = match specs.len() {
        0 => {
            let Some(latest) = store.latest()? else {
                return op("no snapshots yet (run 'snapref snap')");
            };
            (Side::Turn(latest), Side::Working)
        }
        1 => (parse_side(&specs[0])?, Side::Working),
        _ => (parse_side(&specs[0])?, parse_side(&specs[1])?),
    };
    let paths: Vec<String> = paths
        .iter()
        .map(|p| rel_path(&store, base, p))
        .collect::<R<_>>()?;

    let from_files = side_files(&store, from)?;
    let to_files = side_files(&store, to)?;
    let mut rels: Vec<&String> = from_files.keys().chain(to_files.keys()).collect();
    rels.sort();
    rels.dedup();

    let from_name = side_name(from);
    let to_name = side_name(to);
    let mut body = String::new();
    let mut summaries: Vec<String> = Vec::new();
    for rel in rels {
        if !paths.is_empty()
            && !paths
                .iter()
                .any(|p| rel == p || rel.starts_with(&format!("{p}/")))
        {
            continue;
        }
        let a = from_files.get(rel);
        let b = to_files.get(rel);
        let a_bytes = a.map(|s| src_bytes(&store, s)).transpose()?;
        let b_bytes = b.map(|s| src_bytes(&store, s)).transpose()?;
        if a_bytes == b_bytes {
            // Same content: only worth mentioning if the mode flipped.
            if let (Some(a), Some(b)) = (a, b) {
                if src_exec(a) != src_exec(b) {
                    let (old, new) = if src_exec(a) {
                        ("100755", "100644")
                    } else {
                        ("100644", "100755")
                    };
                    body.push_str(&format!("mode changed: {rel} ({old} -> {new})\n"));
                    summaries.push(diff_summary(rel, "mode-changed", 0, 0, false));
                }
            }
            continue;
        }
        let status = match (&a_bytes, &b_bytes) {
            (None, Some(_)) => "added",
            (Some(_), None) => "deleted",
            _ => "modified",
        };
        let binary = a_bytes.as_deref().is_some_and(diff::is_binary)
            || b_bytes.as_deref().is_some_and(diff::is_binary);
        if binary {
            body.push_str(&format!(
                "Binary files {from_name}:{rel} and {to_name}:{rel} differ\n"
            ));
            summaries.push(diff_summary(rel, status, 0, 0, true));
            continue;
        }
        let a_text = a_bytes
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let b_text = b_bytes
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let a_label = match &a_text {
            Some(_) => format!("{from_name}:{rel}"),
            None => "/dev/null".to_string(),
        };
        let b_label = match &b_text {
            Some(_) => format!("{to_name}:{rel}"),
            None => "/dev/null".to_string(),
        };
        let a_str = a_text.as_deref().unwrap_or("");
        let b_str = b_text.as_deref().unwrap_or("");
        if let Some(rendered) = diff::unified(&a_label, &b_label, a_str, b_str, 3) {
            let (a_lines, _) = diff::split_lines(a_str);
            let (b_lines, _) = diff::split_lines(b_str);
            let (add, rem) = diff::counts(&diff::diff_ops(&a_lines, &b_lines));
            body.push_str(&rendered);
            summaries.push(diff_summary(rel, status, add, rem, false));
        }
    }

    if json {
        out!(
            "{{\"tool\":\"snapref\",\"version\":{},\"from\":{},\"to\":{},\"files\":[{}]}}",
            json_str(VERSION),
            json_str(&from_name),
            json_str(&to_name),
            summaries.join(",")
        );
        return Ok(0);
    }
    if body.is_empty() {
        eprintln!("no differences between {from_name} and {to_name}");
    } else {
        stdout_write(&body);
    }
    Ok(0)
}

fn diff_summary(rel: &str, status: &str, add: usize, rem: usize, binary: bool) -> String {
    format!(
        "{{\"path\":{},\"status\":{},\"lines_added\":{add},\"lines_removed\":{rem},\"binary\":{binary}}}",
        json_str(rel),
        json_str(status)
    )
}

// ---------------------------------------------------------------- show ----

fn cmd_show(base: &Path, rest: &[String]) -> R<i32> {
    if let Some(code) = check_help(
        rest,
        "USAGE:\n    snapref show <TURN> [--json]      snapshot metadata + file list\n\
         \x20   snapref show <TURN>:<PATH>        a file's exact bytes at that turn\n",
    ) {
        return Ok(code);
    }
    let mut target: Option<String> = None;
    let mut json = false;
    for arg in rest {
        match arg.as_str() {
            "--json" => json = true,
            other if other.starts_with('-') => {
                return usage(format!("unknown show option '{other}'"))
            }
            other => {
                if target.is_some() {
                    return usage("show takes exactly one TURN[:PATH] argument");
                }
                target = Some(other.to_string());
            }
        }
    }
    let Some(target) = target else {
        return usage("show requires TURN or TURN:PATH (see 'snapref show --help')");
    };
    let store = Store::discover(base)?;

    if let Some((turn_str, path)) = target.split_once(':') {
        if json {
            return usage(
                "'show TURN:PATH' emits the file's raw bytes; --json applies to 'show TURN' only",
            );
        }
        let turn: u64 = match turn_str.parse() {
            Ok(n) => n,
            Err(_) => return usage(format!("'{turn_str}' is not a turn number")),
        };
        let snap = store.snapshot(turn)?;
        let rel = path.trim_start_matches("./").to_string();
        let Some((bytes, _)) = snapshot::file_at(&store, &snap, &rel)? else {
            return op(format!("no such file in turn {turn}: {rel}"));
        };
        let mut stdout = std::io::stdout().lock();
        if let Err(e) = stdout.write_all(&bytes) {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                return op(format!("cannot write to stdout: {e}"));
            }
        }
        return Ok(0);
    }

    let turn: u64 = match target.parse() {
        Ok(n) => n,
        Err(_) => return usage(format!("'{target}' is not a turn number")),
    };
    let snap = store.snapshot(turn)?;
    let files = snapshot::files_of(&store, &snap)?;
    let parent_turn = match &snap.parent {
        Some(pid) => turn_of_id(&store, pid)?,
        None => None,
    };

    if json {
        let rows: Vec<String> = files
            .iter()
            .map(|f| {
                format!(
                    "{{\"mode\":{},\"id\":{},\"path\":{}}}",
                    json_str(if f.exec { "100755" } else { "100644" }),
                    json_str(&f.id),
                    json_str(&f.rel)
                )
            })
            .collect();
        out!(
            "{{\"tool\":\"snapref\",\"version\":{},\"turn\":{},\"id\":{},\"time\":{},\
             \"agent\":{},\"label\":{},\"parent_turn\":{},\"files\":[{}]}}",
            json_str(VERSION),
            snap.turn,
            json_str(&snap.id),
            json_str(&timefmt::utc(snap.time)),
            json_str(&snap.agent),
            json_str(&snap.label),
            parent_turn.map_or("null".to_string(), |n| n.to_string()),
            rows.join(",")
        );
        return Ok(0);
    }

    out!("snapshot {} (turn {})", snap.id, snap.turn);
    out!("time:    {}", timefmt::utc(snap.time));
    out!(
        "agent:   {}",
        if snap.agent.is_empty() {
            "-"
        } else {
            &snap.agent
        }
    );
    out!(
        "label:   {}",
        if snap.label.is_empty() {
            "-"
        } else {
            &snap.label
        }
    );
    match (parent_turn, &snap.parent) {
        (Some(pt), Some(pid)) => out!("parent:  turn {pt} ({})", short(pid)),
        _ => out!("parent:  none (first snapshot)"),
    }
    out!(
        "changes: +{} ~{} -{} files, +{} -{} lines",
        snap.stats.files_added,
        snap.stats.files_modified,
        snap.stats.files_deleted,
        snap.stats.lines_added,
        snap.stats.lines_removed
    );
    out!("files:   {}", files.len());
    out!();
    for f in &files {
        out!(
            "  {} {}  {}",
            if f.exec { "100755" } else { "100644" },
            short(&f.id),
            f.rel
        );
    }
    Ok(0)
}

fn turn_of_id(store: &Store, id: &str) -> R<Option<u64>> {
    for turn in store.turns()? {
        if store.turn_id(turn)? == id {
            return Ok(Some(turn));
        }
    }
    Ok(None)
}

// ------------------------------------------------------------- restore ----

fn cmd_restore(base: &Path, rest: &[String]) -> R<i32> {
    if let Some(code) = check_help(
        rest,
        "USAGE:\n    snapref restore <TURN> [--path PATH ...] [--no-backup] [--force]\n\n\
         Restores the working tree to TURN. A dirty tree is snapped as an\n\
         automatic backup turn first; --no-backup skips that and then\n\
         requires --force to discard changes. --path limits the restore to\n\
         files (or directory prefixes) and never deletes anything.\n",
    ) {
        return Ok(code);
    }
    let mut turn: Option<u64> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut force = false;
    let mut no_backup = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--path" => paths.push(take_value(&mut it, "--path")?),
            "--force" => force = true,
            "--no-backup" => no_backup = true,
            other if other.starts_with('-') => {
                return usage(format!("unknown restore option '{other}'"))
            }
            other => {
                if turn.is_some() {
                    return usage("restore takes exactly one TURN");
                }
                match other.parse::<u64>() {
                    Ok(n) => turn = Some(n),
                    Err(_) => return usage(format!("'{other}' is not a turn number")),
                }
            }
        }
    }
    let Some(turn) = turn else {
        return usage("restore requires a TURN (see 'snapref restore --help')");
    };
    let store = Store::discover(base)?;
    let paths: Vec<String> = paths
        .iter()
        .map(|p| rel_path(&store, base, p))
        .collect::<R<_>>()?;
    let outcome = restore::restore(
        &store,
        turn,
        &paths,
        force,
        no_backup,
        &agent_from_env(),
        timefmt::now(),
    )?;
    let id = store.turn_id(turn)?;
    if let Some(backup) = outcome.backup {
        out!("working tree backed up as turn {backup}");
    }
    if !paths.is_empty() {
        out!(
            "restored {} file(s) from turn {} ({})",
            outcome.written,
            turn,
            short(&id)
        );
    } else if outcome.written == 0 && outcome.deleted == 0 {
        out!(
            "working tree already matches turn {} ({})",
            turn,
            short(&id)
        );
    } else {
        out!(
            "restored working tree to turn {} ({}): {} file(s) written, {} deleted",
            turn,
            short(&id),
            outcome.written,
            outcome.deleted
        );
    }
    Ok(0)
}

// -------------------------------------------------------------- verify ----

fn cmd_verify(base: &Path, rest: &[String]) -> R<i32> {
    if let Some(code) = check_help(
        rest,
        "USAGE:\n    snapref verify\n\nRehashes every object and checks ref/tree/parent reachability.\n",
    ) {
        return Ok(code);
    }
    if let Some(extra) = rest.first() {
        return usage(format!("verify takes no arguments (got '{extra}')"));
    }
    let store = Store::discover(base)?;
    let report = store::verify(&store)?;
    if report.problems.is_empty() {
        out!(
            "verify OK: {} object(s) ({} blobs, {} trees, {} snapshots), {} turn ref(s)",
            report.total(),
            report.blobs,
            report.trees,
            report.snapshots,
            report.refs
        );
        return Ok(0);
    }
    for problem in &report.problems {
        eprintln!("problem: {problem}");
    }
    op(format!(
        "verify failed: {} problem(s)",
        report.problems.len()
    ))
}

// ------------------------------------------------------------- helpers ----

fn take_value(it: &mut std::slice::Iter<'_, String>, flag: &str) -> R<String> {
    match it.next() {
        Some(v) => Ok(v.clone()),
        None => usage(format!("{flag} requires a value")),
    }
}

fn take_number(it: &mut std::slice::Iter<'_, String>, flag: &str) -> R<u64> {
    let raw = take_value(it, flag)?;
    raw.parse().map_err(|_| Fail {
        code: 2,
        msg: format!("{flag} requires a number (got '{raw}')"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_escape_quotes_and_control_bytes() {
        assert_eq!(json_str("plain"), "\"plain\"");
        assert_eq!(json_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_str("line\nbreak\ttab"), "\"line\\nbreak\\ttab\"");
        assert_eq!(json_str("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn normalize_resolves_dot_and_dotdot_lexically() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(normalize(Path::new("/a/../../b")), PathBuf::from("/b"));
    }

    #[test]
    fn truncate_appends_a_two_dot_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactly-10", 10), "exactly-10");
        assert_eq!(truncate("much longer than that", 10), "much lon..");
    }
}
