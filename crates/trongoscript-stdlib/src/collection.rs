//! Collection (list / map) builtins.

use crate::{
    expect_array, expect_object, inventory, num_from_f64, Builtin, BuiltinError, BuiltinResult,
    Value,
};

fn count(args: &[Value]) -> BuiltinResult {
    let n = match &args[0] {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        Value::String(s) => s.chars().count(),
        Value::Null => 0,
        other => {
            return Err(BuiltinError::TypeError {
                function: "count".into(),
                expected: "list, map, string or nil".into(),
                got: crate::type_name(other).into(),
            })
        }
    };
    Ok(Value::Number((n as i64).into()))
}

fn first(args: &[Value]) -> BuiltinResult {
    let a = expect_array("first", &args[0])?;
    Ok(a.first().cloned().unwrap_or(Value::Null))
}

fn last(args: &[Value]) -> BuiltinResult {
    let a = expect_array("last", &args[0])?;
    Ok(a.last().cloned().unwrap_or(Value::Null))
}

fn rest(args: &[Value]) -> BuiltinResult {
    let a = expect_array("rest", &args[0])?;
    if a.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    Ok(Value::Array(a[1..].to_vec()))
}

fn nth(args: &[Value]) -> BuiltinResult {
    let a = expect_array("nth", &args[0])?;
    let i = match args[1].as_i64() {
        Some(i) => i,
        None => {
            return Err(BuiltinError::TypeError {
                function: "nth".into(),
                expected: "integer".into(),
                got: crate::type_name(&args[1]).into(),
            })
        }
    };
    if i < 0 || (i as usize) >= a.len() {
        return Err(BuiltinError::IndexOutOfBounds {
            function: "nth".into(),
            index: i,
            len: a.len(),
        });
    }
    Ok(a[i as usize].clone())
}

fn list(args: &[Value]) -> BuiltinResult {
    Ok(Value::Array(args.to_vec()))
}

fn cons(args: &[Value]) -> BuiltinResult {
    let tail = expect_array("cons", &args[1])?;
    let mut out = Vec::with_capacity(tail.len() + 1);
    out.push(args[0].clone());
    out.extend(tail.iter().cloned());
    Ok(Value::Array(out))
}

fn conj(args: &[Value]) -> BuiltinResult {
    // Registered variadic, which means `min_args = 0` — the evaluator's
    // arity check cannot reject `(conj)`, so the body must. Without this,
    // the indexing below panics and takes the process down, and it is
    // reachable from `embed` as well as `eval`.
    if args.is_empty() {
        return Err(BuiltinError::ArgError {
            function: "conj".into(),
            message: "expected (conj coll v ...) with at least 1 argument".into(),
        });
    }
    let a = expect_array("conj", &args[0])?;
    let mut out = a.clone();
    for v in &args[1..] {
        out.push(v.clone());
    }
    Ok(Value::Array(out))
}

fn concat(args: &[Value]) -> BuiltinResult {
    let mut out = Vec::new();
    for a in args {
        let arr = expect_array("concat", a)?;
        out.extend(arr.iter().cloned());
    }
    Ok(Value::Array(out))
}

fn reverse(args: &[Value]) -> BuiltinResult {
    let a = expect_array("reverse", &args[0])?;
    let mut out = a.clone();
    out.reverse();
    Ok(Value::Array(out))
}

fn range(args: &[Value]) -> BuiltinResult {
    let (start, end, step) = match args.len() {
        1 => (0f64, args[0].as_f64().unwrap_or(0.0), 1f64),
        2 => (
            args[0].as_f64().unwrap_or(0.0),
            args[1].as_f64().unwrap_or(0.0),
            1f64,
        ),
        3 => (
            args[0].as_f64().unwrap_or(0.0),
            args[1].as_f64().unwrap_or(0.0),
            args[2].as_f64().unwrap_or(1.0),
        ),
        _ => {
            return Err(BuiltinError::ArgError {
                function: "range".into(),
                message: format!("expected 1-3 args, got {}", args.len()),
            })
        }
    };
    if step == 0.0 {
        return Err(BuiltinError::ArgError {
            function: "range".into(),
            message: "step must be non-zero".into(),
        });
    }
    let mut out = Vec::new();
    let mut x = start;
    if step > 0.0 {
        while x < end {
            out.push(num_from_f64(x));
            x += step;
        }
    } else {
        while x > end {
            out.push(num_from_f64(x));
            x += step;
        }
    }
    Ok(Value::Array(out))
}

fn keys(args: &[Value]) -> BuiltinResult {
    let m = expect_object("keys", &args[0])?;
    Ok(Value::Array(
        m.keys().map(|k| Value::String(k.clone())).collect(),
    ))
}

fn values(args: &[Value]) -> BuiltinResult {
    let m = expect_object("values", &args[0])?;
    Ok(Value::Array(m.values().cloned().collect()))
}

fn get(args: &[Value]) -> BuiltinResult {
    let default = if args.len() == 3 {
        args[2].clone()
    } else {
        Value::Null
    };
    match &args[0] {
        Value::Object(m) => {
            let k = match &args[1] {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            Ok(m.get(&k).cloned().unwrap_or(default))
        }
        Value::Array(a) => {
            let i = args[1].as_i64().ok_or_else(|| BuiltinError::TypeError {
                function: "get".into(),
                expected: "integer index for list".into(),
                got: crate::type_name(&args[1]).into(),
            })?;
            if i < 0 || (i as usize) >= a.len() {
                Ok(default)
            } else {
                Ok(a[i as usize].clone())
            }
        }
        Value::Null => Ok(default),
        other => Err(BuiltinError::TypeError {
            function: "get".into(),
            expected: "map or list".into(),
            got: crate::type_name(other).into(),
        }),
    }
}

fn assoc(args: &[Value]) -> BuiltinResult {
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return Err(BuiltinError::ArgError {
            function: "assoc".into(),
            message: "expected (assoc m k v ...) with odd argument count >= 3".into(),
        });
    }
    let mut m = expect_object("assoc", &args[0])?.clone();
    let mut i = 1;
    while i < args.len() {
        let k = match &args[i] {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        m.insert(k, args[i + 1].clone());
        i += 2;
    }
    Ok(Value::Object(m))
}

fn merge(args: &[Value]) -> BuiltinResult {
    let mut out = serde_json::Map::new();
    for a in args {
        let m = expect_object("merge", a)?;
        for (k, v) in m {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(out))
}

/// How deeply nested maps may be merged before `deep-merge` gives up.
///
/// The merge recurses once per level of nesting shared by both sides, on
/// the native stack, where the evaluator's call-depth guard cannot see
/// it. A script can build a map nested a million levels deep with a
/// `reduce`, so the depth has to be bounded here. Real config overlays
/// are a handful of levels deep.
const MAX_DEEP_MERGE_DEPTH: usize = 256;

/// Like `merge`, but where both sides hold a map under the same key, the
/// two maps are merged recursively instead of the right one replacing
/// the left. Anything else, lists included, is replaced by the rightmost
/// value, so a list in an overlay replaces the base list rather than
/// being appended to it.
fn deep_merge(args: &[Value]) -> BuiltinResult {
    let mut out = serde_json::Map::new();
    for a in args {
        let m = expect_object("deep-merge", a)?;
        deep_merge_into(&mut out, m, 0)?;
    }
    Ok(Value::Object(out))
}

fn deep_merge_into(
    dst: &mut serde_json::Map<String, Value>,
    src: &serde_json::Map<String, Value>,
    depth: usize,
) -> Result<(), BuiltinError> {
    if depth >= MAX_DEEP_MERGE_DEPTH {
        return Err(BuiltinError::Failed {
            function: "deep-merge".into(),
            message: format!("maps nested more than {MAX_DEEP_MERGE_DEPTH} levels deep"),
        });
    }
    for (k, v) in src {
        match (dst.get_mut(k), v) {
            (Some(Value::Object(d)), Value::Object(s)) => deep_merge_into(d, s, depth + 1)?,
            _ => {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    Ok(())
}

fn contains_q(args: &[Value]) -> BuiltinResult {
    match &args[0] {
        Value::Array(a) => Ok(Value::Bool(a.contains(&args[1]))),
        Value::Object(m) => {
            let k = match &args[1] {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            Ok(Value::Bool(m.contains_key(&k)))
        }
        _ => Err(BuiltinError::TypeError {
            function: "contains?".into(),
            expected: "list or map".into(),
            got: crate::type_name(&args[0]).into(),
        }),
    }
}

/// Every builtin this module defines.
///
/// This slice, not the `inventory::submit!` calls below, is what makes
/// the module work on `wasm32-unknown-unknown`: inventory registers
/// through a link section, and nothing in a wasm build references these
/// functions by name, so the linker drops them before the constructors
/// that would have registered them ever run. Naming them in an ordinary
/// `static` keeps them alive and makes registration a plain array walk.
///
/// The `inventory::submit!` calls stay so a host can still add builtins
/// that way on a native target.
pub static BUILTINS: &[Builtin] = &[
    Builtin::new("count", 1, 1, count),
    Builtin::new("first", 1, 1, first),
    Builtin::new("last", 1, 1, last),
    Builtin::new("rest", 1, 1, rest),
    Builtin::new("nth", 2, 2, nth),
    Builtin::variadic("list", list),
    Builtin::new("cons", 2, 2, cons),
    Builtin::variadic("conj", conj),
    Builtin::variadic("concat", concat),
    Builtin::new("reverse", 1, 1, reverse),
    Builtin::new("range", 1, 3, range),
    Builtin::new("keys", 1, 1, keys),
    Builtin::new("values", 1, 1, values),
    Builtin::new("get", 2, 3, get),
    Builtin::variadic("assoc", assoc),
    Builtin::variadic("merge", merge),
    Builtin::variadic("deep-merge", deep_merge),
    Builtin::new("contains?", 2, 2, contains_q),
];

inventory::submit! { Builtin::new("count",     1, 1, count) }
inventory::submit! { Builtin::new("first",     1, 1, first) }
inventory::submit! { Builtin::new("last",      1, 1, last) }
inventory::submit! { Builtin::new("rest",      1, 1, rest) }
inventory::submit! { Builtin::new("nth",       2, 2, nth) }
inventory::submit! { Builtin::variadic("list", list) }
inventory::submit! { Builtin::new("cons",      2, 2, cons) }
inventory::submit! { Builtin::variadic("conj", conj) }
inventory::submit! { Builtin::variadic("concat", concat) }
inventory::submit! { Builtin::new("reverse",   1, 1, reverse) }
inventory::submit! { Builtin::new("range",     1, 3, range) }
inventory::submit! { Builtin::new("keys",      1, 1, keys) }
inventory::submit! { Builtin::new("values",    1, 1, values) }
inventory::submit! { Builtin::new("get",       2, 3, get) }
inventory::submit! { Builtin::variadic("assoc", assoc) }
inventory::submit! { Builtin::variadic("merge", merge) }
inventory::submit! { Builtin::variadic("deep-merge", deep_merge) }
inventory::submit! { Builtin::new("contains?", 2, 2, contains_q) }

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deep_merge_recurses_into_shared_maps() {
        let base = json!({"db": {"host": "localhost", "port": 5432}, "debug": false});
        let over = json!({"db": {"host": "prod.internal"}, "debug": true});
        assert_eq!(
            deep_merge(&[base, over]).unwrap(),
            json!({"db": {"host": "prod.internal", "port": 5432}, "debug": true})
        );
    }

    #[test]
    fn deep_merge_replaces_lists_and_mismatched_types() {
        let base = json!({"tags": [1, 2], "a": {"x": 1}, "b": 1});
        let over = json!({"tags": [3], "a": 5, "b": {"y": 2}});
        assert_eq!(
            deep_merge(&[base, over]).unwrap(),
            json!({"tags": [3], "a": 5, "b": {"y": 2}})
        );
    }

    #[test]
    fn deep_merge_applies_left_to_right_across_many_maps() {
        let out = deep_merge(&[
            json!({"a": {"b": {"c": 1, "d": 1}}}),
            json!({"a": {"b": {"c": 2}}}),
            json!({"a": {"b": {"e": 3}}}),
        ])
        .unwrap();
        assert_eq!(out, json!({"a": {"b": {"c": 2, "d": 1, "e": 3}}}));
    }

    #[test]
    fn deep_merge_handles_zero_and_one_argument() {
        assert_eq!(deep_merge(&[]).unwrap(), json!({}));
        assert_eq!(deep_merge(&[json!({"a": 1})]).unwrap(), json!({"a": 1}));
    }

    #[test]
    fn deep_merge_rejects_non_maps() {
        assert!(deep_merge(&[json!({}), json!([1])]).is_err());
    }

    #[test]
    fn deep_merge_bounds_nesting_depth() {
        fn nest(levels: usize) -> Value {
            let mut v = json!(1);
            for _ in 0..levels {
                v = json!({ "k": v });
            }
            v
        }
        let ok = nest(MAX_DEEP_MERGE_DEPTH);
        assert!(deep_merge(&[ok.clone(), ok]).is_ok());
        let deep = nest(MAX_DEEP_MERGE_DEPTH + 1);
        assert!(deep_merge(&[deep.clone(), deep]).is_err());
    }
}
