//! Content-addressed object model: blobs, trees and snapshot records.
//!
//! Every object is stored as `<kind> <payload-len>\0<payload>` under
//! `objects/<id[0..2]>/<id[2..]>`, where the id is the SHA-1 of the whole
//! encoded object. Blobs therefore share their ids with git blobs. Trees
//! and snapshots use a line-oriented text payload documented in
//! `docs/store-format.md`.

use crate::sha1;
use std::fs;
use std::path::{Path, PathBuf};

pub const MODE_FILE: &str = "100644";
pub const MODE_EXEC: &str = "100755";
pub const MODE_DIR: &str = "040000";

/// The three object kinds in the shadow store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Blob,
    Tree,
    Snapshot,
}

impl Kind {
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Blob => "blob",
            Kind::Tree => "tree",
            Kind::Snapshot => "snapshot",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Kind> {
        match tag {
            "blob" => Some(Kind::Blob),
            "tree" => Some(Kind::Tree),
            "snapshot" => Some(Kind::Snapshot),
            _ => None,
        }
    }
}

/// Compute the id an object would get, without writing it.
pub fn id_of(kind: Kind, payload: &[u8]) -> String {
    sha1::hex(&encode(kind, payload))
}

fn encode(kind: Kind, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(payload.len() + 16);
    buf.extend_from_slice(kind.tag().as_bytes());
    buf.push(b' ');
    buf.extend_from_slice(payload.len().to_string().as_bytes());
    buf.push(0);
    buf.extend_from_slice(payload);
    buf
}

fn object_path(objects: &Path, id: &str) -> PathBuf {
    objects.join(&id[..2]).join(&id[2..])
}

fn valid_id(id: &str) -> bool {
    id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Write an object, deduplicating on content. Returns its id.
pub fn write(objects: &Path, kind: Kind, payload: &[u8]) -> Result<String, String> {
    let encoded = encode(kind, payload);
    let id = sha1::hex(&encoded);
    let path = object_path(objects, &id);
    if path.exists() {
        return Ok(id);
    }
    let dir = objects.join(&id[..2]);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // Write to a temp name then rename, so a crash never leaves a truncated
    // object under its final content-addressed name.
    let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), &id[2..10]));
    fs::write(&tmp, &encoded).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("cannot store object {id}: {e}"))?;
    Ok(id)
}

/// Read and header-validate an object. (Full rehash lives in `verify`.)
pub fn read(objects: &Path, id: &str) -> Result<(Kind, Vec<u8>), String> {
    if !valid_id(id) {
        return Err(format!("malformed object id: {id}"));
    }
    let path = object_path(objects, id);
    let data = fs::read(&path).map_err(|_| format!("missing object {id}"))?;
    parse_encoded(id, &data)
}

/// Parse `<kind> <len>\0<payload>`, validating the declared length.
pub fn parse_encoded(id: &str, data: &[u8]) -> Result<(Kind, Vec<u8>), String> {
    let nul = data
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| format!("object {id} has no header terminator"))?;
    let header = std::str::from_utf8(&data[..nul])
        .map_err(|_| format!("object {id} has a non-UTF-8 header"))?;
    let (tag, len_str) = header
        .split_once(' ')
        .ok_or_else(|| format!("object {id} has a malformed header"))?;
    let kind =
        Kind::from_tag(tag).ok_or_else(|| format!("object {id} has unknown kind '{tag}'"))?;
    let len: usize = len_str
        .parse()
        .map_err(|_| format!("object {id} has a malformed length"))?;
    let payload = &data[nul + 1..];
    if payload.len() != len {
        return Err(format!(
            "object {id} declares {len} bytes but has {}",
            payload.len()
        ));
    }
    Ok((kind, payload.to_vec()))
}

/// One entry of a tree object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: String,
    pub id: String,
    pub name: String,
}

impl TreeEntry {
    pub fn is_dir(&self) -> bool {
        self.mode == MODE_DIR
    }

    pub fn is_exec(&self) -> bool {
        self.mode == MODE_EXEC
    }
}

/// Encode tree entries (already sorted by name) as `<mode> <id> <name>\n`.
pub fn encode_tree(entries: &[TreeEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(e.mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(e.id.as_bytes());
        out.push(b' ');
        out.extend_from_slice(e.name.as_bytes());
        out.push(b'\n');
    }
    out
}

/// Parse a tree payload back into entries.
pub fn parse_tree(payload: &[u8]) -> Result<Vec<TreeEntry>, String> {
    let text = std::str::from_utf8(payload).map_err(|_| "tree payload is not UTF-8".to_string())?;
    let mut entries = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(mode), Some(id), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("malformed tree entry: {line}"));
        };
        if !valid_id(id) || name.is_empty() {
            return Err(format!("malformed tree entry: {line}"));
        }
        entries.push(TreeEntry {
            mode: mode.to_string(),
            id: id.to_string(),
            name: name.to_string(),
        });
    }
    Ok(entries)
}

/// Per-snapshot change statistics, computed against the parent at snap time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub files_added: u64,
    pub files_modified: u64,
    pub files_deleted: u64,
    pub lines_added: u64,
    pub lines_removed: u64,
}

impl Stats {
    pub fn is_empty(&self) -> bool {
        *self == Stats::default()
    }
}

/// A snapshot record: one agent turn's working-tree state plus metadata.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: String,
    pub tree: String,
    pub parent: Option<String>,
    pub turn: u64,
    pub time: i64,
    pub stats: Stats,
    pub agent: String,
    pub label: String,
}

/// Strip newlines from free-text metadata so the record stays line-oriented.
pub fn clean_text(s: &str) -> String {
    s.replace(['\n', '\r'], " ").trim().to_string()
}

/// Encode a snapshot's payload (the `id` field is ignored).
pub fn encode_snapshot(s: &Snapshot) -> Vec<u8> {
    let parent = s.parent.as_deref().unwrap_or("-");
    format!(
        "tree {}\nparent {}\nturn {}\ntime {}\nstats {} {} {} {} {}\nagent {}\nlabel {}\n",
        s.tree,
        parent,
        s.turn,
        s.time,
        s.stats.files_added,
        s.stats.files_modified,
        s.stats.files_deleted,
        s.stats.lines_added,
        s.stats.lines_removed,
        s.agent,
        s.label
    )
    .into_bytes()
}

/// Parse a snapshot payload; `id` is attached to the result.
pub fn parse_snapshot(id: &str, payload: &[u8]) -> Result<Snapshot, String> {
    let text =
        std::str::from_utf8(payload).map_err(|_| format!("snapshot {id} payload is not UTF-8"))?;
    let mut lines = text.lines();
    let mut field = |prefix: &str| -> Result<String, String> {
        lines
            .next()
            .and_then(|l| l.strip_prefix(prefix))
            .map(str::to_string)
            .ok_or_else(|| format!("snapshot {id} is missing the '{}' field", prefix.trim_end()))
    };
    let tree = field("tree ")?;
    let parent_raw = field("parent ")?;
    let turn: u64 = field("turn ")?
        .parse()
        .map_err(|_| format!("snapshot {id} has a malformed turn"))?;
    let time: i64 = field("time ")?
        .parse()
        .map_err(|_| format!("snapshot {id} has a malformed time"))?;
    let stats_raw = field("stats ")?;
    let nums: Vec<u64> = stats_raw
        .split_whitespace()
        .map(|t| t.parse::<u64>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("snapshot {id} has malformed stats"))?;
    if nums.len() != 5 {
        return Err(format!("snapshot {id} has malformed stats"));
    }
    let agent = field("agent ")?;
    let label = field("label ")?;
    if !valid_id(&tree) || (parent_raw != "-" && !valid_id(&parent_raw)) {
        return Err(format!("snapshot {id} references a malformed object id"));
    }
    Ok(Snapshot {
        id: id.to_string(),
        tree,
        parent: (parent_raw != "-").then_some(parent_raw),
        turn,
        time,
        stats: Stats {
            files_added: nums[0],
            files_modified: nums[1],
            files_deleted: nums[2],
            lines_added: nums[3],
            lines_removed: nums[4],
        },
        agent,
        label,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_objects(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snapref-object-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn blob_ids_match_git_hash_object() {
        assert_eq!(
            id_of(Kind::Blob, b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            id_of(Kind::Blob, b"hello world\n"),
            "3b18e512dba79e4c8300dd08aeb37f8e728b8dad"
        );
    }

    #[test]
    fn write_then_read_round_trips() {
        let objects = temp_objects("roundtrip");
        let id = write(&objects, Kind::Blob, b"payload bytes").unwrap();
        let (kind, payload) = read(&objects, &id).unwrap();
        assert_eq!(kind, Kind::Blob);
        assert_eq!(payload, b"payload bytes");
        let _ = fs::remove_dir_all(&objects);
    }

    #[test]
    fn writing_the_same_content_twice_stores_one_object() {
        let objects = temp_objects("dedup");
        let id1 = write(&objects, Kind::Blob, b"same").unwrap();
        let id2 = write(&objects, Kind::Blob, b"same").unwrap();
        assert_eq!(id1, id2);
        let fan = objects.join(&id1[..2]);
        assert_eq!(fs::read_dir(&fan).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&objects);
    }

    #[test]
    fn corrupted_length_header_is_rejected() {
        let objects = temp_objects("corrupt");
        let id = write(&objects, Kind::Blob, b"12345").unwrap();
        let path = objects.join(&id[..2]).join(&id[2..]);
        fs::write(&path, b"blob 99\x0012345").unwrap();
        let err = read(&objects, &id).unwrap_err();
        assert!(err.contains("declares 99 bytes"), "got: {err}");
        let _ = fs::remove_dir_all(&objects);
    }

    #[test]
    fn tree_encoding_round_trips_names_with_spaces() {
        let entries = vec![
            TreeEntry {
                mode: MODE_FILE.into(),
                id: "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".into(),
                name: "notes and ideas.md".into(),
            },
            TreeEntry {
                mode: MODE_DIR.into(),
                id: "3b18e512dba79e4c8300dd08aeb37f8e728b8dad".into(),
                name: "src".into(),
            },
        ];
        let parsed = parse_tree(&encode_tree(&entries)).unwrap();
        assert_eq!(parsed, entries);
        assert!(parsed[1].is_dir());
    }

    #[test]
    fn snapshot_encoding_round_trips_all_fields() {
        let snap = Snapshot {
            id: String::new(),
            tree: "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".into(),
            parent: Some("3b18e512dba79e4c8300dd08aeb37f8e728b8dad".into()),
            turn: 7,
            time: 1_783_850_400,
            stats: Stats {
                files_added: 1,
                files_modified: 2,
                files_deleted: 3,
                lines_added: 40,
                lines_removed: 5,
            },
            agent: "demo-agent".into(),
            label: "fix the parser: handle empty input".into(),
        };
        let parsed = parse_snapshot("someid", &encode_snapshot(&snap)).unwrap();
        assert_eq!(parsed.tree, snap.tree);
        assert_eq!(parsed.parent, snap.parent);
        assert_eq!(parsed.turn, 7);
        assert_eq!(parsed.time, 1_783_850_400);
        assert_eq!(parsed.stats, snap.stats);
        assert_eq!(parsed.agent, "demo-agent");
        assert_eq!(parsed.label, "fix the parser: handle empty input");
    }

    #[test]
    fn root_snapshot_has_no_parent_and_empty_metadata() {
        let snap = Snapshot {
            id: String::new(),
            tree: "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".into(),
            parent: None,
            turn: 1,
            time: 0,
            stats: Stats::default(),
            agent: String::new(),
            label: String::new(),
        };
        let parsed = parse_snapshot("x", &encode_snapshot(&snap)).unwrap();
        assert_eq!(parsed.parent, None);
        assert_eq!(parsed.agent, "");
        assert_eq!(parsed.label, "");
        assert!(parsed.stats.is_empty());
    }

    #[test]
    fn clean_text_flattens_newlines() {
        assert_eq!(clean_text("a\nb\r\nc  "), "a b  c");
    }
}
