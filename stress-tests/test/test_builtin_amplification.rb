# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# Builtins that can return far more than they were given.
#
# The evaluator's step counter charges one step per call. That is honest
# for `(+ 1 2)` and badly wrong for a builtin whose output size is chosen
# by its arguments: a single call can allocate gigabytes, or run for
# minutes, while the meter reads "one step".
#
# `fuel::predicted_output_len` closes that gap for the builtins it knows
# about, by rejecting an oversized request *before* the call runs. It
# currently knows about `range` and `concat`. Every other size-amplifying
# builtin is caught only afterwards, by the value-size cap on the result —
# which works when the intermediate fits in memory, and does not when it
# doesn't.
#
# These tests pin down which builtins are actually bounded, so that adding
# one to the stdlib without considering its output size shows up here.
class TestBuiltinAmplification < StressTestCase
  # --- Single-call size amplification -------------------------------

  # `(str:repeat s n)` multiplies its input by n. Unbounded, this is the
  # cheapest possible OOM: eight bytes of source asking for terabytes.
  def test_repeat_rejects_oversized_output
    result = run_source(
      '(use strings)
       (str-len (repeat "abcdefgh" 1000000000))',
      limits: { max_steps: 100_000 }
    )
    assert_diagnosable(result, "(repeat s 1e9) must not be attempted")
    refute_equal TrongoHarness::EXIT_OK, result.exitstatus,
                 "a 8 GB string was produced without complaint"
  end

  # The single-call output ceiling shared by `repeat`, `pad-left` and
  # `pad-right` used to be a hardcoded 16 MiB, independent of the fuel
  # budget: an embedder could not tighten it, so a script could allocate
  # 16 MiB per call no matter what `--max-alloc` said. It is now derived
  # from the budget — the tighter of `max_value_len` and what is left of
  # `max_alloc` — with 16 MiB remaining only as the default.
  #
  # The rejection must happen *before* the allocation. Charging after the
  # fact is what gets a process OOM-killed with nothing reported, which is
  # why the ceiling exists at all.
  def test_repeat_ceiling_follows_the_fuel_budget
    result = run_source(
      '(use strings)
       (str-len (repeat "abcdefgh" 2000000))',
      limits: { max_alloc: 1_000 }
    )
    assert_diagnosable(result, "a 16 MiB repeat under a 1 KB alloc budget")
    refute_equal TrongoHarness::EXIT_OK, result.exitstatus,
                 "(repeat) allocated 16 MiB while --max-alloc was 1000 — its " \
                 "internal cap is bypassing the configured allocation budget"
    assert_match(/output would exceed/, result.stderr,
                 "the rejection should come from the pre-allocation size " \
                 "check, not from charging the result afterwards")
  end

  # The flip side: with no budget configured the default ceiling still
  # applies, so the backstop against a single crashing allocation is
  # never simply absent.
  def test_default_output_ceiling_still_applies_without_a_budget
    result = run_source('(use strings)
                         (str-len (repeat "x" 100000000))')
    assert_diagnosable(result, "a 100 MB repeat under default limits")
    refute_equal TrongoHarness::EXIT_OK, result.exitstatus,
                 "(repeat) built a 100 MB string with no ceiling in force"
  end

  # A closure returning the same large captured value on every call is
  # amplification without a size argument to inspect: `map` makes n
  # cheap-looking calls and assembles n copies of something big.
  #
  # Nothing used to charge for it. `call_closure` bills *steps* for
  # copying an argument in; nothing billed *bytes* for the list coming
  # out. 2000 x 1 MB allocated about 2 GB and reported about 1 MB, so
  # `--max-alloc` could not see it and a tight RLIMIT_AS aborted the
  # process instead of reporting anything.
  def test_map_replicating_a_large_value_is_charged
    result = run_source(
      '(use strings)
       (let [big (repeat "x" 1000000)]
         (count (map (fn [i] big) (range 2000))))'
    )
    assert_resource_limited(
      result,
      "map built ~2 GB from a 1 MB value while the allocation budget saw " \
      "only the one copy — each element must be charged as it lands"
    )
  end

  # The charge has to happen per element, not on the finished list.
  # Billing at the end only reports a result the process survived
  # building, which is the failure mode this whole file exists for: the
  # peak RSS must stay near the budget rather than near the full 2 GB.
  def test_map_amplification_is_stopped_before_the_memory_is_committed
    result = run_source(
      '(use strings)
       (let [big (repeat "x" 1000000)]
         (count (map (fn [i] big) (range 2000))))',
      limits: { max_alloc: 10_000_000 }
    )
    assert_resource_limited(result, "a 10 MB budget must stop a 2 GB map")
  end

  # The same shape through the other collecting forms. `substitute-if`
  # and `subst-if` copy a replacement value once per match, which
  # amplifies for exactly the same reason — so each predicate here has to
  # match *every* element, or the test measures one copy and proves
  # nothing.
  def test_collecting_higher_order_forms_charge_their_output
    {
      "(count (mapcan (fn [i] [big big]) (range 2000)))" => "mapcan",
      "(count (substitute-if big (fn [x] true) (range 2000)))" => "substitute-if",
      "(count (subst-if big (fn [x] (number? x)) (range 2000)))" => "subst-if"
    }.each do |expr, fn|
      result = run_source(
        "(use strings)\n(let [big (repeat \"x\" 1000000)]\n  #{expr})"
      )
      assert_diagnosable(result, "#{fn} amplified a 1 MB value uncharged")
      refute_equal TrongoHarness::EXIT_OK, result.exitstatus,
                   "#{fn} produced gigabytes without the budget noticing"
    end
  end

  # Padding to a chosen width is the same amplification as repeat, but
  # `pad_with` has no equivalent of repeat's 16 MiB pre-check: the width
  # goes from an f64 straight into the allocation.
  #
  # At 5e8 the result is merely large and the value-size cap catches it
  # after the fact. At 1e15 the allocator is asked for a petabyte and
  # aborts the process, and at 1e308 the usize conversion overflows and
  # panics in `raw_vec`. Both are crashes, not errors.
  def test_pad_rejects_oversized_width
    %w[pad-left pad-right].each do |fn|
      %w[500000000 999999999999999 1e308].each do |width|
        assert_diagnosable(
          run_source("(use strings)\n(str-len (#{fn} \"x\" #{width} \"a\"))"),
          "(#{fn} \"x\" #{width}) must be rejected before it is allocated — " \
          "pad needs the size pre-check that (repeat) already has"
        )
      end
    end
  end

  # Repeatedly doubling a string reaches gigabytes in a handful of
  # iterations, so a per-iteration step charge never catches it — only a
  # size check on the value does.
  def test_string_doubling_loop_is_bounded
    assert_diagnosable(
      run_source(
        '(use strings collections)
         (str-len (reduce (fn [acc x] (replace acc "a" "aa")) "aaaa" (range 40)))',
        limits: { max_steps: 1_000_000 }
      ),
      "40 rounds of doubling a string is 4 TB"
    )
  end

  # Same shape through `repeat`, which squares rather than doubles.
  def test_string_squaring_loop_is_bounded
    assert_diagnosable(
      run_source(
        '(use strings collections)
         (str-len (reduce (fn [acc x] (repeat acc 2)) "aaaa" (range 40)))',
        limits: { max_steps: 1_000_000 }
      ),
      "squaring a string 40 times must not be attempted"
    )
  end

  # --- List-producing builtins --------------------------------------

  # `zip` and `partition` produce O(n) output from O(n) input, so they
  # cannot amplify — but they allocate a fresh nested structure per
  # element, which the allocation budget must see.
  def test_list_restructuring_is_charged_for_allocation
    assert_source_limited(
      '(use collections)
       (count (zip (range 200000) (range 200000)))',
      /alloc/i,
      limits: { max_alloc: 100_000 },
      msg: "zip's output allocation must be charged"
    )
  end

  # `flatten` on a deeply nested value recurses. The parser's depth guard
  # bounds what a *literal* can nest to, but a value built at runtime is
  # not parsed — so flatten's own recursion is the only thing standing
  # between a script and a stack overflow.
  def test_flatten_on_runtime_built_deep_value_is_bounded
    assert_diagnosable(
      run_source(
        '(use collections)
         (count (flatten (reduce (fn [acc x] [acc]) [1] (range 100000))))',
        limits: { max_steps: 10_000_000 }
      ),
      "flatten over a 100k-deep runtime value must not overflow the stack"
    )
  end

  # `frequencies` and `group-by` build a map keyed by element. A map with
  # n distinct keys is n allocations the budget must account for.
  def test_frequencies_output_is_charged
    assert_source_limited(
      '(use collections)
       (count (keys (frequencies (range 200000))))',
      /alloc/i,
      limits: { max_alloc: 100_000 },
      msg: "a 200k-key map must be charged against the allocation budget"
    )
  end

  # --- Serialization ------------------------------------------------

  # Serializing a value walks it. A value that is cheap to build but
  # expensive to render is the gap worth checking.
  def test_serialization_of_large_values_is_bounded
    %w[to-json to-yaml to-trongo].each do |fn|
      assert_diagnosable(
        run_source(
          "(use serde collections)\n(str-len (#{fn} (range 2000000)))",
          limits: { max_steps: 10_000_000 }
        ),
        "(#{fn}) over a 2M-element list must terminate diagnosably"
      )
    end
  end

  # Parsers are the untrusted-input entry point: the argument is a string,
  # so the interpreter's own nesting guards never see its structure.
  def test_parsers_reject_deeply_nested_input
    {
      "parse-json" => ['(repeat "[" 100000)', '(repeat "]" 100000)'],
      "parse-yaml" => ['(repeat "[" 100000)', '(repeat "]" 100000)']
    }.each do |fn, (open, close)|
      assert_diagnosable(
        run_source(
          "(use serde strings)
           (count (#{fn} (str #{open} #{close})))",
          limits: { max_steps: 1_000_000 }
        ),
        "(#{fn}) over 100k nesting levels must not overflow the stack"
      )
    end
  end

  # A round trip is the composition of both directions; if either side is
  # unbounded, feeding output back in finds it.
  def test_serialization_round_trip_does_not_amplify
    assert_diagnosable(
      run_source(
        '(use serde collections)
         (str-len (reduce (fn [acc x] (to-json [acc acc])) "seed" (range 30)))',
        limits: { max_steps: 1_000_000 }
      ),
      "30 rounds of wrapping a value in itself is exponential"
    )
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
