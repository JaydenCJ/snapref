//! Line diff engine: Myers O((N+M)·D) with common prefix/suffix trimming,
//! plus a unified-format renderer. Pure and deterministic — both
//! `snapref diff` and the blame attribution engine are built on it.

/// One run of a line-level edit script, in order from the top of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// The next `n` lines are identical on both sides.
    Equal(usize),
    /// The next `n` lines of the old side are deleted.
    Del(usize),
    /// The next `n` lines of the new side are inserted.
    Ins(usize),
}

/// Split text into lines the way diff tools do: a trailing newline does not
/// create a phantom empty line. Returns the lines and whether the text ended
/// with a newline (needed for the `\ No newline at end of file` marker).
pub fn split_lines(text: &str) -> (Vec<&str>, bool) {
    if text.is_empty() {
        return (Vec::new(), true);
    }
    let ends_nl = text.ends_with('\n');
    let mut lines: Vec<&str> = text.split('\n').collect();
    if ends_nl {
        lines.pop();
    }
    (lines, ends_nl)
}

/// NUL sniff over the first 8 KiB, the same heuristic git uses.
pub fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8192).any(|&b| b == 0)
}

/// Minimal edit script between two line slices.
pub fn diff_ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let core = myers(&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut ops = Vec::with_capacity(core.len() + 2);
    if pre > 0 {
        ops.push(Op::Equal(pre));
    }
    ops.extend(core);
    if suf > 0 {
        ops.push(Op::Equal(suf));
    }
    merge(ops)
}

/// Total `(added, removed)` line counts of an edit script.
pub fn counts(ops: &[Op]) -> (usize, usize) {
    ops.iter().fold((0, 0), |(add, rem), op| match op {
        Op::Ins(n) => (add + n, rem),
        Op::Del(n) => (add, rem + n),
        Op::Equal(_) => (add, rem),
    })
}

/// Classic Myers greedy diff with a saved trace for backtracking.
fn myers(a: &[&str], b: &[&str]) -> Vec<Op> {
    let n = a.len();
    let m = b.len();
    if n == 0 && m == 0 {
        return Vec::new();
    }
    if n == 0 {
        return vec![Op::Ins(m)];
    }
    if m == 0 {
        return vec![Op::Del(n)];
    }

    let max = n + m;
    let off = max as i64;
    let mut v = vec![0i64; 2 * max + 1];
    let mut trace: Vec<Vec<i64>> = Vec::new();
    let mut d_final = 0i64;
    'outer: for d in 0..=(max as i64) {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let i = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) {
                v[i + 1]
            } else {
                v[i - 1] + 1
            };
            let mut y = x - k;
            while (x as usize) < n && (y as usize) < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x as usize >= n && y as usize >= m {
                d_final = d;
                break 'outer;
            }
            k += 2;
        }
    }

    // Backtrack from (n, m) to (0, 0), emitting per-line ops in reverse.
    let mut ops_rev: Vec<Op> = Vec::new();
    let (mut x, mut y) = (n as i64, m as i64);
    let mut d = d_final;
    while d > 0 {
        let vv = &trace[d as usize];
        let k = x - y;
        let prev_k =
            if k == -d || (k != d && vv[(k - 1 + off) as usize] < vv[(k + 1 + off) as usize]) {
                k + 1
            } else {
                k - 1
            };
        let prev_x = vv[(prev_k + off) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops_rev.push(Op::Equal(1));
            x -= 1;
            y -= 1;
        }
        if x == prev_x {
            ops_rev.push(Op::Ins(1));
        } else {
            ops_rev.push(Op::Del(1));
        }
        x = prev_x;
        y = prev_y;
        d -= 1;
    }
    while x > 0 && y > 0 {
        ops_rev.push(Op::Equal(1));
        x -= 1;
        y -= 1;
    }
    ops_rev.reverse();
    merge(ops_rev)
}

/// Merge adjacent runs of the same kind.
fn merge(ops: Vec<Op>) -> Vec<Op> {
    let mut out: Vec<Op> = Vec::with_capacity(ops.len());
    for op in ops {
        match (out.last_mut(), op) {
            (Some(Op::Equal(n)), Op::Equal(k)) => *n += k,
            (Some(Op::Del(n)), Op::Del(k)) => *n += k,
            (Some(Op::Ins(n)), Op::Ins(k)) => *n += k,
            (_, op) => out.push(op),
        }
    }
    out
}

/// Within each maximal changed region, list all deletions before all
/// insertions (presentation only; the script stays equivalent).
fn normalize_regions(ops: Vec<Op>) -> Vec<Op> {
    let mut out: Vec<Op> = Vec::with_capacity(ops.len());
    let (mut dels, mut inss) = (0usize, 0usize);
    for op in ops {
        match op {
            Op::Del(n) => dels += n,
            Op::Ins(n) => inss += n,
            Op::Equal(n) => {
                if dels > 0 {
                    out.push(Op::Del(dels));
                    dels = 0;
                }
                if inss > 0 {
                    out.push(Op::Ins(inss));
                    inss = 0;
                }
                out.push(Op::Equal(n));
            }
        }
    }
    if dels > 0 {
        out.push(Op::Del(dels));
    }
    if inss > 0 {
        out.push(Op::Ins(inss));
    }
    merge(out)
}

#[derive(Clone, Copy, PartialEq)]
enum Tag {
    Ctx,
    Del,
    Ins,
}

struct Rec {
    tag: Tag,
    ai: usize,
    bi: usize,
}

/// Render a unified diff with `ctx` context lines, or `None` if the two
/// texts are byte-identical. Emits `\ No newline at end of file` markers
/// exactly like git, including when only the trailing newline changed.
pub fn unified(
    a_label: &str,
    b_label: &str,
    a_text: &str,
    b_text: &str,
    ctx: usize,
) -> Option<String> {
    if a_text == b_text {
        return None;
    }
    let (a, a_nl) = split_lines(a_text);
    let (b, b_nl) = split_lines(b_text);
    let mut ops = diff_ops(&a, &b);

    // Only the trailing newline changed on the last (otherwise equal) line:
    // surface it as a one-line change, the way git does.
    if a_nl != b_nl && !a.is_empty() && !b.is_empty() && a.last() == b.last() {
        if let Some(&Op::Equal(n)) = ops.last() {
            ops.pop();
            if n > 1 {
                ops.push(Op::Equal(n - 1));
            }
            ops.push(Op::Del(1));
            ops.push(Op::Ins(1));
        }
    }
    let ops = normalize_regions(ops);

    // Expand to per-line records carrying both cursors.
    let mut recs: Vec<Rec> = Vec::new();
    let (mut ai, mut bi) = (0usize, 0usize);
    for op in ops {
        match op {
            Op::Equal(n) => {
                for _ in 0..n {
                    recs.push(Rec {
                        tag: Tag::Ctx,
                        ai,
                        bi,
                    });
                    ai += 1;
                    bi += 1;
                }
            }
            Op::Del(n) => {
                for _ in 0..n {
                    recs.push(Rec {
                        tag: Tag::Del,
                        ai,
                        bi,
                    });
                    ai += 1;
                }
            }
            Op::Ins(n) => {
                for _ in 0..n {
                    recs.push(Rec {
                        tag: Tag::Ins,
                        ai,
                        bi,
                    });
                    bi += 1;
                }
            }
        }
    }
    if recs.iter().all(|r| r.tag == Tag::Ctx) {
        return None;
    }

    // Keep changed records plus `ctx` lines of context around each.
    let mut keep = vec![false; recs.len()];
    for (i, r) in recs.iter().enumerate() {
        if r.tag != Tag::Ctx {
            let lo = i.saturating_sub(ctx);
            let hi = (i + ctx).min(recs.len() - 1);
            for slot in keep.iter_mut().take(hi + 1).skip(lo) {
                *slot = true;
            }
        }
    }

    let mut out = String::new();
    out.push_str(&format!("--- {a_label}\n+++ {b_label}\n"));
    let no_nl = "\\ No newline at end of file\n";
    let mut i = 0;
    while i < recs.len() {
        if !keep[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < recs.len() && keep[j] {
            j += 1;
        }
        let hunk = &recs[i..j];
        let a_len = hunk.iter().filter(|r| r.tag != Tag::Ins).count();
        let b_len = hunk.iter().filter(|r| r.tag != Tag::Del).count();
        let a_start = if a_len > 0 {
            hunk[0].ai + 1
        } else {
            hunk[0].ai
        };
        let b_start = if b_len > 0 {
            hunk[0].bi + 1
        } else {
            hunk[0].bi
        };
        out.push_str(&format!("@@ -{a_start},{a_len} +{b_start},{b_len} @@\n"));
        for r in hunk {
            match r.tag {
                Tag::Ctx => {
                    out.push(' ');
                    out.push_str(a[r.ai]);
                    out.push('\n');
                    if r.ai == a.len() - 1 && !a_nl {
                        out.push_str(no_nl);
                    }
                }
                Tag::Del => {
                    out.push('-');
                    out.push_str(a[r.ai]);
                    out.push('\n');
                    if r.ai == a.len() - 1 && !a_nl {
                        out.push_str(no_nl);
                    }
                }
                Tag::Ins => {
                    out.push('+');
                    out.push_str(b[r.bi]);
                    out.push('\n');
                    if r.bi == b.len() - 1 && !b_nl {
                        out.push_str(no_nl);
                    }
                }
            }
        }
        i = j;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply an edit script to `a`; the result must equal `b` — the core
    /// correctness property of any diff implementation.
    fn apply(a: &[&str], b: &[&str], ops: &[Op]) -> Vec<String> {
        let (mut ai, mut bi) = (0usize, 0usize);
        let mut out = Vec::new();
        for op in ops {
            match *op {
                Op::Equal(n) => {
                    for _ in 0..n {
                        assert_eq!(a[ai], b[bi], "Equal over differing lines");
                        out.push(a[ai].to_string());
                        ai += 1;
                        bi += 1;
                    }
                }
                Op::Del(n) => ai += n,
                Op::Ins(n) => {
                    for _ in 0..n {
                        out.push(b[bi].to_string());
                        bi += 1;
                    }
                }
            }
        }
        assert_eq!(ai, a.len(), "script did not consume all of a");
        assert_eq!(bi, b.len(), "script did not consume all of b");
        out
    }

    fn check(a: &[&str], b: &[&str]) -> Vec<Op> {
        let ops = diff_ops(a, b);
        assert_eq!(apply(a, b, &ops), b, "patched a != b");
        ops
    }

    #[test]
    fn identical_inputs_yield_one_equal_run() {
        let a = ["x", "y", "z"];
        assert_eq!(check(&a, &a), vec![Op::Equal(3)]);
    }

    #[test]
    fn pure_insertion_and_pure_deletion() {
        assert_eq!(check(&[], &["a", "b"]), vec![Op::Ins(2)]);
        assert_eq!(check(&["a", "b"], &[]), vec![Op::Del(2)]);
    }

    #[test]
    fn single_line_replacement_in_the_middle() {
        let ops = check(&["a", "b", "c"], &["a", "X", "c"]);
        assert_eq!(counts(&ops), (1, 1));
    }

    #[test]
    fn insertion_between_common_prefix_and_suffix() {
        let ops = check(&["a", "c"], &["a", "b", "c"]);
        assert_eq!(ops, vec![Op::Equal(1), Op::Ins(1), Op::Equal(1)]);
    }

    #[test]
    fn completely_different_files() {
        let ops = check(&["1", "2"], &["8", "9", "10"]);
        assert_eq!(counts(&ops), (3, 2));
    }

    #[test]
    fn repeated_lines_do_not_confuse_the_matcher() {
        // Classic Myers stress case: many identical lines.
        check(&["a", "a", "a", "b", "a"], &["a", "b", "a", "a"]);
    }

    #[test]
    fn seeded_pseudo_random_scripts_all_apply_cleanly() {
        // Deterministic LCG fuzz: 40 pairs of small files over a tiny
        // alphabet, which maximizes tricky common subsequences.
        let mut seed = 0x5eed_cafe_u64;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        let alphabet = ["p", "q", "r"];
        for _ in 0..40 {
            let la = next() % 12;
            let lb = next() % 12;
            let a: Vec<&str> = (0..la).map(|_| alphabet[next() % 3]).collect();
            let b: Vec<&str> = (0..lb).map(|_| alphabet[next() % 3]).collect();
            check(&a, &b);
        }
    }

    #[test]
    fn split_lines_handles_trailing_newlines() {
        assert_eq!(split_lines("a\nb\n").0, vec!["a", "b"]);
        assert_eq!(split_lines("a\nb"), (vec!["a", "b"], false));
        assert_eq!(split_lines("").0.len(), 0);
    }

    #[test]
    fn unified_renders_hunk_headers_and_markers() {
        let d = unified("old", "new", "a\nb\nc\n", "a\nX\nc\n", 1).unwrap();
        assert!(d.starts_with("--- old\n+++ new\n"));
        assert!(d.contains("@@ -1,3 +1,3 @@"));
        assert!(d.contains("\n-b\n"));
        assert!(d.contains("\n+X\n"));
    }

    #[test]
    fn unified_reports_missing_trailing_newline() {
        let d = unified("old", "new", "a\nb\n", "a\nb", 3).unwrap();
        assert!(d.contains("-b\n"));
        assert!(d.contains("+b\n\\ No newline at end of file\n"));
    }

    #[test]
    fn unified_new_file_uses_zero_base_header() {
        let d = unified("/dev/null", "new", "", "x\ny\n", 3).unwrap();
        assert!(d.contains("@@ -0,0 +1,2 @@"), "got: {d}");
    }

    #[test]
    fn unified_splits_distant_changes_into_two_hunks() {
        let a = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n";
        let b = "one\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\ntwelve\n";
        let d = unified("a", "b", a, b, 2).unwrap();
        assert_eq!(d.matches("@@ ").count(), 2, "got: {d}");
    }

    #[test]
    fn binary_sniff_only_triggers_on_nul() {
        assert!(is_binary(b"ab\0cd"));
        assert!(!is_binary("plain text\nwith unicode: 日本語\n".as_bytes()));
    }
}
