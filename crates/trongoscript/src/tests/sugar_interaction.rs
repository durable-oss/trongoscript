//! Where the four sugars meet each other, and where they meet the rest of
//! the language.
//!
//! Each feature is covered on its own in `conditionals`, `destructuring`,
//! and the parser's `interp_tests` / `infix` modules. What is left is the
//! seams: a pattern whose value comes from a `cond`, an interpolation whose
//! body destructures, a macro that expands to sugar. Those are where a
//! feature that works alone stops working, so they are pinned here rather
//! than left to be discovered in a config file.

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

fn s(v: &str) -> Value {
    Value::String(v.into())
}

// -- sugar composed with sugar ----------------------------------------------

/// An infix group inside an interpolation inside a destructured parameter:
/// all four features in one expression, which is what a real config line
/// looks like once they all exist.
#[test]
fn all_four_sugars_in_one_expression() {
    let src = r##"
        (defn describe [{:name n :ram r}]
          (cond
            ${r > 8}  "heap: ${r * 3 / 4}G on #{n}"
            :else     "heap: default on #{n}"))
        [(describe {:name "web" :ram 16}) (describe {:name "tiny" :ram 2})]
    "##;
    assert_eq!(
        eval_str(src),
        Value::Array(vec![s("heap: 12G on web"), s("heap: default on tiny")])
    );
}

#[test]
fn a_pattern_binds_the_result_of_a_cond() {
    let src = r#"
        (let [[a b] (cond false [1 2] :else [3 4])]
          (+ a b))
    "#;
    assert_eq!(eval_str(src), num(7));
}

#[test]
fn an_infix_group_is_a_cond_test() {
    let src = r#"
        (let [n 5]
          (cond ${n > 10} "big" ${n > 3} "medium" :else "small"))
    "#;
    assert_eq!(eval_str(src), s("medium"));
}

#[test]
fn an_infix_group_is_an_if_marker_test() {
    let src = r#"
        (let [n 5]
          (if ${n > 10} "big" :elsif ${n > 3} "medium" :else "small"))
    "#;
    assert_eq!(eval_str(src), s("medium"));
}

/// Destructured names are ordinary bindings, so an interpolation and a group
/// both see them.
#[test]
fn destructured_names_are_visible_to_interpolation_and_groups() {
    assert_eq!(
        eval_str(r##"(let [{:a x} {:a 3}] "x=#{x} double=${x * 2}")"##),
        s("x=3 double=6")
    );
    assert_eq!(eval_str(r##"(let [[a b] [1 2]] "${a + b}")"##), s("3"));
}

/// An interpolation body is a full form, so it may itself be a `cond` or a
/// destructuring `let`.
#[test]
fn an_interpolation_body_may_be_any_form() {
    assert_eq!(
        eval_str(r##"(let [n 1] "level=#{(cond (= n 1) "one" :else "many")}")"##),
        s("level=one")
    );
    assert_eq!(
        eval_str(r##""name=#{(let [{:n v} {:n "x"}] v)}""##),
        s("name=x")
    );
}

/// A pattern's value may come from a group, and the mismatch it produces is
/// the ordinary one.
#[test]
fn a_group_feeding_a_pattern_still_reports_mismatch() {
    assert!(matches!(
        eval_err("(let [[a b] ${1 + 1}] a)"),
        EvalError::PatternMismatch { .. }
    ));
}

// -- sugar meets macros -----------------------------------------------------

/// The desugaring discipline's whole point: a macro can expand to sugar,
/// because the sugar is not a distinct node. `${1 + 2}` inside a
/// quasiquote is already `(+ 1 2)` by the time the macro body runs.
#[test]
fn a_macro_may_expand_to_sugar() {
    assert_eq!(eval_str("(defmacro m [] `${1 + 2}) (m)"), num(3));
    assert_eq!(
        eval_str("(defmacro m [] `(cond false 1 :else 9)) (m)"),
        num(9)
    );
    assert_eq!(
        eval_str("(defmacro m [x] `(if ~x 1 :else 2)) (m false)"),
        num(2)
    );
    assert_eq!(
        eval_str(r##"(defmacro m [x] `(let [{:a y} ~x] y)) (m {:a 7})"##),
        num(7)
    );
}

/// `macroexpand` prints the desugaring, since that is what the reader built.
/// A user who cannot recover a sugar's meaning by expanding it has a syntax
/// they cannot reason about.
#[test]
fn macroexpand_shows_the_desugaring() {
    // Interpolation is a `(str ...)` call.
    assert_eq!(
        eval_str(r##"(macroexpand '"a#{x}b")"##),
        Value::Array(vec![s("str"), s("a"), s("x"), s("b")])
    );
    // A group is the operator call it desugars to.
    assert_eq!(
        eval_str("(macroexpand '${1 + 2})"),
        Value::Array(vec![s("+"), num(1), num(2)])
    );
}

/// Quoting sugar yields the desugared form, not a sugar-preserving node.
/// This is the reader-level statement of the same discipline.
#[test]
fn quoting_sugar_yields_the_desugaring() {
    assert_eq!(
        eval_str("'${1 + 2}"),
        Value::Array(vec![s("+"), num(1), num(2)])
    );
    assert_eq!(
        eval_str(r##"'"a#{x}b""##),
        Value::Array(vec![s("str"), s("a"), s("x"), s("b")])
    );
}

/// `defmacro` parameters do not destructure, unlike `fn` and `defn`. Macro
/// parameters bind unevaluated `Expr` forms rather than values, so a pattern
/// there would be matching syntax, not data — a different feature wearing
/// the same spelling. Pinned so the asymmetry is deliberate and visible
/// rather than an oversight someone "fixes" by accident.
#[test]
fn defmacro_parameters_do_not_destructure() {
    let e = eval_err("(defmacro m [[a b]] `(+ ~a ~b)) (m [1 2])");
    assert!(matches!(e, EvalError::InvalidForm(_)), "got {:?}", e);
    assert!(
        e.to_string().contains("macro parameter"),
        "message should say it is about macro parameters, got: {}",
        e
    );
}

// -- sugar meets the surrounding language -----------------------------------

/// Interpolation desugars to a call to `str`, which resolves the same way
/// any builtin does. A local binding named `str` shadows the *symbol*, but
/// the desugared call still finds the builtin, so interpolation does not
/// break in a scope that happens to use the name.
#[test]
fn a_local_named_str_does_not_break_interpolation() {
    assert_eq!(eval_str(r##"(let [str 1] "a#{str}b")"##), s("a1b"));
}

/// Sugar is legal anywhere a form is, including in map keys and vectors —
/// which is most of what a config document is made of.
#[test]
fn sugar_works_in_data_positions() {
    let v = eval_str(r##"(let [k "a" n 2] {"#{k}-key" ${n * 3}})"##);
    assert_eq!(v.as_object().unwrap().get("a-key"), Some(&num(6)));

    assert_eq!(
        eval_str(r##"(let [n 1] [${n + 1} "n=#{n}"])"##),
        Value::Array(vec![num(2), s("n=1")])
    );
}

/// Comments and `#_` discards are consumed by the ordinary whitespace
/// reader, which runs inside groups and interpolation bodies too.
#[test]
fn comments_and_discards_work_inside_sugar() {
    assert_eq!(eval_str("${1 + ; a comment\n 2}"), num(3));
    assert_eq!(eval_str("${1 + #_ 99 2}"), num(3));
    assert_eq!(eval_str("\"a#{ ; comment\n 1 }b\""), s("a1b"));
}

/// `str` accepts scalars only, so interpolating a collection is a type
/// error rather than a silently stringified structure. Pinned because the
/// alternative — some ad-hoc rendering of a map into a config string — is
/// exactly the kind of thing that would be quietly wrong.
#[test]
fn interpolating_a_collection_is_a_type_error() {
    assert!(eval_source(
        r##"(let [v [1 2]] "v=#{v}")"##,
        &mut Context::new(EvalConfig::default())
    )
    .is_err());
    assert!(eval_source(
        r##"(let [m {:a 1}] "m=#{m}")"##,
        &mut Context::new(EvalConfig::default())
    )
    .is_err());
}

/// Scalars interpolate as their source spelling, which is what a config
/// string wants. `nil` contributes nothing rather than the text "null".
#[test]
fn scalars_interpolate_as_their_source_spelling() {
    assert_eq!(eval_str(r##""b=#{true}""##), s("b=true"));
    assert_eq!(eval_str(r##"(let [f 1.5] "f=#{f}")"##), s("f=1.5"));
    assert_eq!(eval_str(r##"(let [k :a] "k=#{k}")"##), s("k=a"));
    assert_eq!(eval_str(r##""nil=#{nil}""##), s("nil="));
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
