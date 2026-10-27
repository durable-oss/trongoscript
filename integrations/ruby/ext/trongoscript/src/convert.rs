//! Ruby objects to and from Trongoscript values.
//!
//! Both directions walk with an explicit stack instead of recursing: a
//! Ruby thread other than the main one has about 1 MiB of machine stack,
//! and nothing here should be the reason a deep document crashes it.

use magnus::rb_sys::AsRawValue;
use magnus::value::ReprValue;
use magnus::{
    Error, Float, Integer, IntoValue, RArray, RHash, RString, Ruby, Symbol, TryConvert, Value,
};
use serde_json::{Map, Number, Value as Data};
use trongoscript_host::MAX_NESTING;

/// Convert a Ruby object to a Trongoscript value.
///
/// Accepts `nil`, booleans, 64-bit integers, finite floats, strings that
/// can be represented as UTF-8, symbols (as strings), arrays, and hashes
/// whose keys are strings or symbols. Anything else raises `TypeError`
/// rather than being coerced through `to_s`.
pub fn from_ruby(ruby: &Ruby, root: Value) -> Result<Data, Error> {
    enum Frame {
        Array {
            src: RArray,
            next: usize,
            out: Vec<Data>,
        },
        Hash {
            pairs: std::vec::IntoIter<(String, Value)>,
            key: Option<String>,
            out: Map<String, Data>,
        },
    }

    let open = |v: Value, depth: usize| -> Result<Result<Data, Frame>, Error> {
        if let Some(arr) = RArray::from_value(v) {
            check_depth(ruby, depth)?;
            return Ok(Err(Frame::Array {
                src: arr,
                next: 0,
                out: Vec::with_capacity(arr.len()),
            }));
        }
        if let Some(hash) = RHash::from_value(v) {
            check_depth(ruby, depth)?;
            // Collecting the pairs first is GC-safe: they stay reachable
            // from the hash, and no Ruby code runs until conversion ends.
            let mut pairs = Vec::with_capacity(hash.len());
            hash.foreach(|k: Value, v: Value| {
                pairs.push((hash_key(ruby, k)?, v));
                Ok(magnus::r_hash::ForEach::Continue)
            })?;
            return Ok(Err(Frame::Hash {
                pairs: pairs.into_iter(),
                key: None,
                out: Map::new(),
            }));
        }
        scalar(ruby, v).map(Ok)
    };

    let mut stack = match open(root, 1)? {
        Ok(scalar) => return Ok(scalar),
        Err(frame) => vec![frame],
    };
    loop {
        let depth = stack.len() + 1;
        let top = stack.last_mut().expect("stack is non-empty");
        let child = match top {
            Frame::Array { src, next, .. } => {
                if *next < src.len() {
                    *next += 1;
                    Some(src.entry::<Value>(*next as isize - 1)?)
                } else {
                    None
                }
            }
            Frame::Hash { pairs, key, .. } => pairs.next().map(|(k, v)| {
                *key = Some(k);
                v
            }),
        };
        let done = match child {
            Some(v) => match open(v, depth)? {
                Ok(scalar) => scalar,
                Err(frame) => {
                    stack.push(frame);
                    continue;
                }
            },
            None => match stack.pop().expect("stack is non-empty") {
                Frame::Array { out, .. } => Data::Array(out),
                Frame::Hash { out, .. } => Data::Object(out),
            },
        };
        // `done` is a finished child of the frame now on top, or, if none
        // is left, the finished root.
        match stack.last_mut() {
            None => return Ok(done),
            Some(Frame::Array { out, .. }) => out.push(done),
            Some(Frame::Hash { out, key, .. }) => {
                let k = key.take().expect("hash child has a key");
                if out.contains_key(&k) {
                    return Err(Error::new(
                        ruby.exception_arg_error(),
                        format!("duplicate key {k:?} (a String and a Symbol with the same name)"),
                    ));
                }
                out.insert(k, done);
            }
        }
    }
}

fn check_depth(ruby: &Ruby, depth: usize) -> Result<(), Error> {
    if depth > MAX_NESTING {
        Err(Error::new(
            ruby.exception_arg_error(),
            format!("value nests deeper than {MAX_NESTING} levels (is it self-referential?)"),
        ))
    } else {
        Ok(())
    }
}

fn hash_key(ruby: &Ruby, k: Value) -> Result<String, Error> {
    if let Some(s) = RString::from_value(k) {
        s.to_string()
    } else if let Some(sym) = Symbol::from_value(k) {
        Ok(sym.name()?.into_owned())
    } else {
        Err(Error::new(
            ruby.exception_type_error(),
            format!("hash keys must be Strings or Symbols, got {}", unsafe {
                k.classname()
            }),
        ))
    }
}

fn scalar(ruby: &Ruby, v: Value) -> Result<Data, Error> {
    let raw = v.as_raw();
    if v.is_nil() {
        return Ok(Data::Null);
    }
    if raw == ruby.qtrue().as_raw() {
        return Ok(Data::Bool(true));
    }
    if raw == ruby.qfalse().as_raw() {
        return Ok(Data::Bool(false));
    }
    if let Some(i) = Integer::from_value(v) {
        if let Ok(n) = i.to_i64() {
            return Ok(Data::from(n));
        }
        if let Ok(n) = i.to_u64() {
            return Ok(Data::from(n));
        }
        return Err(Error::new(
            ruby.exception_range_error(),
            format!("integer {} does not fit in 64 bits", i.inspect()),
        ));
    }
    if let Some(f) = Float::from_value(v) {
        let f = f.to_f64();
        return Number::from_f64(f).map(Data::Number).ok_or_else(|| {
            Error::new(
                ruby.exception_float_domain_error(),
                format!("{f} has no Trongoscript representation"),
            )
        });
    }
    if let Some(s) = RString::from_value(v) {
        return s.to_string().map(Data::String);
    }
    if let Some(sym) = Symbol::from_value(v) {
        return Ok(Data::String(sym.name()?.into_owned()));
    }
    Err(Error::new(
        ruby.exception_type_error(),
        format!("cannot convert {} to a Trongoscript value", unsafe {
            v.classname()
        }),
    ))
}

/// Convert a Trongoscript value to a Ruby object. Map keys become strings,
/// or symbols with `symbolize`.
///
/// Containers are attached to their parent before they are filled, so
/// every object built so far is reachable from `root`, which lives on this
/// stack frame where Ruby's GC scans for it.
pub fn to_ruby(ruby: &Ruby, v: Data, symbolize: bool) -> Result<Value, Error> {
    enum Frame {
        Array(std::vec::IntoIter<Data>, RArray),
        Hash(serde_json::map::IntoIter, RHash),
    }

    let open = |v: Data| -> (Value, Option<Frame>) {
        match v {
            Data::Array(items) => {
                let arr = ruby.ary_new_capa(items.len());
                (arr.as_value(), Some(Frame::Array(items.into_iter(), arr)))
            }
            Data::Object(map) => {
                let hash = ruby.hash_new_capa(map.len());
                (hash.as_value(), Some(Frame::Hash(map.into_iter(), hash)))
            }
            other => (scalar_to_ruby(ruby, other), None),
        }
    };

    let (root, frame) = open(v);
    let mut stack: Vec<Frame> = frame.into_iter().collect();
    while let Some(top) = stack.last_mut() {
        match top {
            Frame::Array(items, arr) => {
                let arr = *arr;
                match items.next() {
                    Some(child) => {
                        let (value, frame) = open(child);
                        arr.push(value)?;
                        stack.extend(frame);
                    }
                    None => {
                        stack.pop();
                    }
                }
            }
            Frame::Hash(entries, hash) => {
                let hash = *hash;
                match entries.next() {
                    Some((k, child)) => {
                        let key = if symbolize {
                            ruby.to_symbol(&k).as_value()
                        } else {
                            ruby.str_new(&k).as_value()
                        };
                        let (value, frame) = open(child);
                        hash.aset(key, value)?;
                        stack.extend(frame);
                    }
                    None => {
                        stack.pop();
                    }
                }
            }
        }
    }
    Ok(root)
}

fn scalar_to_ruby(ruby: &Ruby, v: Data) -> Value {
    match v {
        Data::Null => ruby.qnil().as_value(),
        Data::Bool(b) => b.into_value_with(ruby),
        Data::Number(n) => {
            if let Some(i) = n.as_i64() {
                ruby.integer_from_i64(i).as_value()
            } else if let Some(u) = n.as_u64() {
                ruby.integer_from_u64(u).as_value()
            } else {
                ruby.float_from_f64(n.as_f64().unwrap_or(f64::NAN))
                    .as_value()
            }
        }
        Data::String(s) => ruby.str_new(&s).as_value(),
        Data::Array(_) | Data::Object(_) => unreachable!("containers are opened, not converted"),
    }
}

/// Convert a Ruby string to a Rust one, re-encoding to UTF-8 when the
/// string's encoding allows it.
pub fn string(v: Value) -> Result<String, Error> {
    RString::try_convert(v)?.to_string()
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
