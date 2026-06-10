//! # Trongoscript
//!
//! A YAMLScript-inspired DSL for building nested data structures from a
//! small Lisp-flavored expression language. Written entirely in Rust and
//! designed for static compilation.
//!
//! Trongoscript can be used in three modes:
//!
//! - **Document mode** (default): evaluate a `.trongo` file and produce a
//!   value, serialized as JSON / YAML / TOML.
//! - **Embedded mode**: walk an existing JSON / YAML / TOML tree and
//!   substitute scalars / single-key maps that carry the `!trongo` marker
//!   with the result of evaluating their body.
//! - **Template mode**: ERB-style text templates interleaving raw text
//!   with `<%= expr %>` and `<% form %>` blocks.
//!
//! ## Security model
//!
//! Trongoscript has *no* ambient capabilities. The interpreter cannot open
//! network connections, run subprocesses, or touch the filesystem unless
//! the embedder hands it a [`Sandbox`] rooted at a specific directory. The
//! sandbox limits paths to its subtree, refuses absolute paths, declines
//! to follow symlinks during directory listings, and enforces per-file
//! size and recursion limits. See [`sandbox`] for details.
//!
//! Hosts may also register additional functions via [`EvalConfig::with_host_builtin`].
//!
//! ## Quick example
//!
//! ```
//! use trongoscript::{eval_source, Context, EvalConfig};
//! let mut ctx = Context::new(EvalConfig::default());
//! let v = eval_source(
//!     r#"{:greeting (upcase "hi") :count (+ 1 2 3)}"#,
//!     &mut ctx,
//! ).unwrap();
//! assert_eq!(v["greeting"], serde_json::Value::String("HI".into()));
//! assert_eq!(v["count"], serde_json::Value::Number(6.into()));
//! ```

pub mod embed;
pub mod error;
pub mod eval;
pub mod higher_order;
pub mod modules;
pub mod rng;
pub mod sandbox;
pub mod template;
pub mod value;

pub use embed::{
    evaluate_embedded, evaluate_embedded_with, evaluate_embedded_yaml,
    evaluate_embedded_yaml_with, DEFAULT_KEY, DEFAULT_PREFIX, DEFAULT_TAG,
};
pub use error::{EvalError, EvalResult};
pub use eval::{eval, eval_module, eval_source, partial_eval, value_to_expr, Closure, Context, EvalConfig, HostBuiltin, MacroDef, Partial};
pub use trongoscript_parser::{
    parse_expr, parse_expr_with, parse_module, parse_module_with, parse_template, Expr, Module,
    ParserOptions, ReaderMacroFn, ReaderMacroOutput, Template,
};
pub use trongoscript_stdlib::{Builtin, BuiltinError, BuiltinResult, Value};
pub use sandbox::{Sandbox, SandboxError, SandboxLimits};
pub use template::{render, render_template};

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.