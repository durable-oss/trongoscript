//! Hashable projections of [`Value`], so membership tests can be set
//! lookups instead of scans.
//!
//! `serde_json::Value` is only `PartialEq` — it holds an `f64`, so it
//! implements neither `Eq` nor `Hash` and cannot key a `HashSet`. Every
//! builtin that needs "have I seen this before?" therefore reached for a
//! linear scan with deep equality, which is O(n^2) over n distinct
//! elements. Because the whole pass happens inside a single builtin call,
//! the evaluator charges it one step: the cost is real but invisible to
//! any step budget.
//!
//! [`ValueKey`] projects *any* value, composite included, onto something
//! hashable. Building a key for a composite is O(size) — but so is a
//! single deep-equality comparison, and the scan it replaces performs one
//! per element already kept. Paying that cost once per element instead of
//! once per pair is what turns quadratic into linear.
//!
//! Keys are canonical for the values a script can produce: numbers use
//! their serialized form, object entries are visited in `serde_json`'s
//! sorted key order, and every variant is tagged so `1`, `"1"` and `true`
//! stay distinct.

use std::hash::{Hash, Hasher};

use crate::Value;

/// Type tags, hashed and compared ahead of a variant's payload so values
/// of different kinds never collide.
mod tag {
    pub const NULL: u8 = 0;
    pub const BOOL: u8 = 1;
    pub const NUMBER: u8 = 2;
    pub const STRING: u8 = 3;
    pub const ARRAY: u8 = 4;
    pub const OBJECT: u8 = 5;
}

/// A hashable, totally-comparable stand-in for a [`Value`].
///
/// Construct with [`ValueKey::new`]. Two keys are equal exactly when the
/// values they were built from are equal under `Value`'s own `PartialEq`,
/// with one deliberate exception: numbers compare by serialized form, so
/// `1.0` and `1` are the same key. A script cannot observe the difference
/// — the parser and [`crate::num_from_f64`] already normalize integral
/// floats to integers before a value reaches a builtin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueKey(Repr);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Repr {
    Null,
    Bool(bool),
    Number(String),
    Str(String),
    Array(Vec<ValueKey>),
    /// Entries in `serde_json`'s iteration order, which is sorted by key
    /// under the default `BTreeMap` backing and insertion-ordered under
    /// `preserve_order`. Sorting here keeps the key canonical either way.
    Object(Vec<(String, ValueKey)>),
}

impl ValueKey {
    /// Project `v` onto a hashable key.
    ///
    /// Cost is O(size of `v`), the same as one deep-equality comparison
    /// against it.
    pub fn new(v: &Value) -> Self {
        ValueKey(match v {
            Value::Null => Repr::Null,
            Value::Bool(b) => Repr::Bool(*b),
            Value::Number(n) => Repr::Number(n.to_string()),
            Value::String(s) => Repr::Str(s.clone()),
            Value::Array(items) => Repr::Array(items.iter().map(ValueKey::new).collect()),
            Value::Object(map) => {
                let mut entries: Vec<(String, ValueKey)> = map
                    .iter()
                    .map(|(k, val)| (k.clone(), ValueKey::new(val)))
                    .collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                Repr::Object(entries)
            }
        })
    }

    /// Number of nodes walked to build this key.
    ///
    /// Callers charge this against the evaluation budget: the work is
    /// proportional to it, and without a charge the step counter sees only
    /// the enclosing call. Counting the already-built key costs no extra
    /// traversal of the original value.
    pub fn node_count(&self) -> u64 {
        match &self.0 {
            Repr::Null | Repr::Bool(_) | Repr::Number(_) | Repr::Str(_) => 1,
            Repr::Array(items) => items
                .iter()
                .fold(1u64, |acc, k| acc.saturating_add(k.node_count())),
            Repr::Object(entries) => entries
                .iter()
                .fold(1u64, |acc, (_, k)| acc.saturating_add(k.node_count())),
        }
    }
}

impl Hash for ValueKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0 {
            Repr::Null => tag::NULL.hash(state),
            Repr::Bool(b) => {
                tag::BOOL.hash(state);
                b.hash(state);
            }
            Repr::Number(n) => {
                tag::NUMBER.hash(state);
                n.hash(state);
            }
            Repr::Str(s) => {
                tag::STRING.hash(state);
                s.hash(state);
            }
            Repr::Array(items) => {
                tag::ARRAY.hash(state);
                // Length is hashed so [[a] [b]] and [[a b]] differ even
                // when their flattened contents match.
                items.len().hash(state);
                for it in items {
                    it.hash(state);
                }
            }
            Repr::Object(entries) => {
                tag::OBJECT.hash(state);
                entries.len().hash(state);
                for (k, val) in entries {
                    k.hash(state);
                    val.hash(state);
                }
            }
        }
    }
}

/// A first-seen-order set of values, keyed by [`ValueKey`].
///
/// This is the shape every deduplicating builtin needs: ask whether a
/// value has been seen, remember it if not, and keep the originals in the
/// order they arrived.
#[derive(Default)]
pub struct ValueSet {
    seen: std::collections::HashSet<ValueKey>,
    /// Nodes walked building keys, for callers that charge for the work.
    nodes: u64,
}

impl ValueSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty set with room for `n` entries.
    pub fn with_capacity(n: usize) -> Self {
        ValueSet {
            seen: std::collections::HashSet::with_capacity(n),
            nodes: 0,
        }
    }

    /// Record `v`, returning `true` if it had not been seen before.
    pub fn insert(&mut self, v: &Value) -> bool {
        let key = ValueKey::new(v);
        self.nodes = self.nodes.saturating_add(key.node_count());
        self.seen.insert(key)
    }

    /// Whether `v` has been recorded.
    pub fn contains(&mut self, v: &Value) -> bool {
        let key = ValueKey::new(v);
        self.nodes = self.nodes.saturating_add(key.node_count());
        self.seen.contains(&key)
    }

    /// Build a set from every element of `items`.
    pub fn from_values<'a>(items: impl IntoIterator<Item = &'a Value>) -> Self {
        let mut set = ValueSet::new();
        for v in items {
            set.insert(v);
        }
        set
    }

    /// Total nodes walked building keys for this set, so a caller with
    /// access to the fuel meter can charge for work the step counter
    /// cannot see.
    pub fn nodes_walked(&self) -> u64 {
        self.nodes
    }
}

/// Remove duplicates from `items`, preserving first-seen order.
///
/// Linear in the total size of the input, against the O(n^2) of a scan
/// with deep equality. Returns the kept values and the node count walked,
/// for callers that charge for it.
pub fn dedup_values(items: &[Value]) -> (Vec<Value>, u64) {
    let mut set = ValueSet::with_capacity(items.len());
    let mut out: Vec<Value> = Vec::new();
    for v in items {
        if set.insert(v) {
            out.push(v.clone());
        }
    }
    (out, set.nodes_walked())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(v: serde_json::Value) -> ValueKey {
        ValueKey::new(&v)
    }

    #[test]
    fn scalars_of_different_types_are_distinct() {
        assert_ne!(key(json!(1)), key(json!("1")));
        assert_ne!(key(json!(1)), key(json!(true)));
        assert_ne!(key(json!(null)), key(json!(false)));
    }

    #[test]
    fn equal_composites_share_a_key() {
        assert_eq!(
            key(json!([1, [2, 3], {"a": 1}])),
            key(json!([1, [2, 3], {"a": 1}]))
        );
        assert_eq!(key(json!({"a": 1, "b": 2})), key(json!({"b": 2, "a": 1})));
    }

    #[test]
    fn differing_composites_do_not_collide() {
        assert_ne!(key(json!([[1], [2]])), key(json!([[1, 2]])));
        assert_ne!(key(json!({"a": 1})), key(json!({"b": 1})));
        assert_ne!(key(json!([1, 2])), key(json!([2, 1])));
    }

    #[test]
    fn nesting_depth_is_significant() {
        assert_ne!(key(json!([1])), key(json!([[1]])));
    }

    #[test]
    fn dedup_handles_composites() {
        let items = vec![
            json!([1]),
            json!([2]),
            json!([1]),
            json!({"a": 1}),
            json!({"a": 1}),
        ];
        let (out, _) = dedup_values(&items);
        assert_eq!(out, vec![json!([1]), json!([2]), json!({"a": 1})]);
    }

    #[test]
    fn dedup_preserves_first_seen_order() {
        let items = vec![json!(3), json!(1), json!(3), json!(2), json!(1)];
        let (out, _) = dedup_values(&items);
        assert_eq!(out, vec![json!(3), json!(1), json!(2)]);
    }

    #[test]
    fn node_count_tracks_size() {
        // One node per scalar, plus one for each container.
        assert_eq!(key(json!(1)).node_count(), 1);
        assert_eq!(key(json!([1, 2])).node_count(), 3);
        assert_eq!(key(json!({"a": [1, 2]})).node_count(), 4);
    }

    #[test]
    fn set_membership_matches_value_equality() {
        let mut set = ValueSet::from_values([&json!([1, 2]), &json!("x")]);
        assert!(set.contains(&json!([1, 2])));
        assert!(set.contains(&json!("x")));
        assert!(!set.contains(&json!([1, 3])));
        assert!(!set.contains(&json!("y")));
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
