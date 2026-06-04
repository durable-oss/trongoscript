//! Trongoscript error types.

use trongoscript_parser::ParseError;
use trongoscript_stdlib::BuiltinError;
use thiserror::Error;

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
}

pub type EvalResult<T> = Result<T, EvalError>;

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.