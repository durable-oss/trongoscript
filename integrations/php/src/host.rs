//! The PHP side of an evaluation: running host functions.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;

use ext_php_rs::convert::IntoZvalDyn;
use ext_php_rs::types::{ZendCallable, Zval};
use ext_php_rs::zend::{try_catch, CatchError, ExecutorGlobals};
use serde_json::Value as Data;
use trongoscript_host::Host;

use crate::convert::{from_php, to_php, type_error, Fail};

pub struct PhpHost<'a> {
    functions: &'a HashMap<String, Zval>,
    objects: bool,
    /// A conversion error to throw once the evaluation has unwound, unless
    /// the callable's own exception is already pending.
    pub failure: Option<Fail>,
    /// A callable hit a fatal error (memory limit, time limit, `exit` in
    /// some SAPIs) and the engine tried to unwind with `longjmp`. It was
    /// caught at the call so it could not jump over the evaluator thread;
    /// the caller re-raises it once the evaluation is over.
    pub bailed_out: bool,
}

impl<'a> PhpHost<'a> {
    pub fn new(functions: &'a HashMap<String, Zval>, objects: bool) -> Self {
        Self {
            functions,
            objects,
            failure: None,
            bailed_out: false,
        }
    }

    fn invoke(&self, f: &Zval, args: Vec<Data>) -> Result<Data, Fail> {
        let args = args
            .into_iter()
            .map(|a| to_php(a, self.objects))
            .collect::<Result<Vec<Zval>, Fail>>()?;
        let callable =
            ZendCallable::new(f).map_err(|e| type_error(format!("not callable: {e}")))?;
        let params: Vec<&dyn IntoZvalDyn> = args.iter().map(|z| z as &dyn IntoZvalDyn).collect();
        let result = callable
            .try_call(params)
            .map_err(|e| type_error(format!("call failed: {e}")))?;
        from_php(&result)
    }
}

impl Host for PhpHost<'_> {
    fn call(&mut self, name: &str, args: Vec<Data>) -> Result<Data, String> {
        let Some(f) = self.functions.get(name) else {
            return Err(format!("no host function `{name}`"));
        };
        // `zend_try` around every call: a fatal error inside the callable
        // must not `longjmp` past this frame while the evaluator thread is
        // still running against a context on this stack.
        match try_catch(AssertUnwindSafe(|| self.invoke(f, args))) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(fail)) => {
                let message = if ExecutorGlobals::has_exception() {
                    ExecutorGlobals::pending_exception_class()
                        .map_or_else(|| "threw an exception".into(), |c| format!("threw {c}"))
                } else {
                    fail.message.clone()
                };
                self.failure = Some(fail);
                Err(message)
            }
            Err(CatchError::Bailout) => {
                self.bailed_out = true;
                Err("PHP fatal error".into())
            }
            Err(e) => Err(format!("host function failed: {e}")),
        }
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
