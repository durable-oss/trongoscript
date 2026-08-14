//! `strings` module — additional string operations.
//!
//! Activated by `(use strings)`. Pure string operations, no I/O.

use crate::{expect_number, expect_string, inventory, Builtin, BuiltinError, BuiltinResult, Value};

const MODULE: &str = "strings";

/// Describe a byte count for an error message, in whichever unit reads
/// most clearly. The ceiling is now configurable, so it is no longer
/// always a whole number of mebibytes.
fn describe_bytes(n: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    if n >= MIB && n.is_multiple_of(MIB) {
        format!("{} MiB", n / MIB)
    } else if n >= KIB && n.is_multiple_of(KIB) {
        format!("{} KiB", n / KIB)
    } else {
        format!("{n} bytes")
    }
}

/// Interpret a numeric argument as an output length in bytes, rejecting
/// anything that cannot produce a string of that size.
///
/// `f64 as usize` saturates rather than wrapping, so a huge or infinite
/// count silently becomes `usize::MAX` and the caller then asks the
/// allocator for it. NaN converts to 0, which is worse: it fails a
/// `< 0.0` test and looks like a legitimate empty request. Both are
/// rejected explicitly here.
///
/// `unit` is the byte cost of one output element, so the check bounds
/// bytes rather than element count.
///
/// The ceiling comes from [`crate::output_limit`], which the evaluator
/// sets from its allocation budget for the duration of the call. It was
/// once a hardcoded 16 MiB, which made `--max-alloc 1000` unable to stop
/// a 16 MiB allocation inside a single call; that value is now only the
/// default for callers that publish no budget.
fn output_len(fname: &str, n: f64, unit: usize) -> Result<usize, BuiltinError> {
    let max_out = crate::output_limit::limit();

    if n.is_nan() {
        return Err(BuiltinError::ArgError {
            function: fname.into(),
            message: "count must be a number, got NaN".into(),
        });
    }
    if n < 0.0 {
        return Err(BuiltinError::ArgError {
            function: fname.into(),
            message: "count must be non-negative".into(),
        });
    }
    // Compare in f64 before converting: a count past usize saturates on
    // cast, so the cast alone cannot tell "enormous" from "the maximum".
    if !n.is_finite() || n * (unit.max(1) as f64) > max_out as f64 {
        return Err(BuiltinError::Failed {
            function: fname.into(),
            message: format!("output would exceed {}", describe_bytes(max_out)),
        });
    }
    let n = n as usize;
    if n.saturating_mul(unit) > max_out {
        return Err(BuiltinError::Failed {
            function: fname.into(),
            message: format!("output would exceed {}", describe_bytes(max_out)),
        });
    }
    Ok(n)
}

fn lstrip(args: &[Value]) -> BuiltinResult {
    Ok(Value::String(
        expect_string("lstrip", &args[0])?.trim_start().to_string(),
    ))
}

fn rstrip(args: &[Value]) -> BuiltinResult {
    Ok(Value::String(
        expect_string("rstrip", &args[0])?.trim_end().to_string(),
    ))
}

fn truncate(args: &[Value]) -> BuiltinResult {
    let s = expect_string("truncate", &args[0])?;
    let n = expect_number("truncate", &args[1])?.max(0.0) as usize;
    Ok(Value::String(s.chars().take(n).collect()))
}

fn pad_with(args: &[Value], fname: &str, left: bool) -> BuiltinResult {
    let s = expect_string(fname, &args[0])?;
    let width = expect_number(fname, &args[1])?;
    let pad: char = if args.len() == 3 {
        let p = expect_string(fname, &args[2])?;
        match p.chars().next() {
            Some(c) if p.chars().count() == 1 => c,
            _ => {
                return Err(BuiltinError::ArgError {
                    function: fname.into(),
                    message: "pad must be a single character".into(),
                })
            }
        }
    } else {
        ' '
    };
    // Width is a character count, but the cap is on bytes, so a
    // multibyte pad character costs proportionally more.
    let n = output_len(fname, width, pad.len_utf8())?;
    let cur = s.chars().count();
    if cur >= n {
        return Ok(Value::String(s.to_string()));
    }
    let extra = std::iter::repeat_n(pad, n - cur).collect::<String>();
    Ok(Value::String(if left {
        format!("{}{}", extra, s)
    } else {
        format!("{}{}", s, extra)
    }))
}

fn pad_left(args: &[Value]) -> BuiltinResult {
    pad_with(args, "pad-left", true)
}
fn pad_right(args: &[Value]) -> BuiltinResult {
    pad_with(args, "pad-right", false)
}

fn repeat(args: &[Value]) -> BuiltinResult {
    let s = expect_string("repeat", &args[0])?;
    let n = expect_number("repeat", &args[1])?;
    let n = output_len("repeat", n, s.len())?;
    Ok(Value::String(s.repeat(n)))
}

fn chars(args: &[Value]) -> BuiltinResult {
    let s = expect_string("chars", &args[0])?;
    Ok(Value::Array(
        s.chars().map(|c| Value::String(c.to_string())).collect(),
    ))
}

fn lines(args: &[Value]) -> BuiltinResult {
    let s = expect_string("lines", &args[0])?;
    Ok(Value::Array(
        s.lines().map(|l| Value::String(l.to_string())).collect(),
    ))
}

fn str_reverse(args: &[Value]) -> BuiltinResult {
    let s = expect_string("str-reverse", &args[0])?;
    Ok(Value::String(s.chars().rev().collect()))
}

fn index_of(args: &[Value]) -> BuiltinResult {
    let s = expect_string("index-of", &args[0])?;
    let needle = expect_string("index-of", &args[1])?;
    // Char-index, not byte-index, so it lines up with `substr` semantics.
    match s.find(needle) {
        Some(byte_idx) => {
            let char_idx = s[..byte_idx].chars().count() as i64;
            Ok(Value::Number(char_idx.into()))
        }
        None => Ok(Value::Number((-1i64).into())),
    }
}

fn replace_first(args: &[Value]) -> BuiltinResult {
    let s = expect_string("replace-first", &args[0])?;
    let from = expect_string("replace-first", &args[1])?;
    let to = expect_string("replace-first", &args[2])?;
    Ok(Value::String(s.replacen(from, to, 1)))
}

fn to_uppercase(args: &[Value]) -> BuiltinResult {
    Ok(Value::String(
        expect_string("to-uppercase", &args[0])?.to_uppercase(),
    ))
}

fn to_lowercase(args: &[Value]) -> BuiltinResult {
    Ok(Value::String(
        expect_string("to-lowercase", &args[0])?.to_lowercase(),
    ))
}

/// "hello WORLD" → "Hello World". Word boundaries are whitespace runs.
fn to_titlecase(args: &[Value]) -> BuiltinResult {
    let s = expect_string("to-titlecase", &args[0])?;
    let mut out = String::with_capacity(s.len());
    let mut start_of_word = true;
    for c in s.chars() {
        if c.is_whitespace() {
            out.push(c);
            start_of_word = true;
        } else if start_of_word {
            out.extend(c.to_uppercase());
            start_of_word = false;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    Ok(Value::String(out))
}

/// "HELLO. how are YOU?" → "Hello. How are you?".
/// Capitalizes the first alphabetic character, plus the first alphabetic
/// character after each `. ! ?` (skipping intervening whitespace).
fn to_sentencecase(args: &[Value]) -> BuiltinResult {
    let s = expect_string("to-sentencecase", &args[0])?;
    let lowered = s.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut capitalize_next = true;
    for c in lowered.chars() {
        if capitalize_next && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(c);
            if matches!(c, '.' | '!' | '?') {
                capitalize_next = true;
            }
        }
    }
    Ok(Value::String(out))
}

/// Generic "split into words" pass used by snake/camel converters.
/// Handles separators (` `, `_`, `-`) and intra-word capital boundaries
/// ("helloWorld" → ["hello", "World"], "XMLParser" → ["XML", "Parser"]).
fn split_words(s: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    for i in 0..chars.len() {
        let c = chars[i];
        if c == '_' || c == '-' || c.is_whitespace() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() {
            let prev = chars[i - 1];
            // lowercase|digit → Uppercase: "hello|World", "foo|2|Bar"
            let lower_to_upper = (prev.is_lowercase() || prev.is_ascii_digit()) && c.is_uppercase();
            // Uppercase → Uppercase followed by lowercase: "XML|Parser".
            let upper_run_end = prev.is_uppercase()
                && c.is_uppercase()
                && i + 1 < chars.len()
                && chars[i + 1].is_lowercase();
            if lower_to_upper || upper_run_end {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn to_snakecase(args: &[Value]) -> BuiltinResult {
    let s = expect_string("to-snakecase", &args[0])?;
    let words: Vec<String> = split_words(s)
        .into_iter()
        .map(|w| w.to_lowercase())
        .collect();
    Ok(Value::String(words.join("_")))
}

fn to_camelcase(args: &[Value]) -> BuiltinResult {
    let s = expect_string("to-camelcase", &args[0])?;
    let words = split_words(s);
    let mut out = String::with_capacity(s.len());
    for (i, w) in words.iter().enumerate() {
        if i == 0 {
            out.push_str(&w.to_lowercase());
        } else {
            let mut chars = w.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(&chars.as_str().to_lowercase());
            }
        }
    }
    Ok(Value::String(out))
}

/// Remove HTML tags. As a defense for config/template contexts, the
/// *content* of `<script>` and `<style>` blocks is also dropped, not just
/// the surrounding tags. HTML entities are left as-is (decode them on the
/// caller side if needed).
fn strip_html(args: &[Value]) -> BuiltinResult {
    let s = expect_string("strip-html", &args[0])?;
    Ok(Value::String(strip_html_impl(s)))
}

fn strip_html_impl(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        match rest.find('<') {
            None => {
                out.push_str(rest);
                break;
            }
            Some(idx) => {
                out.push_str(&rest[..idx]);
                let after = &rest[idx..]; // starts with '<'

                if let Some(skip) = skip_block(after, "script") {
                    rest = &after[skip..];
                    continue;
                }
                if let Some(skip) = skip_block(after, "style") {
                    rest = &after[skip..];
                    continue;
                }
                // <!-- comment -->
                if after.starts_with("<!--") {
                    if let Some(end) = after.find("-->") {
                        rest = &after[end + 3..];
                        continue;
                    } else {
                        break; // unterminated comment: drop the rest
                    }
                }
                // Generic tag — skip to next `>`.
                match after.find('>') {
                    None => break,
                    Some(g) => rest = &after[g + 1..],
                }
            }
        }
    }
    out
}

/// `(spintax pattern seed)` — expand a spintax template by choosing one
/// of the alternatives in each `{a|b|c}` group. Groups can nest. The
/// result is fully deterministic given `seed` — Trongoscript has no
/// ambient RNG, so the caller varies the seed per call to get variety
/// (or holds it fixed to get reproducible output).
///
/// Unbalanced patterns are tolerated: an orphan `{` with no matching `}`
/// is emitted literally rather than raising an error.
fn spintax(args: &[Value]) -> BuiltinResult {
    let pat = expect_string("spintax", &args[0])?;
    let seed = expect_number("spintax", &args[1])?;
    // Trongoscript numbers are JSON numbers; truncate to i64 then to u64.
    let seed_u = seed as i64 as u64;
    let mut state = seed_u;
    Ok(Value::String(spintax_with(pat, &mut state)))
}

/// Expand a spintax template using `state` as the SplitMix64 seed. The
/// state is advanced once per `{...|...}` group consumed, so callers can
/// thread it across multiple invocations to share the same RNG stream.
pub fn spintax_with(pat: &str, state: &mut u64) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let mut idx = 0usize;
    spin_expand(&chars, &mut idx, state, false, 0)
}

/// How deeply `{` groups may nest before further `{` are treated as
/// ordinary characters.
///
/// Expansion recurses once per `{`, on the native stack — the evaluator's
/// call-depth guard cannot see it, so `--max-call-depth` does not apply
/// and an embedder has no way to bound it. A pattern of a million open
/// braces is a plausible stack overflow and, worse, was quadratic: every
/// level that reaches EOF rebuilds a string containing everything below
/// it, so 500k braces took 24 seconds and reported seven evaluation
/// steps.
///
/// Capping the depth bounds both. The limit is far above any real
/// template — nesting past a handful of levels is already unreadable —
/// and past it a `{` is emitted literally, which is what an unbalanced
/// pattern already did.
const MAX_SPIN_DEPTH: usize = 64;

/// SplitMix64 step. Tiny, well-distributed, no allocation, no dependency.
fn splitmix64_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn spin_expand(
    chars: &[char],
    idx: &mut usize,
    state: &mut u64,
    in_group: bool,
    depth: usize,
) -> String {
    let mut alternatives: Vec<String> = Vec::new();
    let mut current = String::new();
    while *idx < chars.len() {
        let c = chars[*idx];
        match c {
            // Past the nesting cap, treat `{` as an ordinary character
            // rather than recursing. Consistent with how an unbalanced
            // pattern is already handled: emit it literally.
            '{' if depth >= MAX_SPIN_DEPTH => {
                *idx += 1;
                current.push(c);
            }
            '{' => {
                *idx += 1;
                let inner = spin_expand(chars, idx, state, true, depth + 1);
                current.push_str(&inner);
            }
            '|' if in_group => {
                *idx += 1;
                alternatives.push(std::mem::take(&mut current));
            }
            '}' if in_group => {
                *idx += 1;
                alternatives.push(std::mem::take(&mut current));
                let n = alternatives.len().max(1);
                let r = (splitmix64_next(state) as usize) % n;
                return alternatives.into_iter().nth(r).unwrap_or_default();
            }
            _ => {
                *idx += 1;
                current.push(c);
            }
        }
    }
    if in_group {
        // EOF inside a group — emit literally to preserve user intent.
        alternatives.push(current);
        let mut s = String::from("{");
        s.push_str(&alternatives.join("|"));
        s
    } else {
        current
    }
}

/// If `s` starts with `<TAG[ />]`, returns the byte offset past the next
/// `</TAG>` (case-insensitive). Otherwise None.
fn skip_block(s: &str, tag: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let tag_bytes = tag.as_bytes();
    if bytes.len() < 1 + tag_bytes.len() + 1 || bytes[0] != b'<' {
        return None;
    }
    for (i, &t) in tag_bytes.iter().enumerate() {
        if !bytes[1 + i].eq_ignore_ascii_case(&t) {
            return None;
        }
    }
    // Char immediately after the tag name must be a tag terminator.
    let next = bytes[1 + tag_bytes.len()];
    if !matches!(next, b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/') {
        return None;
    }
    // Search for closing `</tag>` case-insensitively.
    let close = format!("</{}>", tag);
    let close_bytes = close.as_bytes();
    let s_lower = s.to_ascii_lowercase();
    s_lower.find(close.as_str()).map(|p| p + close_bytes.len())
}

inventory::submit! { Builtin::in_module(MODULE, "lstrip",        1, 1, lstrip) }
inventory::submit! { Builtin::in_module(MODULE, "rstrip",        1, 1, rstrip) }
inventory::submit! { Builtin::in_module(MODULE, "truncate",      2, 2, truncate) }
inventory::submit! { Builtin::in_module(MODULE, "pad-left",      2, 3, pad_left) }
inventory::submit! { Builtin::in_module(MODULE, "pad-right",     2, 3, pad_right) }
inventory::submit! { Builtin::in_module(MODULE, "repeat",        2, 2, repeat) }
inventory::submit! { Builtin::in_module(MODULE, "chars",         1, 1, chars) }
inventory::submit! { Builtin::in_module(MODULE, "lines",         1, 1, lines) }
inventory::submit! { Builtin::in_module(MODULE, "str-reverse",   1, 1, str_reverse) }
inventory::submit! { Builtin::in_module(MODULE, "index-of",      2, 2, index_of) }
inventory::submit! { Builtin::in_module(MODULE, "replace-first", 3, 3, replace_first) }
inventory::submit! { Builtin::in_module(MODULE, "to-uppercase",    1, 1, to_uppercase) }
inventory::submit! { Builtin::in_module(MODULE, "to-lowercase",    1, 1, to_lowercase) }
inventory::submit! { Builtin::in_module(MODULE, "to-titlecase",    1, 1, to_titlecase) }
inventory::submit! { Builtin::in_module(MODULE, "to-sentencecase", 1, 1, to_sentencecase) }
inventory::submit! { Builtin::in_module(MODULE, "to-snakecase",    1, 1, to_snakecase) }
inventory::submit! { Builtin::in_module(MODULE, "to-camelcase",    1, 1, to_camelcase) }
inventory::submit! { Builtin::in_module(MODULE, "strip-html",      1, 1, strip_html) }
inventory::submit! { Builtin::in_module(MODULE, "spintax",         2, 2, spintax) }

#[cfg(test)]
mod tests {
    use super::*;

    fn call(f: fn(&[Value]) -> BuiltinResult, s: &str) -> String {
        match f(&[Value::String(s.into())]).unwrap() {
            Value::String(o) => o,
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn titlecase_basic() {
        assert_eq!(call(to_titlecase, "hello world"), "Hello World");
        assert_eq!(call(to_titlecase, "HELLO WORLD"), "Hello World");
        assert_eq!(call(to_titlecase, ""), "");
    }

    #[test]
    fn sentencecase_basic() {
        assert_eq!(
            call(to_sentencecase, "hello. how are YOU?"),
            "Hello. How are you?"
        );
        assert_eq!(call(to_sentencecase, "ALREADY OK"), "Already ok");
    }

    #[test]
    fn snakecase_handles_inputs() {
        assert_eq!(call(to_snakecase, "helloWorld"), "hello_world");
        assert_eq!(call(to_snakecase, "HelloWorld"), "hello_world");
        assert_eq!(call(to_snakecase, "hello-world"), "hello_world");
        assert_eq!(call(to_snakecase, "hello world"), "hello_world");
        assert_eq!(call(to_snakecase, "XMLParser"), "xml_parser");
        assert_eq!(call(to_snakecase, "already_snake"), "already_snake");
    }

    #[test]
    fn camelcase_handles_inputs() {
        assert_eq!(call(to_camelcase, "hello_world"), "helloWorld");
        assert_eq!(call(to_camelcase, "Hello-World"), "helloWorld");
        assert_eq!(call(to_camelcase, "hello world"), "helloWorld");
        assert_eq!(call(to_camelcase, "HTML-parser"), "htmlParser");
    }

    #[test]
    fn strip_html_basic() {
        assert_eq!(strip_html_impl("<p>hello <b>world</b></p>"), "hello world");
    }

    #[test]
    fn strip_html_drops_script_content() {
        let html = r#"before<script>alert('x')</script>after"#;
        assert_eq!(strip_html_impl(html), "beforeafter");
    }

    #[test]
    fn strip_html_drops_style_content() {
        let html = r#"<style>body{color:red}</style>hi"#;
        assert_eq!(strip_html_impl(html), "hi");
    }

    #[test]
    fn strip_html_drops_comments() {
        assert_eq!(strip_html_impl("a<!-- comment -->b"), "ab");
    }

    #[test]
    fn strip_html_case_insensitive_script() {
        assert_eq!(strip_html_impl("x<SCRIPT>bad()</SCRIPT>y"), "xy");
    }

    fn spin(pat: &str, seed: i64) -> String {
        match spintax(&[Value::String(pat.into()), Value::Number(seed.into())]).unwrap() {
            Value::String(s) => s,
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn spintax_picks_one_alternative() {
        let s = spin("{Hi|Hello|Hey} there", 1);
        assert!(
            matches!(s.as_str(), "Hi there" | "Hello there" | "Hey there"),
            "got {:?}",
            s
        );
    }

    #[test]
    fn spintax_is_deterministic_per_seed() {
        let a = spin("{a|b|c|d} {x|y|z}", 42);
        let b = spin("{a|b|c|d} {x|y|z}", 42);
        assert_eq!(a, b);
    }

    #[test]
    fn spintax_varies_with_seed() {
        // Sample a chunk of seed space — should hit at least two distinct
        // outputs across 16 alternatives.
        let mut outs = std::collections::HashSet::new();
        for seed in 0..32 {
            outs.insert(spin("{red|orange|yellow|green|blue|indigo|violet}", seed));
        }
        assert!(outs.len() >= 2, "no variation across seeds: {:?}", outs);
    }

    #[test]
    fn spintax_nested_groups() {
        let s = spin("{Hi|Hello {there|friend}} — welcome", 7);
        assert!(
            [
                "Hi — welcome",
                "Hello there — welcome",
                "Hello friend — welcome"
            ]
            .contains(&s.as_str()),
            "unexpected output: {:?}",
            s
        );
    }

    #[test]
    fn spintax_all_alternatives_reachable() {
        let mut hit = [false; 4];
        for seed in 0..200 {
            let s = spin("{0|1|2|3}", seed);
            let idx: usize = s.parse().unwrap();
            hit[idx] = true;
        }
        assert!(hit.iter().all(|&b| b), "missed alternatives: {:?}", hit);
    }

    #[test]
    fn spintax_empty_alternative_ok() {
        // Empty branch is a valid choice — "{a|}" can produce "" or "a".
        for seed in 0..16 {
            let s = spin("{a|}", seed);
            assert!(s.is_empty() || s == "a");
        }
    }

    #[test]
    fn spintax_passes_through_plain_text() {
        assert_eq!(spin("no spin here", 0), "no spin here");
    }

    #[test]
    fn spintax_unbalanced_open_brace_emits_literally() {
        // Orphan `{` with no `}` shouldn't crash or eat content.
        assert_eq!(spin("hello {there", 0), "hello {there");
    }

    #[test]
    fn spintax_bounds_nesting_depth() {
        // Recursion here is on the native stack, where the evaluator's
        // depth guard cannot reach. Past the cap a `{` is an ordinary
        // character, so a pathological pattern returns instead of
        // overflowing — this used to be quadratic and did not finish.
        let deep = "{".repeat(1_000_000);
        let out = spin(&deep, 1);
        assert_eq!(
            out.len(),
            1_000_000,
            "every brace past the cap should survive as a literal"
        );
    }

    #[test]
    fn spintax_still_nests_up_to_the_cap() {
        // A group nested just under the cap must still expand normally.
        let depth = MAX_SPIN_DEPTH - 1;
        let pat = format!("{}{}{}", "{".repeat(depth), "a|a", "}".repeat(depth));
        assert_eq!(spin(&pat, 3), "a");
    }

    fn count_err(f: fn(&[Value]) -> BuiltinResult, s: &str, n: f64) -> BuiltinError {
        f(&[Value::String(s.into()), Value::from(n)]).unwrap_err()
    }

    #[test]
    fn repeat_rejects_sizes_it_cannot_allocate() {
        // `f64 as usize` saturates, so an enormous count silently became
        // `usize::MAX` and was handed to the allocator.
        for n in [1e15, 1e308] {
            assert!(
                matches!(count_err(repeat, "ab", n), BuiltinError::Failed { .. }),
                "repeat accepted a count of {n}"
            );
        }
    }

    #[test]
    fn repeat_rejects_negative_counts() {
        assert!(matches!(
            count_err(repeat, "ab", -1.0),
            BuiltinError::ArgError { .. }
        ));
    }

    #[test]
    fn pad_rejects_widths_it_cannot_allocate() {
        // These aborted the process: 1e15 asked the allocator for a
        // petabyte, 1e308 overflowed the usize conversion in raw_vec.
        for f in [pad_left, pad_right] {
            for n in [1e15, 1e308] {
                assert!(
                    matches!(count_err(f, "x", n), BuiltinError::Failed { .. }),
                    "pad accepted a width of {n}"
                );
            }
        }
    }

    #[test]
    fn output_len_rejects_non_finite_counts() {
        // NaN and the infinities cannot reach a builtin through a
        // `Value` — JSON has no encoding for them, so `Value::from`
        // yields null and `expect_number` rejects it first. The guard is
        // still checked directly, since `output_len` is the single place
        // a size is approved and it should not depend on that.
        //
        // NaN matters most: it fails a `< 0.0` test, so the old code let
        // it through and converted it to 0, turning an error into a
        // silently empty string.
        assert!(matches!(
            output_len("f", f64::NAN, 1),
            Err(BuiltinError::ArgError { .. })
        ));
        assert!(matches!(
            output_len("f", f64::INFINITY, 1),
            Err(BuiltinError::Failed { .. })
        ));
        assert!(matches!(
            output_len("f", f64::NEG_INFINITY, 1),
            Err(BuiltinError::ArgError { .. })
        ));
    }

    #[test]
    fn output_len_accepts_sizes_up_to_the_cap() {
        const MAX_OUT: usize = crate::output_limit::DEFAULT_MAX_OUT;
        assert_eq!(output_len("f", 0.0, 1).unwrap(), 0);
        assert_eq!(output_len("f", MAX_OUT as f64, 1).unwrap(), MAX_OUT);
        assert!(output_len("f", MAX_OUT as f64 + 1.0, 1).is_err());
        // The unit multiplies: half the cap at two bytes each is exactly
        // the cap, one more element is over it.
        assert_eq!(
            output_len("f", (MAX_OUT / 2) as f64, 2).unwrap(),
            MAX_OUT / 2
        );
        assert!(output_len("f", (MAX_OUT / 2) as f64 + 1.0, 2).is_err());
    }

    #[test]
    fn output_len_follows_the_configured_ceiling() {
        // The whole point of making it configurable: a tight budget must
        // reject a size the 16 MiB default would have allowed.
        crate::output_limit::with_limit(1024, || {
            assert_eq!(output_len("f", 1024.0, 1).unwrap(), 1024);
            assert!(output_len("f", 1025.0, 1).is_err());
            // Well under the old hardcoded cap, still rejected.
            assert!(output_len("f", 1_000_000.0, 1).is_err());
        });
    }

    #[test]
    fn repeat_follows_the_configured_ceiling() {
        let call = |n: i64| repeat(&[Value::String("x".into()), Value::from(n)]);
        crate::output_limit::with_limit(64, || {
            assert!(call(64).is_ok());
            assert!(call(65).is_err());
        });
        // Outside the scope the default applies again.
        assert!(call(65).is_ok());
    }

    #[test]
    fn oversize_error_names_the_configured_ceiling() {
        crate::output_limit::with_limit(2048, || {
            let e = output_len("f", 1e9, 1).unwrap_err();
            assert!(
                e.to_string().contains("2 KiB"),
                "error should name the active ceiling, got: {e}"
            );
        });
    }

    #[test]
    fn pad_still_pads_ordinary_widths() {
        let got = pad_left(&[Value::String("5".into()), Value::from(3)]).unwrap();
        assert_eq!(got, Value::String("  5".into()));
        let got = pad_right(&[
            Value::String("ab".into()),
            Value::from(5),
            Value::String(".".into()),
        ])
        .unwrap();
        assert_eq!(got, Value::String("ab...".into()));
    }

    #[test]
    fn pad_charges_multibyte_pad_characters_by_byte() {
        // The cap is on bytes, so a 4-byte pad character reaches it at a
        // quarter of the character width a 1-byte one would.
        let width = (crate::output_limit::DEFAULT_MAX_OUT / 2) as f64;
        let err = pad_left(&[
            Value::String("x".into()),
            Value::from(width),
            Value::String("\u{1F600}".into()),
        ])
        .unwrap_err();
        assert!(matches!(err, BuiltinError::Failed { .. }));
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
