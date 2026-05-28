use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepositoryId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OperationId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ViewId(String);

impl RepositoryId {
    pub fn new() -> Self {
        Self(format!("repo_{}", unique_suffix()))
    }
}

impl Default for RepositoryId {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationId {
    pub fn new() -> Self {
        Self(format!("op_{}", unique_suffix()))
    }
}

impl Default for OperationId {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewId {
    pub fn new() -> Self {
        Self(format!("view_{}", unique_suffix()))
    }
}

impl Default for ViewId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RepositoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for ViewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    Initialized,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// Collision-resistant, time-sortable suffix. UUIDv7 carries a 48-bit millisecond
// timestamp in its high bits plus per-process monotonic + random low bits, so back-to-back
// mints are distinct and lexicographically ordered by creation time — unlike the previous
// `{millis}_{nanos}` scheme, which collided on same-nanosecond mints and was not fixed-width.
fn unique_suffix() -> String {
    Uuid::now_v7().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_prefixed_unique_and_sortable() {
        let a = OperationId::new().to_string();
        let b = OperationId::new().to_string();
        assert!(a.starts_with("op_"), "missing prefix: {a}");
        assert_ne!(a, b);
        // UUIDv7 is time-ordered and now_v7 is monotonic within a process, so an
        // earlier-minted id sorts before a later one.
        assert!(a < b, "ids not creation-ordered: {a} !< {b}");
        let suffix = a.strip_prefix("op_").unwrap();
        assert_eq!(suffix.len(), 32);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ten_thousand_mints_have_no_collisions() {
        let mut seen = HashSet::new();
        for _ in 0..10_000 {
            assert!(seen.insert(unique_suffix()), "duplicate id minted");
        }
        assert_eq!(seen.len(), 10_000);
    }

    #[test]
    fn distinct_id_types_carry_distinct_prefixes() {
        assert!(RepositoryId::new().to_string().starts_with("repo_"));
        assert!(ViewId::new().to_string().starts_with("view_"));
    }
}
