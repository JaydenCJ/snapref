//! Line → turn attribution engine.
//!
//! The algorithm is the classic incremental blame: start from the oldest
//! version of a file (every line belongs to that turn), then for each newer
//! version diff old→new; lines the diff marks equal keep their attribution,
//! inserted lines are charged to the newer version's origin. The result
//! answers "which turn wrote this exact line?" for the newest version.

use crate::diff;
use crate::snapshot;
use crate::store::Store;

/// Where a line came from: a recorded turn, or the live working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Turn(u64),
    Working,
}

/// One attributed line of the newest version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub origin: Origin,
    /// 1-based line number in the newest version.
    pub line: usize,
    pub text: String,
}

/// Attribute every line of the newest version, given the distinct versions
/// of a file oldest→newest. Panics on an empty version list (callers verify
/// the file has history first).
pub fn attribute(versions: &[(Origin, String)]) -> Vec<BlameLine> {
    assert!(
        !versions.is_empty(),
        "attribute() needs at least one version"
    );
    let (first_origin, first_text) = &versions[0];
    let (mut lines, _) = diff::split_lines(first_text);
    let mut attr: Vec<Origin> = vec![*first_origin; lines.len()];

    for (origin, text) in &versions[1..] {
        let (next_lines, _) = diff::split_lines(text);
        let ops = diff::diff_ops(&lines, &next_lines);
        let mut next_attr = Vec::with_capacity(next_lines.len());
        let mut ai = 0usize;
        for op in ops {
            match op {
                diff::Op::Equal(n) => {
                    for _ in 0..n {
                        next_attr.push(attr[ai]);
                        ai += 1;
                    }
                }
                diff::Op::Del(n) => ai += n,
                diff::Op::Ins(n) => {
                    for _ in 0..n {
                        next_attr.push(*origin);
                    }
                }
            }
        }
        attr = next_attr;
        lines = next_lines;
    }

    attr.into_iter()
        .zip(lines)
        .enumerate()
        .map(|(i, (origin, text))| BlameLine {
            origin,
            line: i + 1,
            text: text.to_string(),
        })
        .collect()
}

/// Collect the distinct consecutive versions of `rel` across recorded turns
/// (ascending), optionally capped at `upto`. Errors if any version is
/// binary — blame is a line-oriented operation.
pub fn file_versions(
    store: &Store,
    rel: &str,
    upto: Option<u64>,
) -> Result<Vec<(u64, String)>, String> {
    let mut versions: Vec<(u64, String)> = Vec::new();
    for turn in store.turns()? {
        if let Some(cap) = upto {
            if turn > cap {
                break;
            }
        }
        let snap = store.snapshot(turn)?;
        let Some((bytes, _)) = snapshot::file_at(store, &snap, rel)? else {
            continue;
        };
        if diff::is_binary(&bytes) {
            return Err(format!(
                "cannot blame binary file: {rel} (as of turn {turn})"
            ));
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if versions.last().map(|(_, t)| t.as_str()) != Some(text.as_str()) {
            versions.push((turn, text));
        }
    }
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(turn: u64, text: &str) -> (Origin, String) {
        (Origin::Turn(turn), text.to_string())
    }

    fn origins(lines: &[BlameLine]) -> Vec<Origin> {
        lines.iter().map(|l| l.origin).collect()
    }

    #[test]
    fn single_version_charges_every_line_to_that_turn() {
        let out = attribute(&[v(1, "a\nb\nc\n")]);
        assert_eq!(origins(&out), vec![Origin::Turn(1); 3]);
        assert_eq!(out[2].line, 3);
        assert_eq!(out[2].text, "c");
    }

    #[test]
    fn inserted_lines_are_charged_to_the_new_turn() {
        let out = attribute(&[v(1, "a\nc\n"), v(3, "a\nb\nc\n")]);
        assert_eq!(
            origins(&out),
            vec![Origin::Turn(1), Origin::Turn(3), Origin::Turn(1)]
        );
    }

    #[test]
    fn replaced_lines_move_to_the_new_turn_untouched_lines_stay() {
        let out = attribute(&[v(1, "a\nb\nc\n"), v(2, "a\nB!\nc\n")]);
        assert_eq!(
            origins(&out),
            vec![Origin::Turn(1), Origin::Turn(2), Origin::Turn(1)]
        );
    }

    #[test]
    fn deletions_shrink_the_attribution_vector() {
        let out = attribute(&[v(1, "a\nb\nc\nd\n"), v(2, "a\nd\n")]);
        assert_eq!(origins(&out), vec![Origin::Turn(1), Origin::Turn(1)]);
        assert_eq!(out[1].text, "d");
        assert_eq!(out[1].line, 2);
    }

    #[test]
    fn attribution_survives_a_chain_of_edits() {
        // Turn 1 writes the file, turn 2 inserts in the middle, turn 4
        // appends, turn 5 rewrites the first line.
        let out = attribute(&[
            v(1, "top\nbottom\n"),
            v(2, "top\nmiddle\nbottom\n"),
            v(4, "top\nmiddle\nbottom\nend\n"),
            v(5, "TOP\nmiddle\nbottom\nend\n"),
        ]);
        assert_eq!(
            origins(&out),
            vec![
                Origin::Turn(5),
                Origin::Turn(2),
                Origin::Turn(1),
                Origin::Turn(4)
            ]
        );
    }

    #[test]
    fn working_tree_origin_is_attributed_like_a_turn() {
        let out = attribute(&[
            v(1, "a\n"),
            (Origin::Working, "a\nuncommitted\n".to_string()),
        ]);
        assert_eq!(origins(&out), vec![Origin::Turn(1), Origin::Working]);
    }

    #[test]
    fn a_line_deleted_and_reintroduced_belongs_to_the_reintroducer() {
        // Turn 2 deletes "b", turn 3 types the same text again: the line is
        // turn 3's — snapref never guesses that it is "the same" line.
        let out = attribute(&[v(1, "a\nb\n"), v(2, "a\n"), v(3, "a\nb\n")]);
        assert_eq!(origins(&out), vec![Origin::Turn(1), Origin::Turn(3)]);
    }
}
