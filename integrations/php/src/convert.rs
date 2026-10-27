//! PHP values to and from Trongoscript values.
//!
//! Both directions walk with an explicit stack rather than recursing.
//! PHP's own array destructor recurses, which is why values crossing the
//! boundary are capped at [`MAX_NESTING`]; the conversions should not add
//! a second depth limit of their own.

use ext_php_rs::boxed::ZBox;
use ext_php_rs::convert::IntoZval;
use ext_php_rs::exception::PhpException;
use ext_php_rs::types::{ZendHashTable, ZendObject, Zval};
use ext_php_rs::zend::{ce, ClassEntry};
use serde_json::{Map, Number, Value as Data};
use trongoscript_host::MAX_NESTING;

/// An exception to throw, with its message still readable from Rust.
#[derive(Debug)]
pub struct Fail {
    pub class: &'static ClassEntry,
    pub message: String,
}

impl From<Fail> for PhpException {
    fn from(f: Fail) -> Self {
        PhpException::new(f.message, 0, f.class)
    }
}

pub fn type_error(message: impl Into<String>) -> Fail {
    Fail {
        class: ce::type_error(),
        message: message.into(),
    }
}

pub fn value_error(message: impl Into<String>) -> Fail {
    Fail {
        class: ce::value_error(),
        message: message.into(),
    }
}

/// A PHP string as UTF-8. PHP strings are bytes and Trongoscript strings
/// are text, so bytes that are not UTF-8 are refused rather than mangled.
pub fn utf8(what: &str, z: &Zval) -> Result<String, Fail> {
    let s = z
        .zend_str()
        .ok_or_else(|| type_error(format!("{what} must be a string, got {}", type_name(z))))?;
    std::str::from_utf8(s.as_bytes())
        .map(str::to_owned)
        .map_err(|_| value_error(format!("{what} is not valid UTF-8")))
}

pub fn type_name(z: &Zval) -> String {
    let z = z.dereference();
    if let Some(obj) = z.object() {
        return obj.get_class_name().unwrap_or_else(|_| "object".into());
    }
    let name = if z.is_null() {
        "null"
    } else if z.is_bool() {
        "bool"
    } else if z.is_long() {
        "int"
    } else if z.is_double() {
        "float"
    } else if z.is_string() {
        "string"
    } else if z.is_array() {
        "array"
    } else if z.is_resource() {
        "resource"
    } else {
        "unknown"
    };
    name.into()
}

enum InFrame {
    List(Vec<Data>, std::vec::IntoIter<Zval>),
    Map(
        Map<String, Data>,
        std::vec::IntoIter<(String, Zval)>,
        Option<String>,
    ),
}

/// Convert a PHP value to a Trongoscript value.
///
/// Accepts null, bool, int, finite float, UTF-8 strings, arrays, `stdClass`
/// objects, and `JsonSerializable` objects (through `jsonSerialize()`).
/// An array whose keys are exactly 0, 1, 2, ... becomes a list, as with
/// `json_encode`; any other array becomes a map with string keys.
pub fn from_php(root: &Zval) -> Result<Data, Fail> {
    let mut stack = match open(root, 1)? {
        Ok(v) => return Ok(v),
        Err(frame) => vec![frame],
    };
    loop {
        let depth = stack.len() + 1;
        let child = match stack.last_mut().expect("stack is non-empty") {
            InFrame::List(_, rest) => rest.next(),
            InFrame::Map(_, rest, key) => rest.next().map(|(k, v)| {
                *key = Some(k);
                v
            }),
        };
        let done = match child {
            Some(z) => match open(&z, depth)? {
                Ok(v) => v,
                Err(frame) => {
                    stack.push(frame);
                    continue;
                }
            },
            None => match stack.pop().expect("stack is non-empty") {
                InFrame::List(items, _) => Data::Array(items),
                InFrame::Map(map, _, _) => Data::Object(map),
            },
        };
        match stack.last_mut() {
            None => return Ok(done),
            Some(InFrame::List(items, _)) => items.push(done),
            Some(InFrame::Map(map, _, key)) => {
                map.insert(key.take().expect("map child has a key"), done);
            }
        }
    }
}

/// A scalar converts directly; a container opens a frame. Children are
/// copied out (refcounted, so cheaply) before any of them is visited, so
/// nothing borrows the PHP value while `jsonSerialize()` runs.
fn open(z: &Zval, depth: usize) -> Result<Result<Data, InFrame>, Fail> {
    let mut current = z.dereference().shallow_clone();
    let mut depth = depth;
    loop {
        let z = current.dereference();
        if let Some(ht) = z.array() {
            check_depth(depth)?;
            let raw = raw_entries(ht);
            let is_list = raw
                .iter()
                .enumerate()
                .all(|(i, (k, _))| k.long() == i64::try_from(i).ok());
            let frame = if is_list {
                let values: Vec<Zval> = raw.into_iter().map(|(_, v)| v).collect();
                InFrame::List(Vec::with_capacity(values.len()), values.into_iter())
            } else {
                InFrame::Map(Map::new(), keyed(raw)?.into_iter(), None)
            };
            return Ok(Err(frame));
        }
        let Some(obj) = z.object() else {
            return scalar(z).map(Ok);
        };
        check_depth(depth)?;
        if std::ptr::eq(obj.get_class_entry(), ce::stdclass()) {
            let props = obj
                .get_properties()
                .map_err(|e| type_error(format!("cannot read stdClass properties: {e}")))?;
            let entries = keyed(raw_entries(props))?;
            return Ok(Err(InFrame::Map(Map::new(), entries.into_iter(), None)));
        }
        if !implements_json_serializable(obj) {
            return Err(type_error(format!(
                "cannot convert {} to a Trongoscript value",
                type_name(z)
            )));
        }
        // Each unwrapping counts as a level, so a `jsonSerialize()` that
        // returns `$this` ends at the nesting limit instead of looping.
        let inner = obj
            .try_call_method("jsonSerialize", vec![])
            .map_err(|e| type_error(format!("{}::jsonSerialize() failed: {e}", type_name(z))))?;
        current = inner;
        depth += 1;
    }
}

/// An array's entries with each key as a raw `Zval`.
///
/// Not `ZendHashTable::iter`: it decodes string keys as UTF-8 and panics on
/// a key that is not, and PHP keys are bytes. So is `has_sequential_keys`,
/// which uses it.
pub fn raw_entries(ht: &ZendHashTable) -> Vec<(Zval, Zval)> {
    let mut it = ht.iter();
    let mut out = Vec::with_capacity(ht.len());
    while let Some((k, v)) = it.next_zval() {
        out.push((k, v.shallow_clone()));
    }
    out
}

/// A raw array key as a Trongoscript map key: integers in decimal, strings
/// as UTF-8 or not at all.
pub fn key_string(k: &Zval) -> Result<String, Fail> {
    match k.long() {
        Some(i) => Ok(i.to_string()),
        None => utf8("array key", k),
    }
}

fn keyed(raw: Vec<(Zval, Zval)>) -> Result<Vec<(String, Zval)>, Fail> {
    raw.into_iter()
        .map(|(k, v)| Ok((key_string(&k)?, v)))
        .collect()
}

fn check_depth(depth: usize) -> Result<(), Fail> {
    if depth > MAX_NESTING {
        Err(value_error(format!(
            "value nests deeper than {MAX_NESTING} levels (is it self-referential?)"
        )))
    } else {
        Ok(())
    }
}

fn implements_json_serializable(obj: &ZendObject) -> bool {
    ClassEntry::try_find("JsonSerializable")
        .is_some_and(|iface| obj.get_class_entry().instance_of(iface))
}

fn scalar(z: &Zval) -> Result<Data, Fail> {
    if z.is_null() {
        return Ok(Data::Null);
    }
    if let Some(b) = z.bool() {
        return Ok(Data::Bool(b));
    }
    if let Some(n) = z.long() {
        return Ok(Data::from(n));
    }
    if let Some(f) = z.double() {
        return Number::from_f64(f)
            .map(Data::Number)
            .ok_or_else(|| value_error(format!("{f} has no Trongoscript representation")));
    }
    if z.is_string() {
        return utf8("string", z).map(Data::String);
    }
    Err(type_error(format!(
        "cannot convert {} to a Trongoscript value",
        type_name(z)
    )))
}

enum OutFrame {
    List(ZBox<ZendHashTable>, std::vec::IntoIter<Data>),
    Array(
        ZBox<ZendHashTable>,
        serde_json::map::IntoIter,
        Option<String>,
    ),
    Object(ZBox<ZendObject>, serde_json::map::IntoIter, Option<String>),
}

/// Convert a Trongoscript value to PHP. Maps become associative arrays, or
/// `stdClass` objects with `objects`, which keeps `{}` distinct from `[]`
/// and `{"0": x}` distinct from `[x]`.
///
/// Integers past `PHP_INT_MAX` become floats, as with `json_decode`.
pub fn to_php(v: Data, objects: bool) -> Result<Zval, Fail> {
    let open = |v: Data| -> Result<Result<Zval, OutFrame>, Fail> {
        Ok(match v {
            Data::Array(items) => Err(OutFrame::List(
                ZendHashTable::with_capacity(u32::try_from(items.len()).unwrap_or(u32::MAX)),
                items.into_iter(),
            )),
            Data::Object(map) if objects => Err(OutFrame::Object(
                ZendObject::new_stdclass(),
                map.into_iter(),
                None,
            )),
            Data::Object(map) => Err(OutFrame::Array(
                ZendHashTable::with_capacity(u32::try_from(map.len()).unwrap_or(u32::MAX)),
                map.into_iter(),
                None,
            )),
            scalar => Ok(scalar_to_php(scalar)?),
        })
    };

    let mut stack = match open(v)? {
        Ok(z) => return Ok(z),
        Err(frame) => vec![frame],
    };
    loop {
        let child = match stack.last_mut().expect("stack is non-empty") {
            OutFrame::List(_, rest) => rest.next(),
            OutFrame::Array(_, rest, key) | OutFrame::Object(_, rest, key) => {
                rest.next().map(|(k, v)| {
                    *key = Some(k);
                    v
                })
            }
        };
        let done = match child {
            Some(v) => match open(v)? {
                Ok(z) => z,
                Err(frame) => {
                    stack.push(frame);
                    continue;
                }
            },
            None => match stack.pop().expect("stack is non-empty") {
                OutFrame::List(ht, _) | OutFrame::Array(ht, _, _) => {
                    let mut z = Zval::new();
                    z.set_hashtable(ht);
                    z
                }
                OutFrame::Object(obj, _, _) => obj
                    .into_zval(false)
                    .map_err(|e| value_error(format!("cannot build object: {e}")))?,
            },
        };
        let failed = |e: ext_php_rs::error::Error| value_error(format!("cannot build array: {e}"));
        match stack.last_mut() {
            None => return Ok(done),
            Some(OutFrame::List(ht, _)) => ht.push(done).map_err(failed)?,
            Some(OutFrame::Array(ht, _, key)) => {
                let key = key.take().expect("map child has a key");
                match numeric_key(&key) {
                    Some(i) => ht.insert_at_index(i, done).map_err(failed)?,
                    None => ht.insert(key.as_str(), done).map_err(failed)?,
                }
            }
            Some(OutFrame::Object(obj, _, key)) => {
                let key = key.take().expect("map child has a key");
                obj.set_property(&key, done).map_err(|e| {
                    value_error(format!("cannot use {key:?} as a property name: {e}"))
                })?;
            }
        }
    }
}

fn scalar_to_php(v: Data) -> Result<Zval, Fail> {
    let mut z = Zval::new();
    match v {
        Data::Null => z.set_null(),
        Data::Bool(b) => z.set_bool(b),
        Data::Number(n) => {
            if let Some(i) = n.as_i64() {
                z.set_long(i);
            } else {
                z.set_double(n.as_f64().unwrap_or(f64::NAN));
            }
        }
        Data::String(s) => z
            .set_string(&s, false)
            .map_err(|e| value_error(format!("cannot build string: {e}")))?,
        Data::Array(_) | Data::Object(_) => unreachable!("containers are opened, not converted"),
    }
    Ok(z)
}

/// The integer PHP itself would use for string array key `s`, following
/// the engine's rule (`ZEND_HANDLE_NUMERIC_STR`): an optional `-`, then
/// digits with no leading zero, fitting in 64 bits. `"-0"` stays a string.
///
/// Inserting `"1"` as a string key would create an entry that `$a["1"]`
/// can never find, because PHP looks that up as the integer 1.
pub fn numeric_key(s: &str) -> Option<i64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
        || s == "-0"
    {
        return None;
    }
    s.parse().ok()
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
