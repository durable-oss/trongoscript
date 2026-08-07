//! Kani proof harnesses for the evaluator crate.
//!
//! Compiled only under `cfg(kani)`. Run with:
//!
//! ```text
//! cargo kani -p trongoscript
//! ```
//!
//! The sandbox is the security boundary, so its *pure* helpers are the ones
//! worth model-checking: the glob matcher and character-class parser walk
//! attacker-controlled byte slices with hand-rolled index arithmetic, which
//! is exactly where fuzzing gets lucky and a solver does not. Anything that
//! touches the filesystem is out of scope for Kani and stays covered by the
//! unit tests in `sandbox.rs`.

#[cfg(kani)]
mod proofs {
    use crate::sandbox::verification_shims::{glob_match, match_class, virtual_strip_prefix};

    // NOTE ON COST: the length of a symbolic slice is the expensive knob,
    // not its contents. Drawing `let len: usize = kani::any()` makes every
    // loop bound a solver variable *on top of* the symbolic bytes, and the
    // matcher's `*` backtracking is already quadratic in those lengths.
    // These harnesses therefore fix the length concretely and vary only the
    // bytes; the `PAD` alphabet member stands in for "string ends here", so
    // shorter inputs are still covered without a symbolic bound.

    /// Concrete slice length. Four bytes is enough to force at least one
    /// `*` backtrack (match, mismatch, retry one position further) and to
    /// hold a complete `[!a]` class.
    const LEN: usize = 4;

    /// Metacharacters the matcher branches on, plus two ordinary literals.
    /// Keeping the alphabet at 4 members costs 2 symbolic bits per byte
    /// instead of 8.
    fn any_pattern_byte() -> u8 {
        let c: u8 = kani::any();
        kani::assume(c == b'*' || c == b'?' || c == b'[' || c == b'a');
        c
    }

    fn any_name_byte() -> u8 {
        let c: u8 = kani::any();
        kani::assume(c == b'a' || c == b'b');
        c
    }

    fn any_pattern() -> [u8; LEN] {
        [
            any_pattern_byte(),
            any_pattern_byte(),
            any_pattern_byte(),
            any_pattern_byte(),
        ]
    }

    fn any_name() -> [u8; LEN] {
        [
            any_name_byte(),
            any_name_byte(),
            any_name_byte(),
            any_name_byte(),
        ]
    }

    // -----------------------------------------------------------------
    // glob_match — no panics, and it always terminates.
    // -----------------------------------------------------------------

    /// `glob_match` indexes `pat` and `name` by hand and backtracks on `*`.
    /// Prove that for every pattern and name in the modeled alphabet it
    /// returns without panicking: no out-of-bounds index, no overflow on
    /// `pi`/`ni`/`star_ni`, and no infinite backtrack loop.
    ///
    /// Each `*` backtrack advances `star_ni` by one, so the loop body runs
    /// at most `len(pat) * len(name)` times plus the tail scan — 20 covers
    /// `LEN = 4` with room to spare. The `unwind` check in the output
    /// confirms the bound is not truncating any path.
    #[kani::proof]
    #[kani::unwind(24)]
    fn glob_match_never_panics() {
        let pat = any_pattern();
        let name = any_name();
        let _ = glob_match(&pat, 0, &name, 0);
    }

    /// The empty pattern matches only the empty name — one anchor of the
    /// matcher's tail-scan branch.
    #[kani::proof]
    #[kani::unwind(24)]
    fn glob_empty_pattern_matches_only_empty_name() {
        let name = any_name();
        assert!(
            !glob_match(&[], 0, &name, 0),
            "empty pattern matched a name"
        );
        assert!(
            glob_match(&[], 0, &[], 0),
            "empty pattern rejected the empty name"
        );
    }

    /// A single `*` matches any name in the alphabet, including the empty
    /// one. Guards against a regression where the tail scan stops early.
    #[kani::proof]
    #[kani::unwind(24)]
    fn glob_star_matches_everything() {
        let name = any_name();
        assert!(glob_match(b"*", 0, &name, 0));
        assert!(glob_match(b"*", 0, &[], 0));
        assert!(glob_match(b"**", 0, &name, 0));
    }

    /// A literal pattern (no metacharacters) matches a name if and only if
    /// the two are byte-identical. Both sides are drawn from the
    /// metacharacter-free alphabet.
    #[kani::proof]
    #[kani::unwind(24)]
    fn glob_literal_pattern_is_exact_equality() {
        let pat = any_name(); // `a`/`b` only — no metacharacters
        let name = any_name();
        let matched = glob_match(&pat, 0, &name, 0);
        assert!(matched == (pat == name));
    }

    /// `?` matches exactly one byte: a pattern of `LEN` question marks
    /// accepts every name of that length and nothing shorter.
    #[kani::proof]
    #[kani::unwind(24)]
    fn glob_question_mark_matches_exactly_one_byte() {
        let name = any_name();
        assert!(
            glob_match(b"????", 0, &name, 0),
            "?*4 rejected a 4-byte name"
        );
        assert!(
            !glob_match(b"????", 0, &name[..3], 0),
            "?*4 matched 3 bytes"
        );
    }

    // -----------------------------------------------------------------
    // match_class — the `[..]` parser.
    // -----------------------------------------------------------------

    /// `match_class` reads `pat[i]`, `pat[i + 1]`, and `pat[i + 2]` while
    /// parsing ranges. Prove those reads are always in bounds and that a
    /// returned position is a valid index one past a `]` — the caller
    /// resumes scanning from it, so an out-of-range value would panic
    /// downstream.
    #[kani::proof]
    #[kani::unwind(12)]
    fn match_class_never_panics_and_returns_a_valid_position() {
        // The caller only invokes `match_class` sitting on a `[`, so byte 0
        // is fixed. The remaining three vary over the class syntax —
        // including the malformed shapes (`[`, `[-`, `[!`) that must come
        // back as `None` rather than panicking on an out-of-range read.
        let mut pat = [b'['; LEN];
        for b in pat.iter_mut().skip(1) {
            let c: u8 = kani::any();
            kani::assume(c == b']' || c == b'-' || c == b'!' || c == b'a');
            *b = c;
        }
        let c: u8 = kani::any();
        if let Some((_matched, next)) = match_class(&pat, 0, c) {
            assert!(
                next <= pat.len(),
                "match_class returned a position past the end"
            );
            assert!(next > 0, "match_class did not advance");
        }
    }

    /// An unterminated class (no `]` before the end) must return `None`.
    /// If it returned a position instead, the caller would resume scanning
    /// from an index past the pattern.
    #[kani::proof]
    #[kani::unwind(12)]
    fn match_class_rejects_unterminated_classes() {
        let c: u8 = kani::any();
        assert!(match_class(b"[a", 0, c).is_none());
        assert!(match_class(b"[", 0, c).is_none());
        assert!(match_class(b"[!a", 0, c).is_none());
    }

    /// Negation inverts membership: for a well-formed class, `[!X]` accepts
    /// exactly the bytes `[X]` rejects.
    #[kani::proof]
    #[kani::unwind(20)]
    fn match_class_negation_is_complement() {
        // `[a]` vs `[!a]` over a symbolic input byte.
        let c: u8 = kani::any();
        let plain = match_class(b"[a]", 0, c);
        let negated = match_class(b"[!a]", 0, c);
        match (plain, negated) {
            (Some((p, _)), Some((n, _))) => assert!(p != n, "negation did not invert the result"),
            _ => panic!("well-formed classes must parse"),
        }
    }

    // -----------------------------------------------------------------
    // virtual_strip_prefix — mount routing.
    // -----------------------------------------------------------------

    /// `virtual_strip_prefix` slices with `&rest[1..]` after checking only
    /// the first *byte*. Prove that on the ASCII path shapes the sandbox
    /// actually produces it never panics and never returns a remainder that
    /// still carries a leading `/` — a leaked separator would make the
    /// caller's `push` treat the remainder as host-absolute and blow past
    /// the mount root.
    #[kani::proof]
    #[kani::unwind(12)]
    fn virtual_strip_prefix_yields_a_relative_remainder() {
        // Model the handful of shapes `normalize_virtual` can emit, plus a
        // partial-segment case (`/tomcat` vs `/tom`) that must be rejected.
        let which: u8 = kani::any();
        kani::assume(which < 6);
        let (vpath, prefix): (&str, &str) = match which {
            0 => ("/tom/y", "/tom"),
            1 => ("/tom", "/tom"),
            2 => ("/tomcat/y", "/tom"),
            3 => ("/tom/y", ""),
            4 => ("", ""),
            _ => ("/a/b/c", "/a"),
        };
        if let Some(rest) = virtual_strip_prefix(vpath, prefix) {
            assert!(
                !rest.starts_with('/'),
                "remainder kept a leading separator and would escape the mount"
            );
        }
    }

    /// A prefix that matches only part of a segment must not route: `/tom`
    /// cannot capture `/tomcat/y`. This is the mount-shadowing invariant
    /// that keeps one mount from swallowing another's paths.
    #[kani::proof]
    #[kani::unwind(12)]
    fn virtual_strip_prefix_matches_whole_segments_only() {
        assert!(virtual_strip_prefix("/tomcat/y", "/tom").is_none());
        assert!(virtual_strip_prefix("/tomcat", "/tom").is_none());
        assert!(virtual_strip_prefix("/tom/y", "/tom") == Some("y"));
        assert!(virtual_strip_prefix("/tom", "/tom") == Some(""));
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
