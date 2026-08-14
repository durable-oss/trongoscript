use std::collections::HashSet;

use super::super::*;
use pretty_assertions::assert_eq;

fn opaque(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn pe(src: &str, ctx: &mut Context, ops: &HashSet<String>) -> Partial {
    let expr = trongoscript_parser::parse_expr(src).unwrap();
    partial_eval(&expr, ctx, ops).unwrap()
}

fn pe_ctx() -> Context {
    Context::new(EvalConfig::default())
}

fn done_num(p: Partial) -> f64 {
    match p {
        Partial::Done(Value::Number(n)) => n.as_f64().unwrap(),
        other => panic!(
            "expected Done(Number), got {:?}",
            matches!(other, Partial::Done(_))
        ),
    }
}

fn done_val(p: Partial) -> Value {
    match p {
        Partial::Done(v) => v,
        Partial::Suspended(_) => panic!("expected Done, got Suspended"),
    }
}

// ---------------------------------------------------------------------------
// Atoms
// ---------------------------------------------------------------------------

#[test]
fn nil_atom() {
    let mut ctx = pe_ctx();
    assert_eq!(done_val(pe("nil", &mut ctx, &opaque(&[]))), Value::Null);
}

#[test]
fn bool_true() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe("true", &mut ctx, &opaque(&[]))),
        Value::Bool(true)
    );
}

#[test]
fn bool_false() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe("false", &mut ctx, &opaque(&[]))),
        Value::Bool(false)
    );
}

#[test]
fn number() {
    let mut ctx = pe_ctx();
    assert_eq!(done_num(pe("42", &mut ctx, &opaque(&[]))), 42.0);
}

#[test]
fn string() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe(r#""hello""#, &mut ctx, &opaque(&[]))),
        Value::String("hello".into())
    );
}

#[test]
fn keyword() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe(":foo", &mut ctx, &opaque(&[]))),
        Value::String("foo".into())
    );
}

// ---------------------------------------------------------------------------
// Symbol resolution
// ---------------------------------------------------------------------------

#[test]
fn defined_symbol_resolves() {
    let mut ctx = pe_ctx();
    ctx.globals_mut()
        .insert("x".into(), Value::Number(99.into()));
    assert_eq!(done_num(pe("x", &mut ctx, &opaque(&[]))), 99.0);
}

#[test]
fn undefined_symbol_errors() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("undefined-sym").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&[]));
    assert!(matches!(r, Err(EvalError::UndefinedSymbol(_))));
}

// ---------------------------------------------------------------------------
// Pure list forms — all reduce to Done
// ---------------------------------------------------------------------------

#[test]
fn pure_expr_is_done() {
    let mut ctx = pe_ctx();
    assert_eq!(done_num(pe("(+ 1 2)", &mut ctx, &opaque(&[]))), 3.0);
}

#[test]
fn empty_list_is_done() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe("()", &mut ctx, &opaque(&[]))),
        Value::Array(vec![])
    );
}

#[test]
fn nested_pure_reduces_fully() {
    // (* (+ 1 2) (- 10 4)) = 3 * 6 = 18
    let mut ctx = pe_ctx();
    assert_eq!(
        done_num(pe("(* (+ 1 2) (- 10 4))", &mut ctx, &opaque(&[]))),
        18.0
    );
}

#[test]
fn empty_opaque_set_is_passthrough() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe(r#"(str "a" "b")"#, &mut ctx, &opaque(&[]))),
        Value::String("ab".into())
    );
}

#[test]
fn def_form_works() {
    let mut ctx = pe_ctx();
    pe("(def y 7)", &mut ctx, &opaque(&[]));
    assert_eq!(done_num(pe("y", &mut ctx, &opaque(&[]))), 7.0);
}

#[test]
fn let_form_works() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_num(pe("(let [a 3 b 4] (+ a b))", &mut ctx, &opaque(&[]))),
        7.0
    );
}

#[test]
fn do_form_works() {
    let mut ctx = pe_ctx();
    assert_eq!(done_num(pe("(do 1 2 3)", &mut ctx, &opaque(&[]))), 3.0);
}

// ---------------------------------------------------------------------------
// Opaque forms
// ---------------------------------------------------------------------------

#[test]
fn opaque_form_suspends() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(run-activity :foo 42)").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap(),
        Partial::Suspended(_)
    ));
}

#[test]
fn opaque_form_no_args_suspends() {
    let mut ctx = pe_ctx();
    assert!(matches!(
        pe("(tick)", &mut ctx, &opaque(&["tick"])),
        Partial::Suspended(_)
    ));
}

#[test]
fn opaque_bound_arg_is_resolved() {
    // A variable in an opaque form's args is resolved to its value when the form
    // suspends, so the suspended payload carries the value, not the name. This is
    // what lets a workflow thread a `let`-bound input into an activity call.
    let mut ctx = pe_ctx();
    ctx.bind("who", serde_json::json!("ada"));
    let expr = trongoscript_parser::parse_expr("(run-activity :to who)").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::List(items)) = r {
        assert_eq!(items[0], Expr::Symbol("run-activity".into()));
        assert_eq!(items[1], Expr::Keyword("to".into()));
        assert_eq!(items[2], Expr::String("ada".into()));
    } else {
        panic!("expected Suspended(List(...))");
    }
}

#[test]
fn opaque_undefined_arg_errors() {
    // An argument referencing an undefined symbol is now an error — args are
    // reduced, so a typo in an activity argument is caught rather than silently
    // carried through as an unresolved name.
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(run-activity undefined-sym)").unwrap();
    assert!(partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).is_err());
}

#[test]
fn opaque_form_folds_pure_args() {
    // The suspended payload has its pure argument subexpressions folded to
    // literals; the head and keyword labels are preserved.
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(run-activity :task (+ 1 2))").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::List(items)) = r {
        assert_eq!(items[0], Expr::Symbol("run-activity".into()));
        assert_eq!(items[1], Expr::Keyword("task".into()));
        assert_eq!(items[2], Expr::Int(3));
    } else {
        panic!("expected Suspended(List(...))");
    }
}

#[test]
fn multiple_opaque_forms_in_set() {
    let ops = opaque(&["run-activity", "sleep-until"]);
    let mut ctx = pe_ctx();
    assert!(matches!(
        pe("(run-activity :x)", &mut ctx, &ops),
        Partial::Suspended(_)
    ));
    assert!(matches!(
        pe("(sleep-until 100)", &mut ctx, &ops),
        Partial::Suspended(_)
    ));
}

#[test]
fn non_opaque_form_falls_through_to_normal_eval_error() {
    // run-activity not in the opaque set → normal eval → known error.
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(run-activity :x)").unwrap();
    assert!(partial_eval(&expr, &mut ctx, &opaque(&[])).is_err());
}

#[test]
fn nested_opaque_inside_opaque_outer_preserved() {
    // Outer opaque suspends immediately; inner is never touched.
    let mut ctx = pe_ctx();
    let expr =
        trongoscript_parser::parse_expr("(run-activity :outer (run-activity :inner))").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::List(items)) = r {
        assert_eq!(items[0], Expr::Symbol("run-activity".into()));
        assert_eq!(items[1], Expr::Keyword("outer".into()));
        assert_eq!(
            items[2],
            Expr::List(vec![
                Expr::Symbol("run-activity".into()),
                Expr::Keyword("inner".into()),
            ])
        );
    } else {
        panic!("expected Suspended(List(...))");
    }
}

// ---------------------------------------------------------------------------
// List with suspended child — folding behavior
// ---------------------------------------------------------------------------

#[test]
fn list_with_opaque_child_suspends() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(+ (run-activity :x) 1)").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap(),
        Partial::Suspended(_)
    ));
}

#[test]
fn list_pure_args_folded_in_residual() {
    // (+ (run-activity :x) (* 3 4)) — (* 3 4) folds to 12 in the residual.
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(+ (run-activity :x) (* 3 4))").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::List(items)) = r {
        assert_eq!(items[0], Expr::Symbol("+".into()));
        assert!(matches!(&items[1], Expr::List(_)));
        assert_eq!(items[2], Expr::Int(12));
    } else {
        panic!("expected Suspended(List(...))");
    }
}

#[test]
fn list_multiple_opaque_children_both_suspended() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(+ (run-activity :a) (run-activity :b))").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::List(items)) = r {
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[1], Expr::List(_)));
        assert!(matches!(&items[2], Expr::List(_)));
    } else {
        panic!("expected Suspended(List(...))");
    }
}

// ---------------------------------------------------------------------------
// `if` — all branches
// ---------------------------------------------------------------------------

#[test]
fn if_true_condition_opaque_then_suspends() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(if true (run-activity :work) (+ 1 2))").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap(),
        Partial::Suspended(_)
    ));
}

#[test]
fn if_false_condition_pure_else_is_done() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(if false (run-activity :work) (+ 1 2))").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap(),
        Partial::Done(v) if v == Value::Number(3.into())
    ));
}

#[test]
fn if_true_condition_pure_then_no_else() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_num(pe("(if true (+ 10 5))", &mut ctx, &opaque(&[]))),
        15.0
    );
}

#[test]
fn if_false_no_else_returns_nil() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe("(if false 99)", &mut ctx, &opaque(&[]))),
        Value::Null
    );
}

#[test]
fn if_suspended_condition_suspends_whole_if() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(if (run-activity :check) 1 2)").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap(),
        Partial::Suspended(_)
    ));
}

#[test]
fn if_suspended_condition_folds_pure_branches() {
    // Both branches pure; they should be folded in the residual.
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(if (run-activity :c) (* 3 3) (+ 1 1))").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::List(items)) = r {
        assert_eq!(items[0], Expr::Symbol("if".into()));
        assert!(matches!(&items[1], Expr::List(_))); // suspended condition
        assert_eq!(items[2], Expr::Int(9)); // then folded
        assert_eq!(items[3], Expr::Int(2)); // else folded
    } else {
        panic!("expected Suspended(List(if ...))");
    }
}

#[test]
fn if_suspended_condition_no_else_residual_has_three_items() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(if (run-activity :c) 42)").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::List(items)) = r {
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Expr::Symbol("if".into()));
    } else {
        panic!("expected Suspended(List(if ...))");
    }
}

#[test]
fn if_suspended_condition_opaque_then_pure_else() {
    let mut ctx = pe_ctx();
    let expr =
        trongoscript_parser::parse_expr("(if (run-activity :c) (run-activity :work) 0)").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::List(items)) = r {
        assert!(matches!(&items[2], Expr::List(_))); // then still suspended
        assert_eq!(items[3], Expr::Int(0)); // else folded
    } else {
        panic!("expected Suspended");
    }
}

#[test]
fn if_arity_too_few_errors() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(if true)").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&[])),
        Err(EvalError::Arity { .. })
    ));
}

#[test]
fn if_arity_too_many_errors() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(if true 1 2 3)").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&[])),
        Err(EvalError::Arity { .. })
    ));
}

// ---------------------------------------------------------------------------
// Vector literals
// ---------------------------------------------------------------------------

#[test]
fn pure_vector_is_done() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe("[1 2 3]", &mut ctx, &opaque(&[]))),
        Value::Array(vec![
            Value::Number(1.into()),
            Value::Number(2.into()),
            Value::Number(3.into()),
        ])
    );
}

#[test]
fn empty_vector_is_done() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe("[]", &mut ctx, &opaque(&[]))),
        Value::Array(vec![])
    );
}

#[test]
fn vector_with_opaque_child_suspends_and_folds_pure() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("[(+ 1 2) (run-activity :x) 10]").unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::Vector(items)) = r {
        assert_eq!(items[0], Expr::Int(3));
        assert!(matches!(&items[1], Expr::List(_)));
        assert_eq!(items[2], Expr::Int(10));
    } else {
        panic!("expected Suspended(Vector(...))");
    }
}

#[test]
fn vector_all_compound_but_pure_is_done() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe(
            "[(+ 1 1) (* 3 3)]",
            &mut ctx,
            &opaque(&["run-activity"])
        )),
        Value::Array(vec![Value::Number(2.into()), Value::Number(9.into())])
    );
}

// ---------------------------------------------------------------------------
// Map literals
// ---------------------------------------------------------------------------

#[test]
fn pure_map_is_done() {
    let mut ctx = pe_ctx();
    let v = done_val(pe(r#"{:a 1 :b 2}"#, &mut ctx, &opaque(&[])));
    assert_eq!(v["a"], Value::Number(1.into()));
    assert_eq!(v["b"], Value::Number(2.into()));
}

#[test]
fn empty_map_is_done() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_val(pe("{}", &mut ctx, &opaque(&[]))),
        Value::Object(serde_json::Map::new())
    );
}

#[test]
fn map_with_suspended_value_suspends_and_folds_pure() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr(r#"{:result (run-activity :fetch) :count (+ 1 2)}"#)
        .unwrap();
    let r = partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap();
    if let Partial::Suspended(Expr::Map(pairs)) = r {
        assert!(matches!(&pairs[0].1, Expr::List(_))); // :result still opaque
        assert_eq!(pairs[1].1, Expr::Int(3)); // :count folded
    } else {
        panic!("expected Suspended(Map(...))");
    }
}

#[test]
fn map_with_suspended_key_suspends() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr(r#"{(run-activity :k) "value"}"#).unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap(),
        Partial::Suspended(Expr::Map(_))
    ));
}

// ---------------------------------------------------------------------------
// def / let / do with opaque in body
// ---------------------------------------------------------------------------

#[test]
fn def_with_opaque_rhs_errors() {
    // `def` is a binding special form passed straight to the normal evaluator,
    // which errors on run-activity rather than suspending. Gearwork never
    // places opaque forms directly inside def bodies — it uses regular function
    // calls where argument-level suspension works.
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(def x (run-activity :fetch))").unwrap();
    assert!(partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).is_err());
}

#[test]
fn let_with_opaque_body_suspends() {
    // `let` is partially evaluated structurally: a pure binding is applied, and
    // an opaque body suspends rather than erroring. This is what lets a durable
    // workflow thread an activity result through a `let`.
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(let [a 1] (run-activity :work))").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap(),
        Partial::Suspended(_)
    ));
}

#[test]
fn let_with_opaque_binding_suspends() {
    // A binding whose value is opaque suspends the whole let — the body may use
    // the unbound name, so it cannot proceed yet.
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(let [a (run-activity :fetch)] a)").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap(),
        Partial::Suspended(_)
    ));
}

#[test]
fn let_with_pure_body_reduces() {
    // With no opaque forms, a let reduces fully, exactly as `eval` would.
    let mut ctx = pe_ctx();
    assert_eq!(
        done_num(pe("(let [a 2 b 3] (+ a b))", &mut ctx, &opaque(&[]))),
        5.0
    );
}

#[test]
fn do_with_opaque_form_suspends() {
    let mut ctx = pe_ctx();
    let expr = trongoscript_parser::parse_expr("(do 1 (run-activity :work) 3)").unwrap();
    assert!(matches!(
        partial_eval(&expr, &mut ctx, &opaque(&["run-activity"])).unwrap(),
        Partial::Suspended(_)
    ));
}

#[test]
fn do_pure_reduces_to_last() {
    let mut ctx = pe_ctx();
    assert_eq!(done_num(pe("(do 1 2 3)", &mut ctx, &opaque(&[]))), 3.0);
}

// ---------------------------------------------------------------------------
// Context side effects survive across calls
// ---------------------------------------------------------------------------

#[test]
fn def_side_effect_persists() {
    let mut ctx = pe_ctx();
    pe("(def z 55)", &mut ctx, &opaque(&[]));
    assert_eq!(done_num(pe("z", &mut ctx, &opaque(&[]))), 55.0);
}

#[test]
fn sequence_of_calls_shares_context() {
    let mut ctx = pe_ctx();
    pe("(def a 10)", &mut ctx, &opaque(&[]));
    pe("(def b 20)", &mut ctx, &opaque(&[]));
    assert_eq!(done_num(pe("(+ a b)", &mut ctx, &opaque(&[]))), 30.0);
}

// ---------------------------------------------------------------------------
// First-class (non-symbol) head
// ---------------------------------------------------------------------------

#[test]
fn first_class_fn_head_reduces() {
    let mut ctx = pe_ctx();
    assert_eq!(
        done_num(pe("((fn [x] (* x 2)) 5)", &mut ctx, &opaque(&[]))),
        10.0
    );
}

#[test]
fn thread_first_fully_reduces_when_pure() {
    let mut ctx = pe_ctx();
    // No opaque forms: (-> 5 (+ 3) (* 2)) folds to 16.
    assert_eq!(
        done_num(pe("(-> 5 (+ 3) (* 2))", &mut ctx, &opaque(&[]))),
        16.0
    );
}

#[test]
fn thread_first_threads_opaque_result_and_suspends() {
    // (-> (run-activity :a) (+ 1)) must expand to (+ (run-activity :a) 1)
    // and suspend, carrying the threaded opaque sub-form.
    let mut ctx = pe_ctx();
    let r = pe(
        "(-> (run-activity :a) (+ 1))",
        &mut ctx,
        &opaque(&["run-activity"]),
    );
    if let Partial::Suspended(Expr::List(items)) = r {
        assert_eq!(items[0], Expr::Symbol("+".into()));
        assert_eq!(
            items[1],
            Expr::List(vec![
                Expr::Symbol("run-activity".into()),
                Expr::Keyword("a".into()),
            ])
        );
        assert_eq!(items[2], Expr::Int(1));
    } else {
        panic!("expected Suspended(List(...))");
    }
}

#[test]
fn thread_last_threads_opaque_result_and_suspends() {
    // (->> (run-activity :a) (- 100)) expands to (- 100 (run-activity :a)).
    let mut ctx = pe_ctx();
    let r = pe(
        "(->> (run-activity :a) (- 100))",
        &mut ctx,
        &opaque(&["run-activity"]),
    );
    if let Partial::Suspended(Expr::List(items)) = r {
        assert_eq!(items[0], Expr::Symbol("-".into()));
        assert_eq!(items[1], Expr::Int(100));
        assert_eq!(
            items[2],
            Expr::List(vec![
                Expr::Symbol("run-activity".into()),
                Expr::Keyword("a".into()),
            ])
        );
    } else {
        panic!("expected Suspended(List(...))");
    }
}
