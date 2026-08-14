//! Higher-order builtins.
//!
//! These functions need access to the live [`Context`] because they
//! invoke user-defined closures (whose call requires the evaluator and
//! the closure registry). That's why they live in this crate rather than
//! in the pure-stdlib registry.
//!
//! Surface includes `map` / `filter` / `reduce` plus a Common-Lisp-style
//! family (`every`, `some`, `find-if`, `position-if`, `subst-if`,
//! `complement`, `apply`, etc.). Destructive variants (`delete-if`,
//! `nsubst-if`, ...) and `multiple-value-call` are deliberately *not*
//! provided — Trongoscript values are JSON-shaped and immutable from the
//! script's point of view, and there is no multiple-values mechanism.
//! CL's `merge` is omitted because the name collides with the existing
//! map-merging builtin.

use trongoscript_parser::Expr;
use trongoscript_stdlib::Value;

use crate::error::{EvalError, EvalResult};
use crate::eval::{
    call_closure, coerce_to_fn, coerce_to_list, eval, eval_args, type_name_v, value_to_expr,
    Closure, Context,
};
use crate::value::is_truthy;

/// Dispatch a higher-order builtin by name. Returns `None` if `name` is
/// not one of ours, so the caller can fall through to the next lookup.
pub fn try_dispatch(
    name: &str,
    arg_exprs: &[Expr],
    ctx: &mut Context,
) -> Option<EvalResult<Value>> {
    let r = match name {
        // Core mapping / folding.
        "map" | "mapcar" => map_(arg_exprs, ctx),
        "mapc" => mapc(arg_exprs, ctx),
        "maplist" => maplist(arg_exprs, ctx),
        "mapl" => mapl(arg_exprs, ctx),
        "mapcan" => mapcan(arg_exprs, ctx),
        "mapcon" => mapcon(arg_exprs, ctx),
        "filter" => filter_(arg_exprs, ctx),
        "reduce" => reduce_(arg_exprs, ctx),

        // Universal / existential predicates.
        "every" => every(arg_exprs, ctx, false),
        "some" => some(arg_exprs, ctx, false),
        "notany" => some(arg_exprs, ctx, true),
        "notevery" => every(arg_exprs, ctx, true),

        // Counting / finding.
        "count-if" => count_if(arg_exprs, ctx, false),
        "count-if-not" => count_if(arg_exprs, ctx, true),
        "find-if" => find_if(arg_exprs, ctx, false),
        "find-if-not" => find_if(arg_exprs, ctx, true),
        "position-if" => position_if(arg_exprs, ctx, false),
        "position-if-not" => position_if(arg_exprs, ctx, true),
        "member-if" => member_if(arg_exprs, ctx, false),
        "member-if-not" => member_if(arg_exprs, ctx, true),

        // Filtering.
        "remove-if" => remove_if(arg_exprs, ctx, false),
        "remove-if-not" => remove_if(arg_exprs, ctx, true),

        // Association lists.
        "assoc-if" => assoc_if(arg_exprs, ctx, false),
        "assoc-if-not" => assoc_if(arg_exprs, ctx, true),
        "rassoc-if" => rassoc_if(arg_exprs, ctx, false),
        "rassoc-if-not" => rassoc_if(arg_exprs, ctx, true),

        // Tree / sequence substitution.
        "subst-if" => subst_if(arg_exprs, ctx, false),
        "subst-if-not" => subst_if(arg_exprs, ctx, true),
        "substitute-if" => substitute_if(arg_exprs, ctx, false),
        "substitute-if-not" => substitute_if(arg_exprs, ctx, true),

        // Sorting.
        "sort" | "stable-sort" => sort_(arg_exprs, ctx),

        // Function combinators / call dispatch.
        "funcall" => funcall(arg_exprs, ctx),
        "apply" => apply(arg_exprs, ctx),
        "identity" => identity(arg_exprs, ctx),
        "constantly" => constantly(arg_exprs, ctx),
        "complement" => complement(arg_exprs, ctx),

        // Set / dedup operations.
        "remove-duplicates" => remove_duplicates(arg_exprs, ctx),
        "tree-equal" => tree_equal(arg_exprs, ctx),
        "union" => set_op(arg_exprs, ctx, "union", SetOp::Union),
        "intersection" => set_op(arg_exprs, ctx, "intersection", SetOp::Intersection),
        "set-difference" => set_op(arg_exprs, ctx, "set-difference", SetOp::Difference),
        "set-exclusive-or" => set_op(arg_exprs, ctx, "set-exclusive-or", SetOp::ExclusiveOr),

        _ => return None,
    };
    // Backstop charge for the assembled result.
    //
    // The collecting forms below bill each element as they build (see
    // `Collector`), which is what stops an oversized result before the
    // memory is committed. This catches whatever returns a list without
    // going through one — the slicing forms, and anything added later.
    // Double-charging a collected list is deliberate: over-charging can
    // only stop a script early, while under-charging is the failure mode
    // that gets a process OOM-killed.
    let r = r.and_then(|out| {
        ctx.fuel.charge_value(&out)?;
        Ok(out)
    });
    Some(r)
}

/// A result list that charges the allocation budget as it grows.
///
/// Nothing used to bill these forms for what they produced:
/// `call_closure` charges *steps* for copying an argument in, but no one
/// charged *bytes* for the list coming out. A closure returning the same
/// large captured value each time made that visible — `(let [big (repeat
/// "x" 1000000)] (map (fn [i] big) (range 2000)))` allocated about 2 GB
/// and reported about 1 MB, leaving `--max-alloc` unable to see it and a
/// tight `RLIMIT_AS` to abort the process instead.
///
/// The charge has to happen per element rather than on the finished list.
/// Billing at the end is the mistake the metering here keeps having to
/// unlearn: by then the allocation already happened, so an oversized
/// result is reported only if the process survived making it.
///
/// Each element is counted as it appears, so a value pushed n times costs
/// n times its size. That is what the process actually allocates, and
/// what an out-of-memory kill responds to.
struct Collector {
    out: Vec<Value>,
}

impl Collector {
    fn new() -> Self {
        Collector { out: Vec::new() }
    }

    fn with_capacity(n: usize) -> Self {
        Collector {
            out: Vec::with_capacity(n),
        }
    }

    /// Charge for `v`, then append it.
    fn push(&mut self, v: Value, ctx: &mut Context) -> EvalResult<()> {
        ctx.fuel.charge_alloc(crate::fuel::value_cost(&v))?;
        self.out.push(v);
        Ok(())
    }

    /// Charge for every element of `vs`, then append them all.
    fn extend(&mut self, vs: impl IntoIterator<Item = Value>, ctx: &mut Context) -> EvalResult<()> {
        for v in vs {
            self.push(v, ctx)?;
        }
        Ok(())
    }

    fn finish(self) -> Value {
        Value::Array(self.out)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn arity_exact(name: &str, expected: usize, actual: usize) -> EvalResult<()> {
    if actual != expected {
        return Err(EvalError::Arity {
            function: name.into(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

fn arity_between(name: &str, lo: usize, hi: usize, actual: usize) -> EvalResult<()> {
    if actual < lo || actual > hi {
        return Err(EvalError::Arity {
            function: name.into(),
            expected: if lo == hi {
                lo.to_string()
            } else {
                format!("{}-{}", lo, hi)
            },
            actual,
        });
    }
    Ok(())
}

fn arity_at_least(name: &str, lo: usize, actual: usize) -> EvalResult<()> {
    if actual < lo {
        return Err(EvalError::Arity {
            function: name.into(),
            expected: format!(">={}", lo),
            actual,
        });
    }
    Ok(())
}

/// Run a unary predicate closure, optionally inverting truthiness.
fn pred_holds(handle: usize, value: &Value, ctx: &mut Context, invert: bool) -> EvalResult<bool> {
    let r = call_closure(handle, std::slice::from_ref(value), ctx)?;
    Ok(if invert {
        !is_truthy(&r)
    } else {
        is_truthy(&r)
    })
}

// ---------------------------------------------------------------------------
// Core mapping / folding
// ---------------------------------------------------------------------------

fn map_(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("map", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("map", &f)?;
    let items = coerce_to_list("map", &xs)?;
    let mut out = Collector::with_capacity(items.len());
    for it in items {
        let v = call_closure(handle, &[it], ctx)?;
        out.push(v, ctx)?;
    }
    Ok(out.finish())
}

/// Call `f` on each element for side-effect; return the original list.
fn mapc(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("mapc", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("mapc", &f)?;
    let items = coerce_to_list("mapc", &xs)?;
    for it in &items {
        call_closure(handle, std::slice::from_ref(it), ctx)?;
    }
    Ok(Value::Array(items))
}

/// `maplist`: call `f` on successive suffixes (cdrs) of the list and
/// collect the results.
fn maplist(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("maplist", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("maplist", &f)?;
    let items = coerce_to_list("maplist", &xs)?;
    let mut out = Collector::with_capacity(items.len());
    for i in 0..items.len() {
        // Each suffix is a fresh copy, so this loop allocates O(n^2) even
        // though it makes only n calls. Charge the copy explicitly.
        let suffix = Value::Array(items[i..].to_vec());
        ctx.fuel
            .charge_alloc(crate::fuel::container_overhead(items.len() - i))?;
        let v = call_closure(handle, &[suffix], ctx)?;
        out.push(v, ctx)?;
    }
    Ok(out.finish())
}

/// `mapl`: like `maplist` but for side-effect; returns the original list.
fn mapl(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("mapl", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("mapl", &f)?;
    let items = coerce_to_list("mapl", &xs)?;
    for i in 0..items.len() {
        let suffix = Value::Array(items[i..].to_vec());
        ctx.fuel
            .charge_alloc(crate::fuel::container_overhead(items.len() - i))?;
        call_closure(handle, &[suffix], ctx)?;
    }
    Ok(Value::Array(items))
}

/// `mapcan`: like `map`, but each call must return a list; results are
/// concatenated. (Non-destructive variant of CL's mapcan.)
fn mapcan(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("mapcan", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("mapcan", &f)?;
    let items = coerce_to_list("mapcan", &xs)?;
    let mut out = Collector::new();
    for it in items {
        let part = call_closure(handle, &[it], ctx)?;
        let piece = coerce_to_list("mapcan", &part)?;
        out.extend(piece, ctx)?;
    }
    Ok(out.finish())
}

/// `mapcon`: like `maplist` but concatenate. Each call must return a list.
fn mapcon(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("mapcon", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("mapcon", &f)?;
    let items = coerce_to_list("mapcon", &xs)?;
    let mut out = Collector::new();
    for i in 0..items.len() {
        let suffix = Value::Array(items[i..].to_vec());
        ctx.fuel
            .charge_alloc(crate::fuel::container_overhead(items.len() - i))?;
        let part = call_closure(handle, &[suffix], ctx)?;
        let piece = coerce_to_list("mapcon", &part)?;
        out.extend(piece, ctx)?;
    }
    Ok(out.finish())
}

fn filter_(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("filter", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("filter", &f)?;
    let items = coerce_to_list("filter", &xs)?;
    let mut out = Vec::new();
    for it in items {
        if pred_holds(handle, &it, ctx, false)? {
            out.push(it);
        }
    }
    Ok(Value::Array(out))
}

fn reduce_(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_between("reduce", 2, 3, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let handle = coerce_to_fn("reduce", &f)?;
    let (mut acc, items): (Value, Vec<Value>) = if arg_exprs.len() == 2 {
        let xs = eval(&arg_exprs[1], ctx)?;
        let items = coerce_to_list("reduce", &xs)?;
        let mut iter = items.into_iter();
        let seed = iter.next().ok_or_else(|| {
            EvalError::InvalidForm(
                "reduce of an empty list requires an explicit initial value".into(),
            )
        })?;
        (seed, iter.collect())
    } else {
        let init = eval(&arg_exprs[1], ctx)?;
        let xs = eval(&arg_exprs[2], ctx)?;
        let items = coerce_to_list("reduce", &xs)?;
        (init, items)
    };
    for it in items {
        acc = call_closure(handle, &[acc, it], ctx)?;
    }
    Ok(acc)
}

// ---------------------------------------------------------------------------
// Universal / existential predicates
// ---------------------------------------------------------------------------

fn every(arg_exprs: &[Expr], ctx: &mut Context, invert_each: bool) -> EvalResult<Value> {
    let fname = if invert_each { "notevery" } else { "every" };
    arity_exact(fname, 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn(fname, &f)?;
    let items = coerce_to_list(fname, &xs)?;
    // `every`     = ∀ pred(x)
    // `notevery`  = ¬∀ pred(x)  ⇔  ∃ ¬pred(x)
    for it in items {
        let holds = pred_holds(handle, &it, ctx, false)?;
        if invert_each {
            if !holds {
                return Ok(Value::Bool(true));
            }
        } else if !holds {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(!invert_each))
}

fn some(arg_exprs: &[Expr], ctx: &mut Context, invert_result: bool) -> EvalResult<Value> {
    let fname = if invert_result { "notany" } else { "some" };
    arity_exact(fname, 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn(fname, &f)?;
    let items = coerce_to_list(fname, &xs)?;
    // `some`   returns the first truthy pred value (CL semantics).
    // `notany` returns true iff every pred(x) is falsy.
    for it in items {
        let r = call_closure(handle, &[it], ctx)?;
        if is_truthy(&r) {
            return Ok(if invert_result { Value::Bool(false) } else { r });
        }
    }
    Ok(if invert_result {
        Value::Bool(true)
    } else {
        Value::Null
    })
}

// ---------------------------------------------------------------------------
// Counting / finding
// ---------------------------------------------------------------------------

fn count_if(arg_exprs: &[Expr], ctx: &mut Context, invert: bool) -> EvalResult<Value> {
    let fname = if invert { "count-if-not" } else { "count-if" };
    arity_exact(fname, 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn(fname, &f)?;
    let items = coerce_to_list(fname, &xs)?;
    let mut n: i64 = 0;
    for it in items {
        if pred_holds(handle, &it, ctx, invert)? {
            n += 1;
        }
    }
    Ok(Value::Number(n.into()))
}

fn find_if(arg_exprs: &[Expr], ctx: &mut Context, invert: bool) -> EvalResult<Value> {
    let fname = if invert { "find-if-not" } else { "find-if" };
    arity_exact(fname, 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn(fname, &f)?;
    let items = coerce_to_list(fname, &xs)?;
    for it in items {
        if pred_holds(handle, &it, ctx, invert)? {
            return Ok(it);
        }
    }
    Ok(Value::Null)
}

fn position_if(arg_exprs: &[Expr], ctx: &mut Context, invert: bool) -> EvalResult<Value> {
    let fname = if invert {
        "position-if-not"
    } else {
        "position-if"
    };
    arity_exact(fname, 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn(fname, &f)?;
    let items = coerce_to_list(fname, &xs)?;
    for (i, it) in items.into_iter().enumerate() {
        if pred_holds(handle, &it, ctx, invert)? {
            return Ok(Value::Number((i as i64).into()));
        }
    }
    Ok(Value::Null)
}

fn member_if(arg_exprs: &[Expr], ctx: &mut Context, invert: bool) -> EvalResult<Value> {
    let fname = if invert { "member-if-not" } else { "member-if" };
    arity_exact(fname, 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn(fname, &f)?;
    let items = coerce_to_list(fname, &xs)?;
    for i in 0..items.len() {
        if pred_holds(handle, &items[i], ctx, invert)? {
            return Ok(Value::Array(items[i..].to_vec()));
        }
    }
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

fn remove_if(arg_exprs: &[Expr], ctx: &mut Context, invert: bool) -> EvalResult<Value> {
    let fname = if invert { "remove-if-not" } else { "remove-if" };
    arity_exact(fname, 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn(fname, &f)?;
    let items = coerce_to_list(fname, &xs)?;
    let mut out = Vec::new();
    for it in items {
        let drop = pred_holds(handle, &it, ctx, invert)?;
        if !drop {
            out.push(it);
        }
    }
    Ok(Value::Array(out))
}

// ---------------------------------------------------------------------------
// Association lists
//
// An "alist" in Trongoscript is a list of two-element lists: `[[k v]
// [k v] ...]`. `assoc-if` matches against the key (first element),
// `rassoc-if` matches against the value (second element). Returns the
// matching pair, or nil if no match.
// ---------------------------------------------------------------------------

fn pair_slot(pair: &Value, slot: usize, fname: &str) -> EvalResult<Value> {
    let arr = match pair {
        Value::Array(items) => items,
        other => {
            return Err(EvalError::Type {
                expected: format!("alist of [k v] pairs (in `{}`)", fname),
                got: type_name_v(other).into(),
            })
        }
    };
    arr.get(slot).cloned().ok_or_else(|| EvalError::Type {
        expected: format!("alist entry with both k and v (in `{}`)", fname),
        got: "shorter array".into(),
    })
}

fn assoc_if(arg_exprs: &[Expr], ctx: &mut Context, invert: bool) -> EvalResult<Value> {
    let fname = if invert { "assoc-if-not" } else { "assoc-if" };
    arity_exact(fname, 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn(fname, &f)?;
    let pairs = coerce_to_list(fname, &xs)?;
    for pair in pairs {
        let key = pair_slot(&pair, 0, fname)?;
        if pred_holds(handle, &key, ctx, invert)? {
            return Ok(pair);
        }
    }
    Ok(Value::Null)
}

fn rassoc_if(arg_exprs: &[Expr], ctx: &mut Context, invert: bool) -> EvalResult<Value> {
    let fname = if invert { "rassoc-if-not" } else { "rassoc-if" };
    arity_exact(fname, 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn(fname, &f)?;
    let pairs = coerce_to_list(fname, &xs)?;
    for pair in pairs {
        let val = pair_slot(&pair, 1, fname)?;
        if pred_holds(handle, &val, ctx, invert)? {
            return Ok(pair);
        }
    }
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Tree / sequence substitution
//
// `subst-if`     : walk the entire value tree; replace any node where
//                  (pred node) holds with `new`. Recurses into arrays
//                  and objects.
// `substitute-if`: top-level (flat) sequence: walk the list elements;
//                  replace each elt where (pred elt) holds with `new`.
// ---------------------------------------------------------------------------

fn subst_if(arg_exprs: &[Expr], ctx: &mut Context, invert: bool) -> EvalResult<Value> {
    let fname = if invert { "subst-if-not" } else { "subst-if" };
    arity_exact(fname, 3, arg_exprs.len())?;
    let new_v = eval(&arg_exprs[0], ctx)?;
    let pred = eval(&arg_exprs[1], ctx)?;
    let tree = eval(&arg_exprs[2], ctx)?;
    let handle = coerce_to_fn(fname, &pred)?;
    subst_walk(handle, &new_v, tree, ctx, invert)
}

fn subst_walk(
    handle: usize,
    new_v: &Value,
    node: Value,
    ctx: &mut Context,
    invert: bool,
) -> EvalResult<Value> {
    if pred_holds(handle, &node, ctx, invert)? {
        // The replacement can be arbitrarily large and is substituted once
        // per matching node, so a small tree and a big `new_v` amplify.
        // Charge each copy as it is made rather than billing the finished
        // tree, which would only report a result the process survived.
        ctx.fuel.charge_alloc(crate::fuel::value_cost(new_v))?;
        return Ok(new_v.clone());
    }
    match node {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(subst_walk(handle, new_v, it, ctx, invert)?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, subst_walk(handle, new_v, v, ctx, invert)?);
            }
            Ok(Value::Object(out))
        }
        atom => Ok(atom),
    }
}

fn substitute_if(arg_exprs: &[Expr], ctx: &mut Context, invert: bool) -> EvalResult<Value> {
    let fname = if invert {
        "substitute-if-not"
    } else {
        "substitute-if"
    };
    arity_exact(fname, 3, arg_exprs.len())?;
    let new_v = eval(&arg_exprs[0], ctx)?;
    let pred = eval(&arg_exprs[1], ctx)?;
    let xs = eval(&arg_exprs[2], ctx)?;
    let handle = coerce_to_fn(fname, &pred)?;
    let items = coerce_to_list(fname, &xs)?;
    // Same amplification as `subst-if`: one large replacement can be
    // copied once per element, so the collector charges each copy as it
    // lands. Elements kept from the input cost their own size, which is
    // already accounted for, but charging them again is the safe
    // direction — it can only stop a script early.
    let mut out = Collector::with_capacity(items.len());
    for it in items {
        if pred_holds(handle, &it, ctx, invert)? {
            out.push(new_v.clone(), ctx)?;
        } else {
            out.push(it, ctx)?;
        }
    }
    Ok(out.finish())
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

/// `(sort pred xs)` — pred is a 2-arg "is-less-than" comparator. Stable.
fn sort_(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("sort", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("sort", &f)?;
    let mut items = coerce_to_list("sort", &xs)?;
    // We can't propagate errors out of `sort_by`'s comparator, so do an
    // insertion sort with explicit `?`. Stable; O(n²) — fine for the
    // config-shaped lists this DSL handles. Switch to a merge sort if a
    // user ever sorts a really long list.
    let n = items.len();
    for i in 1..n {
        let mut j = i;
        while j > 0 {
            let less = call_closure(handle, &[items[j].clone(), items[j - 1].clone()], ctx)?;
            if is_truthy(&less) {
                items.swap(j, j - 1);
                j -= 1;
            } else {
                break;
            }
        }
    }
    Ok(Value::Array(items))
}

// ---------------------------------------------------------------------------
// Function combinators / call dispatch
// ---------------------------------------------------------------------------

fn funcall(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_at_least("funcall", 1, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let handle = coerce_to_fn("funcall", &f)?;
    let args = eval_args(&arg_exprs[1..], ctx)?;
    call_closure(handle, &args, ctx)
}

/// `(apply f a b ... rest-list)` — splice `rest-list` (the last arg)
/// onto the end of the preceding args. CL semantics.
fn apply(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_at_least("apply", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let handle = coerce_to_fn("apply", &f)?;
    let mut prefix = eval_args(&arg_exprs[1..arg_exprs.len() - 1], ctx)?;
    let tail = eval(&arg_exprs[arg_exprs.len() - 1], ctx)?;
    let mut tail_list = coerce_to_list("apply", &tail)?;
    prefix.append(&mut tail_list);
    call_closure(handle, &prefix, ctx)
}

fn identity(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("identity", 1, arg_exprs.len())?;
    eval(&arg_exprs[0], ctx)
}

/// `(constantly v)` returns a function-of-any-arity that always
/// produces `v`. Implemented by synthesizing a closure whose body is
/// the literal expression form of `v`.
fn constantly(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("constantly", 1, arg_exprs.len())?;
    let v = eval(&arg_exprs[0], ctx)?;
    let closure = Closure {
        params: Vec::new(),
        rest_param: Some("_".into()),
        body: vec![value_to_expr(&v)],
        captured_lexical: Vec::new(),
    };
    Ok(ctx.register_closure(closure))
}

/// `(complement f)` returns a function that calls `f` and inverts the
/// truthiness of its result. Synthesized as a closure whose body is
/// `(if (apply <f> args) false true)`.
fn complement(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("complement", 1, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let _ = coerce_to_fn("complement", &f)?; // validate
    let inner = value_to_expr(&f);
    let body = vec![Expr::List(vec![
        Expr::Symbol("if".into()),
        Expr::List(vec![
            Expr::Symbol("apply".into()),
            inner,
            Expr::Symbol("args".into()),
        ]),
        Expr::Bool(false),
        Expr::Bool(true),
    ])];
    let closure = Closure {
        params: Vec::new(),
        rest_param: Some("args".into()),
        body,
        captured_lexical: Vec::new(),
    };
    Ok(ctx.register_closure(closure))
}

// ---------------------------------------------------------------------------
// Set / dedup operations
//
// Equality here is structural Value equality (serde_json's PartialEq).
// CL's optional `:test` / `:key` keywords are not supported.
//
// Membership goes through `value_key::ValueSet` rather than a scan. The
// scan version was O(n^2) with a deep comparison at each step, and since
// each of these is a single dispatch the evaluator charged it one step:
// `remove-duplicates` over 120k elements took 16.6 seconds and reported
// four. The set makes the work linear, and `charge_scan` bills it so a
// step budget can see what remains.
// ---------------------------------------------------------------------------

/// Charge the evaluator for key-building work done inside one of these
/// calls.
///
/// Without this the cost is invisible: the whole pass is one dispatch, so
/// the step counter bills it once no matter how much data moved through.
/// Nodes are charged at a fraction of a step, since walking one node is
/// far cheaper than evaluating an expression, but the charge still grows
/// with the input so a budget bounds it.
fn charge_scan(ctx: &mut Context, nodes: u64) -> EvalResult<()> {
    /// Nodes walked per step charged.
    const NODES_PER_STEP: u64 = 8;
    ctx.fuel.charge_steps(nodes / NODES_PER_STEP)
}

fn remove_duplicates(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("remove-duplicates", 1, arg_exprs.len())?;
    let xs = eval(&arg_exprs[0], ctx)?;
    let items = coerce_to_list("remove-duplicates", &xs)?;
    let (out, nodes) = trongoscript_stdlib::value_key::dedup_values(&items);
    charge_scan(ctx, nodes)?;
    Ok(Value::Array(out))
}

fn tree_equal(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("tree-equal", 2, arg_exprs.len())?;
    let a = eval(&arg_exprs[0], ctx)?;
    let b = eval(&arg_exprs[1], ctx)?;
    Ok(Value::Bool(a == b))
}

enum SetOp {
    Union,
    Intersection,
    Difference,
    ExclusiveOr,
}

fn set_op(arg_exprs: &[Expr], ctx: &mut Context, fname: &str, op: SetOp) -> EvalResult<Value> {
    use trongoscript_stdlib::value_key::ValueSet;

    arity_exact(fname, 2, arg_exprs.len())?;
    let a = eval(&arg_exprs[0], ctx)?;
    let b = eval(&arg_exprs[1], ctx)?;
    let a_items = coerce_to_list(fname, &a)?;
    let b_items = coerce_to_list(fname, &b)?;

    // `emitted` replaces the "not already in `out`" scan; the membership
    // sets replace the scans over the opposite input. Output order is
    // unchanged from the scan version: first-seen order within A, then
    // within B.
    let mut emitted = ValueSet::with_capacity(a_items.len());
    let mut out: Vec<Value> = Vec::new();
    let mut nodes: u64 = 0;

    match op {
        // Union: every element of A, then elements of B not already in A.
        SetOp::Union => {
            for x in a_items.iter().chain(b_items.iter()) {
                if emitted.insert(x) {
                    out.push(x.clone());
                }
            }
        }
        // Intersection: elements of A also in B (dedup, order from A).
        SetOp::Intersection => {
            let mut in_b = ValueSet::from_values(b_items.iter());
            for x in &a_items {
                if in_b.contains(x) && emitted.insert(x) {
                    out.push(x.clone());
                }
            }
            nodes = nodes.saturating_add(in_b.nodes_walked());
        }
        // Difference: elements of A not in B.
        SetOp::Difference => {
            let mut in_b = ValueSet::from_values(b_items.iter());
            for x in &a_items {
                if !in_b.contains(x) && emitted.insert(x) {
                    out.push(x.clone());
                }
            }
            nodes = nodes.saturating_add(in_b.nodes_walked());
        }
        // Exclusive-or: (A ∪ B) − (A ∩ B).
        SetOp::ExclusiveOr => {
            let mut in_a = ValueSet::from_values(a_items.iter());
            let mut in_b = ValueSet::from_values(b_items.iter());
            for x in &a_items {
                if !in_b.contains(x) && emitted.insert(x) {
                    out.push(x.clone());
                }
            }
            for x in &b_items {
                if !in_a.contains(x) && emitted.insert(x) {
                    out.push(x.clone());
                }
            }
            nodes = nodes
                .saturating_add(in_a.nodes_walked())
                .saturating_add(in_b.nodes_walked());
        }
    }

    charge_scan(ctx, nodes.saturating_add(emitted.nodes_walked()))?;
    Ok(Value::Array(out))
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
