# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# Memory-exhaustion stress tests.
#
# A step counter cannot see these: `(range 100000000)` is one step and
# gigabytes of RSS. Every case here runs under a hard address-space rlimit,
# so a missing allocation guard shows up as the child being killed rather
# than as the developer's machine swapping.
#
# The standard these assert: the interpreter must *refuse* an oversized
# allocation, not attempt it and die. Those look the same from a distance
# and are very different in production — one returns an error to the
# caller, the other takes down whatever process is hosting the interpreter.
class TestMemoryExhaustion < StressTestCase
  # Deliberately tight: enough for the interpreter, not enough to hide a
  # runaway allocation.
  TIGHT_MEMORY_MB = 512

  def test_a_single_enormous_collection_is_refused
    assert_source_limited(
      "(count (range 500000000))",
      /exceeds the maximum value size/,
      memory_mb: TIGHT_MEMORY_MB,
      msg: "a half-billion-element list must be refused, not attempted"
    )
  end

  def test_the_refusal_costs_almost_no_memory
    # The point of a pre-flight check is that the allocation never happens.
    # Before the check existed, this input peaked over 3 GB of RSS; now it
    # should be refused within an ordinary process footprint. If this test
    # starts failing on memory, the guard has regressed to checking after
    # the fact.
    result = run_source("(count (range 200000000))", memory_mb: 384)
    assert_resource_limited(
      result,
      "refusing an oversized allocation must not itself require the memory"
    )
  end

  def test_runtime_nesting_bomb_is_charged_for_its_copy_cost
    # `[acc]` wraps the accumulator each round, so copying it into the next
    # call is O(depth) while the step counter sees only one call. Before
    # this was charged, 8,000 rounds cost 3.4s of real work but reported
    # 16,005 steps — a script the default budget would never stop.
    result = run_source("(reduce (fn [acc x] [acc]) [] (range 50000))")
    assert_resource_limited(
      result,
      "a runtime nesting bomb must be charged for the copying it causes"
    )
    assert_operator result.duration, :<, 20,
                    "the guard should fire promptly, not after a long stall"
  end

  def test_repeated_moderate_allocations_are_bounded_cumulatively
    # Each list is far below max_value_len; only the cumulative budget can
    # see that together they are far too much.
    src = "(count (map (fn [x] (count (range 10000))) (range 100000)))"
    assert_source_limited(
      src,
      /allocation limit/,
      limits: { max_alloc: 5_000_000 },
      memory_mb: TIGHT_MEMORY_MB,
      msg: "many moderate allocations must be caught by the total budget"
    )
  end

  def test_growing_a_string_is_bounded
    # String concatenation in a fold: each step is small, the result is not.
    src = <<~TRONGO
      (reduce (fn [acc x] (str acc "padding-padding-padding-padding"))
              ""
              (range 200000))
    TRONGO
    assert_diagnosable(
      run_source(src, limits: { max_alloc: 10_000_000 }, memory_mb: TIGHT_MEMORY_MB),
      "unbounded string growth must be caught"
    )
  end

  def test_nested_collection_growth_is_bounded
    # Each round wraps the previous result, so size grows geometrically
    # while the step count grows linearly.
    src = <<~TRONGO
      (reduce (fn [acc x] (concat acc acc)) (range 100) (range 100))
    TRONGO
    assert_diagnosable(
      run_source(src, limits: { max_alloc: 20_000_000 }, memory_mb: TIGHT_MEMORY_MB),
      "geometric growth must be caught by the allocation budget"
    )
  end

  def test_deeply_nested_output_value_is_bounded
    # Build nesting at runtime rather than in the source, so the parser's
    # depth limit does not catch it first.
    src = "(reduce (fn [acc x] [acc]) [] (range 100000))"
    assert_diagnosable(
      run_source(src, memory_mb: TIGHT_MEMORY_MB, timeout: 40),
      "runtime-constructed nesting must be bounded"
    )
  end

  def test_value_size_limit_is_reported_with_the_offending_size
    result = run_source("(count (range 50000))",
                        limits: { max_value_len: 100 })
    assert_limit_message(
      result, /list of length 50000 exceeds the maximum value size \(100\)/,
      "the error should name both the actual and the permitted size"
    )
  end

  def test_legitimate_allocation_still_works
    result = assert_completes(
      run_source("(count (range 100000))"),
      "an ordinary 100k list must be allowed"
    )
    assert_equal 100_000, result.json
  end

  def test_allocation_accounting_is_reported
    result = assert_completes(run_source("(count (range 10000))"))
    fuel = result.fuel
    refute_nil fuel
    assert_operator fuel[:allocated], :>, 10_000,
                    "a 10k list should register meaningfully: #{fuel.inspect}"
  end

  def test_untrusted_preset_bounds_memory_aggressively
    # The preset exists so a host does not have to tune five numbers to be
    # safe. Verify it actually is.
    assert_resource_limited(
      run_source("(count (range 10000000))",
                 limits: { untrusted: true }, memory_mb: TIGHT_MEMORY_MB),
      "--untrusted must refuse a 10M-element allocation"
    )
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
