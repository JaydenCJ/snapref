//! Deterministic working-tree walker.
//!
//! Rules: entries are visited in sorted order; `.snapref` and `.git` are
//! always skipped (at any depth); a small set of heavy build/dependency
//! directories is excluded by default; `.snaprefignore` patterns prune
//! further; symlinks are never followed (they are skipped and counted);
//! dotfiles ARE included — an agent edits `.env.example` and `.gitignore`
//! too, and restore must round-trip them.

use crate::glob::Pattern;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory or file names that are never snapshotted, at any depth.
pub const ALWAYS_SKIP: &[&str] = &[".snapref", ".git"];

/// Heavy, regenerable names excluded by default (documented in the README).
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    ".cache",
    ".DS_Store",
];

/// One file found in the working tree.
#[derive(Debug, Clone)]
pub struct WorkFile {
    /// `/`-separated path relative to the working-tree root.
    pub rel: String,
    pub abs: PathBuf,
    pub exec: bool,
}

/// Counters for things the walker deliberately skipped.
#[derive(Debug, Default)]
pub struct WalkStats {
    pub symlinks: usize,
    pub excluded: usize,
}

/// Walk `root`, returning files sorted by relative path.
pub fn walk(root: &Path, ignore: &[Pattern]) -> Result<(Vec<WorkFile>, WalkStats), String> {
    let mut files = Vec::new();
    let mut stats = WalkStats::default();
    walk_dir(root, "", ignore, &mut files, &mut stats)?;
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok((files, stats))
}

/// Is this metadata's file executable? (Always false off Unix.)
#[cfg(unix)]
pub fn is_exec(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
pub fn is_exec(_meta: &fs::Metadata) -> bool {
    false
}

fn walk_dir(
    dir: &Path,
    prefix: &str,
    ignore: &[Pattern],
    files: &mut Vec<WorkFile>,
    stats: &mut WalkStats,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut names: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let os_name = entry.file_name();
        let Some(name) = os_name.to_str() else {
            return Err(format!(
                "non-UTF-8 file name is not supported: {}",
                entry.path().display()
            ));
        };
        if name.contains('\n') {
            return Err(format!(
                "file name containing a newline is not supported: {}",
                entry.path().display()
            ));
        }
        names.push((name.to_string(), entry.path()));
    }
    names.sort();

    for (name, path) in names {
        if ALWAYS_SKIP.contains(&name.as_str()) {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
        if meta.file_type().is_symlink() {
            stats.symlinks += 1;
            continue;
        }
        if DEFAULT_EXCLUDES.contains(&name.as_str()) {
            stats.excluded += 1;
            continue;
        }
        if meta.is_dir() {
            if ignore.iter().any(|p| p.matches(&rel, true)) {
                stats.excluded += 1;
                continue;
            }
            walk_dir(&path, &rel, ignore, files, stats)?;
        } else {
            if ignore.iter().any(|p| p.matches(&rel, false)) {
                stats.excluded += 1;
                continue;
            }
            files.push(WorkFile {
                rel,
                abs: path,
                exec: is_exec(&meta),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glob;

    fn temp_tree(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snapref-walker-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn files_come_back_sorted_by_relative_path() {
        let root = temp_tree("sorted");
        put(&root, "b.txt", "b");
        put(&root, "a/z.txt", "z");
        put(&root, "a/a.txt", "a");
        let (files, _) = walk(&root, &[]).unwrap();
        let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, vec!["a/a.txt", "a/z.txt", "b.txt"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn git_and_snapref_are_skipped_at_any_depth() {
        let root = temp_tree("always-skip");
        put(&root, ".git/config", "x");
        put(&root, "sub/.git/config", "x");
        put(&root, ".snapref/objects/ab/cd", "x");
        put(&root, "kept.txt", "k");
        let (files, _) = walk(&root, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rel, "kept.txt");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn heavy_build_directories_are_excluded_by_default() {
        let root = temp_tree("defaults");
        put(&root, "node_modules/pkg/index.js", "x");
        put(&root, "target/debug/bin", "x");
        put(&root, "src/main.rs", "fn main() {}");
        let (files, stats) = walk(&root, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rel, "src/main.rs");
        assert_eq!(stats.excluded, 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dotfiles_are_included_by_default() {
        // Agents edit .gitignore and .env.example; restore must round-trip them.
        let root = temp_tree("dotfiles");
        put(&root, ".gitignore", "/target\n");
        put(&root, ".config/settings.toml", "k = 1\n");
        let (files, _) = walk(&root, &[]).unwrap();
        let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, vec![".config/settings.toml", ".gitignore"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ignore_patterns_prune_files_and_directories() {
        let root = temp_tree("patterns");
        put(&root, "keep.rs", "k");
        put(&root, "scratch.log", "s");
        put(&root, "tmp/deep/file.txt", "t");
        let pats = glob::parse_lines("*.log\ntmp/\n");
        let (files, stats) = walk(&root, &pats).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rel, "keep.rs");
        assert_eq!(stats.excluded, 2); // one file, one pruned directory
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped_and_counted() {
        let root = temp_tree("symlinks");
        put(&root, "real.txt", "r");
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();
        let (files, stats) = walk(&root, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(stats.symlinks, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_is_detected() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_tree("exec");
        put(&root, "run.sh", "#!/bin/sh\n");
        put(&root, "data.txt", "d");
        fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let (files, _) = walk(&root, &[]).unwrap();
        let by_rel: std::collections::BTreeMap<&str, bool> =
            files.iter().map(|f| (f.rel.as_str(), f.exec)).collect();
        assert!(by_rel["run.sh"]);
        assert!(!by_rel["data.txt"]);
        let _ = fs::remove_dir_all(&root);
    }
}
