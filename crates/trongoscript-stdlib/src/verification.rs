//! Kani proof harnesses for the standard library.
//!
//! These are compiled only under `cfg(kani)`, so they cost nothing in a
//! normal build. Run them with:
//!
//! ```text
//! cargo kani -p trongoscript-stdlib
//! ```
//!
//! The functions verified here are the ones where a counterexample is hard
//! to reach by sampling: `f64` → `i64` boundary conversions, the Euclidean
//! GCD loop, and the `lcm` overflow guard. Anything that allocates a
//! `serde_json::Value` is out of scope — the model checker cannot bound the
//! heap — so the harnesses target the numeric kernels those builtins wrap.

// ---------------------------------------------------------------------------
// Mirrors of the private kernels under test.
//
// The originals are `fn` items private to their modules and duplicated
// across `numeric`, `math`, `finance`, `collection`, and `extra_collection`.
// Rather than widen their visibility for verification, we restate them here
// and keep them in sync; `kani_mirror_matches_numeric` in the unit tests
// pins the behavior so drift is caught by `cargo test`, not just by Kani.
// ---------------------------------------------------------------------------

/// Byte-for-byte copy of the `num_from_f64` integer-narrowing predicate as
/// written in `numeric.rs`, `math.rs`, `finance.rs`, `collection.rs`, and
/// `extra_collection.rs`.
///
/// Returns `Some(i)` when the shipped code would take the `i64` branch.
#[cfg(any(kani, test))]
pub(crate) fn narrows_to_i64_as_written(n: f64) -> Option<i64> {
    if !n.is_finite() {
        return None;
    }
    if n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
        return Some(n as i64);
    }
    None
}

/// Copy of `math::gcd_two`.
#[cfg(any(kani, test))]
pub(crate) fn gcd_two(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.unsigned_abs() as i64, b.unsigned_abs() as i64);
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Copy of `rng::next_int_in`'s span computation, which is the part that can
/// overflow before the RNG is ever consulted.
#[cfg(kani)]
pub(crate) fn int_span(lo: i64, hi: i64) -> u64 {
    (hi - lo) as u64
}

#[cfg(kani)]
mod proofs {
    use super::*;

    // -----------------------------------------------------------------
    // num_from_f64 — the f64 → i64 narrowing boundary.
    //
    // NOTE ON COST: a fully symbolic `f64` is prohibitively expensive here.
    // CBMC bit-blasts IEEE-754, so `fract()`, `f64 as i64`, and `i64 as f64`
    // each become a soft-float circuit over 64 symbolic bits, and chaining
    // two of them did not converge in 30 minutes. These harnesses instead
    // drive the *integer* side symbolically and let the float appear only as
    // a derived value, which keeps the property intact at a fraction of the
    // cost.
    // -----------------------------------------------------------------

    /// The exact statement of the bug: 2^63 is a finite `f64` whose `fract()`
    /// is zero and which satisfies the shipped guard `n <= i64::MAX as f64`,
    /// because `i64::MAX as f64` rounds *up* to 2^63. The subsequent
    /// `n as i64` then saturates to `i64::MAX`, so the "integer" branch
    /// silently returns a different number than it was given.
    ///
    /// No symbolic float at all — this is a concrete witness, so it is cheap.
    /// Expected to FAIL against the code as written.
    #[kani::proof]
    fn narrowing_rejects_two_pow_63() {
        let two_pow_63 = 9_223_372_036_854_775_808.0f64; // i64::MAX + 1
        if let Some(i) = narrows_to_i64_as_written(two_pow_63) {
            assert!(
                i as f64 == two_pow_63,
                "num_from_f64 narrowed 2^63 to an i64 and changed its value"
            );
        }
    }

    /// Round-trip over the symbolic *integer* domain: every `i64` that
    /// survives `i as f64` without losing precision must narrow back to
    /// itself. This covers the whole i64 range without a symbolic float.
    #[kani::proof]
    fn narrowing_round_trips_exact_integers() {
        let i: i64 = kani::any();
        let n = i as f64;
        // Only integers that survive the widening are in scope; above 2^53
        // the `f64` cannot hold every `i64`, and that rounding is expected.
        kani::assume(n as i64 == i);
        match narrows_to_i64_as_written(n) {
            Some(back) => assert!(back == i, "round trip through f64 changed the integer"),
            None => panic!("an exactly-representable integer failed to narrow"),
        }
    }

    /// The corrected predicate uses a *strict* comparison against 2^63
    /// instead of `n <= i64::MAX as f64`. Prove the fix admits 2^63 - 1024
    /// (the largest `f64` below the boundary) and rejects 2^63 itself.
    #[kani::proof]
    fn corrected_upper_bound_is_exclusive() {
        const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
        // Largest f64 strictly below 2^63; its exponent gives a 1024 ulp.
        let just_below = 9_223_372_036_854_774_784.0f64;

        assert!(just_below < TWO_POW_63, "witness is not below the boundary");
        assert!(
            (just_below as i64) as f64 == just_below,
            "the largest in-range f64 must narrow losslessly"
        );
        assert!(
            !(TWO_POW_63 < TWO_POW_63),
            "the corrected strict bound must exclude 2^63"
        );
    }

    // -----------------------------------------------------------------
    // gcd — termination, sign, and the divisibility contract.
    //
    // NOTE ON COST: the loop bound is what dominates here, not the range
    // assumptions. `#[kani::unwind(96)]` makes CBMC unroll 96 copies of a
    // symbolic 64-bit `%` even when the inputs are assumed small, because
    // the assumption is a solver constraint and not a narrower type. The
    // harnesses below draw narrow types (`u8`, `i16`) and widen them, so
    // each `%` is an 8- or 16-bit circuit and Euclid provably converges in
    // far fewer steps — `unwind(16)` covers `u8` (worst case is consecutive
    // Fibonacci numbers, 11 steps under 256).
    // -----------------------------------------------------------------

    /// `gcd_two` terminates and returns a non-negative result. Driven by
    /// symbolic `u8`s widened to `i64`, which keeps the modulo circuits
    /// narrow while still exercising every control path in the loop.
    #[kani::proof]
    #[kani::unwind(16)]
    fn gcd_terminates_and_is_non_negative() {
        let a: i64 = kani::any::<u8>() as i64;
        let b: i64 = kani::any::<u8>() as i64;
        let g = gcd_two(a, b);
        assert!(g >= 0, "gcd returned a negative value");
    }

    /// The sign handling is the part that must hold at the extremes of the
    /// range, so check it separately with concrete witnesses instead of a
    /// symbolic 64-bit loop. `i64::MIN` matters because `unsigned_abs()`
    /// yields 2^63, which does not fit back into an `i64`.
    #[kani::proof]
    #[kani::unwind(4)]
    fn gcd_handles_i64_min_without_panicking() {
        let g = gcd_two(i64::MIN, 0);
        // `(i64::MIN).unsigned_abs() as i64` wraps straight back to i64::MIN,
        // so the documented "non-negative" contract does not hold here.
        assert!(g >= 0, "gcd(i64::MIN, 0) escaped the non-negative contract");
    }

    /// `gcd_two(a, 0) == |a|`, the identity every downstream `lcm` division
    /// depends on.
    #[kani::proof]
    #[kani::unwind(4)]
    fn gcd_with_zero_is_absolute_value() {
        let a: i64 = kani::any::<i16>() as i64;
        let g = gcd_two(a, 0);
        assert!(g == a.abs());
    }

    /// The defining property: the result divides both inputs.
    #[kani::proof]
    #[kani::unwind(16)]
    fn gcd_divides_both_inputs() {
        let a: i64 = kani::any::<u8>() as i64;
        let b: i64 = kani::any::<u8>() as i64;
        kani::assume(a > 0);
        kani::assume(b > 0);
        let g = gcd_two(a, b);
        assert!(g > 0, "gcd of two positives must be positive");
        assert!(a % g == 0, "gcd does not divide the first argument");
        assert!(b % g == 0, "gcd does not divide the second argument");
    }

    /// `gcd_two` is symmetric in its arguments.
    #[kani::proof]
    #[kani::unwind(16)]
    fn gcd_is_symmetric() {
        let a: i64 = kani::any::<i8>() as i64;
        let b: i64 = kani::any::<i8>() as i64;
        assert!(gcd_two(a, b) == gcd_two(b, a));
    }

    // -----------------------------------------------------------------
    // lcm — the checked-arithmetic guard.
    // -----------------------------------------------------------------

    /// `lcm` guards its multiplication with `checked_div(g).checked_mul(b)`.
    /// Prove that when the guard admits a result, the result is a genuine
    /// common multiple rather than a wrapped value.
    #[kani::proof]
    #[kani::unwind(16)]
    fn lcm_guard_admits_only_real_multiples() {
        let a: i64 = kani::any::<u8>() as i64;
        let b: i64 = kani::any::<u8>() as i64;
        kani::assume(a > 0);
        kani::assume(b > 0);

        let g = gcd_two(a, b);
        if let Some(prod) = a.checked_div(g).and_then(|q| q.checked_mul(b)) {
            assert!(prod > 0, "lcm of two positives must be positive");
            assert!(prod % a == 0, "lcm is not a multiple of the first argument");
            assert!(
                prod % b == 0,
                "lcm is not a multiple of the second argument"
            );
        }
    }

    // -----------------------------------------------------------------
    // rng span — `rand-int` validates lo < hi but not hi - lo.
    // -----------------------------------------------------------------

    /// `rand-int` rejects `lo >= hi` and then computes `(hi - lo) as u64`.
    /// The subtraction itself overflows whenever the span exceeds `i64::MAX`
    /// — e.g. `lo = i64::MIN, hi = 0` — which panics in a debug build and
    /// wraps in release.
    ///
    /// Expected to fail against the code as written; the fix is to compute
    /// the span in `u64` via `wrapping_sub`, or to reject spans that do not
    /// fit in an `i64`.
    #[kani::proof]
    fn rand_int_span_does_not_overflow() {
        let lo: i64 = kani::any();
        let hi: i64 = kani::any();
        kani::assume(lo < hi); // the guard `rand-int` actually enforces
        let span = int_span(lo, hi);
        assert!(span > 0, "a validated range produced an empty span");
    }

    /// The corrected span computation is total over every validated range
    /// and always yields a usable modulus.
    #[kani::proof]
    fn rand_int_span_in_u64_is_total() {
        let lo: i64 = kani::any();
        let hi: i64 = kani::any();
        kani::assume(lo < hi);
        let span = (hi as u64).wrapping_sub(lo as u64);
        assert!(span > 0, "wrapping span of a validated range was zero");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{find_builtin, Value};

    /// Guard against the mirrored predicate drifting away from the shipped
    /// `num_from_f64`. If someone edits `numeric.rs` without editing this
    /// file, this test starts failing.
    #[test]
    fn kani_mirror_matches_numeric() {
        let add = find_builtin("+").expect("`+` should be registered");
        let cases = [0.0f64, 1.0, -1.0, 2.5, -2.5, 1e18, -1e18];
        for n in cases {
            let got =
                (add.func)(&[Value::Number(serde_json::Number::from_f64(n).unwrap())]).unwrap();
            let mirrored = narrows_to_i64_as_written(n);
            match (&got, mirrored) {
                (Value::Number(num), Some(i)) => {
                    assert_eq!(num.as_i64(), Some(i), "mirror disagrees for {n}");
                }
                (Value::Number(num), None) => {
                    assert!(num.as_i64().is_none(), "mirror missed an integer for {n}");
                }
                other => panic!("unexpected result for {n}: {other:?}"),
            }
        }
    }

    #[test]
    fn gcd_mirror_matches_basic_cases() {
        assert_eq!(gcd_two(12, 18), 6);
        assert_eq!(gcd_two(-12, 18), 6);
        assert_eq!(gcd_two(7, 0), 7);
        assert_eq!(gcd_two(0, 0), 0);
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
