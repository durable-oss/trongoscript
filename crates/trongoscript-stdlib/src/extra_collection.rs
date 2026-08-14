//! `collections` module — additional list/map operations.
//!
//! Activated by `(use collections)`. Pure data transformations.

use crate::{
    expect_array, expect_number, expect_string, inventory, num_from_f64, type_name, Builtin,
    BuiltinError, BuiltinResult, Value,
};

const MODULE: &str = "collections";

/// Total ordering for `Value`s that allows mixed homogeneous arrays.
/// Returns `None` for cross-type comparisons.
fn value_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .and_then(|f| y.as_f64().map(|g| (f, g)))
            .and_then(|(f, g)| f.partial_cmp(&g)),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        _ => None,
    }
}

fn sort(args: &[Value]) -> BuiltinResult {
    let a = expect_array("sort", &args[0])?;
    let mut out = a.clone();
    // Detect cross-type early.
    let mut cmp_err: Option<BuiltinError> = None;
    out.sort_by(|x, y| match value_cmp(x, y) {
        Some(ord) => ord,
        None => {
            if cmp_err.is_none() {
                cmp_err = Some(BuiltinError::Failed {
                    function: "sort".into(),
                    message: format!("cannot compare {} with {}", type_name(x), type_name(y)),
                });
            }
            std::cmp::Ordering::Equal
        }
    });
    if let Some(e) = cmp_err {
        return Err(e);
    }
    Ok(Value::Array(out))
}

/// Remove duplicates, preserving first-seen order.
///
/// The obvious implementation compares each element against everything
/// kept so far, which is O(n^2) on distinct input — and since the whole
/// pass is a single builtin call, it costs one evaluation step, so no
/// step budget can bound it. 80k distinct elements took six seconds and
/// reported five steps.
///
/// Every element therefore goes through [`crate::value_key`], including
/// composites. An earlier version hashed only scalars, on the grounds
/// that keying a list means walking it — but so does a single deep
/// comparison against one, and the scan does that once per element
/// already kept. Wrapping each element in a one-element list was enough
/// to restore the original quadratic behaviour.
fn uniq(args: &[Value]) -> BuiltinResult {
    let a = expect_array("uniq", &args[0])?;
    let (out, _) = crate::value_key::dedup_values(a);
    Ok(Value::Array(out))
}

fn sum(args: &[Value]) -> BuiltinResult {
    let a = expect_array("sum", &args[0])?;
    let mut acc = 0.0f64;
    for v in a {
        match v.as_f64() {
            Some(n) => acc += n,
            None => {
                return Err(BuiltinError::TypeError {
                    function: "sum".into(),
                    expected: "list of numbers".into(),
                    got: format!("list containing {}", type_name(v)),
                })
            }
        }
    }
    Ok(num_from_f64(acc))
}

fn avg(args: &[Value]) -> BuiltinResult {
    let a = expect_array("avg", &args[0])?;
    if a.is_empty() {
        return Ok(Value::Null);
    }
    let mut acc = 0.0f64;
    for v in a {
        match v.as_f64() {
            Some(n) => acc += n,
            None => {
                return Err(BuiltinError::TypeError {
                    function: "avg".into(),
                    expected: "list of numbers".into(),
                    got: format!("list containing {}", type_name(v)),
                })
            }
        }
    }
    Ok(num_from_f64(acc / a.len() as f64))
}

fn flatten(args: &[Value]) -> BuiltinResult {
    let a = expect_array("flatten", &args[0])?;
    let mut out: Vec<Value> = Vec::new();
    for v in a {
        match v {
            Value::Array(inner) => out.extend(inner.iter().cloned()),
            other => out.push(other.clone()),
        }
    }
    Ok(Value::Array(out))
}

fn partition(args: &[Value]) -> BuiltinResult {
    let a = expect_array("partition", &args[0])?;
    let n = expect_number("partition", &args[1])? as i64;
    if n <= 0 {
        return Err(BuiltinError::ArgError {
            function: "partition".into(),
            message: "chunk size must be positive".into(),
        });
    }
    let n = n as usize;
    let out: Vec<Value> = a.chunks(n).map(|c| Value::Array(c.to_vec())).collect();
    Ok(Value::Array(out))
}

fn zip(args: &[Value]) -> BuiltinResult {
    let a = expect_array("zip", &args[0])?;
    let b = expect_array("zip", &args[1])?;
    let n = a.len().min(b.len());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(Value::Array(vec![a[i].clone(), b[i].clone()]));
    }
    Ok(Value::Array(out))
}

fn take(args: &[Value]) -> BuiltinResult {
    let n = expect_number("take", &args[0])? as i64;
    let a = expect_array("take", &args[1])?;
    let n = n.max(0) as usize;
    Ok(Value::Array(a.iter().take(n).cloned().collect()))
}

fn drop_n(args: &[Value]) -> BuiltinResult {
    let n = expect_number("drop", &args[0])? as i64;
    let a = expect_array("drop", &args[1])?;
    let n = n.max(0) as usize;
    Ok(Value::Array(a.iter().skip(n).cloned().collect()))
}

fn frequencies(args: &[Value]) -> BuiltinResult {
    let a = expect_array("frequencies", &args[0])?;
    let mut out = serde_json::Map::new();
    for v in a {
        let key = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "nil".to_string(),
            other => {
                return Err(BuiltinError::TypeError {
                    function: "frequencies".into(),
                    expected: "list of scalars".into(),
                    got: format!("list containing {}", type_name(other)),
                })
            }
        };
        let entry = out.entry(key).or_insert_with(|| Value::Number(0i64.into()));
        let cur = entry.as_i64().unwrap_or(0);
        *entry = Value::Number((cur + 1).into());
    }
    Ok(Value::Object(out))
}

fn pluck(args: &[Value]) -> BuiltinResult {
    // (pluck key list-of-maps) → [m1[key], m2[key], ...]; missing → nil.
    let key = expect_string("pluck", &args[0])?;
    let a = expect_array("pluck", &args[1])?;
    let mut out = Vec::with_capacity(a.len());
    for v in a {
        match v {
            Value::Object(m) => out.push(m.get(key).cloned().unwrap_or(Value::Null)),
            Value::Null => out.push(Value::Null),
            other => {
                return Err(BuiltinError::TypeError {
                    function: "pluck".into(),
                    expected: "list of maps".into(),
                    got: format!("list containing {}", type_name(other)),
                })
            }
        }
    }
    Ok(Value::Array(out))
}

fn group_by(args: &[Value]) -> BuiltinResult {
    // (group-by key list-of-maps) → {key-value -> [matching maps]}
    let key = expect_string("group-by", &args[0])?;
    let a = expect_array("group-by", &args[1])?;
    let mut out: serde_json::Map<String, Value> = serde_json::Map::new();
    for v in a {
        let m = match v {
            Value::Object(m) => m,
            other => {
                return Err(BuiltinError::TypeError {
                    function: "group-by".into(),
                    expected: "list of maps".into(),
                    got: format!("list containing {}", type_name(other)),
                })
            }
        };
        let group_key = match m.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            Some(Value::Null) | None => "nil".to_string(),
            Some(other) => {
                return Err(BuiltinError::TypeError {
                    function: "group-by".into(),
                    expected: "scalar group key".into(),
                    got: type_name(other).into(),
                })
            }
        };
        out.entry(group_key)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .unwrap()
            .push(v.clone());
    }
    Ok(Value::Object(out))
}

inventory::submit! { Builtin::in_module(MODULE, "sort",        1, 1, sort) }
inventory::submit! { Builtin::in_module(MODULE, "uniq",        1, 1, uniq) }
inventory::submit! { Builtin::in_module(MODULE, "sum",         1, 1, sum) }
inventory::submit! { Builtin::in_module(MODULE, "avg",         1, 1, avg) }
inventory::submit! { Builtin::in_module(MODULE, "flatten",     1, 1, flatten) }
inventory::submit! { Builtin::in_module(MODULE, "partition",   2, 2, partition) }
inventory::submit! { Builtin::in_module(MODULE, "zip",         2, 2, zip) }
inventory::submit! { Builtin::in_module(MODULE, "take",        2, 2, take) }
inventory::submit! { Builtin::in_module(MODULE, "drop",        2, 2, drop_n) }
inventory::submit! { Builtin::in_module(MODULE, "frequencies", 1, 1, frequencies) }
inventory::submit! { Builtin::in_module(MODULE, "pluck",       2, 2, pluck) }
inventory::submit! { Builtin::in_module(MODULE, "group-by",    2, 2, group_by) }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn uniq_of(v: Value) -> Value {
        uniq(&[v]).unwrap()
    }

    #[test]
    fn uniq_dedupes_scalars_preserving_first_seen_order() {
        assert_eq!(
            uniq_of(json!([3, 1, 3, 2, 1, "a", "a", true, true, null, null])),
            json!([3, 1, 2, "a", true, null])
        );
    }

    #[test]
    fn uniq_keeps_types_distinct() {
        // The hash key carries a type tag, so these must not collapse
        // into each other the way a bare string key would.
        assert_eq!(uniq_of(json!([1, "1", true])), json!([1, "1", true]));
    }

    #[test]
    fn uniq_keys_numbers_by_their_serialized_form() {
        // `serde_json` keeps integer and float reprs distinct even when
        // the values compare equal, so `1` and `1.0` are separate keys
        // here. That matches the previous `==` comparison, which used
        // the same `PartialEq`. In practice the parser produces one repr
        // or the other, so a script sees `(uniq [1 1.0])` → `[1]`.
        assert_eq!(uniq_of(json!([1, 1.0])), json!([1, 1.0]));
        assert_eq!(uniq_of(json!([1, 1])), json!([1]));
        assert_eq!(uniq_of(json!([1.5, 1.5])), json!([1.5]));
    }

    #[test]
    fn uniq_still_dedupes_composites() {
        // Arrays and objects have no cheap hash key, so they keep the
        // linear scan. They must still dedupe.
        assert_eq!(
            uniq_of(json!([[1, 2], [1, 2], [3], {"a": 1}, {"a": 1}, [3]])),
            json!([[1, 2], [3], {"a": 1}])
        );
    }

    #[test]
    fn uniq_preserves_order_across_mixed_kinds() {
        // Scalars go through the set and composites through the scan, so
        // interleaving them is where an ordering bug would show up.
        assert_eq!(
            uniq_of(json!([1, [1], 1, "1", [1], true])),
            json!([1, [1], "1", true])
        );
    }

    #[test]
    fn uniq_is_linear_on_distinct_scalars() {
        // The scan this replaced took six seconds on 80k distinct
        // elements and reported five evaluation steps, so no step budget
        // could bound it. 200k is instant now and would not finish then.
        let input: Vec<Value> = (0..200_000).map(|i| json!(i)).collect();
        let out = uniq_of(Value::Array(input));
        assert_eq!(out.as_array().unwrap().len(), 200_000);
    }

    #[test]
    fn uniq_of_empty_is_empty() {
        assert_eq!(uniq_of(json!([])), json!([]));
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
