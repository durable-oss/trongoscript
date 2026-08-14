# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# Builtins whose running time is superlinear in their input.
#
# This is the failure mode README bug #6 describes, moved inside a single
# builtin. The step counter charges per *call*, so a builtin that takes
# quadratic time on a linear input is invisible to it: the script reports
# a handful of steps and runs for minutes. No step budget, however tight,
# can stop it — the budget is never consulted again until the call
# returns.
#
# The fix for such a builtin is either a better algorithm or a fuel charge
# proportional to the work it will do. Both are fine; a test here only
# asserts that the interpreter comes back.
#
# The inputs below are sized so that a *linear* implementation finishes in
# well under a second, while a quadratic one exceeds the harness timeout.
# That keeps the suite fast when the code is right, and makes a regression
# unmistakable rather than merely slow.
class TestBuiltinComplexity < StressTestCase
  # `coll:uniq` compares each element against everything kept so far, so
  # an input of n distinct elements costs O(n^2) comparisons. 400k
  # distinct elements is ~80 billion comparisons and does not return.
  #
  # Measured before the fix: 20k took 0.39s, 40k took 1.5s, 80k took 6.1s
  # — quadrupling per doubling — and every run reported `steps=5`.
  def test_uniq_over_distinct_elements_terminates
    result = run_source(
      "(use collections)\n(count (uniq (range 200000)))",
      limits: { max_steps: 10_000_000 }
    )
    assert_diagnosable(
      result,
      "(uniq) over 200k distinct elements did not come back. Its scan is " \
      "O(n^2) and costs one step, so no step budget can bound it — it needs " \
      "a set-based lookup or a fuel charge proportional to the input"
    )
  end

  # The same builtin on the same input length, but with few distinct
  # values, stays fast even with the quadratic scan — the kept-set never
  # grows. Pinning this separately keeps the test above honest about
  # *which* input shape is the expensive one.
  def test_uniq_over_repeated_elements_is_fast
    assert_completes(
      run_source(
        '(use collections)
         (count (uniq (map (fn [i] (mod i 10)) (range 200000))))',
        limits: { max_steps: 10_000_000 }
      ),
      "uniq over 10 distinct values should be cheap regardless"
    )
  end

  # The fix for bug #10 sent *scalars* through a hash set. Composites
  # cannot be hashed as `serde_json::Value`, so they kept the pairwise
  # scan — which means the original quadratic is still reachable, just
  # behind a `[i]` instead of an `i`.
  #
  # Measured on the release build after that fix: 40k distinct
  # single-element lists took 3.9s, and 120k did not return inside 20s.
  # The scalar case at 200k finishes in 53 ms.
  def test_uniq_over_distinct_composite_elements_terminates
    assert_diagnosable(
      run_source(
        '(use collections)
         (count (uniq (map (fn [i] [i]) (range 200000))))',
        limits: { max_steps: 10_000_000 }
      ),
      "(uniq) over 200k distinct *composite* elements did not come back. " \
      "The hash-set fix covers scalars only; composites still fall back to " \
      "an O(n^2) scan with deep equality, so the same attack works with " \
      "every element wrapped in a list"
    )
  end

  # Deep equality is O(size), so quadratic comparisons over large
  # elements multiply: the cost is O(n^2 * size), and only n is visible
  # in the source.
  def test_uniq_over_large_composite_elements_terminates
    assert_diagnosable(
      run_source(
        '(use collections strings)
         (count (uniq (map (fn [i] [i (repeat "x" 500)]) (range 20000))))',
        limits: { max_steps: 10_000_000 }
      ),
      "pairwise deep equality over 20k half-kilobyte elements must terminate"
    )
  end

  # `contains?` on a list is the same linear scan with the same deep
  # equality, so a list of large composites costs O(n * size) per call.
  def test_contains_over_large_composite_elements_is_bounded
    assert_diagnosable(
      run_source(
        '(use collections strings)
         (let [xs (map (fn [i] [i (repeat "x" 1000)]) (range 20000))]
           (count (map (fn [i] (contains? xs [-1 "y"])) (range 2000))))',
        limits: { max_steps: 1_000_000 }
      ),
      "2000 scans of a 20k-element list of 1 KB values must be bounded"
    )
  end

  # `str:spintax` recurses once per `{` and, on the way back out, joins
  # the accumulated alternatives — so a run of nested open braces is
  # quadratic in the nesting depth.
  #
  # Measured before the fix: 200k braces took 2.8s, 300k took 7.6s, 500k
  # took 23.8s, and 1M did not return inside 20s. Reported cost: 7 steps.
  def test_spintax_over_nested_braces_terminates
    result = run_source(
      '(use strings)
       (str-len (spintax (repeat "{" 400000) 1))',
      limits: { max_steps: 10_000_000 }
    )
    assert_diagnosable(
      result,
      "(spintax) over 400k nested braces did not come back. Expansion is " \
      "O(depth^2) and costs one step; it needs a nesting limit or a fuel " \
      "charge proportional to the pattern length"
    )
  end

  # Depth is the expensive axis, not length: the same pattern size in
  # flat alternations expands in milliseconds. If this test ever slows
  # down, the problem is in the scan rather than the recursion.
  def test_spintax_over_flat_alternations_is_fast
    assert_completes(
      run_source(
        '(use strings)
         (str-len (spintax (repeat "{a|b}" 200000) 1))',
        limits: { max_steps: 10_000_000 }
      ),
      "flat spintax alternations should expand in linear time"
    )
  end

  # `spintax` recursion is native stack, not evaluator stack, so
  # `--max-call-depth` does not apply to it. A depth that the evaluator
  # would refuse must still not overflow the process when it happens
  # inside one builtin call.
  def test_spintax_nesting_does_not_overflow_the_stack
    result = run_source(
      '(use strings)
       (str-len (spintax (repeat "{" 1000000) 1))',
      limits: { max_call_depth: 64, max_steps: 10_000_000 },
      timeout: 30
    )
    assert_diagnosable(
      result,
      "1M nested braces inside a single builtin call must not crash or hang " \
      "— the evaluator's depth guard cannot see native recursion"
    )
  end

  # Substring search is linear in most implementations but has quadratic
  # worst cases. A long run of one character searched for a near-miss is
  # the classic trigger.
  def test_substring_search_worst_case_terminates
    assert_diagnosable(
      run_source(
        '(use strings)
         (index-of (repeat "a" 4000000) (str (repeat "a" 2000000) "b"))',
        limits: { max_steps: 1_000_000 }
      ),
      "index-of on its quadratic worst case must terminate"
    )
  end

  # `contains?` scans a list per call. Calling it inside a map over the
  # same list is quadratic — but unlike `uniq` above, this one *is*
  # visible to the step counter, because each scan is a separate call.
  # It is bounded by the step budget rather than by a timeout, which is
  # the behavior every quadratic builtin should have.
  #
  # The contrast with `test_uniq_over_distinct_elements_terminates` is
  # the point: same asymptotics, but the work here crosses the evaluator
  # once per scan, so the meter sees it.
  def test_repeated_membership_scans_are_charged
    assert_source_limited(
      '(use collections)
       (count (map (fn [i] (contains? (range 3000) 2999)) (range 3000)))',
      /step/i,
      limits: { max_steps: 5_000 },
      msg: "n scans of an n-element list must be charged per call"
    )
  end

  # Sorting is O(n log n) with comparisons that are themselves O(size) on
  # nested values, so a list of large equal-prefix values costs much more
  # per comparison than the element count suggests.
  def test_sort_with_expensive_comparisons_terminates
    assert_diagnosable(
      run_source(
        '(use collections strings)
         (count (sort (map (fn [i] (str (repeat "x" 2000) i)) (range 20000))))',
        limits: { max_steps: 10_000_000 }
      ),
      "sorting 20k long-common-prefix strings must terminate"
    )
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
