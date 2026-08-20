//! Destructuring patterns in `let` bindings and `fn` parameter lists.

use super::super::*;
use pretty_assertions::assert_eq;

fn eval_str(src: &str) -> Value {
    let mut ctx = Context::new(EvalConfig::default());
    eval_source(src, &mut ctx).expect("eval failed")
}

fn eval_err(src: &str) -> EvalError {
    let mut ctx = Context::new(EvalConfig::default());
    eval_source(src, &mut ctx).expect_err("expected an error")
}

fn num(n: i64) -> Value {
    Value::Number(n.into())
}

// -- the today-form still works ---------------------------------------------

/// Purely additive: every existing `let` and `fn` keeps parsing and keeps
/// meaning what it meant.
#[test]
fn plain_symbol_bindings_are_unchanged() {
    assert_eq!(eval_str("(let [x 1] x)"), num(1));
    assert_eq!(eval_str("(let [x 1 y 2] (+ x y))"), num(3));
    // `let` is sequential: later bindings see earlier ones.
    assert_eq!(eval_str("(let [x 1 y (+ x 1)] y)"), num(2));
    assert_eq!(eval_str("((fn [a b] (+ a b)) 1 2)"), num(3));
    assert_eq!(
        eval_str("((fn [a & rest] rest) 1 2 3)"),
        Value::Array(vec![num(2), num(3)])
    );
}

// -- vector patterns --------------------------------------------------------

#[test]
fn vector_pattern_binds_by_position() {
    assert_eq!(eval_str("(let [[a b] [1 2]] (+ a b))"), num(3));
    assert_eq!(eval_str("(let [[a] [1]] a)"), num(1));
    assert_eq!(eval_str("(let [[a b c] [1 2 3]] c)"), num(3));
}

/// Without a `& rest` tail the match is exact-length, so a wrong-sized
/// vector is an error rather than a silent nil.
#[test]
fn vector_pattern_without_rest_requires_an_exact_length() {
    assert!(matches!(
        eval_err("(let [[a b] [1]] a)"),
        EvalError::PatternMismatch { .. }
    ));
    assert!(matches!(
        eval_err("(let [[a b] [1 2 3]] a)"),
        EvalError::PatternMismatch { .. }
    ));
}

#[test]
fn vector_pattern_with_rest_is_minimum_length() {
    assert_eq!(eval_str("(let [[a & more] [1 2 3]] a)"), num(1));
    assert_eq!(eval_str("(let [[a & more] [1 2 3]] (count more))"), num(2));
    // The remainder may be empty.
    assert_eq!(eval_str("(let [[a & more] [1]] (count more))"), num(0));
    // But the fixed part still has to be there.
    assert!(matches!(
        eval_err("(let [[a b & more] [1]] a)"),
        EvalError::PatternMismatch { .. }
    ));
}

#[test]
fn vector_pattern_rejects_a_non_vector() {
    assert!(matches!(
        eval_err("(let [[a b] 5] a)"),
        EvalError::PatternMismatch { .. }
    ));
    assert!(matches!(
        eval_err(r#"(let [[a b] {:x 1}] a)"#),
        EvalError::PatternMismatch { .. }
    ));
}

// -- map patterns -----------------------------------------------------------

#[test]
fn map_pattern_binds_by_key() {
    assert_eq!(eval_str(r#"(let [{:a x} {:a 1}] x)"#), num(1));
    assert_eq!(
        eval_str(r#"(let [{:a x :b y} {:a 1 :b 2}] (+ x y))"#),
        num(3)
    );
}

/// A map pattern matches a map containing *at least* its named keys. Extra
/// keys are ignored, which is the right default for configuration: a
/// document gains fields over time and a pattern should not break when it
/// does.
#[test]
fn map_pattern_ignores_extra_keys() {
    assert_eq!(eval_str(r#"(let [{:a x} {:a 1 :b 2 :c 3}] x)"#), num(1));
}

/// A missing key raises with the key name in the message. Silent
/// nil-binding is how a typo'd key becomes a `null` in the emitted YAML and
/// an incident hours later.
#[test]
fn map_pattern_raises_on_a_missing_key() {
    let e = eval_err(r#"(let [{:a x :b y} {:a 1}] x)"#);
    assert!(matches!(e, EvalError::PatternMismatch { .. }));
    assert!(
        e.to_string().contains(":b"),
        "message should name the missing key, got: {}",
        e
    );
}

#[test]
fn map_pattern_rejects_a_non_map() {
    assert!(matches!(
        eval_err(r#"(let [{:a x} [1 2]] x)"#),
        EvalError::PatternMismatch { .. }
    ));
    assert!(matches!(
        eval_err(r#"(let [{:a x} 5] x)"#),
        EvalError::PatternMismatch { .. }
    ));
}

// -- nesting ----------------------------------------------------------------

#[test]
fn patterns_nest() {
    assert_eq!(
        eval_str(r#"(let [[{:a x} {:a y}] [{:a 1} {:a 2}]] (+ x y))"#),
        num(3)
    );
    assert_eq!(eval_str(r#"(let [{:a [x y]} {:a [1 2]}] (+ x y))"#), num(3));
    assert_eq!(eval_str(r#"(let [{:a {:b x}} {:a {:b 42}}] x)"#), num(42));
}

#[test]
fn nested_mismatch_still_raises() {
    assert!(matches!(
        eval_err(r#"(let [{:a [x y]} {:a [1]}] x)"#),
        EvalError::PatternMismatch { .. }
    ));
}

// -- fn parameters ----------------------------------------------------------

/// The same matcher runs on parameter lists, since a parameter list is a
/// vector of binding forms. Splitting these would leave destructuring in one
/// binding position and not the other.
#[test]
fn fn_parameters_destructure() {
    assert_eq!(eval_str(r#"((fn [{:a x}] x) {:a 1})"#), num(1));
    assert_eq!(eval_str(r#"((fn [[a b]] (+ a b)) [1 2])"#), num(3));
    assert_eq!(
        eval_str(r#"((fn [{:a x} {:b y}] (+ x y)) {:a 1} {:b 2})"#),
        num(3)
    );
}

#[test]
fn defn_parameters_destructure() {
    let src = r#"
        (defn describe [{:name n :port p}]
          (str n " on " p))
        (describe {:name "web" :port 443})
    "#;
    assert_eq!(eval_str(src), Value::String("web on 443".into()));
}

#[test]
fn destructuring_params_mix_with_a_rest_param() {
    assert_eq!(
        eval_str(r#"((fn [{:a x} & more] (+ x (count more))) {:a 1} 2 3)"#),
        num(3)
    );
}

#[test]
fn fn_parameter_mismatch_raises() {
    assert!(matches!(
        eval_err(r#"((fn [{:a x}] x) {:b 1})"#),
        EvalError::PatternMismatch { .. }
    ));
    assert!(matches!(
        eval_err(r#"((fn [[a b]] a) [1])"#),
        EvalError::PatternMismatch { .. }
    ));
}

/// A pattern that fails to match must not leave the evaluator's scope stack
/// swapped out — the caller's bindings have to survive the error.
#[test]
fn a_failed_parameter_match_does_not_corrupt_scope() {
    let src = r#"
        (let [outer 7]
          (def f (fn [{:a x}] x))
          outer)
    "#;
    assert_eq!(eval_str(src), num(7));
}

// -- the shape this exists for ----------------------------------------------

/// `map` over a vector of maps is the most common higher-order shape in a
/// config document, and today every such lambda opens with `get` calls.
#[test]
fn destructuring_in_map_over_a_vector_of_maps() {
    let src = r#"
        (let [servers [{:name "a" :port 1} {:name "b" :port 2}]]
          (map (fn [{:port p}] p) servers))
    "#;
    assert_eq!(eval_str(src), Value::Array(vec![num(1), num(2)]));
}

#[test]
fn pulling_several_fields_out_of_one_map() {
    let src = r#"
        (let [{:host h :port p :name n} {:host "db" :port 5432 :name "main" :extra true}]
          {:host h :port p :name n})
    "#;
    let v = eval_str(src);
    let obj = v.as_object().unwrap();
    assert_eq!(obj.get("host"), Some(&Value::String("db".into())));
    assert_eq!(obj.get("port"), Some(&num(5432)));
    assert_eq!(obj.get("name"), Some(&Value::String("main".into())));
}

// -- degenerate and edge-shaped patterns ------------------------------------

/// An empty vector pattern is exact-length like any other, so it matches
/// only the empty vector. It is not a wildcard.
#[test]
fn an_empty_vector_pattern_matches_only_the_empty_vector() {
    assert_eq!(eval_str("(let [[] []] 1)"), num(1));
    assert!(matches!(
        eval_err("(let [[] [1]] 1)"),
        EvalError::PatternMismatch { .. }
    ));
}

/// An empty map pattern names no keys, and a map pattern requires only the
/// keys it names — so it matches any map. It still rejects a non-map, which
/// is the one thing it does assert.
#[test]
fn an_empty_map_pattern_matches_any_map_but_not_a_non_map() {
    assert_eq!(eval_str("(let [{} {:a 1}] 2)"), num(2));
    assert_eq!(eval_str("(let [{} {}] 2)"), num(2));
    assert!(matches!(
        eval_err("(let [{} 5] 2)"),
        EvalError::PatternMismatch { .. }
    ));
}

/// A vector pattern of only `& rest` binds the whole vector, including an
/// empty one.
#[test]
fn a_rest_only_pattern_binds_the_whole_vector() {
    assert_eq!(
        eval_str("(let [[& all] [1 2 3]] all)"),
        Value::Array(vec![num(1), num(2), num(3)])
    );
    assert_eq!(eval_str("(let [[& all] []] (count all))"), num(0));
}

/// nil is neither a vector nor a map, so a collection pattern raises rather
/// than binding its names to nil. This is the case the raise-don't-bind-nil
/// rule exists for: a missing config section should stop the build, not
/// propagate nulls into the output.
#[test]
fn a_pattern_against_nil_raises() {
    assert!(matches!(
        eval_err("(let [[a b] nil] a)"),
        EvalError::PatternMismatch { .. }
    ));
    assert!(matches!(
        eval_err(r#"(let [{:a x} nil] x)"#),
        EvalError::PatternMismatch { .. }
    ));
    // Including via a key that is present but nil-valued.
    assert!(matches!(
        eval_err(r#"(let [{:a [x]} {:a nil}] x)"#),
        EvalError::PatternMismatch { .. }
    ));
}

/// A key that is present but nil-valued still matches a plain name binding —
/// the pattern asserts presence, not non-nil-ness.
#[test]
fn a_present_but_nil_key_binds_nil() {
    assert_eq!(eval_str(r#"(let [{:a x} {:a nil}] x)"#), Value::Null);
}

/// Names bind left to right, so a repeated name in one pattern ends up with
/// the rightmost match. Not a useful thing to write, but it has to do
/// *something* predictable rather than depend on iteration order.
#[test]
fn a_repeated_name_in_one_pattern_takes_the_last_match() {
    assert_eq!(eval_str("(let [[a a] [1 2]] a)"), num(2));
    assert_eq!(eval_str(r#"(let [{:x a :y a} {:x 1 :y 2}] a)"#), num(2));
    // The rest binding is pushed last, so it wins over a fixed slot.
    assert_eq!(
        eval_str("(let [[a & a] [1 2 3]] a)"),
        Value::Array(vec![num(2), num(3)])
    );
}

/// A destructuring binding shadows an outer name for the rest of the `let`,
/// the same way a plain binding does.
#[test]
fn destructuring_shadows_like_a_plain_binding() {
    assert_eq!(eval_str("(let [x 1] (let [{:x x} {:x 2}] x))"), num(2));
    // And the outer binding is intact once the inner scope ends.
    assert_eq!(eval_str("(let [x 1] (let [{:x x} {:x 2}] x) x)"), num(1));
}

/// Patterns nest to arbitrary depth, and a failure deep inside still names
/// the sub-pattern that failed rather than the outermost one.
#[test]
fn a_deep_mismatch_names_the_inner_pattern() {
    let e = eval_err(r#"(let [{:a {:b [x y]}} {:a {:b [1]}}] x)"#);
    let msg = e.to_string();
    // The reported pattern is the inner `[x y]`, not the whole thing.
    assert!(
        msg.contains("[x y]"),
        "should name the inner pattern that failed, got: {}",
        msg
    );
}

/// A `let` is sequential, so a pattern may destructure a value built from
/// names bound earlier in the same binding vector.
#[test]
fn a_pattern_may_use_names_bound_earlier_in_the_same_let() {
    assert_eq!(
        eval_str(r#"(let [m {:a 1 :b 2} {:a x :b y} m] (+ x y))"#),
        num(3)
    );
}

// -- malformed patterns -----------------------------------------------------

#[test]
fn malformed_patterns_error() {
    // `&` with no name after it.
    assert!(eval_source(
        "(let [[a &] [1]] a)",
        &mut Context::new(EvalConfig::default())
    )
    .is_err());
    // A pattern after `& rest`.
    assert!(eval_source(
        "(let [[a & r b] [1 2]] a)",
        &mut Context::new(EvalConfig::default())
    )
    .is_err());
    // A non-keyword map-pattern key.
    assert!(eval_source(
        "(let [{1 x} {:a 1}] x)",
        &mut Context::new(EvalConfig::default())
    )
    .is_err());
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
