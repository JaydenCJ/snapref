//! snapref — shadow git-style snapshots of a working tree, one per agent turn.
//!
//! The library is split into small pure modules so every behavior is
//! unit-testable without a terminal: content hashing (`sha1`), the object
//! model (`object`), store layout (`store`), the deterministic walker
//! (`walker`) with its ignore matcher (`glob`), snapshotting (`snapshot`),
//! the diff engine (`diff`), line→turn attribution (`blame`), and
//! status/restore (`restore`). `cli` glues them together.

pub mod blame;
pub mod cli;
pub mod diff;
pub mod glob;
pub mod object;
pub mod restore;
pub mod sha1;
pub mod snapshot;
pub mod store;
pub mod timefmt;
pub mod walker;

/// Crate version, single source of truth for `--version` and JSON envelopes.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
