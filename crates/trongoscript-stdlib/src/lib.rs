//! Trongoscript standard library.
//!
//! Builtins are registered via the [`inventory`] crate's compile-time
//! distributed slice. The evaluator looks up functions by name at runtime,
//! and host programs (CLI or library callers) can add their own builtins
//! the same way:
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
//! The set of "io" builtins (`include`, `read_file`, `list_files`) is
//! intentionally **not** in this crate — those need access to the
//! sandboxed FS root, which lives in the evaluator.

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

/// Look up a builtin by name, ignoring module visibility. O(N) over the
/// registry. Prefer [`find_builtin_for`] when visibility matters.
pub fn find_builtin(name: &str) -> Option<&'static Builtin> {
    inventory::iter::<Builtin>
        .into_iter()
        .find(|b| b.name == name)
}

/// Look up a builtin by name, honoring module visibility. Core builtins
/// (`module == None`) always resolve; module-scoped builtins resolve only
/// when `is_enabled(module_name)` returns true.
pub fn find_builtin_for(name: &str, is_enabled: impl Fn(&str) -> bool) -> Option<&'static Builtin> {
    inventory::iter::<Builtin>.into_iter().find(|b| {
        b.name == name
            && match b.module {
                None => true,
                Some(m) => is_enabled(m),
            }
    })
}

/// Iterate over every registered builtin.
pub fn iter_builtins() -> impl Iterator<Item = &'static Builtin> {
    inventory::iter::<Builtin>.into_iter()
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
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
