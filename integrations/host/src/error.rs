//! Errors as the bindings report them.
//!
//! Ruby and PHP each map [`ErrorKind`] onto their own exception classes, so
//! the classification lives here once instead of being re-derived from
//! [`EvalError`] variants in two languages.

use std::fmt;

use trongoscript::{EvalError, SandboxError};

/// What went wrong, at the granularity a host language exposes as distinct
/// exception classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The source text (or a YAML document handed to `embed_yaml`) does not
    /// parse.
    Parse,
    /// The script ran out of a fuel budget: steps, depth, value size, or
    /// total allocation. Retrying with larger limits may succeed.
    ResourceLimit,
    /// A sandboxed filesystem operation was refused or failed, including
    /// using `include` / `read-file` with no sandbox configured.
    Sandbox,
    /// The script is wrong: an undefined symbol, a type error, a failing
    /// builtin, and so on.
    Eval,
    /// A host-defined function failed. The binding normally re-raises the
    /// host's own exception instead of this one.
    Callback,
    /// The host passed an invalid option or value.
    Argument,
    /// The interpreter itself failed: a panic, or no thread could be started.
    Internal,
}

/// An error crossing the binding boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    pub kind: ErrorKind,
    pub message: String,
}

impl HostError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn argument(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Argument, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HostError {}

impl From<EvalError> for HostError {
    fn from(e: EvalError) -> Self {
        // Resource limits are checked first: a parse that nests too deeply
        // is a resource problem, and `is_resource_limit` already says so.
        let kind = if e.is_resource_limit() {
            ErrorKind::ResourceLimit
        } else {
            match e {
                EvalError::Parse(_) => ErrorKind::Parse,
                EvalError::Sandbox(_) | EvalError::NoSandbox => ErrorKind::Sandbox,
                _ => ErrorKind::Eval,
            }
        };
        Self::new(kind, e.to_string())
    }
}

impl From<SandboxError> for HostError {
    fn from(e: SandboxError) -> Self {
        Self::new(ErrorKind::Sandbox, e.to_string())
    }
}

// Copyright (c) 2026 Durable Programming, LLC.
