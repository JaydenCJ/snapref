//! Minimal gitignore-flavored glob matching for `.snaprefignore`.
//!
//! Supported: `*` (within a path component), `?` (one byte), `**` (zero or
//! more whole components), a trailing `/` (directories only), and a `/`
//! anywhere else (anchors the pattern at the working-tree root). A pattern
//! without `/` matches a file or directory *name* at any depth. `#` starts
//! a comment; blank lines are ignored. Negation (`!`) is not supported in
//! 0.1.0 and such lines are skipped.

/// One compiled ignore pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    anchored: bool,
    dir_only: bool,
    comps: Vec<String>,
}

impl Pattern {
    /// Parse a single `.snaprefignore` line. Returns `None` for blanks,
    /// comments and (unsupported) negations.
    pub fn parse(line: &str) -> Option<Pattern> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            return None;
        }
        let mut pat = line.to_string();
        let dir_only = pat.ends_with('/');
        while pat.ends_with('/') {
            pat.pop();
        }
        let anchored = pat.contains('/');
        let comps: Vec<String> = pat
            .split('/')
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .collect();
        if comps.is_empty() {
            return None;
        }
        Some(Pattern {
            anchored,
            dir_only,
            comps,
        })
    }

    /// Test a `/`-separated path relative to the working-tree root.
    /// `is_dir` says whether the path names a directory (the walker tests
    /// every directory it is about to descend into, so a matching directory
    /// prunes its whole subtree).
    pub fn matches(&self, rel: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        let comps: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
        if comps.is_empty() {
            return false;
        }
        if self.anchored {
            comps_match(&self.comps, &comps)
        } else {
            // Bare name: match the entry's own (last) component at any depth.
            comp_match(self.comps[0].as_bytes(), comps[comps.len() - 1].as_bytes())
        }
    }
}

/// Parse a whole ignore file into patterns, skipping blanks and comments.
pub fn parse_lines(text: &str) -> Vec<Pattern> {
    text.lines().filter_map(Pattern::parse).collect()
}

/// Match one component pattern (`*`, `?`, literals) against one name.
/// Byte-wise: `?` consumes a single byte, which is intentional and cheap;
/// multi-byte UTF-8 names still match exactly via literals and `*`.
fn comp_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0usize, 0usize);
    let (mut star_p, mut star_i) = (usize::MAX, 0usize);
    while i < s.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star_p = p;
            star_i = i;
            p += 1;
        } else if star_p != usize::MAX {
            p = star_p + 1;
            star_i += 1;
            i = star_i;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// Match pattern components against path components, `**` spanning any run.
fn comps_match(pats: &[String], comps: &[&str]) -> bool {
    if pats.is_empty() {
        return comps.is_empty();
    }
    if pats[0] == "**" {
        return (0..=comps.len()).any(|k| comps_match(&pats[1..], &comps[k..]));
    }
    if comps.is_empty() {
        return false;
    }
    comp_match(pats[0].as_bytes(), comps[0].as_bytes()) && comps_match(&pats[1..], &comps[1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(s: &str) -> Pattern {
        Pattern::parse(s).expect("pattern should parse")
    }

    #[test]
    fn bare_name_matches_at_any_depth() {
        let p = pat("scratch.txt");
        assert!(p.matches("scratch.txt", false));
        assert!(p.matches("deep/nested/scratch.txt", false));
        assert!(!p.matches("scratch.txt.bak", false));
    }

    #[test]
    fn star_matches_within_a_component_only() {
        let p = pat("*.log");
        assert!(p.matches("build.log", false));
        assert!(p.matches("logs/build.log", false)); // bare name, any depth
        assert!(!p.matches("build.log2", false));
    }

    #[test]
    fn question_mark_matches_one_byte() {
        let p = pat("v?.txt");
        assert!(p.matches("v1.txt", false));
        assert!(!p.matches("v10.txt", false));
    }

    #[test]
    fn anchored_pattern_matches_from_the_root() {
        let p = pat("docs/*.md");
        assert!(p.matches("docs/guide.md", false));
        assert!(!p.matches("src/docs/guide.md", false));
        assert!(!p.matches("docs/sub/guide.md", false)); // `*` does not cross `/`
    }

    #[test]
    fn double_star_crosses_directories() {
        let p = pat("gen/**/*.rs");
        assert!(p.matches("gen/a.rs", false));
        assert!(p.matches("gen/x/y/b.rs", false));
        assert!(!p.matches("src/gen/a.rs", false)); // anchored
    }

    #[test]
    fn trailing_slash_is_directory_only() {
        let p = pat("cache/");
        assert!(p.matches("cache", true));
        assert!(p.matches("a/cache", true));
        assert!(!p.matches("cache", false)); // a *file* named cache stays
    }

    #[test]
    fn dir_pattern_with_double_star_covers_contents() {
        let p = pat("vendor/**");
        assert!(p.matches("vendor", true)); // `**` may consume zero components
        assert!(p.matches("vendor/pkg/mod.rs", false));
    }

    #[test]
    fn comments_blanks_and_negations_are_skipped() {
        assert!(Pattern::parse("").is_none());
        assert!(Pattern::parse("   ").is_none());
        assert!(Pattern::parse("# a comment").is_none());
        assert!(Pattern::parse("!keep-me.txt").is_none());
        assert_eq!(parse_lines("# hdr\n\n*.tmp\ncache/\n").len(), 2);
    }

    #[test]
    fn multibyte_names_match_via_literals_and_star() {
        let p = pat("メモ*.txt");
        assert!(p.matches("メモ-1.txt", false));
        assert!(!p.matches("めも-1.txt", false));
    }
}
