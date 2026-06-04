//! String builtins.

use crate::{expect_number, expect_string, inventory, Builtin, BuiltinError, BuiltinResult, Value};

fn upcase(args: &[Value]) -> BuiltinResult {
    let s = expect_string("upcase", &args[0])?;
    Ok(Value::String(s.to_uppercase()))
}

fn downcase(args: &[Value]) -> BuiltinResult {
    let s = expect_string("downcase", &args[0])?;
    Ok(Value::String(s.to_lowercase()))
}

fn capitalize(args: &[Value]) -> BuiltinResult {
    let s = expect_string("capitalize", &args[0])?;
    let mut chars = s.chars();
    let out = match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
    };
    Ok(Value::String(out))
}

fn trim(args: &[Value]) -> BuiltinResult {
    Ok(Value::String(
        expect_string("trim", &args[0])?.trim().to_string(),
    ))
}

fn split(args: &[Value]) -> BuiltinResult {
    let s = expect_string("split", &args[0])?;
    let sep = expect_string("split", &args[1])?;
    let parts: Vec<Value> = s
        .split(sep)
        .map(|p| Value::String(p.to_string()))
        .collect();
    Ok(Value::Array(parts))
}

fn join(args: &[Value]) -> BuiltinResult {
    let arr = match &args[0] {
        Value::Array(a) => a,
        _ => {
            return Err(BuiltinError::TypeError {
                function: "join".into(),
                expected: "list".into(),
                got: crate::type_name(&args[0]).into(),
            })
        }
    };
    let sep = expect_string("join", &args[1])?;
    let mut parts = Vec::with_capacity(arr.len());
    for v in arr {
        match v {
            Value::String(s) => parts.push(s.clone()),
            Value::Number(n) => parts.push(n.to_string()),
            Value::Bool(b) => parts.push(b.to_string()),
            Value::Null => parts.push(String::new()),
            other => parts.push(serde_json::to_string(other).unwrap_or_default()),
        }
    }
    Ok(Value::String(parts.join(sep)))
}

fn replace(args: &[Value]) -> BuiltinResult {
    let s = expect_string("replace", &args[0])?;
    let from = expect_string("replace", &args[1])?;
    let to = expect_string("replace", &args[2])?;
    Ok(Value::String(s.replace(from, to)))
}

fn str_len(args: &[Value]) -> BuiltinResult {
    let s = expect_string("str-len", &args[0])?;
    Ok(Value::Number((s.chars().count() as i64).into()))
}

fn str_concat(args: &[Value]) -> BuiltinResult {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        match a {
            Value::String(s) => out.push_str(s),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::Bool(b) => out.push_str(&b.to_string()),
            Value::Null => {}
            other => {
                return Err(BuiltinError::TypeError {
                    function: "str".into(),
                    expected: "string/number/bool/nil".into(),
                    got: format!("{} at position {}", crate::type_name(other), i),
                })
            }
        }
    }
    Ok(Value::String(out))
}

fn starts_with(args: &[Value]) -> BuiltinResult {
    let s = expect_string("starts-with?", &args[0])?;
    let p = expect_string("starts-with?", &args[1])?;
    Ok(Value::Bool(s.starts_with(p)))
}

fn ends_with(args: &[Value]) -> BuiltinResult {
    let s = expect_string("ends-with?", &args[0])?;
    let p = expect_string("ends-with?", &args[1])?;
    Ok(Value::Bool(s.ends_with(p)))
}

fn contains_str(args: &[Value]) -> BuiltinResult {
    let s = expect_string("contains-str?", &args[0])?;
    let p = expect_string("contains-str?", &args[1])?;
    Ok(Value::Bool(s.contains(p)))
}

fn substr(args: &[Value]) -> BuiltinResult {
    let s = expect_string("substr", &args[0])?;
    let start = expect_number("substr", &args[1])? as i64;
    let len = if args.len() == 3 {
        Some(expect_number("substr", &args[2])? as i64)
    } else {
        None
    };
    let chars: Vec<char> = s.chars().collect();
    let total = chars.len() as i64;
    let start = if start < 0 {
        (total + start).max(0)
    } else {
        start.min(total)
    } as usize;
    let end = match len {
        None => chars.len(),
        Some(l) if l < 0 => start,
        Some(l) => (start + l as usize).min(chars.len()),
    };
    let out: String = chars[start..end].iter().collect();
    Ok(Value::String(out))
}

inventory::submit! { Builtin::new("upcase",        1, 1, upcase) }
inventory::submit! { Builtin::new("downcase",      1, 1, downcase) }
inventory::submit! { Builtin::new("capitalize",    1, 1, capitalize) }
inventory::submit! { Builtin::new("trim",          1, 1, trim) }
inventory::submit! { Builtin::new("split",         2, 2, split) }
inventory::submit! { Builtin::new("join",          2, 2, join) }
inventory::submit! { Builtin::new("replace",       3, 3, replace) }
inventory::submit! { Builtin::new("str-len",       1, 1, str_len) }
inventory::submit! { Builtin::variadic("str",      str_concat) }
inventory::submit! { Builtin::new("starts-with?",  2, 2, starts_with) }
inventory::submit! { Builtin::new("ends-with?",    2, 2, ends_with) }
inventory::submit! { Builtin::new("contains-str?", 2, 2, contains_str) }
inventory::submit! { Builtin::new("substr",        2, 3, substr) }

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.