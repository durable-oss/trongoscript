//! Trongoscript AST — a Lisp/s-expression form.
//!
//! Every Trongoscript program is one or more s-expressions. The top-level
//! result is the value of the *last* form (or, if there is exactly one
//! form, the value of that form).
//!
//! Atoms:
//!   nil, true, false                — constants
//!   42, 3.14, -7, 1e9               — numbers (f64)
//!   "string"                        — strings (escapes: \n \t \r \\ \" \u{XXXX})
//!   :keyword                        — keyword (evaluates to its string name)
//!   foo, foo-bar, +, my/fn          — symbols (variable lookup or special form head)
//!
//! Compound forms:
//!   (head arg1 arg2 ...)            — list: function call or special form
//!   [a b c]                         — vector: a data literal (list of values)
//!   {:k v :k2 v2}                   — map: a data literal (alternating keys and values)
//!
//! Special forms (recognized by `head` being a specific symbol):
//!   (let [name value ...] body...)
//!   (if cond then else?)
//!   (do expr...)
//!   (quote x)            also written as 'x
//!   (def name value)     binds a top-level value
//!
//! Comments: `;` to end-of-line, or `#_ form` to discard the next form.

use std::fmt;

/// A Trongoscript expression / value form.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Nil,
    Bool(bool),
    Number(f64),
    String(String),
    Keyword(String),
    Symbol(String),
    /// `(head arg ...)` — list form; evaluated as call or special form.
    List(Vec<Expr>),
    /// `[a b c]` — vector literal; evaluates each element, returns a list.
    Vector(Vec<Expr>),
    /// `{:k v :k2 v2}` — map literal; alternating keys and values.
    Map(Vec<(Expr, Expr)>),
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Nil => f.write_str("nil"),
            Expr::Bool(b) => write!(f, "{}", b),
            Expr::Number(n) => write!(f, "{}", n),
            Expr::String(s) => write!(f, "{:?}", s),
            Expr::Keyword(k) => write!(f, ":{}", k),
            Expr::Symbol(s) => f.write_str(s),
            Expr::List(items) => {
                f.write_str("(")?;
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{}", e)?;
                }
                f.write_str(")")
            }
            Expr::Vector(items) => {
                f.write_str("[")?;
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{}", e)?;
                }
                f.write_str("]")
            }
            Expr::Map(pairs) => {
                f.write_str("{")?;
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{} {}", k, v)?;
                }
                f.write_str("}")
            }
        }
    }
}

/// A whole document: zero or more top-level forms.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub forms: Vec<Expr>,
}

/// A template (ERB-like) document: raw text interleaved with Trongoscript blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub chunks: Vec<TemplateChunk>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TemplateChunk {
    /// Literal text to emit verbatim.
    Text(String),
    /// `<%= expr %>` — evaluate and interpolate.
    Expr(Expr),
    /// `<% form ... %>` — evaluate, discard result (control flow / bindings).
    Silent(Vec<Expr>),
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.