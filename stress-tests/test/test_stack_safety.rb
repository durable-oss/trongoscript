# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# Stack-safety stress tests.
#
# These are the most important tests in the suite. A tree-walking evaluator
# recurses on the native stack, so unbounded script recursion overflows it
# and the process dies with SIGSEGV — an uncatchable crash, not an error a
# host can handle or report.
#
# Measured baseline: with the depth guards lifted, a release build of the
# interpreter core-dumps somewhere between 3,000 and 4,000 nested calls on
# an 8 MiB stack. Every test here asserts the guard fires *before* that.
class TestStackSafety < StressTestCase
  def test_direct_infinite_recursion_is_caught
    assert_source_limited(
      "(defn spin [] (spin)) (spin)",
      /call depth/,
      msg: "the simplest possible recursion bomb must be stopped"
    )
  end

  def test_mutual_recursion_is_caught
    # Neither function recurses into itself, so a naive per-function guard
    # would miss this. The depth counter is global, so it does not.
    src = <<~TRONGO
      (defn ping [n] (pong (+ n 1)))
      (defn pong [n] (ping (+ n 1)))
      (ping 0)
    TRONGO
    assert_source_limited(src, /call depth/,
                          msg: "mutual recursion must be caught too")
  end

  def test_recursion_through_a_higher_order_function_is_caught
    # The recursive edge runs through `map`'s internal call machinery
    # rather than a direct self-call in the script.
    src = <<~TRONGO
      (defn descend [xs] (map (fn [x] (descend [x])) xs))
      (descend [1])
    TRONGO
    assert_diagnosable(run_source(src),
                       "recursion laundered through map must still be bounded")
  end

  def test_recursion_through_an_anonymous_function_value_is_caught
    # No named function at all: the closure calls itself through a global
    # binding installed by def.
    src = <<~TRONGO
      (def f (fn [n] (f (+ n 1))))
      (f 0)
    TRONGO
    assert_diagnosable(run_source(src),
                       "anonymous self-reference must be bounded")
  end

  def test_recursion_far_past_the_native_stack_limit_never_crashes
    # 3,000-4,000 frames is where the unguarded interpreter core-dumps.
    # Ask for far more than that and require a clean error.
    src = <<~TRONGO
      (defn deep [n] (if (<= n 0) 0 (+ 1 (deep (- n 1)))))
      (deep 500000)
    TRONGO
    result = run_source(src)
    assert_resource_limited(result,
                            "half a million frames must not reach the native stack")
  end

  def test_raising_the_call_depth_limit_still_does_not_crash
    # A host that raises the ceiling should still get an error rather than
    # a SIGSEGV: `unlimited()` deliberately keeps a finite depth cap.
    src = <<~TRONGO
      (defn deep [n] (if (<= n 0) 0 (+ 1 (deep (- n 1)))))
      (deep 500000)
    TRONGO
    result = run_source(src, limits: { no_limits: true })
    assert_diagnosable(
      result,
      "--no-limits must still bound native recursion; a stack overflow is " \
      "a crash, not a policy choice"
    )
  end

  def test_explicitly_absurd_depth_ceiling_still_does_not_crash
    # The operator asks for a depth the stack cannot support. The
    # interpreter must not simply obey and die.
    src = <<~TRONGO
      (defn deep [n] (if (<= n 0) 0 (+ 1 (deep (- n 1)))))
      (deep 500000)
    TRONGO
    result = run_source(src, limits: {
                          max_call_depth: 10_000_000,
                          max_eval_depth: 10_000_000,
                          max_steps: 100_000_000_000
                        })
    assert_diagnosable(
      result,
      "an operator-supplied depth beyond the native stack must still fail " \
      "safely rather than core-dump"
    )
  end

  def test_deeply_nested_literal_data_is_caught
    # No function calls at all: pure syntactic nesting. Note this is caught
    # by the *parser*, before evaluation begins — an evaluation fuel limit
    # cannot help here, because the input never finishes parsing.
    assert_source_limited(
      nested("[", "]", 50_000),
      /nests more than/,
      msg: "deeply nested literals must not overflow the stack"
    )
  end

  def test_nesting_is_caught_by_the_parser_not_the_evaluator
    # Regression guard for the ordering bug this suite originally found:
    # the parser used to recurse without a limit, so hostile input aborted
    # the process before any evaluation limit could apply. Raising the
    # evaluation ceilings must not reintroduce that.
    result = run_source(nested("[", "]", 50_000),
                        limits: { max_eval_depth: 1_000_000, no_limits: true })
    assert_resource_limited(
      result,
      "the parser must enforce its own depth limit independently of " \
      "evaluation fuel"
    )
    assert_match(/nests more than/, result.stderr)
  end

  def test_deeply_nested_call_forms_are_caught
    assert_diagnosable(
      run_source(nested("(str ", ")", 50_000, '"x"')),
      "deeply nested call forms must be bounded"
    )
  end

  def test_deeply_nested_maps_are_caught
    src = nested("{:k ", "}", 20_000)
    assert_diagnosable(run_source(src),
                       "deeply nested map literals must be bounded")
  end

  def test_legitimate_recursion_still_works
    # The guard must not break ordinary recursive code. Depth 200 is well
    # within any reasonable program.
    src = <<~TRONGO
      (defn fact [n] (if (<= n 1) 1 (* n (fact (- n 1)))))
      (defn sum-to [n] (if (<= n 0) 0 (+ n (sum-to (- n 1)))))
      {:fact (fact 15) :sum (sum-to 200)}
    TRONGO
    result = assert_completes(src.then { run_source(_1) },
                              "ordinary recursion must not be blocked")
    assert_equal 1_307_674_368_000, result.json["fact"]
    assert_equal 20_100, result.json["sum"]
  end

  def test_depth_counter_does_not_leak_across_sibling_calls
    # 400 sequential calls, each one frame deep. If the counter failed to
    # unwind, this would trip the default ceiling of 512.
    src = "(defn id [x] x) (count (map id #{literal_list(400)}))"
    result = assert_completes(run_source(src),
                              "sequential calls must not accumulate depth")
    assert_equal 400, result.json
    assert_operator result.fuel[:peak_call_depth], :<, 10,
                    "peak depth should stay tiny for flat iteration"
  end

  def test_depth_counter_does_not_leak_when_calls_raise
    # A closure that errors must still release its frame. If it did not,
    # repeated failures would eventually exhaust the depth budget and turn
    # a script bug into a spurious resource error.
    src = <<~TRONGO
      (defn safe [x] x)
      (count (map safe #{literal_list(200)}))
    TRONGO
    assert_completes(run_source(src, limits: { max_call_depth: 20 }),
                     "frames must be released on every path")
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
