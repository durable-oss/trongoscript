//! Shared embedding layer for the Trongoscript language bindings.
//!
//! The Ruby gem and the PHP extension are thin: each converts its
//! language's values to and from [`Value`], maps [`ErrorKind`] to its own
//! exception classes, and implements [`Host`] to run callbacks. Everything
//! else lives here, where it can be tested without either interpreter:
//!
//! - [`Options`] validates sandbox, limit, and global settings.
//! - [`Session`] owns a [`trongoscript::Context`] and runs each evaluation
//!   on a large-stack thread, routing host function calls back to the
//!   calling thread. The module docs on [`session`] explain why.
//! - [`depth`] keeps values nested too deeply for a host's stack from
//!   crossing the boundary.
//!
//! This crate knows nothing about Ruby or PHP.

pub mod depth;
pub mod error;
pub mod options;
pub mod session;

pub use depth::{drop_flat, exceeds_nesting, MAX_NESTING};
pub use error::{ErrorKind, HostError};
pub use options::{fuel_limits, sandbox_limits, Options, SandboxSpec};
pub use session::{CallRequest, Event, Events, Host, Markers, NoHost, Session, Usage, Waker};
pub use trongoscript::{FuelLimits, SandboxLimits, Value};

/// Format a value as Trongoscript source, which evaluates back to the same
/// value.
pub fn to_trongo(value: &Value, pretty: bool) -> String {
    if pretty {
        trongoscript::to_trongo_source_pretty(value)
    } else {
        trongoscript::to_trongo_source(value)
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
