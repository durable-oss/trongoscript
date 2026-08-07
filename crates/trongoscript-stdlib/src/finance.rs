//! `finance` module — common financial functions (Excel-flavored).
//!
//! Activated by `(use finance)`. All functions are pure arithmetic and
//! convergence routines — no I/O, no clock.
//!
//! Conventions follow standard spreadsheet semantics: `rate` is the
//! per-period interest rate, `nper` the number of periods, cash *outflows*
//! (e.g. loan payments) are negative, inflows positive. `type` (last arg,
//! optional, default 0) selects end-of-period (0) vs. beginning-of-period
//! (1) payments.

use crate::{expect_number, inventory, Builtin, BuiltinError, BuiltinResult, Value};

const MODULE: &str = "finance";

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

fn opt_arg(args: &[Value], i: usize, default: f64, fname: &str) -> Result<f64, BuiltinError> {
    if i < args.len() {
        expect_number(fname, &args[i])
    } else {
        Ok(default)
    }
}

/// PMT(rate, nper, pv [, fv [, type]]) — periodic payment.
///
/// Formula: pmt = -(pv*(1+r)^n + fv) * r / ((1+r*type)*((1+r)^n - 1))
/// Degenerate r=0: pmt = -(pv + fv) / n
fn pmt(args: &[Value]) -> BuiltinResult {
    let r = expect_number("pmt", &args[0])?;
    let n = expect_number("pmt", &args[1])?;
    let pv = expect_number("pmt", &args[2])?;
    let fv = opt_arg(args, 3, 0.0, "pmt")?;
    let typ = opt_arg(args, 4, 0.0, "pmt")?;
    if n == 0.0 {
        return Err(BuiltinError::ArgError {
            function: "pmt".into(),
            message: "nper must be non-zero".into(),
        });
    }
    let result = if r == 0.0 {
        -(pv + fv) / n
    } else {
        let factor = (1.0 + r).powf(n);
        -(pv * factor + fv) * r / ((1.0 + r * typ) * (factor - 1.0))
    };
    Ok(num_from_f64(result))
}

/// FV(rate, nper, pmt [, pv [, type]]) — future value.
fn fv(args: &[Value]) -> BuiltinResult {
    let r = expect_number("fv", &args[0])?;
    let n = expect_number("fv", &args[1])?;
    let pmt = expect_number("fv", &args[2])?;
    let pv = opt_arg(args, 3, 0.0, "fv")?;
    let typ = opt_arg(args, 4, 0.0, "fv")?;
    let result = if r == 0.0 {
        -(pv + pmt * n)
    } else {
        let factor = (1.0 + r).powf(n);
        -pv * factor - pmt * (1.0 + r * typ) * (factor - 1.0) / r
    };
    Ok(num_from_f64(result))
}

/// PV(rate, nper, pmt [, fv [, type]]) — present value.
fn pv(args: &[Value]) -> BuiltinResult {
    let r = expect_number("pv", &args[0])?;
    let n = expect_number("pv", &args[1])?;
    let pmt = expect_number("pv", &args[2])?;
    let fv = opt_arg(args, 3, 0.0, "pv")?;
    let typ = opt_arg(args, 4, 0.0, "pv")?;
    let result = if r == 0.0 {
        -(fv + pmt * n)
    } else {
        let factor = (1.0 + r).powf(n);
        -(fv + pmt * (1.0 + r * typ) * (factor - 1.0) / r) / factor
    };
    Ok(num_from_f64(result))
}

/// NPV(rate, c1, c2, ...) — net present value of a stream of cashflows
/// starting at period 1.
fn npv(args: &[Value]) -> BuiltinResult {
    if args.len() < 2 {
        return Err(BuiltinError::ArgError {
            function: "npv".into(),
            message: "expected (npv rate cashflow...) with at least one cashflow".into(),
        });
    }
    let r = expect_number("npv", &args[0])?;
    let mut acc = 0.0;
    for (i, v) in args[1..].iter().enumerate() {
        let c = expect_number("npv", v)?;
        acc += c / (1.0 + r).powi(i as i32 + 1);
    }
    Ok(num_from_f64(acc))
}

/// NPER(rate, pmt, pv [, fv [, type]]) — number of periods.
fn nper(args: &[Value]) -> BuiltinResult {
    let r = expect_number("nper", &args[0])?;
    let pmt = expect_number("nper", &args[1])?;
    let pv = expect_number("nper", &args[2])?;
    let fv = opt_arg(args, 3, 0.0, "nper")?;
    let typ = opt_arg(args, 4, 0.0, "nper")?;
    let result = if r == 0.0 {
        if pmt == 0.0 {
            return Err(BuiltinError::ArgError {
                function: "nper".into(),
                message: "with zero rate and zero pmt, nper is undefined".into(),
            });
        }
        -(pv + fv) / pmt
    } else {
        let adj = pmt * (1.0 + r * typ) / r;
        let num = adj - fv;
        let den = pv + adj;
        if num.signum() != -den.signum() || den == 0.0 {
            return Err(BuiltinError::Failed {
                function: "nper".into(),
                message: "no solution: payment direction does not amortize balance".into(),
            });
        }
        (num / -den).ln() / (1.0 + r).ln()
    };
    Ok(num_from_f64(result))
}

/// RATE(nper, pmt, pv [, fv [, type [, guess]]]) — periodic interest rate.
/// Solved iteratively (Newton's method) up to 1e-10 over 64 iterations.
fn rate(args: &[Value]) -> BuiltinResult {
    let n = expect_number("rate", &args[0])?;
    let pmt = expect_number("rate", &args[1])?;
    let pv = expect_number("rate", &args[2])?;
    let fv = opt_arg(args, 3, 0.0, "rate")?;
    let typ = opt_arg(args, 4, 0.0, "rate")?;
    let guess = opt_arg(args, 5, 0.1, "rate")?;

    fn f(r: f64, n: f64, pmt: f64, pv: f64, fv: f64, typ: f64) -> f64 {
        if r.abs() < 1e-15 {
            pv + pmt * n + fv
        } else {
            let factor = (1.0 + r).powf(n);
            pv * factor + pmt * (1.0 + r * typ) * (factor - 1.0) / r + fv
        }
    }

    let mut x = guess;
    for _ in 0..64 {
        let y = f(x, n, pmt, pv, fv, typ);
        if y.abs() < 1e-10 {
            return Ok(num_from_f64(x));
        }
        let eps = 1e-7;
        let dy = (f(x + eps, n, pmt, pv, fv, typ) - y) / eps;
        if dy.abs() < 1e-15 {
            return Err(BuiltinError::Failed {
                function: "rate".into(),
                message: "Newton iteration stalled (derivative ~= 0)".into(),
            });
        }
        x -= y / dy;
        if !x.is_finite() {
            return Err(BuiltinError::Failed {
                function: "rate".into(),
                message: "iteration diverged".into(),
            });
        }
    }
    Err(BuiltinError::Failed {
        function: "rate".into(),
        message: "did not converge in 64 iterations".into(),
    })
}

inventory::submit! { Builtin::in_module(MODULE, "pmt",  3, 5, pmt) }
inventory::submit! { Builtin::in_module(MODULE, "fv",   3, 5, fv) }
inventory::submit! { Builtin::in_module(MODULE, "pv",   3, 5, pv) }
inventory::submit! { Builtin::in_module_variadic(MODULE, "npv", npv) }
inventory::submit! { Builtin::in_module(MODULE, "nper", 3, 5, nper) }
inventory::submit! { Builtin::in_module(MODULE, "rate", 3, 6, rate) }

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
