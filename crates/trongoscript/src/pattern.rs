//! Destructuring patterns for `let` bindings and `fn` parameter lists.
//!
//! A binding position may hold a pattern instead of a bare symbol:
//!
//! ```clojure
//! (let [[host port]       (split-host-port addr)
//!       {:name n :port p} server-config]
//!   {:label "#{n}@#{host}:#{p}"})
//! ```
//!
//! Patterns are spelled with the literal syntax they mirror — a vector
//! pattern looks like a vector, a map pattern looks like a map — so a
//! pattern is an ordinary [`Expr`] in binding position and the reader
//! learns nothing new.
//!
//! ```text
//! pattern ::= symbol                      ; binds one name
//!           | '[' pattern* ('&' symbol)? ']'
//!           | '{' (keyword pattern)* '}'
//! ```
//!
//! The two collection patterns differ in strictness, deliberately:
//!
//! - A **vector pattern** matches a vector of exactly its length, unless it
//!   carries an `& rest` tail, which makes the match minimum-length and
//!   binds the remainder (possibly empty).
//! - A **map pattern** matches a map containing *at least* its named keys.
//!   Extra keys are ignored. That is the right default for configuration,
//!   where a document gains fields over time and a pattern should not break
//!   when it does.
//!
//! `&` is the rest marker rather than `*`, because `&` is already
//! Trongoscript's rest marker in `fn` parameter lists and a second spelling
//! would buy nothing.

use trongoscript_parser::Expr;

use crate::error::{EvalError, EvalResult};
use crate::value::Value;

/// A compiled binding form.
///
/// Compiled once at `let`/`fn` time rather than re-walked per match, which
/// matters for `fn`: a lambda's parameter patterns are matched on every
/// call, and `map` over a vector of maps is the most common higher-order
/// shape in a config document.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Pattern {
    /// `x` — binds one name.
    Bind(String),
    /// `[a b & rest]` — positional.
    Vector {
        items: Vec<Pattern>,
        rest: Option<String>,
    },
    /// `{:k pat ...}` — by key.
    Map(Vec<(String, Pattern)>),
}

impl Pattern {
    /// Compile a binding form into a [`Pattern`].
    ///
    /// A bare symbol is the today-form and stays one, so every existing
    /// `let` and `fn` keeps parsing and keeps meaning what it meant.
    pub fn compile(e: &Expr) -> EvalResult<Pattern> {
        match e {
            Expr::Symbol(s) => {
                if s == "&" {
                    return Err(EvalError::InvalidForm(
                        "`&` is only valid before the last name in a vector pattern".into(),
                    ));
                }
                Ok(Pattern::Bind(s.clone()))
            }

            Expr::Vector(items) => {
                let mut out = Vec::new();
                let mut rest = None;
                let mut iter = items.iter();
                while let Some(item) = iter.next() {
                    if matches!(item, Expr::Symbol(s) if s == "&") {
                        let Some(name) = iter.next() else {
                            return Err(EvalError::InvalidForm(
                                "expected a name after `&` in a pattern".into(),
                            ));
                        };
                        let Expr::Symbol(n) = name else {
                            return Err(EvalError::InvalidForm(format!(
                                "`& rest` must be a symbol, got {}",
                                name
                            )));
                        };
                        if iter.next().is_some() {
                            return Err(EvalError::InvalidForm(
                                "no pattern allowed after `& rest`".into(),
                            ));
                        }
                        rest = Some(n.clone());
                        break;
                    }
                    out.push(Pattern::compile(item)?);
                }
                Ok(Pattern::Vector { items: out, rest })
            }

            Expr::Map(pairs) => {
                let mut out = Vec::with_capacity(pairs.len());
                for (k, v) in pairs {
                    // Keys are keywords or strings, matching how map keys
                    // are written everywhere else in the language.
                    let key = match k {
                        Expr::Keyword(k) => k.clone(),
                        Expr::String(s) => s.clone(),
                        other => {
                            return Err(EvalError::InvalidForm(format!(
                                "map pattern key must be a keyword or string, got {}",
                                other
                            )))
                        }
                    };
                    out.push((key, Pattern::compile(v)?));
                }
                Ok(Pattern::Map(out))
            }

            other => Err(EvalError::InvalidForm(format!(
                "binding form must be a symbol, vector pattern, or map pattern, got {}",
                other
            ))),
        }
    }

    /// Match `value` against this pattern, pushing `(name, value)` pairs
    /// onto `out`.
    ///
    /// Returns [`EvalError::PatternMismatch`] rather than binding nil for a
    /// pattern that cannot succeed. See the note on that variant.
    pub fn match_value(&self, value: &Value, out: &mut Vec<(String, Value)>) -> EvalResult<()> {
        match self {
            Pattern::Bind(name) => {
                out.push((name.clone(), value.clone()));
                Ok(())
            }

            Pattern::Vector { items, rest } => {
                let Some(arr) = value.as_array() else {
                    return Err(self.mismatch(format!("expected a vector, got {}", type_of(value))));
                };

                if rest.is_some() {
                    if arr.len() < items.len() {
                        return Err(self.mismatch(format!(
                            "expected at least {} element(s), got {}",
                            items.len(),
                            arr.len()
                        )));
                    }
                } else if arr.len() != items.len() {
                    return Err(self.mismatch(format!(
                        "expected exactly {} element(s), got {}",
                        items.len(),
                        arr.len()
                    )));
                }

                for (p, v) in items.iter().zip(arr.iter()) {
                    p.match_value(v, out)?;
                }
                if let Some(name) = rest {
                    out.push((name.clone(), Value::Array(arr[items.len()..].to_vec())));
                }
                Ok(())
            }

            Pattern::Map(pairs) => {
                let Some(obj) = value.as_object() else {
                    return Err(self.mismatch(format!("expected a map, got {}", type_of(value))));
                };
                for (key, p) in pairs {
                    let Some(v) = obj.get(key) else {
                        return Err(self.mismatch(format!("map has no key :{}", key)));
                    };
                    p.match_value(v, out)?;
                }
                Ok(())
            }
        }
    }

    fn mismatch(&self, reason: String) -> EvalError {
        EvalError::PatternMismatch {
            pattern: self.to_string(),
            reason,
        }
    }
}

impl std::fmt::Display for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pattern::Bind(n) => f.write_str(n),
            Pattern::Vector { items, rest } => {
                f.write_str("[")?;
                for (i, p) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{}", p)?;
                }
                if let Some(r) = rest {
                    if !items.is_empty() {
                        f.write_str(" ")?;
                    }
                    write!(f, "& {}", r)?;
                }
                f.write_str("]")
            }
            Pattern::Map(pairs) => {
                f.write_str("{")?;
                for (i, (k, p)) in pairs.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, ":{} {}", k, p)?;
                }
                f.write_str("}")
            }
        }
    }
}

/// The type name used in mismatch messages, matching the stdlib's `*?`
/// predicate vocabulary.
fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "nil",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "vector",
        Value::Object(_) => "map",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use trongoscript_parser::parse_expr;

    fn compile(src: &str) -> Pattern {
        Pattern::compile(&parse_expr(src).expect("parse failed")).expect("compile failed")
    }

    fn match_src(pat: &str, value: serde_json::Value) -> EvalResult<Vec<(String, Value)>> {
        let mut out = Vec::new();
        compile(pat).match_value(&value, &mut out)?;
        Ok(out)
    }

    #[test]
    fn a_bare_symbol_compiles_to_a_simple_bind() {
        assert_eq!(compile("x"), Pattern::Bind("x".into()));
    }

    #[test]
    fn compiles_vector_and_map_patterns() {
        assert_eq!(
            compile("[a b]"),
            Pattern::Vector {
                items: vec![Pattern::Bind("a".into()), Pattern::Bind("b".into())],
                rest: None,
            }
        );
        assert_eq!(
            compile("[a & r]"),
            Pattern::Vector {
                items: vec![Pattern::Bind("a".into())],
                rest: Some("r".into()),
            }
        );
        assert_eq!(
            compile("{:a x}"),
            Pattern::Map(vec![("a".into(), Pattern::Bind("x".into()))])
        );
    }

    /// A string key is accepted alongside a keyword, matching how map keys
    /// are written everywhere else in the language.
    #[test]
    fn map_pattern_accepts_a_string_key() {
        assert_eq!(
            compile(r#"{"a" x}"#),
            Pattern::Map(vec![("a".into(), Pattern::Bind("x".into()))])
        );
    }

    #[test]
    fn rejects_malformed_patterns() {
        assert!(Pattern::compile(&parse_expr("[a &]").unwrap()).is_err());
        assert!(Pattern::compile(&parse_expr("[a & r b]").unwrap()).is_err());
        assert!(Pattern::compile(&parse_expr("[& 1]").unwrap()).is_err());
        assert!(Pattern::compile(&parse_expr("{1 x}").unwrap()).is_err());
        assert!(Pattern::compile(&parse_expr("42").unwrap()).is_err());
        assert!(Pattern::compile(&parse_expr("&").unwrap()).is_err());
    }

    #[test]
    fn matching_binds_in_order() {
        let binds = match_src("[a b]", serde_json::json!([1, 2])).unwrap();
        assert_eq!(binds.len(), 2);
        assert_eq!(binds[0].0, "a");
        assert_eq!(binds[1].0, "b");
    }

    #[test]
    fn rest_binds_the_remainder_as_a_vector() {
        let binds = match_src("[a & r]", serde_json::json!([1, 2, 3])).unwrap();
        assert_eq!(binds[1].0, "r");
        assert_eq!(binds[1].1, serde_json::json!([2, 3]));
        // Possibly empty.
        let binds = match_src("[a & r]", serde_json::json!([1])).unwrap();
        assert_eq!(binds[1].1, serde_json::json!([]));
    }

    /// The mismatch message names what went wrong, since it is the whole
    /// point of raising instead of binding nil.
    #[test]
    fn mismatch_messages_are_specific() {
        let e = match_src("{:a x :b y}", serde_json::json!({"a": 1})).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains(":b"), "should name the missing key: {}", msg);

        let e = match_src("[a b]", serde_json::json!([1])).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains('2'), "should give the expected arity: {}", msg);
        assert!(msg.contains('1'), "should give the actual arity: {}", msg);

        let e = match_src("[a b]", serde_json::json!(5)).unwrap_err();
        assert!(
            e.to_string().contains("number"),
            "should name the type it got: {}",
            e
        );
    }

    /// A compiled pattern survives a serde round trip.
    ///
    /// [`Pattern`] is `Serialize`/`Deserialize` because it sits inside
    /// [`crate::eval::Closure`], which is persisted whenever a partially
    /// evaluated program is suspended and resumed. A parameter pattern that
    /// did not survive that trip would silently change what a resumed
    /// lambda binds, so the derives are exercised rather than assumed.
    #[test]
    fn a_pattern_survives_a_serde_round_trip() {
        for src in [
            "x",
            "[a b]",
            "[a & r]",
            "[& r]",
            "[]",
            "{:a x}",
            "{}",
            "{:a [b c] :d {:e f}}",
            "[{:a x} [y & z]]",
        ] {
            let p = compile(src);
            let json = serde_json::to_string(&p).expect("serialize failed");
            let back: Pattern = serde_json::from_str(&json).expect("deserialize failed");
            assert_eq!(back, p, "{} serialized as {}", src, json);
        }
    }

    /// Matching is driven only by the pattern's own structure, so a
    /// deserialized pattern binds exactly what the original would.
    #[test]
    fn a_deserialized_pattern_matches_identically() {
        let p = compile("[a {:b c} & rest]");
        let back: Pattern = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();

        let value = serde_json::json!([1, {"b": 2}, 3, 4]);
        let (mut lhs, mut rhs) = (Vec::new(), Vec::new());
        p.match_value(&value, &mut lhs).unwrap();
        back.match_value(&value, &mut rhs).unwrap();
        assert_eq!(lhs, rhs);
        assert_eq!(lhs.len(), 3);
    }

    /// The pattern prints back in the syntax it was written in, so the
    /// message shows the reader something it would accept.
    #[test]
    fn display_round_trips_the_written_syntax() {
        for src in [
            "x",
            "[a b]",
            "[a & r]",
            "{:a x}",
            "{:a [b c]}",
            "[{:a x} y]",
            // The degenerate shapes, where a printer is most likely to drop
            // or add a separator: empty collections and a rest-only vector,
            // whose `& r` must not be preceded by a stray space.
            "[]",
            "{}",
            "[& r]",
            "[[] {}]",
            "{:a {} :b []}",
        ] {
            let printed = compile(src).to_string();
            assert_eq!(
                compile(&printed),
                compile(src),
                "{} printed as {}",
                src,
                printed
            );
        }
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
