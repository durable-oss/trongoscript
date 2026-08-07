//! Render an ERB-like template by interleaving raw text with the value of
//! `<%= expr %>` blocks and the side effects of `<% form ... %>` blocks.

use trongoscript_parser::{parse_template, Template, TemplateChunk};

use crate::error::{EvalError, EvalResult};
use crate::eval::{eval, Context};
use crate::value::to_display_string;

/// Render a template source string, returning the produced text.
pub fn render_template(source: &str, ctx: &mut Context) -> EvalResult<String> {
    let tmpl = parse_template(source).map_err(EvalError::from)?;
    render(&tmpl, ctx)
}

/// Render a pre-parsed [`Template`].
pub fn render(tmpl: &Template, ctx: &mut Context) -> EvalResult<String> {
    let mut out = String::new();
    for chunk in &tmpl.chunks {
        match chunk {
            TemplateChunk::Text(t) => out.push_str(t),
            TemplateChunk::Expr(e) => {
                let v = eval(e, ctx)?;
                out.push_str(&to_display_string(&v));
            }
            TemplateChunk::Silent(forms) => {
                for f in forms {
                    eval(f, ctx)?;
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::EvalConfig;
    use pretty_assertions::assert_eq;

    fn render_str(src: &str) -> String {
        let mut ctx = Context::new(EvalConfig::default());
        render_template(src, &mut ctx).expect("render failed")
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(render_str("hello world"), "hello world");
    }

    #[test]
    fn interpolates_expression() {
        assert_eq!(render_str("hi <%= (+ 1 2) %>"), "hi 3");
    }

    #[test]
    fn def_then_use() {
        let out = render_str("<% (def name \"alice\") %>Hello <%= name %>!");
        assert_eq!(out, "Hello alice!");
    }

    #[test]
    fn nil_renders_empty() {
        assert_eq!(render_str("[<%= nil %>]"), "[]");
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
