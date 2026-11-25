//! The Ruby side of an evaluation: running callbacks and waiting without
//! the GVL.

use std::ffi::c_void;
use std::time::Duration;

use magnus::value::{BoxValue, ReprValue};
use magnus::{error::ErrorType, Error, Exception, Ruby, Value};
use serde_json::Value as Data;
use trongoscript_host::{Event, Events, Host, Waker};

use crate::convert::{from_ruby, to_ruby};

/// How long to wait for the evaluator while still holding the GVL.
///
/// Reacquiring the GVL after a release means waiting for whichever thread
/// holds it to reach the end of its timeslice, up to 100ms when other
/// threads are busy. Paid once per callback, that turns a script making a
/// few thousand callbacks into minutes. Callbacks usually follow each other
/// within microseconds, so holding on briefly keeps the GVL across them,
/// and Ruby's timer still hands it to other threads at their fair share
/// whenever a callback runs Ruby code. A script that runs without calling
/// back releases the GVL after this long.
const HOLD_GVL: Duration = Duration::from_micros(100);

/// An exception (or `throw`, or `Thread#kill`) kept until the evaluation
/// has unwound, so it can be raised from the evaluating method.
enum Failure {
    /// Boxed so the GC sees it: Ruby may collect garbage while this thread
    /// waits for the evaluator with the GVL released.
    Exception(BoxValue<Exception>),
    /// A `throw` or `break`, or an error built in Rust. Neither holds a
    /// collectable object.
    Other(Error),
}

pub struct RubyHost<'a> {
    ruby: &'a Ruby,
    /// `->(name, args) { ... }`, supplied by `Trongoscript::Context`.
    dispatcher: Value,
    symbolize: bool,
    /// A callback raised.
    failure: Option<Failure>,
    /// A real interrupt (signal, `Thread#raise`, `Thread#kill`) arrived
    /// while waiting. Ruby has already handed it over as an exception, which
    /// is re-raised once the evaluator has stopped.
    interrupt: Option<Failure>,
}

impl<'a> RubyHost<'a> {
    pub fn new(ruby: &'a Ruby, dispatcher: Value, symbolize: bool) -> Self {
        Self {
            ruby,
            dispatcher,
            symbolize,
            failure: None,
            interrupt: None,
        }
    }

    /// The error to raise in place of the evaluation's own, if any: an
    /// interrupt that arrived meanwhile, or else a callback's exception.
    pub fn take_error(&mut self) -> Result<(), Error> {
        match self.interrupt.take().or_else(|| self.failure.take()) {
            None => Ok(()),
            Some(f) => Err(f.into_error()),
        }
    }

    fn fail(&mut self, e: Error) -> String {
        let message = e.to_string();
        self.failure = Some(Failure::keep(e));
        message
    }

    fn try_call(&self, name: &str, args: Vec<Data>) -> Result<Data, Error> {
        let rargs = self.ruby.ary_new_capa(args.len());
        for a in args {
            rargs.push(to_ruby(self.ruby, a, self.symbolize)?)?;
        }
        let result: Value = self
            .dispatcher
            .funcall("call", (self.ruby.str_new(name), rargs))?;
        from_ruby(self.ruby, result)
    }
}

impl Failure {
    fn keep(e: Error) -> Self {
        match e.error_type() {
            ErrorType::Exception(exc) => Failure::Exception(BoxValue::new(*exc)),
            _ => Failure::Other(e),
        }
    }

    fn into_error(self) -> Error {
        match self {
            Failure::Exception(e) => Error::from(*e),
            Failure::Other(e) => e,
        }
    }
}

impl Host for RubyHost<'_> {
    fn call(&mut self, name: &str, args: Vec<Data>) -> Result<Data, String> {
        self.try_call(name, args).map_err(|e| self.fail(e))
    }

    fn wait(&mut self, events: &Events) -> Event {
        let mut declined = 0;
        loop {
            if self.interrupt.is_some() {
                // Waiting with the GVL held, because the evaluator stops
                // promptly when callbacks are refused (and fuel bounds it
                // regardless).
                return events.recv();
            }
            let event = if declined > 1 {
                // Ruby keeps declining to let this thread block: wait with
                // the GVL held rather than spin.
                Some(events.recv())
            } else {
                events
                    .recv_timeout(HOLD_GVL)
                    .or_else(|| wait_without_gvl(events))
            };
            match event {
                // A wake only means Ruby wanted this thread's attention;
                // whether that was a real interrupt is decided below. It is
                // never passed on: `unblock` can run after the wait above has
                // already received an event, leaving a stale wake queued
                // that a later wait would otherwise mistake for an interrupt.
                Some(Event::Wake) | None => {}
                Some(event) => return event,
            }
            // Something wants this thread: a real interrupt, or just Ruby's
            // timer asking it to yield to another thread. Let Ruby handle
            // it. A real one comes back as an error to raise later; a
            // thread switch comes back `Ok` and waiting resumes.
            match self.ruby.thread_check_ints() {
                Ok(()) => declined += 1,
                Err(e) => {
                    self.interrupt = Some(Failure::keep(e));
                    return Event::Wake;
                }
            }
        }
    }
}

struct WaitSlot<'a> {
    events: &'a Events,
    event: Option<Event>,
}

unsafe extern "C" fn recv_without_gvl(data: *mut c_void) -> *mut c_void {
    // SAFETY: `data` is the `WaitSlot` on `wait_without_gvl`'s stack,
    // which outlives this call. Nothing here touches Ruby.
    let slot = unsafe { &mut *(data as *mut WaitSlot) };
    slot.event = Some(slot.events.recv());
    std::ptr::null_mut()
}

unsafe extern "C" fn unblock(data: *mut c_void) {
    // SAFETY: `data` is the `Waker` on `wait_without_gvl`'s stack. Ruby
    // only calls this while `recv_without_gvl` may be running, which is
    // within that frame. `Waker` is `Sync`, and Ruby may call this from
    // another thread.
    let waker = unsafe { &*(data as *const Waker) };
    waker.wake();
}

/// Block for the next event with the GVL released, so other Ruby threads
/// run meanwhile.
///
/// `rb_thread_call_without_gvl2` rather than the plain version: the plain
/// one checks for interrupts on the way out and raises them with a
/// `longjmp` straight through these Rust frames, skipping destructors and
/// abandoning the evaluator thread. The `2` variant never raises. If any
/// interrupt flag is already set, including the timer's request for a
/// thread switch, it declines to block at all, which shows up here as
/// `None`.
fn wait_without_gvl(events: &Events) -> Option<Event> {
    let waker = events.waker();
    let mut slot = WaitSlot {
        events,
        event: None,
    };
    unsafe {
        rb_sys::rb_thread_call_without_gvl2(
            Some(recv_without_gvl),
            &mut slot as *mut WaitSlot as *mut c_void,
            Some(unblock),
            &waker as *const Waker as *mut c_void,
        );
    }
    slot.event
}

// Copyright (c) 2026 Durable Programming, LLC.
