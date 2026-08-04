#![allow(mismatched_lifetime_syntaxes)]

//! Trongoscript parser — Lisp/s-expression syntax, built with [`nom`].
//!
//! Whitespace- and comma-insensitive (commas are treated as whitespace, per
//! Clojure convention). Supports `;` line comments and `#_` form-discard.
//!
//! Entry points:
//!   - [`parse_module`] — parse a sequence of top-level forms.
//!   - [`parse_expr`]   — parse exactly one expression.
//!   - [`parse_template`] — parse an ERB-like template document.

pub mod ast;
mod template;

pub use ast::*;
pub use template::parse_template;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use nom::{
    branch::alt,
    bytes::complete::{tag, take_while, take_while1, take_while_m_n},
    character::complete::{char as nom_char, one_of},
    combinator::{cut, opt, recognize},
    error::VerboseError,
    multi::many0,
    sequence::{delimited, tuple},
    IResult,
};
use thiserror::Error;

/// Errors returned by the parser.
#[derive(Error, Debug, Clone, PartialEq)]
pub enum ParseError {
    #[error("syntax error at offset {offset}: {message}")]
    Syntax { offset: usize, message: String },
    #[error("unexpected trailing input at offset {offset}: {snippet:?}")]
    Trailing { offset: usize, snippet: String },
    #[error("reader macro `{name}` failed: {message}")]
    ReaderMacro { name: String, message: String },
}

/// Return type of a custom reader macro: the parsed expression plus the
/// number of input *bytes* consumed (including the dispatch prefix).
#[derive(Debug, Clone)]
pub struct ReaderMacroOutput {
    pub expr: Expr,
    pub consumed: usize,
}

/// Signature for a custom reader macro: given the parser input *positioned
/// at the byte after the dispatch char(s)*, parse a form and return it
/// alongside the number of bytes consumed from `rest`.
///
/// The macro should NOT consume the dispatch prefix itself — the parser
/// has already done that.
pub type ReaderMacroFn = Arc<dyn Fn(&str) -> Result<ReaderMacroOutput, ParseError> + Send + Sync>;

/// Parser configuration. Currently exposes the dispatch reader-macro
/// table, which maps a character following `#` to a custom expansion.
///
/// Built-in dispatches (`#_` for form-discard) take precedence and cannot
/// be overridden.
#[derive(Clone, Default)]
pub struct ParserOptions {
    /// `# X ...` — when the parser sees `#X` and `X` is in this map, the
    /// associated function is invoked with the input positioned *after*
    /// `#X`.
    pub dispatch_macros: HashMap<char, ReaderMacroFn>,
}

impl ParserOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a `#X ...` dispatch reader macro.
    pub fn with_dispatch_macro<F>(mut self, ch: char, f: F) -> Self
    where
        F: Fn(&str) -> Result<ReaderMacroOutput, ParseError> + Send + Sync + 'static,
    {
        self.dispatch_macros.insert(ch, Arc::new(f));
        self
    }
}

thread_local! {
    static PARSER_OPTIONS: RefCell<Option<ParserOptions>> = const { RefCell::new(None) };
}

fn with_options<R>(opts: &ParserOptions, f: impl FnOnce() -> R) -> R {
    PARSER_OPTIONS.with(|slot| {
        let prev = slot.replace(Some(opts.clone()));
        let result = f();
        slot.replace(prev);
        result
    })
}

fn lookup_dispatch_macro(ch: char) -> Option<ReaderMacroFn> {
    PARSER_OPTIONS.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|o| o.dispatch_macros.get(&ch).cloned())
    })
}

pub(crate) type PResult<'a, T> = IResult<&'a str, T, VerboseError<&'a str>>;

/// Parse a Trongoscript source string into a [`Module`] — a sequence of
/// top-level forms.
pub fn parse_module(source: &str) -> Result<Module, ParseError> {
    parse_module_with(source, &ParserOptions::default())
}

/// Parse a single Trongoscript expression. The source may contain leading
/// or trailing whitespace and comments but exactly one form.
pub fn parse_expr(source: &str) -> Result<Expr, ParseError> {
    parse_expr_with(source, &ParserOptions::default())
}

/// Like [`parse_module`] but with a custom reader-macro configuration.
pub fn parse_module_with(source: &str, opts: &ParserOptions) -> Result<Module, ParseError> {
    with_options(opts, || {
        let (rest, _) = ws(source).map_err(|e| convert_err(source, e))?;
        let (rest, forms) =
            many0(form_with_trailing_ws)(rest).map_err(|e| convert_err(source, e))?;
        if !rest.is_empty() {
            return Err(ParseError::Trailing {
                offset: source.len() - rest.len(),
                snippet: rest.chars().take(40).collect(),
            });
        }
        Ok(Module { forms })
    })
}

/// Like [`parse_expr`] but with a custom reader-macro configuration.
pub fn parse_expr_with(source: &str, opts: &ParserOptions) -> Result<Expr, ParseError> {
    with_options(opts, || {
        let (rest, _) = ws(source).map_err(|e| convert_err(source, e))?;
        let (rest, e) = form(rest).map_err(|e| convert_err(source, e))?;
        let (rest, _) = ws(rest).map_err(|e| convert_err(source, e))?;
        if !rest.is_empty() {
            return Err(ParseError::Trailing {
                offset: source.len() - rest.len(),
                snippet: rest.chars().take(40).collect(),
            });
        }
        Ok(e)
    })
}

fn convert_err(source: &str, e: nom::Err<VerboseError<&str>>) -> ParseError {
    let (input, msg) = match &e {
        nom::Err::Incomplete(_) => ("", "incomplete input".to_string()),
        nom::Err::Error(ve) | nom::Err::Failure(ve) => ve
            .errors
            .first()
            .map(|(i, k)| (*i, format!("{:?}", k)))
            .unwrap_or(("", "parse error".to_string())),
    };
    let offset = source.len().saturating_sub(input.len());
    ParseError::Syntax {
        offset,
        message: msg,
    }
}

// ---------------------------------------------------------------------------
// Whitespace and comments (commas count as whitespace, Clojure-style).
// ---------------------------------------------------------------------------

fn is_lisp_ws(c: char) -> bool {
    c.is_whitespace() || c == ','
}

fn skip_pure_ws(input: &str) -> &str {
    let end = input
        .char_indices()
        .find(|(_, c)| !is_lisp_ws(*c))
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    &input[end..]
}

fn line_comment(input: &str) -> PResult<()> {
    let (input, _) = nom_char(';')(input)?;
    let (input, _) = take_while(|c: char| c != '\n')(input)?;
    Ok((input, ()))
}

fn discard_comment(input: &str) -> PResult<()> {
    let (input, _) = tag("#_")(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = form(input)?;
    Ok((input, ()))
}

pub(crate) fn ws(mut input: &str) -> PResult<()> {
    loop {
        input = skip_pure_ws(input);
        if let Ok((rest, _)) = line_comment(input) {
            input = rest;
            continue;
        }
        if let Ok((rest, _)) = discard_comment(input) {
            input = rest;
            continue;
        }
        break;
    }
    Ok((input, ()))
}

fn form_with_trailing_ws(input: &str) -> PResult<Expr> {
    let (input, e) = form(input)?;
    let (input, _) = ws(input)?;
    Ok((input, e))
}

// ---------------------------------------------------------------------------
// Forms / atoms
// ---------------------------------------------------------------------------

pub(crate) fn form(input: &str) -> PResult<Expr> {
    alt((
        quote_form,
        quasiquote_form,
        unquote_form,
        dispatch_form,
        list_form,
        vector_form,
        map_form,
        string_form,
        number_form,
        keyword_form,
        symbol_form,
    ))(input)
}

/// `'expr` is sugar for `(quote expr)`.
fn quote_form(input: &str) -> PResult<Expr> {
    let (input, _) = nom_char('\'')(input)?;
    let (input, inner) = form(input)?;
    Ok((
        input,
        Expr::List(vec![Expr::Symbol("quote".to_string()), inner]),
    ))
}

/// `` `expr `` is sugar for `(quasiquote expr)`.
fn quasiquote_form(input: &str) -> PResult<Expr> {
    let (input, _) = nom_char('`')(input)?;
    let (input, inner) = form(input)?;
    Ok((
        input,
        Expr::List(vec![Expr::Symbol("quasiquote".to_string()), inner]),
    ))
}

/// `~expr` → `(unquote expr)`; `~@expr` → `(unquote-splicing expr)`.
fn unquote_form(input: &str) -> PResult<Expr> {
    let (input, _) = nom_char('~')(input)?;
    // Try splicing form `~@` first.
    if let Some(rest) = input.strip_prefix('@') {
        let (rest, inner) = form(rest)?;
        return Ok((
            rest,
            Expr::List(vec![
                Expr::Symbol("unquote-splicing".to_string()),
                inner,
            ]),
        ));
    }
    let (input, inner) = form(input)?;
    Ok((
        input,
        Expr::List(vec![Expr::Symbol("unquote".to_string()), inner]),
    ))
}

/// `#X ...` dispatch: looks up `X` in the parser options' dispatch table.
/// `#_ form` (form-discard) is handled by `ws`/`discard_comment`, so by
/// the time we get here `#_` should already be consumed.
fn dispatch_form(input: &str) -> PResult<Expr> {
    let (rest_after_hash, _) = nom_char('#')(input)?;
    let mut chars = rest_after_hash.chars();
    let Some(ch) = chars.next() else {
        return Err(nom::Err::Error(VerboseError {
            errors: vec![(
                input,
                nom::error::VerboseErrorKind::Context("expected dispatch char after `#`"),
            )],
        }));
    };
    let after_ch = &rest_after_hash[ch.len_utf8()..];
    let Some(handler) = lookup_dispatch_macro(ch) else {
        // No handler registered: treat `#...` as a parse error so we don't
        // silently swallow it.
        return Err(nom::Err::Error(VerboseError {
            errors: vec![(
                input,
                nom::error::VerboseErrorKind::Context("unknown # dispatch macro"),
            )],
        }));
    };
    let output = handler(after_ch).map_err(|_| {
        nom::Err::Failure(VerboseError {
            errors: vec![(
                input,
                nom::error::VerboseErrorKind::Context("reader macro failed"),
            )],
        })
    })?;
    let consumed = output.consumed.min(after_ch.len());
    Ok((&after_ch[consumed..], output.expr))
}

fn list_form(input: &str) -> PResult<Expr> {
    let (input, _) = nom_char('(')(input)?;
    let (input, _) = ws(input)?;
    let (input, items) = many0(form_with_trailing_ws)(input)?;
    let (input, _) = cut(nom_char(')'))(input)?;
    Ok((input, Expr::List(items)))
}

fn vector_form(input: &str) -> PResult<Expr> {
    let (input, _) = nom_char('[')(input)?;
    let (input, _) = ws(input)?;
    let (input, items) = many0(form_with_trailing_ws)(input)?;
    let (input, _) = cut(nom_char(']'))(input)?;
    Ok((input, Expr::Vector(items)))
}

fn map_form(input: &str) -> PResult<Expr> {
    let (input, _) = nom_char('{')(input)?;
    let (input, _) = ws(input)?;
    let (input, items) = many0(form_with_trailing_ws)(input)?;
    let (input, _) = cut(nom_char('}'))(input)?;
    if items.len() % 2 != 0 {
        return Err(nom::Err::Failure(VerboseError {
            errors: vec![(input, nom::error::VerboseErrorKind::Context("map requires an even number of forms"))],
        }));
    }
    let pairs: Vec<(Expr, Expr)> = items
        .chunks(2)
        .map(|p| (p[0].clone(), p[1].clone()))
        .collect();
    Ok((input, Expr::Map(pairs)))
}

fn string_form(input: &str) -> PResult<Expr> {
    let (input, _) = nom_char('"')(input)?;
    let mut out = String::new();
    let mut rest = input;
    loop {
        match rest.chars().next() {
            None => {
                return Err(nom::Err::Failure(VerboseError {
                    errors: vec![(
                        rest,
                        nom::error::VerboseErrorKind::Context("unterminated string literal"),
                    )],
                }));
            }
            Some('"') => {
                let (after, _) = nom_char('"')(rest)?;
                return Ok((after, Expr::String(out)));
            }
            Some('\\') => {
                let mut iter = rest.chars();
                iter.next();
                let esc = iter.next().ok_or_else(|| {
                    nom::Err::Failure(VerboseError {
                        errors: vec![(
                            rest,
                            nom::error::VerboseErrorKind::Context("dangling string escape"),
                        )],
                    })
                })?;
                rest = &rest[1 + esc.len_utf8()..];
                match esc {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    '0' => out.push('\0'),
                    'u' => {
                        let (after, hex) = delimited(
                            nom_char::<&str, VerboseError<&str>>('{'),
                            take_while_m_n(1, 6, |c: char| c.is_ascii_hexdigit()),
                            nom_char('}'),
                        )(rest)?;
                        let code = u32::from_str_radix(hex, 16).map_err(|_| {
                            nom::Err::Failure(VerboseError {
                                errors: vec![(
                                    rest,
                                    nom::error::VerboseErrorKind::Context("bad unicode escape"),
                                )],
                            })
                        })?;
                        if let Some(c) = char::from_u32(code) {
                            out.push(c);
                        }
                        rest = after;
                    }
                    other => out.push(other),
                }
            }
            Some(c) => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
}

fn number_form(input: &str) -> PResult<Expr> {
    let (after, raw) = recognize(tuple((
        opt(nom_char::<&str, VerboseError<&str>>('-')),
        take_while1(|c: char| c.is_ascii_digit()),
        opt(tuple((
            nom_char::<&str, VerboseError<&str>>('.'),
            take_while1(|c: char| c.is_ascii_digit()),
        ))),
        opt(tuple((
            one_of::<&str, _, VerboseError<&str>>("eE"),
            opt(one_of::<&str, _, VerboseError<&str>>("+-")),
            take_while1(|c: char| c.is_ascii_digit()),
        ))),
    )))(input)?;

    if let Some(c) = after.chars().next() {
        if is_symbol_char(c) {
            return Err(nom::Err::Error(VerboseError {
                errors: vec![(
                    after,
                    nom::error::VerboseErrorKind::Context("number followed by symbol char"),
                )],
            }));
        }
    }

    let n: f64 = raw.parse().map_err(|_| {
        nom::Err::Failure(VerboseError {
            errors: vec![(input, nom::error::VerboseErrorKind::Context("bad number"))],
        })
    })?;
    Ok((after, Expr::Number(n)))
}

fn keyword_form(input: &str) -> PResult<Expr> {
    let (input, _) = nom_char(':')(input)?;
    let (input, name) = take_while1(is_symbol_char)(input)?;
    Ok((input, Expr::Keyword(name.to_string())))
}

fn symbol_form(input: &str) -> PResult<Expr> {
    let (input, raw) = take_while1(is_symbol_char)(input)?;
    let expr = match raw {
        "nil" => Expr::Nil,
        "true" => Expr::Bool(true),
        "false" => Expr::Bool(false),
        other => Expr::Symbol(other.to_string()),
    };
    Ok((input, expr))
}

/// Characters allowed in a symbol or keyword name. Excludes the structural
/// delimiters (`(){}[]`), string delimiter `"`, comment marker `;`, quote
/// `'`, comma `,`, leading-of-keyword `:`, backslash, backtick, tilde, and
/// dispatch sigil `#`.
fn is_symbol_char(c: char) -> bool {
    if c.is_whitespace() {
        return false;
    }
    !matches!(
        c,
        '(' | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '"'
            | ';'
            | '\''
            | ','
            | ':'
            | '\\'
            | '`'
            | '~'
            | '#'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn one(src: &str) -> Expr {
        parse_expr(src).expect("parse failed")
    }

    #[test]
    fn parses_atoms() {
        assert_eq!(one("42"), Expr::Number(42.0));
        assert_eq!(one("-3.25"), Expr::Number(-3.25));
        assert_eq!(one("nil"), Expr::Nil);
        assert_eq!(one("true"), Expr::Bool(true));
        assert_eq!(one("false"), Expr::Bool(false));
        assert_eq!(one(":foo"), Expr::Keyword("foo".into()));
        assert_eq!(one("foo"), Expr::Symbol("foo".into()));
        assert_eq!(one(r#""hi""#), Expr::String("hi".into()));
        assert_eq!(one(r#""line\n""#), Expr::String("line\n".into()));
    }

    #[test]
    fn parses_symbols_with_punctuation() {
        assert_eq!(one("+"), Expr::Symbol("+".into()));
        assert_eq!(one("foo-bar"), Expr::Symbol("foo-bar".into()));
        assert_eq!(one("foo?"), Expr::Symbol("foo?".into()));
        assert_eq!(one("foo.bar"), Expr::Symbol("foo.bar".into()));
    }

    #[test]
    fn parses_list() {
        assert_eq!(
            one("(+ 1 2 3)"),
            Expr::List(vec![
                Expr::Symbol("+".into()),
                Expr::Number(1.0),
                Expr::Number(2.0),
                Expr::Number(3.0),
            ])
        );
    }

    #[test]
    fn parses_vector() {
        assert_eq!(
            one("[1 2 3]"),
            Expr::Vector(vec![
                Expr::Number(1.0),
                Expr::Number(2.0),
                Expr::Number(3.0),
            ])
        );
    }

    #[test]
    fn parses_map_with_keywords() {
        let e = one(r#"{:name "alice" :age 30}"#);
        match e {
            Expr::Map(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[0].0, Expr::Keyword("name".into()));
                assert_eq!(pairs[0].1, Expr::String("alice".into()));
                assert_eq!(pairs[1].0, Expr::Keyword("age".into()));
                assert_eq!(pairs[1].1, Expr::Number(30.0));
            }
            other => panic!("expected map, got {:?}", other),
        }
    }

    #[test]
    fn map_with_commas_ok() {
        let _ = one(r#"{:a 1, :b 2, :c 3}"#);
    }

    #[test]
    fn parses_nested() {
        let _ = one(
            r#"
            {:name "alice"
             :tags ["admin" "user"]
             :nested {:active true :count (+ 1 2)}}
        "#,
        );
    }

    #[test]
    fn skips_line_comments() {
        assert_eq!(one("; comment\n42 ; trailing\n"), Expr::Number(42.0));
    }

    #[test]
    fn handles_discard_comment() {
        let m = parse_module("1 #_ 2 3").unwrap();
        assert_eq!(m.forms, vec![Expr::Number(1.0), Expr::Number(3.0)]);
    }

    #[test]
    fn quote_sugar() {
        assert_eq!(
            one("'foo"),
            Expr::List(vec![
                Expr::Symbol("quote".into()),
                Expr::Symbol("foo".into()),
            ])
        );
    }

    #[test]
    fn quasiquote_sugar() {
        assert_eq!(
            one("`foo"),
            Expr::List(vec![
                Expr::Symbol("quasiquote".into()),
                Expr::Symbol("foo".into()),
            ])
        );
    }

    #[test]
    fn unquote_sugar() {
        assert_eq!(
            one("~foo"),
            Expr::List(vec![
                Expr::Symbol("unquote".into()),
                Expr::Symbol("foo".into()),
            ])
        );
    }

    #[test]
    fn unquote_splicing_sugar() {
        assert_eq!(
            one("~@xs"),
            Expr::List(vec![
                Expr::Symbol("unquote-splicing".into()),
                Expr::Symbol("xs".into()),
            ])
        );
    }

    #[test]
    fn nested_quasiquote() {
        // `(if ~cond ~then ~else)` → (quasiquote (if (unquote cond) (unquote then) (unquote else)))
        let e = one("`(if ~cond ~then ~else)");
        if let Expr::List(items) = &e {
            assert_eq!(items[0], Expr::Symbol("quasiquote".into()));
            if let Expr::List(inner) = &items[1] {
                assert_eq!(inner[0], Expr::Symbol("if".into()));
                assert_eq!(
                    inner[1],
                    Expr::List(vec![
                        Expr::Symbol("unquote".into()),
                        Expr::Symbol("cond".into()),
                    ])
                );
            } else {
                panic!("expected inner list");
            }
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn custom_dispatch_macro() {
        // Register `#u expr` → (upcase expr) and verify it parses.
        let opts = ParserOptions::new().with_dispatch_macro('u', |rest: &str| {
            // Parse exactly one form following the `#u`.
            let inner = parse_expr(rest).map_err(|e| ParseError::ReaderMacro {
                name: "u".into(),
                message: format!("{}", e),
            })?;
            // We need to know how many bytes the inner form consumed; we
            // approximate by re-stripping leading WS and using a heuristic:
            // re-serialize the form's source span by length difference.
            // For the test we'll consume the entire rest string.
            Ok(ReaderMacroOutput {
                expr: Expr::List(vec![Expr::Symbol("upcase".into()), inner]),
                consumed: rest.len(),
            })
        });
        let e = parse_expr_with("#u \"hi\"", &opts).unwrap();
        assert_eq!(
            e,
            Expr::List(vec![
                Expr::Symbol("upcase".into()),
                Expr::String("hi".into()),
            ])
        );
    }

    #[test]
    fn module_with_multiple_forms() {
        let m = parse_module("(def x 1) (def y 2) (+ x y)").unwrap();
        assert_eq!(m.forms.len(), 3);
    }

    #[test]
    fn empty_module() {
        assert_eq!(parse_module("").unwrap().forms.len(), 0);
    }

    #[test]
    fn negative_number_vs_minus_symbol() {
        assert_eq!(one("-5"), Expr::Number(-5.0));
        assert_eq!(one("-"), Expr::Symbol("-".into()));
    }

    #[test]
    fn unbalanced_paren_errors() {
        assert!(parse_module("(+ 1 2").is_err());
        assert!(parse_module("(+ 1 2))").is_err());
    }

    #[test]
    fn unbalanced_map_errors() {
        assert!(parse_expr("{:a 1 :b}").is_err());
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.