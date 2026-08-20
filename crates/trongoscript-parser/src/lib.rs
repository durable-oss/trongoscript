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
mod infix;
#[cfg(test)]
mod interp_tests;
#[cfg(test)]
mod roundtrip_tests;
mod template;

use infix::infix_group;

pub use ast::*;
pub use template::parse_template;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use nom::{
    branch::alt,
    bytes::complete::{tag, take_while, take_while1, take_while_m_n},
    character::complete::{char as nom_char, one_of},
    combinator::{cut, opt},
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
    #[error("input nests more than {max} levels deep")]
    TooDeep { max: usize },
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
    /// Current nesting depth inside [`form`]. See [`MAX_NESTING_DEPTH`].
    static NESTING_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Maximum nesting depth the parser will descend to.
///
/// The parser is recursive-descent: every nested list, vector, map, or
/// quote costs native stack frames. Without a limit, input like fifty
/// thousand consecutive `[` overflows the stack and aborts the process
/// before the evaluator — and therefore before any evaluation fuel limit —
/// ever runs.
///
/// # Why this number
///
/// [`parse_module`] is public API and may be called from any thread, so
/// this limit has to hold on the *smallest* stack a caller might have, not
/// on the large one the CLI reserves for evaluation. Measured with a debug
/// build on a 2 MiB thread stack (Rust's default for spawned threads):
/// parsing survives 256 levels and overflows at 512.
///
/// Real data does not nest 256 levels deep. Source that does is either
/// machine-generated pathology or an attack, and either way the honest
/// answer is an error rather than a guess about how much stack is left.
///
/// This is a fixed constant rather than a configurable limit because it
/// guards the parser, which runs before any evaluation config exists.
pub const MAX_NESTING_DEPTH: usize = 256;

/// Run `f` one nesting level deeper, failing if that exceeds
/// [`MAX_NESTING_DEPTH`].
///
/// The depth is restored on every path out, including the error path, so
/// sibling forms at the same level do not accumulate depth between them.
fn nested<'a, T>(input: &'a str, f: impl FnOnce(&'a str) -> PResult<'a, T>) -> PResult<'a, T> {
    let depth = NESTING_DEPTH.with(|d| {
        let next = d.get() + 1;
        d.set(next);
        next
    });
    if depth > MAX_NESTING_DEPTH {
        NESTING_DEPTH.with(|d| d.set(d.get() - 1));
        // `Failure` rather than `Error`: this is not a "try the next
        // alternative" condition, it is a hard stop. An `Error` would let
        // `alt` backtrack and retry, re-descending the same input.
        return Err(nom::Err::Failure(
            <VerboseError<&str> as nom::error::ParseError<&str>>::from_error_kind(
                input,
                nom::error::ErrorKind::TooLarge,
            ),
        ));
    }
    let result = f(input);
    NESTING_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    result
}

/// Reset the depth counter. Called at each top-level entry point so a
/// parse that bailed mid-way cannot leave the thread-local dirty for the
/// next parse on the same thread.
fn reset_nesting_depth() {
    NESTING_DEPTH.with(|d| d.set(0));
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
        reset_nesting_depth();
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
        reset_nesting_depth();
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
    // A depth bail-out is reported as its own error rather than as a
    // generic syntax error, so callers can tell "this input is hostile"
    // from "this input has a typo".
    if let nom::Err::Error(ve) | nom::Err::Failure(ve) = &e {
        if ve.errors.iter().any(|(_, k)| {
            matches!(
                k,
                nom::error::VerboseErrorKind::Nom(nom::error::ErrorKind::TooLarge)
            )
        }) {
            return ParseError::TooDeep {
                max: MAX_NESTING_DEPTH,
            };
        }
    }
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
    // Every nested construct routes through here, so this is the one place
    // the recursion depth needs guarding.
    nested(input, |input| {
        alt((
            // `${` must be checked before `symbol_form`: `$` is a legal
            // symbol char, so `$` alone would otherwise be munched into a
            // symbol. A symbol can never contain `{`, so there is no
            // ambiguity to resolve — this is a pure prepend, and a bare `$`
            // still reads as a symbol.
            infix_group,
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
    })
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
            Expr::List(vec![Expr::Symbol("unquote-splicing".to_string()), inner]),
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
            errors: vec![(
                input,
                nom::error::VerboseErrorKind::Context("map requires an even number of forms"),
            )],
        }));
    }
    let pairs: Vec<(Expr, Expr)> = items
        .chunks(2)
        .map(|p| (p[0].clone(), p[1].clone()))
        .collect();
    Ok((input, Expr::Map(pairs)))
}

/// A double-quoted string, with `#{expr}` interpolation.
///
/// A string containing no interpolation stays an [`Expr::String`]. One that
/// does interpolate desugars to `(str ...)` over its alternating literal and
/// expression segments — and that desugaring is the definition: `"a#{x}b"`
/// and `(str "a" x "b")` are the same AST after read.
///
/// Keeping the no-interpolation case a literal matters: map keys are
/// frequently strings, and turning every one of them into a call would cost
/// constant-folding everywhere for a feature most strings do not use.
fn string_form(input: &str) -> PResult<Expr> {
    let (input, _) = nom_char('"')(input)?;
    let mut out = String::new();
    // Non-empty only once an interpolation has been seen. `out` holds the
    // literal text accumulated since the last segment boundary.
    let mut segments: Vec<Expr> = Vec::new();
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
                if segments.is_empty() {
                    return Ok((after, Expr::String(out)));
                }
                // Trailing literal, if any. An empty tail contributes
                // nothing to the concatenation and is dropped.
                if !out.is_empty() {
                    segments.push(Expr::String(out));
                }
                let mut call = Vec::with_capacity(segments.len() + 1);
                call.push(Expr::Symbol("str".to_string()));
                call.extend(segments);
                return Ok((after, Expr::List(call)));
            }
            // `#{ expr }` interpolates; `${ infix }` interpolates an infix
            // group. A bare `#` or `$` not followed by `{` is ordinary
            // string content.
            Some(c @ ('#' | '$')) if rest[c.len_utf8()..].starts_with('{') => {
                if !out.is_empty() {
                    segments.push(Expr::String(std::mem::take(&mut out)));
                }
                let (after, expr) = if c == '#' {
                    // `#{` — the body is an ordinary form. Consume the
                    // braces here and let `form` handle everything between,
                    // including nested strings that interpolate in turn.
                    let (b, _) = nom_char('{')(&rest['#'.len_utf8()..])?;
                    let (b, _) = ws(b)?;
                    let (b, e) = cut(form)(b)?;
                    let (b, _) = ws(b)?;
                    let (b, _) = cut(nom_char('}'))(b)?;
                    (b, e)
                } else {
                    // `${` — the same production as an infix group outside
                    // a string, value interpolated rather than returned.
                    // `infix_group` expects to see the `$` itself.
                    infix_group(rest)?
                };
                segments.push(expr);
                rest = after;
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
                    // `\#` and `\$` write a literal sigil, so `\#{` is the
                    // escape hatch out of interpolation.
                    '#' => out.push('#'),
                    '$' => out.push('$'),
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
    let (after, (_, _, frac, exp)) = tuple((
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
    ))(input)?;
    let raw = &input[..input.len() - after.len()];

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

    // A literal with a fractional part or exponent is a float; a bare
    // integer literal is an int unless it overflows i64, in which case it
    // falls back to float (matching the range the old f64-only lexer
    // accepted).
    if frac.is_none() && exp.is_none() {
        if let Ok(i) = raw.parse::<i64>() {
            return Ok((after, Expr::Int(i)));
        }
    }

    let n: f64 = raw.parse().map_err(|_| {
        nom::Err::Failure(VerboseError {
            errors: vec![(input, nom::error::VerboseErrorKind::Context("bad number"))],
        })
    })?;
    Ok((after, Expr::Float(n)))
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
        '(' | ')' | '[' | ']' | '{' | '}' | '"' | ';' | '\'' | ',' | ':' | '\\' | '`' | '~' | '#'
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
        assert_eq!(one("42"), Expr::Int(42));
        assert_eq!(one("-3.25"), Expr::Float(-3.25));
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
                Expr::Int(1),
                Expr::Int(2),
                Expr::Int(3),
            ])
        );
    }

    #[test]
    fn parses_vector() {
        assert_eq!(
            one("[1 2 3]"),
            Expr::Vector(vec![Expr::Int(1), Expr::Int(2), Expr::Int(3)])
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
                assert_eq!(pairs[1].1, Expr::Int(30));
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
        let _ = one(r#"
            {:name "alice"
             :tags ["admin" "user"]
             :nested {:active true :count (+ 1 2)}}
        "#);
    }

    #[test]
    fn skips_line_comments() {
        assert_eq!(one("; comment\n42 ; trailing\n"), Expr::Int(42));
    }

    #[test]
    fn handles_discard_comment() {
        let m = parse_module("1 #_ 2 3").unwrap();
        assert_eq!(m.forms, vec![Expr::Int(1), Expr::Int(3)]);
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
        assert_eq!(one("-5"), Expr::Int(-5));
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

#[cfg(test)]
mod depth_probe {
    /// The parser guard must hold on a *small* stack, not just the large
    /// one the CLI reserves for evaluation: `parse_module` is public API
    /// and an embedder may call it from any thread.
    ///
    /// 2 MiB is Rust's default stack for a spawned thread, so this is the
    /// realistic worst case. If this test starts aborting the whole test
    /// binary with "has overflowed its stack", `MAX_NESTING_DEPTH` has
    /// been raised past what a small stack can take.
    #[test]
    fn max_nesting_depth_is_survivable_on_a_small_stack() {
        let h = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                // Unterminated, so the parser descends the full depth
                // before it can fail on anything else.
                let src = "[".repeat(super::MAX_NESTING_DEPTH * 2) + "1";
                match super::parse_module(&src) {
                    Err(super::ParseError::TooDeep { max }) => {
                        assert_eq!(max, super::MAX_NESTING_DEPTH);
                    }
                    other => panic!("expected TooDeep, got {other:?}"),
                }
            })
            .unwrap();
        h.join()
            .expect("parser overflowed a 2 MiB stack at MAX_NESTING_DEPTH");
    }

    /// Nesting just under the ceiling must still parse. A guard that
    /// rejects legitimate input is as broken as one that lets an attack
    /// through.
    #[test]
    fn nesting_just_below_the_ceiling_still_parses() {
        let depth = super::MAX_NESTING_DEPTH - 1;
        let src = "[".repeat(depth) + "1" + &"]".repeat(depth);
        assert!(super::parse_module(&src).is_ok());
    }

    /// Depth must not accumulate across sibling forms: a thousand
    /// shallow forms in a row is not deep nesting.
    #[test]
    fn sibling_forms_do_not_accumulate_depth() {
        let src = "[1] ".repeat(1000);
        let m = super::parse_module(&src).expect("siblings should parse");
        assert_eq!(m.forms.len(), 1000);
    }

    /// A failed parse must not leave the thread-local depth counter dirty
    /// for the next parse on the same thread.
    #[test]
    fn depth_counter_resets_after_a_failed_parse() {
        let too_deep = "[".repeat(super::MAX_NESTING_DEPTH * 2) + "1";
        assert!(super::parse_module(&too_deep).is_err());
        // Same thread, immediately after: an ordinary parse must work.
        assert!(super::parse_module("[1 2 3]").is_ok());
    }
}
