//! `cond`, and the `:elsif` / `:else` markers in `if`.

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

// -- cond -------------------------------------------------------------------

#[test]
fn cond_takes_the_first_truthy_branch() {
    assert_eq!(eval_str("(cond true 1 true 2)"), num(1));
    assert_eq!(eval_str("(cond false 1 true 2)"), num(2));
    assert_eq!(eval_str("(cond false 1 false 2 true 3)"), num(3));
}

/// `:else` needs no special casing — a keyword is a literal and always
/// truthy, so it simply works as the last test.
#[test]
fn else_is_an_ordinary_always_truthy_keyword() {
    assert_eq!(eval_str("(cond false 1 :else 2)"), num(2));
    assert_eq!(eval_str("(cond true 1 :else 2)"), num(1));
    // Which means it is not magic: any truthy literal reads the same way.
    assert_eq!(eval_str("(cond false 1 :anything 2)"), num(2));
}

/// A `cond` with no matching clause and no `:else` is nil, like a one-armed
/// `if`.
#[test]
fn cond_with_no_match_is_nil() {
    assert_eq!(eval_str("(cond false 1 false 2)"), Value::Null);
    assert_eq!(eval_str("(cond)"), Value::Null);
}

#[test]
fn cond_only_evaluates_the_branch_it_takes() {
    // A later branch that would error is never reached.
    assert_eq!(eval_str("(cond true 1 :else (/ 1 0))"), num(1));
    // Nor is a later *test* evaluated once one has matched.
    assert_eq!(eval_str("(cond true 1 (/ 1 0) 2)"), num(1));
}

#[test]
fn cond_rejects_an_odd_number_of_forms() {
    assert!(matches!(
        eval_err("(cond true 1 :else)"),
        EvalError::InvalidForm(_)
    ));
}

/// The shape this exists for: a multi-branch decision that would otherwise
/// be an `if` staircase.
#[test]
fn cond_replaces_the_nested_if_staircase() {
    let src = r#"
        (let [env "staging"]
          (cond
            (= env "production") "warn"
            (= env "staging")    "info"
            (= env "dev")        "debug"
            :else                "trace"))
    "#;
    assert_eq!(eval_str(src), Value::String("info".into()));
}

// -- if markers -------------------------------------------------------------

#[test]
fn positional_if_is_unchanged() {
    assert_eq!(eval_str("(if true 1)"), num(1));
    assert_eq!(eval_str("(if false 1)"), Value::Null);
    assert_eq!(eval_str("(if true 1 2)"), num(1));
    assert_eq!(eval_str("(if false 1 2)"), num(2));
}

#[test]
fn else_marker_says_the_positional_form_out_loud() {
    assert_eq!(eval_str("(if true 1 :else 2)"), num(1));
    assert_eq!(eval_str("(if false 1 :else 2)"), num(2));
}

#[test]
fn elsif_marker_chains() {
    assert_eq!(eval_str("(if true 1 :elsif true 2)"), num(1));
    assert_eq!(eval_str("(if false 1 :elsif true 2)"), num(2));
    assert_eq!(eval_str("(if false 1 :elsif false 2)"), Value::Null);
    assert_eq!(eval_str("(if false 1 :elsif true 2 :else 3)"), num(2));
    assert_eq!(eval_str("(if false 1 :elsif false 2 :else 3)"), num(3));
    assert_eq!(
        eval_str("(if false 1 :elsif false 2 :elsif true 3 :else 4)"),
        num(3)
    );
}

#[test]
fn markers_only_evaluate_the_live_branch() {
    assert_eq!(eval_str("(if true 1 :elsif (/ 1 0) 2)"), num(1));
    assert_eq!(eval_str("(if false (/ 1 0) :else 2)"), num(2));
}

#[test]
fn malformed_marker_usage_errors() {
    // `:else` with nothing after it.
    assert!(matches!(
        eval_err("(if false 1 :else)"),
        EvalError::InvalidForm(_)
    ));
    // `:else` is not the last clause.
    assert!(matches!(
        eval_err("(if false 1 :else 2 :elsif true 3)"),
        EvalError::InvalidForm(_)
    ));
    // `:elsif` with a test but no body.
    assert!(matches!(
        eval_err("(if false 1 :elsif true)"),
        EvalError::InvalidForm(_)
    ));
    // Something other than a marker where one is expected.
    assert!(matches!(
        eval_err("(if false 1 :elsif true 2 3)"),
        EvalError::InvalidForm(_)
    ));
}

/// A marker only counts in marker position — index 2, after a complete
/// test/body pair. In test position `:else` is just an always-truthy
/// keyword, which is what makes it work as `cond`'s catch-all and what
/// `cond`'s expansion to nested `if`s relies on.
#[test]
fn a_keyword_in_test_position_is_a_value_not_a_marker() {
    assert_eq!(eval_str("(if :else 1 2)"), num(1));
    assert_eq!(eval_str("(if :elsif 1 2)"), num(1));
    // Which is why this shape — the one `cond` expands into — evaluates
    // rather than erroring.
    assert_eq!(eval_str("(if false 1 (if :else 2 nil))"), num(2));
}

/// The markers are keywords rather than bare symbols precisely so they
/// cannot collide with a user's bindings. Trongoscript resolves symbols at
/// eval time and could not catch the collision otherwise.
#[test]
fn a_binding_named_else_does_not_collide() {
    assert_eq!(eval_str(r#"(let [else 99] (if false 1 :else 2))"#), num(2));
    assert_eq!(
        eval_str(r#"(let [else 99] (cond false 1 :else 2))"#),
        num(2)
    );
    // And the *symbol* `else` is still an ordinary name.
    assert_eq!(eval_str(r#"(let [else 99] else)"#), num(99));
}

// -- both, evaluated in a document ------------------------------------------

#[test]
fn conditionals_work_inside_a_config_document() {
    let src = r#"
        (def env "production")
        {:log-level (cond
                      (= env "production") "warn"
                      (= env "staging")    "info"
                      :else                "debug")
         :port      (if (= env "production") 443
                      :elsif (= env "staging") 8443
                      :else 8080)}
    "#;
    let v = eval_str(src);
    let obj = v.as_object().unwrap();
    assert_eq!(obj.get("log-level"), Some(&Value::String("warn".into())));
    assert_eq!(obj.get("port"), Some(&num(443)));
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
