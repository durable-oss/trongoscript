//! The `trongoscript` PHP extension.
//!
//! Exposes `Trongoscript\Context` and a handful of one-shot functions over
//! [`trongoscript_host::Session`]. This layer converts values, parses
//! option arrays, maps errors to exception classes, and implements
//! [`trongoscript_host::Host`] so PHP callables can serve as Trongoscript
//! functions.

#![cfg_attr(windows, feature(abi_vectorcall))]

mod convert;
mod host;
mod options;

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;

use ext_php_rs::exception::{PhpException, PhpResult};
use ext_php_rs::prelude::*;
use ext_php_rs::types::{ZendClassObject, ZendHashTable, Zval};
use ext_php_rs::zend::{bailout, ce, ExecutorGlobals};
use serde_json::Value as Data;
use trongoscript_host::{to_trongo, ErrorKind, HostError, Markers, Session};

use crate::convert::{from_php, to_php, type_error, type_name, utf8, value_error};
use crate::host::PhpHost;

/// Base class for every error a script can cause. Invalid arguments raise
/// PHP's own `TypeError` or `ValueError` instead.
#[php_class]
#[php(name = "Trongoscript\\Exception")]
#[php(extends(ce = ce::exception, stub = "\\Exception"))]
#[derive(Default)]
pub struct TrongoException;

/// The source, a template, or a YAML document does not parse.
#[php_class]
#[php(name = "Trongoscript\\ParseException")]
#[php(extends(TrongoException))]
#[derive(Default)]
pub struct ParseException;

/// The script is wrong: an undefined symbol, a type error, a failing
/// builtin.
#[php_class]
#[php(name = "Trongoscript\\EvalException")]
#[php(extends(TrongoException))]
#[derive(Default)]
pub struct EvalException;

/// A sandboxed file operation was refused, or there is no sandbox.
#[php_class]
#[php(name = "Trongoscript\\SandboxException")]
#[php(extends(TrongoException))]
#[derive(Default)]
pub struct SandboxException;

/// The script exceeded a fuel limit, or produced a value nested too deeply
/// to hand to PHP.
#[php_class]
#[php(name = "Trongoscript\\ResourceLimitException")]
#[php(extends(TrongoException))]
#[derive(Default)]
pub struct ResourceLimitException;

/// A host function failed without throwing. One that throws has its own
/// exception propagate instead.
#[php_class]
#[php(name = "Trongoscript\\CallbackException")]
#[php(extends(TrongoException))]
#[derive(Default)]
pub struct CallbackException;

/// A context was used from inside one of its own host functions.
#[php_class]
#[php(name = "Trongoscript\\ConcurrencyException")]
#[php(extends(TrongoException))]
#[derive(Default)]
pub struct ConcurrencyException;

fn exception(e: HostError) -> PhpException {
    let m = e.message;
    match e.kind {
        ErrorKind::Parse => PhpException::from_class::<ParseException>(m),
        ErrorKind::Eval => PhpException::from_class::<EvalException>(m),
        ErrorKind::Sandbox => PhpException::from_class::<SandboxException>(m),
        ErrorKind::ResourceLimit => PhpException::from_class::<ResourceLimitException>(m),
        ErrorKind::Callback => PhpException::from_class::<CallbackException>(m),
        ErrorKind::Argument => value_error(m).into(),
        ErrorKind::Internal => PhpException::from_class::<TrongoException>(m),
    }
}

/// A Trongoscript evaluation context.
///
/// Definitions made by one evaluation stay visible to the next. Each
/// evaluation gets a fresh fuel budget.
#[php_class]
#[php(name = "Trongoscript\\Context")]
pub struct Context {
    /// `RefCell`s because a host function can call back into the context
    /// that is running it. That gets `ConcurrencyException`.
    session: RefCell<Session>,
    functions: RefCell<HashMap<String, Zval>>,
    objects: bool,
}

impl Context {
    fn build(opts: options::ContextOptions) -> PhpResult<Self> {
        let session = Session::new(opts.options).map_err(exception)?;
        let ctx = Self {
            session: RefCell::new(session),
            functions: RefCell::new(HashMap::new()),
            objects: opts.objects,
        };
        for (name, f) in opts.functions {
            ctx.define_fn(name, f)?;
        }
        Ok(ctx)
    }

    fn busy() -> PhpException {
        PhpException::from_class::<ConcurrencyException>(
            "this context is already evaluating: a host function cannot re-enter \
             its own context"
                .into(),
        )
    }

    fn session(&self) -> PhpResult<RefMut<'_, Session>> {
        self.session.try_borrow_mut().map_err(|_| Self::busy())
    }

    fn define_fn(&self, name: String, f: Zval) -> PhpResult<()> {
        let mut functions = self.functions.try_borrow_mut().map_err(|_| Self::busy())?;
        self.session()?.define(&name).map_err(exception)?;
        functions.insert(name, f);
        Ok(())
    }

    /// Run one evaluation. A callable's own exception, or a fatal error
    /// it raised, takes precedence over the evaluation's error.
    fn run<T>(
        &self,
        work: impl FnOnce(&mut Session, &mut PhpHost) -> Result<T, HostError>,
    ) -> PhpResult<T> {
        let functions = self.functions.try_borrow().map_err(|_| Self::busy())?;
        let mut session = self.session()?;
        let mut host = PhpHost::new(&functions, self.objects);
        let result = work(&mut session, &mut host);
        drop(session);
        let (bailed_out, failure) = (host.bailed_out, host.failure.take());
        drop(functions);
        if bailed_out {
            // The evaluator thread has finished and every borrow is gone,
            // so the engine's unwind can proceed as it intended.
            unsafe { bailout() };
        }
        if ExecutorGlobals::has_exception() {
            // Throwing does nothing while one is pending, which is the point:
            // the callable's exception reaches the caller unchanged.
            return Err(PhpException::from_class::<CallbackException>(String::new()));
        }
        if let Some(f) = failure {
            return Err(f.into());
        }
        result.map_err(exception)
    }

    fn value(&self, v: Data) -> PhpResult<Zval> {
        Ok(to_php(v, self.objects)?)
    }

    fn name(z: &Zval) -> PhpResult<String> {
        Ok(utf8("name", z)?)
    }
}

#[php_impl]
impl Context {
    /// Options: `root`, `mounts`, `files`, `seed`, `limits`,
    /// `sandbox_limits`, `globals`, `functions`, and `objects`. See the
    /// README for each.
    #[php(optional = "options")]
    pub fn __construct(options: Option<&ZendHashTable>) -> PhpResult<Self> {
        let (opts, _) = options::parse(options, false)?;
        Self::build(opts)
    }

    /// Evaluate a document and return the value of its last form.
    pub fn eval(&self, source: &Zval) -> PhpResult<Zval> {
        let source = utf8("source", source)?;
        let v = self.run(|s, h| s.eval(&source, h))?;
        self.value(v)
    }

    /// Evaluate a file from the sandbox, as `(include path)` would.
    pub fn eval_file(&self, path: &Zval) -> PhpResult<Zval> {
        let path = utf8("path", path)?;
        let v = self.run(|s, h| s.eval_file(&path, h))?;
        self.value(v)
    }

    /// Render an ERB-style template.
    pub fn render(&self, template: &Zval) -> PhpResult<String> {
        let template = utf8("template", template)?;
        self.run(|s, h| s.render(&template, h))
    }

    /// Replace embedded snippets in a document. `$markers` may set
    /// `prefix` (default `"!trongo "`) and `key` (default `"!trongo"`).
    #[php(optional = "markers")]
    pub fn embed(&self, document: &Zval, markers: Option<&ZendHashTable>) -> PhpResult<Zval> {
        let doc = from_php(document)?;
        let markers = options::markers(markers)?;
        let v = self.run(|s, h| s.embed(doc, &markers, h))?;
        self.value(v)
    }

    /// Like `embed()` for YAML text, also recognizing nodes tagged with
    /// `tag` (default `!trongo`).
    #[php(optional = "markers")]
    pub fn embed_yaml(&self, yaml: &Zval, markers: Option<&ZendHashTable>) -> PhpResult<Zval> {
        let yaml = utf8("yaml", yaml)?;
        let markers = options::markers(markers)?;
        let v = self.run(|s, h| s.embed_yaml(&yaml, &markers, h))?;
        self.value(v)
    }

    /// Call a function by name (a `defn`, builtin, macro, or host
    /// function), or a function value a script returned. Arguments are
    /// passed as data and never evaluated as code.
    pub fn call(&self, function: &Zval, args: &[&Zval]) -> PhpResult<Zval> {
        let args = args
            .iter()
            .map(|a| from_php(a))
            .collect::<Result<Vec<_>, _>>()?;
        let v = if function.dereference().is_string() {
            let name = Self::name(function)?;
            self.run(|s, h| s.call(&name, &args, h))?
        } else {
            let f = from_php(function)?;
            self.run(|s, h| s.call_value(&f, &args, h))?
        };
        self.value(v)
    }

    /// Expose a PHP callable to scripts as the function `$name`. Host
    /// functions shadow builtins of the same name. Returns `$this`.
    pub fn define<'a>(
        self_: &'a mut ZendClassObject<Context>,
        name: &Zval,
        callable: &Zval,
    ) -> PhpResult<&'a mut ZendClassObject<Context>> {
        let name = Self::name(name)?;
        if !callable.is_callable() {
            return Err(type_error(format!(
                "define() expects a callable, got {}",
                type_name(callable)
            ))
            .into());
        }
        self_.define_fn(name, callable.shallow_clone())?;
        Ok(self_)
    }

    /// Remove a host function. Returns whether it was defined.
    pub fn undefine(&self, name: &Zval) -> PhpResult<bool> {
        let name = Self::name(name)?;
        let mut functions = self.functions.try_borrow_mut().map_err(|_| Self::busy())?;
        let removed = self.session()?.undefine(&name);
        functions.remove(&name);
        Ok(removed)
    }

    /// Names of the host functions, sorted.
    pub fn functions(&self) -> PhpResult<Vec<String>> {
        Ok(self.session()?.functions())
    }

    /// A global's value, or null.
    pub fn get(&self, name: &Zval) -> PhpResult<Zval> {
        let name = Self::name(name)?;
        let v = self.session()?.get(&name).map_err(exception)?;
        self.value(v.unwrap_or(Data::Null))
    }

    pub fn set(&self, name: &Zval, value: &Zval) -> PhpResult<()> {
        let name = Self::name(name)?;
        let value = from_php(value)?;
        self.session()?.set(&name, value).map_err(exception)
    }

    pub fn has(&self, name: &Zval) -> PhpResult<bool> {
        let name = Self::name(name)?;
        Ok(self.session()?.contains(&name))
    }

    /// Remove a global. Returns whether it existed.
    pub fn remove(&self, name: &Zval) -> PhpResult<bool> {
        let name = Self::name(name)?;
        Ok(self.session()?.unset(&name))
    }

    /// Every global, sorted by name.
    pub fn globals(&self) -> PhpResult<Zval> {
        let all = self.session()?.globals().map_err(exception)?;
        let map = all.into_iter().collect::<serde_json::Map<_, _>>();
        // Always an array keyed by name, whatever `objects` says: this is a
        // listing, not a Trongoscript map.
        Ok(to_php(Data::Object(map), false)?)
    }

    /// What the last evaluation consumed: `steps`, `allocated` (bytes),
    /// `peak_call_depth`, and `peak_eval_depth`.
    pub fn usage(&self) -> PhpResult<HashMap<String, i64>> {
        let u = self.session()?.usage();
        let int = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        Ok(HashMap::from([
            ("steps".into(), int(u.steps)),
            ("allocated".into(), int(u.allocated)),
            ("peak_call_depth".into(), int(u.peak_call_depth as u64)),
            ("peak_eval_depth".into(), int(u.peak_eval_depth as u64)),
        ]))
    }
}

fn one_shot(options: Option<&ZendHashTable>, with_markers: bool) -> PhpResult<(Context, Markers)> {
    let (opts, markers) = options::parse(options, with_markers)?;
    Ok((Context::build(opts)?, markers))
}

/// Evaluate a document in a fresh context.
#[php_function]
#[php(name = "Trongoscript\\evaluate", optional = "options")]
pub fn evaluate(source: &Zval, options: Option<&ZendHashTable>) -> PhpResult<Zval> {
    one_shot(options, false)?.0.eval(source)
}

/// Evaluate a sandbox file in a fresh context.
#[php_function]
#[php(name = "Trongoscript\\evaluate_file", optional = "options")]
pub fn evaluate_file(path: &Zval, options: Option<&ZendHashTable>) -> PhpResult<Zval> {
    one_shot(options, false)?.0.eval_file(path)
}

/// Render a template in a fresh context.
#[php_function]
#[php(name = "Trongoscript\\render", optional = "options")]
pub fn render(template: &Zval, options: Option<&ZendHashTable>) -> PhpResult<String> {
    one_shot(options, false)?.0.render(template)
}

/// Replace embedded snippets in a document, in a fresh context. Options may
/// also set the markers `prefix` and `key`.
#[php_function]
#[php(name = "Trongoscript\\embed", optional = "options")]
pub fn embed(document: &Zval, options: Option<&ZendHashTable>) -> PhpResult<Zval> {
    let (ctx, markers) = one_shot(options, true)?;
    let doc = from_php(document)?;
    let v = ctx.run(|s, h| s.embed(doc, &markers, h))?;
    ctx.value(v)
}

/// Like `embed()` for YAML text. Options may also set `tag`.
#[php_function]
#[php(name = "Trongoscript\\embed_yaml", optional = "options")]
pub fn embed_yaml(yaml: &Zval, options: Option<&ZendHashTable>) -> PhpResult<Zval> {
    let (ctx, markers) = one_shot(options, true)?;
    let yaml = utf8("yaml", yaml)?;
    let v = ctx.run(|s, h| s.embed_yaml(&yaml, &markers, h))?;
    ctx.value(v)
}

/// Format a PHP value as Trongoscript source that evaluates back to it.
#[php_function]
#[php(name = "Trongoscript\\dump", optional = "pretty")]
pub fn dump(value: &Zval, pretty: Option<bool>) -> PhpResult<String> {
    Ok(to_trongo(&from_php(value)?, pretty.unwrap_or(false)))
}

#[php_module]
pub fn get_module(module: ModuleBuilder) -> ModuleBuilder {
    // The macro names the module after the Cargo package, which cannot be
    // `trongoscript`: that is the core crate's name, in the same build.
    module
        .name("trongoscript")
        .class::<TrongoException>()
        .class::<ParseException>()
        .class::<EvalException>()
        .class::<SandboxException>()
        .class::<ResourceLimitException>()
        .class::<CallbackException>()
        .class::<ConcurrencyException>()
        .class::<Context>()
        .function(wrap_function!(evaluate))
        .function(wrap_function!(evaluate_file))
        .function(wrap_function!(render))
        .function(wrap_function!(embed))
        .function(wrap_function!(embed_yaml))
        .function(wrap_function!(dump))
}

// Copyright (c) 2026 Durable Programming, LLC.
