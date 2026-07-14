//! Taking snapshots and reading files back out of them.
//!
//! A snapshot walks the working tree, writes every file as a blob (content
//! deduplicated — unchanged files cost nothing), builds nested tree objects
//! bottom-up, computes change statistics against the parent snapshot, and
//! records the result under the next turn number.

use crate::diff;
use crate::object::{self, Kind, Snapshot, Stats};
use crate::store::Store;
use crate::walker;
use std::collections::BTreeMap;
use std::path::Path;

/// A file inside a snapshot: repo-relative path, blob id, exec bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub rel: String,
    pub id: String,
    pub exec: bool,
}

/// Snapshot the working tree as the next turn (or `turn_override`).
///
/// An unchanged tree still records a turn: turn numbers must stay aligned
/// with the agent conversation, and "the agent claimed to edit X but
/// nothing changed" is itself information.
pub fn take(
    store: &Store,
    label: &str,
    agent: &str,
    turn_override: Option<u64>,
    time: i64,
) -> Result<Snapshot, String> {
    let patterns = store.ignore_patterns();
    let (files, _stats) = walker::walk(&store.work, &patterns)?;
    let objects = store.objects();

    let mut entries = Vec::with_capacity(files.len());
    for f in &files {
        let bytes =
            std::fs::read(&f.abs).map_err(|e| format!("cannot read {}: {e}", f.abs.display()))?;
        let id = object::write(&objects, Kind::Blob, &bytes)?;
        entries.push(FileEntry {
            rel: f.rel.clone(),
            id,
            exec: f.exec,
        });
    }
    let tree = write_tree(&objects, &entries)?;

    let latest = store.latest()?;
    let (parent_id, parent_files) = match latest {
        Some(n) => {
            let parent = store.snapshot(n)?;
            let parent_files = files_of(store, &parent)?;
            (Some(parent.id), parent_files)
        }
        None => (None, Vec::new()),
    };
    let turn = match turn_override {
        Some(n) => {
            if n == 0 {
                return Err("turn numbers start at 1".to_string());
            }
            if let Some(l) = latest {
                if n <= l {
                    return Err(format!(
                        "requested turn {n} is not greater than the latest turn {l}"
                    ));
                }
            }
            n
        }
        None => latest.map_or(1, |l| l + 1),
    };

    let stats = stats_between(&objects, &parent_files, &entries)?;
    let snap = Snapshot {
        id: String::new(),
        tree,
        parent: parent_id,
        turn,
        time,
        stats,
        agent: object::clean_text(agent),
        label: object::clean_text(label),
    };
    let id = object::write(&objects, Kind::Snapshot, &object::encode_snapshot(&snap))?;
    store.write_turn(turn, &id)?;
    Ok(Snapshot { id, ..snap })
}

/// Flatten a snapshot's tree into sorted file entries.
pub fn files_of(store: &Store, snap: &Snapshot) -> Result<Vec<FileEntry>, String> {
    let objects = store.objects();
    let mut out = Vec::new();
    collect(&objects, &snap.tree, "", &mut out)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// Read one file's bytes (and exec bit) out of a snapshot, if present.
pub fn file_at(
    store: &Store,
    snap: &Snapshot,
    rel: &str,
) -> Result<Option<(Vec<u8>, bool)>, String> {
    let objects = store.objects();
    let comps: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
    if comps.is_empty() {
        return Ok(None);
    }
    let mut tree_id = snap.tree.clone();
    for (i, comp) in comps.iter().enumerate() {
        let (kind, payload) = object::read(&objects, &tree_id)?;
        if kind != Kind::Tree {
            return Ok(None);
        }
        let entries = object::parse_tree(&payload)?;
        let Some(entry) = entries.into_iter().find(|e| e.name == *comp) else {
            return Ok(None);
        };
        if i == comps.len() - 1 {
            if entry.is_dir() {
                return Ok(None);
            }
            let (kind, bytes) = object::read(&objects, &entry.id)?;
            if kind != Kind::Blob {
                return Err(format!(
                    "tree entry {rel} points at a {} object",
                    kind.tag()
                ));
            }
            return Ok(Some((bytes, entry.is_exec())));
        }
        if !entry.is_dir() {
            return Ok(None);
        }
        tree_id = entry.id;
    }
    Ok(None)
}

fn collect(
    objects: &Path,
    tree_id: &str,
    prefix: &str,
    out: &mut Vec<FileEntry>,
) -> Result<(), String> {
    let (kind, payload) = object::read(objects, tree_id)?;
    if kind != Kind::Tree {
        return Err(format!("object {tree_id} is not a tree"));
    }
    for entry in object::parse_tree(&payload)? {
        let rel = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        if entry.is_dir() {
            collect(objects, &entry.id, &rel, out)?;
        } else {
            out.push(FileEntry {
                exec: entry.is_exec(),
                id: entry.id,
                rel,
            });
        }
    }
    Ok(())
}

enum Node {
    File { id: String, exec: bool },
    Dir(BTreeMap<String, Node>),
}

fn write_tree(objects: &Path, entries: &[FileEntry]) -> Result<String, String> {
    let mut root: BTreeMap<String, Node> = BTreeMap::new();
    for entry in entries {
        let comps: Vec<&str> = entry.rel.split('/').collect();
        insert(&mut root, &comps, entry)?;
    }
    write_dir(objects, &root)
}

fn insert(map: &mut BTreeMap<String, Node>, comps: &[&str], e: &FileEntry) -> Result<(), String> {
    if comps.len() == 1 {
        map.insert(
            comps[0].to_string(),
            Node::File {
                id: e.id.clone(),
                exec: e.exec,
            },
        );
        return Ok(());
    }
    let node = map
        .entry(comps[0].to_string())
        .or_insert_with(|| Node::Dir(BTreeMap::new()));
    match node {
        Node::Dir(sub) => insert(sub, &comps[1..], e),
        Node::File { .. } => Err(format!("path conflict at component '{}'", comps[0])),
    }
}

fn write_dir(objects: &Path, map: &BTreeMap<String, Node>) -> Result<String, String> {
    let mut tree_entries = Vec::with_capacity(map.len());
    for (name, node) in map {
        let entry = match node {
            Node::File { id, exec } => object::TreeEntry {
                mode: if *exec {
                    object::MODE_EXEC
                } else {
                    object::MODE_FILE
                }
                .to_string(),
                id: id.clone(),
                name: name.clone(),
            },
            Node::Dir(sub) => object::TreeEntry {
                mode: object::MODE_DIR.to_string(),
                id: write_dir(objects, sub)?,
                name: name.clone(),
            },
        };
        tree_entries.push(entry);
    }
    object::write(objects, Kind::Tree, &object::encode_tree(&tree_entries))
}

/// Change statistics between two flat file lists (line counts skip binaries).
fn stats_between(objects: &Path, old: &[FileEntry], new: &[FileEntry]) -> Result<Stats, String> {
    let old_map: BTreeMap<&str, &FileEntry> = old.iter().map(|e| (e.rel.as_str(), e)).collect();
    let new_map: BTreeMap<&str, &FileEntry> = new.iter().map(|e| (e.rel.as_str(), e)).collect();
    let mut stats = Stats::default();

    for (rel, entry) in &new_map {
        match old_map.get(rel) {
            None => {
                stats.files_added += 1;
                stats.lines_added += line_count(objects, &entry.id)?;
            }
            Some(prev) if prev.id != entry.id || prev.exec != entry.exec => {
                stats.files_modified += 1;
                if prev.id != entry.id {
                    let (add, rem) = line_delta(objects, &prev.id, &entry.id)?;
                    stats.lines_added += add;
                    stats.lines_removed += rem;
                }
            }
            Some(_) => {}
        }
    }
    for (rel, entry) in &old_map {
        if !new_map.contains_key(rel) {
            stats.files_deleted += 1;
            stats.lines_removed += line_count(objects, &entry.id)?;
        }
    }
    Ok(stats)
}

fn line_count(objects: &Path, blob_id: &str) -> Result<u64, String> {
    let (_, bytes) = object::read(objects, blob_id)?;
    if diff::is_binary(&bytes) {
        return Ok(0);
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(diff::split_lines(&text).0.len() as u64)
}

fn line_delta(objects: &Path, old_id: &str, new_id: &str) -> Result<(u64, u64), String> {
    let (_, old_bytes) = object::read(objects, old_id)?;
    let (_, new_bytes) = object::read(objects, new_id)?;
    if diff::is_binary(&old_bytes) || diff::is_binary(&new_bytes) {
        return Ok((0, 0));
    }
    let old_text = String::from_utf8_lossy(&old_bytes);
    let new_text = String::from_utf8_lossy(&new_bytes);
    let (old_lines, _) = diff::split_lines(&old_text);
    let (new_lines, _) = diff::split_lines(&new_text);
    let (add, rem) = diff::counts(&diff::diff_ops(&old_lines, &new_lines));
    Ok((add as u64, rem as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!(
            "snapref-snapshot-test-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Store::init(&dir).unwrap().0
    }

    fn put(store: &Store, rel: &str, content: &str) {
        let path = store.work.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn cleanup(store: Store) {
        let _ = fs::remove_dir_all(&store.work);
    }

    fn rels(entries: &[FileEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.rel.as_str()).collect()
    }

    #[test]
    fn first_snapshot_records_every_file_as_added() {
        let store = temp_store("first");
        put(&store, "src/main.rs", "fn main() {}\n");
        put(&store, "README.md", "# demo\nsecond line\n");
        let snap = take(&store, "initial", "tester", None, 100).unwrap();
        assert_eq!(snap.turn, 1);
        assert_eq!(snap.parent, None);
        assert_eq!(snap.stats.files_added, 2);
        assert_eq!(snap.stats.lines_added, 3);
        assert_eq!(
            rels(&files_of(&store, &snap).unwrap()),
            vec!["README.md", "src/main.rs"]
        );
        cleanup(store);
    }

    #[test]
    fn nested_directories_round_trip_through_trees() {
        let store = temp_store("nested");
        put(&store, "a/b/c/deep.txt", "deep\n");
        put(&store, "a/top.txt", "top\n");
        let snap = take(&store, "", "", None, 0).unwrap();
        let (bytes, exec) = file_at(&store, &snap, "a/b/c/deep.txt").unwrap().unwrap();
        assert_eq!(bytes, b"deep\n");
        assert!(!exec);
        assert!(file_at(&store, &snap, "a/b/missing.txt").unwrap().is_none());
        assert!(file_at(&store, &snap, "a/b").unwrap().is_none()); // a dir, not a file
        cleanup(store);
    }

    #[test]
    fn identical_content_in_two_files_shares_one_blob() {
        let store = temp_store("dedup");
        put(&store, "one.txt", "same bytes\n");
        put(&store, "two.txt", "same bytes\n");
        let snap = take(&store, "", "", None, 0).unwrap();
        let files = files_of(&store, &snap).unwrap();
        assert_eq!(files[0].id, files[1].id);
        cleanup(store);
    }

    #[test]
    fn second_snapshot_computes_modify_and_delete_stats() {
        let store = temp_store("stats");
        put(&store, "keep.txt", "a\nb\n");
        put(&store, "gone.txt", "x\ny\nz\n");
        take(&store, "one", "", None, 1).unwrap();
        put(&store, "keep.txt", "a\nB\nc\n"); // 1 line changed, 1 added
        fs::remove_file(store.work.join("gone.txt")).unwrap();
        put(&store, "new.txt", "n\n");
        let snap = take(&store, "two", "", None, 2).unwrap();
        assert_eq!(snap.turn, 2);
        assert_eq!(snap.stats.files_added, 1);
        assert_eq!(snap.stats.files_modified, 1);
        assert_eq!(snap.stats.files_deleted, 1);
        assert_eq!(snap.stats.lines_added, 2 + 1); // keep.txt +2, new.txt +1
        assert_eq!(snap.stats.lines_removed, 1 + 3); // keep.txt -1, gone.txt -3
        cleanup(store);
    }

    #[test]
    fn unchanged_tree_still_records_a_turn_with_empty_stats() {
        let store = temp_store("nochange");
        put(&store, "f.txt", "same\n");
        let first = take(&store, "", "", None, 1).unwrap();
        let second = take(&store, "", "", None, 2).unwrap();
        assert_eq!(second.turn, 2);
        assert_eq!(second.tree, first.tree); // whole tree deduplicated
        assert!(second.stats.is_empty());
        cleanup(store);
    }

    #[test]
    fn turn_override_must_move_forward() {
        let store = temp_store("override");
        put(&store, "f.txt", "1\n");
        // Turns are 1-based everywhere; 0 is rejected up front.
        let err = take(&store, "", "", Some(0), 0).unwrap_err();
        assert!(err.contains("start at 1"), "got: {err}");
        take(&store, "", "", Some(5), 0).unwrap();
        let err = take(&store, "", "", Some(5), 0).unwrap_err();
        assert!(err.contains("not greater"), "got: {err}");
        let snap = take(&store, "", "", Some(9), 0).unwrap();
        assert_eq!(snap.turn, 9);
        assert_eq!(store.latest().unwrap(), Some(9));
        cleanup(store);
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_is_recorded_in_the_tree() {
        use std::os::unix::fs::PermissionsExt;
        let store = temp_store("exec");
        put(&store, "run.sh", "#!/bin/sh\n");
        fs::set_permissions(store.work.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let snap = take(&store, "", "", None, 0).unwrap();
        let (_, exec) = file_at(&store, &snap, "run.sh").unwrap().unwrap();
        assert!(exec);
        cleanup(store);
    }
}
