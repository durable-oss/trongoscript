//! Logic / equality / type-predicate builtins.

use crate::{inventory, Builtin, BuiltinResult, Value};

fn eq(args: &[Value]) -> BuiltinResult {
    if args.is_empty() {
        return Ok(Value::Bool(true));
    }
    let first = &args[0];
    Ok(Value::Bool(args.iter().all(|v| v == first)))
}

fn neq(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(args[0] != args[1]))
}

fn not(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(!truthy(&args[0])))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        _ => true,
    }
}

fn nil_q(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(matches!(&args[0], Value::Null)))
}

fn bool_q(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(matches!(&args[0], Value::Bool(_))))
}

fn number_q(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(matches!(&args[0], Value::Number(_))))
}

fn string_q(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(matches!(&args[0], Value::String(_))))
}

fn list_q(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(matches!(&args[0], Value::Array(_))))
}

fn map_q(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(matches!(&args[0], Value::Object(_))))
}

fn empty_q(args: &[Value]) -> BuiltinResult {
    let b = match &args[0] {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        Value::String(s) => s.is_empty(),
        _ => false,
    };
    Ok(Value::Bool(b))
}

inventory::submit! { Builtin::variadic("=",       eq) }
inventory::submit! { Builtin::new("not=",         2, 2, neq) }
inventory::submit! { Builtin::new("not",          1, 1, not) }
inventory::submit! { Builtin::new("nil?",         1, 1, nil_q) }
inventory::submit! { Builtin::new("bool?",        1, 1, bool_q) }
inventory::submit! { Builtin::new("number?",      1, 1, number_q) }
inventory::submit! { Builtin::new("string?",      1, 1, string_q) }
inventory::submit! { Builtin::new("list?",        1, 1, list_q) }
inventory::submit! { Builtin::new("map?",         1, 1, map_q) }
inventory::submit! { Builtin::new("empty?",       1, 1, empty_q) }

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.