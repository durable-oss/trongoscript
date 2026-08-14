//! `math` module — pure numeric / transcendental functions.
//!
//! Activated by `(use math)`. Everything here is pure computation: no
//! clock access, no RNG, no I/O. Safe to expose under any sandbox.

use crate::{
    expect_number, expect_string, inventory, num_from_f64, Builtin, BuiltinError, BuiltinResult,
    Value,
};

const MODULE: &str = "math";

fn sqrt(args: &[Value]) -> BuiltinResult {
    let x = expect_number("sqrt", &args[0])?;
    if x < 0.0 {
        return Err(BuiltinError::Failed {
            function: "sqrt".into(),
            message: "sqrt of negative number".into(),
        });
    }
    Ok(num_from_f64(x.sqrt()))
}

fn cbrt(args: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(expect_number("cbrt", &args[0])?.cbrt()))
}

/// Report a result JSON cannot represent, instead of silently nulling it.
///
/// [`num_from_f64`] maps both infinity and NaN to `Value::Null`, because
/// JSON can encode neither. That is the right *serialization* and the
/// wrong *answer*: a function that was asked to compute something returns
/// a null indistinguishable from a legitimate one, and the failure is
/// lost.
///
/// `sqrt` and the logarithms check their domain before computing, which
/// is better because the error can name the actual problem. The functions
/// here cannot: whether `pow` overflows depends on both arguments
/// together, and `asin`/`acos` produce NaN across an entire region rather
/// than at a checkable boundary. So they check the result instead.
///
/// Finite results pass through unchanged.
fn finite_or_undefined(fname: &str, n: f64) -> BuiltinResult {
    if n.is_infinite() {
        return Err(BuiltinError::Failed {
            function: fname.into(),
            message: format!(
                "result overflowed to {}infinity, which JSON cannot represent",
                if n.is_sign_negative() { "-" } else { "" }
            ),
        });
    }
    if n.is_nan() {
        return Err(BuiltinError::Failed {
            function: fname.into(),
            message: "result is undefined (argument outside the function's domain)".into(),
        });
    }
    Ok(num_from_f64(n))
}

fn pow(args: &[Value]) -> BuiltinResult {
    let x = expect_number("pow", &args[0])?;
    let y = expect_number("pow", &args[1])?;
    // Covers `(pow 10 100000)`, which overflows the exponent range;
    // `(pow 0 -1)`, a division by zero in disguise; and `(pow -1 0.5)`,
    // which has no real result and yields NaN.
    finite_or_undefined("pow", x.powf(y))
}

fn ln(args: &[Value]) -> BuiltinResult {
    let x = expect_number("log", &args[0])?;
    if x <= 0.0 {
        return Err(BuiltinError::Failed {
            function: "log".into(),
            message: "log of non-positive number".into(),
        });
    }
    Ok(num_from_f64(x.ln()))
}

fn log10(args: &[Value]) -> BuiltinResult {
    let x = expect_number("log10", &args[0])?;
    if x <= 0.0 {
        return Err(BuiltinError::Failed {
            function: "log10".into(),
            message: "log10 of non-positive number".into(),
        });
    }
    Ok(num_from_f64(x.log10()))
}

fn log2(args: &[Value]) -> BuiltinResult {
    let x = expect_number("log2", &args[0])?;
    if x <= 0.0 {
        return Err(BuiltinError::Failed {
            function: "log2".into(),
            message: "log2 of non-positive number".into(),
        });
    }
    Ok(num_from_f64(x.log2()))
}

fn exp(args: &[Value]) -> BuiltinResult {
    finite_or_undefined("exp", expect_number("exp", &args[0])?.exp())
}

fn sin(args: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(expect_number("sin", &args[0])?.sin()))
}
fn cos(args: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(expect_number("cos", &args[0])?.cos()))
}
fn tan(args: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(expect_number("tan", &args[0])?.tan()))
}
/// `asin` and `acos` are defined only on [-1, 1]. Outside it they yield
/// NaN, which serializes as `null` — the same silent loss `pow` had. The
/// domain is checkable up front, so check it there: the error can then
/// name the actual problem rather than reporting an undefined result.
fn unit_interval_arg(fname: &str, args: &[Value]) -> Result<f64, BuiltinError> {
    let x = expect_number(fname, &args[0])?;
    if !(-1.0..=1.0).contains(&x) {
        return Err(BuiltinError::Failed {
            function: fname.into(),
            message: format!("{fname} is undefined outside [-1, 1], got {x}"),
        });
    }
    Ok(x)
}

fn asin(args: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(unit_interval_arg("asin", args)?.asin()))
}
fn acos(args: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(unit_interval_arg("acos", args)?.acos()))
}
fn atan(args: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(expect_number("atan", &args[0])?.atan()))
}
fn atan2(args: &[Value]) -> BuiltinResult {
    let y = expect_number("atan2", &args[0])?;
    let x = expect_number("atan2", &args[1])?;
    Ok(num_from_f64(y.atan2(x)))
}

fn sign(args: &[Value]) -> BuiltinResult {
    let x = expect_number("sign", &args[0])?;
    Ok(Value::Number(
        (if x > 0.0 {
            1i64
        } else if x < 0.0 {
            -1i64
        } else {
            0i64
        })
        .into(),
    ))
}

fn gcd_two(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.unsigned_abs() as i64, b.unsigned_abs() as i64);
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

fn expect_i64(fname: &str, v: &Value) -> Result<i64, BuiltinError> {
    let f = expect_number(fname, v)?;
    if f.fract() != 0.0 {
        return Err(BuiltinError::TypeError {
            function: fname.into(),
            expected: "integer".into(),
            got: "non-integer number".into(),
        });
    }
    Ok(f as i64)
}

fn gcd(args: &[Value]) -> BuiltinResult {
    if args.is_empty() {
        return Err(BuiltinError::ArgError {
            function: "gcd".into(),
            message: "expected at least 1 arg".into(),
        });
    }
    let mut acc = expect_i64("gcd", &args[0])?.abs();
    for a in &args[1..] {
        acc = gcd_two(acc, expect_i64("gcd", a)?);
    }
    Ok(Value::Number(acc.into()))
}

fn lcm(args: &[Value]) -> BuiltinResult {
    if args.is_empty() {
        return Err(BuiltinError::ArgError {
            function: "lcm".into(),
            message: "expected at least 1 arg".into(),
        });
    }
    let mut acc = expect_i64("lcm", &args[0])?.abs();
    for a in &args[1..] {
        let b = expect_i64("lcm", a)?.abs();
        if acc == 0 || b == 0 {
            return Ok(Value::Number(0i64.into()));
        }
        let g = gcd_two(acc, b);
        // Guard against overflow on i64.
        let prod = match acc.checked_div(g).and_then(|q| q.checked_mul(b)) {
            Some(v) => v,
            None => {
                return Err(BuiltinError::Failed {
                    function: "lcm".into(),
                    message: "result overflows i64".into(),
                })
            }
        };
        acc = prod;
    }
    Ok(Value::Number(acc.into()))
}

fn pi(_: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(std::f64::consts::PI))
}
fn e_const(_: &[Value]) -> BuiltinResult {
    Ok(num_from_f64(std::f64::consts::E))
}

fn parse_int(args: &[Value]) -> BuiltinResult {
    let s = expect_string("parse-int", &args[0])?.trim();
    s.parse::<i64>()
        .map(|i| Value::Number(i.into()))
        .map_err(|e| BuiltinError::Failed {
            function: "parse-int".into(),
            message: e.to_string(),
        })
}

fn parse_float(args: &[Value]) -> BuiltinResult {
    let s = expect_string("parse-float", &args[0])?.trim();
    s.parse::<f64>()
        .map(num_from_f64)
        .map_err(|e| BuiltinError::Failed {
            function: "parse-float".into(),
            message: e.to_string(),
        })
}

inventory::submit! { Builtin::in_module(MODULE, "sqrt",        1, 1, sqrt) }
inventory::submit! { Builtin::in_module(MODULE, "cbrt",        1, 1, cbrt) }
inventory::submit! { Builtin::in_module(MODULE, "pow",         2, 2, pow) }
inventory::submit! { Builtin::in_module(MODULE, "log",         1, 1, ln) }
inventory::submit! { Builtin::in_module(MODULE, "log10",       1, 1, log10) }
inventory::submit! { Builtin::in_module(MODULE, "log2",        1, 1, log2) }
inventory::submit! { Builtin::in_module(MODULE, "exp",         1, 1, exp) }
inventory::submit! { Builtin::in_module(MODULE, "sin",         1, 1, sin) }
inventory::submit! { Builtin::in_module(MODULE, "cos",         1, 1, cos) }
inventory::submit! { Builtin::in_module(MODULE, "tan",         1, 1, tan) }
inventory::submit! { Builtin::in_module(MODULE, "asin",        1, 1, asin) }
inventory::submit! { Builtin::in_module(MODULE, "acos",        1, 1, acos) }
inventory::submit! { Builtin::in_module(MODULE, "atan",        1, 1, atan) }
inventory::submit! { Builtin::in_module(MODULE, "atan2",       2, 2, atan2) }
inventory::submit! { Builtin::in_module(MODULE, "sign",        1, 1, sign) }
inventory::submit! { Builtin::in_module_variadic(MODULE, "gcd", gcd) }
inventory::submit! { Builtin::in_module_variadic(MODULE, "lcm", lcm) }
inventory::submit! { Builtin::in_module(MODULE, "pi",          0, 0, pi) }
inventory::submit! { Builtin::in_module(MODULE, "e",           0, 0, e_const) }
inventory::submit! { Builtin::in_module(MODULE, "parse-int",   1, 1, parse_int) }
inventory::submit! { Builtin::in_module(MODULE, "parse-float", 1, 1, parse_float) }

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
