//! Tests for evaluation resource metering.
//!
//! Each of the runaway shapes below hangs or crashes an unmetered
//! tree-walking evaluator. The point of these tests is that every one of
//! them now terminates with a typed error.

use std::collections::HashMap;

use crate::error::EvalError;
use crate::eval::{eval_source, Context, EvalConfig};
use crate::fuel::{check_value_len, container_overhead, value_cost, Fuel, FuelLimits};
use trongoscript_stdlib::Value;

fn ctx_with(limits: FuelLimits) -> Context {
    Context::new(EvalConfig::default().with_fuel_limits(limits))
}

// ---------------------------------------------------------------------------
// Step budget
// ---------------------------------------------------------------------------

#[test]
fn tail_recursive_loop_runs_out_of_fuel_instead_of_hanging() {
    // Deliberately low ceilings: the Rust test harness runs each test on a
    // 2 MiB thread stack, far smaller than the main thread's 8 MiB, so a
    // test that recursed to the *default* ceiling here would overflow
    // before the guard fired. The out-of-process Ruby stress suite covers
    // the real defaults against a real stack; see stress-tests/.
    let mut ctx = ctx_with(FuelLimits {
        max_steps: 10_000,
        max_call_depth: 64,
        max_eval_depth: 256,
        ..FuelLimits::default()
    });
    // Without metering this never returns.
    let err = eval_source("(defn spin [] (spin)) (spin)", &mut ctx).unwrap_err();
    assert!(
        err.is_resource_limit(),
        "expected a resource-limit error, got {err}"
    );
}

#[test]
fn step_limit_fires_on_flat_iteration_that_never_recurses() {
    // `reduce` iterates without nesting: every closure call returns before
    // the next begins, so neither depth counter grows. Only the step
    // budget can stop this, which is exactly what it is for.
    let mut ctx = ctx_with(FuelLimits {
        max_steps: 5_000,
        ..FuelLimits::default()
    });
    let src = "(reduce (fn [acc x] (+ acc x)) 0 (range 100000))";
    let err = eval_source(src, &mut ctx).unwrap_err();
    assert!(
        matches!(err, EvalError::FuelExhausted { limit: 5_000 }),
        "expected FuelExhausted, got {err}"
    );
    assert!(
        ctx.fuel.peak_call_depth() < 10,
        "reduce should not nest; peak depth was {}",
        ctx.fuel.peak_call_depth()
    );
}

#[test]
fn steps_are_actually_counted() {
    let mut ctx = ctx_with(FuelLimits::default());
    eval_source("(+ 1 2)", &mut ctx).unwrap();
    let after_small = ctx.fuel.steps();
    assert!(after_small > 0, "evaluation should consume steps");

    let mut ctx2 = ctx_with(FuelLimits::default());
    eval_source("(+ 1 2 3 4 5 6 7 8 9 10)", &mut ctx2).unwrap();
    assert!(
        ctx2.fuel.steps() > after_small,
        "more work should cost more steps ({} vs {})",
        ctx2.fuel.steps(),
        after_small
    );
}

#[test]
fn a_normal_script_finishes_well_under_the_default_budget() {
    // The defaults must not get in a legitimate script's way.
    let mut ctx = ctx_with(FuelLimits::default());
    let src = r#"
        (use math)
        (defn fib [n] (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
        {:fib (fib 15)
         :mapped (map (fn [x] (* x x)) [1 2 3 4 5 6 7 8 9 10])}
    "#;
    let v = eval_source(src, &mut ctx).unwrap();
    assert_eq!(v["fib"], Value::from(610));
    assert!(
        ctx.fuel.steps() < FuelLimits::default().max_steps / 100,
        "a small script used {} steps, too close to the default ceiling",
        ctx.fuel.steps()
    );
}

// ---------------------------------------------------------------------------
// Call depth — the memory-safety case
// ---------------------------------------------------------------------------

#[test]
fn unbounded_recursion_errors_instead_of_overflowing_the_stack() {
    // With no depth guard this is a SIGSEGV, which no test could observe:
    // the process dies. Reaching the assertion at all is the result.
    let mut ctx = ctx_with(FuelLimits {
        max_call_depth: 64,
        max_steps: u64::MAX,
        ..FuelLimits::default()
    });
    let err = eval_source("(defn deep [n] (+ 1 (deep (+ n 1)))) (deep 0)", &mut ctx).unwrap_err();
    assert!(
        matches!(err, EvalError::CallDepthExceeded { max: 64 }),
        "expected CallDepthExceeded, got {err}"
    );
}

#[test]
fn call_depth_unwinds_so_sequential_calls_are_not_charged_cumulatively() {
    let mut ctx = ctx_with(FuelLimits {
        max_call_depth: 8,
        ..FuelLimits::default()
    });
    // 50 calls, each only one frame deep. If exit_call were missing on any
    // path this would trip the depth limit.
    let src = r#"
        (defn id [x] x)
        (map id [1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20
                 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40
                 41 42 43 44 45 46 47 48 49 50])
    "#;
    let v = eval_source(src, &mut ctx).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 50);
    assert_eq!(ctx.fuel.call_depth(), 0, "depth counter leaked");
}

#[test]
fn call_depth_unwinds_on_the_error_path_too() {
    let mut ctx = ctx_with(FuelLimits {
        max_call_depth: 16,
        ..FuelLimits::default()
    });
    // A closure that errors mid-body; the frame must still be released.
    eval_source("(defn boom [] (undefined-fn-xyz)) (boom)", &mut ctx).unwrap_err();
    assert_eq!(
        ctx.fuel.call_depth(),
        0,
        "depth counter leaked on the error path"
    );
}

#[test]
fn peak_call_depth_is_recorded() {
    let mut ctx = ctx_with(FuelLimits::default());
    let src = r#"
        (defn down [n] (if (<= n 0) 0 (down (- n 1))))
        (down 10)
    "#;
    eval_source(src, &mut ctx).unwrap();
    assert!(
        ctx.fuel.peak_call_depth() >= 10,
        "expected peak depth >= 10, got {}",
        ctx.fuel.peak_call_depth()
    );
    assert_eq!(ctx.fuel.call_depth(), 0);
}

// ---------------------------------------------------------------------------
// Expression nesting depth
// ---------------------------------------------------------------------------

#[test]
fn deeply_nested_data_hits_the_eval_depth_limit() {
    let mut ctx = ctx_with(FuelLimits {
        max_eval_depth: 32,
        ..FuelLimits::default()
    });
    // 200 levels of nested vectors.
    let src = format!("{}{}", "[".repeat(200), "]".repeat(200));
    let err = eval_source(&src, &mut ctx).unwrap_err();
    assert!(
        matches!(err, EvalError::EvalDepthExceeded { max: 32 }),
        "expected EvalDepthExceeded, got {err}"
    );
}

#[test]
fn eval_depth_unwinds_between_sibling_expressions() {
    let mut ctx = ctx_with(FuelLimits {
        max_eval_depth: 16,
        ..FuelLimits::default()
    });
    // Many shallow siblings, none deep. Must not accumulate.
    let src = format!("[{}]", "1 ".repeat(500));
    let v = eval_source(&src, &mut ctx).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 500);
}

// ---------------------------------------------------------------------------
// Value size and cumulative allocation
// ---------------------------------------------------------------------------

#[test]
fn an_oversized_single_value_is_rejected() {
    let mut ctx = ctx_with(FuelLimits {
        max_value_len: 1000,
        ..FuelLimits::default()
    });
    // One step, enormous result — invisible to the step counter.
    let err = eval_source("(range 100000)", &mut ctx).unwrap_err();
    assert!(
        matches!(
            err,
            EvalError::ValueTooLarge {
                kind: "list",
                max: 1000,
                ..
            }
        ),
        "expected ValueTooLarge, got {err}"
    );
}

#[test]
fn cumulative_allocation_is_bounded_across_many_small_values() {
    let mut ctx = ctx_with(FuelLimits {
        // Each value is well under max_value_len; only the cumulative
        // budget can catch the aggregate.
        max_value_len: 100_000,
        max_total_alloc: 50_000,
        max_steps: u64::MAX,
        // Kept shallow for the 2 MiB test-thread stack; the allocation
        // budget is what should stop this, not depth.
        max_call_depth: 64,
        max_eval_depth: 256,
    });
    // Flat iteration, so depth stays at 1: each call allocates a 500-element
    // list that is immediately discarded. No single value is near
    // max_value_len, and no recursion occurs — only the cumulative budget
    // sees the aggregate.
    let src = "(map (fn [x] (count (range 500))) (range 1000))";
    let err = eval_source(src, &mut ctx).unwrap_err();
    assert!(
        matches!(err, EvalError::AllocationExceeded { limit: 50_000 }),
        "expected AllocationExceeded, got {err}"
    );
}

#[test]
fn nested_literals_are_not_billed_once_per_level() {
    // A vector literal charges only its own container overhead, because
    // its elements were charged as they were evaluated. Without that,
    // deeply nested literals would be billed exponentially.
    let mut ctx = ctx_with(FuelLimits::default());
    eval_source("[[[[1 2 3]]]]", &mut ctx).unwrap();
    let nested = ctx.fuel.allocated();

    let mut ctx2 = ctx_with(FuelLimits::default());
    eval_source("[1 2 3]", &mut ctx2).unwrap();
    let flat = ctx2.fuel.allocated();

    assert!(
        nested < flat * 10,
        "nesting is being over-charged: {nested} vs {flat}"
    );
}

// ---------------------------------------------------------------------------
// The meter itself
// ---------------------------------------------------------------------------

#[test]
fn value_cost_scales_with_content() {
    assert!(value_cost(&Value::Null) > 0, "even a scalar has overhead");
    let small = value_cost(&Value::String("hi".into()));
    let big = value_cost(&Value::String("x".repeat(10_000)));
    assert!(big > small + 9_000, "string cost should track length");

    let arr = Value::Array(vec![Value::from(1); 100]);
    assert!(
        value_cost(&arr) > value_cost(&Value::from(1)) * 100,
        "array cost should include its elements"
    );
}

#[test]
fn check_value_len_recurses_into_nested_collections() {
    // A short outer array hiding an oversized inner one.
    let inner = Value::Array(vec![Value::from(1); 50]);
    let outer = Value::Array(vec![inner]);
    assert!(check_value_len(&outer, 100).is_ok());
    let err = check_value_len(&outer, 10).unwrap_err();
    assert!(matches!(err, EvalError::ValueTooLarge { len: 50, .. }));
}

#[test]
fn container_overhead_grows_with_length() {
    assert!(container_overhead(100) > container_overhead(1));
    // Must not panic or wrap on absurd inputs.
    let _ = container_overhead(usize::MAX);
}

#[test]
fn charge_steps_saturates_rather_than_wrapping() {
    let mut fuel = Fuel::new(FuelLimits::unlimited());
    fuel.charge_steps(u64::MAX).unwrap();
    // A second huge charge must not wrap around to a small number.
    fuel.charge_steps(u64::MAX).unwrap();
    assert_eq!(fuel.steps(), u64::MAX);
}

#[test]
fn reset_clears_consumption_but_keeps_limits() {
    let mut ctx = ctx_with(FuelLimits {
        max_steps: 10_000,
        ..FuelLimits::default()
    });
    eval_source("(+ 1 2)", &mut ctx).unwrap();
    assert!(ctx.fuel.steps() > 0);

    ctx.fuel.reset();
    assert_eq!(ctx.fuel.steps(), 0);
    assert_eq!(ctx.fuel.allocated(), 0);
    assert_eq!(ctx.fuel.limits().max_steps, 10_000, "limits must survive");

    // The context is still usable with a fresh budget.
    eval_source("(+ 3 4)", &mut ctx).unwrap();
}

#[test]
fn steps_remaining_tracks_consumption() {
    let mut ctx = ctx_with(FuelLimits {
        max_steps: 1_000,
        ..FuelLimits::default()
    });
    let before = ctx.fuel.steps_remaining();
    assert_eq!(before, 1_000);
    eval_source("(+ 1 2)", &mut ctx).unwrap();
    assert!(ctx.fuel.steps_remaining() < before);
    assert_eq!(
        ctx.fuel.steps_remaining() + ctx.fuel.steps(),
        1_000,
        "remaining + spent should equal the budget"
    );
}

#[test]
fn lowering_limits_mid_run_takes_effect_immediately() {
    let mut ctx = ctx_with(FuelLimits::default());
    eval_source("(+ 1 2)", &mut ctx).unwrap();
    let spent = ctx.fuel.steps();

    // Set the ceiling below what has already been spent.
    ctx.fuel.set_limits(FuelLimits {
        max_steps: spent - 1,
        ..FuelLimits::default()
    });
    let err = eval_source("(+ 1 2)", &mut ctx).unwrap_err();
    assert!(matches!(err, EvalError::FuelExhausted { .. }));
}

#[test]
fn budget_is_shared_across_evaluations_on_one_context() {
    // A host that reuses a Context for many snippets should not hand each
    // one a fresh budget by accident.
    let mut ctx = ctx_with(FuelLimits {
        max_steps: 200,
        ..FuelLimits::default()
    });
    let mut count = 0;
    loop {
        if eval_source("(+ 1 2)", &mut ctx).is_err() {
            break;
        }
        count += 1;
        assert!(count < 10_000, "budget was never exhausted");
    }
    assert!(count > 0, "the first evaluation should have succeeded");
}

#[test]
fn resource_errors_are_distinguishable_from_script_errors() {
    let script_err = EvalError::UndefinedSymbol("nope".into());
    assert!(!script_err.is_resource_limit());

    let limit_err = EvalError::FuelExhausted { limit: 1 };
    assert!(limit_err.is_resource_limit());
    let depth_err = EvalError::CallDepthExceeded { max: 1 };
    assert!(depth_err.is_resource_limit());
    let alloc_err = EvalError::AllocationExceeded { limit: 1 };
    assert!(alloc_err.is_resource_limit());
}

#[test]
fn unlimited_still_bounds_native_recursion() {
    // `unlimited()` disables the step and allocation budgets but must keep
    // a finite call depth: a stack overflow is not a recoverable error.
    let limits = FuelLimits::unlimited();
    assert_eq!(limits.max_steps, u64::MAX);
    assert!(
        limits.max_call_depth < usize::MAX,
        "unlimited() must still bound native stack growth"
    );
}

#[test]
fn strict_limits_are_tighter_than_defaults_on_every_axis() {
    let strict = FuelLimits::strict();
    let default = FuelLimits::default();
    assert!(strict.max_steps < default.max_steps);
    assert!(strict.max_call_depth < default.max_call_depth);
    assert!(strict.max_eval_depth < default.max_eval_depth);
    assert!(strict.max_value_len < default.max_value_len);
    assert!(strict.max_total_alloc < default.max_total_alloc);
}

#[test]
fn eval_depth_ceiling_sits_above_call_depth_in_every_preset() {
    // A closure call costs roughly two eval levels. If max_eval_depth were
    // at or below max_call_depth, runaway recursion would always be
    // reported as a nesting error and the call-depth limit would be dead
    // code. Measured on the countdown benchmark: ~2 eval levels per call.
    for (name, l) in [
        ("default", FuelLimits::default()),
        ("strict", FuelLimits::strict()),
        ("unlimited", FuelLimits::unlimited()),
    ] {
        assert!(
            l.max_eval_depth > l.max_call_depth * 2,
            "{name}: max_eval_depth ({}) must exceed 2x max_call_depth ({}) \
             or it will preempt the call-depth check",
            l.max_eval_depth,
            l.max_call_depth
        );
    }
}

// ---------------------------------------------------------------------------
// Interaction with the rest of the evaluator
// ---------------------------------------------------------------------------

#[test]
fn macro_expansion_bomb_is_still_caught() {
    // The pre-existing macro depth guard must keep working alongside fuel.
    let mut ctx = ctx_with(FuelLimits::default());
    let err = eval_source("(defmacro m [] (m)) (m)", &mut ctx).unwrap_err();
    assert!(
        matches!(err, EvalError::InvalidForm(_)) || err.is_resource_limit(),
        "expected the expansion to be stopped, got {err}"
    );
}

#[test]
fn higher_order_functions_consume_the_shared_budget() {
    let mut ctx = ctx_with(FuelLimits {
        max_steps: 2_000,
        max_call_depth: 100_000,
        ..FuelLimits::default()
    });
    // A map over a large list: n closure calls, each charging steps.
    let big: Vec<String> = (0..5_000).map(|i| i.to_string()).collect();
    let src = format!("(map (fn [x] (* x 2)) [{}])", big.join(" "));
    let err = eval_source(&src, &mut ctx).unwrap_err();
    assert!(
        err.is_resource_limit(),
        "expected map to be metered, got {err}"
    );
}

#[test]
fn quadratic_suffix_copies_are_charged() {
    // `maplist` builds n suffixes averaging n/2 elements: O(n^2) memory
    // from O(n) calls. The allocation budget is what catches it.
    let mut ctx = ctx_with(FuelLimits {
        max_steps: u64::MAX,
        max_total_alloc: 20_000,
        max_value_len: usize::MAX,
        ..FuelLimits::default()
    });
    let src = "(maplist (fn [xs] (count xs)) (range 2000))".to_string();
    let err = eval_source(&src, &mut ctx).unwrap_err();
    assert!(
        err.is_resource_limit(),
        "expected the suffix copies to be charged, got {err}"
    );
}

#[test]
fn host_builtin_results_are_charged() {
    let huge: HostBuiltinFn =
        std::sync::Arc::new(|_ctx, _args| Ok(Value::Array(vec![Value::from(1); 100_000])));
    let cfg = EvalConfig::default()
        .with_fuel_limits(FuelLimits {
            max_value_len: 500,
            ..FuelLimits::default()
        })
        .with_host_builtin("make-huge", huge);
    let mut ctx = Context::new(cfg);
    let err = eval_source("(make-huge)", &mut ctx).unwrap_err();
    assert!(
        matches!(err, EvalError::ValueTooLarge { .. }),
        "expected a host builtin's result to be charged, got {err}"
    );
}

type HostBuiltinFn = crate::eval::HostBuiltin;

#[test]
fn context_data_bindings_do_not_consume_the_budget() {
    // Host-supplied data is not the script's doing; binding it should not
    // eat into the script's own budget.
    let mut data = HashMap::new();
    data.insert("input".to_string(), Value::from(42));
    let ctx = Context::new(EvalConfig::default()).with_data(data);
    assert_eq!(ctx.fuel.steps(), 0);
    assert_eq!(ctx.fuel.allocated(), 0);
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
