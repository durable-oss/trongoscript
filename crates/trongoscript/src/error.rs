//! Trongoscript error types.

use thiserror::Error;
use trongoscript_parser::ParseError;
use trongoscript_stdlib::BuiltinError;

use crate::sandbox::SandboxError;

/// Errors raised during evaluation.
#[derive(Error, Debug)]
pub enum EvalError {
    #[error("parse error: {0}")]
    Parse(#[from] ParseError),

    #[error("undefined symbol `{0}`")]
    UndefinedSymbol(String),

    #[error("undefined function `{0}`")]
    UndefinedFunction(String),

    #[error("`{function}` expects {expected} arg(s), got {actual}")]
    Arity {
        function: String,
        expected: String,
        actual: usize,
    },

    #[error("cannot call non-callable value of type {0}")]
    NotCallable(String),

    #[error("type error: expected {expected}, got {got}")]
    Type { expected: String, got: String },

    #[error("invalid special form: {0}")]
    InvalidForm(String),

    #[error(transparent)]
    Builtin(#[from] BuiltinError),

    #[error("sandbox: {0}")]
    Sandbox(#[from] SandboxError),

    #[error("filesystem access not configured (no sandbox root was provided)")]
    NoSandbox,

    #[error("map key must be a string or keyword (got {0})")]
    BadMapKey(String),

    /// The evaluation step budget was spent. Raised by
    /// [`Fuel::charge_step`](crate::fuel::Fuel::charge_step); usually means
    /// a non-terminating loop or recursion.
    #[error("evaluation step limit exceeded ({limit} steps) — possible infinite loop")]
    FuelExhausted { limit: u64 },

    /// Closure calls nested deeper than the configured maximum. Raised
    /// before the native stack can overflow.
    #[error("call depth limit exceeded (max {max}) — possible unbounded recursion")]
    CallDepthExceeded { max: usize },

    /// `eval` recursed deeper than the configured maximum, from nested data
    /// or nested forms rather than from calls.
    #[error("expression nesting limit exceeded (max {max})")]
    EvalDepthExceeded { max: usize },

    /// A single collection or string exceeded
    /// [`FuelLimits::max_value_len`](crate::fuel::FuelLimits::max_value_len).
    #[error("{kind} of length {len} exceeds the maximum value size ({max})")]
    ValueTooLarge {
        kind: &'static str,
        len: usize,
        max: usize,
    },

    /// The cumulative allocation budget was spent.
    #[error("allocation limit exceeded ({limit} bytes)")]
    AllocationExceeded { limit: u64 },
}

impl EvalError {
    /// Whether this error came from resource metering rather than from a
    /// defect in the script.
    ///
    /// Hosts use this to distinguish "this script is wrong" (report to the
    /// author) from "this script is too expensive" (report to the operator,
    /// possibly retry with a larger budget).
    pub fn is_resource_limit(&self) -> bool {
        matches!(
            self,
            EvalError::FuelExhausted { .. }
                | EvalError::CallDepthExceeded { .. }
                | EvalError::EvalDepthExceeded { .. }
                | EvalError::ValueTooLarge { .. }
                | EvalError::AllocationExceeded { .. }
                // Input that nests too deeply to parse is a resource
                // problem, not a syntax problem: the source is well-formed,
                // there is simply too much of it to descend safely.
                | EvalError::Parse(ParseError::TooDeep { .. })
        )
    }
}

pub type EvalResult<T> = Result<T, EvalError>;

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
