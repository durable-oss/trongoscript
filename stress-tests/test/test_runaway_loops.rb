# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# Non-termination stress tests.
#
# The step budget is what stands between the interpreter and a script that
# simply never finishes. These cases all run forever — or long enough to be
# indistinguishable from forever — without it.
#
# Every test here would hang the harness if the guard were removed; the
# timeout in TrongoHarness is what turns that hang into a reported failure
# rather than a wedged test run.
class TestRunawayLoops < StressTestCase
  def test_flat_iteration_over_a_large_range_is_bounded_by_steps
    # No recursion and no nesting: depth guards cannot see this. The list
    # itself is comfortably under max_value_len, so the *step* counter is
    # the only thing that can stop the fold.
    assert_source_limited(
      "(reduce (fn [acc x] (+ acc x)) 0 (range 1000000))",
      /step limit/,
      limits: { max_steps: 100_000 },
      msg: "a long flat fold must be stopped by the step budget"
    )
  end

  def test_an_absurdly_large_allocation_is_refused_before_it_is_made
    # Regression guard for a bug this suite found: the interpreter used to
    # build the whole list and only then notice it was too big, peaking at
    # over 3 GB of RSS for this input and getting OOM-killed under a memory
    # cap. The size is predictable from the argument, so it must be refused
    # up front.
    result = run_source("(count (range 100000000))",
                        limits: { max_steps: 1_000 }, memory_mb: 512)
    assert_limit_message(
      result, /exceeds the maximum value size/,
      "an oversized allocation must be refused before it is attempted"
    )
  end

  def test_repeated_mapping_over_a_large_list_is_bounded
    assert_source_limited(
      "(count (map (fn [x] (* x x)) (range 50000000)))",
      /step limit|value|allocation/,
      limits: { max_steps: 200_000 },
      msg: "mapping over a huge range must be bounded"
    )
  end

  def test_nested_higher_order_calls_are_bounded
    # n*m closure calls from a script that looks small.
    src = <<~TRONGO
      (count (map (fn [x] (count (map (fn [y] (* x y)) (range 10000))))
                  (range 10000)))
    TRONGO
    assert_diagnosable(
      run_source(src, limits: { max_steps: 500_000 }),
      "a nested map/map product must be bounded"
    )
  end

  def test_macro_expansion_bomb_is_bounded
    # Each expansion doubles the form count. Left alone this exhausts
    # memory during expansion, before evaluation proper.
    src = <<~TRONGO
      (defmacro double [x] `(do ~x ~x))
      (double (double (double (double (double (double (double (double 1))))))))
    TRONGO
    assert_diagnosable(run_source(src, limits: { max_steps: 1_000_000 }),
                       "an expansion bomb must terminate")
  end

  def test_self_expanding_macro_is_bounded
    assert_diagnosable(
      run_source("(defmacro loop-macro [] (loop-macro)) (loop-macro)"),
      "a self-expanding macro must terminate"
    )
  end

  def test_budget_is_shared_across_the_whole_document
    # Many separate cheap forms, none individually expensive. The budget is
    # per-evaluation, not per-form, so their sum is what counts.
    forms = Array.new(2_000) { |i| "(def v#{i} (+ #{i} 1))" }.join("\n")
    assert_source_limited(
      forms,
      /step limit/,
      limits: { max_steps: 2_000 },
      msg: "the budget must cover the whole document, not each form"
    )
  end

  def test_a_tight_loop_terminates_quickly_not_eventually
    # Metering is worthless if it only fires after the operator has already
    # given up. Assert the wall-clock cost of stopping, not just that it
    # stops at all.
    result = run_source("(reduce (fn [a x] (+ a x)) 0 (range 100000000))",
                        limits: { max_steps: 50_000 })
    assert_resource_limited(result)
    assert_operator result.duration, :<, 10,
                    "the step limit should fire promptly, not after a long stall"
  end

  def test_legitimate_iteration_still_completes
    # The budget must not break real work.
    src = "(reduce (fn [acc x] (+ acc x)) 0 (range 10000))"
    result = assert_completes(run_source(src),
                              "ordinary iteration must not be blocked")
    assert_equal 49_995_000, result.json
  end

  def test_step_counter_reports_plausible_usage
    result = assert_completes(run_source("(reduce (fn [a x] (+ a x)) 0 (range 1000))"))
    fuel = result.fuel
    refute_nil fuel, "expected --report-fuel output"
    # 1000 iterations cannot cost fewer than 1000 steps.
    assert_operator fuel[:steps], :>, 1_000,
                    "step count looks too low to be real: #{fuel.inspect}"
  end

  def test_exhausting_the_budget_reports_the_step_limit_specifically
    result = run_source("(reduce (fn [a x] (+ a x)) 0 (range 10000000))",
                        limits: { max_steps: 12_345 })
    assert_limit_message(result, /step limit exceeded \(12345 steps\)/,
                         "the error should name the limit that was hit")
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
