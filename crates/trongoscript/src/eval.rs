//! Tree-walking evaluator.
//!
//! Evaluates a parsed [`Module`] or [`Expr`] into a [`Value`] using the
//! inventory-registered [`trongoscript_stdlib::Builtin`] registry plus a
//! small set of "host" builtins that need access to the [`Sandbox`]
//! (currently `include`, `read-file`, `list-files`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use trongoscript_parser::{parse_module, Expr, Module};
use trongoscript_stdlib::{find_builtin_for, BuiltinError, BuiltinResult, Value};

use crate::error::{EvalError, EvalResult};
use crate::modules::{is_known_module, ts_module_source};
use crate::sandbox::{IncludeStack, Sandbox};
use crate::value::{fn_handle, is_truthy, make_fn_value};

/// Configuration passed to the evaluator.
pub struct EvalConfig {
    /// Optional filesystem sandbox. Without one, the `include`,
    /// `read-file`, and `list-files` builtins all fail with `NoSandbox`.
    pub sandbox: Option<Sandbox>,
    /// Host-provided builtins, looked up *before* the inventory registry
    /// so a host can override a stdlib name if it really wants to.
    pub host_builtins: HashMap<String, HostBuiltin>,
    /// Initial seed for the per-context RNG used by the `rng` module
    /// (`rand`, `choose`, `dice-roll`, ...). `None` → seed 0, which makes
    /// runs fully reproducible by default. Hosts that want freshness per
    /// invocation (e.g. the CLI) should pass the current timestamp.
    pub rng_seed: Option<u64>,
}

impl EvalConfig {
    pub fn new() -> Self {
        Self {
            sandbox: None,
            host_builtins: HashMap::new(),
            rng_seed: None,
        }
    }

    pub fn with_sandbox(mut self, sandbox: Sandbox) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    pub fn with_host_builtin(mut self, name: impl Into<String>, b: HostBuiltin) -> Self {
        self.host_builtins.insert(name.into(), b);
        self
    }

    pub fn with_rng_seed(mut self, seed: u64) -> Self {
        self.rng_seed = Some(seed);
        self
    }
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// A host-provided builtin. Unlike inventory builtins, these receive a
/// mutable reference to the [`Context`] so they can perform sandboxed I/O
/// or other privileged work the host wants to expose.
pub type HostBuiltin = std::sync::Arc<
    dyn for<'a> Fn(&'a mut Context, &[Value]) -> BuiltinResult + Send + Sync + 'static,
>;

/// A user-defined macro.
#[derive(Debug, Clone)]
pub struct MacroDef {
    pub name: String,
    pub params: Vec<String>,
    pub rest_param: Option<String>,
    pub body: Vec<Expr>,
}

/// A user-defined first-class function (a closure).
///
/// `captured_lexical` is a snapshot of the lexical scope frames *above*
/// the global frame at the moment the closure was created. Globals are
/// not captured — they're read live from `scopes[0]` at call time, so
/// `def`s installed after closure creation are visible (matching common
/// Lisp/Scheme/Clojure semantics).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Closure {
    pub params: Vec<String>,
    pub rest_param: Option<String>,
    pub body: Vec<Expr>,
    pub captured_lexical: Vec<HashMap<String, Value>>,
}

/// Per-invocation evaluator state.
pub struct Context {
    /// Lexical bindings; the innermost frame is at the end.
    scopes: Vec<HashMap<String, Value>>,
    /// User-defined macros (installed by `defmacro`).
    pub macros: HashMap<String, MacroDef>,
    /// The currently-executing file's directory (used to resolve relative
    /// `include` paths). `None` for inline code.
    pub current_file_dir: Option<PathBuf>,
    /// Active include guard for cycle detection / depth limiting.
    pub includes: IncludeStack,
    /// The sandbox, if configured.
    pub sandbox: Option<Sandbox>,
    /// Host-provided builtins.
    pub host_builtins: HashMap<String, HostBuiltin>,
    /// Recursion limit for macro expansion (to defang `(defmacro a [] (a))`).
    pub max_macro_expansion_depth: usize,
    /// Current macro expansion depth, tracked across re-entrant `eval`
    /// calls so a self-expanding macro is caught quickly.
    pub(crate) macro_depth: usize,
    /// Module names enabled via `(use ...)`. Names here unlock the
    /// matching set of module-tagged builtins (and TS macros).
    pub enabled_modules: HashSet<String>,
    /// TS modules whose source has already been eval'd in this Context
    /// (so a repeated `(use foo)` is a no-op rather than re-installing).
    pub(crate) loaded_ts_modules: HashSet<String>,
    /// SplitMix64 state shared by every `rng` module function. Mutated on
    /// every consumption; resettable via `(set-seed n)`. Starts at the
    /// value supplied by [`EvalConfig::rng_seed`] (or 0).
    pub rng_state: u64,
    /// First-class function registry. A `(fn ...)` or `(defn ...)` form
    /// pushes a [`Closure`] here and returns a handle [`Value`] (see
    /// [`crate::value::make_fn_value`]) that indexes into this vec.
    pub fns: Vec<Closure>,
}

impl Context {
    pub fn new(config: EvalConfig) -> Self {
        Self {
            scopes: vec![HashMap::new()],
            macros: HashMap::new(),
            current_file_dir: None,
            includes: IncludeStack::default(),
            sandbox: config.sandbox,
            host_builtins: config.host_builtins,
            max_macro_expansion_depth: 256,
            macro_depth: 0,
            enabled_modules: HashSet::new(),
            loaded_ts_modules: HashSet::new(),
            rng_state: config.rng_seed.unwrap_or(0),
            fns: Vec::new(),
        }
    }

    pub fn with_data(mut self, data: HashMap<String, Value>) -> Self {
        for (k, v) in data {
            self.scopes[0].insert(k, v);
        }
        self
    }

    /// Returns a reference to the global scope (scopes[0]).
    pub fn globals(&self) -> &HashMap<String, Value> {
        &self.scopes[0]
    }

    /// Returns a mutable reference to the global scope.
    pub fn globals_mut(&mut self) -> &mut HashMap<String, Value> {
        &mut self.scopes[0]
    }

    /// Rebuilds a Context from persisted globals and closures.
    /// The sandbox, host_builtins, and config must be re-provided —
    /// they are not serialized because they are host configuration, not state.
    pub fn from_persisted(
        globals: HashMap<String, Value>,
        fns: Vec<Closure>,
        enabled_modules: HashSet<String>,
        rng_state: u64,
        config: EvalConfig,
    ) -> Self {
        let mut ctx = Self::new(config);
        ctx.scopes[0] = globals;
        ctx.fns = fns;
        ctx.enabled_modules = enabled_modules;
        ctx.rng_state = rng_state;
        ctx
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind(&mut self, name: &str, value: Value) {
        // Top-level scope is `scopes[0]`; if we're at top-level, `def`
        // installs there. `let` always uses the innermost scope.
        let last = self.scopes.last_mut().expect("at least one scope");
        last.insert(name.to_string(), value);
    }

    fn bind_global(&mut self, name: &str, value: Value) {
        self.scopes[0].insert(name.to_string(), value);
    }

    /// Register a synthesized closure and return its function-handle [`Value`].
    /// Used by combinators like `constantly` and `complement` that need to
    /// produce a function value at runtime.
    pub(crate) fn register_closure(&mut self, closure: Closure) -> Value {
        let handle = self.fns.len();
        self.fns.push(closure);
        make_fn_value(handle)
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }
}

/// Convenience: parse + evaluate inline source.
pub fn eval_source(source: &str, ctx: &mut Context) -> EvalResult<Value> {
    let module = parse_module(source)?;
    eval_module(&module, ctx)
}

/// Evaluate a sequence of top-level forms; the value of the module is the
/// value of the last form (or `nil` if there are no forms).
pub fn eval_module(module: &Module, ctx: &mut Context) -> EvalResult<Value> {
    let mut last = Value::Null;
    for f in &module.forms {
        last = eval(f, ctx)?;
    }
    Ok(last)
}

/// Evaluate a single expression.
pub fn eval(expr: &Expr, ctx: &mut Context) -> EvalResult<Value> {
    match expr {
        Expr::Nil => Ok(Value::Null),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::Number(n) => Ok(crate::value::num_from_f64(*n)),
        Expr::String(s) => Ok(Value::String(s.clone())),
        // Keywords evaluate to their string name. This keeps the data model
        // JSON-shaped.
        Expr::Keyword(k) => Ok(Value::String(k.clone())),
        Expr::Symbol(s) => match ctx.lookup(s) {
            Some(v) => Ok(v.clone()),
            None => Err(EvalError::UndefinedSymbol(s.clone())),
        },
        Expr::Vector(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(eval(it, ctx)?);
            }
            Ok(Value::Array(out))
        }
        Expr::Map(pairs) => {
            let mut out = serde_json::Map::with_capacity(pairs.len());
            for (k, v) in pairs {
                let key = eval_map_key(k, ctx)?;
                let value = eval(v, ctx)?;
                out.insert(key, value);
            }
            Ok(Value::Object(out))
        }
        Expr::List(items) => eval_list(items, ctx),
    }
}

fn eval_map_key(k: &Expr, ctx: &mut Context) -> EvalResult<String> {
    match k {
        // Common cases first: bare keyword or string literal.
        Expr::Keyword(s) | Expr::String(s) => Ok(s.clone()),
        other => {
            let v = eval(other, ctx)?;
            match v {
                Value::String(s) => Ok(s),
                Value::Bool(b) => Ok(b.to_string()),
                Value::Number(n) => Ok(n.to_string()),
                Value::Null => Ok(String::new()),
                bad => Err(EvalError::BadMapKey(format!("{:?}", bad))),
            }
        }
    }
}

fn eval_list(items: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    // Empty list: () evaluates to an empty list value.
    let Some(head) = items.first() else {
        return Ok(Value::Array(Vec::new()));
    };

    // Special forms — head must be a literal Symbol.
    if let Expr::Symbol(name) = head {
        match name.as_str() {
            "quote" => return special_quote(&items[1..]),
            "quasiquote" => return special_quasiquote(&items[1..], ctx),
            "unquote" | "unquote-splicing" => {
                return Err(EvalError::InvalidForm(format!(
                    "`{}` only valid inside a quasiquote",
                    name
                )))
            }
            "if" => return special_if(&items[1..], ctx),
            "let" => return special_let(&items[1..], ctx),
            "do" => return special_do(&items[1..], ctx),
            "def" => return special_def(&items[1..], ctx),
            "defmacro" => return special_defmacro(&items[1..], ctx),
            "macroexpand" => return special_macroexpand(&items[1..], ctx),
            "and" => return special_and(&items[1..], ctx),
            "or" => return special_or(&items[1..], ctx),
            "when" => return special_when(&items[1..], ctx),
            "include" => return io_include(&items[1..], ctx),
            "read-file" => return io_read_file(&items[1..], ctx),
            "list-files" => return io_list_files(&items[1..], ctx),
            "glob" => return io_glob(&items[1..], ctx),
            "use" => return special_use(&items[1..], ctx),
            "fn" => return special_fn(&items[1..], ctx),
            "defn" => return special_defn(&items[1..], ctx),
            "run-activity" | "run-subprocess" | "sleep-until" => {
                return Err(EvalError::InvalidForm(format!(
                    "`{}` is only valid inside a Gearwork workflow",
                    name
                )))
            }
            _ => {}
        }

        // User-defined macro? Expand and evaluate the expansion.
        if ctx.macros.contains_key(name) {
            if ctx.macro_depth >= ctx.max_macro_expansion_depth {
                return Err(EvalError::InvalidForm(format!(
                    "macro expansion depth exceeded while expanding `{}`",
                    name
                )));
            }
            ctx.macro_depth += 1;
            let result = (|| {
                let expanded = expand_macro_call(name, &items[1..], ctx, 0)?;
                eval(&expanded, ctx)
            })();
            ctx.macro_depth -= 1;
            return result;
        }
    }

    // Otherwise: evaluate head + args and dispatch.
    let head_val = match head {
        // A bare symbol resolves through dispatch_call, which checks host
        // builtins, context modules, the inventory registry, and finally
        // any scope-bound function value with that name.
        Expr::Symbol(name) => {
            return dispatch_call(name, &items[1..], ctx);
        }
        other => eval(other, ctx)?,
    };

    // First-class call: the head expression evaluated to a function value
    // (e.g. `((fn [x] (* x 2)) 5)` or `((get fns 0) 5)`).
    if let Some(handle) = fn_handle(&head_val) {
        let args = eval_args(&items[1..], ctx)?;
        return call_closure(handle, &args, ctx);
    }

    Err(EvalError::NotCallable(format!("{:?}", head_val)))
}

/// 1-arg `(spintax pat)` overload: parses the template with the shared
/// per-context RNG seed, so it draws from the same stream as `rand`,
/// `choose`, `dice-roll`, etc.
fn spintax_with_context_seed(arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    let v = eval(&arg_exprs[0], ctx)?;
    let pat = v.as_str().ok_or_else(|| {
        EvalError::from(BuiltinError::TypeError {
            function: "spintax".into(),
            expected: "string".into(),
            got: trongoscript_stdlib::type_name(&v).into(),
        })
    })?;
    let out = trongoscript_stdlib::extra_string::spintax_with(pat, &mut ctx.rng_state);
    Ok(Value::String(out))
}

fn dispatch_call(name: &str, arg_exprs: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    // 1. Host builtin (gets a Context reference).
    if let Some(host) = ctx.host_builtins.get(name).cloned() {
        let args = eval_args(arg_exprs, ctx)?;
        return host(ctx, &args).map_err(EvalError::from);
    }

    // 1a. Context-bound `rng` module functions. Run *before* the
    // inventory lookup so the 1-arg `(spintax pat)` overload below can
    // use the shared seed even when the `strings` module is also enabled.
    if let Some(r) = crate::rng::try_dispatch(name, arg_exprs, ctx) {
        return r;
    }

    // 1b. 1-arg `(spintax pat)` overload that uses the context seed,
    // available whenever the `rng` module is enabled. The 2-arg form
    // with an explicit seed continues to dispatch through the stdlib
    // `strings` module as usual.
    if name == "spintax"
        && arg_exprs.len() == 1
        && ctx.enabled_modules.contains("rng")
    {
        return spintax_with_context_seed(arg_exprs, ctx);
    }

    // 2. Inventory builtin (pure: just args → result). Module-tagged
    // builtins are only visible if the caller has done `(use <module>)`.
    let lookup = {
        // Borrow the enabled set as an `&HashSet<String>` and check via
        // a tiny closure; this avoids cloning the set per lookup.
        let enabled = &ctx.enabled_modules;
        find_builtin_for(name, |m| enabled.contains(m))
    };
    if let Some(b) = lookup {
        let args = eval_args(arg_exprs, ctx)?;
        if args.len() < b.min_args || args.len() > b.max_args {
            let expected = if b.min_args == b.max_args {
                b.min_args.to_string()
            } else if b.max_args == usize::MAX {
                format!(">={}", b.min_args)
            } else {
                format!("{}-{}", b.min_args, b.max_args)
            };
            return Err(EvalError::Arity {
                function: name.to_string(),
                expected,
                actual: args.len(),
            });
        }
        return (b.func)(&args).map_err(EvalError::from);
    }

    // 2a. Evaluator-level higher-order functions (need Context to invoke
    // closure values, so they cannot live in the pure-stdlib registry).
    if let Some(r) = crate::higher_order::try_dispatch(name, arg_exprs, ctx) {
        return r;
    }

    // 3. A bound variable: if it holds a function handle, call it;
    // otherwise it isn't callable.
    if let Some(v) = ctx.lookup(name).cloned() {
        if let Some(handle) = fn_handle(&v) {
            let args = eval_args(arg_exprs, ctx)?;
            return call_closure(handle, &args, ctx);
        }
        return Err(EvalError::NotCallable(name.to_string()));
    }

    Err(EvalError::UndefinedFunction(name.to_string()))
}

pub(crate) fn eval_args(args: &[Expr], ctx: &mut Context) -> EvalResult<Vec<Value>> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        out.push(eval(a, ctx)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Special forms
// ---------------------------------------------------------------------------

fn special_quote(args: &[Expr]) -> EvalResult<Value> {
    if args.len() != 1 {
        return Err(EvalError::Arity {
            function: "quote".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    Ok(quote_to_value(&args[0]))
}

/// `quote` returns the literal data shape of the form, without evaluation.
/// Symbols and keywords both become strings (mirroring `:foo`'s evaluation).
fn quote_to_value(e: &Expr) -> Value {
    match e {
        Expr::Nil => Value::Null,
        Expr::Bool(b) => Value::Bool(*b),
        Expr::Number(n) => crate::value::num_from_f64(*n),
        Expr::String(s) | Expr::Keyword(s) | Expr::Symbol(s) => Value::String(s.clone()),
        Expr::Vector(items) | Expr::List(items) => {
            Value::Array(items.iter().map(quote_to_value).collect())
        }
        Expr::Map(pairs) => {
            let mut m = serde_json::Map::new();
            for (k, v) in pairs {
                let key = match k {
                    Expr::String(s) | Expr::Keyword(s) | Expr::Symbol(s) => s.clone(),
                    other => format!("{}", other),
                };
                m.insert(key, quote_to_value(v));
            }
            Value::Object(m)
        }
    }
}

fn special_if(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(EvalError::Arity {
            function: "if".into(),
            expected: "2 or 3".into(),
            actual: args.len(),
        });
    }
    let cond = eval(&args[0], ctx)?;
    if is_truthy(&cond) {
        eval(&args[1], ctx)
    } else if args.len() == 3 {
        eval(&args[2], ctx)
    } else {
        Ok(Value::Null)
    }
}

fn special_when(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    if args.is_empty() {
        return Err(EvalError::Arity {
            function: "when".into(),
            expected: ">=1".into(),
            actual: 0,
        });
    }
    let cond = eval(&args[0], ctx)?;
    if !is_truthy(&cond) {
        return Ok(Value::Null);
    }
    let mut last = Value::Null;
    for e in &args[1..] {
        last = eval(e, ctx)?;
    }
    Ok(last)
}

/// `(let [n1 v1 n2 v2 ...] body...)`
fn special_let(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    let Some((bindings_expr, body)) = args.split_first() else {
        return Err(EvalError::InvalidForm(
            "let requires a bindings vector and a body".into(),
        ));
    };
    let bindings = match bindings_expr {
        Expr::Vector(items) => items,
        _ => {
            return Err(EvalError::InvalidForm(
                "let bindings must be a vector".into(),
            ))
        }
    };
    if bindings.len() % 2 != 0 {
        return Err(EvalError::InvalidForm(
            "let bindings must have an even number of forms".into(),
        ));
    }

    ctx.push_scope();
    let result: EvalResult<Value> = (|| {
        let mut i = 0;
        while i < bindings.len() {
            let name = match &bindings[i] {
                Expr::Symbol(s) => s.clone(),
                other => {
                    return Err(EvalError::InvalidForm(format!(
                        "let binding name must be a symbol, got {:?}",
                        other
                    )))
                }
            };
            let value = eval(&bindings[i + 1], ctx)?;
            ctx.bind(&name, value);
            i += 2;
        }
        let mut last = Value::Null;
        for e in body {
            last = eval(e, ctx)?;
        }
        Ok(last)
    })();
    ctx.pop_scope();
    result
}

fn special_do(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    let mut last = Value::Null;
    for a in args {
        last = eval(a, ctx)?;
    }
    Ok(last)
}

fn special_def(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    if args.len() != 2 {
        return Err(EvalError::Arity {
            function: "def".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let name = match &args[0] {
        Expr::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::InvalidForm(format!(
                "def name must be a symbol, got {:?}",
                other
            )))
        }
    };
    let value = eval(&args[1], ctx)?;
    ctx.bind_global(&name, value.clone());
    Ok(value)
}

// ---------------------------------------------------------------------------
// First-class functions
//
// `(fn [params...] body...)` evaluates to a function-handle Value. The
// closure captures the *lexical* scope frames live at fn-creation time
// (everything above the global frame), so a `let`-bound variable
// referenced inside the body keeps its value even after the binding has
// fallen out of scope at the call site.
//
// `(defn name [params...] body...)` is sugar for `(def name (fn [...] body))`.
// ---------------------------------------------------------------------------

fn parse_param_list(params_vec: &[Expr]) -> EvalResult<(Vec<String>, Option<String>)> {
    let mut params = Vec::new();
    let mut rest_param = None;
    let mut iter = params_vec.iter();
    while let Some(item) = iter.next() {
        match item {
            Expr::Symbol(s) if s == "&" => {
                let rest = iter.next().ok_or_else(|| {
                    EvalError::InvalidForm("expected name after `&` in parameter list".into())
                })?;
                match rest {
                    Expr::Symbol(rs) => rest_param = Some(rs.clone()),
                    other => {
                        return Err(EvalError::InvalidForm(format!(
                            "`& rest` parameter must be a symbol, got {:?}",
                            other
                        )))
                    }
                }
                if iter.next().is_some() {
                    return Err(EvalError::InvalidForm(
                        "no parameters allowed after `& rest`".into(),
                    ));
                }
            }
            Expr::Symbol(s) => params.push(s.clone()),
            other => {
                return Err(EvalError::InvalidForm(format!(
                    "fn parameter must be a symbol, got {:?}",
                    other
                )))
            }
        }
    }
    Ok((params, rest_param))
}

fn special_fn(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm(
            "fn requires a parameter vector and a body".into(),
        ));
    }
    let params_vec = match &args[0] {
        Expr::Vector(items) => items,
        _ => {
            return Err(EvalError::InvalidForm(
                "fn parameters must be a vector".into(),
            ))
        }
    };
    let (params, rest_param) = parse_param_list(params_vec)?;
    // Capture lexical frames *above* the global frame. The global frame
    // is always read live, so def's installed later are visible.
    let captured_lexical: Vec<HashMap<String, Value>> = if ctx.scopes.len() > 1 {
        ctx.scopes[1..].to_vec()
    } else {
        Vec::new()
    };
    let closure = Closure {
        params,
        rest_param,
        body: args[1..].to_vec(),
        captured_lexical,
    };
    let handle = ctx.fns.len();
    ctx.fns.push(closure);
    Ok(make_fn_value(handle))
}

fn special_defn(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    if args.len() < 3 {
        return Err(EvalError::InvalidForm(
            "defn requires a name, a parameter vector, and a body".into(),
        ));
    }
    let name = match &args[0] {
        Expr::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::InvalidForm(format!(
                "defn name must be a symbol, got {:?}",
                other
            )))
        }
    };
    let fn_value = special_fn(&args[1..], ctx)?;
    ctx.bind_global(&name, fn_value.clone());
    Ok(fn_value)
}

/// Invoke a previously-registered closure with already-evaluated args.
/// Globals stay live (so the closure sees `def`s installed after its
/// creation), while lexical frames are restored from the snapshot taken
/// at `(fn ...)` time.
pub(crate) fn call_closure(handle: usize, args: &[Value], ctx: &mut Context) -> EvalResult<Value> {
    let closure = ctx
        .fns
        .get(handle)
        .cloned()
        .ok_or_else(|| EvalError::InvalidForm(format!("invalid function handle {}", handle)))?;

    let fixed = closure.params.len();
    if closure.rest_param.is_none() && args.len() != fixed {
        return Err(EvalError::Arity {
            function: "fn".into(),
            expected: fixed.to_string(),
            actual: args.len(),
        });
    }
    if closure.rest_param.is_some() && args.len() < fixed {
        return Err(EvalError::Arity {
            function: "fn".into(),
            expected: format!(">={}", fixed),
            actual: args.len(),
        });
    }

    // Swap the caller's lexical frames out for the closure's captured
    // ones, plus a fresh arg frame. Globals (scopes[0]) stay live.
    let saved_lexical = ctx.scopes.split_off(1);
    ctx.scopes.extend(closure.captured_lexical.iter().cloned());
    let mut arg_frame: HashMap<String, Value> = HashMap::new();
    for (i, p) in closure.params.iter().enumerate() {
        arg_frame.insert(p.clone(), args[i].clone());
    }
    if let Some(rest_name) = closure.rest_param.as_ref() {
        let rest: Vec<Value> = args[fixed..].to_vec();
        arg_frame.insert(rest_name.clone(), Value::Array(rest));
    }
    ctx.scopes.push(arg_frame);

    let result: EvalResult<Value> = (|| {
        let mut last = Value::Null;
        for e in &closure.body {
            last = eval(e, ctx)?;
        }
        Ok(last)
    })();

    // Restore.
    ctx.scopes.truncate(1);
    ctx.scopes.extend(saved_lexical);
    result
}

pub(crate) fn coerce_to_fn(fname: &str, v: &Value) -> EvalResult<usize> {
    fn_handle(v).ok_or_else(|| EvalError::Type {
        expected: format!("function (1st arg to `{}`)", fname),
        got: type_name_v(v).into(),
    })
}

pub(crate) fn coerce_to_list(fname: &str, v: &Value) -> EvalResult<Vec<Value>> {
    match v {
        Value::Array(items) => Ok(items.clone()),
        other => Err(EvalError::Type {
            expected: format!("list (2nd arg to `{}`)", fname),
            got: type_name_v(other).into(),
        }),
    }
}

// Higher-order builtins (`map`, `filter`, `reduce`, plus the CL-style
// family) live in [`crate::higher_order`].

fn special_and(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    let mut last = Value::Bool(true);
    for a in args {
        last = eval(a, ctx)?;
        if !is_truthy(&last) {
            return Ok(last);
        }
    }
    Ok(last)
}

fn special_or(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    let mut last = Value::Null;
    for a in args {
        last = eval(a, ctx)?;
        if is_truthy(&last) {
            return Ok(last);
        }
    }
    Ok(last)
}

/// `(use module-name ...)` — enable one or more stdlib modules.
///
/// Accepted forms for each module-name argument:
/// - bare symbol: `(use math)`
/// - quoted symbol: `(use 'math)` (parser sugar for `(quote math)`)
/// - keyword: `(use :math)`
/// - string literal: `(use "math")`
///
/// Each name is looked up in the static module registry; unknown names
/// raise an error. For modules with a Trongoscript-side source (see
/// [`crate::modules`]), the source is evaluated exactly once per Context.
fn special_use(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    if args.is_empty() {
        return Err(EvalError::Arity {
            function: "use".into(),
            expected: ">=1".into(),
            actual: 0,
        });
    }
    for a in args {
        let name = use_arg_to_name(a)?;
        if !is_known_module(&name) {
            return Err(EvalError::InvalidForm(format!(
                "unknown module `{}` (known: {})",
                name,
                crate::modules::all_module_names().join(", ")
            )));
        }
        ctx.enabled_modules.insert(name.clone());
        if !ctx.loaded_ts_modules.contains(&name) {
            if let Some(src) = ts_module_source(&name) {
                // Mark as loaded *before* evaluation so a circular
                // (use foo) inside the source doesn't recurse forever.
                ctx.loaded_ts_modules.insert(name.clone());
                eval_source(src, ctx)?;
            }
        }
    }
    Ok(Value::Null)
}

fn use_arg_to_name(a: &Expr) -> EvalResult<String> {
    match a {
        Expr::Symbol(s) => Ok(s.clone()),
        Expr::String(s) | Expr::Keyword(s) => Ok(s.clone()),
        // `(quote sym)` — what `'sym` parses to.
        Expr::List(items)
            if items.len() == 2
                && matches!(items.first(), Some(Expr::Symbol(s)) if s == "quote") =>
        {
            match &items[1] {
                Expr::Symbol(s) | Expr::String(s) | Expr::Keyword(s) => Ok(s.clone()),
                other => Err(EvalError::InvalidForm(format!(
                    "`use` expects a module name, got {:?}",
                    other
                ))),
            }
        }
        other => Err(EvalError::InvalidForm(format!(
            "`use` expects a module name (symbol, keyword, string, or 'sym), got {:?}",
            other
        ))),
    }
}

// ---------------------------------------------------------------------------
// defmacro and macro expansion
//
// Macros are template-based and expand at evaluation time. The macro body
// is an expression tree in which:
//   - `(quasiquote x)` (also `` `x ``) freezes its argument as data;
//   - `(unquote y)` (`~y`) inside a quasiquote evaluates `y` in the
//     macro's lexical scope (with parameters bound to their unevaluated
//     argument forms-converted-to-data) and splices the result in;
//   - `(unquote-splicing ys)` (`~@ys`) does the same but expects a list
//     and splices its elements into the surrounding list/vector.
//
// Macro parameters that appear *outside* a quasiquote (i.e. bare in the
// body) are substituted with their argument *form* (as data). This makes
// the common case ergonomic:
//
//   (defmacro unless [c body] (if c nil body))    ; no backtick needed
//
// expands `(unless cond expr)` to `(if cond nil expr)`.
//
// A `& rest` parameter collects trailing argument forms into a list value.
// ---------------------------------------------------------------------------

fn special_defmacro(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    if args.len() < 3 {
        return Err(EvalError::InvalidForm(
            "defmacro requires a name, a parameter vector, and a body".into(),
        ));
    }
    let name = match &args[0] {
        Expr::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::InvalidForm(format!(
                "defmacro name must be a symbol, got {:?}",
                other
            )))
        }
    };
    let params_vec = match &args[1] {
        Expr::Vector(items) => items,
        _ => {
            return Err(EvalError::InvalidForm(
                "defmacro parameters must be a vector".into(),
            ))
        }
    };
    let mut params = Vec::new();
    let mut rest_param = None;
    let mut iter = params_vec.iter();
    while let Some(item) = iter.next() {
        match item {
            Expr::Symbol(s) if s == "&" => {
                let rest = iter.next().ok_or_else(|| {
                    EvalError::InvalidForm("expected name after `&` in parameter list".into())
                })?;
                match rest {
                    Expr::Symbol(rs) => {
                        rest_param = Some(rs.clone());
                    }
                    other => {
                        return Err(EvalError::InvalidForm(format!(
                            "`& rest` parameter must be a symbol, got {:?}",
                            other
                        )))
                    }
                }
                if iter.next().is_some() {
                    return Err(EvalError::InvalidForm(
                        "no parameters allowed after `& rest`".into(),
                    ));
                }
            }
            Expr::Symbol(s) => params.push(s.clone()),
            other => {
                return Err(EvalError::InvalidForm(format!(
                    "macro parameter must be a symbol, got {:?}",
                    other
                )))
            }
        }
    }
    let body: Vec<Expr> = args[2..].to_vec();
    ctx.macros.insert(
        name.clone(),
        MacroDef {
            name: name.clone(),
            params,
            rest_param,
            body,
        },
    );
    Ok(Value::String(name))
}

fn special_macroexpand(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    if args.len() != 1 {
        return Err(EvalError::Arity {
            function: "macroexpand".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    // The argument is a *quoted* form: typically `(macroexpand '(my-macro x y))`.
    let target = match &args[0] {
        Expr::List(items)
            if matches!(items.first(), Some(Expr::Symbol(s)) if s == "quote") && items.len() == 2 =>
        {
            items[1].clone()
        }
        other => other.clone(),
    };
    let expanded = expand_once(&target, ctx, 0)?;
    Ok(quote_to_value(&expanded))
}

/// Expand a macro call `(macro-name args...)` to its body expression.
fn expand_macro_call(
    name: &str,
    args: &[Expr],
    ctx: &mut Context,
    depth: usize,
) -> EvalResult<Expr> {
    if depth >= ctx.max_macro_expansion_depth {
        return Err(EvalError::InvalidForm(format!(
            "macro expansion depth exceeded while expanding `{}`",
            name
        )));
    }
    let mac = ctx
        .macros
        .get(name)
        .ok_or_else(|| EvalError::UndefinedFunction(name.to_string()))?
        .clone();

    // Validate arity.
    let fixed = mac.params.len();
    if mac.rest_param.is_none() && args.len() != fixed {
        return Err(EvalError::Arity {
            function: name.to_string(),
            expected: fixed.to_string(),
            actual: args.len(),
        });
    }
    if mac.rest_param.is_some() && args.len() < fixed {
        return Err(EvalError::Arity {
            function: name.to_string(),
            expected: format!(">={}", fixed),
            actual: args.len(),
        });
    }

    // Build substitution map: param-name → argument *form*.
    let mut subs: HashMap<String, Expr> = HashMap::new();
    for (i, p) in mac.params.iter().enumerate() {
        subs.insert(p.clone(), args[i].clone());
    }
    if let Some(rest) = mac.rest_param.as_ref() {
        let rest_args = args[fixed..].to_vec();
        subs.insert(rest.clone(), Expr::Vector(rest_args));
    }

    // Macro body: a sequence of forms. The result is the last form's
    // expansion, wrapped in `(do ...)` if there are multiple forms.
    let expanded_forms: Vec<Expr> = mac
        .body
        .iter()
        .map(|f| substitute(f, &subs, ctx, depth))
        .collect::<EvalResult<_>>()?;

    let expanded = if expanded_forms.len() == 1 {
        expanded_forms.into_iter().next().unwrap()
    } else {
        let mut all = Vec::with_capacity(expanded_forms.len() + 1);
        all.push(Expr::Symbol("do".to_string()));
        all.extend(expanded_forms);
        Expr::List(all)
    };
    Ok(expanded)
}

/// Public macro-expansion entry point: expand the head of `form` once if
/// it's a macro call, otherwise return it unchanged.
fn expand_once(form: &Expr, ctx: &mut Context, depth: usize) -> EvalResult<Expr> {
    if let Expr::List(items) = form {
        if let Some(Expr::Symbol(name)) = items.first() {
            if ctx.macros.contains_key(name) {
                return expand_macro_call(name, &items[1..], ctx, depth);
            }
        }
    }
    Ok(form.clone())
}

/// Walk a macro body Expr, substituting parameter symbols and resolving
/// any embedded `(quasiquote ...)` blocks. Outside of a quasiquote,
/// parameter symbols are replaced with their unevaluated argument form.
fn substitute(
    form: &Expr,
    subs: &HashMap<String, Expr>,
    ctx: &mut Context,
    depth: usize,
) -> EvalResult<Expr> {
    match form {
        Expr::Symbol(s) => {
            if let Some(repl) = subs.get(s) {
                Ok(repl.clone())
            } else {
                Ok(form.clone())
            }
        }
        Expr::List(items) => {
            // (quasiquote x) → evaluate quasiquote here.
            if items.len() == 2 {
                if let Expr::Symbol(head) = &items[0] {
                    if head == "quasiquote" {
                        return quasi_walk(&items[1], subs, ctx, 1, depth);
                    }
                }
            }
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(substitute(it, subs, ctx, depth)?);
            }
            Ok(Expr::List(out))
        }
        Expr::Vector(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(substitute(it, subs, ctx, depth)?);
            }
            Ok(Expr::Vector(out))
        }
        Expr::Map(pairs) => {
            let mut out = Vec::with_capacity(pairs.len());
            for (k, v) in pairs {
                out.push((substitute(k, subs, ctx, depth)?, substitute(v, subs, ctx, depth)?));
            }
            Ok(Expr::Map(out))
        }
        atom => Ok(atom.clone()),
    }
}

/// Walk inside a `(quasiquote ...)` template. Inside, everything is kept
/// literal except `(unquote x)` and `(unquote-splicing x)` which evaluate
/// their argument *with* parameter substitution applied first.
fn quasi_walk(
    form: &Expr,
    subs: &HashMap<String, Expr>,
    ctx: &mut Context,
    qq_depth: usize,
    rec_depth: usize,
) -> EvalResult<Expr> {
    match form {
        Expr::List(items) => {
            // Recognize unquote / unquote-splicing / nested quasiquote.
            if items.len() == 2 {
                if let Expr::Symbol(head) = &items[0] {
                    match (head.as_str(), qq_depth) {
                        ("unquote", 1) => {
                            // Substitute params then evaluate; the result
                            // (a Value) is converted back to an Expr.
                            let sub = substitute(&items[1], subs, ctx, rec_depth)?;
                            let v = eval(&sub, ctx)?;
                            return Ok(value_to_expr(&v));
                        }
                        ("unquote", d) if d > 1 => {
                            // Decrement qq depth — still quoted.
                            return Ok(Expr::List(vec![
                                Expr::Symbol("unquote".into()),
                                quasi_walk(&items[1], subs, ctx, d - 1, rec_depth)?,
                            ]));
                        }
                        ("quasiquote", d) => {
                            return Ok(Expr::List(vec![
                                Expr::Symbol("quasiquote".into()),
                                quasi_walk(&items[1], subs, ctx, d + 1, rec_depth)?,
                            ]));
                        }
                        _ => {}
                    }
                }
            }
            // Walk list, handling unquote-splicing at level qq_depth==1.
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                if let Some(spliced) = try_splice(it, subs, ctx, qq_depth, rec_depth)? {
                    out.extend(spliced);
                } else {
                    out.push(quasi_walk(it, subs, ctx, qq_depth, rec_depth)?);
                }
            }
            Ok(Expr::List(out))
        }
        Expr::Vector(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                if let Some(spliced) = try_splice(it, subs, ctx, qq_depth, rec_depth)? {
                    out.extend(spliced);
                } else {
                    out.push(quasi_walk(it, subs, ctx, qq_depth, rec_depth)?);
                }
            }
            Ok(Expr::Vector(out))
        }
        Expr::Map(pairs) => {
            let mut out = Vec::with_capacity(pairs.len());
            for (k, v) in pairs {
                out.push((
                    quasi_walk(k, subs, ctx, qq_depth, rec_depth)?,
                    quasi_walk(v, subs, ctx, qq_depth, rec_depth)?,
                ));
            }
            Ok(Expr::Map(out))
        }
        // Atoms (incl. symbols) inside quasiquote are literal. Parameter
        // substitution still applies because users expect e.g.
        // `` `(if ~cond ...) `` where `cond` is unquoted — but a bare
        // `cond` in the template is literal symbol "cond".
        atom => Ok(atom.clone()),
    }
}

/// If `form` is `(unquote-splicing x)` at the active quasiquote depth,
/// evaluate `x` (which must yield a list) and return the splice items.
fn try_splice(
    form: &Expr,
    subs: &HashMap<String, Expr>,
    ctx: &mut Context,
    qq_depth: usize,
    rec_depth: usize,
) -> EvalResult<Option<Vec<Expr>>> {
    if let Expr::List(items) = form {
        if items.len() == 2 {
            if let Expr::Symbol(head) = &items[0] {
                if head == "unquote-splicing" && qq_depth == 1 {
                    let sub = substitute(&items[1], subs, ctx, rec_depth)?;
                    let v = eval(&sub, ctx)?;
                    match v {
                        Value::Array(arr) => {
                            return Ok(Some(arr.iter().map(value_to_expr).collect()));
                        }
                        other => {
                            return Err(EvalError::Type {
                                expected: "list (for ~@ splicing)".into(),
                                got: type_name_v(&other).into(),
                            })
                        }
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Inverse of [`quote_to_value`]: convert a JSON-like Value back to an
/// Expr. Lists become `Expr::Vector` (since we can't tell apart a quoted
/// list of forms from a literal list value at this layer); strings stay
/// strings; the only "code-shaped" round trip we preserve via a sentinel
/// is for symbols, which we'd need a tagged representation for. We treat
/// the result as a *data literal expression*: numbers, bools, nil,
/// strings, vectors, maps.
pub fn value_to_expr(v: &Value) -> Expr {
    match v {
        Value::Null => Expr::Nil,
        Value::Bool(b) => Expr::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Expr::Number(i as f64)
            } else if let Some(f) = n.as_f64() {
                Expr::Number(f)
            } else {
                Expr::Nil
            }
        }
        Value::String(s) => Expr::String(s.clone()),
        Value::Array(items) => Expr::Vector(items.iter().map(value_to_expr).collect()),
        Value::Object(map) => {
            let pairs = map
                .iter()
                .map(|(k, v)| (Expr::String(k.clone()), value_to_expr(v)))
                .collect();
            Expr::Map(pairs)
        }
    }
}

fn special_quasiquote(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    // `(quasiquote x)` at value-position (not inside a macro body) just
    // produces the data shape of `x`, with `(unquote y)` evaluated and
    // `(unquote-splicing y)` spliced.
    if args.len() != 1 {
        return Err(EvalError::Arity {
            function: "quasiquote".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let empty = HashMap::new();
    let expanded = quasi_walk(&args[0], &empty, ctx, 1, 0)?;
    Ok(quote_to_value(&expanded))
}

// ---------------------------------------------------------------------------
// Sandboxed I/O builtins.
//
// These are *not* in the inventory registry because they need access to
// the live `Context` (sandbox + include stack + current file dir).
// ---------------------------------------------------------------------------

fn require_sandbox(ctx: &Context) -> EvalResult<&Sandbox> {
    ctx.sandbox.as_ref().ok_or(EvalError::NoSandbox)
}

fn one_string_arg(name: &str, args: &[Expr], ctx: &mut Context) -> EvalResult<String> {
    if args.len() != 1 {
        return Err(EvalError::Arity {
            function: name.into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let v = eval(&args[0], ctx)?;
    match v {
        Value::String(s) => Ok(s),
        other => Err(EvalError::Type {
            expected: "string".into(),
            got: type_name_v(&other).into(),
        }),
    }
}

pub(crate) fn type_name_v(v: &Value) -> &'static str {
    match v {
        Value::Null => "nil",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "list",
        Value::Object(_) => "map",
    }
}

fn io_include(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    let path = one_string_arg("include", args, ctx)?;
    let sb = require_sandbox(ctx)?.clone();
    let max_depth = sb.limits().max_include_depth;

    let resolved = sb.resolve(ctx.current_file_dir.as_deref(), &path)?;

    // Manual push/pop instead of RAII so we don't hold a mutable borrow of
    // `ctx.includes` while we also need `ctx` for evaluation.
    if ctx.includes.stack.len() >= max_depth {
        return Err(EvalError::Sandbox(
            crate::sandbox::SandboxError::IncludeTooDeep { max: max_depth },
        ));
    }
    if !ctx.includes.visited.insert(resolved.clone()) {
        return Err(EvalError::Sandbox(
            crate::sandbox::SandboxError::IncludeCycle(resolved.display().to_string()),
        ));
    }
    ctx.includes.stack.push(resolved.clone());

    let source = sb.read_text(ctx.current_file_dir.as_deref(), &path);

    let prev_dir = ctx.current_file_dir.take();
    ctx.current_file_dir = resolved.parent().map(|p| p.to_path_buf());

    let result = source.map_err(EvalError::from).and_then(|s| eval_source(&s, ctx));

    ctx.current_file_dir = prev_dir;
    ctx.includes.stack.pop();
    ctx.includes.visited.remove(&resolved);
    result
}

fn io_read_file(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    let path = one_string_arg("read-file", args, ctx)?;
    let sb = require_sandbox(ctx)?.clone();
    let text = sb.read_text(ctx.current_file_dir.as_deref(), &path)?;
    Ok(Value::String(text))
}

fn io_list_files(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    let path = one_string_arg("list-files", args, ctx)?;
    let sb = require_sandbox(ctx)?.clone();
    let entries = sb.list(ctx.current_file_dir.as_deref(), &path)?;
    let arr = entries
        .into_iter()
        .map(|e| {
            let mut m = serde_json::Map::new();
            m.insert("name".into(), Value::String(e.name));
            m.insert("dir?".into(), Value::Bool(e.is_dir));
            Value::Object(m)
        })
        .collect();
    Ok(Value::Array(arr))
}

/// `(glob "pattern")` — list paths matching a glob, anchored at the
/// current file's directory (or sandbox root). Requires a sandbox.
fn io_glob(args: &[Expr], ctx: &mut Context) -> EvalResult<Value> {
    let pattern = one_string_arg("glob", args, ctx)?;
    let sb = require_sandbox(ctx)?.clone();
    let matches = sb.glob(ctx.current_file_dir.as_deref(), &pattern)?;
    let arr = matches
        .into_iter()
        .map(|m| Value::String(m.path))
        .collect();
    Ok(Value::Array(arr))
}

// Re-export for tests so the host can construct a BuiltinError if needed.
#[allow(dead_code)]
fn _re_export_builtin_error(_: BuiltinError) {}

// ---------------------------------------------------------------------------
// Partial evaluation (binding-time analysis)
// ---------------------------------------------------------------------------

/// Result of partially evaluating an expression.
///
/// `Done` means the expression reduced to a value. `Suspended` means it
/// contained at least one opaque form and cannot be fully reduced yet — the
/// payload is the residual expression (pure subexpressions already folded).
pub enum Partial {
    Done(Value),
    Suspended(Expr),
}

/// Partially evaluate `expr`, reducing all pure subexpressions eagerly while
/// leaving forms whose head symbol appears in `opaque_forms` suspended.
///
/// Rules:
/// - Atoms and forms with no opaque subexpressions reduce normally via `eval`.
/// - A `List` whose head symbol is in `opaque_forms` returns
///   `Suspended(expr.clone())` without evaluating arguments.
/// - A `List` containing any suspended child returns `Suspended` with pure
///   children already folded to their `value_to_expr` literals.
/// - `if`: the condition is partially evaluated first. If it suspends, the
///   whole `if` is `Suspended`. Otherwise only the live branch is reduced.
/// - `Vector` and `Map` literals: each element is partially evaluated; if
///   any suspends the whole literal is `Suspended` (with pure elements folded).
pub fn partial_eval(
    expr: &Expr,
    ctx: &mut Context,
    opaque_forms: &HashSet<String>,
) -> EvalResult<Partial> {
    match expr {
        // Atoms and self-evaluating forms: reduce fully.
        Expr::Nil | Expr::Bool(_) | Expr::Number(_) | Expr::String(_) | Expr::Keyword(_) => {
            eval(expr, ctx).map(Partial::Done)
        }

        // Symbol: resolve; if undefined this is an error, not a suspension.
        Expr::Symbol(_) => eval(expr, ctx).map(Partial::Done),

        // Vector literal: partially eval each element.
        Expr::Vector(items) => {
            let mut reduced: Vec<Expr> = Vec::with_capacity(items.len());
            let mut any_suspended = false;
            for item in items {
                match partial_eval(item, ctx, opaque_forms)? {
                    Partial::Done(v) => reduced.push(value_to_expr(&v)),
                    Partial::Suspended(e) => {
                        any_suspended = true;
                        reduced.push(e);
                    }
                }
            }
            if any_suspended {
                Ok(Partial::Suspended(Expr::Vector(reduced)))
            } else {
                // All done — evaluate the folded vector to get the Value.
                eval(&Expr::Vector(reduced), ctx).map(Partial::Done)
            }
        }

        // Map literal: partially eval each key and value.
        Expr::Map(pairs) => {
            let mut reduced: Vec<(Expr, Expr)> = Vec::with_capacity(pairs.len());
            let mut any_suspended = false;
            for (k, v) in pairs {
                let rk = match partial_eval(k, ctx, opaque_forms)? {
                    Partial::Done(val) => value_to_expr(&val),
                    Partial::Suspended(e) => {
                        any_suspended = true;
                        e
                    }
                };
                let rv = match partial_eval(v, ctx, opaque_forms)? {
                    Partial::Done(val) => value_to_expr(&val),
                    Partial::Suspended(e) => {
                        any_suspended = true;
                        e
                    }
                };
                reduced.push((rk, rv));
            }
            if any_suspended {
                Ok(Partial::Suspended(Expr::Map(reduced)))
            } else {
                eval(&Expr::Map(reduced), ctx).map(Partial::Done)
            }
        }

        // List form: the interesting case.
        Expr::List(items) => partial_eval_list(items, ctx, opaque_forms),
    }
}

fn partial_eval_list(
    items: &[Expr],
    ctx: &mut Context,
    opaque_forms: &HashSet<String>,
) -> EvalResult<Partial> {
    let Some(head) = items.first() else {
        // Empty list: fully reducible.
        return eval(&Expr::List(vec![]), ctx).map(Partial::Done);
    };

    if let Expr::Symbol(name) = head {
        // Opaque form: suspend immediately without touching arguments.
        if opaque_forms.contains(name.as_str()) {
            return Ok(Partial::Suspended(Expr::List(items.to_vec())));
        }

        // `if`: only reduce the live branch.
        if name == "if" {
            return partial_eval_if(&items[1..], ctx, opaque_forms);
        }

        // Special forms with binding structure that must not be decomposed
        // argument-by-argument (the binding vector contains unbound names, not
        // evaluable sub-expressions). Hand them straight to the normal evaluator;
        // if they contain an opaque sub-form they will error, but these forms
        // cannot meaningfully be split across a suspension boundary anyway.
        if matches!(
            name.as_str(),
            "let" | "fn" | "defn" | "defmacro" | "macroexpand"
                | "quote" | "quasiquote" | "unquote" | "unquote-splicing"
                | "and" | "or" | "when" | "do" | "def"
                | "include" | "read-file" | "list-files" | "glob" | "use"
        ) {
            return eval(&Expr::List(items.to_vec()), ctx).map(Partial::Done);
        }
    }

    // For regular function calls (symbol head that is a builtin or user fn, or
    // a first-class expression head): keep the head as-is and partially eval
    // each argument. Evaluating a symbol head through partial_eval would try to
    // resolve it as a variable, which fails for builtins in the dispatch table.
    let mut reduced: Vec<Expr> = Vec::with_capacity(items.len());
    let mut any_suspended = false;

    for (i, item) in items.iter().enumerate() {
        if i == 0 && matches!(item, Expr::Symbol(_)) {
            // Symbol head: keep verbatim.
            reduced.push(item.clone());
            continue;
        }
        match partial_eval(item, ctx, opaque_forms)? {
            Partial::Done(v) => reduced.push(value_to_expr(&v)),
            Partial::Suspended(e) => {
                any_suspended = true;
                reduced.push(e);
            }
        }
    }

    if any_suspended {
        Ok(Partial::Suspended(Expr::List(reduced)))
    } else {
        eval(&Expr::List(items.to_vec()), ctx).map(Partial::Done)
    }
}

fn partial_eval_if(
    args: &[Expr],
    ctx: &mut Context,
    opaque_forms: &HashSet<String>,
) -> EvalResult<Partial> {
    if args.len() < 2 || args.len() > 3 {
        return Err(EvalError::Arity {
            function: "if".into(),
            expected: "2 or 3".into(),
            actual: args.len(),
        });
    }

    match partial_eval(&args[0], ctx, opaque_forms)? {
        Partial::Suspended(cond_expr) => {
            // Condition is dynamic — the whole if is suspended.
            // Partially reduce both branches so any nested pure work is folded.
            let then_expr = match partial_eval(&args[1], ctx, opaque_forms)? {
                Partial::Done(v) => value_to_expr(&v),
                Partial::Suspended(e) => e,
            };
            let else_expr = if args.len() == 3 {
                match partial_eval(&args[2], ctx, opaque_forms)? {
                    Partial::Done(v) => value_to_expr(&v),
                    Partial::Suspended(e) => e,
                }
            } else {
                Expr::Nil
            };
            let mut rebuilt = vec![Expr::Symbol("if".into()), cond_expr, then_expr];
            if args.len() == 3 {
                rebuilt.push(else_expr);
            }
            Ok(Partial::Suspended(Expr::List(rebuilt)))
        }
        Partial::Done(cond_val) => {
            // Condition is known — reduce only the live branch.
            if is_truthy(&cond_val) {
                partial_eval(&args[1], ctx, opaque_forms)
            } else if args.len() == 3 {
                partial_eval(&args[2], ctx, opaque_forms)
            } else {
                Ok(Partial::Done(Value::Null))
            }
        }
    }
}



#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;


// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.