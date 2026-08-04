//! Numeric / arithmetic builtins.
//!
//! Arithmetic functions are variadic. Comparisons are binary.

use crate::{expect_number, inventory, Builtin, BuiltinError, BuiltinResult, Value};

/// Convert an `f64` to a JSON number, preferring an integer representation
/// when the value has no fractional part and fits in `i64`.
fn num_from_f64(n: f64) -> Value {
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

fn add(args: &[Value]) -> BuiltinResult {
    if args.is_empty() {
        return Ok(num_from_f64(0.0));
    }
    let mut acc = 0f64;
    for a in args {
        acc += expect_number("+", a)?;
    }
    Ok(num_from_f64(acc))
}

fn sub(args: &[Value]) -> BuiltinResult {
    if args.is_empty() {
        return Err(BuiltinError::ArgError {
            function: "-".into(),
            message: "expected at least 1 arg".into(),
        });
    }
    let first = expect_number("-", &args[0])?;
    if args.len() == 1 {
        return Ok(num_from_f64(-first));
    }
    let mut acc = first;
    for a in &args[1..] {
        acc -= expect_number("-", a)?;
    }
    Ok(num_from_f64(acc))
}

fn mul(args: &[Value]) -> BuiltinResult {
    let mut acc = 1f64;
    for a in args {
        acc *= expect_number("*", a)?;
    }
    Ok(num_from_f64(acc))
}

fn div(args: &[Value]) -> BuiltinResult {
    if args.is_empty() {
        return Err(BuiltinError::ArgError {
            function: "/".into(),
            message: "expected at least 1 arg".into(),
        });
    }
    let first = expect_number("/", &args[0])?;
    if args.len() == 1 {
        if first == 0.0 {
            return Err(BuiltinError::Failed {
                function: "/".into(),
                message: "division by zero".into(),
            });
        }
        return Ok(num_from_f64(1.0 / first));
    }
    let mut acc = first;
    for a in &args[1..] {
        let v = expect_number("/", a)?;
        if v == 0.0 {
            return Err(BuiltinError::Failed {
                function: "/".into(),
                message: "division by zero".into(),
            });
        }
        acc /= v;
    }
    Ok(num_from_f64(acc))
}

fn rem(args: &[Value]) -> BuiltinResult {
    let a = expect_number("mod", &args[0])?;
    let b = expect_number("mod", &args[1])?;
    if b == 0.0 {
        return Err(BuiltinError::Failed {
            function: "mod".into(),
            message: "modulo by zero".into(),
        });
    }
    Ok(num_from_f64(a % b))
}

/// Absolute value.
///
/// Integers are handled without a round trip through `f64`, so magnitudes
/// beyond 2^53 keep full precision. `abs` of `i64::MIN` does not fit in an
/// `i64`, so it is returned as an unsigned number rather than saturating.
fn abs(args: &[Value]) -> BuiltinResult {
    if let Value::Number(n) = &args[0] {
        if let Some(i) = n.as_i64() {
            return Ok(match i.checked_abs() {
                Some(a) => Value::Number(a.into()),
                None => Value::Number((i.unsigned_abs()).into()),
            });
        }
        if n.as_u64().is_some() {
            return Ok(args[0].clone());
        }
    }
    Ok(num_from_f64(expect_number("abs", &args[0])?.abs()))
}

fn floor(args: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(expect_number("floor", &args[0])?.floor()))
}

fn ceil(args: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(expect_number("ceil", &args[0])?.ceil()))
}

fn round(args: &[Value]) -> BuiltinResult {
    let n = expect_number("round", &args[0])?;
    let digits = if args.len() == 2 {
        expect_number("round", &args[1])? as i32
    } else {
        0
    };
    let factor = 10f64.powi(digits);
    Ok(num_from_f64((n * factor).round() / factor))
}

fn min(args: &[Value]) -> BuiltinResult {
    if args.is_empty() {
        return Err(BuiltinError::ArgError {
            function: "min".into(),
            message: "expected at least 1 arg".into(),
        });
    }
    let mut best = expect_number("min", &args[0])?;
    for a in &args[1..] {
        let v = expect_number("min", a)?;
        if v < best {
            best = v;
        }
    }
    Ok(num_from_f64(best))
}

fn max(args: &[Value]) -> BuiltinResult {
    if args.is_empty() {
        return Err(BuiltinError::ArgError {
            function: "max".into(),
            message: "expected at least 1 arg".into(),
        });
    }
    let mut best = expect_number("max", &args[0])?;
    for a in &args[1..] {
        let v = expect_number("max", a)?;
        if v > best {
            best = v;
        }
    }
    Ok(num_from_f64(best))
}

fn lt(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(
        expect_number("<", &args[0])? < expect_number("<", &args[1])?,
    ))
}
fn gt(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(
        expect_number(">", &args[0])? > expect_number(">", &args[1])?,
    ))
}
fn le(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(
        expect_number("<=", &args[0])? <= expect_number("<=", &args[1])?,
    ))
}
fn ge(args: &[Value]) -> BuiltinResult {
    Ok(Value::Bool(
        expect_number(">=", &args[0])? >= expect_number(">=", &args[1])?,
    ))
}

inventory::submit! { Builtin::variadic("+",   add) }
inventory::submit! { Builtin::variadic("-",   sub) }
inventory::submit! { Builtin::variadic("*",   mul) }
inventory::submit! { Builtin::variadic("/",   div) }
inventory::submit! { Builtin::new("mod",      2, 2, rem) }
inventory::submit! { Builtin::new("abs",      1, 1, abs) }
inventory::submit! { Builtin::new("floor",    1, 1, floor) }
inventory::submit! { Builtin::new("ceil",     1, 1, ceil) }
inventory::submit! { Builtin::new("round",    1, 2, round) }
inventory::submit! { Builtin::variadic("min", min) }
inventory::submit! { Builtin::variadic("max", max) }
inventory::submit! { Builtin::new("<",        2, 2, lt) }
inventory::submit! { Builtin::new(">",        2, 2, gt) }
inventory::submit! { Builtin::new("<=",       2, 2, le) }
inventory::submit! { Builtin::new(">=",       2, 2, ge) }

#[cfg(test)]
mod tests {
    use super::*;

    fn call_abs(v: Value) -> Value {
        abs(&[v]).unwrap()
    }

    #[test]
    fn abs_basic() {
        assert_eq!(call_abs(Value::from(-5i64)), Value::from(5i64));
        assert_eq!(call_abs(Value::from(5i64)), Value::from(5i64));
        assert_eq!(call_abs(Value::from(0i64)), Value::from(0i64));
        assert_eq!(call_abs(Value::from(-2.5f64)), Value::from(2.5f64));
        assert_eq!(call_abs(Value::from(2.5f64)), Value::from(2.5f64));
    }

    #[test]
    fn abs_preserves_large_integers() {
        // Beyond 2^53: an f64 round trip would lose the low bits.
        let big = 9_007_199_254_740_993i64; // 2^53 + 1
        assert_eq!(call_abs(Value::from(-big)), Value::from(big));
        assert_eq!(call_abs(Value::from(i64::MAX)), Value::from(i64::MAX));
    }

    #[test]
    fn abs_of_i64_min_does_not_saturate() {
        let got = call_abs(Value::from(i64::MIN));
        assert_eq!(got, Value::from(9_223_372_036_854_775_808u64));
    }

    #[test]
    fn abs_rejects_non_numbers() {
        assert!(abs(&[Value::String("nope".into())]).is_err());
        assert!(abs(&[Value::Null]).is_err());
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.