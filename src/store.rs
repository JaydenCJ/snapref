//! Store layout under `.snapref/`: objects, turn refs and the format marker.
//!
//! ```text
//! .snapref/
//! ├── format            # store format version ("1")
//! ├── objects/xx/yyyy…  # content-addressed objects
//! └── refs/turns/<n>    # one file per turn, containing a snapshot id
//! ```

use crate::glob::{self, Pattern};
use crate::object::{self, Kind, Snapshot};
use std::fs;
use std::path::{Path, PathBuf};

pub const STORE_DIR: &str = ".snapref";
const FORMAT_VERSION: &str = "1";

/// An opened shadow store: the working-tree root and the `.snapref` dir.
#[derive(Debug, Clone)]
pub struct Store {
    pub work: PathBuf,
    pub dir: PathBuf,
}

impl Store {
    /// Create (or re-open) a store at `work`. Returns the store and whether
    /// it was newly created. Idempotent by design: agent wrappers can call
    /// `snapref init` unconditionally at session start.
    pub fn init(work: &Path) -> Result<(Store, bool), String> {
        let work = fs::canonicalize(work)
            .map_err(|e| format!("cannot open directory {}: {e}", work.display()))?;
        let dir = work.join(STORE_DIR);
        let format_file = dir.join("format");
        let created = !format_file.is_file();
        for sub in ["objects", "refs/turns"] {
            let p = dir.join(sub);
            fs::create_dir_all(&p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
        if created {
            fs::write(&format_file, format!("{FORMAT_VERSION}\n"))
                .map_err(|e| format!("cannot write {}: {e}", format_file.display()))?;
        }
        let store = Store { work, dir };
        store.check_format()?;
        Ok((store, created))
    }

    /// Walk upward from `start` until a `.snapref/format` marker is found.
    pub fn discover(start: &Path) -> Result<Store, String> {
        let mut cur = fs::canonicalize(start)
            .map_err(|e| format!("cannot open directory {}: {e}", start.display()))?;
        loop {
            if cur.join(STORE_DIR).join("format").is_file() {
                let store = Store {
                    dir: cur.join(STORE_DIR),
                    work: cur,
                };
                store.check_format()?;
                return Ok(store);
            }
            if !cur.pop() {
                return Err("not a snapref working tree (run 'snapref init' first)".to_string());
            }
        }
    }

    fn check_format(&self) -> Result<(), String> {
        let raw = fs::read_to_string(self.dir.join("format"))
            .map_err(|e| format!("cannot read store format marker: {e}"))?;
        if raw.trim() != FORMAT_VERSION {
            return Err(format!(
                "unsupported store format '{}' (this snapref understands format {FORMAT_VERSION})",
                raw.trim()
            ));
        }
        Ok(())
    }

    pub fn objects(&self) -> PathBuf {
        self.dir.join("objects")
    }

    fn turns_dir(&self) -> PathBuf {
        self.dir.join("refs").join("turns")
    }

    /// All recorded turn numbers, ascending.
    pub fn turns(&self) -> Result<Vec<u64>, String> {
        let dir = self.turns_dir();
        let mut out = Vec::new();
        let entries =
            fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Ok(n) = name.parse::<u64>() {
                out.push(n);
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// The newest recorded turn, if any.
    pub fn latest(&self) -> Result<Option<u64>, String> {
        Ok(self.turns()?.into_iter().next_back())
    }

    /// Resolve a turn number to its snapshot id.
    pub fn turn_id(&self, turn: u64) -> Result<String, String> {
        let path = self.turns_dir().join(turn.to_string());
        let raw = fs::read_to_string(&path)
            .map_err(|_| format!("unknown turn {turn} (see 'snapref log')"))?;
        Ok(raw.trim().to_string())
    }

    /// Record a new turn ref. Turns are append-only: overwriting is refused.
    pub fn write_turn(&self, turn: u64, id: &str) -> Result<(), String> {
        let path = self.turns_dir().join(turn.to_string());
        if path.exists() {
            return Err(format!("turn {turn} is already recorded"));
        }
        fs::write(&path, format!("{id}\n"))
            .map_err(|e| format!("cannot write turn ref {turn}: {e}"))
    }

    /// Load the snapshot record behind a turn.
    pub fn snapshot(&self, turn: u64) -> Result<Snapshot, String> {
        let id = self.turn_id(turn)?;
        let (kind, payload) = object::read(&self.objects(), &id)?;
        if kind != Kind::Snapshot {
            return Err(format!("turn {turn} ref points at a {} object", kind.tag()));
        }
        object::parse_snapshot(&id, &payload)
    }

    /// Ignore patterns from `.snaprefignore` at the working-tree root.
    pub fn ignore_patterns(&self) -> Vec<Pattern> {
        match fs::read_to_string(self.work.join(".snaprefignore")) {
            Ok(text) => glob::parse_lines(&text),
            Err(_) => Vec::new(),
        }
    }
}

/// Outcome of a full store integrity check.
#[derive(Debug, Default)]
pub struct VerifyReport {
    pub blobs: usize,
    pub trees: usize,
    pub snapshots: usize,
    pub refs: usize,
    pub problems: Vec<String>,
}

impl VerifyReport {
    pub fn total(&self) -> usize {
        self.blobs + self.trees + self.snapshots
    }
}

/// Rehash every object and check that every turn ref resolves to a snapshot
/// whose whole tree (and parent chain) is present. Deterministic order.
pub fn verify(store: &Store) -> Result<VerifyReport, String> {
    let objects = store.objects();
    let mut report = VerifyReport::default();
    let mut kinds = std::collections::BTreeMap::new();

    // Pass 1: every object file must rehash to its own name.
    let mut fans: Vec<PathBuf> = fs::read_dir(&objects)
        .map_err(|e| format!("cannot read {}: {e}", objects.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    fans.sort();
    for fan in fans {
        let prefix = fan
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let mut files: Vec<PathBuf> = fs::read_dir(&fan)
            .map_err(|e| format!("cannot read {}: {e}", fan.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        files.sort();
        for file in files {
            let rest = file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let id = format!("{prefix}{rest}");
            let data = match fs::read(&file) {
                Ok(d) => d,
                Err(e) => {
                    report
                        .problems
                        .push(format!("cannot read object {id}: {e}"));
                    continue;
                }
            };
            if crate::sha1::hex(&data) != id {
                report
                    .problems
                    .push(format!("object {id} is corrupt (content hash mismatch)"));
                continue;
            }
            match object::parse_encoded(&id, &data) {
                Ok((kind, _)) => {
                    match kind {
                        Kind::Blob => report.blobs += 1,
                        Kind::Tree => report.trees += 1,
                        Kind::Snapshot => report.snapshots += 1,
                    }
                    kinds.insert(id, kind);
                }
                Err(e) => report.problems.push(e),
            }
        }
    }

    // Pass 2: refs resolve, trees are complete, parents exist.
    for turn in store.turns()? {
        report.refs += 1;
        let id = store.turn_id(turn)?;
        match kinds.get(&id) {
            Some(Kind::Snapshot) => {}
            Some(k) => {
                report
                    .problems
                    .push(format!("turn {turn} points at a {} object", k.tag()));
                continue;
            }
            None => {
                report
                    .problems
                    .push(format!("turn {turn} points at missing object {id}"));
                continue;
            }
        }
        let snap = match store.snapshot(turn) {
            Ok(s) => s,
            Err(e) => {
                report.problems.push(e);
                continue;
            }
        };
        if let Some(parent) = &snap.parent {
            if kinds.get(parent) != Some(&Kind::Snapshot) {
                report
                    .problems
                    .push(format!("turn {turn} parent {parent} is missing"));
            }
        }
        check_tree(&objects, &kinds, &snap.tree, turn, &mut report.problems);
    }
    Ok(report)
}

fn check_tree(
    objects: &Path,
    kinds: &std::collections::BTreeMap<String, Kind>,
    tree_id: &str,
    turn: u64,
    problems: &mut Vec<String>,
) {
    if kinds.get(tree_id) != Some(&Kind::Tree) {
        problems.push(format!("turn {turn} references missing tree {tree_id}"));
        return;
    }
    let Ok((_, payload)) = object::read(objects, tree_id) else {
        problems.push(format!("turn {turn} tree {tree_id} is unreadable"));
        return;
    };
    let entries = match object::parse_tree(&payload) {
        Ok(e) => e,
        Err(e) => {
            problems.push(format!("turn {turn}: {e}"));
            return;
        }
    };
    for entry in entries {
        if entry.is_dir() {
            check_tree(objects, kinds, &entry.id, turn, problems);
        } else if kinds.get(&entry.id) != Some(&Kind::Blob) {
            problems.push(format!(
                "turn {turn} references missing blob {} ({})",
                entry.id, entry.name
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_work(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snapref-store-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn init_creates_layout_and_is_idempotent() {
        let work = temp_work("init");
        let (store, created) = Store::init(&work).unwrap();
        assert!(created);
        assert!(store.objects().is_dir());
        assert!(store.dir.join("refs/turns").is_dir());
        let (_, created_again) = Store::init(&work).unwrap();
        assert!(!created_again);
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn discover_walks_up_from_a_subdirectory() {
        let work = temp_work("discover");
        Store::init(&work).unwrap();
        let deep = work.join("a/b/c");
        fs::create_dir_all(&deep).unwrap();
        let store = Store::discover(&deep).unwrap();
        assert_eq!(store.work, fs::canonicalize(&work).unwrap());
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn discover_fails_cleanly_outside_any_store() {
        let work = temp_work("nostore");
        let err = Store::discover(&work).unwrap_err();
        assert!(err.contains("snapref init"), "got: {err}");
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn turn_refs_are_append_only_and_sorted_numerically() {
        let work = temp_work("turns");
        let (store, _) = Store::init(&work).unwrap();
        let id = object::write(&store.objects(), Kind::Blob, b"x").unwrap();
        store.write_turn(2, &id).unwrap();
        store.write_turn(10, &id).unwrap();
        store.write_turn(1, &id).unwrap();
        assert_eq!(store.turns().unwrap(), vec![1, 2, 10]); // numeric, not lexicographic
        assert_eq!(store.latest().unwrap(), Some(10));
        assert!(store
            .write_turn(2, &id)
            .unwrap_err()
            .contains("already recorded"));
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn unknown_turn_produces_a_helpful_error() {
        let work = temp_work("unknown-turn");
        let (store, _) = Store::init(&work).unwrap();
        let err = store.turn_id(42).unwrap_err();
        assert!(err.contains("unknown turn 42"), "got: {err}");
        let _ = fs::remove_dir_all(&work);
    }
}
