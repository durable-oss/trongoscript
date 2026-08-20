//! String interpolation: `#{expr}` and `${infix}` inside a string literal.

use crate::{ast::Expr, parse_expr};
use pretty_assertions::assert_eq;

/// The desugaring is the definition, so each case asserts that the sugar and
/// its `(str ...)` form read to the same AST.
fn same(sugar: &str, sexp: &str) {
    let lhs = parse_expr(sugar).unwrap_or_else(|e| panic!("{} failed: {}", sugar, e));
    let rhs = parse_expr(sexp).unwrap_or_else(|e| panic!("{} failed: {}", sexp, e));
    assert_eq!(lhs, rhs, "{} should equal {}", sugar, sexp);
}

#[test]
fn a_string_without_interpolation_stays_a_literal() {
    // This is load-bearing. Map keys are frequently strings; turning every
    // one of them into a `(str ...)` call would cost constant-folding
    // everywhere for a feature most strings do not use.
    assert_eq!(
        parse_expr(r#""plain""#).unwrap(),
        Expr::String("plain".into())
    );
    assert_eq!(parse_expr(r#""""#).unwrap(), Expr::String(String::new()));
    // A `#` or `$` not followed by `{` is ordinary content.
    assert_eq!(
        parse_expr(r##""a#b""##).unwrap(),
        Expr::String("a#b".into())
    );
    assert_eq!(
        parse_expr(r#""US$5""#).unwrap(),
        Expr::String("US$5".into())
    );
    assert_eq!(parse_expr(r##""#""##).unwrap(), Expr::String("#".into()));
}

#[test]
fn interpolation_desugars_to_str() {
    same(r##""a#{x}b""##, r#"(str "a" x "b")"#);
    same(r##""#{x}""##, "(str x)");
    same(r##""#{x}#{y}""##, "(str x y)");
    same(r##""pre#{x}""##, r#"(str "pre" x)"#);
    same(r##""#{x}post""##, r#"(str x "post")"#);
}

#[test]
fn the_config_shapes_this_exists_for() {
    same(
        r##""https://#{host}:#{port}/api""##,
        r#"(str "https://" host ":" port "/api")"#,
    );
    same(
        r##""/var/log/#{app-name}/#{env}.log""##,
        r#"(str "/var/log/" app-name "/" env ".log")"#,
    );
    same(
        r##""postgres://#{user}@#{db-host}/#{db-name}""##,
        r#"(str "postgres://" user "@" db-host "/" db-name)"#,
    );
}

#[test]
fn the_body_is_a_full_form_not_just_a_name() {
    same(r##""n=#{(+ a b)}""##, r#"(str "n=" (+ a b))"#);
    same(r##""#{(get m :k)}""##, "(str (get m :k))");
    same(r##""#{[1 2]}""##, "(str [1 2])");
    same(r##""#{:kw}""##, "(str :kw)");
    // Whitespace inside the braces is insignificant.
    same(r##""#{  x  }""##, "(str x)");
}

#[test]
fn infix_groups_interpolate_too() {
    same(
        r##""heap: ${total-ram * 3 / 4} bytes""##,
        r#"(str "heap: " (/ (* total-ram 3) 4) " bytes")"#,
    );
    same(r##""${a + b}""##, "(str (+ a b))");
    // `#{${...}}` is an interpolation whose body is a group — the same
    // thing as `"${...}"` with redundant syntax. Legal, undocumented.
    same(r##""#{${a + b}}""##, r##""${a + b}""##);
}

#[test]
fn escaping_a_literal_sigil() {
    // `\#` and `\$` are the escape hatch out of interpolation.
    assert_eq!(
        parse_expr(r##""\#{not-interpolated}""##).unwrap(),
        Expr::String("#{not-interpolated}".into())
    );
    assert_eq!(
        parse_expr(r#""\${literal}""#).unwrap(),
        Expr::String("${literal}".into())
    );
    // And an escaped sigil in a string that interpolates elsewhere. The
    // desugaring's own literal needs the escape too, since it is a string
    // whose content is `#{a}`.
    same(r##""\#{a}#{b}""##, r##"(str "\#{a}" b)"##);
}

/// The unicode escape uses `{` immediately after `\u`, and is consumed by
/// the escape branch before the interpolation branch ever sees it. Order
/// matters here, so it is pinned.
#[test]
fn unicode_escapes_still_work() {
    assert_eq!(parse_expr(r#""\u{41}""#).unwrap(), Expr::String("A".into()));
    assert_eq!(
        parse_expr(r#""\u{1F600}""#).unwrap(),
        Expr::String("\u{1F600}".into())
    );
    // Alongside an interpolation.
    same(r##""\u{41}#{x}""##, r#"(str "A" x)"#);
    same(r##""#{x}\u{41}""##, r#"(str x "A")"#);
}

#[test]
fn other_escapes_are_unaffected() {
    same(r##""a\nb#{x}""##, "(str \"a\\nb\" x)");
    assert_eq!(
        parse_expr(r#""a\"b""#).unwrap(),
        Expr::String("a\"b".into())
    );
    assert_eq!(
        parse_expr(r#""a\\b""#).unwrap(),
        Expr::String("a\\b".into())
    );
}

/// The body of `#{...}` is a full form, so it may contain a string, which
/// may itself interpolate. Legal, and nobody should write it.
#[test]
fn interpolation_nests() {
    same(
        r##""#{(str "a#{b}c")}""##,
        r##"(str (str (str "a" b "c")))"##,
    );
}

#[test]
fn unterminated_interpolation_is_an_error() {
    assert!(parse_expr(r##""#{x""##).is_err());
    assert!(parse_expr(r##""#{""##).is_err());
    assert!(parse_expr(r##""#{}""##).is_err());
}

/// An interpolated string is a `(str ...)` call, so it prints as one and
/// reads back identically — the discipline holds for this sugar too.
#[test]
fn interpolated_strings_round_trip_through_display() {
    for src in [
        r##""a#{x}b""##,
        r##""https://#{host}:#{port}/api""##,
        r##""${a + b}""##,
        r##""\#{escaped}""##,
        r#""plain""#,
    ] {
        let parsed = parse_expr(src).unwrap_or_else(|e| panic!("{} failed: {}", src, e));
        let printed = parsed.to_string();
        let reread = parse_expr(&printed)
            .unwrap_or_else(|e| panic!("{} printed as {} which failed: {}", src, printed, e));
        assert_eq!(reread, parsed, "{} printed as {}", src, printed);
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
