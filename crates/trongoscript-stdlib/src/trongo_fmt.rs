//! Emit Trongoscript source text from a [`Value`].
//!
//! This is the inverse of parsing: it turns the JSON-shaped value model
//! back into the data-literal subset of Trongoscript, so a JSON, YAML, or
//! TOML document can be re-expressed as a Trongoscript form.
//!
//! ```text
//! {"a": 1, "b": [1, 2]}   =>   {:a 1 :b [1 2]}
//! ```
//!
//! Only data literals are produced — nil, booleans, numbers, strings,
//! vectors, and maps. Nothing here emits a call form, so the output is
//! always inert data that evaluates to the value it came from.
//!
//! The three things worth knowing:
//!
//! * **Numbers.** Integers print without a decimal point and keep their
//!   full 64-bit precision, so a value past 2^53 survives the trip through
//!   text even though the parser will read it back as `f64`.
//! * **Map keys.** A key is written as `:keyword` when it can be read back
//!   as one, and as a quoted string otherwise. Keys with spaces, braces, or
//!   a leading `:` fall into the string case.
//! * **Layout.** [`to_source`] emits one line; [`to_source_pretty`] indents
//!   nested collections two spaces per level.

use crate::Value;

/// Render `v` as a single line of Trongoscript.
pub fn to_source(v: &Value) -> String {
    let mut out = String::new();
    write_compact(v, &mut out);
    out
}

/// Render `v` as indented Trongoscript, two spaces per nesting level.
///
/// Empty collections and collections of scalars stay on one line; only a
/// collection holding another collection is broken across lines. That keeps
/// `[1 2 3]` from becoming four lines while still making a deep config
/// readable.
pub fn to_source_pretty(v: &Value) -> String {
    let mut out = String::new();
    write_pretty(v, 0, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Compact
// ---------------------------------------------------------------------------

fn write_compact(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("nil"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&format_number(n)),
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write_compact(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write_key(k, out);
                out.push(' ');
                write_compact(val, out);
            }
            out.push('}');
        }
    }
}

// ---------------------------------------------------------------------------
// Pretty
// ---------------------------------------------------------------------------

/// True when `v` holds a collection somewhere directly inside it. A vector
/// of scalars reads fine on one line; a vector of maps does not.
fn has_nested_collection(v: &Value) -> bool {
    match v {
        Value::Array(items) => items
            .iter()
            .any(|i| matches!(i, Value::Array(_) | Value::Object(_))),
        Value::Object(map) => map
            .values()
            .any(|i| matches!(i, Value::Array(_) | Value::Object(_))),
        _ => false,
    }
}

fn indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_pretty(v: &Value, depth: usize, out: &mut String) {
    match v {
        Value::Array(items) if !items.is_empty() && has_nested_collection(v) => {
            out.push_str("[\n");
            for item in items {
                indent(depth + 1, out);
                write_pretty(item, depth + 1, out);
                out.push('\n');
            }
            indent(depth, out);
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() && has_nested_collection(v) => {
            out.push_str("{\n");
            for (k, val) in map {
                indent(depth + 1, out);
                write_key(k, out);
                out.push(' ');
                write_pretty(val, depth + 1, out);
                out.push('\n');
            }
            indent(depth, out);
            out.push('}');
        }
        // Scalars, empty collections, and flat collections of scalars.
        other => write_compact(other, out),
    }
}

// ---------------------------------------------------------------------------
// Leaves
// ---------------------------------------------------------------------------

/// Format a number the way the reader will accept it.
///
/// Integers go through the integer accessors first, so they print bare
/// (`42`, not `42.0`) and keep every digit no matter the magnitude.
///
/// Floats use Rust's shortest round-tripping form, except that `f64`'s
/// `Display` never switches to exponent notation — it would spell `1e300`
/// as 301 digits, which both bloats the output and implies precision the
/// value does not have. Past a length threshold, switch to `{:e}`, which
/// the reader's number grammar also accepts.
///
/// Non-finite values cannot reach here: `serde_json::Number` has no NaN or
/// infinity, and both parsers in [`crate::serde_fmt`] reject them at the
/// door.
fn format_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let Some(f) = n.as_f64() else {
        return "0".to_string();
    };
    let plain = f.to_string();
    // 20 digits covers every value an f64 represents exactly, so anything
    // longer is padding from an extreme exponent.
    if plain.len() > 20 {
        format!("{f:e}")
    } else {
        plain
    }
}

/// Write `s` as a Trongoscript string literal, including the quotes.
///
/// Escapes exactly the set the reader understands: `\n`, `\t`, `\r`, `\\`,
/// `\"`, `\0`, and `\u{XXXX}` for the remaining control characters. Every
/// other character, including non-ASCII, is emitted literally.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\0' => out.push_str("\\0"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Write a map key, preferring `:keyword` and falling back to a string.
fn write_key(k: &str, out: &mut String) {
    if is_keyword_safe(k) {
        out.push(':');
        out.push_str(k);
    } else {
        write_string(k, out);
    }
}

/// Whether `name` can follow a `:` and read back as the same keyword.
///
/// The reader takes every character up to a delimiter as part of the
/// keyword, so the test is simply that the name is non-empty and made
/// entirely of symbol characters. A `:` inside the name is excluded because
/// the reader would stop there, leaving a trailing fragment behind.
fn is_keyword_safe(name: &str) -> bool {
    !name.is_empty() && name.chars().all(is_symbol_char)
}

/// Mirrors the reader's `is_symbol_char`: everything except whitespace, the
/// structural delimiters, and the reserved sigils.
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
    use serde_json::json;

    // -- scalars ------------------------------------------------------------

    #[test]
    fn scalars_render_as_atoms() {
        assert_eq!(to_source(&Value::Null), "nil");
        assert_eq!(to_source(&json!(true)), "true");
        assert_eq!(to_source(&json!(false)), "false");
        assert_eq!(to_source(&json!("hi")), r#""hi""#);
    }

    #[test]
    fn integers_have_no_decimal_point() {
        assert_eq!(to_source(&json!(42)), "42");
        assert_eq!(to_source(&json!(-7)), "-7");
        assert_eq!(to_source(&json!(0)), "0");
    }

    #[test]
    fn large_integers_keep_every_digit() {
        // Past 2^53, so a trip through f64 would round.
        assert_eq!(to_source(&json!(9007199254740993i64)), "9007199254740993");
        assert_eq!(to_source(&json!(u64::MAX)), "18446744073709551615");
        assert_eq!(to_source(&json!(i64::MIN)), "-9223372036854775808");
    }

    #[test]
    fn floats_round_trip_their_shortest_form() {
        assert_eq!(to_source(&json!(3.25)), "3.25");
        assert_eq!(to_source(&json!(-0.5)), "-0.5");
    }

    #[test]
    fn whole_valued_floats_lose_the_trailing_zero() {
        // JSON stores 3.0 as a float, but `3` reads back as the same value.
        assert_eq!(to_source(&json!(3.0)), "3");
    }

    #[test]
    fn extreme_floats_use_exponent_form() {
        // f64's Display would spell these out in full — 301 digits for
        // 1e300. The reader's number grammar accepts the exponent form.
        assert_eq!(to_source(&json!(1e300)), "1e300");
        assert_eq!(to_source(&json!(-1e300)), "-1e300");
        assert_eq!(to_source(&json!(1e-300)), "1e-300");
        // A subnormal, where the mantissa needs its full width.
        assert_eq!(
            to_source(&json!(f64::MIN_POSITIVE / 2.0)),
            "1.1125369292536007e-308"
        );
    }

    #[test]
    fn ordinary_floats_keep_the_plain_spelling() {
        // Just under the length threshold, so no exponent switch.
        assert_eq!(to_source(&json!(0.1)), "0.1");
        assert_eq!(to_source(&json!(1234567.891)), "1234567.891");
    }

    // -- strings ------------------------------------------------------------

    #[test]
    fn strings_escape_what_the_reader_understands() {
        assert_eq!(to_source(&json!("a\nb")), r#""a\nb""#);
        assert_eq!(to_source(&json!("a\tb")), r#""a\tb""#);
        assert_eq!(to_source(&json!("a\"b")), r#""a\"b""#);
        assert_eq!(to_source(&json!("a\\b")), r#""a\\b""#);
    }

    #[test]
    fn other_control_chars_use_unicode_escapes() {
        assert_eq!(to_source(&json!("a\u{1}b")), r#""a\u{1}b""#);
    }

    #[test]
    fn non_ascii_is_emitted_literally() {
        assert_eq!(to_source(&json!("héllo → 世界")), r#""héllo → 世界""#);
    }

    // -- collections --------------------------------------------------------

    #[test]
    fn arrays_become_vectors() {
        assert_eq!(to_source(&json!([1, 2, 3])), "[1 2 3]");
        assert_eq!(to_source(&json!([])), "[]");
        assert_eq!(to_source(&json!([[1], [2]])), "[[1] [2]]");
    }

    #[test]
    fn objects_become_maps_with_keyword_keys() {
        assert_eq!(to_source(&json!({"a": 1})), "{:a 1}");
        assert_eq!(to_source(&json!({})), "{}");
    }

    #[test]
    fn nested_structures_compose() {
        assert_eq!(
            to_source(&json!({"a": 1, "b": [1, {"c": nil_value()}]})),
            "{:a 1 :b [1 {:c nil}]}"
        );
    }

    fn nil_value() -> Value {
        Value::Null
    }

    // -- keys ---------------------------------------------------------------

    #[test]
    fn keys_needing_quotes_become_strings() {
        assert_eq!(to_source(&json!({"has space": 1})), r#"{"has space" 1}"#);
        assert_eq!(to_source(&json!({"": 1})), r#"{"" 1}"#);
        assert_eq!(to_source(&json!({"a:b": 1})), r#"{"a:b" 1}"#);
        assert_eq!(to_source(&json!({"a{b": 1})), r#"{"a{b" 1}"#);
        assert_eq!(to_source(&json!({"a;b": 1})), r#"{"a;b" 1}"#);
    }

    #[test]
    fn numeric_and_reserved_keys_stay_keywords() {
        // `:42` and `:nil` are unambiguous after the leading colon — the
        // reader only treats bare `42` / `nil` specially.
        assert_eq!(to_source(&json!({"42": 1})), "{:42 1}");
        assert_eq!(to_source(&json!({"nil": 1})), "{:nil 1}");
        assert_eq!(to_source(&json!({"true": 1})), "{:true 1}");
    }

    #[test]
    fn hyphenated_and_slashed_keys_stay_keywords() {
        assert_eq!(to_source(&json!({"foo-bar": 1})), "{:foo-bar 1}");
        assert_eq!(to_source(&json!({"my/key": 1})), "{:my/key 1}");
        assert_eq!(to_source(&json!({"a.b": 1})), "{:a.b 1}");
    }

    // -- pretty -------------------------------------------------------------

    #[test]
    fn pretty_keeps_flat_collections_inline() {
        assert_eq!(to_source_pretty(&json!([1, 2, 3])), "[1 2 3]");
        assert_eq!(to_source_pretty(&json!({"a": 1, "b": 2})), "{:a 1 :b 2}");
        assert_eq!(to_source_pretty(&json!([])), "[]");
        assert_eq!(to_source_pretty(&json!({})), "{}");
    }

    #[test]
    fn pretty_breaks_nested_collections() {
        assert_eq!(
            to_source_pretty(&json!({"a": 1, "b": [1, 2]})),
            "{\n  :a 1\n  :b [1 2]\n}"
        );
    }

    #[test]
    fn pretty_indents_two_spaces_per_level() {
        let out = to_source_pretty(&json!({"outer": {"inner": [1, 2]}}));
        assert_eq!(out, "{\n  :outer {\n    :inner [1 2]\n  }\n}");
    }

    #[test]
    fn pretty_handles_arrays_of_maps() {
        let out = to_source_pretty(&json!([{"a": 1}, {"b": 2}]));
        assert_eq!(out, "[\n  {:a 1}\n  {:b 2}\n]");
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
