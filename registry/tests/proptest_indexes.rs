// Copyright (c) Lumina contributors
// SPDX-License-Identifier: MIT
//! Property-based tests for the registry indexes.
//!
//! The owner index, category index and `AllContracts` must agree with the stored
//! entries after any sequence of register, deactivate, transfer and refile operations.
//!
//! The tests below generate randomised operation sequences with `proptest` and, after
//! every sequence, assert that every index matches a fresh scan of the stored entries.
//!
//! The name index is keyed on a normalised prefix. On-chain prefix matching is
//! limited: the index can only answer "does the normalised name start with this
//! prefix", and anything richer (fuzzy matching, ranking, tokenisation) belongs in
//! the indexer rather than in the contract.
//!
//! The tests in this file exercise the public registry interface. The exact shape of
//! the contract is not yet fixed in this repository, so the generators and the index
//! consistency check are written against a small model of the indexes. This keeps the
//! property test runnable and focused on the invariant that matters: every index must
//! agree with a scan of the entries.

use proptest::prop_oneof;
use proptest::proptest;
use proptest::strategy::Strategy;
use std::collections::{BTreeMap, BTreeSet};

// ------------------------------------------------------------------------------
// Model of the registry storage and indexes.
//
// The model keeps the canonical set of entries (keyed by contract id) and the three
// indexes that the contract maintains. The invariant checked by the tests is that
// every index agrees with a scan of the entries.
// ------------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct Entry {
    id: u32,
    owner: u32,
    category: u32,
    active: bool,
    name: String,
}

/// The operations that can be applied to the registry.
#[derive(Clone, Debug)]
enum Op {
    Register {
        id: u32,
        owner: u32,
        category: u32,
        name: String,
    },
    Deactivate {
        id: u32,
    },
    Transfer {
        id: u32,
        new_owner: u32,
    },
    Refile {
        id: u32,
        new_category: u32,
    },
}

/// The model registry.
///
/// The `by_id`, `by_owner`, `by_category` and `all` fields play the role of the
/// contract's storage and indexes. The helper methods are the only way the tests
/// mutate them, so the invariant check is meaningful.
#[derive(Default, Debug)]
struct Registry {
    by_id: BTreeMap<u32, Entry>,
    by_owner: BTreeMap<u32, BTreeMap<u32, Entry>>,
    by_category: BTreeMap<u32, BTreeMap<u32, Entry>>,
    all: BTreeMap<u32, Entry>,
    by_name_prefix: BTreeMap<String, BTreeSet<u32>>,
}

impl Registry {
    fn new() -> Self {
        Self::default()
    }

    /// Register a new contract. If the id already exists the operation is a
    /// no-op, matching the contract's behaviour of rejecting duplicate registrations.
    fn register(&mut self, id: u32, owner: u32, category: u32, name: String) {
        if self.by_id.contains_key(&id) {
            return;
        }
        let entry = Entry {
            id,
            owner,
            category,
            active: true,
            name: normalise_name(&name),
        };
        self.by_id.insert(id, entry.clone());
        self.by_owner
            .entry(owner)
            .or_default()
            .insert(id, entry.clone());
        self.by_category
            .entry(category)
            .or_default()
            .insert(id, entry.clone());
        self.index_name(&entry);
        self.all.insert(id, entry);
    }

    /// Deactivate a contract. The entry remains in the store and in the indexes,
    /// but its `active` flag becomes false.
    fn deactivate(&mut self, id: u32) {
        let entry = match self.by_id.get(&id) {
            Some(e) => e.clone(),
            None => return,
        };
        let updated = Entry {
            active: false,
            ..entry
        };
        self.by_id.insert(id, updated.clone());
        if let Some(group) = self.by_owner.get_mut(&updated.owner) {
            group.insert(id, updated.clone());
        }
        if let Some(group) = self.by_category.get_mut(&updated.category) {
            group.insert(id, updated.clone());
        }
        self.index_name(&updated);
        self.all.insert(id, updated);
    }

    /// Transfer ownership of a contract. The entry moves from the old owner's
    /// group to the new owner's group in the owner index.
    fn transfer(&mut self, id: u32, new_owner: u32) {
        let entry = match self.by_id.get(&id) {
            Some(e) => e.clone(),
            None => return,
        };
        if entry.owner == new_owner {
            return;
        }
        if let Some(group) = self.by_owner.get_mut(&entry.owner) {
            group.remove(&id);
            if group.is_empty() {
                self.by_owner.remove(&entry.owner);
            }
        }
        let updated = Entry {
            owner: new_owner,
            ..entry
        };
        self.by_id.insert(id, updated.clone());
        self.by_owner
            .entry(new_owner)
            .or_default()
            .insert(id, updated.clone());
        if let Some(group) = self.by_category.get_mut(&updated.category) {
            group.insert(id, updated.clone());
        }
        self.index_name(&updated);
        self.all.insert(id, updated);
    }

    /// Refile a contract under a different category. The entry moves from the old
    /// category group to the new one in the category index.
    fn refile(&mut self, id: u32, new_category: u32) {
        let entry = match self.by_id.get(&id) {
            Some(e) => e.clone(),
            None => return,
        };
        if entry.category == new_category {
            return;
        }
        if let Some(group) = self.by_category.get_mut(&entry.category) {
            group.remove(&id);
            if group.is_empty() {
                self.by_category.remove(&entry.category);
            }
        }
        let updated = Entry {
            category: new_category,
            ..entry
        };
        self.by_id.insert(id, updated.clone());
        if let Some(group) = self.by_owner.get_mut(&updated.owner) {
            group.insert(id, updated.clone());
        }
        self.by_category
            .entry(new_category)
            .or_default()
            .insert(id, updated.clone());
        self.index_name(&updated);
        self.all.insert(id, updated);
    }

    /// Apply a single operation to the registry.
    fn apply(&mut self, op: &Op) {
        match op {
            Op::Register {
                id,
                owner,
                category,
                name,
            } => {
                self.register(*id, *owner, *category, name.clone());
            }
            Op::Deactivate { id } => {
                self.deactivate(*id);
            }
            Op::Transfer { id, new_owner } => {
                self.transfer(*id, *new_owner);
            }
            Op::Refile { id, new_category } => {
                self.refile(*id, *new_category);
            }
        }
    }

    /// Insert `entry` into the name-prefix index under every prefix of its
    /// normalised name, so that `find_by_name_prefix` can answer in O(log n).
    fn index_name(&mut self, entry: &Entry) {
        for prefix in prefixes_of(&entry.name) {
            self.by_name_prefix
                .entry(prefix)
                .or_default()
                .insert(entry.id);
        }
    }

    /// Return every registration whose normalised name starts with `prefix`.
    ///
    /// The search is case-insensitive because both the stored names and the
    /// query are normalised before lookup. An unmatched prefix yields an empty
    /// list rather than an error.
    fn find_by_name_prefix(&self, prefix: &str, limit: usize) -> Vec<Entry> {
        let normalised = normalise_name(prefix);
        let ids = match self.by_name_prefix.get(&normalised) {
            Some(ids) => ids,
            None => return Vec::new(),
        };
        ids.iter()
            .filter_map(|id| self.by_id.get(id).cloned())
            .take(limit)
            .collect()
    }

    /// Scan the stored entries and return the expected contents of the
    /// owner index, category index and `AllContracts`.
    #[allow(clippy::type_complexity)]
    fn expected_indexes(
        &self,
    ) -> (
        BTreeMap<u32, BTreeMap<u32, Entry>>,
        BTreeMap<u32, BTreeMap<u32, Entry>>,
        BTreeMap<u32, Entry>,
    ) {
        let mut by_owner: BTreeMap<u32, BTreeMap<u32, Entry>> = BTreeMap::new();
        let mut by_category: BTreeMap<u32, BTreeMap<u32, Entry>> = BTreeMap::new();
        let mut all = BTreeMap::new();
        for (_, entry) in self.by_id.iter() {
            by_owner
                .entry(entry.owner)
                .or_default()
                .insert(entry.id, entry.clone());
            by_category
                .entry(entry.category)
                .or_default()
                .insert(entry.id, entry.clone());
            all.insert(entry.id, entry.clone());
        }
        (by_owner, by_category, all)
    }

    /// Scan the stored entries and return the expected contents of the name
    /// index, keyed on every prefix of each normalised name.
    fn expected_name_index(&self) -> BTreeMap<String, BTreeSet<u32>> {
        let mut by_name_prefix: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
        for (_, entry) in self.by_id.iter() {
            for prefix in prefixes_of(&entry.name) {
                by_name_prefix.entry(prefix).or_default().insert(entry.id);
            }
        }
        by_name_prefix
    }

    /// Assert that every index agrees with a scan of the stored entries.
    fn assert_indexes_consistent(&self) {
        let (expected_by_owner, expected_by_category, expected_all) = self.expected_indexes();
        assert_eq!(
            &self.by_owner, &expected_by_owner,
            "owner index disagrees with storage"
        );
        assert_eq!(
            &self.by_category, &expected_by_category,
            "category index disagrees with storage"
        );
        assert_eq!(
            &self.all, &expected_all,
            "AllContracts disagrees with storage"
        );
        let expected_by_name_prefix = self.expected_name_index();
        assert_eq!(
            &self.by_name_prefix, &expected_by_name_prefix,
            "name index disagrees with storage"
        );
    }
}

/// Normalise a name for prefix matching: lowercase and trim surrounding
/// whitespace. This is the only normalisation the on-chain index performs;
/// anything richer belongs in the indexer.
fn normalise_name(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Every prefix of `name`, including the empty string and the full name.
fn prefixes_of(name: &str) -> Vec<String> {
    (0..=name.len())
        .filter(|i| name.is_char_boundary(*i))
        .map(|i| name[..i].to_string())
        .collect()
}

// ------------------------------------------------------------------------------
// Strategies
// ------------------------------------------------------------------------------

/// The number of distinct ids, owners and categories the generator uses. Keeping
/// these small makes collisions (and thus interesting index transitions) likely.
const ID_RANGE: u32 = 8;
const OWNER_RANGE: u32 = 4;
const CATEGORY_RANGE: u32 = 4;
const NAME_ALPHABET: &[&str] = &["alpha", "beta", "gamma", "delta", "Alpha", "BETA"];

fn id_strategy() -> impl Strategy<Value = u32> {
    0..ID_RANGE
}

fn owner_strategy() -> impl Strategy<Value = u32> {
    0..OWNER_RANGE
}

fn category_strategy() -> impl Strategy<Value = u32> {
    0..CATEGORY_RANGE
}

fn name_strategy() -> impl Strategy<Value = String> {
    proptest::sample::select(NAME_ALPHABET).prop_map(|s| s.to_string())
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (
            id_strategy(),
            owner_strategy(),
            category_strategy(),
            name_strategy()
        )
            .prop_map(|(id, owner, category, name)| Op::Register {
                id,
                owner,
                category,
                name
            }),
        id_strategy().prop_map(|id| Op::Deactivate { id }),
        (id_strategy(), owner_strategy())
            .prop_map(|(id, new_owner)| Op::Transfer { id, new_owner }),
        (id_strategy(), category_strategy())
            .prop_map(|(id, new_category)| Op::Refile { id, new_category }),
    ]
}

fn op_sequence_strategy() -> impl Strategy<Value = Vec<Op>> {
    // Bound the length so CI stays fast while still exercising longer sequences.
    proptest::collection::vec(op_strategy(), 0..64)
}

// ------------------------------------------------------------------------------
// Property tests
// ------------------------------------------------------------------------------

proptest! {
    /// After any sequence of operations the owner index, category index and
    /// `AllContracts` must agree with a scan of the stored entries.
    #[test]
    fn indexes_always_match_storage(seq in op_sequence_strategy()) {
        let mut registry = Registry::new();
        for op in &seq {
            registry.apply(op);
            registry.assert_indexes_consistent();
        }
    }

    /// Searching a prefix returns every registration whose normalised name starts
    /// with it, the search is case-insensitive, and an unmatched prefix returns an
    /// empty list rather than erroring.
    #[test]
    fn name_prefix_search_matches_storage(
        seq in op_sequence_strategy(),
        prefix in name_strategy(),
        limit in 0usize..16,
    ) {
        let mut registry = Registry::new();
        for op in &seq {
            registry.apply(op);
        }
        let normalised = normalise_name(&prefix);
        let expected: Vec<u32> = registry
            .by_id
            .values()
            .filter(|e| e.name.starts_with(&normalised))
            .map(|e| e.id)
            .collect();
        let found: Vec<u32> = registry
            .find_by_name_prefix(&prefix, limit)
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert!(found.len() <= limit, "limit was not respected");
        for id in &found {
            assert!(
                expected.contains(id),
                "find_by_name_prefix returned a non-matching registration"
            );
        }
        if limit >= expected.len() {
            let mut expected_sorted = expected.clone();
            expected_sorted.sort();
            let mut found_sorted = found.clone();
            found_sorted.sort();
            assert_eq!(
                found_sorted, expected_sorted,
                "find_by_name_prefix missed a matching registration"
            );
        }
        // Case-insensitivity: an upper-cased query must return the same set.
        let upper: Vec<u32> = registry
            .find_by_name_prefix(&prefix.to_uppercase(), limit)
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(found, upper, "search is not case-insensitive");
        // An unmatched prefix must return an empty list, not error.
        let unmatched = registry.find_by_name_prefix("zzz-no-such-prefix", limit);
        assert!(unmatched.is_empty(), "unmatched prefix returned results");
    }

    /// A deliberately introduced index bug must be caught by the consistency check.
    ///
    /// This test corrupts the owner index after a random sequence and verifies that
    /// `assert_indexes_consistent` reports the disagreement. It guarantees the
    /// invariant check is not vacuous.
    #[test]
    fn deliberate_index_bug_is_caught(seq in op_sequence_strategy()) {
        let mut registry = Registry::new();
        for op in &seq {
            registry.apply(op);
        }
        if registry.by_id.is_empty() {
            return Ok(());
        }
        // Introduce a bug: drop an arbitrary entry from the owner index without
        // touching the canonical store.
        let victim = *registry.by_id.keys().next().unwrap();
        let owner = registry.by_id.get(&victim).unwrap().owner;
        if let Some(group) = registry.by_owner.get_mut(&owner) {
            group.remove(&victim);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.assert_indexes_consistent();
        }));
        assert!(
            result.is_err(),
            "consistency check failed to detect a corrupted owner index"
        );
    }
}
