# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# Configuration and accounting tests.
#
# The guards in the other files are only as good as the knobs that control
# them. These check that limits mean what they say, that presets are
# actually safe, that the accounting is honest, and — most importantly —
# that a caller cannot configure their way into a crash.
class TestLimitConfiguration < StressTestCase
  # -------------------------------------------------------------------------
  # Limits mean what they say
  # -------------------------------------------------------------------------

  def test_step_limit_is_honored_exactly_as_given
    result = run_source("(reduce (fn [a x] (+ a x)) 0 (range 1000000))",
                        limits: { max_steps: 7_777 })
    assert_limit_message(result, /\(7777 steps\)/,
                         "the configured number should appear in the error")
  end

  def test_a_generous_limit_permits_what_a_tight_one_refuses
    src = "(reduce (fn [a x] (+ a x)) 0 (range 5000))"
    assert_resource_limited(run_source(src, limits: { max_steps: 1_000 }),
                            "1k steps should not be enough")
    assert_completes(run_source(src, limits: { max_steps: 10_000_000 }),
                     "10M steps should be plenty")
  end

  def test_value_length_limit_is_honored
    assert_completes(run_source("(count (range 500))",
                                limits: { max_value_len: 1_000 }))
    assert_resource_limited(run_source("(count (range 5000))",
                                       limits: { max_value_len: 1_000 }))
  end

  def test_call_depth_limit_is_honored
    src = "(defn d [n] (if (<= n 0) 0 (+ 1 (d (- n 1))))) (d 40)"
    assert_completes(run_source(src, limits: { max_call_depth: 100 }),
                     "40 frames should fit in a 100-frame budget")
    assert_resource_limited(run_source(src, limits: { max_call_depth: 20 }),
                            "40 frames should not fit in a 20-frame budget")
  end

  # -------------------------------------------------------------------------
  # Presets
  # -------------------------------------------------------------------------

  def test_untrusted_preset_stops_every_attack_shape
    {
      "infinite recursion" => "(defn s [] (s)) (s)",
      "infinite iteration" => "(reduce (fn [a x] (+ a x)) 0 (range 100000000))",
      "huge allocation" => "(count (range 100000000))",
      "nesting bomb" => "(reduce (fn [acc x] [acc]) [] (range 100000))"
    }.each do |name, src|
      assert_resource_limited(
        run_source(src, limits: { untrusted: true }, memory_mb: 512),
        "--untrusted must stop: #{name}"
      )
    end
  end

  def test_untrusted_preset_still_runs_ordinary_scripts
    src = <<~TRONGO
      (defn fact [n] (if (<= n 1) 1 (* n (fact (- n 1)))))
      {:fact (fact 10)
       :sum (reduce (fn [a x] (+ a x)) 0 (range 100))
       :mapped (count (map (fn [x] (* x 2)) (range 100)))}
    TRONGO
    result = assert_completes(run_source(src, limits: { untrusted: true }),
                              "the strict preset must not break normal work")
    assert_equal 3_628_800, result.json["fact"]
    assert_equal 4_950, result.json["sum"]
  end

  def test_default_limits_permit_a_realistic_workload
    # Nothing here is unusual. If the defaults trip on this, they are too
    # tight to ship.
    src = <<~TRONGO
      (defn fib [n] (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
      (let [rows (map (fn [i] {:id i :sq (* i i)}) (range 500))]
        {:count (count rows)
         :fib (fib 18)
         :total (reduce (fn [a x] (+ a x)) 0 (range 5000))})
    TRONGO
    result = assert_completes(run_source(src),
                              "default limits must not obstruct real work")
    assert_equal 500, result.json["count"]
    assert_equal 2584, result.json["fib"]
  end

  # -------------------------------------------------------------------------
  # Configuration cannot produce a crash
  # -------------------------------------------------------------------------

  def test_absurd_depth_requests_are_clamped_not_obeyed
    # Honoring this literally would mean a stack overflow, which no exit
    # code can report. The request must be capped instead.
    src = "(defn d [n] (+ 1 (d (+ n 1)))) (d 0)"
    result = run_source(src, limits: {
                          max_call_depth: 100_000_000,
                          max_eval_depth: 100_000_000,
                          max_steps: 10_000_000_000
                        })
    assert_resource_limited(result,
                            "an unsatisfiable depth request must be clamped")
  end

  def test_zero_limits_are_not_a_crash
    # Degenerate but legal input. Should error cleanly, not panic.
    [{ max_steps: 0 }, { max_call_depth: 0 }, { max_value_len: 0 },
     { max_alloc: 0 }, { max_eval_depth: 0 }].each do |limits|
      assert_diagnosable(run_source("(+ 1 2)", limits: limits),
                         "zero limit #{limits.keys.first} must not crash")
    end
  end

  def test_no_limits_flag_does_not_disable_stack_safety
    # "No limits" means unlimited work, not unlimited stack: exceeding the
    # native stack is a crash rather than an error, so it is not something
    # a flag can switch off.
    src = "(defn d [n] (+ 1 (d (+ n 1)))) (d 0)"
    assert_resource_limited(
      run_source(src, limits: { no_limits: true }),
      "--no-limits must still bound recursion depth"
    )
  end

  def test_conflicting_presets_are_rejected_by_the_cli
    result = run_cli(["eval", "-i", "-", "--untrusted", "--no-limits"])
    assert_diagnosable(result, "conflicting flags must be rejected cleanly")
    refute result.ok?, "conflicting flags should not silently succeed"
  end

  # -------------------------------------------------------------------------
  # Accounting
  # -------------------------------------------------------------------------

  def test_reported_usage_grows_with_actual_work
    small = assert_completes(run_source("(reduce (fn [a x] (+ a x)) 0 (range 100))"))
    large = assert_completes(run_source("(reduce (fn [a x] (+ a x)) 0 (range 10000))"))
    assert_operator large.fuel[:steps], :>, small.fuel[:steps] * 10,
                    "100x the work should cost substantially more steps"
  end

  def test_fuel_report_goes_to_stderr_not_stdout
    # Diagnostics must never contaminate the evaluated output, or piping
    # `trongo eval` into a JSON consumer breaks.
    result = assert_completes(run_source("(+ 1 2)"))
    assert_equal 3, result.json, "stdout must be parseable JSON only"
    assert_match(/fuel:/, result.stderr, "the report belongs on stderr")
  end

  def test_usage_is_not_reported_unless_requested
    result = run_source("(+ 1 2)", limits: { report_fuel: false })
    assert_completes(result)
    refute_match(/fuel:/, result.stderr,
                 "usage should be silent without --report-fuel")
  end

  # -------------------------------------------------------------------------
  # Limits apply to every entry point, not just `eval`
  # -------------------------------------------------------------------------

  def test_template_rendering_is_metered
    src = "before <%= (reduce (fn [a x] (+ a x)) 0 (range 10000000)) %> after"
    assert_resource_limited(
      run_source(src, limits: { max_steps: 50_000 }, subcommand: :render),
      "template rendering must be metered too"
    )
  end

  def test_embedded_documents_are_metered
    doc = JSON.generate(
      { "value" => "!trongo (reduce (fn [a x] (+ a x)) 0 (range 10000000))" }
    )
    result = run_source(doc, limits: { max_steps: 50_000 }, subcommand: :embed,
                             args: ["--format", "json"])
    assert_diagnosable(result, "embedded evaluation must be metered too")
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
