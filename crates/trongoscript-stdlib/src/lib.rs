//! Trongoscript standard library.
//!
//! The evaluator looks builtins up by name at runtime, against two
//! registries that [`iter_builtins`] presents as one.
//!
//! **This crate's own builtins** live in a `BUILTINS` slice per module,
//! gathered into `CRATE_BUILTINS`. That is not stylistic: `inventory`
//! registers through a link section populated by constructors, and on
//! `wasm32-unknown-unknown` nothing references these functions by name, so
//! the linker drops them as dead code before the constructors that would
//! have registered them ever run. The symptom is every document failing with
//! "undefined function `str`" on a target where the build succeeded and
//! nothing looks wrong. Naming them in ordinary statics keeps them alive
//! everywhere.
//!
//! **Host builtins** still come in through [`inventory`], which remains the
//! extension point for a program embedding the language:
//!
//! ```ignore
//! use trongoscript_stdlib::{Builtin, BuiltinResult, Value};
//!
//! fn my_hash(args: &[Value]) -> BuiltinResult {
//!     // ...
//! }
//!
//! inventory::submit! {
//!     Builtin::new("my_hash", 1, 1, my_hash)
//! }
//! ```
//!
//! A host targeting wasm should expect the same dead-code problem with its
//! own submissions, and pass its builtins to the evaluator directly instead.
//!
//! Two sets of functions are deliberately *not* here. The "io" builtins
//! (`include`, `read_file`, `list_files`) need the sandboxed filesystem
//! root, which lives in the evaluator. So do the higher-order forms (`map`,
//! `filter`, `reduce`), which take unevaluated expressions and so are
//! dispatched by the evaluator rather than registered as values.

pub use inventory;
pub use serde_json::Value;

use std::fmt;
use thiserror::Error;

pub mod collection;
pub mod extra_collection;
pub mod extra_string;
pub mod finance;
pub mod logic;
pub mod math;
pub mod numeric;
pub mod serde_fmt;
pub mod string;
pub mod trongo_fmt;

/// Kani proof harnesses and the pure kernels they verify. Present in normal
/// builds only as a small set of `pub(crate)` helpers plus their drift
/// tests; the proofs themselves are `cfg(kani)`.
mod verification;

/// Convert an `f64` to a JSON number, preferring an integer representation
/// when the value has no fractional part and fits in `i64`. Returns
/// `Value::Null` for `NaN` and infinities (which JSON cannot represent).
///
/// The upper bound check is strict (`<`, not `<=`): `i64::MAX as f64`
/// rounds up to exactly `2^63`, one past the actual maximum `i64`, so a
/// non-strict comparison would let `2^63` through and silently saturate it
/// to `i64::MAX` on the cast below.
pub fn num_from_f64(n: f64) -> Value {
    if !n.is_finite() {
        return Value::Null;
    }
    const I64_MAX_AS_F64_UPPER_BOUND: f64 = 9_223_372_036_854_775_808.0; // 2^63
    if n.fract() == 0.0 && n >= i64::MIN as f64 && n < I64_MAX_AS_F64_UPPER_BOUND {
        return Value::Number((n as i64).into());
    }
    match serde_json::Number::from_f64(n) {
        Some(num) => Value::Number(num),
        None => Value::Null,
    }
}

/// Result type for builtin functions.
pub type BuiltinResult = Result<Value, BuiltinError>;

/// Errors a builtin can raise.
#[derive(Error, Debug, Clone, PartialEq)]
pub enum BuiltinError {
    #[error("type error in `{function}`: expected {expected}, got {got}")]
    TypeError {
        function: String,
        expected: String,
        got: String,
    },
    #[error("argument error in `{function}`: {message}")]
    ArgError { function: String, message: String },
    #[error("`{function}` index {index} out of bounds (len {len})")]
    IndexOutOfBounds {
        function: String,
        index: i64,
        len: usize,
    },
    #[error("`{function}` failed: {message}")]
    Failed { function: String, message: String },
}

/// A function callable from Trongoscript.
///
/// Stored as a compile-time inventory entry. Each `Builtin` has a static
/// name, arity bounds, and an optional `module` tag. Builtins whose
/// `module` is `None` are always callable; module-tagged builtins are
/// only callable after the program has executed `(use <module>)`.
pub struct Builtin {
    pub name: &'static str,
    pub module: Option<&'static str>,
    pub min_args: usize,
    pub max_args: usize,
    pub func: fn(&[Value]) -> BuiltinResult,
}

impl Builtin {
    /// Always-available core builtin.
    pub const fn new(
        name: &'static str,
        min_args: usize,
        max_args: usize,
        func: fn(&[Value]) -> BuiltinResult,
    ) -> Self {
        Self {
            name,
            module: None,
            min_args,
            max_args,
            func,
        }
    }

    /// Always-available variadic core builtin.
    pub const fn variadic(name: &'static str, func: fn(&[Value]) -> BuiltinResult) -> Self {
        Self {
            name,
            module: None,
            min_args: 0,
            max_args: usize::MAX,
            func,
        }
    }

    /// Module-scoped builtin: only callable after `(use <module>)`.
    pub const fn in_module(
        module: &'static str,
        name: &'static str,
        min_args: usize,
        max_args: usize,
        func: fn(&[Value]) -> BuiltinResult,
    ) -> Self {
        Self {
            name,
            module: Some(module),
            min_args,
            max_args,
            func,
        }
    }

    /// Module-scoped variadic builtin.
    pub const fn in_module_variadic(
        module: &'static str,
        name: &'static str,
        func: fn(&[Value]) -> BuiltinResult,
    ) -> Self {
        Self {
            name,
            module: Some(module),
            min_args: 0,
            max_args: usize::MAX,
            func,
        }
    }
}

impl fmt::Debug for Builtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builtin")
            .field("name", &self.name)
            .field("min_args", &self.min_args)
            .field("max_args", &self.max_args)
            .finish()
    }
}

inventory::collect!(Builtin);

/// Every builtin this crate ships, as plain static slices.
///
/// This, rather than the `inventory::submit!` calls beside each definition,
/// is what makes the standard library work on `wasm32-unknown-unknown`.
/// Inventory registers through a link section populated by constructors, and
/// in a wasm build nothing references these functions by name — so the
/// linker drops them as dead code before the constructors that would have
/// registered them ever run, and every document fails with "undefined
/// function `str`" on a target where nothing looks wrong.
///
/// Naming them in ordinary statics keeps them alive on every target and
/// makes registration an array walk. The inventory path stays alongside it,
/// because a *host* adding its own builtins with `inventory::submit!` is a
/// documented extension point and native builds still honor it.
static CRATE_BUILTINS: &[&[Builtin]] = &[
    collection::BUILTINS,
    extra_collection::BUILTINS,
    extra_string::BUILTINS,
    finance::BUILTINS,
    logic::BUILTINS,
    math::BUILTINS,
    numeric::BUILTINS,
    serde_fmt::BUILTINS,
    string::BUILTINS,
];

/// Look up a builtin by name, ignoring module visibility. O(N) over the
/// registry. Prefer [`find_builtin_for`] when visibility matters.
pub fn find_builtin(name: &str) -> Option<&'static Builtin> {
    iter_builtins().find(|b| b.name == name)
}

/// Look up a builtin by name, honoring module visibility. Core builtins
/// (`module == None`) always resolve; module-scoped builtins resolve only
/// when `is_enabled(module_name)` returns true.
pub fn find_builtin_for(name: &str, is_enabled: impl Fn(&str) -> bool) -> Option<&'static Builtin> {
    iter_builtins().find(|b| {
        b.name == name
            && match b.module {
                None => true,
                Some(m) => is_enabled(m),
            }
    })
}

/// Iterate over every registered builtin: this crate's, then any a host
/// submitted through inventory.
///
/// The crate's own builtins are yielded first, so a host submitting a name
/// that already exists shadows nothing — lookups find the standard one. That
/// is the conservative choice; a host that wants to *replace* a builtin
/// should say so through a mechanism that can't happen by accident.
///
/// On a native target the crate's builtins appear in the inventory registry
/// too, so this deduplicates by identity to keep the sequence honest.
pub fn iter_builtins() -> impl Iterator<Item = &'static Builtin> {
    let own = CRATE_BUILTINS.iter().copied().flatten();
    let submitted = inventory::iter::<Builtin>
        .into_iter()
        .filter(|b| !is_own(b));
    own.chain(submitted)
}

/// Is this inventory entry one of the crate's own, registered twice?
fn is_own(candidate: &'static Builtin) -> bool {
    CRATE_BUILTINS
        .iter()
        .copied()
        .flatten()
        .any(|b| b.name == candidate.name && b.module == candidate.module)
}

/// Iterate over every module name referenced by a registered builtin.
pub fn known_modules() -> impl Iterator<Item = &'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for b in iter_builtins() {
        if let Some(m) = b.module {
            if !seen.contains(&m) {
                seen.push(m);
            }
        }
    }
    seen.into_iter()
}

// ---------------------------------------------------------------------------
// Helpers for builtin authors
// ---------------------------------------------------------------------------

pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "list",
        Value::Object(_) => "map",
    }
}

pub fn expect_string<'a>(fname: &str, v: &'a Value) -> Result<&'a str, BuiltinError> {
    v.as_str().ok_or_else(|| BuiltinError::TypeError {
        function: fname.into(),
        expected: "string".into(),
        got: type_name(v).into(),
    })
}

pub fn expect_number(fname: &str, v: &Value) -> Result<f64, BuiltinError> {
    v.as_f64().ok_or_else(|| BuiltinError::TypeError {
        function: fname.into(),
        expected: "number".into(),
        got: type_name(v).into(),
    })
}

pub fn expect_array<'a>(fname: &str, v: &'a Value) -> Result<&'a Vec<Value>, BuiltinError> {
    v.as_array().ok_or_else(|| BuiltinError::TypeError {
        function: fname.into(),
        expected: "list".into(),
        got: type_name(v).into(),
    })
}

pub fn expect_object<'a>(
    fname: &str,
    v: &'a Value,
) -> Result<&'a serde_json::Map<String, Value>, BuiltinError> {
    v.as_object().ok_or_else(|| BuiltinError::TypeError {
        function: fname.into(),
        expected: "map".into(),
        got: type_name(v).into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_upcase() {
        let b = find_builtin("upcase").expect("upcase should be registered");
        assert_eq!(b.name, "upcase");
        let r = (b.func)(&[Value::String("hi".into())]).unwrap();
        assert_eq!(r, Value::String("HI".into()));
    }

    #[test]
    fn registry_lists_many_builtins() {
        let count = iter_builtins().count();
        assert!(count > 10, "expected >10 builtins, got {count}");
    }

    #[test]
    fn every_module_slice_is_reachable_without_inventory() {
        // The point of `CRATE_BUILTINS` is that the standard library works on
        // targets where inventory's link-section registration does not — on
        // wasm32 the constructors never run, the functions are dropped as
        // dead code, and every document fails with "undefined function
        // `str`". Walking the slices directly is what a wasm build does, so
        // check the slices rather than the merged iterator.
        let names: Vec<&str> = CRATE_BUILTINS
            .iter()
            .copied()
            .flatten()
            .map(|b| b.name)
            .collect();

        // Names this crate owns. `map` and the other higher-order forms are
        // deliberately absent: the evaluator dispatches those by `match`
        // rather than registering them, so they were never affected.
        for expected in ["str", "upcase", "abs", "not", "count"] {
            assert!(
                names.contains(&expected),
                "`{expected}` is missing from the static slices, so it would \
                 not exist on wasm32"
            );
        }
        assert!(
            names.len() >= 100,
            "expected the whole stdlib, got {}",
            names.len()
        );
    }

    #[test]
    fn nothing_is_registered_through_inventory_alone() {
        // The guard that generalizes the wasm32 fix. Adding a module and
        // wiring up only its `inventory::submit!` calls compiles, passes
        // every native test, and ships a build where those builtins simply
        // do not exist in a browser. Since this crate's own builtins are
        // always in a slice, anything reaching the inventory registry from
        // *inside* this crate is a module someone forgot to add to
        // `CRATE_BUILTINS`.
        let missing: Vec<&str> = inventory::iter::<Builtin>
            .into_iter()
            .filter(|b| !is_own(b))
            .map(|b| b.name)
            .collect();

        assert!(
            missing.is_empty(),
            "these builtins exist only in the inventory registry, so they \
             would be missing on wasm32 — add their module's `BUILTINS` \
             slice to `CRATE_BUILTINS`: {missing:?}"
        );
    }

    #[test]
    fn a_builtin_is_not_listed_twice_when_inventory_also_has_it() {
        // Natively, every one of the crate's builtins is registered both ways.
        // The dedup in `iter_builtins` is what keeps `known_modules` and any
        // caller counting builtins from seeing each one twice.
        let mut names: Vec<&str> = iter_builtins().map(|b| b.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(total, names.len(), "a builtin appeared more than once");
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
