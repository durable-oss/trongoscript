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

fn uniq(args: &[Value]) -> BuiltinResult {
    let a = expect_array("uniq", &args[0])?;
    let mut out: Vec<Value> = Vec::with_capacity(a.len());
    for v in a {
        if !out.iter().any(|w| w == v) {
            out.push(v.clone());
        }
    }
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
    Builtin::in_module(MODULE, "sort", 1, 1, sort),
    Builtin::in_module(MODULE, "uniq", 1, 1, uniq),
    Builtin::in_module(MODULE, "sum", 1, 1, sum),
    Builtin::in_module(MODULE, "avg", 1, 1, avg),
    Builtin::in_module(MODULE, "flatten", 1, 1, flatten),
    Builtin::in_module(MODULE, "partition", 2, 2, partition),
    Builtin::in_module(MODULE, "zip", 2, 2, zip),
    Builtin::in_module(MODULE, "take", 2, 2, take),
    Builtin::in_module(MODULE, "drop", 2, 2, drop_n),
    Builtin::in_module(MODULE, "frequencies", 1, 1, frequencies),
    Builtin::in_module(MODULE, "pluck", 2, 2, pluck),
    Builtin::in_module(MODULE, "group-by", 2, 2, group_by),
];

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

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
