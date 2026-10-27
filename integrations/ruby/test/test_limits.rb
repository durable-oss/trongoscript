# frozen_string_literal: true

require "test_helper"

class TestLimits < Minitest::Test
  RECURSE = "(defn down [n] (if (= n 0) 0 (+ 1 (down (- n 1)))))"

  def test_step_budget
    ctx = Trongoscript::Context.new(limits: { max_steps: 5_000 })
    assert_kind_of Integer, ctx.eval("(reduce (fn [a x] (+ a x)) 0 (range 100))")
    e = assert_raises(Trongoscript::ResourceLimitError) { ctx.eval("(reduce (fn [a x] (+ a x)) 0 (range 10000))") }
    assert_includes e.message, "5000"
  end

  def test_each_evaluation_gets_a_fresh_budget
    ctx = Trongoscript::Context.new(limits: { max_steps: 5_000 })
    20.times { ctx.eval("(reduce (fn [a x] (+ a x)) 0 (range 300))") }
  end

  def test_strict_preset
    assert_equal 50, Trongoscript.eval("#{RECURSE} (down 50)", limits: :strict)
    assert_raises(Trongoscript::ResourceLimitError) { Trongoscript.eval("#{RECURSE} (down 200)", limits: :strict) }
    assert_raises(Trongoscript::ResourceLimitError) { Trongoscript.eval("(range 100000)", limits: :strict) }
  end

  def test_preset_with_overrides
    assert_equal 200, Trongoscript.eval("#{RECURSE} (down 200)", limits: { preset: :strict, max_call_depth: 500, max_eval_depth: 2000 })
  end

  def test_call_depth
    assert_equal 1500, Trongoscript.eval("#{RECURSE} (down 1500)")
    assert_raises(Trongoscript::ResourceLimitError) { Trongoscript.eval("#{RECURSE} (down 3000)") }
  end

  def test_depth_requests_are_clamped_to_what_the_stack_survives
    # A billion frames would crash the process; the request is capped at
    # a safe depth instead, and recursion within it still works.
    limits = { max_call_depth: 1_000_000_000, max_eval_depth: 1_000_000_000 }
    assert_equal 3000, Trongoscript.eval("#{RECURSE} (down 3000)", limits: limits)
    assert_raises(Trongoscript::ResourceLimitError) { Trongoscript.eval("#{RECURSE} (down 100000)", limits: limits) }
  end

  def test_value_length
    assert_equal 10, Trongoscript.eval("(count (range 10))", limits: { max_value_len: 10 })
    assert_raises(Trongoscript::ResourceLimitError) { Trongoscript.eval("(range 11)", limits: { max_value_len: 10 }) }
  end

  def test_total_allocation
    assert_raises(Trongoscript::ResourceLimitError) do
      Trongoscript.eval("(reduce (fn [a x] (range 1000)) [] (range 1000))", limits: { max_total_alloc: 100_000 })
    end
  end

  def test_unlimited_preset_still_caps_depth
    assert_raises(Trongoscript::ResourceLimitError) { Trongoscript.eval("#{RECURSE} (down 100000)", limits: :unlimited) }
  end

  def test_usage_reflects_the_last_evaluation
    ctx = Trongoscript::Context.new
    ctx.eval("#{RECURSE} (down 100)")
    assert_operator ctx.usage[:peak_call_depth], :>=, 100
    ctx.eval("1")
    assert_operator ctx.usage[:peak_call_depth], :<, 100
  end

  def test_usage_is_recorded_for_failures_too
    ctx = Trongoscript::Context.new(limits: { max_steps: 1_000 })
    assert_raises(Trongoscript::ResourceLimitError) { ctx.eval("(defn f [] (f)) (f)") }
    assert_operator ctx.usage[:steps], :>=, 1_000
  end
end
