//! Native half of the `trongoscript` gem.
//!
//! Defines `Trongoscript::Native`, a thin wrapper over
//! [`trongoscript_host::Session`]. The public API, keyword arguments, and
//! host-function bookkeeping live in Ruby (`lib/trongoscript/context.rb`);
//! this layer converts values, maps errors to exception classes, and
//! implements [`trongoscript_host::Host`].

mod convert;
mod host;

use std::cell::{RefCell, RefMut};

use magnus::value::ReprValue;
use magnus::{
    function, method, prelude::*, Error, ExceptionClass, RArray, RHash, RModule, Ruby, TryConvert,
    Value,
};
use serde_json::Value as Data;
use trongoscript_host::{
    fuel_limits, sandbox_limits, to_trongo, ErrorKind, HostError, Markers, Options, SandboxSpec,
    Session,
};

use crate::convert::{from_ruby, string, to_ruby};
use crate::host::RubyHost;

#[magnus::wrap(class = "Trongoscript::Native", size)]
struct Native {
    /// `RefCell` because the GVL is released while an evaluation runs, so
    /// another Ruby thread, or a host function, can reach this object
    /// mid-evaluation. Such a call gets `ConcurrencyError`, not a data race.
    session: RefCell<Session>,
    symbolize: bool,
}

impl Native {
    /// `opts` is the hash `Trongoscript::Context#initialize` builds; its
    /// shape is checked there, so errors here are type errors.
    fn new(ruby: &Ruby, opts: RHash) -> Result<Self, Error> {
        let get =
            |key: &str| -> Result<Value, Error> { opts.lookup::<_, Value>(ruby.to_symbol(key)) };
        let pairs = |key: &str| -> Result<Vec<(String, Value)>, Error> {
            let v = get(key)?;
            if v.is_nil() {
                return Ok(Vec::new());
            }
            let arr = RArray::try_convert(v)?;
            let mut out = Vec::with_capacity(arr.len());
            for pair in arr.into_iter() {
                let (k, v): (Value, Value) = TryConvert::try_convert(pair)?;
                out.push((string(k)?, v));
            }
            Ok(out)
        };
        let strings = |key: &str| -> Result<Vec<(String, String)>, Error> {
            pairs(key)?
                .into_iter()
                .map(|(k, v)| Ok((k, string(v)?)))
                .collect()
        };
        let numbers = |key: &str| -> Result<Vec<(String, u64)>, Error> {
            pairs(key)?
                .into_iter()
                .map(|(k, v)| Ok((k, u64::try_convert(v)?)))
                .collect()
        };

        let root = get("root")?;
        let files = get("files")?;
        let sandbox = if !root.is_nil() {
            SandboxSpec::Dir {
                root: string(root)?.into(),
                mounts: strings("mounts")?
                    .into_iter()
                    .map(|(v, h)| (v, h.into()))
                    .collect(),
            }
        } else if !files.is_nil() {
            SandboxSpec::Memory {
                files: strings("files")?,
            }
        } else {
            SandboxSpec::None
        };
        let preset = get("preset")?;
        let preset = if preset.is_nil() {
            None
        } else {
            Some(string(preset)?)
        };
        let seed = get("seed")?;
        let globals = pairs("globals")?
            .into_iter()
            .map(|(k, v)| Ok((k, from_ruby(ruby, v)?)))
            .collect::<Result<Vec<_>, Error>>()?;

        let options = Options {
            sandbox,
            sandbox_limits: sandbox_limits(&numbers("sandbox_limits")?)
                .map_err(|e| raise(ruby, e))?,
            seed: if seed.is_nil() {
                None
            } else {
                Some(u64::try_convert(seed)?)
            },
            limits: fuel_limits(preset.as_deref(), &numbers("limits")?)
                .map_err(|e| raise(ruby, e))?,
            globals,
        };
        let session = Session::new(options).map_err(|e| raise(ruby, e))?;
        Ok(Self {
            session: RefCell::new(session),
            symbolize: get("symbolize_names")?.to_bool(),
        })
    }

    fn session(&self, ruby: &Ruby) -> Result<RefMut<'_, Session>, Error> {
        self.session
            .try_borrow_mut()
            .map_err(|_| match error_class(ruby, "ConcurrencyError") {
                Ok(class) => Error::new(
                    class,
                    "this context is already evaluating: a host function cannot \
                     re-enter its own context, and a context cannot be used from \
                     two threads at once",
                ),
                Err(e) => e,
            })
    }

    /// Run one evaluation, re-raising a host function's own exception, or
    /// a pending interrupt, in place of the evaluation's error.
    fn run<T>(
        &self,
        ruby: &Ruby,
        dispatcher: Value,
        work: impl FnOnce(&mut Session, &mut RubyHost) -> Result<T, HostError>,
    ) -> Result<T, Error> {
        let mut session = self.session(ruby)?;
        let mut host = RubyHost::new(ruby, dispatcher, self.symbolize);
        let result = work(&mut session, &mut host);
        drop(session);
        host.take_error()?;
        result.map_err(|e| raise(ruby, e))
    }

    fn data(&self, ruby: &Ruby, v: Data) -> Result<Value, Error> {
        to_ruby(ruby, v, self.symbolize)
    }

    fn eval(ruby: &Ruby, rb_self: &Self, source: Value, dispatcher: Value) -> Result<Value, Error> {
        let source = string(source)?;
        let v = rb_self.run(ruby, dispatcher, |s, h| s.eval(&source, h))?;
        rb_self.data(ruby, v)
    }

    fn eval_file(
        ruby: &Ruby,
        rb_self: &Self,
        path: Value,
        dispatcher: Value,
    ) -> Result<Value, Error> {
        let path = string(path)?;
        let v = rb_self.run(ruby, dispatcher, |s, h| s.eval_file(&path, h))?;
        rb_self.data(ruby, v)
    }

    fn render(
        ruby: &Ruby,
        rb_self: &Self,
        source: Value,
        dispatcher: Value,
    ) -> Result<String, Error> {
        let source = string(source)?;
        rb_self.run(ruby, dispatcher, |s, h| s.render(&source, h))
    }

    fn embed(
        ruby: &Ruby,
        rb_self: &Self,
        doc: Value,
        markers: RHash,
        dispatcher: Value,
    ) -> Result<Value, Error> {
        let doc = from_ruby(ruby, doc)?;
        let markers = markers_from(ruby, markers)?;
        let v = rb_self.run(ruby, dispatcher, |s, h| s.embed(doc, &markers, h))?;
        rb_self.data(ruby, v)
    }

    fn embed_yaml(
        ruby: &Ruby,
        rb_self: &Self,
        yaml: Value,
        markers: RHash,
        dispatcher: Value,
    ) -> Result<Value, Error> {
        let yaml = string(yaml)?;
        let markers = markers_from(ruby, markers)?;
        let v = rb_self.run(ruby, dispatcher, |s, h| s.embed_yaml(&yaml, &markers, h))?;
        rb_self.data(ruby, v)
    }

    fn call(
        ruby: &Ruby,
        rb_self: &Self,
        name: Value,
        args: RArray,
        dispatcher: Value,
    ) -> Result<Value, Error> {
        let name = string(name)?;
        let args = args_from(ruby, args)?;
        let v = rb_self.run(ruby, dispatcher, |s, h| s.call(&name, &args, h))?;
        rb_self.data(ruby, v)
    }

    fn call_value(
        ruby: &Ruby,
        rb_self: &Self,
        f: Value,
        args: RArray,
        dispatcher: Value,
    ) -> Result<Value, Error> {
        let f = from_ruby(ruby, f)?;
        let args = args_from(ruby, args)?;
        let v = rb_self.run(ruby, dispatcher, |s, h| s.call_value(&f, &args, h))?;
        rb_self.data(ruby, v)
    }

    fn define(ruby: &Ruby, rb_self: &Self, name: Value) -> Result<(), Error> {
        let name = string(name)?;
        rb_self
            .session(ruby)?
            .define(&name)
            .map_err(|e| raise(ruby, e))
    }

    fn undefine(ruby: &Ruby, rb_self: &Self, name: Value) -> Result<bool, Error> {
        let name = string(name)?;
        Ok(rb_self.session(ruby)?.undefine(&name))
    }

    fn functions(ruby: &Ruby, rb_self: &Self) -> Result<Vec<String>, Error> {
        Ok(rb_self.session(ruby)?.functions())
    }

    fn get(ruby: &Ruby, rb_self: &Self, name: Value) -> Result<Value, Error> {
        let name = string(name)?;
        let v = rb_self
            .session(ruby)?
            .get(&name)
            .map_err(|e| raise(ruby, e))?;
        rb_self.data(ruby, v.unwrap_or(Data::Null))
    }

    fn has(ruby: &Ruby, rb_self: &Self, name: Value) -> Result<bool, Error> {
        let name = string(name)?;
        Ok(rb_self.session(ruby)?.contains(&name))
    }

    fn set(ruby: &Ruby, rb_self: &Self, name: Value, value: Value) -> Result<(), Error> {
        let name = string(name)?;
        let value = from_ruby(ruby, value)?;
        rb_self
            .session(ruby)?
            .set(&name, value)
            .map_err(|e| raise(ruby, e))
    }

    fn unset(ruby: &Ruby, rb_self: &Self, name: Value) -> Result<bool, Error> {
        let name = string(name)?;
        Ok(rb_self.session(ruby)?.unset(&name))
    }

    fn globals(ruby: &Ruby, rb_self: &Self) -> Result<RHash, Error> {
        let all = rb_self
            .session(ruby)?
            .globals()
            .map_err(|e| raise(ruby, e))?;
        let hash = ruby.hash_new_capa(all.len());
        for (k, v) in all {
            hash.aset(ruby.str_new(&k), rb_self.data(ruby, v)?)?;
        }
        Ok(hash)
    }

    fn usage(ruby: &Ruby, rb_self: &Self) -> Result<RHash, Error> {
        let u = rb_self.session(ruby)?.usage();
        let hash = ruby.hash_new();
        hash.aset(ruby.to_symbol("steps"), u.steps)?;
        hash.aset(ruby.to_symbol("allocated"), u.allocated)?;
        hash.aset(ruby.to_symbol("peak_call_depth"), u.peak_call_depth)?;
        hash.aset(ruby.to_symbol("peak_eval_depth"), u.peak_eval_depth)?;
        Ok(hash)
    }
}

fn args_from(ruby: &Ruby, args: RArray) -> Result<Vec<Data>, Error> {
    args.into_iter().map(|a| from_ruby(ruby, a)).collect()
}

fn markers_from(ruby: &Ruby, opts: RHash) -> Result<Markers, Error> {
    let mut m = Markers::default();
    for (key, slot) in [
        ("prefix", &mut m.prefix),
        ("key", &mut m.key),
        ("tag", &mut m.tag),
    ] {
        let v: Value = opts.lookup(ruby.to_symbol(key))?;
        if !v.is_nil() {
            *slot = string(v)?;
        }
    }
    Ok(m)
}

fn dump(ruby: &Ruby, value: Value, pretty: bool) -> Result<String, Error> {
    Ok(to_trongo(&from_ruby(ruby, value)?, pretty))
}

fn error_class(ruby: &Ruby, name: &str) -> Result<ExceptionClass, Error> {
    ruby.class_object()
        .const_get::<_, RModule>("Trongoscript")?
        .const_get(name)
}

fn raise(ruby: &Ruby, e: HostError) -> Error {
    let name = match e.kind {
        ErrorKind::Argument => return Error::new(ruby.exception_arg_error(), e.message),
        ErrorKind::Parse => "ParseError",
        ErrorKind::ResourceLimit => "ResourceLimitError",
        ErrorKind::Sandbox => "SandboxError",
        ErrorKind::Eval => "EvalError",
        ErrorKind::Callback => "CallbackError",
        ErrorKind::Internal => "Error",
    };
    match error_class(ruby, name) {
        Ok(class) => Error::new(class, e.message),
        Err(lookup) => lookup,
    }
}

#[magnus::init(name = "trongoscript")]
fn init(ruby: &Ruby) -> Result<(), Error> {
    let module = ruby.define_module("Trongoscript")?;
    let class = module.define_class("Native", ruby.class_object())?;
    class.define_singleton_method("new", function!(Native::new, 1))?;
    class.define_singleton_method("dump", function!(dump, 2))?;
    class.define_method("eval", method!(Native::eval, 2))?;
    class.define_method("eval_file", method!(Native::eval_file, 2))?;
    class.define_method("render", method!(Native::render, 2))?;
    class.define_method("embed", method!(Native::embed, 3))?;
    class.define_method("embed_yaml", method!(Native::embed_yaml, 3))?;
    class.define_method("call", method!(Native::call, 3))?;
    class.define_method("call_value", method!(Native::call_value, 3))?;
    class.define_method("define", method!(Native::define, 1))?;
    class.define_method("undefine", method!(Native::undefine, 1))?;
    class.define_method("functions", method!(Native::functions, 0))?;
    class.define_method("get", method!(Native::get, 1))?;
    class.define_method("key?", method!(Native::has, 1))?;
    class.define_method("set", method!(Native::set, 2))?;
    class.define_method("unset", method!(Native::unset, 1))?;
    class.define_method("globals", method!(Native::globals, 0))?;
    class.define_method("usage", method!(Native::usage, 0))?;
    Ok(())
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
