//! A long-lived evaluation context and the thread arrangement it runs on.
//!
//! # Why evaluation leaves the calling thread
//!
//! The evaluator recurses on the native stack. A Ruby thread other than the
//! main one gets about 1 MiB of machine stack, and a debug build of the
//! evaluator overflows 8 MiB after a few hundred nested calls; an overflow
//! is a SIGSEGV that kills the whole host process, not an exception. So,
//! like the CLI, every evaluation runs on a thread with an explicitly large
//! stack, and the fuel limits' depth ceilings are sized for that stack.
//!
//! # Why callbacks come back
//!
//! Host functions (Ruby blocks, PHP callables) can only run on the thread
//! that owns the interpreter: Ruby requires the GVL holder, and PHP's engine
//! is not thread-aware at all. The evaluator thread therefore never calls
//! the host directly. A host builtin posts an [`Event::Call`] to the
//! calling thread, which is parked in [`Host::wait`], and blocks until the
//! reply arrives. The calling thread runs the callback, replies, and parks
//! again, until [`Event::Done`] says the evaluation has finished.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use trongoscript::{
    call_fn, eval, eval_source, evaluate_embedded_with, evaluate_embedded_yaml_with,
    render_template, value_to_expr, BuiltinError, Context, EvalConfig, Expr, Fuel, HostBuiltin,
    Value, DEFAULT_KEY, DEFAULT_PREFIX, DEFAULT_TAG,
};

use crate::depth::{drop_flat, exceeds_nesting, too_deep, MAX_NESTING};
use crate::error::{ErrorKind, HostError};
use crate::options::{validate_name, Options};

/// Stack sizes to try for the evaluator thread, largest first.
///
/// The same ladder the CLI uses, for the same reason: a thread's stack is
/// reserved address space, so 256 MiB fails under a tight `RLIMIT_AS`, and
/// a smaller stack is better than no evaluation at all. Only address space
/// is reserved; pages are committed as the recursion actually touches them.
const STACK_SIZES: [usize; 4] = [
    256 * 1024 * 1024,
    64 * 1024 * 1024,
    16 * 1024 * 1024,
    4 * 1024 * 1024,
];

/// What the calling thread is woken up for while an evaluation runs.
#[derive(Debug)]
pub enum Event {
    /// The script called a host-defined function.
    Call(CallRequest),
    /// Something outside the evaluation wants the calling thread's
    /// attention, e.g. Ruby delivering `Thread#raise` or Ctrl-C. See
    /// [`Waker`].
    Wake,
    /// The evaluator thread has finished, successfully or not.
    Done,
}

/// One host function invocation, waiting for its result.
#[derive(Debug)]
pub struct CallRequest {
    pub name: String,
    pub args: Vec<Value>,
    reply: SyncSender<Result<Value, String>>,
}

/// The calling thread's inbox for one evaluation.
#[derive(Debug)]
pub struct Events {
    rx: Receiver<Event>,
    tx: Sender<Event>,
}

impl Events {
    /// Block until the next event.
    pub fn recv(&self) -> Event {
        // `self.tx` keeps the channel open, so this cannot disconnect.
        self.rx.recv().unwrap_or(Event::Done)
    }

    /// Wait up to `timeout` for the next event.
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// A handle that can interrupt [`recv`](Self::recv) from any thread.
    pub fn waker(&self) -> Waker {
        Waker(self.tx.clone())
    }
}

/// Interrupts a [`Host::wait`] from another thread.
///
/// Ruby's unblock function is the intended user: when a thread blocked in
/// an evaluation receives an interrupt, Ruby calls it from whichever thread
/// raised the interrupt.
#[derive(Debug, Clone)]
pub struct Waker(Sender<Event>);

impl Waker {
    pub fn wake(&self) {
        // A send after the evaluation has finished has no one to wake.
        let _ = self.0.send(Event::Wake);
    }
}

/// The host language's side of an evaluation.
pub trait Host {
    /// Run the host function registered as `name`. An `Err` aborts the
    /// evaluation; its message becomes the evaluation error's message.
    fn call(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String>;

    /// Block until the evaluator thread has something for this thread.
    ///
    /// The default blocks plainly. Ruby overrides it to release the GVL
    /// while waiting, so other Ruby threads keep running.
    fn wait(&mut self, events: &Events) -> Event {
        events.recv()
    }
}

/// A [`Host`] with no functions, for evaluations that cannot call back.
pub struct NoHost;

impl Host for NoHost {
    fn call(&mut self, name: &str, _args: Vec<Value>) -> Result<Value, String> {
        Err(format!("no host function `{name}`"))
    }
}

/// Resource consumption of the most recent evaluation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub steps: u64,
    pub allocated: u64,
    pub peak_call_depth: usize,
    pub peak_eval_depth: usize,
}

/// Marker tokens for embedded mode. Defaults match the CLI's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Markers {
    /// Prefix that marks a string scalar as code, e.g. `"!trongo (+ 1 2)"`.
    pub prefix: String,
    /// Key of a single-key map whose value is code, e.g. `{"!trongo": "..."}`.
    pub key: String,
    /// YAML tag that marks a node as code, e.g. `!trongo (+ 1 2)`.
    pub tag: String,
}

impl Default for Markers {
    fn default() -> Self {
        Self {
            prefix: DEFAULT_PREFIX.into(),
            key: DEFAULT_KEY.into(),
            tag: DEFAULT_TAG.into(),
        }
    }
}

/// Where host builtins post their calls. Holds a sender only while an
/// evaluation is running.
type Link = Arc<Mutex<Option<Sender<Event>>>>;

/// A Trongoscript context that keeps its globals, functions, and macros
/// between evaluations.
pub struct Session {
    ctx: Context,
    link: Link,
    usage: Usage,
}

impl Session {
    pub fn new(options: Options) -> Result<Self, HostError> {
        let (config, globals) = options.into_config()?;
        let mut ctx = Context::new(config);
        ctx.globals_mut().extend(globals);
        Ok(Self {
            ctx,
            link: Arc::new(Mutex::new(None)),
            usage: Usage::default(),
        })
    }

    /// Make `name` callable from scripts. Calls are routed to
    /// [`Host::call`] on whichever thread is running the evaluation.
    ///
    /// Host functions shadow builtins of the same name, so a host can
    /// replace one if it needs to.
    pub fn define(&mut self, name: &str) -> Result<(), HostError> {
        validate_name(name)?;
        let link = Arc::clone(&self.link);
        let fname = name.to_string();
        let f: HostBuiltin =
            Arc::new(move |_ctx: &mut Context, args: &[Value]| forward(&link, &fname, args));
        self.ctx.host_builtins.insert(name.to_string(), f);
        Ok(())
    }

    /// Remove a host function. Returns whether it was defined.
    pub fn undefine(&mut self, name: &str) -> bool {
        self.ctx.host_builtins.remove(name).is_some()
    }

    /// Host function names, sorted.
    pub fn functions(&self) -> Vec<String> {
        let mut names: Vec<String> = self.ctx.host_builtins.keys().cloned().collect();
        names.sort();
        names
    }

    /// A global's value. Fails if a script stored something nested too
    /// deeply to hand over; see [`crate::depth`].
    pub fn get(&self, name: &str) -> Result<Option<Value>, HostError> {
        match self.ctx.globals().get(name) {
            Some(v) if exceeds_nesting(v, MAX_NESTING) => Err(too_deep(name)),
            other => Ok(other.cloned()),
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.ctx.globals().contains_key(name)
    }

    pub fn set(&mut self, name: &str, value: Value) -> Result<(), HostError> {
        validate_name(name)?;
        if exceeds_nesting(&value, MAX_NESTING) {
            drop_flat(value);
            return Err(too_deep(name));
        }
        if let Some(old) = self.ctx.globals_mut().insert(name.to_string(), value) {
            drop_flat(old);
        }
        Ok(())
    }

    /// Remove a global. Returns whether it existed.
    pub fn unset(&mut self, name: &str) -> bool {
        self.ctx.globals_mut().remove(name).map(drop_flat).is_some()
    }

    /// Every global, sorted by name.
    pub fn globals(&self) -> Result<Vec<(String, Value)>, HostError> {
        let mut all = Vec::with_capacity(self.ctx.globals().len());
        for (k, v) in self.ctx.globals() {
            if exceeds_nesting(v, MAX_NESTING) {
                return Err(too_deep(k));
            }
            all.push((k.clone(), v.clone()));
        }
        all.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(all)
    }

    /// What the last evaluation consumed. Budgets are per evaluation: each
    /// one starts with a full tank.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Evaluate a document and return the value of its last form.
    pub fn eval(&mut self, source: &str, host: &mut dyn Host) -> Result<Value, HostError> {
        self.run_value(host, |ctx| Ok(eval_source(source, ctx)?))
    }

    /// Evaluate a file from the sandbox, as `(include path)` would.
    pub fn eval_file(&mut self, path: &str, host: &mut dyn Host) -> Result<Value, HostError> {
        let form = Expr::List(vec![
            Expr::Symbol("include".into()),
            Expr::String(path.into()),
        ]);
        self.run_value(host, |ctx| Ok(eval(&form, ctx)?))
    }

    /// Render an ERB-style template.
    pub fn render(&mut self, source: &str, host: &mut dyn Host) -> Result<String, HostError> {
        self.run(host, |ctx| Ok(render_template(source, ctx)?))
    }

    /// Substitute embedded snippets in an already-parsed document.
    pub fn embed(
        &mut self,
        doc: Value,
        markers: &Markers,
        host: &mut dyn Host,
    ) -> Result<Value, HostError> {
        self.run_value(host, |ctx| {
            Ok(evaluate_embedded_with(
                doc,
                ctx,
                &markers.prefix,
                &markers.key,
            )?)
        })
    }

    /// Parse a YAML document and substitute embedded snippets, including
    /// nodes carrying the YAML tag in `markers.tag`. Tags are only visible
    /// in the YAML text, which is why this takes a string rather than a
    /// parsed document.
    pub fn embed_yaml(
        &mut self,
        yaml: &str,
        markers: &Markers,
        host: &mut dyn Host,
    ) -> Result<Value, HostError> {
        self.run_value(host, |ctx| {
            let doc: serde_yaml::Value = serde_yaml::from_str(yaml)
                .map_err(|e| HostError::new(ErrorKind::Parse, format!("yaml: {e}")))?;
            Ok(evaluate_embedded_yaml_with(
                doc,
                ctx,
                &markers.tag,
                &markers.prefix,
                &markers.key,
            )?)
        })
    }

    /// Call a function by name: a `defn`, a builtin, a macro, or a host
    /// function. Arguments are passed as data, never evaluated as code.
    pub fn call(
        &mut self,
        name: &str,
        args: &[Value],
        host: &mut dyn Host,
    ) -> Result<Value, HostError> {
        validate_name(name)?;
        let mut form = Vec::with_capacity(args.len() + 1);
        form.push(Expr::Symbol(name.into()));
        // `value_to_expr` yields data literals, which evaluate to
        // themselves, so a string argument that looks like code stays a
        // string.
        form.extend(args.iter().map(value_to_expr));
        let form = Expr::List(form);
        self.run_value(host, |ctx| Ok(eval(&form, ctx)?))
    }

    /// Call a function value, such as one a script returned or stored in a
    /// global. Function values are only meaningful to the session that
    /// created them.
    pub fn call_value(
        &mut self,
        f: &Value,
        args: &[Value],
        host: &mut dyn Host,
    ) -> Result<Value, HostError> {
        self.run_value(host, |ctx| Ok(call_fn(ctx, f, args)?))
    }

    /// [`run`](Self::run) for work producing a value the host will receive.
    /// The nesting check happens on the evaluator thread, so a value too
    /// deep to hand over is also dropped there, on the large stack.
    fn run_value<F>(&mut self, host: &mut dyn Host, work: F) -> Result<Value, HostError>
    where
        F: FnOnce(&mut Context) -> Result<Value, HostError> + Send,
    {
        self.run(host, |ctx| {
            let v = work(ctx)?;
            if exceeds_nesting(&v, MAX_NESTING) {
                return Err(too_deep("result"));
            }
            Ok(v)
        })
    }

    /// Run `work` against this session's context on the evaluator thread,
    /// servicing host calls on this thread until it finishes.
    fn run<T, F>(&mut self, host: &mut dyn Host, work: F) -> Result<T, HostError>
    where
        T: Send,
        F: FnOnce(&mut Context) -> Result<T, HostError> + Send,
    {
        let (tx, rx) = channel();
        let events = Events { rx, tx: tx.clone() };
        let ctx = &mut self.ctx;
        // A new meter rather than `Fuel::reset`, which leaves the peak
        // depths from earlier evaluations in place.
        ctx.fuel = Fuel::new(*ctx.fuel.limits());
        set_link(&self.link, Some(tx.clone()));

        let mut callback_failed = false;
        let mut interrupted = false;

        let outcome = thread::scope(|scope| {
            let worker = spawn_with_fallback(scope, tx, move || work(ctx))?;
            loop {
                match host.wait(&events) {
                    Event::Done => break,
                    Event::Wake => interrupted = true,
                    Event::Call(req) => {
                        // After an interrupt, refuse further callbacks so a
                        // script that calls back often stops promptly.
                        let result = if interrupted {
                            Err("evaluation interrupted".to_string())
                        } else {
                            host.call(&req.name, req.args)
                        };
                        callback_failed |= result.is_err();
                        // The worker is blocked on this reply, so it is
                        // still listening.
                        let _ = req.reply.send(result);
                    }
                }
            }
            match worker.join() {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(panic)) => Err(HostError::internal(format!(
                    "evaluator panicked: {}",
                    panic_message(&*panic)
                ))),
                // Unreachable in practice: the worker catches its own
                // panics, so only a panic in the catching itself lands here.
                Err(panic) => Err(HostError::internal(format!(
                    "evaluator thread failed: {}",
                    panic_message(&*panic)
                ))),
            }
        });

        set_link(&self.link, None);
        let fuel = &self.ctx.fuel;
        self.usage = Usage {
            steps: fuel.steps(),
            allocated: fuel.allocated(),
            peak_call_depth: fuel.peak_call_depth(),
            peak_eval_depth: fuel.peak_eval_depth(),
        };

        match outcome.and_then(|r| r) {
            Err(mut e) if e.kind == ErrorKind::Eval && callback_failed => {
                // There is no try/catch in the language, so a failed
                // callback always ends the evaluation: this error is it.
                e.kind = ErrorKind::Callback;
                Err(e)
            }
            other => other,
        }
    }
}

impl Drop for Session {
    /// Tear the context down on a large stack: globals and closures may
    /// hold values nested deeply enough that their recursive `Drop` would
    /// overflow the host thread, which for Ruby is a GC finalizer.
    fn drop(&mut self) {
        let ctx = std::mem::replace(&mut self.ctx, Context::new(EvalConfig::default()));
        let slot = Mutex::new(Some(ctx));
        let spawned = thread::scope(|scope| {
            STACK_SIZES.iter().any(|&size| {
                thread::Builder::new()
                    .stack_size(size)
                    .spawn_scoped(scope, || drop(take_slot(&slot)))
                    .is_ok()
            })
        });
        if !spawned {
            drop(take_slot(&slot));
        }
    }
}

fn take_slot<T>(slot: &Mutex<Option<T>>) -> Option<T> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

fn set_link(link: &Link, tx: Option<Sender<Event>>) {
    *link.lock().unwrap_or_else(PoisonError::into_inner) = tx;
}

/// The body of every host builtin: post the call to the waiting host thread
/// and block until it replies.
fn forward(link: &Link, name: &str, args: &[Value]) -> Result<Value, BuiltinError> {
    let failed = |message: String| BuiltinError::Failed {
        function: name.to_string(),
        message,
    };
    let tx = link
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| failed("host function called outside an evaluation".into()))?;
    // Checked here, on the evaluator's stack, so the host thread never
    // receives anything it cannot safely take apart.
    if args.iter().any(|a| exceeds_nesting(a, MAX_NESTING)) {
        return Err(failed(format!(
            "arguments nest deeper than {MAX_NESTING} levels"
        )));
    }
    let (reply_tx, reply_rx) = sync_channel(1);
    tx.send(Event::Call(CallRequest {
        name: name.to_string(),
        args: args.to_vec(),
        reply: reply_tx,
    }))
    .map_err(|_| failed("host is no longer listening".into()))?;
    reply_rx
        .recv()
        .map_err(|_| failed("host dropped the call without replying".into()))?
        .map_err(failed)
}

/// Sends [`Event::Done`] when dropped, so the host stops waiting whether the
/// work returned or panicked.
struct DoneGuard(Sender<Event>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        let _ = self.0.send(Event::Done);
    }
}

type Caught<T> = Result<T, Box<dyn Any + Send>>;

/// Spawn `work` on the largest stack the process will grant.
fn spawn_with_fallback<'scope, 'env, T, F>(
    scope: &'scope thread::Scope<'scope, 'env>,
    done: Sender<Event>,
    work: F,
) -> Result<thread::ScopedJoinHandle<'scope, Caught<T>>, HostError>
where
    T: Send + 'scope,
    F: FnOnce() -> T + Send + 'scope,
{
    // A failed spawn drops the closure it was given, so the work waits in a
    // slot each attempt borrows rather than being moved into the first one.
    let slot = Arc::new(Mutex::new(Some((work, done))));
    let mut last_err = None;
    for size in STACK_SIZES {
        let slot = Arc::clone(&slot);
        let spawned = thread::Builder::new()
            .name("trongo-eval".into())
            .stack_size(size)
            .spawn_scoped(scope, move || {
                let (work, done) = take_slot(&slot).expect("evaluator work runs once");
                let _done = DoneGuard(done);
                catch_unwind(AssertUnwindSafe(work))
            });
        match spawned {
            Ok(handle) => return Ok(handle),
            Err(e) => last_err = Some(e),
        }
    }
    Err(HostError::internal(format!(
        "could not start the evaluator thread: {}",
        last_err.map_or_else(|| "unknown error".into(), |e| e.to_string())
    )))
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

// Copyright (c) 2026 Durable Programming, LLC.
