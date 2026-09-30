use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::types::{OclaError, OclaResult};

static NEXT_FORK_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SNAPSHOT_ID: AtomicU64 = AtomicU64::new(1);
static GLOBAL_CAPSULE_STORE: OnceLock<CapsuleStore> = OnceLock::new();

#[must_use]
pub fn global_capsule_store() -> &'static CapsuleStore {
    GLOBAL_CAPSULE_STORE.get_or_init(CapsuleStore::new)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delta {
    pub offset: usize,
    pub data: Vec<u8>,
}

/// A point-in-time snapshot of a capsule for rollback.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapsuleSnapshot {
    pub snapshot_id: String,
    pub capsule_ref: String,
    pub content: Vec<u8>,
    pub delta_count: usize,
    pub created_at_ms: u64,
}

/// A compressed change set for transferring a capsule between agents.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandoffDelta {
    pub from_ref: String,
    pub to_ref: String,
    pub operations: Vec<DeltaOp>,
    pub compressed_size: usize,
    pub original_size: usize,
}

/// A single operation in a [`HandoffDelta`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DeltaOp {
    Keep { offset: usize, len: usize },
    Insert { offset: usize, data: Vec<u8> },
    Delete { offset: usize, len: usize },
}

#[derive(Clone, Debug)]
pub struct CapsuleEntry {
    pub parent_ref: Option<String>,
    pub data: Vec<u8>,
    pub deltas: Vec<Delta>,
    pub budget_tokens: u64,
    pub created_at: Instant,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CapsuleStats {
    pub total_entries: usize,
    pub total_bytes: usize,
    pub max_depth: usize,
}

#[derive(Clone, Debug, Default)]
struct CapsuleStoreState {
    entries: HashMap<String, CapsuleEntry>,
    snapshots: Vec<CapsuleSnapshot>,
}

#[derive(Clone, Debug, Default)]
pub struct CapsuleStore {
    state: Arc<RwLock<CapsuleStoreState>>,
}

impl CapsuleStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, data: &[u8]) -> String {
        let capsule_ref = format!("capsule:{}", blake3::hash(data).to_hex());
        let entry = CapsuleEntry {
            parent_ref: None,
            data: data.to_vec(),
            deltas: Vec::new(),
            budget_tokens: 0,
            created_at: Instant::now(),
        };
        if let Ok(mut state) = self.state.write() {
            state.entries.insert(capsule_ref.clone(), entry);
        }
        capsule_ref
    }

    pub fn fork(&self, parent_ref: &str, budget_tokens: u64) -> OclaResult<String> {
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("capsule store lock poisoned"))?;
        if !state.entries.contains_key(parent_ref) {
            return Err(invalid(format!("unknown parent capsule: {parent_ref}")));
        }

        let fork_id = NEXT_FORK_ID.fetch_add(1, Ordering::Relaxed);
        let identity = format!("{parent_ref}\0{budget_tokens}\0{fork_id}");
        let capsule_ref = format!("capsule:{}", blake3::hash(identity.as_bytes()).to_hex());
        state.entries.insert(
            capsule_ref.clone(),
            CapsuleEntry {
                parent_ref: Some(parent_ref.to_string()),
                data: Vec::new(),
                deltas: Vec::new(),
                budget_tokens,
                created_at: Instant::now(),
            },
        );
        Ok(capsule_ref)
    }

    pub fn resolve(&self, capsule_ref: &str) -> OclaResult<Vec<u8>> {
        let state = self
            .state
            .read()
            .map_err(|_| invalid("capsule store lock poisoned"))?;
        resolve_entries(&state.entries, capsule_ref)
    }

    #[cfg(test)]
    pub(crate) fn budget_tokens(&self, capsule_ref: &str) -> OclaResult<u64> {
        let state = self
            .state
            .read()
            .map_err(|_| invalid("capsule store lock poisoned"))?;
        state
            .entries
            .get(capsule_ref)
            .map(|entry| entry.budget_tokens)
            .ok_or_else(|| invalid(format!("unknown capsule: {capsule_ref}")))
    }

    pub fn apply_delta(&self, capsule_ref: &str, delta: Delta) -> OclaResult<()> {
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("capsule store lock poisoned"))?;
        let current = resolve_entries(&state.entries, capsule_ref)?;
        let entry = state
            .entries
            .get_mut(capsule_ref)
            .ok_or_else(|| invalid(format!("unknown capsule: {capsule_ref}")))?;
        if entry.parent_ref.is_none() {
            return Err(invalid("deltas can only be applied to forked capsules"));
        }
        if delta.offset > current.len() {
            return Err(invalid("capsule delta starts beyond materialized content"));
        }
        entry.deltas.push(delta);
        Ok(())
    }

    pub fn merge_back(&self, child_ref: &str) -> OclaResult<()> {
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("capsule store lock poisoned"))?;
        resolve_entries(&state.entries, child_ref)?;
        let (parent_ref, deltas) = {
            let child = state
                .entries
                .get(child_ref)
                .ok_or_else(|| invalid(format!("unknown capsule: {child_ref}")))?;
            (
                child
                    .parent_ref
                    .clone()
                    .ok_or_else(|| invalid("root capsules cannot merge back"))?,
                child.deltas.clone(),
            )
        };
        let parent = state
            .entries
            .get_mut(&parent_ref)
            .ok_or_else(|| invalid(format!("unknown parent capsule: {parent_ref}")))?;
        parent.deltas.extend(deltas);
        state
            .entries
            .get_mut(child_ref)
            .ok_or_else(|| invalid(format!("unknown capsule: {child_ref}")))?
            .deltas
            .clear();
        Ok(())
    }

    /// Creates a snapshot of the current capsule state for potential rollback.
    pub fn snapshot(&self, capsule_ref: &str) -> OclaResult<CapsuleSnapshot> {
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("capsule store lock poisoned"))?;
        let content = resolve_entries(&state.entries, capsule_ref)?;
        let delta_count = state
            .entries
            .get(capsule_ref)
            .ok_or_else(|| invalid(format!("unknown capsule: {capsule_ref}")))?
            .deltas
            .len();
        let snapshot_number = NEXT_SNAPSHOT_ID.fetch_add(1, Ordering::Relaxed);
        let snapshot_id = format!(
            "snapshot:{}",
            blake3::hash(format!("{capsule_ref}\\0{snapshot_number}").as_bytes()).to_hex()
        );
        let snapshot = CapsuleSnapshot {
            snapshot_id,
            capsule_ref: capsule_ref.to_string(),
            content,
            delta_count,
            created_at_ms: unix_time_ms(),
        };
        state.snapshots.push(snapshot.clone());
        Ok(snapshot)
    }

    /// Rolls back a capsule to a previous snapshot.
    pub fn rollback(&self, capsule_ref: &str, snapshot_id: &str) -> OclaResult<()> {
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("capsule store lock poisoned"))?;
        let snapshot = state
            .snapshots
            .iter()
            .find(|snapshot| {
                snapshot.snapshot_id == snapshot_id && snapshot.capsule_ref == capsule_ref
            })
            .cloned()
            .ok_or_else(|| invalid(format!("unknown capsule snapshot: {snapshot_id}")))?;
        let entry = state
            .entries
            .get_mut(capsule_ref)
            .ok_or_else(|| invalid(format!("unknown capsule: {capsule_ref}")))?;
        entry.parent_ref = None;
        entry.data = snapshot.content;
        entry.deltas.clear();
        Ok(())
    }

    /// Computes a compressed delta between two capsule versions.
    pub fn compute_handoff_delta(&self, from_ref: &str, to_ref: &str) -> OclaResult<HandoffDelta> {
        let state = self
            .state
            .read()
            .map_err(|_| invalid("capsule store lock poisoned"))?;
        let from = resolve_entries(&state.entries, from_ref)?;
        let to = resolve_entries(&state.entries, to_ref)?;
        let operations = compute_delta_operations(&from, &to);
        Ok(HandoffDelta {
            from_ref: from_ref.to_string(),
            to_ref: to_ref.to_string(),
            compressed_size: handoff_delta_size(&operations),
            original_size: to.len(),
            operations,
        })
    }

    /// Applies a handoff delta to create a new capsule version.
    pub fn apply_handoff_delta(&self, base_ref: &str, delta: &HandoffDelta) -> OclaResult<String> {
        if delta.from_ref != base_ref {
            return Err(invalid("handoff delta base reference does not match"));
        }
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("capsule store lock poisoned"))?;
        let base = resolve_entries(&state.entries, base_ref)?;
        let content = apply_handoff_operations(&base, &delta.operations)?;
        let handoff_id = NEXT_FORK_ID.fetch_add(1, Ordering::Relaxed);
        let identity = format!(
            "handoff\\0{base_ref}\\0{}\\0{handoff_id}",
            blake3::hash(&content)
        );
        let capsule_ref = format!("capsule:{}", blake3::hash(identity.as_bytes()).to_hex());
        state.entries.insert(
            capsule_ref.clone(),
            CapsuleEntry {
                parent_ref: None,
                data: content,
                deltas: Vec::new(),
                budget_tokens: 0,
                created_at: Instant::now(),
            },
        );
        Ok(capsule_ref)
    }

    #[must_use]
    pub fn stats(&self) -> CapsuleStats {
        let Ok(state) = self.state.read() else {
            return CapsuleStats::default();
        };
        let total_bytes = state.entries.values().fold(0_usize, |total, entry| {
            total.saturating_add(entry.data.len()).saturating_add(
                entry
                    .deltas
                    .iter()
                    .map(|delta| delta.data.len())
                    .sum::<usize>(),
            )
        });
        let max_depth = state
            .entries
            .keys()
            .map(|capsule_ref| depth_of(&state.entries, capsule_ref))
            .max()
            .unwrap_or(0);
        CapsuleStats {
            total_entries: state.entries.len(),
            total_bytes,
            max_depth,
        }
    }
}

fn resolve_entries(
    entries: &HashMap<String, CapsuleEntry>,
    capsule_ref: &str,
) -> OclaResult<Vec<u8>> {
    let mut current = capsule_ref;
    let mut visited = HashSet::new();
    let mut layers = Vec::new();
    loop {
        if !visited.insert(current) {
            return Err(invalid("capsule parent cycle detected"));
        }
        let entry = entries
            .get(current)
            .ok_or_else(|| invalid(format!("unknown capsule: {capsule_ref}")))?;
        layers.push(entry.deltas.clone());
        if let Some(parent_ref) = entry.parent_ref.as_deref() {
            current = parent_ref;
        } else {
            let mut data = entry.data.clone();
            for layer in layers.iter().rev() {
                for delta in layer {
                    apply_patch(&mut data, delta)?;
                }
            }
            return Ok(data);
        }
    }
}

fn apply_patch(data: &mut Vec<u8>, delta: &Delta) -> OclaResult<()> {
    let end = delta
        .offset
        .checked_add(delta.data.len())
        .ok_or_else(|| invalid("capsule delta range overflow"))?;
    if delta.offset > data.len() {
        return Err(invalid("capsule delta starts beyond materialized content"));
    }
    if end > data.len() {
        data.resize(end, 0);
    }
    data[delta.offset..end].copy_from_slice(&delta.data);
    Ok(())
}

fn compute_delta_operations(from: &[u8], to: &[u8]) -> Vec<DeltaOp> {
    if from == to {
        return Vec::new();
    }

    let prefix_len = from
        .iter()
        .zip(to)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix_len = from[prefix_len..]
        .iter()
        .rev()
        .zip(to[prefix_len..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let from_middle_end = from.len() - suffix_len;
    let to_middle_end = to.len() - suffix_len;
    let mut operations = Vec::with_capacity(4);

    if prefix_len > 0 {
        operations.push(DeltaOp::Keep {
            offset: 0,
            len: prefix_len,
        });
    }
    if from_middle_end > prefix_len {
        operations.push(DeltaOp::Delete {
            offset: prefix_len,
            len: from_middle_end - prefix_len,
        });
    }
    if to_middle_end > prefix_len {
        operations.push(DeltaOp::Insert {
            offset: from_middle_end,
            data: to[prefix_len..to_middle_end].to_vec(),
        });
    }
    if suffix_len > 0 {
        operations.push(DeltaOp::Keep {
            offset: from_middle_end,
            len: suffix_len,
        });
    }
    operations
}

fn apply_handoff_operations(base: &[u8], operations: &[DeltaOp]) -> OclaResult<Vec<u8>> {
    if operations.is_empty() {
        return Ok(base.to_vec());
    }

    let mut cursor = 0;
    let mut output = Vec::with_capacity(base.len());
    for operation in operations {
        match operation {
            DeltaOp::Keep { offset, len } => {
                validate_handoff_offset(*offset, cursor)?;
                let end = cursor
                    .checked_add(*len)
                    .ok_or_else(|| invalid("handoff keep range overflow"))?;
                let bytes = base
                    .get(cursor..end)
                    .ok_or_else(|| invalid("handoff keep extends beyond base content"))?;
                output.extend_from_slice(bytes);
                cursor = end;
            }
            DeltaOp::Insert { offset, data } => {
                validate_handoff_offset(*offset, cursor)?;
                output.extend_from_slice(data);
            }
            DeltaOp::Delete { offset, len } => {
                validate_handoff_offset(*offset, cursor)?;
                cursor = cursor
                    .checked_add(*len)
                    .ok_or_else(|| invalid("handoff delete range overflow"))?;
                if cursor > base.len() {
                    return Err(invalid("handoff delete extends beyond base content"));
                }
            }
        }
    }
    if cursor != base.len() {
        return Err(invalid("handoff delta does not consume base content"));
    }
    Ok(output)
}

fn validate_handoff_offset(offset: usize, cursor: usize) -> OclaResult<()> {
    if offset != cursor {
        return Err(invalid("handoff operation offset is out of order"));
    }
    Ok(())
}

fn handoff_delta_size(operations: &[DeltaOp]) -> usize {
    operations.iter().fold(0_usize, |size, operation| {
        let operation_size = match operation {
            DeltaOp::Keep { .. } | DeltaOp::Delete { .. } => 2 * std::mem::size_of::<usize>(),
            DeltaOp::Insert { data, .. } => 2 * std::mem::size_of::<usize>() + data.len(),
        };
        size.saturating_add(operation_size)
    })
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn depth_of(entries: &HashMap<String, CapsuleEntry>, capsule_ref: &str) -> usize {
    let mut depth = 0;
    let mut current = capsule_ref;
    let mut visited = HashSet::new();
    while visited.insert(current) {
        let Some(entry) = entries.get(current) else {
            break;
        };
        let Some(parent_ref) = entry.parent_ref.as_deref() else {
            break;
        };
        depth += 1;
        current = parent_ref;
    }
    depth
}

fn invalid(message: impl Into<String>) -> OclaError {
    OclaError::InvalidRequest(message.into())
}

#[cfg(test)]
mod tests {
    use super::{CapsuleStore, Delta, DeltaOp, global_capsule_store};

    fn test_delta() -> Delta {
        Delta {
            offset: 1,
            data: vec![b'a'],
        }
    }

    #[test]
    fn global_store_registers_capsule() {
        let capsule_ref = global_capsule_store().register(b"global capsule");
        assert_eq!(
            global_capsule_store()
                .resolve(&capsule_ref)
                .expect("global resolves"),
            b"global capsule"
        );
    }

    #[test]
    fn register_resolves_original_data() {
        let store = CapsuleStore::new();
        let capsule_ref = store.register(b"hello");
        assert_eq!(
            store.resolve(&capsule_ref).expect("root resolves"),
            b"hello"
        );
    }
    #[test]
    fn fork_resolves_parent_data() {
        let store = CapsuleStore::new();
        let parent_ref = store.register(b"hello");
        let child_ref = store.fork(&parent_ref, 100).expect("fork succeeds");
        assert_eq!(store.resolve(&child_ref).expect("child resolves"), b"hello");
        assert_eq!(store.budget_tokens(&child_ref).expect("budget exists"), 100);
    }
    #[test]
    fn fork_delta_resolves_patched_data() {
        let store = CapsuleStore::new();
        let parent_ref = store.register(b"hello");
        let child_ref = store.fork(&parent_ref, 100).expect("fork succeeds");
        store
            .apply_delta(&child_ref, test_delta())
            .expect("delta applies");
        assert_eq!(store.resolve(&child_ref).expect("child resolves"), b"hallo");
    }

    #[test]
    fn later_overlapping_delta_wins_within_layer() {
        let store = CapsuleStore::new();
        let parent_ref = store.register(b"hello");
        let fork_ref = store.fork(&parent_ref, 100).expect("fork succeeds");

        store
            .apply_delta(
                &fork_ref,
                Delta {
                    offset: 1,
                    data: vec![b'a'],
                },
            )
            .expect("first delta applies");
        store
            .apply_delta(
                &fork_ref,
                Delta {
                    offset: 1,
                    data: vec![b'u'],
                },
            )
            .expect("second delta applies");

        assert_eq!(store.resolve(&fork_ref).expect("fork resolves"), b"hullo");
    }

    #[test]
    fn child_delta_overrides_parent_delta() {
        let store = CapsuleStore::new();
        let root_ref = store.register(b"hello");
        let parent_ref = store.fork(&root_ref, 100).expect("parent fork succeeds");
        store
            .apply_delta(
                &parent_ref,
                Delta {
                    offset: 1,
                    data: vec![b'a'],
                },
            )
            .expect("parent delta applies");
        let child_ref = store.fork(&parent_ref, 100).expect("child fork succeeds");
        store
            .apply_delta(
                &child_ref,
                Delta {
                    offset: 1,
                    data: vec![b'u'],
                },
            )
            .expect("child delta applies");

        assert_eq!(store.resolve(&child_ref).expect("child resolves"), b"hullo");
    }

    #[test]
    fn merge_back_preserves_chronological_delta_order() {
        let store = CapsuleStore::new();
        let root_ref = store.register(b"hello");
        let parent_ref = store.fork(&root_ref, 100).expect("parent fork succeeds");
        store
            .apply_delta(
                &parent_ref,
                Delta {
                    offset: 1,
                    data: vec![b'a'],
                },
            )
            .expect("parent delta applies");
        let child_ref = store.fork(&parent_ref, 100).expect("child fork succeeds");
        store
            .apply_delta(
                &child_ref,
                Delta {
                    offset: 1,
                    data: vec![b'u'],
                },
            )
            .expect("child delta applies");

        store.merge_back(&child_ref).expect("merge succeeds");

        assert_eq!(
            store.resolve(&parent_ref).expect("parent resolves"),
            b"hullo"
        );
        assert_eq!(store.resolve(&child_ref).expect("child resolves"), b"hullo");
    }
    #[test]
    fn merge_back_projects_deltas_to_parent() {
        let store = CapsuleStore::new();
        let parent_ref = store.register(b"hello");
        let child_ref = store.fork(&parent_ref, 100).expect("fork succeeds");
        store
            .apply_delta(&child_ref, test_delta())
            .expect("delta applies");
        store.merge_back(&child_ref).expect("merge succeeds");
        assert_eq!(
            store.resolve(&parent_ref).expect("parent resolves"),
            b"hallo"
        );
        assert_eq!(store.resolve(&child_ref).expect("child resolves"), b"hallo");
    }
    #[test]
    fn stats_report_entries_storage_and_depth() {
        let store = CapsuleStore::new();
        let parent_ref = store.register(b"hello");
        let child_ref = store.fork(&parent_ref, 100).expect("fork succeeds");
        store
            .apply_delta(&child_ref, test_delta())
            .expect("delta applies");
        let stats = store.stats();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.total_bytes, 6);
        assert_eq!(stats.max_depth, 1);
    }

    #[test]
    fn snapshot_rollback_restores_snapshot_content() {
        let store = CapsuleStore::new();
        let root_ref = store.register(b"hello");
        let child_ref = store.fork(&root_ref, 100).expect("fork succeeds");
        store
            .apply_delta(
                &child_ref,
                Delta {
                    offset: 1,
                    data: vec![b'a'],
                },
            )
            .expect("first delta applies");
        let snapshot = store.snapshot(&child_ref).expect("snapshot succeeds");
        store
            .apply_delta(
                &child_ref,
                Delta {
                    offset: 1,
                    data: vec![b'u'],
                },
            )
            .expect("second delta applies");

        store
            .rollback(&child_ref, &snapshot.snapshot_id)
            .expect("rollback succeeds");

        assert_eq!(store.resolve(&child_ref).expect("child resolves"), b"hallo");
    }

    #[test]
    fn handoff_delta_for_identical_capsules_is_empty() {
        let store = CapsuleStore::new();
        let first_ref = store.register(b"unchanged");
        let second_ref = store.register(b"unchanged");

        let delta = store
            .compute_handoff_delta(&first_ref, &second_ref)
            .expect("delta computes");

        assert!(delta.operations.is_empty());
        assert_eq!(delta.compressed_size, 0);
    }

    #[test]
    fn handoff_delta_for_different_capsules_has_insert_and_delete() {
        let store = CapsuleStore::new();
        let from_ref = store.register(b"hello");
        let to_ref = store.register(b"halo!");

        let delta = store
            .compute_handoff_delta(&from_ref, &to_ref)
            .expect("delta computes");

        assert!(
            delta
                .operations
                .iter()
                .any(|operation| matches!(operation, DeltaOp::Insert { .. }))
        );
        assert!(
            delta
                .operations
                .iter()
                .any(|operation| matches!(operation, DeltaOp::Delete { .. }))
        );
    }

    #[test]
    fn apply_handoff_delta_round_trip_matches_target() {
        let store = CapsuleStore::new();
        let from_ref = store.register(b"goodbye");
        let to_ref = store.register(b"good day!");
        let delta = store
            .compute_handoff_delta(&from_ref, &to_ref)
            .expect("delta computes");

        let applied_ref = store
            .apply_handoff_delta(&from_ref, &delta)
            .expect("delta applies");

        assert_eq!(
            store.resolve(&applied_ref).expect("applied resolves"),
            store.resolve(&to_ref).expect("target resolves")
        );
    }

    #[test]
    fn rollback_with_invalid_snapshot_id_errors() {
        let store = CapsuleStore::new();
        let capsule_ref = store.register(b"hello");

        assert!(store.rollback(&capsule_ref, "snapshot:missing").is_err());
    }
}
