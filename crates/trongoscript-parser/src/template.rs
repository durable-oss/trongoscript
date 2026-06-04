//! ERB-like template parser.
//!
//! Templates interleave raw text with Trongoscript tag blocks:
//!
//! ```text
//! Hello, <%= name %>!
//! <% (def count (size items)) %>
//! You have <%= count %> items.
//! ```
//!
//! Tags:
//!   `<%= expr %>`     — evaluate and interpolate the value.
//!   `<% form ... %>`  — evaluate forms, discard their values
//!                       (useful for `(def ...)` and side-effecting forms).
//!   `<%# comment %>`  — comment; removed entirely.
//!   `<%% ... %>`      — literal `<% ... %>` (the leading `<%%` becomes `<%`).
//!   `%%>`             — literal `%>` inside a tag body.

use crate::ast::{Expr, Template, TemplateChunk};
use crate::{form_with_trailing_ws, parse_expr, ws, ParseError};
use nom::multi::many1;

/// Parse a Trongoscript template document.
pub fn parse_template(source: &str) -> Result<Template, ParseError> {
    let mut chunks: Vec<TemplateChunk> = Vec::new();
    let mut text = String::new();
    let mut rest = source;

    while !rest.is_empty() {
        match rest.find("<%") {
            None => {
                text.push_str(rest);
                rest = "";
            }
            Some(idx) => {
                text.push_str(&rest[..idx]);
                rest = &rest[idx..];

                // <%% → literal <%
                if rest.starts_with("<%%") {
                    text.push_str("<%");
                    rest = &rest[3..];
                    continue;
                }

                // <%# … %> → discard
                if rest.starts_with("<%#") {
                    rest = &rest[3..];
                    let end = rest.find("%>").ok_or_else(|| ParseError::Syntax {
                        offset: source.len() - rest.len(),
                        message: "unterminated <%# comment".into(),
                    })?;
                    rest = &rest[end + 2..];
                    continue;
                }

                let silent;
                if let Some(after) = rest.strip_prefix("<%=") {
                    rest = after;
                    silent = false;
                } else if let Some(after) = rest.strip_prefix("<%") {
                    rest = after;
                    silent = true;
                } else {
                    // Shouldn't happen — `find` returned a `<%` position.
                    text.push_str(&rest[..1]);
                    rest = &rest[1..];
                    continue;
                }

                if !text.is_empty() {
                    chunks.push(TemplateChunk::Text(std::mem::take(&mut text)));
                }

                let end = find_close(rest).ok_or_else(|| ParseError::Syntax {
                    offset: source.len() - rest.len(),
                    message: "unterminated <% block".into(),
                })?;
                let body = &rest[..end];
                rest = &rest[end + 2..];

                let body_cleaned = body.replace("%%>", "%>");

                if silent {
                    let forms = parse_tag_forms(&body_cleaned)?;
                    chunks.push(TemplateChunk::Silent(forms));
                    // Trim line — swallow trailing newline for silent tags so
                    // `<% ... %>` lines don't leave blank lines in output.
                    if let Some(s) = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n')) {
                        rest = s;
                    }
                } else {
                    let expr = parse_expr(&body_cleaned)?;
                    chunks.push(TemplateChunk::Expr(expr));
                }
            }
        }
    }

    if !text.is_empty() {
        chunks.push(TemplateChunk::Text(text));
    }

    Ok(Template { chunks })
}

/// Find the closing `%>` for a tag body, treating `%%>` as a literal `%>`.
fn find_close(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'%' && bytes[i + 1] == b'>' {
            if i > 0 && bytes[i - 1] == b'%' {
                i += 2;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

fn parse_tag_forms(body: &str) -> Result<Vec<Expr>, ParseError> {
    let (rest, _) = ws(body).map_err(|_| ParseError::Syntax {
        offset: 0,
        message: "tag whitespace error".into(),
    })?;
    let (rest, forms) = many1(form_with_trailing_ws)(rest).map_err(|e| match e {
        nom::Err::Incomplete(_) => ParseError::Syntax {
            offset: 0,
            message: "incomplete tag form".into(),
        },
        nom::Err::Error(ve) | nom::Err::Failure(ve) => {
            let kind = ve
                .errors
                .first()
                .map(|(_, k)| format!("{:?}", k))
                .unwrap_or_else(|| "parse error".into());
            ParseError::Syntax {
                offset: 0,
                message: kind,
            }
        }
    })?;
    if !rest.trim().is_empty() {
        return Err(ParseError::Syntax {
            offset: 0,
            message: format!("trailing input in tag: {:?}", rest),
        });
    }
    Ok(forms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn plain_text() {
        let t = parse_template("hello").unwrap();
        assert_eq!(t.chunks, vec![TemplateChunk::Text("hello".into())]);
    }

    #[test]
    fn expr_tag() {
        let t = parse_template("Hi <%= name %>!").unwrap();
        assert_eq!(t.chunks.len(), 3);
        assert_eq!(t.chunks[0], TemplateChunk::Text("Hi ".into()));
        match &t.chunks[1] {
            TemplateChunk::Expr(Expr::Symbol(n)) => assert_eq!(n, "name"),
            other => panic!("expected expr tag, got {:?}", other),
        }
        assert_eq!(t.chunks[2], TemplateChunk::Text("!".into()));
    }

    #[test]
    fn silent_tag_swallows_newline() {
        let t = parse_template("<% (def x 1) %>\nafter").unwrap();
        // The newline after the silent tag is swallowed.
        assert_eq!(t.chunks.len(), 2);
        assert_eq!(t.chunks[1], TemplateChunk::Text("after".into()));
    }

    #[test]
    fn escape() {
        let t = parse_template("literal <%% something").unwrap();
        assert_eq!(
            t.chunks,
            vec![TemplateChunk::Text("literal <% something".into())]
        );
    }

    #[test]
    fn comment() {
        let t = parse_template("a<%# ignored %>b").unwrap();
        assert_eq!(t.chunks, vec![TemplateChunk::Text("ab".into())]);
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.