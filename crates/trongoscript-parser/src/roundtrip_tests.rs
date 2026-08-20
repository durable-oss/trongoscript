//! The desugaring discipline, pinned as a test.
//!
//! Trongoscript has no sugar-only AST node: every surface syntax the reader
//! accepts produces an [`Expr`] the reader also accepts, and every [`Expr`]
//! the parser can build prints (via `Display`) to source that reads back to
//! the same value.
//!
//! This matters because Trongoscript has `defmacro`, `quasiquote`, and
//! `macroexpand`. A macro constructs `Expr` values directly. If some syntax
//! produced a node only the reader could make, a macro could never build one,
//! and `macroexpand` would print a form that does not parse.
//!
//! The property under test:
//!
//! ```text
//! parse(Display(e)) == e     for every e the parser can produce
//! ```
//!
//! Floats are the one wrinkle: `Display` of an `f64` is not required to
//! round-trip through the number reader (`1e9` prints as `1000000000`, which
//! reads back as an `Int`). The generator therefore emits only floats with a
//! literal fractional part, which do round-trip; the exponent-notation cases
//! are covered separately by [`float_display_reparses_to_equal_value`], which
//! asserts on the value rather than on the node.

use crate::{ast::Expr, parse_expr};

/// Depth-bounded generator over `Expr`.
///
/// Deterministic rather than random: with a small enum and a shallow depth
/// bound, exhaustive-ish enumeration covers more of the space than sampling
/// would, and a failure is reproducible without a seed.
fn generate(depth: usize) -> Vec<Expr> {
    let atoms = vec![
        Expr::Nil,
        Expr::Bool(true),
        Expr::Bool(false),
        Expr::Int(0),
        Expr::Int(42),
        Expr::Int(-7),
        Expr::Int(i64::MAX),
        Expr::Int(i64::MIN),
        Expr::Float(3.25),
        Expr::Float(-0.5),
        Expr::String(String::new()),
        Expr::String("hi".into()),
        // Escapes that `Display` must emit and the reader must consume.
        Expr::String("a\"b".into()),
        Expr::String("a\\b".into()),
        Expr::String("tab\there".into()),
        Expr::String("nl\nhere".into()),
        // Content that would be sugar if it were not inside a string. The
        // printer has to escape `#` and `$` for these, since `#{` and `${`
        // open an interpolation on the way back in.
        Expr::String("#{x}".into()),
        Expr::String("${a + b}".into()),
        // The sigils in every awkward position: bare, doubled, adjacent to
        // the brace, at the ends of the string, and already-escaped.
        Expr::String("#".into()),
        Expr::String("$".into()),
        Expr::String("##{x}".into()),
        Expr::String("$${x}".into()),
        Expr::String("#$".into()),
        Expr::String("a#".into()),
        Expr::String("#a".into()),
        Expr::String("a$".into()),
        Expr::String("{}".into()),
        Expr::String("#{".into()),
        Expr::String("${".into()),
        Expr::String("\\#{x}".into()),
        Expr::String("\\".into()),
        Expr::String("US$5 and #1".into()),
        Expr::String("; not a comment".into()),
        // A symbol containing `$`, which is a legal symbol character but
        // must not print as something that re-reads as a group.
        Expr::Symbol("$".into()),
        Expr::Symbol("$foo".into()),
        Expr::Symbol("a$b".into()),
        Expr::Keyword("foo".into()),
        Expr::Keyword("foo-bar".into()),
        Expr::Symbol("foo".into()),
        Expr::Symbol("foo-bar".into()),
        Expr::Symbol("+".into()),
        Expr::Symbol("-".into()),
        Expr::Symbol("not=".into()),
        Expr::Symbol("my/fn".into()),
        Expr::Symbol("*".into()),
        Expr::Symbol("&".into()),
    ];

    if depth == 0 {
        return atoms;
    }

    let inner = generate(depth - 1);
    let mut out = atoms;

    // A few representative shapes at each arity, rather than the full
    // cross-product, which explodes without adding coverage.
    out.push(Expr::List(Vec::new()));
    out.push(Expr::Vector(Vec::new()));
    out.push(Expr::Map(Vec::new()));

    for e in &inner {
        out.push(Expr::List(vec![e.clone()]));
        out.push(Expr::Vector(vec![e.clone()]));
        out.push(Expr::List(vec![Expr::Symbol("f".into()), e.clone()]));
        out.push(Expr::Map(vec![(Expr::Keyword("k".into()), e.clone())]));
        out.push(Expr::Map(vec![(e.clone(), Expr::Int(1))]));
    }

    // One deeper mixed form, to catch nesting-specific printer bugs.
    out.push(Expr::List(vec![
        Expr::Symbol("let".into()),
        Expr::Vector(vec![Expr::Symbol("x".into()), Expr::Int(1)]),
        Expr::Map(vec![(
            Expr::Keyword("a".into()),
            Expr::Vector(vec![Expr::Int(1), Expr::Symbol("x".into())]),
        )]),
    ]));

    out
}

/// The core property: anything the parser can build prints to source that
/// parses back to the identical AST.
#[test]
fn display_output_reparses_to_the_same_ast() {
    for e in generate(3) {
        let printed = e.to_string();
        let reparsed = parse_expr(&printed).unwrap_or_else(|err| {
            panic!(
                "{:?} printed as {:?}, which failed to parse: {}",
                e, printed, err
            )
        });
        assert_eq!(
            reparsed, e,
            "{:?} printed as {:?}, which parsed back as {:?}",
            e, printed, reparsed
        );
    }
}

/// Floats whose `Display` drops to integer notation still re-read to the same
/// numeric value, even though the node type changes.
#[test]
fn float_display_reparses_to_equal_value() {
    for f in [1e9_f64, 1e-3, 0.0, -0.0, 1.5e300, f64::MIN_POSITIVE] {
        let e = Expr::Float(f);
        let printed = e.to_string();
        let reparsed = parse_expr(&printed)
            .unwrap_or_else(|err| panic!("float {} printed as {:?}: {}", f, printed, err));
        let value = match reparsed {
            Expr::Float(g) => g,
            Expr::Int(i) => i as f64,
            other => panic!(
                "float {} printed as {:?}, parsed as {:?}",
                f, printed, other
            ),
        };
        assert_eq!(value, f, "float {} printed as {:?}", f, printed);
    }
}

/// The specific invariant that string interpolation put at risk: a string
/// whose *content* looks like sugar must print as a string, not as the sugar
/// it resembles.
///
/// Before interpolation existed, `{:?}` was an adequate printer, because no
/// character inside a string had reader significance beyond the escapes Rust
/// already handles. `#{` and `${` changed that, and `Debug` for `str`
/// escapes neither. Without the dedicated printer, emitting a config value
/// of `#{x}` and reading it back yields a `(str x)` call — a string silently
/// becoming a computation, which is the worst class of round-trip bug.
#[test]
fn a_string_containing_sugar_prints_as_a_string() {
    for content in [
        "#{x}",
        "${a + b}",
        "#{",
        "${",
        "#",
        "$",
        "##{x}",
        "a#{x}b",
        "US$5",
        "\\#{x}",
        "#{nested #{deeper}}",
    ] {
        let e = Expr::String(content.to_string());
        let printed = e.to_string();
        let reread = parse_expr(&printed)
            .unwrap_or_else(|err| panic!("{:?} printed as {:?}: {}", content, printed, err));
        assert_eq!(
            reread, e,
            "{:?} printed as {:?}, which read back as {:?}",
            content, printed, reread
        );
        // Specifically: it is still a string, not a call.
        assert!(
            matches!(reread, Expr::String(_)),
            "{:?} printed as {:?}, which is no longer a string literal",
            content,
            printed
        );
    }
}

/// The mirror of the above: sugar in *source* must not survive printing.
/// An interpolated string prints as the `(str ...)` call it already is, and
/// a group prints as its operator call, because neither has a distinct node
/// to print from. This is what keeps `macroexpand` output copy-pasteable.
#[test]
fn sugar_in_source_leaves_no_trace_after_printing() {
    for src in [
        r##""a#{x}b""##,
        r##""#{host}:#{port}""##,
        "${a + b}",
        "${a + b * c}",
        r##""heap: ${n * 2}""##,
    ] {
        let parsed = parse_expr(src).unwrap_or_else(|e| panic!("{} failed: {}", src, e));
        let printed = parsed.to_string();
        assert!(
            !printed.contains("#{") && !printed.contains("${"),
            "{} printed as {}, which still contains sugar",
            src,
            printed
        );
        let reread = parse_expr(&printed)
            .unwrap_or_else(|e| panic!("{} printed as {}: {}", src, printed, e));
        assert_eq!(reread, parsed, "{} printed as {}", src, printed);
    }
}

/// Every sugar the reader accepts must produce a form the reader also
/// accepts on its own. `parse(sugar) == parse(desugaring)` is the definition
/// of each sugar, so it is asserted directly rather than inferred.
///
/// Rows are added here as each sugar lands.
#[test]
fn every_sugar_equals_its_desugaring() {
    let cases: &[(&str, &str)] = &[
        // Reader sugar that predates this test.
        ("'x", "(quote x)"),
        ("'(a b)", "(quote (a b))"),
        ("`x", "(quasiquote x)"),
        ("~x", "(unquote x)"),
        ("~@x", "(unquote-splicing x)"),
        (
            "`(a ~b ~@c)",
            "(quasiquote (a (unquote b) (unquote-splicing c)))",
        ),
        // String interpolation.
        (r##""a#{x}b""##, r#"(str "a" x "b")"#),
        (r##""#{x}""##, "(str x)"),
        (r##""#{host}:#{port}""##, r#"(str host ":" port)"#),
        (r##""n=#{(+ a b)}""##, r#"(str "n=" (+ a b))"#),
        // Infix groups.
        ("${a + b}", "(+ a b)"),
        ("${a + b * c}", "(+ a (* b c))"),
        ("${(a + b) / 2}", "(/ (+ a b) 2)"),
        ("${a % b}", "(mod a b)"),
        ("${a != b}", "(not= a b)"),
        ("${-a}", "(- a)"),
        ("${not a}", "(not a)"),
        // A group inside a string is both sugars at once.
        (r##""v=${a + b}""##, r#"(str "v=" (+ a b))"#),
        // Sugar nested in ordinary forms, since that is where it is written.
        ("(def x ${a + 1})", "(def x (+ a 1))"),
        (r##"{:url "#{h}/api"}"##, r#"{:url (str h "/api")}"#),
    ];

    for (sugar, desugared) in cases {
        let lhs = parse_expr(sugar)
            .unwrap_or_else(|e| panic!("sugar {:?} failed to parse: {}", sugar, e));
        let rhs = parse_expr(desugared)
            .unwrap_or_else(|e| panic!("desugaring {:?} failed to parse: {}", desugared, e));
        assert_eq!(lhs, rhs, "{:?} should desugar to {:?}", sugar, desugared);

        // And the desugaring itself must survive a print/read round trip,
        // so `macroexpand` output stays copy-pasteable.
        let printed = rhs.to_string();
        let reread = parse_expr(&printed).unwrap_or_else(|e| {
            panic!("desugaring of {:?} printed as {:?}: {}", sugar, printed, e)
        });
        assert_eq!(
            reread, rhs,
            "desugaring of {:?} did not survive printing",
            sugar
        );
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
