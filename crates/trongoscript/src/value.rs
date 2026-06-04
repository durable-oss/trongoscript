//! The Trongoscript value type.
//!
//! Trongoscript values are `serde_json::Value` directly, which makes
//! conversion to/from JSON, YAML, and TOML trivial and keeps the public
//! surface small.

pub use serde_json::Value;

/// Render a `Value` as a human-readable string (used by the template mode
/// and string-coercion contexts).
pub fn to_display_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Truthiness rules: `nil` and `false` are falsy; everything else is truthy.
pub fn is_truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

/// Sentinel key used to encode a Trongoscript function value inside the
/// JSON-shaped [`Value`] tree. A function value is a one-key object
/// `{"__trongo_fn__": <handle>}` where `<handle>` indexes into
/// `Context::fns`. Encoding functions this way keeps `Value` compatible
/// with `serde_json::Value` (the public surface of the crate) while
/// allowing first-class closures internally.
pub const FN_TAG: &str = "__trongo_fn__";

/// Build a function-handle [`Value`] for a closure registered at `handle`.
pub fn make_fn_value(handle: usize) -> Value {
    let mut m = serde_json::Map::new();
    m.insert(FN_TAG.to_string(), Value::Number((handle as u64).into()));
    Value::Object(m)
}

/// If `v` is a function-handle value, return the registered handle.
pub fn fn_handle(v: &Value) -> Option<usize> {
    let obj = v.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    obj.get(FN_TAG).and_then(|h| h.as_u64()).map(|h| h as usize)
}

/// True if `v` is a function-handle value.
pub fn is_function(v: &Value) -> bool {
    fn_handle(v).is_some()
}

/// Convert an `f64` to a JSON number, preferring an integer representation
/// when the value has no fractional part and fits in `i64`. Returns
/// `Value::Null` for `NaN` and infinities (which JSON cannot represent).
pub fn num_from_f64(n: f64) -> Value {
    if !n.is_finite() {
        return Value::Null;
    }
    if n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
        return Value::Number((n as i64).into());
    }
    match serde_json::Number::from_f64(n) {
        Some(num) => Value::Number(num),
        None => Value::Null,
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.