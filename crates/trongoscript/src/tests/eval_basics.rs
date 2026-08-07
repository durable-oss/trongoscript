use super::super::*;
use pretty_assertions::assert_eq;

fn eval_str(src: &str) -> Value {
    let mut ctx = Context::new(EvalConfig::default());
    eval_source(src, &mut ctx).expect("eval failed")
}

#[test]
fn evaluates_literals() {
    assert_eq!(eval_str("42"), Value::Number(42.into()));
    assert_eq!(eval_str("\"hi\""), Value::String("hi".into()));
    assert_eq!(eval_str("true"), Value::Bool(true));
    assert_eq!(eval_str("nil"), Value::Null);
    assert_eq!(eval_str(":foo"), Value::String("foo".into()));
}

#[test]
fn evaluates_arithmetic() {
    assert_eq!(eval_str("(+ 1 2 3)"), Value::Number(6.into()));
    assert_eq!(eval_str("(* 2 3 4)"), Value::Number(24.into()));
    assert_eq!(eval_str("(- 10 3 2)"), Value::Number(5.into()));
}

#[test]
fn evaluates_map_with_keywords() {
    let v = eval_str(r#"{:name "alice" :age 30}"#);
    let obj = v.as_object().unwrap();
    assert_eq!(obj.get("name"), Some(&Value::String("alice".into())));
    assert_eq!(obj.get("age"), Some(&Value::Number(30.into())));
}

#[test]
fn evaluates_let() {
    let v = eval_str("(let [x 10 y 20] (+ x y))");
    assert_eq!(v, Value::Number(30.into()));
}

#[test]
fn evaluates_def_then_use() {
    let v = eval_str("(def x 5) (* x x)");
    assert_eq!(v, Value::Number(25.into()));
}

#[test]
fn if_truthy_branches() {
    assert_eq!(eval_str("(if true 1 2)"), Value::Number(1.into()));
    assert_eq!(eval_str("(if false 1 2)"), Value::Number(2.into()));
    assert_eq!(eval_str("(if nil 1 2)"), Value::Number(2.into()));
    assert_eq!(eval_str("(if 0 1 2)"), Value::Number(1.into())); // 0 is truthy
}

#[test]
fn nested_data() {
    let v = eval_str(
        r#"
        {:name (upcase "alice")
         :age (+ 20 10)
         :tags ["a" "b" "c"]}
        "#,
    );
    let obj = v.as_object().unwrap();
    assert_eq!(obj.get("name"), Some(&Value::String("ALICE".into())));
    assert_eq!(obj.get("age"), Some(&Value::Number(30.into())));
}

#[test]
fn undefined_function() {
    let mut ctx = Context::new(EvalConfig::default());
    let r = eval_source("(no-such-func 1)", &mut ctx);
    assert!(matches!(r, Err(EvalError::UndefinedFunction(_))));
}

#[test]
fn include_requires_sandbox() {
    let mut ctx = Context::new(EvalConfig::default());
    let r = eval_source("(include \"foo.trongo\")", &mut ctx);
    assert!(matches!(r, Err(EvalError::NoSandbox)));
}

#[test]
fn simple_template_macro() {
    let v = eval_str(
        "(defmacro unless [c body] (if c nil body))
         (unless false 42)",
    );
    assert_eq!(v, Value::Number(42.into()));
}

#[test]
fn macro_with_quasiquote() {
    let v = eval_str(
        "(defmacro inc [x] `(+ ~x 1))
         (inc 41)",
    );
    assert_eq!(v, Value::Number(42.into()));
}

#[test]
fn macro_with_unquote_splicing() {
    let v = eval_str(
        "(defmacro sum-of [& xs] `(+ ~@xs))
         (sum-of 1 2 3 4)",
    );
    assert_eq!(v, Value::Number(10.into()));
}

#[test]
fn macro_with_rest_param_no_quasi() {
    let v = eval_str(
        "(defmacro first-of [& xs] (first xs))
         (first-of 100 200 300)",
    );
    assert_eq!(v, Value::Number(100.into()));
}

#[test]
fn macroexpand_returns_expansion() {
    let v = eval_str(
        "(defmacro unless [c b] (if c nil b))
         (macroexpand '(unless x y))",
    );
    let arr = v.as_array().unwrap();
    assert_eq!(arr[0], Value::String("if".into()));
    assert_eq!(arr[1], Value::String("x".into()));
    assert_eq!(arr[2], Value::Null);
    assert_eq!(arr[3], Value::String("y".into()));
}

#[test]
fn macro_expansion_depth_bounded() {
    let mut ctx = Context::new(EvalConfig::default());
    ctx.max_macro_expansion_depth = 10;
    let r = eval_source(
        "(defmacro loop-me [] (loop-me))
         (loop-me)",
        &mut ctx,
    );
    assert!(matches!(r, Err(EvalError::InvalidForm(_))));
}

#[test]
fn quasiquote_alone_works_as_data_literal() {
    let v = eval_str("`[1 2 ~(+ 1 2) 4]");
    let arr = v.as_array().unwrap();
    assert_eq!(arr[0], Value::Number(1.into()));
    assert_eq!(arr[2], Value::Number(3.into()));
    assert_eq!(arr[3], Value::Number(4.into()));
}

#[test]
fn thread_first_threads_as_first_arg() {
    // ((5 + 3) * 2) = 16
    assert_eq!(eval_str("(-> 5 (+ 3) (* 2))"), Value::Number(16.into()));
    // bare symbol step wraps as a single-arg call
    assert_eq!(
        eval_str("(use predicates) (-> 10 inc)"),
        Value::Number(11.into())
    );
    // first arg position matters: (- 10 3) = 7
    assert_eq!(eval_str("(-> 10 (- 3))"), Value::Number(7.into()));
    // single value passes through
    assert_eq!(eval_str("(-> 5)"), Value::Number(5.into()));
}

#[test]
fn thread_last_threads_as_last_arg() {
    // (1 - (100 - 5)) = -94
    assert_eq!(eval_str("(->> 5 (- 100) (- 1))"), Value::Number((-94).into()));
    // last arg position: (- 3 10) = -7
    assert_eq!(eval_str("(->> 10 (- 3))"), Value::Number((-7).into()));
}

#[test]
fn cons_and_list_builtins() {
    let v = eval_str("(cons 1 (list 2 3))");
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0], Value::Number(1.into()));
    assert_eq!(arr[2], Value::Number(3.into()));

    assert_eq!(eval_str("(list)"), Value::Array(vec![]));
    assert_eq!(eval_str("(list? (list 1))"), Value::Bool(true));
    assert_eq!(eval_str("(list? 5)"), Value::Bool(false));
}
