//! Evaluation resource metering.
//!
//! A tree-walking evaluator handed untrusted source has three ways to be
//! made to run forever or die badly:
//!
//! - **Unbounded work.** `(defn loop-forever [] (loop-forever))` never
//!   terminates. Neither does a `reduce` over a list the script generated
//!   with a very large `repeat`.
//! - **Unbounded native recursion.** Trongoscript's `eval` recurses on the
//!   Rust call stack, so a deeply self-recursive script overflows it. That
//!   is a SIGSEGV, not a `Result::Err` — it takes the host process down and
//!   cannot be caught.
//! - **Unbounded allocation.** `(repeat 100000000 "x")` asks for a value
//!   far larger than RAM. A step counter never fires, because it is one
//!   step.
//!
//! [`Fuel`] meters all three. It lives on the [`Context`](crate::Context)
//! and is checked at the evaluator's chokepoints — every `eval` node visit,
//! every closure call, and every value returned into the evaluator.
//!
//! # Defaults
//!
//! Limits are **on by default** and set generously: a legitimate script
//! doing real work will not hit them, while a runaway one stops in bounded
//! time. See [`FuelLimits::default`] for the numbers.
//!
//! Hosts evaluating untrusted input should lower them. Hosts running only
//! their own trusted scripts, in a batch job where a long run is expected
//! rather than suspicious, can lift them:
//!
//! ```
//! use trongoscript::{Context, EvalConfig, FuelLimits};
//!
//! // Untrusted input: tight budget.
//! let strict = EvalConfig::default().with_fuel_limits(FuelLimits {
//!     max_steps: 100_000,
//!     max_call_depth: 64,
//!     ..FuelLimits::default()
//! });
//!
//! // Trusted batch job: no metering at all.
//! let unmetered = EvalConfig::default().with_fuel_limits(FuelLimits::unlimited());
//! # let _ = (Context::new(strict), Context::new(unmetered));
//! ```
//!
//! # What metering does not do
//!
//! Fuel bounds the *interpreter*. It does not bound a host builtin's
//! internals: a [`HostBuiltin`](crate::HostBuiltin) that blocks on a socket
//! blocks regardless of the budget, because the evaluator never regains
//! control to check. Host builtins that can run long need their own
//! timeouts. Fuel also charges allocation only for values that pass through
//! the evaluator, so it bounds the script's own data, not scratch memory a
//! builtin allocates and frees internally.

use crate::error::{EvalError, EvalResult};
use trongoscript_stdlib::Value;

/// Resource ceilings for one evaluation.
///
/// Every field is a hard cap: crossing it aborts evaluation with an
/// [`EvalError`] rather than degrading. Set a field to its type's `MAX` to
/// disable that individual check, or use [`FuelLimits::unlimited`] to
/// disable all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuelLimits {
    /// Maximum number of evaluation steps. One step is one `eval` node
    /// visit — a literal, a symbol lookup, a special form, or a call. This
    /// is the primary defense against non-terminating scripts.
    pub max_steps: u64,

    /// Maximum depth of nested closure calls. This is the limit a runaway
    /// *recursion* hits, and the one whose error message points at the
    /// actual problem.
    pub max_call_depth: usize,

    /// Maximum depth of nested `eval` recursion.
    ///
    /// This is the true native-stack guard: `eval` recurses on the Rust
    /// stack for nested data, nested forms, *and* function bodies, so this
    /// counter is the one that corresponds to real stack frames. Every
    /// closure call costs several levels of it, which is why the default
    /// sits well above `max_call_depth` — recursion should be reported as
    /// a call-depth problem, leaving this limit to catch what call depth
    /// cannot see: pathologically nested data, such as a literal ten
    /// thousand brackets deep.
    pub max_eval_depth: usize,

    /// Maximum element count of a single collection value, and maximum
    /// byte length of a single string value. Catches a single oversized
    /// allocation, e.g. `(repeat 1e9 "x")`.
    pub max_value_len: usize,

    /// Maximum cumulative allocation charged across the whole evaluation,
    /// in bytes. Catches death by a thousand cuts: many values that each
    /// pass `max_value_len` but together exhaust memory.
    pub max_total_alloc: u64,
}

impl FuelLimits {
    /// Ceilings generous enough that legitimate scripts do not notice them,
    /// but low enough that a runaway one stops in bounded time.
    ///
    /// - `max_steps`: 50 million. A few seconds of tree-walking.
    /// - `max_call_depth`: 2,048. Deeper than hand-written recursion goes,
    ///   with margin under the measured stack ceiling below.
    /// - `max_eval_depth`: 8,192. Set *above* `max_call_depth` on purpose:
    ///   a closure call costs roughly two eval levels, so a lower value
    ///   here would preempt the call-depth check and report runaway
    ///   recursion as a nesting error. This limit exists for the case
    ///   call depth cannot see — deeply nested literal data.
    /// - `max_value_len`: 16 million elements or bytes in one value.
    /// - `max_total_alloc`: 512 MiB charged cumulatively.
    ///
    /// # Depth and the native stack
    ///
    /// `eval` recurses on the native stack, so the safe depth depends on
    /// frame size — which varies by build profile — and on the stack the
    /// evaluator happens to be running on. Measured:
    ///
    /// | stack   | build   | overflows at      |
    /// |---------|---------|-------------------|
    /// | 8 MiB   | release | 3,000–4,000 calls |
    /// | 8 MiB   | debug   | 200–300 calls     |
    /// | 256 MiB | debug   | 8,000–11,000 calls |
    ///
    /// A debug build's frames are an order of magnitude larger, which is
    /// why the CLI runs evaluation on a thread with a large explicit stack
    /// rather than accepting whatever the OS provides. These defaults
    /// assume that arrangement.
    ///
    /// **Embedders calling the evaluator on an ordinary thread get much
    /// less headroom** — a debug build on a default 8 MiB stack overflows
    /// at a couple hundred frames, well below this default. Either spawn a
    /// thread with a matching `stack_size`, or lower `max_call_depth` to
    /// suit the stack you have. Exceeding it is an uncatchable SIGSEGV,
    /// not an error this crate can return.
    pub const DEFAULT: Self = Self {
        max_steps: 50_000_000,
        max_call_depth: 2_048,
        max_eval_depth: 8_192,
        max_value_len: 16 * 1024 * 1024,
        max_total_alloc: 512 * 1024 * 1024,
    };

    /// All checks disabled. Only appropriate for fully trusted input:
    /// a script under these limits can hang or crash the host process.
    ///
    /// The depth ceilings are *not* lifted, because unbounded native
    /// recursion is a memory-safety problem rather than merely a slow one:
    /// a stack overflow is an uncatchable SIGSEGV, not a `Result` any
    /// caller can handle. "Unlimited" here means unlimited *work*, not
    /// unlimited stack.
    ///
    /// The depths stay at the [`DEFAULT`](Self::DEFAULT) values, which are
    /// measured safe for a debug build on an 8 MiB stack. Raising them is
    /// a decision that has to be made against a known build profile and
    /// stack size; see the table on [`DEFAULT`](Self::DEFAULT).
    pub const fn unlimited() -> Self {
        Self {
            max_steps: u64::MAX,
            max_call_depth: Self::DEFAULT.max_call_depth,
            max_eval_depth: Self::DEFAULT.max_eval_depth,
            max_value_len: usize::MAX,
            max_total_alloc: u64::MAX,
        }
    }

    /// The deepest recursion that stays safe on a large interpreter stack.
    ///
    /// Measured on a 256 MiB stack (what the CLI reserves — see
    /// `INTERPRETER_STACK_BYTES`): a debug build, whose frames are the
    /// largest, overflows between 8,000 and 11,000 nested calls. This sits
    /// at roughly half that.
    ///
    /// Anything above this is a configuration that can crash the process
    /// instead of returning an error, so
    /// [`clamp_to_stack_safe`](Self::clamp_to_stack_safe) refuses to
    /// exceed it regardless of what a caller asks for.
    ///
    /// **This assumes a large stack.** An embedder calling the evaluator
    /// directly from a thread with a default-sized stack (often 2–8 MiB)
    /// gets far less: a debug build overflows an 8 MiB stack at only
    /// 200–300 calls. Such hosts should set their own lower ceilings, or
    /// spawn a thread with a matching `stack_size`.
    pub const STACK_SAFE_CALL_DEPTH: usize = 4_096;

    /// Matching ceiling for eval depth. A call costs roughly two eval
    /// levels, so this tracks [`STACK_SAFE_CALL_DEPTH`](Self::STACK_SAFE_CALL_DEPTH)
    /// with headroom for the nesting inside each call.
    pub const STACK_SAFE_EVAL_DEPTH: usize = 16_384;

    /// Lower the depth ceilings to what the native stack can actually
    /// survive, leaving every other field alone.
    ///
    /// Use this on limits that came from user input. A caller asking for
    /// a million frames deep is asking for a stack overflow, which this
    /// crate cannot turn into an error once it happens — so the request is
    /// capped rather than honored. Work budgets (steps, allocation) are
    /// *not* clamped: those fail safely and are the operator's call.
    pub const fn clamp_to_stack_safe(mut self) -> Self {
        if self.max_call_depth > Self::STACK_SAFE_CALL_DEPTH {
            self.max_call_depth = Self::STACK_SAFE_CALL_DEPTH;
        }
        if self.max_eval_depth > Self::STACK_SAFE_EVAL_DEPTH {
            self.max_eval_depth = Self::STACK_SAFE_EVAL_DEPTH;
        }
        self
    }

    /// A budget suitable for untrusted input evaluated in a request path:
    /// 1 million steps, 64 frames deep, 64 Ki elements per value, 16 MiB
    /// total.
    pub const fn strict() -> Self {
        Self {
            max_steps: 1_000_000,
            max_call_depth: 64,
            // Above max_call_depth for the same reason as in DEFAULT: a
            // call costs ~2 eval levels, and recursion should surface as
            // a call-depth error.
            max_eval_depth: 256,
            max_value_len: 64 * 1024,
            max_total_alloc: 16 * 1024 * 1024,
        }
    }
}

impl Default for FuelLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Live metering state for one evaluation.
///
/// Counters only ever move up; [`Fuel::reset`] returns them to zero for a
/// [`Context`](crate::Context) being reused across independent evaluations.
#[derive(Debug, Clone)]
pub struct Fuel {
    limits: FuelLimits,
    steps: u64,
    call_depth: usize,
    eval_depth: usize,
    allocated: u64,
    /// High-water marks, kept for reporting even after `reset`.
    peak_call_depth: usize,
    peak_eval_depth: usize,
}

impl Fuel {
    pub fn new(limits: FuelLimits) -> Self {
        Self {
            limits,
            steps: 0,
            call_depth: 0,
            eval_depth: 0,
            allocated: 0,
            peak_call_depth: 0,
            peak_eval_depth: 0,
        }
    }

    /// The ceilings this meter enforces.
    pub fn limits(&self) -> &FuelLimits {
        &self.limits
    }

    /// Replace the ceilings mid-evaluation. Consumed counters are kept, so
    /// lowering a limit below what has already been spent makes the next
    /// check fail immediately.
    pub fn set_limits(&mut self, limits: FuelLimits) {
        self.limits = limits;
    }

    /// Steps consumed so far.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// Steps left before [`FuelLimits::max_steps`] is hit.
    pub fn steps_remaining(&self) -> u64 {
        self.limits.max_steps.saturating_sub(self.steps)
    }

    /// Bytes charged so far.
    pub fn allocated(&self) -> u64 {
        self.allocated
    }

    /// Current closure call depth.
    pub fn call_depth(&self) -> usize {
        self.call_depth
    }

    /// Deepest closure nesting reached since construction.
    pub fn peak_call_depth(&self) -> usize {
        self.peak_call_depth
    }

    /// Deepest `eval` recursion reached since construction.
    pub fn peak_eval_depth(&self) -> usize {
        self.peak_eval_depth
    }

    /// Zero the consumption counters, keeping the limits and high-water
    /// marks. Use when reusing a `Context` for an unrelated evaluation that
    /// should get a fresh budget.
    pub fn reset(&mut self) {
        self.steps = 0;
        self.allocated = 0;
        self.call_depth = 0;
        self.eval_depth = 0;
    }

    /// Charge one evaluation step.
    ///
    /// # Errors
    ///
    /// [`EvalError::FuelExhausted`] once `max_steps` is reached.
    #[inline]
    pub fn charge_step(&mut self) -> EvalResult<()> {
        self.steps += 1;
        if self.steps > self.limits.max_steps {
            return Err(EvalError::FuelExhausted {
                limit: self.limits.max_steps,
            });
        }
        Ok(())
    }

    /// Charge `n` steps at once, for a builtin whose cost is proportional
    /// to its input (a sort over `n` elements, a `repeat` of `n` copies).
    /// Saturates rather than wrapping.
    ///
    /// # Errors
    ///
    /// [`EvalError::FuelExhausted`] once `max_steps` is reached.
    #[inline]
    pub fn charge_steps(&mut self, n: u64) -> EvalResult<()> {
        self.steps = self.steps.saturating_add(n);
        if self.steps > self.limits.max_steps {
            return Err(EvalError::FuelExhausted {
                limit: self.limits.max_steps,
            });
        }
        Ok(())
    }

    /// Enter one level of `eval` recursion.
    ///
    /// Callers must pair this with [`Fuel::exit_eval`] on **every** path out,
    /// including the error path. Prefer the `metered_eval!` pattern used in
    /// [`crate::eval`] over calling this directly.
    ///
    /// # Errors
    ///
    /// [`EvalError::EvalDepthExceeded`] once `max_eval_depth` is reached.
    #[inline]
    pub fn enter_eval(&mut self) -> EvalResult<()> {
        self.eval_depth += 1;
        if self.eval_depth > self.peak_eval_depth {
            self.peak_eval_depth = self.eval_depth;
        }
        if self.eval_depth > self.limits.max_eval_depth {
            // Unwind our own increment: the caller will not pair an
            // `exit_eval` with a failed `enter_eval`.
            self.eval_depth -= 1;
            return Err(EvalError::EvalDepthExceeded {
                max: self.limits.max_eval_depth,
            });
        }
        Ok(())
    }

    /// Leave one level of `eval` recursion.
    #[inline]
    pub fn exit_eval(&mut self) {
        self.eval_depth = self.eval_depth.saturating_sub(1);
    }

    /// Enter one closure call frame.
    ///
    /// Callers must pair this with [`Fuel::exit_call`] on every path out,
    /// including the error path.
    ///
    /// # Errors
    ///
    /// [`EvalError::CallDepthExceeded`] once `max_call_depth` is reached.
    #[inline]
    pub fn enter_call(&mut self) -> EvalResult<()> {
        self.call_depth += 1;
        if self.call_depth > self.peak_call_depth {
            self.peak_call_depth = self.call_depth;
        }
        if self.call_depth > self.limits.max_call_depth {
            self.call_depth -= 1;
            return Err(EvalError::CallDepthExceeded {
                max: self.limits.max_call_depth,
            });
        }
        Ok(())
    }

    /// Leave one closure call frame.
    #[inline]
    pub fn exit_call(&mut self) {
        self.call_depth = self.call_depth.saturating_sub(1);
    }

    /// Charge `bytes` against the cumulative allocation budget.
    ///
    /// # Errors
    ///
    /// [`EvalError::AllocationExceeded`] once `max_total_alloc` is reached.
    #[inline]
    pub fn charge_alloc(&mut self, bytes: u64) -> EvalResult<()> {
        self.allocated = self.allocated.saturating_add(bytes);
        if self.allocated > self.limits.max_total_alloc {
            return Err(EvalError::AllocationExceeded {
                limit: self.limits.max_total_alloc,
            });
        }
        Ok(())
    }

    /// Check a value against [`FuelLimits::max_value_len`] and charge its
    /// size against the cumulative allocation budget.
    ///
    /// Scalars are cheap to measure and cheap to charge. Collections are
    /// measured with [`value_cost`], which walks them — so this is called
    /// on values *returned into* the evaluator, not on every value that
    /// passes through it.
    ///
    /// # Errors
    ///
    /// [`EvalError::ValueTooLarge`] if any single collection or string in
    /// `v` exceeds `max_value_len`; [`EvalError::AllocationExceeded`] if the
    /// cumulative budget is spent.
    pub fn charge_value(&mut self, v: &Value) -> EvalResult<()> {
        check_value_len(v, self.limits.max_value_len)?;
        self.charge_alloc(value_cost(v))
    }

    /// Reject a builtin call whose output would exceed the limits *before*
    /// it runs, for the builtins whose output size can be predicted from
    /// their arguments (see [`predicted_output_len`]).
    ///
    /// Checking a result after the fact is not enough for these: the
    /// allocation happens inside the builtin, so an oversized request can
    /// exhaust memory and get the process OOM-killed before the evaluator
    /// regains control. A killed process reports nothing; this returns an
    /// error instead.
    ///
    /// # Errors
    ///
    /// [`EvalError::ValueTooLarge`] if the predicted length exceeds
    /// `max_value_len`; [`EvalError::AllocationExceeded`] if it would
    /// exhaust the cumulative budget.
    pub fn check_predicted_output(&mut self, name: &str, args: &[Value]) -> EvalResult<()> {
        let Some(len) = predicted_output_len(name, args) else {
            return Ok(());
        };
        if len > self.limits.max_value_len as u64 {
            return Err(EvalError::ValueTooLarge {
                kind: "list",
                len: len.min(usize::MAX as u64) as usize,
                max: self.limits.max_value_len,
            });
        }
        // Would the allocation itself blow the cumulative budget? Check
        // without committing the charge — the real cost is charged from
        // the actual result once the builtin returns.
        let projected = self
            .allocated
            .saturating_add(container_overhead(len.min(usize::MAX as u64) as usize));
        if projected > self.limits.max_total_alloc {
            return Err(EvalError::AllocationExceeded {
                limit: self.limits.max_total_alloc,
            });
        }
        Ok(())
    }

    /// The largest output a single builtin call may construct, in bytes.
    ///
    /// Builtins that size their output from an argument (`repeat`,
    /// `pad-left`, `pad-right`) must reject an oversized request *before*
    /// allocating, since the budget is only charged once they return. They
    /// have no access to this meter, so the evaluator publishes the number
    /// for them via [`trongoscript_stdlib::output_limit`].
    ///
    /// It is the tighter of `max_value_len` — the ceiling on any one value
    /// — and what is left of the cumulative allocation budget, because a
    /// call cannot legitimately produce more than either would permit.
    /// Deriving it this way is what makes `--max-alloc 1000` able to stop
    /// a 16 MiB allocation inside one call; that figure used to be
    /// hardcoded and independent of every configured limit.
    pub fn single_output_limit(&self) -> usize {
        let remaining = self.limits.max_total_alloc.saturating_sub(self.allocated);
        let remaining = usize::try_from(remaining).unwrap_or(usize::MAX);
        self.limits.max_value_len.min(remaining)
    }

    /// Charge for work a builtin does *inside* one call, proportional to
    /// the size of its arguments rather than their count.
    ///
    /// The step counter charges per call, so a builtin that scans a list
    /// comparing whole values costs one step no matter how much data it
    /// touched. `(contains? big-list big-value)` is linear in the list
    /// and linear again in the size of each element compared, and the
    /// evaluator does not regain control until it returns — so the bound
    /// has to be applied here, before the call.
    ///
    /// # Errors
    ///
    /// [`EvalError::StepLimitExceeded`] if the scan would exhaust the
    /// step budget.
    pub fn charge_scan_cost(&mut self, name: &str, args: &[Value]) -> EvalResult<()> {
        let Some(nodes) = scan_node_cost(name, args) else {
            return Ok(());
        };
        self.charge_steps(nodes / SCAN_NODES_PER_STEP)
    }
}

/// Nodes a scanning builtin may walk per step charged.
///
/// Walking one node is much cheaper than evaluating an expression, so the
/// charge is a fraction of a step. What matters is that it grows with the
/// work, not that it matches it exactly.
const SCAN_NODES_PER_STEP: u64 = 8;

/// Nodes a builtin will walk, for the ones whose cost is proportional to
/// the *size* of their arguments rather than the number of them.
///
/// Returns `None` when the builtin is not one of these, or when the
/// argument shapes do not match what it scans — in which case the call
/// fails on its own terms and there is nothing to charge for.
fn scan_node_cost(name: &str, args: &[Value]) -> Option<u64> {
    match name {
        // (contains? coll v): a linear scan of `coll`, but each element
        // is compared against `v` with deep equality, so the real cost is
        // the number of elements times the size of the comparison. The
        // comparison short-circuits at the first difference, so this is
        // an upper bound — which is the direction a guard should err.
        "contains?" => {
            let [coll, needle] = args else { return None };
            let len = match coll {
                // A map lookup is by key and does not scan.
                Value::Array(items) => items.len() as u64,
                _ => return None,
            };
            let probe = probe_node_count(needle);
            Some(len.saturating_mul(probe))
        }
        _ => None,
    }
}

/// Nodes in `v`, counted up to [`MAX_WALK_NODES`].
///
/// Bounded for the same reason [`value_cost`] is: the measurement must
/// not become the expensive operation. An undercount only lets a call
/// through cheaply; an unbounded walk would be the attack itself.
fn probe_node_count(v: &Value) -> u64 {
    fn walk(v: &Value, budget: &mut u32) -> u64 {
        if *budget == 0 {
            return 0;
        }
        *budget -= 1;
        match v {
            Value::Null | Value::Bool(_) | Value::Number(_) => 1,
            // Long strings compare byte-wise; bill them by length so a
            // list of megabyte strings is not counted as one node each.
            Value::String(s) => 1 + (s.len() / 64) as u64,
            Value::Array(items) => items
                .iter()
                .fold(1u64, |acc, it| acc.saturating_add(walk(it, budget))),
            Value::Object(map) => map
                .values()
                .fold(1u64, |acc, val| acc.saturating_add(walk(val, budget))),
        }
    }
    let mut budget = MAX_WALK_NODES;
    walk(v, &mut budget)
}

impl Default for Fuel {
    fn default() -> Self {
        Self::new(FuelLimits::default())
    }
}

/// Per-value overhead charged on top of contents, approximating the
/// allocation a `serde_json::Value` actually makes. Deliberately rough: the
/// point is to bound growth, not to measure the heap exactly.
const VALUE_OVERHEAD: u64 = 16;

/// Predict how many elements a builtin will produce, for the builtins
/// whose output size is knowable from their arguments alone.
///
/// Charging a builtin's result *after* it returns is too late for the ones
/// that can be asked for an enormous output: `(range 100000000)` allocates
/// gigabytes inside the builtin and can be OOM-killed before the evaluator
/// ever regains control to check the result. For those, the size has to be
/// rejected before the call happens.
///
/// Returns `None` when the output size cannot be predicted — the caller
/// then falls back to charging the result afterwards, which is correct for
/// everything that cannot blow up in a single call.
pub fn predicted_output_len(name: &str, args: &[Value]) -> Option<u64> {
    /// Interpret a numeric argument as a non-negative count.
    fn count_arg(v: &Value) -> Option<u64> {
        v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)
    }

    match name {
        // (range end) / (range start end) / (range start end step)
        "range" => match args {
            [end] => count_arg(end),
            [start, end] => {
                let (s, e) = (start.as_f64()?, end.as_f64()?);
                Some((e - s).max(0.0) as u64)
            }
            [start, end, step] => {
                let (s, e, st) = (start.as_f64()?, end.as_f64()?, step.as_f64()?);
                if st == 0.0 {
                    return None;
                }
                Some(((e - s) / st).max(0.0) as u64)
            }
            _ => None,
        },
        // Concatenation: the sum of the input lengths.
        "concat" => {
            let mut total: u64 = 0;
            for a in args {
                total = total.saturating_add(a.as_array()?.len() as u64);
            }
            Some(total)
        }
        _ => None,
    }
}

/// Steps to charge for deep-copying `v`, as happens when a value is bound
/// to a closure parameter.
///
/// [`Value`] is a tree, so cloning one is O(size) — but the evaluator sees
/// only "one call" and charges one step. A script can exploit that gap by
/// feeding its own accumulated result back in each iteration, which is
/// quadratic in real time while linear in steps.
///
/// The estimate must be cheap, since it runs on every argument of every
/// call — a full traversal here would reintroduce the quadratic cost it
/// exists to charge for. It walks at most [`COPY_PROBE_NODES`] nodes,
/// following the largest child at each level, and extrapolates. That is
/// enough to distinguish the two shapes that matter:
///
/// - a wide value (`[1 2 3 ... 10000]`) is caught by its top-level length;
/// - a deep value (`[[[[...]]]]`, as built by `(reduce (fn [acc x] [acc]))`)
///   is caught by its depth, which a top-level-only check cannot see —
///   every level is a one-element array, yet copying the whole thing is
///   O(depth).
pub fn copy_step_cost(v: &Value) -> u64 {
    /// Copies below this size are ordinary and not worth charging beyond
    /// the one step the call already costs.
    const FREE_THRESHOLD: u64 = 32;

    let (width, depth) = probe_shape(v);
    // A copy touches at least one node per level and one per top-level
    // element. Charging their sum catches both the wide and deep shapes
    // without walking the whole structure.
    width.saturating_add(depth).saturating_sub(FREE_THRESHOLD)
}

/// How many levels [`copy_step_cost`] may descend.
///
/// The probe follows a single-child chain, so each step is a pointer
/// dereference — cheap, and it only runs that far when the value really
/// is that deep. The cap needs to exceed the nesting a script can build
/// before its step budget is spent, or deep values are undercharged by an
/// unbounded factor and the guard never fires.
const COPY_PROBE_NODES: u32 = 100_000;

/// Estimate `(width, depth)` of `v`, descending through the largest child
/// at each level and stopping after [`COPY_PROBE_NODES`] steps.
///
/// When the probe runs out before reaching the bottom, the depth returned
/// is the distance covered — an undercount, not an overcount, so a value
/// that is merely large is never mistaken for one that is hostile.
fn probe_shape(v: &Value) -> (u64, u64) {
    let width = match v {
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
        Value::String(s) => (s.len() / 64) as u64,
        Value::Array(items) => items.len() as u64,
        Value::Object(map) => map.len() as u64,
    };

    let mut depth: u64 = 0;
    let mut cursor = v;
    for _ in 0..COPY_PROBE_NODES {
        let next = match cursor {
            // Follow the single child of a one-element wrapper; that is
            // the exact shape a nesting bomb produces.
            Value::Array(items) if items.len() == 1 => &items[0],
            Value::Object(map) if map.len() == 1 => map.values().next().unwrap(),
            _ => break,
        };
        depth += 1;
        cursor = next;
    }
    (width, depth)
}

/// Cost of a collection with `len` entries, excluding its contents.
///
/// Used where the elements have already been charged individually (a
/// vector or map literal, whose elements were each charged as they were
/// evaluated), so only the container itself is billed. Charging
/// [`value_cost`] there instead would bill nested data once per level of
/// nesting.
pub fn container_overhead(len: usize) -> u64 {
    VALUE_OVERHEAD.saturating_add((len as u64).saturating_mul(VALUE_OVERHEAD))
}

/// How many nodes [`value_cost`] and [`check_value_len`] will visit before
/// giving up and returning what they have.
///
/// Metering must not itself become the expensive operation. Walking a
/// value in full is O(size), and it happens on every builtin return — so a
/// script that repeatedly nests its own output (`(reduce (fn [acc x]
/// [acc]) ...)`) would make the *meter* quadratic, turning the guard into
/// the denial of service it was meant to prevent. Measured before this
/// bound: 10,000 such iterations took 5.8s and grew with the square of the
/// input.
///
/// Stopping early is safe in the direction that matters. An under-count
/// can only let a value through that a later charge will catch, whereas an
/// unbounded walk is itself the attack.
const MAX_WALK_NODES: u32 = 100_000;

/// Approximate heap cost of `v` in bytes, walking nested collections.
///
/// Used to charge cumulative allocation. The numbers are an approximation —
/// they ignore capacity slack and map bookkeeping — but they are
/// proportional to real usage, which is what a budget needs.
///
/// The traversal stops after [`MAX_WALK_NODES`] nodes; see that constant
/// for why bounding this matters.
pub fn value_cost(v: &Value) -> u64 {
    let mut budget = MAX_WALK_NODES;
    value_cost_bounded(v, &mut budget)
}

fn value_cost_bounded(v: &Value, budget: &mut u32) -> u64 {
    if *budget == 0 {
        return 0;
    }
    *budget -= 1;
    match v {
        Value::Null | Value::Bool(_) | Value::Number(_) => VALUE_OVERHEAD,
        Value::String(s) => VALUE_OVERHEAD + s.len() as u64,
        Value::Array(items) => items.iter().fold(VALUE_OVERHEAD, |acc, it| {
            acc.saturating_add(value_cost_bounded(it, budget))
        }),
        Value::Object(map) => map.iter().fold(VALUE_OVERHEAD, |acc, (k, val)| {
            acc.saturating_add(k.len() as u64)
                .saturating_add(value_cost_bounded(val, budget))
        }),
    }
}

/// Verify no single collection or string inside `v` exceeds `max_len`.
///
/// Recurses into nested collections, so an array of 10 elements each
/// holding an oversized string is caught.
///
/// # Errors
///
/// [`EvalError::ValueTooLarge`] naming the offending kind and its size.
pub fn check_value_len(v: &Value, max_len: usize) -> EvalResult<()> {
    let mut budget = MAX_WALK_NODES;
    check_value_len_bounded(v, max_len, &mut budget)
}

fn check_value_len_bounded(v: &Value, max_len: usize, budget: &mut u32) -> EvalResult<()> {
    // Bounded for two reasons: the walk must not become quadratic when
    // called on every builtin return (see MAX_WALK_NODES), and this
    // function recurses, so an adversarially nested value would otherwise
    // overflow the stack inside the very check meant to prevent that.
    if *budget == 0 {
        return Ok(());
    }
    *budget -= 1;
    match v {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(s) => {
            if s.len() > max_len {
                Err(EvalError::ValueTooLarge {
                    kind: "string",
                    len: s.len(),
                    max: max_len,
                })
            } else {
                Ok(())
            }
        }
        Value::Array(items) => {
            if items.len() > max_len {
                return Err(EvalError::ValueTooLarge {
                    kind: "list",
                    len: items.len(),
                    max: max_len,
                });
            }
            for it in items {
                check_value_len_bounded(it, max_len, budget)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            if map.len() > max_len {
                return Err(EvalError::ValueTooLarge {
                    kind: "map",
                    len: map.len(),
                    max: max_len,
                });
            }
            for (_, val) in map {
                check_value_len_bounded(val, max_len, budget)?;
            }
            Ok(())
        }
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
