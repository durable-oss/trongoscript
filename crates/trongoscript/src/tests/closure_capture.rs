//! Tests for how closures capture and reinstall lexical scope.
//!
//! Captured frames are shared rather than copied, so the cost of calling a
//! closure no longer grows with the size of what it can see. These pin down
//! that the sharing happens and that it did not change what a closure sees.

use std::sync::Arc;

use crate::error::EvalError;
use crate::eval::{eval_source, Closure, Context, EvalConfig};
use trongoscript_stdlib::Value;

fn ctx() -> Context {
    Context::new(EvalConfig::default())
}

#[test]
fn closures_made_in_one_let_share_its_frame() {
    // Made in the body, not as `let` bindings: binding a closure into the
    // frame would add a name to it, and so copy it (see the next test).
    let mut ctx = ctx();
    eval_source(
        "(let [rows [{:id 1} {:id 2}]]
           [(fn [] rows) (fn [] (count rows))])",
        &mut ctx,
    )
    .unwrap();
    let f = &ctx.fns[0].captured_lexical;
    let g = &ctx.fns[1].captured_lexical;
    assert!(
        Arc::ptr_eq(&f[0], &g[0]),
        "the second closure got its own copy of the `let` frame"
    );
}

#[test]
fn a_closure_does_not_see_let_bindings_made_after_it() {
    // The frame is shared at capture time, so binding `b` into it must copy
    // it first rather than writing into the closure's view.
    let mut ctx = ctx();
    let err = eval_source("(let [a 1 f (fn [] b) b 2] (f))", &mut ctx).unwrap_err();
    assert!(
        matches!(err, EvalError::UndefinedSymbol(ref s) if s == "b"),
        "got {err:?}"
    );
}

#[test]
fn a_closure_sees_the_bindings_it_captured() {
    let mut ctx = ctx();
    let v = eval_source(
        "(let [rows [1 2 3] n 10]
           (map (fn [x] (+ x n (count rows))) rows))",
        &mut ctx,
    )
    .unwrap();
    assert_eq!(v, serde_json::json!([14, 15, 16]));
}

#[test]
fn a_closure_still_sees_globals_defined_after_it() {
    let mut ctx = ctx();
    let v = eval_source("(let [f (fn [] later)] (def later 7) (f))", &mut ctx).unwrap();
    assert_eq!(v, Value::from(7));
}

#[test]
fn a_persisted_closure_round_trips_through_serde() {
    // `captured_lexical` is shared in memory but must serialize as plain
    // maps, the same as before it was shared.
    let mut ctx = ctx();
    eval_source("(def add-n (let [n 5] (fn [x] (+ x n))))", &mut ctx).unwrap();
    let json = serde_json::to_value(&*ctx.fns[0]).unwrap();
    assert_eq!(json["captured_lexical"], serde_json::json!([{ "n": 5 }]));

    let closure: Closure = serde_json::from_value(json).unwrap();
    let mut restored = Context::from_persisted(
        ctx.globals().clone(),
        vec![closure],
        Default::default(),
        0,
        EvalConfig::default(),
    );
    let v = eval_source("(add-n 1)", &mut restored).unwrap();
    assert_eq!(v, Value::from(6));
}
