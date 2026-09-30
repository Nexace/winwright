//! Per-session element references (spec §9).
//!
//! Refs are numbered per session, never globally, and an element that reappears in a later
//! snapshot keeps its ref so diffs stay readable. Entries remember enough identity to detect
//! staleness and re-resolve; they never hold COM objects, only worker slot keys.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use winwright_contracts::backend::{ElementIdentity, ElementKey};
use winwright_contracts::geometry::PhysicalRect;
use winwright_contracts::ids::{format_element_ref, parse_element_ref};
use winwright_contracts::{WinwrightError, WinwrightResult};

#[derive(Clone, Debug)]
pub struct RefEntry {
    pub number: u64,
    pub key: ElementKey,
    pub identity: ElementIdentity,
    pub bounds: Option<PhysicalRect>,
    /// `Button "Save"`, for error messages.
    pub label: String,
    pub generation: u64,
    pub last_seen: Instant,
}

impl RefEntry {
    pub fn reference(&self) -> String {
        format_element_ref(self.number)
    }

    /// Past its TTL: callers must revalidate before acting on it.
    pub fn is_expired(&self, now: Instant, ttl: Duration) -> bool {
        now.saturating_duration_since(self.last_seen) > ttl
    }
}

pub struct Upserted {
    pub number: u64,
    /// Slot the entry held before, now unreferenced and safe to release.
    pub replaced_key: Option<ElementKey>,
}

#[derive(Default)]
pub struct RefTable {
    next: u64,
    entries: HashMap<u64, RefEntry>,
    by_runtime: HashMap<(u32, Vec<i32>), u64>,
}

impl RefTable {
    pub const MAX_ENTRIES: usize = 20_000;

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Assigns (or reuses) the ref for an element seen in snapshot `generation`.
    pub fn upsert(
        &mut self,
        identity: ElementIdentity,
        key: ElementKey,
        bounds: Option<PhysicalRect>,
        label: String,
        generation: u64,
        now: Instant,
    ) -> Upserted {
        let runtime_key = (identity.process_id, identity.runtime_id.clone());
        if !identity.runtime_id.is_empty()
            && let Some(&number) = self.by_runtime.get(&runtime_key)
            && let Some(entry) = self.entries.get_mut(&number)
            && entry.identity.same_element(&identity)
        {
            let replaced = (entry.key != key).then_some(entry.key);
            entry.key = key;
            entry.identity = identity;
            entry.bounds = bounds;
            entry.label = label;
            entry.generation = generation;
            entry.last_seen = now;
            return Upserted {
                number,
                replaced_key: replaced,
            };
        }

        self.next += 1;
        let number = self.next;
        if !identity.runtime_id.is_empty() {
            // A recreated element with a recycled runtime id gets a fresh ref.
            self.by_runtime.insert(runtime_key, number);
        }
        self.entries.insert(
            number,
            RefEntry {
                number,
                key,
                identity,
                bounds,
                label,
                generation,
                last_seen: now,
            },
        );
        Upserted {
            number,
            replaced_key: None,
        }
    }

    pub fn get(&self, reference: &str) -> WinwrightResult<&RefEntry> {
        let Some(number) = parse_element_ref(reference) else {
            return Err(WinwrightError::invalid(format!(
                "{reference:?} is not an element ref (expected e.g. \"e12\")"
            )));
        };
        if let Some(entry) = self.entries.get(&number) {
            return Ok(entry);
        }
        if number <= self.next {
            Err(WinwrightError::ElementStale {
                reference: reference.to_owned(),
                reason: "the ref was evicted; take a new snapshot".into(),
            })
        } else {
            Err(WinwrightError::invalid(format!(
                "{reference} was never issued in this session"
            )))
        }
    }

    /// Validates the ref still belongs to the live worker.
    pub fn get_live(&self, reference: &str, worker_epoch: u64) -> WinwrightResult<&RefEntry> {
        let entry = self.get(reference)?;
        if entry.key.worker_epoch != worker_epoch {
            return Err(WinwrightError::ElementStale {
                reference: reference.to_owned(),
                reason: "the automation worker restarted since this ref was issued".into(),
            });
        }
        Ok(entry)
    }

    /// Drops refs not seen for `keep_generations` snapshots and older than `ttl`, plus refs
    /// from dead worker epochs, and enforces [`Self::MAX_ENTRIES`]. Returns slots to release.
    pub fn prune(
        &mut self,
        generation: u64,
        keep_generations: u64,
        now: Instant,
        ttl: Duration,
        worker_epoch: u64,
    ) -> Vec<ElementKey> {
        let min_generation = generation.saturating_sub(keep_generations);
        let mut doomed: Vec<u64> = self
            .entries
            .values()
            .filter(|e| {
                e.key.worker_epoch != worker_epoch
                    || (e.generation < min_generation && e.is_expired(now, ttl))
            })
            .map(|e| e.number)
            .collect();

        let overflow = (self.entries.len() - doomed.len()).saturating_sub(Self::MAX_ENTRIES);
        if overflow > 0 {
            let mut survivors: Vec<&RefEntry> = self
                .entries
                .values()
                .filter(|e| !doomed.contains(&e.number))
                .collect();
            survivors.sort_by_key(|e| (e.generation, e.number));
            doomed.extend(survivors.iter().take(overflow).map(|e| e.number));
        }

        let mut released = Vec::with_capacity(doomed.len());
        for number in doomed {
            if let Some(entry) = self.entries.remove(&number) {
                let runtime_key = (entry.identity.process_id, entry.identity.runtime_id);
                if self.by_runtime.get(&runtime_key) == Some(&number) {
                    self.by_runtime.remove(&runtime_key);
                }
                if entry.key.worker_epoch == worker_epoch {
                    released.push(entry.key);
                }
            }
        }
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(runtime: &[i32], automation_id: &str) -> ElementIdentity {
        ElementIdentity {
            process_id: 10,
            runtime_id: runtime.to_vec(),
            control_type_id: 50000,
            automation_id: automation_id.into(),
            name: "Save".into(),
            class_name: "Button".into(),
            framework_id: "Win32".into(),
            ancestor_fingerprint: 7,
        }
    }

    fn key(slot: u64) -> ElementKey {
        ElementKey {
            worker_epoch: 1,
            slot,
        }
    }

    #[test]
    fn same_element_keeps_its_ref_across_snapshots() {
        let mut t = RefTable::default();
        let now = Instant::now();
        let a = t.upsert(identity(&[42, 1], "save"), key(1), None, "b".into(), 1, now);
        let b = t.upsert(
            identity(&[42, 2], "cancel"),
            key(2),
            None,
            "b".into(),
            1,
            now,
        );
        assert_eq!((a.number, b.number), (1, 2));
        let again = t.upsert(identity(&[42, 1], "save"), key(9), None, "b".into(), 2, now);
        assert_eq!(again.number, 1);
        assert_eq!(again.replaced_key, Some(key(1)));
        assert_eq!(t.get("e1").unwrap().key, key(9));
    }

    #[test]
    fn recycled_runtime_id_with_different_identity_gets_new_ref() {
        let mut t = RefTable::default();
        let now = Instant::now();
        t.upsert(identity(&[5], "old"), key(1), None, "x".into(), 1, now);
        let n = t.upsert(identity(&[5], "new"), key(2), None, "x".into(), 2, now);
        assert_eq!(n.number, 2);
        assert!(n.replaced_key.is_none());
    }

    #[test]
    fn elements_without_runtime_ids_are_never_merged() {
        let mut t = RefTable::default();
        let now = Instant::now();
        let a = t.upsert(identity(&[], "a"), key(1), None, "x".into(), 1, now);
        let b = t.upsert(identity(&[], "a"), key(2), None, "x".into(), 2, now);
        assert_ne!(a.number, b.number);
    }

    #[test]
    fn lookup_errors_are_typed() {
        let mut t = RefTable::default();
        let now = Instant::now();
        t.upsert(identity(&[1], "a"), key(1), None, "x".into(), 1, now);
        assert_eq!(
            t.get("nonsense").unwrap_err().code().as_str(),
            "INVALID_REQUEST"
        );
        assert_eq!(t.get("e99").unwrap_err().code().as_str(), "INVALID_REQUEST");
        assert!(t.get_live("e1", 1).is_ok());
        assert_eq!(
            t.get_live("e1", 2).unwrap_err().code().as_str(),
            "ELEMENT_STALE"
        );
    }

    #[test]
    fn prune_evicts_old_expired_and_dead_epoch_refs() {
        let mut t = RefTable::default();
        let start = Instant::now();
        t.upsert(identity(&[1], "old"), key(1), None, "x".into(), 1, start);
        t.upsert(identity(&[2], "fresh"), key(2), None, "x".into(), 5, start);
        let dead = ElementKey {
            worker_epoch: 0,
            slot: 3,
        };
        t.upsert(identity(&[3], "dead"), dead, None, "x".into(), 5, start);

        let later = start + Duration::from_secs(60);
        let released = t.prune(5, 2, later, Duration::from_secs(30), 1);
        assert_eq!(
            released,
            vec![key(1)],
            "dead-epoch slots are not released to the new worker"
        );
        assert_eq!(t.len(), 1);
        assert_eq!(t.get("e1").unwrap_err().code().as_str(), "ELEMENT_STALE");
        assert!(t.get("e2").is_ok());
        assert_eq!(t.get("e3").unwrap_err().code().as_str(), "ELEMENT_STALE");
    }

    #[test]
    fn recent_generations_survive_ttl() {
        let mut t = RefTable::default();
        let start = Instant::now();
        t.upsert(identity(&[1], "a"), key(1), None, "x".into(), 4, start);
        let released = t.prune(
            5,
            2,
            start + Duration::from_secs(600),
            Duration::from_secs(30),
            1,
        );
        assert!(released.is_empty());
    }
}
