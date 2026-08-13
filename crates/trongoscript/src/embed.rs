//! Embedding Trongoscript inside YAML / TOML / JSON documents.
//!
//! YAMLScript-style: a host document carries scalars that should be
//! interpreted as Trongoscript and substituted with the evaluated value.
//!
//! Trongoscript recognizes three markers:
//!
//! - In YAML, a node carrying the **`!trongo` tag** (e.g.
//!   `key: !trongo (+ 1 2)`) — the idiomatic form, using YAML's native tag
//!   mechanism rather than a magic string.
//! - A *string scalar* whose contents start with a configurable prefix
//!   (default: `"!trongo "`) is treated as a Trongoscript expression to be
//!   evaluated; the scalar is replaced by the produced value. Useful in
//!   JSON and TOML, which have no tag concept, and as a fallback in YAML
//!   when the scalar is already quoted.
//! - A *map* with a single key matching the prefix (e.g.
//!   `{"!trongo": "<expr>"}`) is replaced the same way. This form is useful
//!   inside TOML and strict JSON where bare strings can be inconvenient.
//!
//! All other scalars / maps / lists are walked recursively but otherwise
//! left intact.

use serde_json::Value;

use crate::error::{EvalError, EvalResult};
use crate::eval::{eval_source, Context};

/// Substitution prefix for plain-string scalars.
pub const DEFAULT_PREFIX: &str = "!trongo ";
/// Map-form key.
pub const DEFAULT_KEY: &str = "!trongo";
/// YAML tag (sans trailing space) recognized in tagged-node form.
pub const DEFAULT_TAG: &str = "!trongo";

/// Walk a `serde_json::Value` tree, evaluating any embedded Trongoscript
/// snippets in place. Recognizes the string-prefix and single-key-map forms.
/// (YAML tags are not visible at this layer; see [`evaluate_embedded_yaml`].)
pub fn evaluate_embedded(root: Value, ctx: &mut Context) -> EvalResult<Value> {
    evaluate_embedded_with(root, ctx, DEFAULT_PREFIX, DEFAULT_KEY)
}

/// Like [`evaluate_embedded`] but with explicit marker tokens.
pub fn evaluate_embedded_with(
    root: Value,
    ctx: &mut Context,
    prefix: &str,
    key: &str,
) -> EvalResult<Value> {
    match root {
        Value::String(s) => {
            if let Some(code) = s.strip_prefix(prefix) {
                eval_source(code, ctx)
            } else {
                Ok(Value::String(s))
            }
        }
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for v in items {
                out.push(evaluate_embedded_with(v, ctx, prefix, key)?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => {
            // Single-key map form: {"!trongo": "<expr>"}.
            if map.len() == 1 {
                if let Some(Value::String(code)) = map.get(key) {
                    return eval_source(code, ctx);
                }
            }
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, evaluate_embedded_with(v, ctx, prefix, key)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other),
    }
}

/// Walk a `serde_yaml::Value` tree, evaluating embedded Trongoscript snippets
/// and producing a `serde_json::Value`. Recognizes:
///
/// - YAML nodes tagged with `!trongo` (the inner scalar is the expression),
/// - the string-prefix form (`"!trongo <expr>"`),
/// - the single-key-map form (`{"!trongo": "<expr>"}`).
pub fn evaluate_embedded_yaml(root: serde_yaml::Value, ctx: &mut Context) -> EvalResult<Value> {
    evaluate_embedded_yaml_with(root, ctx, DEFAULT_TAG, DEFAULT_PREFIX, DEFAULT_KEY)
}

/// Like [`evaluate_embedded_yaml`] but with explicit marker tokens.
pub fn evaluate_embedded_yaml_with(
    root: serde_yaml::Value,
    ctx: &mut Context,
    tag: &str,
    prefix: &str,
    key: &str,
) -> EvalResult<Value> {
    use serde_yaml::Value as Y;
    match root {
        Y::Tagged(t) => {
            // Compare without the leading `!` to be tolerant of how callers
            // write the tag constant: serde_yaml's Tag display form always
            // includes the bang.
            let matches = tag_equals(&t.tag, tag);
            if matches {
                let code = expect_string_scalar(&t.value)?;
                eval_source(&code, ctx)
            } else {
                // Unknown tag: drop it and process the inner value, since
                // serde_json has no tag concept anyway.
                evaluate_embedded_yaml_with(t.value, ctx, tag, prefix, key)
            }
        }
        Y::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for v in items {
                out.push(evaluate_embedded_yaml_with(v, ctx, tag, prefix, key)?);
            }
            Ok(Value::Array(out))
        }
        Y::Mapping(map) => {
            // Single-key map form: { "!trongo": "<expr>" }.
            if map.len() == 1 {
                let (k, v) = map.iter().next().unwrap();
                if let (Y::String(ks), Y::String(code)) = (k, v) {
                    if ks == key {
                        return eval_source(code, ctx);
                    }
                }
            }
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                let key_s = yaml_key_to_string(&k)?;
                let val = evaluate_embedded_yaml_with(v, ctx, tag, prefix, key)?;
                out.insert(key_s, val);
            }
            Ok(Value::Object(out))
        }
        Y::String(s) => {
            if let Some(code) = s.strip_prefix(prefix) {
                eval_source(code, ctx)
            } else {
                Ok(Value::String(s))
            }
        }
        Y::Bool(b) => Ok(Value::Bool(b)),
        Y::Number(n) => Ok(yaml_number_to_json(n)),
        Y::Null => Ok(Value::Null),
    }
}

fn tag_equals(tag: &serde_yaml::value::Tag, expected: &str) -> bool {
    // serde_yaml::Tag's Display form always includes a leading `!`.
    let actual = tag.to_string();
    let expected_norm = expected.strip_prefix('!').unwrap_or(expected);
    let actual_norm = actual.strip_prefix('!').unwrap_or(&actual);
    actual_norm == expected_norm
}

fn expect_string_scalar(v: &serde_yaml::Value) -> EvalResult<String> {
    match v {
        serde_yaml::Value::String(s) => Ok(s.clone()),
        other => Err(EvalError::InvalidForm(format!(
            "!trongo tag requires a string scalar, got {:?}",
            other
        ))),
    }
}

fn yaml_key_to_string(k: &serde_yaml::Value) -> EvalResult<String> {
    match k {
        serde_yaml::Value::String(s) => Ok(s.clone()),
        serde_yaml::Value::Bool(b) => Ok(b.to_string()),
        serde_yaml::Value::Number(n) => Ok(n.to_string()),
        other => Err(EvalError::BadMapKey(format!("{:?}", other))),
    }
}

fn yaml_number_to_json(n: serde_yaml::Number) -> Value {
    if let Some(i) = n.as_i64() {
        Value::Number(i.into())
    } else if let Some(u) = n.as_u64() {
        Value::Number(u.into())
    } else if let Some(f) = n.as_f64() {
        serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::EvalConfig;
    use pretty_assertions::assert_eq;

    #[test]
    fn replaces_string_scalar() {
        let root = serde_json::json!({
            "name": "!trongo (upcase \"alice\")",
            "static": "untouched"
        });
        let mut ctx = Context::new(EvalConfig::default());
        let v = evaluate_embedded(root, &mut ctx).unwrap();
        assert_eq!(v["name"], Value::String("ALICE".into()));
        assert_eq!(v["static"], Value::String("untouched".into()));
    }

    #[test]
    fn replaces_map_form() {
        let root = serde_json::json!({
            "n": {"!trongo": "(+ 1 2 3)"}
        });
        let mut ctx = Context::new(EvalConfig::default());
        let v = evaluate_embedded(root, &mut ctx).unwrap();
        assert_eq!(v["n"], Value::Number(6.into()));
    }

    #[test]
    fn walks_lists() {
        let root = serde_json::json!(["!trongo (+ 1 1)", "literal", "!trongo (str \"hi-\" 3)"]);
        let mut ctx = Context::new(EvalConfig::default());
        let v = evaluate_embedded(root, &mut ctx).unwrap();
        assert_eq!(v[0], Value::Number(2.into()));
        assert_eq!(v[1], Value::String("literal".into()));
        assert_eq!(v[2], Value::String("hi-3".into()));
    }

    #[test]
    fn yaml_tag_scalar() {
        let raw = r#"
title: !trongo (upcase "production environment")
plain: hello
"#;
        let y: serde_yaml::Value = serde_yaml::from_str(raw).unwrap();
        let mut ctx = Context::new(EvalConfig::default());
        let v = evaluate_embedded_yaml(y, &mut ctx).unwrap();
        assert_eq!(v["title"], Value::String("PRODUCTION ENVIRONMENT".into()));
        assert_eq!(v["plain"], Value::String("hello".into()));
    }

    #[test]
    fn yaml_tag_block_scalar() {
        let raw = r#"
total: !trongo |
  (+ 1 2 3 4)
"#;
        let y: serde_yaml::Value = serde_yaml::from_str(raw).unwrap();
        let mut ctx = Context::new(EvalConfig::default());
        let v = evaluate_embedded_yaml(y, &mut ctx).unwrap();
        assert_eq!(v["total"], Value::Number(10.into()));
    }

    #[test]
    fn yaml_tag_inside_sequence() {
        let raw = r#"
xs:
  - !trongo (range 1 4)
  - literal
"#;
        let y: serde_yaml::Value = serde_yaml::from_str(raw).unwrap();
        let mut ctx = Context::new(EvalConfig::default());
        let v = evaluate_embedded_yaml(y, &mut ctx).unwrap();
        assert_eq!(v["xs"][0], serde_json::json!([1, 2, 3]));
        assert_eq!(v["xs"][1], Value::String("literal".into()));
    }

    #[test]
    fn yaml_prefix_form_still_works() {
        // Compat: quoted-string form keeps working in the YAML path.
        let raw = r#"
v: "!trongo (+ 2 3)"
"#;
        let y: serde_yaml::Value = serde_yaml::from_str(raw).unwrap();
        let mut ctx = Context::new(EvalConfig::default());
        let v = evaluate_embedded_yaml(y, &mut ctx).unwrap();
        assert_eq!(v["v"], Value::Number(5.into()));
    }

    #[test]
    fn yaml_unknown_tag_passes_through() {
        let raw = r#"
v: !other "hello"
"#;
        let y: serde_yaml::Value = serde_yaml::from_str(raw).unwrap();
        let mut ctx = Context::new(EvalConfig::default());
        let v = evaluate_embedded_yaml(y, &mut ctx).unwrap();
        assert_eq!(v["v"], Value::String("hello".into()));
    }

    #[test]
    fn yaml_tag_requires_string() {
        let raw = r#"
v: !trongo
  not: a-string
"#;
        let y: serde_yaml::Value = serde_yaml::from_str(raw).unwrap();
        let mut ctx = Context::new(EvalConfig::default());
        let err = evaluate_embedded_yaml(y, &mut ctx).unwrap_err();
        assert!(matches!(err, EvalError::InvalidForm(_)));
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
