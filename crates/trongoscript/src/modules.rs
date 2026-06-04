//! Trongoscript-implemented stdlib modules.
//!
//! Modules whose entire surface is a set of macros (or `def`s) live here as
//! `.trongo` source files. `(use <name>)` enables Rust-backed builtins
//! tagged with `<name>`, and additionally evaluates the matching source
//! below — exactly once per [`Context`].
//!
//! Add a new module by:
//! 1. Dropping a `modules/<name>.trongo` source file alongside this one.
//! 2. Listing it in [`TS_MODULES`].

/// (name, source) for every Trongoscript-defined module. The source is
/// `include_str!`'d at compile time, so the binary is self-contained.
pub const TS_MODULES: &[(&str, &str)] = &[
    ("predicates", include_str!("modules/predicates.trongo")),
];

/// Modules that have no inventory builtins and no TS source — they're
/// implemented entirely as context-aware host functions in the evaluator
/// crate. Listed here so [`is_known_module`] still recognizes them.
const CONTEXT_MODULES: &[&str] = &["rng"];

/// Source for the named TS module, if any.
pub fn ts_module_source(name: &str) -> Option<&'static str> {
    TS_MODULES
        .iter()
        .find_map(|(n, src)| if *n == name { Some(*src) } else { None })
}

/// True if `name` is a recognized module — Rust builtin, TS-defined, or
/// context-only (evaluator-implemented).
pub fn is_known_module(name: &str) -> bool {
    if TS_MODULES.iter().any(|(n, _)| *n == name) {
        return true;
    }
    if CONTEXT_MODULES.contains(&name) {
        return true;
    }
    trongoscript_stdlib::known_modules().any(|m| m == name)
}

/// Every known module name (Rust + TS + context-only).
pub fn all_module_names() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = trongoscript_stdlib::known_modules().collect();
    for (n, _) in TS_MODULES {
        if !out.contains(n) {
            out.push(*n);
        }
    }
    for n in CONTEXT_MODULES {
        if !out.contains(n) {
            out.push(*n);
        }
    }
    out.sort();
    out
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.