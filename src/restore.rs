//! Working-tree status and restore, with automatic safety backups.
//!
//! Restore is designed to be non-destructive by default: if the working
//! tree has changes that no snapshot holds, they are snapped as an
//! automatic backup turn *before* any file is overwritten. Discarding work
//! requires typing both `--no-backup` and `--force`.

use crate::object::{self, Kind, Snapshot};
use crate::snapshot::{self, FileEntry};
use crate::store::Store;
use crate::walker;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Differences between the working tree and a snapshot.
#[derive(Debug, Default)]
pub struct Changes {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }

    pub fn total(&self) -> usize {
        self.added.len() + self.modified.len() + self.deleted.len()
    }
}

/// Compare the working tree against a snapshot by content hash (and exec bit).
pub fn worktree_changes(store: &Store, snap: &Snapshot) -> Result<Changes, String> {
    let patterns = store.ignore_patterns();
    let (work, _) = walker::walk(&store.work, &patterns)?;
    let snap_files = snapshot::files_of(store, snap)?;
    let snap_map: BTreeMap<&str, &FileEntry> =
        snap_files.iter().map(|e| (e.rel.as_str(), e)).collect();

    let mut changes = Changes::default();
    for file in &work {
        match snap_map.get(file.rel.as_str()) {
            None => changes.added.push(file.rel.clone()),
            Some(entry) => {
                let bytes = fs::read(&file.abs)
                    .map_err(|e| format!("cannot read {}: {e}", file.abs.display()))?;
                let id = object::id_of(Kind::Blob, &bytes);
                if id != entry.id || file.exec != entry.exec {
                    changes.modified.push(file.rel.clone());
                }
            }
        }
    }
    let work_set: BTreeSet<&str> = work.iter().map(|f| f.rel.as_str()).collect();
    for entry in &snap_files {
        if !work_set.contains(entry.rel.as_str()) {
            changes.deleted.push(entry.rel.clone());
        }
    }
    Ok(changes)
}

/// What a restore did.
#[derive(Debug, PartialEq, Eq)]
pub struct RestoreOutcome {
    /// Files created or rewritten (unchanged files are left untouched).
    pub written: usize,
    /// Files deleted because the target turn does not have them.
    pub deleted: usize,
    /// The automatic backup turn, if one was taken.
    pub backup: Option<u64>,
}

/// Restore the working tree (or just `paths`) to the state of `turn`.
///
/// * Full restore also deletes files the target turn does not contain and
///   prunes directories emptied by that.
/// * `paths` entries select an exact file or a whole directory prefix;
///   path-scoped restore never deletes anything.
/// * A dirty working tree is snapped as an automatic backup turn first,
///   unless `no_backup` is set — which then requires `force`.
pub fn restore(
    store: &Store,
    turn: u64,
    paths: &[String],
    force: bool,
    no_backup: bool,
    agent: &str,
    time: i64,
) -> Result<RestoreOutcome, String> {
    let target = store.snapshot(turn)?;
    let latest = store
        .latest()?
        .ok_or_else(|| "no snapshots yet (run 'snapref snap')".to_string())?;
    let latest_snap = if latest == turn {
        target.clone()
    } else {
        store.snapshot(latest)?
    };

    let dirty = !worktree_changes(store, &latest_snap)?.is_empty();
    let mut backup = None;
    if dirty {
        if no_backup {
            if !force {
                return Err(
                    "working tree has changes no snapshot holds; run 'snapref snap' first, \
                     or add --force to discard them"
                        .to_string(),
                );
            }
        } else {
            let label = format!("auto: backup before restore to turn {turn}");
            let snap = snapshot::take(store, &label, agent, None, time)?;
            backup = Some(snap.turn);
        }
    }

    let all = snapshot::files_of(store, &target)?;
    let selected: Vec<&FileEntry> = if paths.is_empty() {
        all.iter().collect()
    } else {
        let selected: Vec<&FileEntry> = all
            .iter()
            .filter(|e| {
                paths
                    .iter()
                    .any(|p| e.rel == *p || e.rel.starts_with(&format!("{p}/")))
            })
            .collect();
        if selected.is_empty() {
            return Err(format!("no files in turn {turn} match the given --path"));
        }
        selected
    };

    let objects = store.objects();
    let mut written = 0usize;
    for entry in &selected {
        let (kind, bytes) = object::read(&objects, &entry.id)?;
        if kind != Kind::Blob {
            return Err(format!(
                "tree entry {} points at a {} object",
                entry.rel,
                kind.tag()
            ));
        }
        let abs = store.work.join(&entry.rel);
        let current = fs::read(&abs).ok();
        let current_exec = fs::symlink_metadata(&abs).ok().map(|m| walker::is_exec(&m));
        if current.as_deref() == Some(bytes.as_slice()) && current_exec == Some(entry.exec) {
            continue; // already identical; keep mtime stable
        }
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        fs::write(&abs, &bytes).map_err(|e| format!("cannot write {}: {e}", abs.display()))?;
        set_exec(&abs, entry.exec)?;
        written += 1;
    }

    let mut deleted = 0usize;
    if paths.is_empty() {
        let patterns = store.ignore_patterns();
        let (work, _) = walker::walk(&store.work, &patterns)?;
        let keep: BTreeSet<&str> = all.iter().map(|e| e.rel.as_str()).collect();
        for file in &work {
            if keep.contains(file.rel.as_str()) {
                continue;
            }
            fs::remove_file(&file.abs)
                .map_err(|e| format!("cannot delete {}: {e}", file.abs.display()))?;
            deleted += 1;
            prune_empty_dirs(&store.work, &file.abs);
        }
    }

    Ok(RestoreOutcome {
        written,
        deleted,
        backup,
    })
}

/// Remove now-empty parent directories, stopping at the working-tree root.
fn prune_empty_dirs(root: &Path, deleted_file: &Path) {
    let mut dir = deleted_file.parent();
    while let Some(d) = dir {
        if d == root || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

#[cfg(unix)]
fn set_exec(path: &Path, exec: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path).map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
    let mut mode = meta.permissions().mode();
    if exec {
        mode |= 0o111;
    } else {
        mode &= !0o111;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("cannot chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_exec(_path: &Path, _exec: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::take;

    fn temp_store(tag: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("snapref-restore-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Store::init(&dir).unwrap().0
    }

    fn put(store: &Store, rel: &str, content: &str) {
        let path = store.work.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn read(store: &Store, rel: &str) -> String {
        fs::read_to_string(store.work.join(rel)).unwrap()
    }

    fn cleanup(store: Store) {
        let _ = fs::remove_dir_all(&store.work);
    }

    #[test]
    fn status_reports_added_modified_and_deleted() {
        let store = temp_store("status");
        put(&store, "stay.txt", "same\n");
        put(&store, "edit.txt", "v1\n");
        put(&store, "gone.txt", "bye\n");
        let snap = take(&store, "", "", None, 0).unwrap();
        put(&store, "edit.txt", "v2\n");
        put(&store, "fresh.txt", "new\n");
        fs::remove_file(store.work.join("gone.txt")).unwrap();
        let ch = worktree_changes(&store, &snap).unwrap();
        assert_eq!(ch.added, vec!["fresh.txt"]);
        assert_eq!(ch.modified, vec!["edit.txt"]);
        assert_eq!(ch.deleted, vec!["gone.txt"]);
        assert_eq!(ch.total(), 3);
        cleanup(store);
    }

    #[test]
    fn clean_tree_is_reported_clean() {
        let store = temp_store("clean");
        put(&store, "f.txt", "x\n");
        let snap = take(&store, "", "", None, 0).unwrap();
        assert!(worktree_changes(&store, &snap).unwrap().is_empty());
        cleanup(store);
    }

    #[test]
    fn full_restore_rewrites_deletes_and_recreates() {
        let store = temp_store("full");
        put(&store, "a.txt", "a-v1\n");
        put(&store, "sub/b.txt", "b-v1\n");
        take(&store, "one", "", None, 1).unwrap();
        put(&store, "a.txt", "a-v2\n");
        fs::remove_file(store.work.join("sub/b.txt")).unwrap();
        put(&store, "extra/new.txt", "later\n");
        take(&store, "two", "", None, 2).unwrap();

        let out = restore(&store, 1, &[], false, false, "", 3).unwrap();
        assert_eq!(out.backup, None); // tree matched turn 2, nothing to back up
        assert_eq!(read(&store, "a.txt"), "a-v1\n");
        assert_eq!(read(&store, "sub/b.txt"), "b-v1\n"); // deleted file is back
        assert!(!store.work.join("extra/new.txt").exists());
        assert!(!store.work.join("extra").exists()); // emptied dir pruned
        assert_eq!(out.written, 2);
        assert_eq!(out.deleted, 1);
        cleanup(store);
    }

    #[test]
    fn dirty_tree_gets_an_automatic_backup_turn() {
        let store = temp_store("backup");
        put(&store, "f.txt", "v1\n");
        take(&store, "one", "", None, 1).unwrap();
        put(&store, "f.txt", "uncommitted work\n");
        let out = restore(&store, 1, &[], false, false, "", 2).unwrap();
        assert_eq!(out.backup, Some(2));
        assert_eq!(read(&store, "f.txt"), "v1\n");
        // The backup turn preserves the discarded-looking content.
        let backup_snap = store.snapshot(2).unwrap();
        let (bytes, _) = snapshot::file_at(&store, &backup_snap, "f.txt")
            .unwrap()
            .unwrap();
        assert_eq!(bytes, b"uncommitted work\n");
        assert!(
            backup_snap.label.contains("auto: backup"),
            "got: {}",
            backup_snap.label
        );
        cleanup(store);
    }

    #[test]
    fn no_backup_on_a_dirty_tree_requires_force() {
        let store = temp_store("force");
        put(&store, "f.txt", "v1\n");
        take(&store, "", "", None, 1).unwrap();
        put(&store, "f.txt", "dirty\n");
        let err = restore(&store, 1, &[], false, true, "", 2).unwrap_err();
        assert!(err.contains("--force"), "got: {err}");
        assert_eq!(read(&store, "f.txt"), "dirty\n"); // untouched on refusal
        let out = restore(&store, 1, &[], true, true, "", 2).unwrap();
        assert_eq!(out.backup, None);
        assert_eq!(read(&store, "f.txt"), "v1\n");
        cleanup(store);
    }

    #[test]
    fn path_scoped_restore_leaves_other_files_alone() {
        let store = temp_store("paths");
        put(&store, "a.txt", "a-v1\n");
        put(&store, "b.txt", "b-v1\n");
        take(&store, "", "", None, 1).unwrap();
        put(&store, "a.txt", "a-v2\n");
        put(&store, "b.txt", "b-v2\n");
        take(&store, "", "", None, 2).unwrap();
        let out = restore(&store, 1, &["a.txt".to_string()], false, false, "", 3).unwrap();
        assert_eq!(read(&store, "a.txt"), "a-v1\n");
        assert_eq!(read(&store, "b.txt"), "b-v2\n");
        assert_eq!(out.written, 1);
        assert_eq!(out.deleted, 0);
        cleanup(store);
    }

    #[test]
    fn a_directory_path_restores_its_whole_prefix() {
        let store = temp_store("prefix");
        put(&store, "src/a.rs", "a-v1\n");
        put(&store, "src/deep/b.rs", "b-v1\n");
        put(&store, "other.txt", "o-v1\n");
        take(&store, "", "", None, 1).unwrap();
        put(&store, "src/a.rs", "a-v2\n");
        put(&store, "src/deep/b.rs", "b-v2\n");
        put(&store, "other.txt", "o-v2\n");
        take(&store, "", "", None, 2).unwrap();
        restore(&store, 1, &["src".to_string()], false, false, "", 3).unwrap();
        assert_eq!(read(&store, "src/a.rs"), "a-v1\n");
        assert_eq!(read(&store, "src/deep/b.rs"), "b-v1\n");
        assert_eq!(read(&store, "other.txt"), "o-v2\n");
        cleanup(store);
    }

    #[test]
    fn unmatched_path_fails_without_touching_anything() {
        let store = temp_store("nomatch");
        put(&store, "f.txt", "v1\n");
        take(&store, "", "", None, 1).unwrap();
        let err = restore(&store, 1, &["nope.txt".to_string()], false, false, "", 2).unwrap_err();
        assert!(err.contains("no files in turn 1"), "got: {err}");
        cleanup(store);
    }
}
