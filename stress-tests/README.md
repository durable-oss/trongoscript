# Stress tests

End-to-end tests for Trongoscript's evaluation resource metering — the
limits that keep a hostile or buggy script from hanging, crashing, or
exhausting the memory of the process hosting the interpreter.

```sh
rake          # build the release CLI, then run everything
rake test     # run against whatever binary is already built
rake crash    # only the tests that must never crash the process
```

Requires Ruby and Minitest. `TRONGO_BIN=/path/to/trongo` overrides which
binary is exercised.

## Why these are separate from `cargo test`

The Rust unit tests in `crates/trongoscript/src/tests/fuel.rs` cover the
metering API: counters increment, limits fire, budgets unwind. They cannot
cover the failure modes that matter most here.

A missing recursion guard is a **stack overflow** — SIGSEGV, uncatchable,
and it takes the test runner down with it, reporting nothing. A missing
allocation guard is an **OOM**, which without a memory cap means the
machine starts swapping and the suite wedges instead of failing. And Rust's
test harness runs each test on a 2 MiB thread stack, an order of magnitude
smaller than a real process gets, so in-process depth tests measure the
harness rather than the interpreter.

So these tests drive the CLI as a subprocess, under a wall-clock timeout
and an `RLIMIT_AS` memory cap. A crash becomes an observable
`Process::Status` with a signal; a hang becomes a timeout; an OOM becomes a
fast clean kill. All three are reported as ordinary test failures.

The standard every test asserts: **hostile input must terminate in a
diagnosable way** — a clean exit code with an error message. Never a
signal, never a hang.

## Layout

| File | Covers |
|---|---|
| `test_stack_safety.rb` | Recursion and nesting: direct, mutual, laundered through higher-order functions, and deeply nested literals |
| `test_runaway_loops.rb` | Non-termination: infinite loops, macro-expansion bombs, budget sharing across a document |
| `test_memory_exhaustion.rb` | Allocation: single oversized values, cumulative growth, quadratic copying |
| `test_limit_configuration.rb` | Limits honored as configured, presets, accounting, and that no configuration can produce a crash |
| `test_builtin_amplification.rb` | Builtins that return more than they were given: `repeat`, `pad-*`, `zip`, `frequencies`, serialization, doubling loops |
| `test_builtin_complexity.rb` | Builtins whose running time is superlinear in their input, where the per-call step charge cannot see the work |
| `test_builtin_arguments.rb` | Argument domains: extreme indices and counts, empty inputs, division by zero, math domain errors, type confusion, multibyte strings |
| `test_builtin_arity.rb` | Every builtin, macro and special form called with 0–7 arguments; user-defined and rest-parameter arity; sandbox and reserved host forms |
| `test_higher_order.rb` | The `higher_order.rs` layer: quadratic set operations, suffix and concat amplification, native-stack tree walks, closure-argument type confusion |
| `test_module_surface.rb` | `rng`, `finance` and `predicates`: uncharged randomness, integer-span overflow, cross-module name shadowing, iterative solvers |
| `test_frontend_surface.rb` | Entry points other than `eval`: the template scanner, the `embed` document walker, glob patterns, path traversal, include cycles |
| `lib/trongo_harness.rb` | Subprocess runner: timeouts, memory caps, signal detection, fuel-report parsing |
| `lib/stress_test_case.rb` | Assertions: `assert_diagnosable`, `assert_resource_limited`, `assert_completes` |

## Bugs this suite found

Every one of these was a real crash, hang, or silently wrong answer that
the Rust unit tests passed straight over. They are listed because each has
a regression test above, and because they show what this harness is for.

1. **The parser recursed without a limit.** 50,000 nested `[` overflowed
   the stack during *parsing* — before evaluation began, so no evaluation
   fuel limit could ever apply. Fixed with a depth guard in the parser's
   `form` combinator (`ParseError::TooDeep`).

2. **Depth defaults were unsafe on debug builds.** They were tuned against
   release measurements. Debug frames are ~10x larger: the interpreter
   core-dumped at 200–300 frames where release survived 3,000–4,000. The
   CLI now evaluates on a thread with a large explicit stack, and the
   defaults are set against the *worst* build profile rather than the best.

3. **`--no-limits` disabled the stack guard.** A stack overflow is a crash,
   not a policy choice, so depth is no longer something a flag can switch
   off.

4. **Operator-supplied depth could request a crash.** `--max-call-depth
   10000000` was obeyed literally. Depth flags are now clamped to what the
   stack can survive.

5. **Oversized allocations were detected only after being made.** `(range
   100000000)` built the whole list first, peaking over 3 GB of RSS, and
   was OOM-killed before the guard could report anything. Builtins whose
   output size is predictable from their arguments are now checked *before*
   they run — the same input now peaks at 4 MB and returns an error.

6. **Deep copying was invisible to the step counter.** `(reduce (fn [acc x]
   [acc]) [] ...)` re-copies its growing accumulator every iteration:
   quadratic in real time, linear in steps. 8,000 rounds cost 3.4 seconds
   but reported 16,005 steps, far under any budget. Argument copying is now
   charged for its size, which raised the same script's reported cost by
   233x and brought it under the budget.

7. **The meter itself could be made expensive.** Walking a value to price
   it is O(size), and it ran on every builtin return — so nesting bombs
   made the *guard* quadratic. Traversals are now node-bounded.

8. **A large stack reservation broke memory-capped environments.** The
   256 MiB interpreter stack is reserved address space and failed outright
   under a tight `RLIMIT_AS`, so the CLI refused to start in a constrained
   container. It now falls back through smaller sizes.

The next three came from the builtin tests. They share a root cause worth
stating on its own: **the step counter charges per call, so work done
inside a single builtin is invisible to it.** A builtin that can take
arbitrary time or memory in one call needs its own bound — the evaluator
cannot supply one, because it does not regain control until the call
returns.

9. **`pad-left` / `pad-right` aborted the process.** The width went from
   an `f64` straight into `repeat_n` with no ceiling, so
   `(pad-left "x" 1e15 "a")` asked the allocator for a petabyte and died
   with SIGABRT and a core dump, while `(pad-left "x" 1e308 "a")`
   overflowed the `usize` conversion and panicked in `raw_vec`. Its
   sibling `repeat` already had the check that was missing. Both now share
   one `output_len` helper that rejects the size before allocating, and
   which also rejects NaN — `n < 0.0` is false for NaN, so it had been
   passing the old guard and converting to 0.

10. **`coll:uniq` was quadratic.** It compared each element against every
    element kept so far, so n distinct inputs cost O(n²). 20k took 0.39s,
    40k 1.5s, 80k 6.1s, and 200k never returned — every run reporting
    `steps=5`. Scalars now go through a hash set (`serde_json::Value` is
    only `PartialEq`, so they are projected onto a hashable key first);
    composites keep the scan but are compared only against other
    composites. 200k distinct elements went from a hang to 53 ms.

11. **`str:spintax` was quadratic in nesting depth, on the native stack.**
    Expansion recursed once per `{`, and each level that reached EOF
    rebuilt a string containing everything below it. 500k nested braces
    took 24 seconds and reported seven steps; 1M did not return. Because
    the recursion was native rather than evaluator frames,
    `--max-call-depth` did not apply and an embedder had no way to bound
    it. Nesting is now capped at 64 levels, past which `{` is emitted
    literally — the same thing an unbalanced pattern already did. 1M
    braces went from a hang to 14 ms.

The last four came from the builtin-surface expansion. The first was a
crash and was fixed on its own terms. The other three were one problem
stated three times, and it is the problem this list kept rediscovering:
**a builtin that scans with deep equality is quadratic, and the step
counter charges it once.** A fix covering only the case in front of it —
scalars but not composites, `uniq` but not `union` — leaves the same
attack available behind a different builtin name, which is exactly how
#13 survived #10. They were fixed together, with one shared mechanism.

12. **`(conj)` with no arguments aborted the process.** It is registered
    variadic with `min_args = 0`, and its body read `args[0]`
    unconditionally, so the empty call indexed an empty slice:

    ```
    thread 'trongo-eval' panicked at collection.rs:78:35:
    index out of bounds: the len is 0 but the index is 0
    ```

    This was reachable from `embed` as well as `eval`, so a JSON document
    containing `"!trongo (conj)"` crashed the process that walked it.
    `conj` now re-validates its arity in the body, the way `assoc` — which
    has the identical registration — already did. `test_builtin_arity.rb`
    sweeps all ~150 builtins at 0–7 arguments and `conj` was the only one
    that panicked.

13. **The `uniq` fix covered scalars only.** Bug #10 replaced the
    quadratic scan with a hash set, but `serde_json::Value` is not
    hashable, so only scalars got a key and composites kept the old path.
    The original attack still worked with every element wrapped in a
    list: 40k distinct one-element lists took 3.9s, 120k did not return,
    and every run reported `steps=5`.

    The reasoning behind the scalars-only split was that keying a
    composite means walking it, which is the cost the hash set exists to
    avoid. That is wrong by a factor of n: *one* deep-equality comparison
    against a composite is also O(size), and the scan performs one per
    element already kept. Paying the walk once per element rather than
    once per pair is what makes it linear. `value_key::ValueKey` now
    projects any value onto a hashable key, composites included. 40k went
    to 0.08s, 120k from a hang to 0.25s.

14. **The higher-order set operations were quadratic and uncharged.**
    `union`, `intersection`, `set-difference`, `set-exclusive-or` and
    `remove-duplicates` each scanned a list per element with deep
    equality. `remove-duplicates`: 30k took 1.0s, 60k 4.0s, 120k 16.6s —
    4x per doubling — reporting **4 steps**. `union` over two 100k lists
    took 24.2s at `steps=6`. These live in `higher_order.rs`, dispatched
    with no `(use ...)` at all, so they are reachable from every script.

    All five now go through the same `ValueKey` set as `uniq`, and charge
    the evaluator for the nodes they walk, so a step budget can see the
    work that remains. Output order and dedup semantics are unchanged.
    `remove-duplicates` at 120k went from 16.6s to 0.17s and from 4 steps
    to 15,004; `union` from 24.2s to 0.35s and from 6 steps to 25,007.

15. **`contains?` over large composite elements did not terminate.** The
    scan is linear in the list but each comparison is O(size) of the
    element, so a list of kilobyte-sized values cost O(n·size) per call
    while the per-call step charge saw only the call.

    Unlike the others this one cannot fix itself: `contains?` is a pure
    stdlib builtin with no access to the fuel meter, and the evaluator
    does not regain control until it returns. The charge is therefore
    applied at the call site, before the call, by `Fuel::charge_scan_cost`
    — the input-size counterpart to the existing `check_predicted_output`.
    The attack now reports the step limit and exits cleanly instead of
    running forever.

These three were recorded as deliberate choices rather than oversights,
and each has since been made the other way.

16. **`map` did not charge for replicating a large value.** The closure's
    result was charged per call, but the assembled list's contents were
    never billed against the cumulative allocation budget, so a value
    returned n times counted once. `(let [big (repeat "x" 1000000)] (map
    (fn [i] big) (range 2000)))` allocated about 2 GB and reported
    `allocated=1032048` — roughly 1 MB — leaving `--max-alloc` unable to
    see it, and aborting outright under a tight `RLIMIT_AS`.

    The charge now happens per element as the result is built, not on the
    finished list: billing at the end only reports a result the process
    survived making, which is the mistake the metering here keeps having
    to unlearn. Each element costs its own size, so n copies cost n times
    — what the process actually allocates, and what an OOM kill responds
    to. The same shape through `mapcan`, `subst-if` and `substitute-if` is
    charged the same way, and a dispatch-level backstop covers the forms
    that return a list without collecting into one. The 2 GB case now
    stops at the 512 MiB default in 0.18s and 531 MB RSS; `--max-alloc
    10000000` stops it at 17 MB.

17. **The 16 MiB output ceiling was not configurable.** `repeat`,
    `pad-left` and `pad-right` shared one hardcoded `MAX_OUT` independent
    of the fuel budget, so `--max-alloc 1000` did not stop a 16 MiB
    allocation inside a single call: the backstop against crashing was
    also, in effect, the allocation policy.

    The evaluator now derives the ceiling from its own limits — the
    tighter of `max_value_len` and what remains of `max_alloc` — and
    publishes it for the duration of each builtin call. It has to travel
    out of band, through a thread-local, because the builtin signature
    (`fn(&[Value]) -> BuiltinResult`) carries no evaluator context and
    changing it would touch every builtin and every host registration.
    16 MiB survives as the default for callers that publish nothing, so
    the anti-crash backstop is never absent. `--max-alloc 1000` now
    rejects a 10 MB `repeat` before allocating, naming the active ceiling.

18. **`pow` and `exp` overflowed to infinity and serialized as `null`.**
    JSON can encode neither infinity nor NaN, so `num_from_f64` mapped
    both to null — the right serialization and the wrong answer, since
    the failure became indistinguishable from a legitimate null result.

    Both now report the overflow. They check the result rather than the
    domain because whether `pow` overflows depends on both arguments
    together; `sqrt` and the logarithms keep their up-front checks, which
    give better messages. This also covers `(pow 0 -1)` and `(pow -1 0.5)`,
    which produced infinity and NaN respectively. Ordinary arithmetic
    (`(/ 1e308 1e-308)`) still yields null: that is a language-wide
    semantic change rather than a fix to these two functions.

    Writing the stronger assertion turned up a bug the old one was too
    weak to see. `asin` and `acos` had no domain check at all — `(asin 2)`
    returned `null`, exactly the silent loss this item is about, while the
    test asserted only that it did not panic. Both now reject arguments
    outside [-1, 1] and name the offending value.

## Known, not fixed

Nothing currently. Entries here are behaviors the tests deliberately do
not fail on; each should say what the behavior is and why living with it
is the better choice.

## Adding a test

Use `run_source` and one of the assertions from `StressTestCase`:

```ruby
def test_my_attack_shape_is_bounded
  assert_source_limited(
    "(some hostile script)",
    /step limit/,                    # which limit should report
    limits: { max_steps: 10_000 },   # optional overrides
    msg: "what should have stopped it"
  )
end
```

Prefer `assert_diagnosable` when several limits could legitimately fire
first and you only care that *something* stopped it cleanly. Reach for
`assert_resource_limited` or `assert_limit_message` when a specific limit
is the point of the test — a test that accepts any error will keep passing
after the guard it covers is removed.

Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
