//! Timing for two evaluator hot paths whose cost should be linear in input:
//! builtin lookup, and calling a closure that captures a large `let` binding.
//!
//! Run with `cargo run --release -p trongoscript --example hot_paths`.
//! Each row's time should scale with N; a 10× larger N taking ~100× longer
//! means a per-call cost has gone quadratic again.

use std::time::Instant;
use trongoscript::{eval_source, Context, EvalConfig, FuelLimits};

fn time(src: &str) -> f64 {
    let mut ctx = Context::new(EvalConfig {
        fuel_limits: FuelLimits {
            max_steps: u64::MAX,
            ..FuelLimits::default()
        },
        ..EvalConfig::default()
    });
    let start = Instant::now();
    eval_source(src, &mut ctx).expect("benchmark script evaluates");
    start.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    println!("{:<34} {:>7} {:>10}", "case", "N", "ms");
    for n in [500, 5_000] {
        // A user function called once per element. Its name is looked up in
        // the builtin registry first and misses, which was the worst case
        // for the old linear lookup.
        let src = format!("(defn bump [i] (+ i 1)) (count (map (fn [i] (bump i)) (range {n})))");
        println!(
            "{:<34} {:>7} {:>10.2}",
            "registry miss per element",
            n,
            time(&src)
        );
    }
    for n in [500, 5_000] {
        // The notezy view shape: map over rows with a closure that captures
        // `rows` itself through the enclosing `let`.
        let src = format!(
            "(let [rows (map (fn [i] {{:id i :name \"row\" :tags [1 2 3]}}) (range {n}))]
               (count (map (fn [o] (get o :id)) rows)))"
        );
        println!(
            "{:<34} {:>7} {:>10.2}",
            "closure capturing the rows",
            n,
            time(&src)
        );
    }
}
