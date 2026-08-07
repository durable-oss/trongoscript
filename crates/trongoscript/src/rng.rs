//! `rng` module — context-bound deterministic random number generation.
//!
//! Unlike the pure module builtins in `trongoscript-stdlib`, these need
//! mutable access to the evaluator's [`Context`] so they can advance the
//! shared seed across calls. They live here in the evaluator crate and are
//! dispatched ahead of the inventory lookup by [`crate::eval::dispatch_call`].
//!
//! Activated by `(use rng)`. The host program sets the initial seed via
//! [`crate::EvalConfig::with_rng_seed`]; the default is `0`, which makes
//! the entire RNG stream fully reproducible.
//!
//! Surface:
//! - `(rand)`               → float in `[0, 1)`
//! - `(rand-int lo hi)`     → integer in `[lo, hi)`
//! - `(choose list)`        → uniformly chosen element
//! - `(dice-roll n sides)`  → sum of n d{sides}
//! - `(shuffle list)`       → Fisher–Yates copy
//! - `(set-seed n)`         → reset the shared state, returns nil
//! - `(spintax pat)`        → 1-arg overload of the `strings` module's
//!   `spintax`, using and advancing the context seed

use trongoscript_parser::Expr;
use trongoscript_stdlib::{BuiltinError, Value};

use crate::error::{EvalError, EvalResult};
use crate::eval::{eval, Context};

/// Names recognized as `rng`-module functions when the module is enabled.
const RNG_NAMES: &[&str] = &[
    "rand",
    "rand-int",
    "choose",
    "dice-roll",
    "shuffle",
    "set-seed",
];

/// True if `name` is an `rng`-module function. Used by the evaluator to
/// decide whether to short-circuit dispatch (when the module is enabled)
/// or to surface "unknown function" cleanly (when it isn't).
pub fn is_rng_name(name: &str) -> bool {
    RNG_NAMES.contains(&name)
}

/// Single SplitMix64 step. Pure on `state`, no side effects.
pub fn splitmix64_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Float in `[0, 1)`, derived from the high 53 bits of one SplitMix64 step.
fn next_unit(state: &mut u64) -> f64 {
    // Match the standard double-precision uniform construction.
    let bits = splitmix64_next(state) >> 11;
    (bits as f64) * (1.0f64 / (1u64 << 53) as f64)
}

/// Integer in `[lo, hi)`. Caller is responsible for `lo < hi`.
fn next_int_in(state: &mut u64, lo: i64, hi: i64) -> i64 {
    let span = (hi - lo) as u64;
    let r = splitmix64_next(state) % span;
    lo + r as i64
}

/// Dispatch hook called by `eval::dispatch_call` *before* the inventory
/// lookup. Returns:
/// - `Some(Ok(value))` if `name` matched and ran successfully
/// - `Some(Err(...))` if it matched but evaluation failed
/// - `None` if `name` isn't an RNG function (caller falls through)
///
/// Only matches when the `rng` module is enabled.
pub fn try_dispatch(
    name: &str,
    arg_exprs: &[Expr],
    ctx: &mut Context,
) -> Option<EvalResult<Value>> {
    if !ctx.enabled_modules.contains("rng") {
        return None;
    }
    match name {
        "rand" => Some(call_rand(arg_exprs, ctx)),
        "rand-int" => Some(call_rand_int(arg_exprs, ctx)),
        "choose" => Some(call_choose(arg_exprs, ctx)),
        "dice-roll" => Some(call_dice_roll(arg_exprs, ctx)),
        "shuffle" => Some(call_shuffle(arg_exprs, ctx)),
        "set-seed" => Some(call_set_seed(arg_exprs, ctx)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Individual builtins
// ---------------------------------------------------------------------------

fn eval_one(args: &[Expr], ctx: &mut Context) -> EvalResult<Vec<Value>> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        out.push(eval(a, ctx)?);
    }
    Ok(out)
}

fn check_arity(name: &str, args: &[Expr], min: usize, max: usize) -> EvalResult<()> {
    if args.len() < min || args.len() > max {
        let expected = if min == max {
            min.to_string()
        } else {
            format!("{}-{}", min, max)
        };
        return Err(EvalError::Arity {
            function: name.into(),
            expected,
            actual: args.len(),
        });
    }
    Ok(())
}

fn expect_int(fname: &str, v: &Value) -> EvalResult<i64> {
    let f = v.as_f64().ok_or_else(|| {
        EvalError::from(BuiltinError::TypeError {
            function: fname.into(),
            expected: "number".into(),
            got: trongoscript_stdlib::type_name(v).into(),
        })
    })?;
    if f.fract() != 0.0 {
        return Err(EvalError::from(BuiltinError::TypeError {
            function: fname.into(),
            expected: "integer".into(),
            got: "non-integer number".into(),
        }));
    }
    Ok(f as i64)
}

fn call_rand(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    check_arity("rand", args, 0, 0)?;
    let v = next_unit(&mut ctx.rng_state);
    serde_json::Number::from_f64(v)
        .map(Value::Number)
        .ok_or_else(|| {
            EvalError::from(BuiltinError::Failed {
                function: "rand".into(),
                message: "could not encode result".into(),
            })
        })
}

fn call_rand_int(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    check_arity("rand-int", args, 2, 2)?;
    let vals = eval_one(args, ctx)?;
    let lo = expect_int("rand-int", &vals[0])?;
    let hi = expect_int("rand-int", &vals[1])?;
    if lo >= hi {
        return Err(EvalError::from(BuiltinError::ArgError {
            function: "rand-int".into(),
            message: format!("expected lo < hi, got lo={} hi={}", lo, hi),
        }));
    }
    Ok(Value::Number(
        next_int_in(&mut ctx.rng_state, lo, hi).into(),
    ))
}

fn call_choose(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    check_arity("choose", args, 1, 1)?;
    let vals = eval_one(args, ctx)?;
    let arr = vals[0].as_array().ok_or_else(|| {
        EvalError::from(BuiltinError::TypeError {
            function: "choose".into(),
            expected: "list".into(),
            got: trongoscript_stdlib::type_name(&vals[0]).into(),
        })
    })?;
    if arr.is_empty() {
        return Err(EvalError::from(BuiltinError::ArgError {
            function: "choose".into(),
            message: "cannot choose from an empty list".into(),
        }));
    }
    let idx = next_int_in(&mut ctx.rng_state, 0, arr.len() as i64) as usize;
    Ok(arr[idx].clone())
}

fn call_dice_roll(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    check_arity("dice-roll", args, 2, 2)?;
    let vals = eval_one(args, ctx)?;
    let n = expect_int("dice-roll", &vals[0])?;
    let sides = expect_int("dice-roll", &vals[1])?;
    if n < 0 {
        return Err(EvalError::from(BuiltinError::ArgError {
            function: "dice-roll".into(),
            message: "die count must be non-negative".into(),
        }));
    }
    if sides < 1 {
        return Err(EvalError::from(BuiltinError::ArgError {
            function: "dice-roll".into(),
            message: "side count must be >= 1".into(),
        }));
    }
    // Hard cap on rolls so an absurd input can't burn CPU.
    if n > 1_000_000 {
        return Err(EvalError::from(BuiltinError::ArgError {
            function: "dice-roll".into(),
            message: "die count exceeds 1,000,000".into(),
        }));
    }
    let mut sum: i64 = 0;
    for _ in 0..n {
        sum += next_int_in(&mut ctx.rng_state, 1, sides + 1);
    }
    Ok(Value::Number(sum.into()))
}

fn call_shuffle(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    check_arity("shuffle", args, 1, 1)?;
    let vals = eval_one(args, ctx)?;
    let arr = vals[0].as_array().ok_or_else(|| {
        EvalError::from(BuiltinError::TypeError {
            function: "shuffle".into(),
            expected: "list".into(),
            got: trongoscript_stdlib::type_name(&vals[0]).into(),
        })
    })?;
    let mut out: Vec<Value> = arr.clone();
    // Fisher–Yates.
    for i in (1..out.len()).rev() {
        let j = next_int_in(&mut ctx.rng_state, 0, (i + 1) as i64) as usize;
        out.swap(i, j);
    }
    Ok(Value::Array(out))
}

fn call_set_seed(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    check_arity("set-seed", args, 1, 1)?;
    let vals = eval_one(args, ctx)?;
    let seed = expect_int("set-seed", &vals[0])?;
    ctx.rng_state = seed as u64;
    Ok(Value::Null)
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
