//! `serde` module — serialize and parse JSON, YAML, and TOML.
//!
//! Activated by `(use serde)`. Pure string↔value conversion, no I/O:
//! reading a file is still `read-file`'s job.
//!
//! ```text
//! (to-json   {:a 1})        ; => "{\"a\":1}"
//! (to-json   {:a 1} true)   ; => pretty-printed, 2-space indent
//! (to-yaml   [1 2 3])       ; => "- 1\n- 2\n- 3\n"
//! (to-toml   {:a 1})        ; => "a = 1\n"
//! (parse-json "{\"a\":1}")  ; => {:a 1}
//! (to-trongo (parse-json "{\"a\":1}"))  ; => "{:a 1}"
//! ```
//!
//! `to-trongo` goes the other way from the parsers: it renders a value as
//! Trongoscript source text, so a JSON, YAML, or TOML document can be
//! converted into the equivalent Trongoscript literal. See
//! [`crate::trongo_fmt`] for how keys, numbers, and layout are handled.
//!
//! The three formats do not describe the same set of values, so the edges
//! are worth knowing:
//!
//! * **TOML** has no null and no top-level scalar or array. `to-toml`
//!   requires a map at the root and rejects maps containing nulls rather
//!   than silently dropping those keys.
//! * **YAML** allows non-string map keys and `!tag`s. `parse-yaml` maps
//!   scalar keys (numbers, bools, null) onto their string spelling; keys
//!   that are themselves collections have no representation here and are
//!   an error.
//! * Both parsers reject NaN and infinity, which JSON cannot express.

use crate::{expect_string, inventory, type_name, Builtin, BuiltinError, BuiltinResult, Value};

const MODULE: &str = "serde";

fn failed(function: &str, message: impl Into<String>) -> BuiltinError {
    BuiltinError::Failed {
        function: function.into(),
        message: message.into(),
    }
}

/// Read an optional trailing "pretty" flag. Any non-nil, non-false value
/// counts as true, matching the language's general truthiness.
fn pretty_flag(args: &[Value]) -> bool {
    match args.get(1) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => true,
    }
}

// ---------------------------------------------------------------------------
// Emitting
// ---------------------------------------------------------------------------

fn to_json(args: &[Value]) -> BuiltinResult {
    let out = if pretty_flag(args) {
        serde_json::to_string_pretty(&args[0])
    } else {
        serde_json::to_string(&args[0])
    };
    out.map(Value::String)
        .map_err(|e| failed("to-json", e.to_string()))
}

/// Emit Trongoscript source for a value — the inverse of reading one.
/// Always succeeds: every value in the model has a literal spelling.
fn to_trongo(args: &[Value]) -> BuiltinResult {
    let out = if pretty_flag(args) {
        crate::trongo_fmt::to_source_pretty(&args[0])
    } else {
        crate::trongo_fmt::to_source(&args[0])
    };
    Ok(Value::String(out))
}

fn to_yaml(args: &[Value]) -> BuiltinResult {
    serde_yaml::to_string(&args[0])
        .map(Value::String)
        .map_err(|e| failed("to-yaml", e.to_string()))
}

fn to_toml(args: &[Value]) -> BuiltinResult {
    // TOML documents are tables. A scalar or list root has nowhere to go.
    if !args[0].is_object() {
        return Err(BuiltinError::TypeError {
            function: "to-toml".into(),
            expected: "map (TOML root must be a table)".into(),
            got: type_name(&args[0]).into(),
        });
    }
    let table = toml_from_json(&args[0], "")?;
    let out = if pretty_flag(args) {
        toml::to_string_pretty(&table)
    } else {
        toml::to_string(&table)
    };
    out.map(Value::String)
        .map_err(|e| failed("to-toml", e.to_string()))
}

/// Convert a JSON value to a TOML value. `path` names the position of `v`
/// inside the document so an unrepresentable leaf can say where it is.
fn toml_from_json(v: &Value, path: &str) -> Result<toml::Value, BuiltinError> {
    let here = || {
        if path.is_empty() {
            "root".to_string()
        } else {
            format!("`{path}`")
        }
    };
    match v {
        // TOML has no null. Dropping the key would silently lose data and
        // writing "" would invent a value, so refuse.
        Value::Null => Err(failed(
            "to-toml",
            format!("TOML has no null; found one at {}", here()),
        )),
        Value::Bool(b) => Ok(toml::Value::Boolean(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                // u64 above i64::MAX — TOML integers are signed 64-bit.
                Err(failed(
                    "to-toml",
                    format!("number at {} does not fit a TOML integer", here()),
                ))
            }
        }
        Value::String(s) => Ok(toml::Value::String(s.clone())),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                out.push(toml_from_json(item, &join_path(path, &i.to_string()))?);
            }
            Ok(toml::Value::Array(out))
        }
        Value::Object(map) => {
            let mut t = toml::map::Map::new();
            for (k, val) in map {
                t.insert(k.clone(), toml_from_json(val, &join_path(path, k))?);
            }
            Ok(toml::Value::Table(t))
        }
    }
}

fn join_path(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_string()
    } else {
        format!("{prefix}.{segment}")
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn parse_json(args: &[Value]) -> BuiltinResult {
    let s = expect_string("parse-json", &args[0])?;
    serde_json::from_str(s).map_err(|e| failed("parse-json", e.to_string()))
}

fn parse_yaml(args: &[Value]) -> BuiltinResult {
    let s = expect_string("parse-yaml", &args[0])?;
    let y: serde_yaml::Value =
        serde_yaml::from_str(s).map_err(|e| failed("parse-yaml", e.to_string()))?;
    json_from_yaml(&y, "")
}

fn parse_toml(args: &[Value]) -> BuiltinResult {
    let s = expect_string("parse-toml", &args[0])?;
    let t: toml::Value = toml::from_str(s).map_err(|e| failed("parse-toml", e.to_string()))?;
    // toml::Value has no null and no non-string keys, so serde's own
    // conversion is total here.
    serde_json::to_value(t).map_err(|e| failed("parse-toml", e.to_string()))
}

/// Convert a parsed YAML value into the JSON-shaped value model. YAML is
/// the wider language of the two, so this is where the lossy cases live.
fn json_from_yaml(y: &serde_yaml::Value, path: &str) -> BuiltinResult {
    let here = || {
        if path.is_empty() {
            "root".to_string()
        } else {
            format!("`{path}`")
        }
    };
    match y {
        serde_yaml::Value::Null => Ok(Value::Null),
        serde_yaml::Value::Bool(b) => Ok(Value::Bool(*b)),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Number(i.into()))
            } else if let Some(u) = n.as_u64() {
                Ok(Value::Number(u.into()))
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .ok_or_else(|| {
                        failed(
                            "parse-yaml",
                            format!("non-finite number at {} has no JSON form", here()),
                        )
                    })
            }
        }
        serde_yaml::Value::String(s) => Ok(Value::String(s.clone())),
        serde_yaml::Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                out.push(json_from_yaml(item, &join_path(path, &i.to_string()))?);
            }
            Ok(Value::Array(out))
        }
        serde_yaml::Value::Mapping(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                let key = yaml_key(k, path)?;
                let child = json_from_yaml(v, &join_path(path, &key))?;
                out.insert(key, child);
            }
            Ok(Value::Object(out))
        }
        // `!Tagged` values carry a `!tag` this value model can't hold. Keep
        // the payload rather than failing — the tag is metadata, and
        // dropping it is the least surprising outcome for a config file.
        serde_yaml::Value::Tagged(t) => json_from_yaml(&t.value, path),
    }
}

/// Render a YAML mapping key as a string. Scalar keys map onto their
/// natural spelling; collection keys have no representation.
fn yaml_key(k: &serde_yaml::Value, path: &str) -> Result<String, BuiltinError> {
    match k {
        serde_yaml::Value::String(s) => Ok(s.clone()),
        serde_yaml::Value::Bool(b) => Ok(b.to_string()),
        serde_yaml::Value::Number(n) => Ok(n.to_string()),
        serde_yaml::Value::Null => Ok("null".to_string()),
        serde_yaml::Value::Tagged(t) => yaml_key(&t.value, path),
        other => Err(failed(
            "parse-yaml",
            format!(
                "YAML mapping key of type {} under {} cannot become a map key",
                match other {
                    serde_yaml::Value::Sequence(_) => "sequence",
                    _ => "mapping",
                },
                if path.is_empty() {
                    "root".to_string()
                } else {
                    format!("`{path}`")
                }
            ),
        )),
    }
}

inventory::submit! { Builtin::in_module(MODULE, "to-trongo",  1, 2, to_trongo) }
inventory::submit! { Builtin::in_module(MODULE, "to-json",    1, 2, to_json) }
inventory::submit! { Builtin::in_module(MODULE, "to-yaml",    1, 1, to_yaml) }
inventory::submit! { Builtin::in_module(MODULE, "to-toml",    1, 2, to_toml) }
inventory::submit! { Builtin::in_module(MODULE, "parse-json", 1, 1, parse_json) }
inventory::submit! { Builtin::in_module(MODULE, "parse-yaml", 1, 1, parse_yaml) }
inventory::submit! { Builtin::in_module(MODULE, "parse-toml", 1, 1, parse_toml) }

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn s(v: Value) -> String {
        v.as_str().expect("expected a string result").to_string()
    }

    // -- to-json ------------------------------------------------------------

    #[test]
    fn to_json_compact_by_default() {
        let out = s(to_json(&[json!({"a": 1, "b": [1, 2]})]).unwrap());
        assert_eq!(out, r#"{"a":1,"b":[1,2]}"#);
    }

    #[test]
    fn to_json_pretty_with_flag() {
        let out = s(to_json(&[json!({"a": 1}), Value::Bool(true)]).unwrap());
        assert_eq!(out, "{\n  \"a\": 1\n}");
    }

    #[test]
    fn to_json_false_flag_stays_compact() {
        let out = s(to_json(&[json!({"a": 1}), Value::Bool(false)]).unwrap());
        assert_eq!(out, r#"{"a":1}"#);
    }

    #[test]
    fn to_json_handles_scalars_and_null() {
        assert_eq!(s(to_json(&[json!(null)]).unwrap()), "null");
        assert_eq!(s(to_json(&[json!("hi")]).unwrap()), r#""hi""#);
        assert_eq!(s(to_json(&[json!(1.5)]).unwrap()), "1.5");
    }

    // -- to-trongo ----------------------------------------------------------

    #[test]
    fn to_trongo_compact_by_default() {
        let out = s(to_trongo(&[json!({"a": 1, "b": [1, 2]})]).unwrap());
        assert_eq!(out, "{:a 1 :b [1 2]}");
    }

    #[test]
    fn to_trongo_pretty_with_flag() {
        let out = s(to_trongo(&[json!({"a": 1, "b": [1, 2]}), json!(true)]).unwrap());
        assert_eq!(out, "{\n  :a 1\n  :b [1 2]\n}");
    }

    #[test]
    fn to_trongo_false_flag_stays_compact() {
        let out = s(to_trongo(&[json!({"a": [1, [2]]}), json!(false)]).unwrap());
        assert_eq!(out, "{:a [1 [2]]}");
    }

    #[test]
    fn to_trongo_handles_scalars_and_null() {
        assert_eq!(s(to_trongo(&[json!(null)]).unwrap()), "nil");
        assert_eq!(s(to_trongo(&[json!("hi")]).unwrap()), r#""hi""#);
        assert_eq!(s(to_trongo(&[json!(1.5)]).unwrap()), "1.5");
        assert_eq!(s(to_trongo(&[json!(42)]).unwrap()), "42");
    }

    #[test]
    fn to_trongo_accepts_any_root_unlike_toml() {
        // No root-shape restriction: a bare scalar or list is a valid form.
        assert_eq!(s(to_trongo(&[json!([1, 2])]).unwrap()), "[1 2]");
        assert_eq!(s(to_trongo(&[json!(true)]).unwrap()), "true");
    }

    #[test]
    fn parsed_json_round_trips_into_trongo() {
        let v = parse_json(&[json!(r#"{"full name":"a b","tags":["x"],"n":null}"#)]).unwrap();
        let out = s(to_trongo(&[v]).unwrap());
        // Key needs quoting, value keeps its space, null becomes nil.
        assert_eq!(out, r#"{"full name" "a b" :n nil :tags ["x"]}"#);
    }

    // -- to-yaml ------------------------------------------------------------

    #[test]
    fn to_yaml_emits_block_style() {
        let out = s(to_yaml(&[json!({"a": 1, "b": "two"})]).unwrap());
        assert_eq!(out, "a: 1\nb: two\n");
    }

    #[test]
    fn to_yaml_sequence() {
        assert_eq!(s(to_yaml(&[json!([1, 2, 3])]).unwrap()), "- 1\n- 2\n- 3\n");
    }

    // -- to-toml ------------------------------------------------------------

    #[test]
    fn to_toml_basic_table() {
        let out = s(to_toml(&[json!({"name": "x", "count": 2})]).unwrap());
        // Key order follows the map's own order.
        assert!(out.contains("name = \"x\""), "got: {out}");
        assert!(out.contains("count = 2"), "got: {out}");
    }

    #[test]
    fn to_toml_nested_table() {
        let out = s(to_toml(&[json!({"pkg": {"name": "x"}})]).unwrap());
        assert!(out.contains("[pkg]"), "got: {out}");
        assert!(out.contains("name = \"x\""), "got: {out}");
    }

    #[test]
    fn to_toml_rejects_non_map_root() {
        let err = to_toml(&[json!([1, 2])]).unwrap_err();
        assert!(
            matches!(err, BuiltinError::TypeError { ref got, .. } if got == "list"),
            "got: {err:?}"
        );
    }

    #[test]
    fn to_toml_rejects_null_and_names_the_path() {
        let err = to_toml(&[json!({"pkg": {"name": null}})]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no null"), "got: {msg}");
        assert!(msg.contains("pkg.name"), "path missing from: {msg}");
    }

    #[test]
    fn to_toml_reports_path_inside_arrays() {
        let err = to_toml(&[json!({"xs": [1, null]})]).unwrap_err();
        assert!(err.to_string().contains("xs.1"), "got: {err}");
    }

    // -- parse-json ---------------------------------------------------------

    #[test]
    fn parse_json_round_trips() {
        let v = parse_json(&[Value::String(r#"{"a":[1,2],"b":null}"#.into())]).unwrap();
        assert_eq!(v, json!({"a": [1, 2], "b": null}));
    }

    #[test]
    fn parse_json_reports_syntax_errors() {
        let err = parse_json(&[Value::String("{".into())]).unwrap_err();
        assert!(matches!(err, BuiltinError::Failed { .. }), "got: {err:?}");
    }

    #[test]
    fn parse_json_requires_a_string() {
        let err = parse_json(&[json!(42)]).unwrap_err();
        assert!(
            matches!(err, BuiltinError::TypeError { .. }),
            "got: {err:?}"
        );
    }

    // -- parse-yaml ---------------------------------------------------------

    #[test]
    fn parse_yaml_basic_mapping() {
        let v = parse_yaml(&[Value::String("a: 1\nb:\n  - x\n  - y\n".into())]).unwrap();
        assert_eq!(v, json!({"a": 1, "b": ["x", "y"]}));
    }

    #[test]
    fn parse_yaml_null_and_bools() {
        let v = parse_yaml(&[Value::String("a: ~\nb: true\n".into())]).unwrap();
        assert_eq!(v, json!({"a": null, "b": true}));
    }

    #[test]
    fn parse_yaml_stringifies_scalar_keys() {
        let v = parse_yaml(&[Value::String("1: one\ntrue: yes-key\n".into())]).unwrap();
        assert_eq!(v, json!({"1": "one", "true": "yes-key"}));
    }

    #[test]
    fn parse_yaml_rejects_collection_keys() {
        let err = parse_yaml(&[Value::String("? [1, 2]\n: value\n".into())]).unwrap_err();
        assert!(
            err.to_string().contains("cannot become a map key"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_yaml_keeps_payload_of_tagged_values() {
        let v = parse_yaml(&[Value::String("a: !custom 5\n".into())]).unwrap();
        assert_eq!(v, json!({"a": 5}));
    }

    #[test]
    fn parse_yaml_rejects_non_finite_numbers() {
        let err = parse_yaml(&[Value::String("a: .inf\n".into())]).unwrap_err();
        assert!(err.to_string().contains("non-finite"), "got: {err}");
    }

    // -- parse-toml ---------------------------------------------------------

    #[test]
    fn parse_toml_basic() {
        let v = parse_toml(&[Value::String("name = \"x\"\n[pkg]\nver = 2\n".into())]).unwrap();
        assert_eq!(v, json!({"name": "x", "pkg": {"ver": 2}}));
    }

    #[test]
    fn parse_toml_reports_syntax_errors() {
        let err = parse_toml(&[Value::String("nope = ".into())]).unwrap_err();
        assert!(matches!(err, BuiltinError::Failed { .. }), "got: {err:?}");
    }

    // -- round trips --------------------------------------------------------

    #[test]
    fn json_round_trip_preserves_structure() {
        let original = json!({"a": 1, "b": [true, null, "x"], "c": {"d": 1.5}});
        let text = s(to_json(std::slice::from_ref(&original)).unwrap());
        assert_eq!(parse_json(&[Value::String(text)]).unwrap(), original);
    }

    #[test]
    fn yaml_round_trip_preserves_structure() {
        let original = json!({"a": 1, "b": [true, null, "x"], "c": {"d": 1.5}});
        let text = s(to_yaml(std::slice::from_ref(&original)).unwrap());
        assert_eq!(parse_yaml(&[Value::String(text)]).unwrap(), original);
    }

    #[test]
    fn toml_round_trip_preserves_structure() {
        // No nulls: TOML can't carry them.
        let original = json!({"a": 1, "c": {"d": 1.5, "e": ["x", "y"]}});
        let text = s(to_toml(std::slice::from_ref(&original)).unwrap());
        assert_eq!(parse_toml(&[Value::String(text)]).unwrap(), original);
    }

    // -- registration -------------------------------------------------------

    #[test]
    fn builtins_are_module_scoped() {
        for name in [
            "to-json",
            "to-yaml",
            "to-toml",
            "parse-json",
            "parse-yaml",
            "parse-toml",
        ] {
            let b = crate::find_builtin(name).unwrap_or_else(|| panic!("{name} not registered"));
            assert_eq!(b.module, Some(MODULE), "{name} should be in `{MODULE}`");
            assert!(
                crate::find_builtin_for(name, |m| m == MODULE).is_some(),
                "{name} should resolve when `serde` is enabled"
            );
            assert!(
                crate::find_builtin_for(name, |_| false).is_none(),
                "{name} should not resolve without `(use serde)`"
            );
        }
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
