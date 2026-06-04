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
    Some(r)
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
fn pred_holds(
    handle: usize,
    value: &Value,
    ctx: &mut Context,
    invert: bool,
) -> EvalResult<bool> {
    let r = call_closure(handle, &[value.clone()], ctx)?;
    Ok(if invert { !is_truthy(&r) } else { is_truthy(&r) })
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
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        out.push(call_closure(handle, &[it], ctx)?);
    }
    Ok(Value::Array(out))
}

/// Call `f` on each element for side-effect; return the original list.
fn mapc(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("mapc", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("mapc", &f)?;
    let items = coerce_to_list("mapc", &xs)?;
    for it in &items {
        call_closure(handle, &[it.clone()], ctx)?;
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
    let mut out = Vec::with_capacity(items.len());
    for i in 0..items.len() {
        let suffix = Value::Array(items[i..].to_vec());
        out.push(call_closure(handle, &[suffix], ctx)?);
    }
    Ok(Value::Array(out))
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
    let mut out: Vec<Value> = Vec::new();
    for it in items {
        let part = call_closure(handle, &[it], ctx)?;
        let mut piece = coerce_to_list("mapcan", &part)?;
        out.append(&mut piece);
    }
    Ok(Value::Array(out))
}

/// `mapcon`: like `maplist` but concatenate. Each call must return a list.
fn mapcon(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("mapcon", 2, arg_exprs.len())?;
    let f = eval(&arg_exprs[0], ctx)?;
    let xs = eval(&arg_exprs[1], ctx)?;
    let handle = coerce_to_fn("mapcon", &f)?;
    let items = coerce_to_list("mapcon", &xs)?;
    let mut out: Vec<Value> = Vec::new();
    for i in 0..items.len() {
        let suffix = Value::Array(items[i..].to_vec());
        let part = call_closure(handle, &[suffix], ctx)?;
        let mut piece = coerce_to_list("mapcon", &part)?;
        out.append(&mut piece);
    }
    Ok(Value::Array(out))
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
    let fname = if invert { "position-if-not" } else { "position-if" };
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
    let fname = if invert { "substitute-if-not" } else { "substitute-if" };
    arity_exact(fname, 3, arg_exprs.len())?;
    let new_v = eval(&arg_exprs[0], ctx)?;
    let pred = eval(&arg_exprs[1], ctx)?;
    let xs = eval(&arg_exprs[2], ctx)?;
    let handle = coerce_to_fn(fname, &pred)?;
    let items = coerce_to_list(fname, &xs)?;
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        if pred_holds(handle, &it, ctx, invert)? {
            out.push(new_v.clone());
        } else {
            out.push(it);
        }
    }
    Ok(Value::Array(out))
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
// ---------------------------------------------------------------------------

fn remove_duplicates(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    arity_exact("remove-duplicates", 1, arg_exprs.len())?;
    let xs = eval(&arg_exprs[0], ctx)?;
    let items = coerce_to_list("remove-duplicates", &xs)?;
    let mut out: Vec<Value> = Vec::new();
    for it in items {
        if !out.iter().any(|prev| prev == &it) {
            out.push(it);
        }
    }
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

fn set_op(
    arg_exprs: &[Expr],
    ctx: &mut Context,
    fname: &str,
    op: SetOp,
) -> EvalResult<Value> {
    arity_exact(fname, 2, arg_exprs.len())?;
    let a = eval(&arg_exprs[0], ctx)?;
    let b = eval(&arg_exprs[1], ctx)?;
    let a_items = coerce_to_list(fname, &a)?;
    let b_items = coerce_to_list(fname, &b)?;
    let contains = |hay: &[Value], needle: &Value| hay.iter().any(|x| x == needle);
    let out: Vec<Value> = match op {
        // Union: every element of A, then elements of B not already in A.
        SetOp::Union => {
            let mut out: Vec<Value> = Vec::new();
            for x in &a_items {
                if !contains(&out, x) {
                    out.push(x.clone());
                }
            }
            for x in &b_items {
                if !contains(&out, x) {
                    out.push(x.clone());
                }
            }
            out
        }
        // Intersection: elements of A also in B (dedup, order from A).
        SetOp::Intersection => {
            let mut out: Vec<Value> = Vec::new();
            for x in &a_items {
                if contains(&b_items, x) && !contains(&out, x) {
                    out.push(x.clone());
                }
            }
            out
        }
        // Difference: elements of A not in B.
        SetOp::Difference => {
            let mut out: Vec<Value> = Vec::new();
            for x in &a_items {
                if !contains(&b_items, x) && !contains(&out, x) {
                    out.push(x.clone());
                }
            }
            out
        }
        // Exclusive-or: (A ∪ B) − (A ∩ B).
        SetOp::ExclusiveOr => {
            let mut out: Vec<Value> = Vec::new();
            for x in &a_items {
                if !contains(&b_items, x) && !contains(&out, x) {
                    out.push(x.clone());
                }
            }
            for x in &b_items {
                if !contains(&a_items, x) && !contains(&out, x) {
                    out.push(x.clone());
                }
            }
            out
        }
    };
    Ok(Value::Array(out))
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.