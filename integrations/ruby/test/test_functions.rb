# frozen_string_literal: true

require "test_helper"

# Ruby callables exposed to scripts.
class TestFunctions < Minitest::Test
  def setup
    @ctx = Trongoscript::Context.new
  end

  class Doubler
    def call(x) = x * 2
  end

  def triple(x) = x * 3

  def test_every_kind_of_callable
    @ctx.define("blk") { |x| x + 1 }
    @ctx.define("lam", ->(x) { x + 2 })
    @ctx.define("prc", proc { |x| x + 3 })
    @ctx.define("obj", Doubler.new)
    @ctx.define(:meth, method(:triple))
    assert_equal [11, 12, 13, 20, 30], @ctx.eval("[(blk 10) (lam 10) (prc 10) (obj 10) (meth 10)]")
  end

  def test_define_returns_the_context
    assert_same @ctx, @ctx.define("a") { 1 }.define("b") { 2 }
    assert_equal %w[a b], @ctx.functions
  end

  def test_functions_option
    ctx = Trongoscript::Context.new(functions: { "inc" => ->(x) { x + 1 }, dec: ->(x) { x - 1 } })
    assert_equal [2, 0], ctx.eval("[(inc 1) (dec 1)]")
    assert_equal({ "a" => 2 }, Trongoscript.eval("{:a (inc 1)}", functions: { inc: ->(x) { x + 1 } }))
  end

  def test_arguments_are_converted
    seen = nil
    @ctx.define("spy") { |*args| seen = args }
    @ctx.eval('(spy 1 2.5 "s" :kw nil true [1 [2]] {:a {:b 1}})')
    assert_equal [1, 2.5, "s", "kw", nil, true, [1, [2]], { "a" => { "b" => 1 } }], seen
  end

  def test_arguments_with_symbolize_names
    seen = nil
    ctx = Trongoscript::Context.new(symbolize_names: true)
    ctx.define("spy") { |m| seen = m }
    ctx.eval("(spy {:a {:b [{:c 1}]}})")
    assert_equal({ a: { b: [{ c: 1 }] } }, seen)
  end

  def test_zero_and_many_arguments
    @ctx.define("count-args") { |*a| a.size }
    assert_equal 0, @ctx.eval("(count-args)")
    assert_equal 50, @ctx.eval("(count-args #{(1..50).to_a.join(" ")})")
  end

  def test_arity_follows_the_callable
    @ctx.define("strict", ->(a, b) { a + b })
    assert_equal 3, @ctx.eval("(strict 1 2)")
    assert_raises(ArgumentError) { @ctx.eval("(strict 1)") }
  end

  def test_return_values_are_converted
    @ctx.define("data") { { a: [1, :sym, nil, { "x" => 1.5 }] } }
    assert_equal({ "a" => [1, "sym", nil, { "x" => 1.5 }] }, @ctx.eval("(data)"))
    @ctx.define("nothing") {}
    assert_nil @ctx.eval("(nothing)")
  end

  def test_unconvertible_return_values_raise_type_error
    @ctx.define("bad") { Object.new }
    assert_raises(TypeError) { @ctx.eval("(bad)") }
    @ctx.define("deep") { nested(2000) }
    assert_raises(ArgumentError) { @ctx.eval("(deep)") }
  end

  def test_results_flow_back_into_the_script
    @ctx.define("lookup") { |k| { "port" => 8080 }.fetch(k) }
    assert_equal({ "url" => "http://localhost:8080" }, @ctx.eval('{:url (str "http://localhost:" (lookup "port"))}'))
  end

  def test_calls_happen_in_order_exactly_once
    log = []
    @ctx.define("log") do |x|
      log << x
      x
    end
    @ctx.eval("(log 1) (log 2) [(log 3) (log 4)]")
    assert_equal [1, 2, 3, 4], log
  end

  def test_inside_higher_order_builtins_and_closures
    @ctx.define("sq") { |x| x * x }
    assert_equal [1, 4, 9], @ctx.eval("(map (fn [x] (sq x)) [1 2 3])")
    assert_equal 14, @ctx.eval("(reduce (fn [a x] (+ a (sq x))) 0 [1 2 3])")
    @ctx.eval("(defn sq-plus [x] (+ 1 (sq x)))")
    assert_equal 26, @ctx.call("sq-plus", 5)
  end

  def test_host_functions_shadow_builtins
    @ctx.define("upcase") { |_| "mine" }
    assert_equal "mine", @ctx.eval('(upcase "a")')
    assert @ctx.undefine("upcase")
    refute @ctx.undefine("upcase")
    assert_equal "A", @ctx.eval('(upcase "a")')
  end

  def test_undefined_functions_are_gone
    @ctx.define("f") { 1 }
    @ctx.undefine(:f)
    assert_raises(Trongoscript::EvalError) { @ctx.eval("(f)") }
    assert_empty @ctx.functions
  end

  def test_redefining_replaces
    @ctx.define("f") { 1 }
    @ctx.define("f") { 2 }
    assert_equal 2, @ctx.eval("(f)")
    assert_equal ["f"], @ctx.functions
  end

  def test_call_reaches_host_functions
    @ctx.define("add") { |a, b| a + b }
    assert_equal 5, @ctx.call("add", 2, 3)
  end

  def test_define_validation
    assert_raises(ArgumentError) { @ctx.define("f") }
    assert_raises(ArgumentError) { @ctx.define("f", -> {}) { 1 } }
    assert_raises(TypeError) { @ctx.define("f", 42) }
    assert_raises(ArgumentError) { @ctx.define("bad name") { 1 } }
    assert_empty @ctx.functions
  end

  # -- exceptions -----------------------------------------------------------

  class CustomError < StandardError; end

  def test_the_original_exception_is_reraised
    error = CustomError.new("custom failure")
    @ctx.define("boom") { raise error }
    raised = assert_raises(CustomError) { @ctx.eval("(+ 1 (boom))") }
    assert_same error, raised
    assert(raised.backtrace.any? { |l| l.include?(__FILE__) })
  end

  def test_exceptions_from_deep_inside_a_script
    @ctx.define("check") { |x| x < 3 ? x : raise(ArgumentError, "too big: #{x}") }
    e = assert_raises(ArgumentError) { @ctx.eval("(map (fn [x] (check x)) [1 2 3 4])") }
    assert_equal "too big: 3", e.message
  end

  def test_exceptions_outside_standard_error
    fatal = Class.new(Exception) # rubocop:disable Lint/InheritException
    @ctx.define("die") { raise fatal, "not a StandardError" }
    assert_raises(fatal) { @ctx.eval("(die)") }
  end

  def test_type_errors_raised_by_ruby_code
    @ctx.define("add") { |a, b| a + b }
    assert_raises(TypeError) { @ctx.eval('(add 1 "x")') }
  end

  def test_throw_unwinds_through_an_evaluation
    @ctx.define("escape") { |v| throw :done, v }
    assert_equal 42, catch(:done) { @ctx.eval("(+ 1 (escape 42))") }
    assert_equal 2, @ctx.eval("(+ 1 1)"), "context is usable after the throw"
  end

  def test_break_from_a_block_after_define_returned
    @ctx.define("brk") { break 1 }
    assert_raises(LocalJumpError) { @ctx.eval("(brk)") }
  end

  def test_the_context_is_usable_after_a_callback_failure
    @ctx.define("boom") { raise "boom" }
    @ctx.eval("(def kept 7)")
    assert_raises(RuntimeError) { @ctx.eval("(boom)") }
    assert_equal 7, @ctx.eval("kept")
  end

  def test_only_the_failing_call_aborts
    calls = 0
    @ctx.define("tick") do
      calls += 1
      raise "stop" if calls == 3

      calls
    end
    assert_raises(RuntimeError) { @ctx.eval("(tick) (tick) (tick) (tick) (tick)") }
    assert_equal 3, calls
  end

  # -- nesting and re-entrancy ---------------------------------------------

  def test_a_callback_can_evaluate_in_another_context
    other = Trongoscript::Context.new
    @ctx.define("inner") { |src| other.eval(src) }
    assert_equal 43, @ctx.eval('(+ 1 (inner "(* 6 7)"))')
    @ctx.define("oneshot") { |src| Trongoscript.eval(src) }
    assert_equal 10, @ctx.eval('(oneshot "(+ 4 6)")')
  end

  def test_nested_callbacks_several_levels_deep
    contexts = Array.new(5) { Trongoscript::Context.new }
    contexts.each_cons(2) do |outer, inner|
      outer.define("down") { |n| inner.eval("(down #{n + 1})") }
    end
    contexts.last.define("down") { |n| n }
    assert_equal 1, contexts.first.eval("(down 0)") + 0 - 3
  end

  def test_a_callback_cannot_reenter_its_own_context
    @ctx.define("again") { @ctx.eval("1") }
    e = assert_raises(Trongoscript::ConcurrencyError) { @ctx.eval("(again)") }
    assert_includes e.message, "already evaluating"
    assert_equal 1, @ctx.eval("1"), "the context recovers"
  end

  def test_a_callback_cannot_touch_its_own_globals
    @ctx.define("peek") { @ctx["x"] }
    assert_raises(Trongoscript::ConcurrencyError) { @ctx.eval("(peek)") }
    @ctx.define("redefine") { @ctx.define("x") { 1 } }
    assert_raises(Trongoscript::ConcurrencyError) { @ctx.eval("(redefine)") }
  end

  def test_many_calls
    @ctx.define("inc") { |x| x + 1 }
    started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    assert_equal 5000, @ctx.eval("(reduce (fn [a x] (inc a)) 0 (range 5000))")
    elapsed = Process.clock_gettime(Process::CLOCK_MONOTONIC) - started
    assert_operator elapsed, :<, 10, "5000 callbacks took #{elapsed}s"
  end

  def test_callbacks_see_fresh_copies
    @ctx.define("mutate") do |list|
      list << 99
      nil
    end
    assert_equal [1, 2], @ctx.eval("(def xs [1 2]) (mutate xs) xs")
  end

  def test_callbacks_under_gc_stress
    @ctx.define("make") { |n| { "items" => Array.new(n) { |i| "s#{i}" } } }
    GC.stress = true
    begin
      result = @ctx.eval("(make 5)")
    ensure
      GC.stress = false
    end
    assert_equal({ "items" => %w[s0 s1 s2 s3 s4] }, result)
  end

  def test_the_stashed_exception_survives_gc
    @ctx.define("boom") { raise CustomError, "kept alive" }
    @ctx.define("slow") { 1 }
    10.times do
      gc_thread = Thread.new { 20.times { GC.start } }
      e = assert_raises(CustomError) { @ctx.eval("(boom)") }
      gc_thread.join
      assert_equal "kept alive", e.message
    end
  end
end
