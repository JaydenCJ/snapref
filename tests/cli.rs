//! End-to-end tests that exercise the compiled `snapref` binary: the full
//! init → snap → log/status/blame/diff/show → restore → verify lifecycle,
//! exit codes, JSON output, ignore handling and corruption detection.
//! Every test builds its own working tree under a temporary directory and
//! pins `SNAPREF_TIME`, so runs are offline and fully deterministic.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_snapref")
}

/// Base timestamp used by every test: 2026-07-12T10:00:00Z.
const T0: i64 = 1_783_850_400;

fn run_at(dir: &Path, time: i64, args: &[&str]) -> Output {
    Command::new(bin())
        .current_dir(dir)
        .env("SNAPREF_TIME", time.to_string())
        .env_remove("SNAPREF_AGENT")
        .args(args)
        .output()
        .expect("failed to run snapref binary")
}

fn run(dir: &Path, args: &[&str]) -> Output {
    run_at(dir, T0, args)
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "expected success, got {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fails(out: &Output, code: i32) -> String {
    assert_eq!(
        out.status.code(),
        Some(code),
        "expected exit {code}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("snapref-cli-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn put(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A three-turn history used by several tests:
/// turn 1 scaffolds, turn 2 edits + adds, turn 3 deletes + edits again.
fn three_turn_project(tag: &str) -> PathBuf {
    let root = tempdir(tag);
    ok(&run(&root, &["init"]));
    put(&root, "src/parser.rs", "fn parse() {\n    todo!()\n}\n");
    put(&root, "README.md", "# demo\n");
    put(&root, "old.txt", "obsolete\n");
    ok(&run_at(
        &root,
        T0,
        &[
            "snap",
            "--label",
            "scaffold parser",
            "--agent",
            "demo-agent",
        ],
    ));
    put(
        &root,
        "src/parser.rs",
        "fn parse() {\n    lex();\n    todo!()\n}\n",
    );
    put(&root, "src/lexer.rs", "fn lex() {}\n");
    ok(&run_at(
        &root,
        T0 + 60,
        &["snap", "--label", "add lexer", "--agent", "demo-agent"],
    ));
    put(
        &root,
        "src/parser.rs",
        "fn parse() {\n    lex();\n    build_ast()\n}\n",
    );
    fs::remove_file(root.join("old.txt")).unwrap();
    ok(&run_at(
        &root,
        T0 + 120,
        &["snap", "--label", "build the AST", "--agent", "demo-agent"],
    ));
    root
}

#[test]
fn version_and_help_are_stable_interfaces() {
    let root = tempdir("version");
    let out = ok(&run(&root, &["--version"]));
    assert_eq!(out.trim(), "snapref 0.1.0");
    let help = ok(&run(&root, &["--help"]));
    assert!(help.contains("COMMANDS:"));
    assert!(help.contains("blame"));
    assert!(help.contains("SNAPREF_TIME"));
    // No command at all is a usage error but still prints the help.
    let out = run(&root, &[]);
    assert_eq!(out.status.code(), Some(2));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn init_is_idempotent_and_snap_reports_turn_stats() {
    let root = tempdir("init-snap");
    let first = ok(&run(&root, &["init"]));
    assert!(
        first.contains("initialized empty snapref store"),
        "got: {first}"
    );
    let again = ok(&run(&root, &["init"]));
    assert!(again.contains("already initialized"), "got: {again}");

    put(&root, "a.txt", "one\ntwo\n");
    put(&root, "sub/b.txt", "three\n");
    let snapped = ok(&run(&root, &["snap", "--label", "first"]));
    assert!(
        snapped.contains("turn 1 snapped") && snapped.contains("+2 ~0 -0 files, +3 -0 lines"),
        "got: {snapped}"
    );

    put(&root, "a.txt", "one\nTWO\n");
    let snapped = ok(&run(&root, &["snap", "--json"]));
    assert!(snapped.contains("\"turn\":2"), "got: {snapped}");
    assert!(snapped.contains("\"files_modified\":1"), "got: {snapped}");
    assert!(snapped.contains("\"lines_added\":1"), "got: {snapped}");
    assert!(
        snapped.contains("\"time\":\"2026-07-12T10:00:00Z\""),
        "got: {snapped}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unchanged_turns_are_recorded_and_status_tracks_dirtiness() {
    let root = tempdir("status");
    ok(&run(&root, &["init"]));
    put(&root, "f.txt", "same\n");
    ok(&run(&root, &["snap", "--label", "one"]));

    // A no-op turn still gets a number — turn alignment is the contract.
    let snapped = ok(&run(&root, &["snap", "--label", "agent did nothing"]));
    assert!(snapped.contains("turn 2 snapped"), "got: {snapped}");
    assert!(
        snapped.contains("no changes since turn 1"),
        "got: {snapped}"
    );

    let clean = ok(&run(&root, &["status"]));
    assert!(
        clean.contains("working tree matches turn 2"),
        "got: {clean}"
    );

    put(&root, "f.txt", "edited\n");
    put(&root, "new.txt", "fresh\n");
    let dirty = ok(&run(&root, &["status"]));
    assert!(dirty.contains("  modified  f.txt"), "got: {dirty}");
    assert!(dirty.contains("  added     new.txt"), "got: {dirty}");
    assert!(
        dirty.contains("2 change(s): 1 added, 1 modified, 0 deleted"),
        "got: {dirty}"
    );

    let json = ok(&run(&root, &["status", "--json"]));
    assert!(json.contains("\"clean\":false"), "got: {json}");
    assert!(json.contains("\"modified\":[\"f.txt\"]"), "got: {json}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn log_lists_turns_newest_first_with_stats_and_labels() {
    let root = three_turn_project("log");
    let log = ok(&run(&root, &["log"]));
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3, "got: {log}");
    assert!(lines[0].starts_with("turn 3"), "got: {log}");
    assert!(lines[2].starts_with("turn 1"), "got: {log}");
    assert!(lines[0].contains("demo-agent: build the AST"), "got: {log}");
    assert!(lines[1].contains("+1 ~1 -0"), "got: {log}"); // turn 2 added lexer, edited parser
    assert!(lines[0].contains("2026-07-12T10:02:00Z"), "got: {log}");

    let limited = ok(&run(&root, &["log", "--limit", "1"]));
    assert_eq!(limited.lines().count(), 1);

    let json = ok(&run(&root, &["log", "--json"]));
    assert!(json.contains("\"turns\":[{\"turn\":3"), "got: {json}");
    assert!(
        json.contains("\"label\":\"scaffold parser\""),
        "got: {json}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn blame_attributes_each_line_to_its_turn_and_the_working_tree() {
    let root = three_turn_project("blame");
    // Add an uncommitted line on top of the three snapped turns.
    put(
        &root,
        "src/parser.rs",
        "fn parse() {\n    lex();\n    build_ast()\n}\n// wip note\n",
    );
    let blame = ok(&run(&root, &["blame", "src/parser.rs"]));
    let lines: Vec<&str> = blame.lines().collect();
    assert_eq!(lines.len(), 5, "got: {blame}");
    assert!(lines[0].starts_with("turn 1"), "got: {blame}"); // fn parse() {
    assert!(lines[1].starts_with("turn 2"), "got: {blame}"); // lex();
    assert!(lines[2].starts_with("turn 3"), "got: {blame}"); // build_ast()
    assert!(lines[3].starts_with("turn 1"), "got: {blame}"); // }
    assert!(lines[4].starts_with("wt"), "got: {blame}"); // // wip note
    assert!(lines[4].contains("(not snapped)"), "got: {blame}");
    assert!(lines[1].contains("add lexer"), "got: {blame}");

    let json = ok(&run(&root, &["blame", "src/parser.rs", "--json"]));
    assert!(json.contains("\"line\":2,\"turn\":2"), "got: {json}");
    assert!(json.contains("\"turn\":null"), "got: {json}"); // the wt line
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn blame_at_turn_ignores_newer_history_and_binary_files_are_refused() {
    let root = three_turn_project("blame-at");
    let blame = ok(&run(&root, &["blame", "src/parser.rs", "--at", "2"]));
    assert!(blame.contains("todo!()"), "got: {blame}");
    assert!(!blame.contains("build_ast"), "got: {blame}");

    // Unknown turn is an operational error, not a panic.
    let err = fails(&run(&root, &["blame", "src/parser.rs", "--at", "99"]), 1);
    assert!(err.contains("unknown turn 99"), "got: {err}");

    // Binary files cannot be blamed.
    fs::write(root.join("logo.bin"), b"PNG\x00fake").unwrap();
    ok(&run(&root, &["snap", "--label", "add binary"]));
    let err = fails(&run(&root, &["blame", "logo.bin"]), 1);
    assert!(err.contains("binary"), "got: {err}");

    // A path with no history at all.
    let err = fails(&run(&root, &["blame", "never-existed.txt"]), 1);
    assert!(err.contains("no history"), "got: {err}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn diff_renders_unified_hunks_between_turns_and_the_working_tree() {
    let root = three_turn_project("diff");
    let out = ok(&run(&root, &["diff", "1", "3"]));
    assert!(out.contains("--- turn 1:src/parser.rs"), "got: {out}");
    assert!(out.contains("+++ turn 3:src/parser.rs"), "got: {out}");
    assert!(out.contains("-    todo!()"), "got: {out}");
    assert!(out.contains("+    lex();"), "got: {out}");
    assert!(out.contains("--- /dev/null"), "got: {out}"); // lexer.rs added
    assert!(out.contains("+++ /dev/null"), "got: {out}"); // old.txt deleted

    // Default: latest turn vs working tree; clean tree means no output.
    let out = run(&root, &["diff"]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "expected no diff output");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no differences between turn 3 and wt"));

    put(&root, "src/parser.rs", "fn parse() {}\n");
    let out = ok(&run(&root, &["diff", "--path", "src/parser.rs"]));
    assert!(out.contains("+++ wt:src/parser.rs"), "got: {out}");

    let json = ok(&run(&root, &["diff", "1", "3", "--json"]));
    assert!(
        json.contains("\"path\":\"old.txt\",\"status\":\"deleted\""),
        "got: {json}"
    );
    assert!(
        json.contains("\"path\":\"src/lexer.rs\",\"status\":\"added\""),
        "got: {json}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn show_prints_metadata_with_git_compatible_blob_ids() {
    let root = tempdir("show-meta");
    ok(&run(&root, &["init"]));
    // "hello world\n" hashes to git's well-known blob id — cross-checkable
    // with `git hash-object` on any machine.
    put(&root, "hello.txt", "hello world\n");
    ok(&run(
        &root,
        &["snap", "--label", "hello", "--agent", "demo-agent"],
    ));
    let show = ok(&run(&root, &["show", "1"]));
    assert!(show.contains("(turn 1)"), "got: {show}");
    assert!(show.contains("label:   hello"), "got: {show}");
    assert!(show.contains("agent:   demo-agent"), "got: {show}");
    assert!(
        show.contains("parent:  none (first snapshot)"),
        "got: {show}"
    );
    assert!(show.contains("100644 3b18e512  hello.txt"), "got: {show}");

    let json = ok(&run(&root, &["show", "1", "--json"]));
    assert!(
        json.contains("\"id\":\"3b18e512dba79e4c8300dd08aeb37f8e728b8dad\""),
        "got: {json}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn show_turn_colon_path_emits_exact_bytes() {
    let root = three_turn_project("show-bytes");
    let v2 = ok(&run(&root, &["show", "2:src/parser.rs"]));
    assert_eq!(v2, "fn parse() {\n    lex();\n    todo!()\n}\n");
    let v1 = ok(&run(&root, &["show", "1:src/parser.rs"]));
    assert_eq!(v1, "fn parse() {\n    todo!()\n}\n");
    let err = fails(&run(&root, &["show", "2:no/such/file.rs"]), 1);
    assert!(err.contains("no such file in turn 2"), "got: {err}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn restore_rewinds_the_tree_and_backs_up_dirty_work_first() {
    let root = three_turn_project("restore");
    // Dirty the tree with work no snapshot holds.
    put(&root, "src/parser.rs", "fn parse() { /* uncommitted */ }\n");
    let out = ok(&run_at(&root, T0 + 300, &["restore", "1"]));
    assert!(
        out.contains("working tree backed up as turn 4"),
        "got: {out}"
    );
    assert!(
        out.contains("restored working tree to turn 1"),
        "got: {out}"
    );

    // Turn 1 state: parser v1 back, lexer gone, old.txt resurrected.
    let parser = fs::read_to_string(root.join("src/parser.rs")).unwrap();
    assert_eq!(parser, "fn parse() {\n    todo!()\n}\n");
    assert!(!root.join("src/lexer.rs").exists());
    assert_eq!(
        fs::read_to_string(root.join("old.txt")).unwrap(),
        "obsolete\n"
    );

    // The uncommitted edit is safe inside the backup turn.
    let backup = ok(&run(&root, &["show", "4:src/parser.rs"]));
    assert_eq!(backup, "fn parse() { /* uncommitted */ }\n");

    // And the rewind is itself reversible: restore the backup turn.
    ok(&run_at(&root, T0 + 360, &["restore", "4"]));
    let parser = fs::read_to_string(root.join("src/parser.rs")).unwrap();
    assert_eq!(parser, "fn parse() { /* uncommitted */ }\n");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn restore_path_scope_and_the_no_backup_force_gate() {
    let root = three_turn_project("restore-path");
    let out = ok(&run(&root, &["restore", "1", "--path", "src/parser.rs"]));
    assert!(out.contains("restored 1 file(s) from turn 1"), "got: {out}");
    assert_eq!(
        fs::read_to_string(root.join("src/parser.rs")).unwrap(),
        "fn parse() {\n    todo!()\n}\n"
    );
    assert!(
        root.join("src/lexer.rs").exists(),
        "path restore must not delete"
    );

    // Tree is now dirty vs turn 3; --no-backup alone must refuse.
    let err = fails(&run(&root, &["restore", "3", "--no-backup"]), 1);
    assert!(err.contains("--force"), "got: {err}");
    let out = ok(&run(&root, &["restore", "3", "--no-backup", "--force"]));
    assert!(
        out.contains("restored working tree to turn 3"),
        "got: {out}"
    );
    assert_eq!(
        fs::read_to_string(root.join("src/parser.rs")).unwrap(),
        "fn parse() {\n    lex();\n    build_ast()\n}\n"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn verify_passes_on_a_healthy_store_and_flags_corruption() {
    let root = three_turn_project("verify");
    let out = ok(&run(&root, &["verify"]));
    assert!(out.contains("verify OK:"), "got: {out}");
    assert!(out.contains("3 turn ref(s)"), "got: {out}");

    // Flip one byte inside one object: verify must name the object and fail.
    let objects = root.join(".snapref/objects");
    let mut victim: Option<PathBuf> = None;
    for fan in fs::read_dir(&objects).unwrap() {
        let fan = fan.unwrap().path();
        if let Some(f) = fs::read_dir(&fan).unwrap().next() {
            victim = Some(f.unwrap().path());
            break;
        }
    }
    let victim = victim.expect("store has objects");
    let saved = fs::read(&victim).unwrap();
    let mut corrupt = saved.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0xff;
    fs::write(&victim, &corrupt).unwrap();
    let err = fails(&run(&root, &["verify"]), 1);
    assert!(err.contains("verify failed"), "got: {err}");
    assert!(err.contains("problem:"), "got: {err}");

    // Restoring the original bytes heals the store.
    fs::write(&victim, &saved).unwrap();
    ok(&run(&root, &["verify"]));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn snaprefignore_prunes_snapshots_and_commands_fail_cleanly_outside_a_store() {
    let root = tempdir("ignore");
    ok(&run(&root, &["init"]));
    put(&root, ".snaprefignore", "*.log\nscratch/\n");
    put(&root, "keep.rs", "kept\n");
    put(&root, "noise.log", "ignored\n");
    put(&root, "scratch/tmp.txt", "ignored\n");
    ok(&run(&root, &["snap", "--label", "with ignores"]));
    let show = ok(&run(&root, &["show", "1"]));
    assert!(show.contains("keep.rs"), "got: {show}");
    assert!(show.contains(".snaprefignore"), "got: {show}"); // the ignore file itself is tracked
    assert!(!show.contains("noise.log"), "got: {show}");
    assert!(!show.contains("scratch"), "got: {show}");

    // Outside any store: clear operational error pointing at init.
    let outside = tempdir("outside");
    let err = fails(&run(&outside, &["status"]), 1);
    assert!(err.contains("snapref init"), "got: {err}");
    // Unknown commands and flags are usage errors (exit 2).
    let err = fails(&run(&outside, &["frobnicate"]), 2);
    assert!(err.contains("unknown command"), "got: {err}");
    fails(&run(&outside, &["log", "--nope"]), 2);
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&outside);
}
