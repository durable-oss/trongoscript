# frozen_string_literal: true

require "test_helper"

# Threads, the GVL, interrupts, and stack sizes.
class TestConcurrency < Minitest::Test
  # Enough steps to keep the evaluator busy for a noticeable time, under a
  # step budget that still ends it.
  BUSY = "(reduce (fn [a x] (+ a x)) 0 (range 300000))"

  def now = Process.clock_gettime(Process::CLOCK_MONOTONIC)

  def test_separate_contexts_in_parallel_threads
    threads = Array.new(8) do |i|
      Thread.new do
        ctx = Trongoscript::Context.new
        ctx.define("id") { |x| x }
        Array.new(25) { |j| ctx.eval("(+ (id #{i}) #{j})") }
      end
    end
    threads.each_with_index do |t, i|
      assert_equal Array.new(25) { |j| i + j }, t.value
    end
  end

  def test_the_gvl_is_released_while_evaluating
    ctx = Trongoscript::Context.new
    ctx.eval(BUSY)
    single = ctx.usage[:steps]
    assert_operator single, :>, 100_000

    ticks = 0
    worker = Thread.new { ctx.eval(BUSY) }
    ticks += 1 while worker.alive?
    worker.join
    # With the GVL held for the whole evaluation this loop would get no
    # timeslices until it finished.
    assert_operator ticks, :>, 1000
  end

  def test_callbacks_keep_running_while_other_threads_compete_for_the_gvl
    # Ruby's timer asks a running thread to yield when others are waiting,
    # which also makes a no-GVL wait decline to block. That must read as a
    # thread switch, not an interrupt.
    stop = false
    spinners = Array.new(3) { Thread.new { Integer.sqrt(12_345) until stop } }
    ctx = Trongoscript::Context.new
    calls = 0
    ctx.define("tick") do |x|
      calls += 1
      x + 1
    end
    started = now
    assert_equal 3000, ctx.eval("(reduce (fn [a x] (tick a)) 0 (range 3000))")
    assert_equal 3000, calls
    # Paying a GVL handoff per callback would take minutes here.
    assert_operator now - started, :<, 20
  ensure
    stop = true
    spinners&.each(&:join)
  end

  def test_thread_wakeup_does_not_interrupt
    ctx = Trongoscript::Context.new
    ctx.define("tick") { |x| x + 1 }
    worker = Thread.new { ctx.eval("(reduce (fn [a x] (tick a)) 0 (range 20000))") }
    # Waking as often as possible lands some wakeups just after a wait has
    # already received a callback, which used to leave a stale wake behind
    # that a later wait took for an interrupt.
    while worker.alive?
      begin
        worker.wakeup
      rescue ThreadError
        break # the worker finished between the check and the wakeup
      end
      Thread.pass
    end
    assert_equal 20_000, worker.value
  end

  def test_sharing_a_context_between_threads_is_refused
    ctx = Trongoscript::Context.new
    started = Queue.new
    release = Queue.new
    ctx.define("wait") do
      started << true
      release.pop
    end
    worker = Thread.new { ctx.eval("(wait)") }
    started.pop
    e = assert_raises(Trongoscript::ConcurrencyError) { ctx.eval("1") }
    assert_includes e.message, "two threads"
    release << 5
    assert_equal 5, worker.value
    assert_equal 1, ctx.eval("1"), "the context recovers"
  end

  def test_timeout_interrupts_a_script_that_calls_back
    ctx = Trongoscript::Context.new(limits: { max_steps: 100_000_000 })
    ctx.define("tick") { |x| x }
    started = now
    assert_raises(Timeout::Error) do
      Timeout.timeout(0.2) { ctx.eval("(reduce (fn [a x] (tick a)) 0 (range 10000000))") }
    end
    assert_operator now - started, :<, 5
    ctx.undefine("tick")
    assert_equal 3, ctx.eval("(+ 1 2)"), "the context recovers"
  end

  def test_timeout_is_delivered_after_a_script_that_never_calls_back
    ctx = Trongoscript::Context.new
    assert_raises(Timeout::Error) do
      Timeout.timeout(0.01) { ctx.eval(BUSY) }
    end
    assert_equal 3, ctx.eval("(+ 1 2)")
  end

  def test_thread_raise
    ctx = Trongoscript::Context.new
    ctx.define("tick") { |x| x }
    worker = Thread.new do
      ctx.eval("(reduce (fn [a x] (tick a)) 0 (range 10000000))")
    rescue RuntimeError => e
      e
    end
    sleep 0.1
    worker.raise(RuntimeError, "stop now")
    result = worker.value
    assert_kind_of RuntimeError, result
    assert_equal "stop now", result.message
  end

  def test_thread_kill
    ctx = Trongoscript::Context.new
    ctx.define("tick") { |x| x }
    worker = Thread.new { ctx.eval("(reduce (fn [a x] (tick a)) 0 (range 10000000))") }
    sleep 0.1
    worker.kill
    assert_nil worker.join(10)&.value
    refute_predicate worker, :alive?
    assert_equal 2, ctx.eval("(+ 1 1)"), "the context recovers"
  end

  def test_deep_recursion_on_a_small_ruby_thread_stack
    # Ruby's non-main threads have about 1 MiB of machine stack; the
    # evaluator would overflow that within a few hundred calls.
    src = "(defn down [n] (if (= n 0) 0 (+ 1 (down (- n 1))))) (down 2000)"
    assert_equal 2000, Thread.new { Trongoscript.eval(src) }.value
  end

  def test_deep_values_on_a_small_ruby_thread_stack
    value = nested(1024)
    result = Thread.new do
      ctx = Trongoscript::Context.new
      ctx["v"] = value
      [ctx.eval("v"), ctx.eval(deep_list_source(1024))]
    end.value
    assert_equal value, result[0]
    assert_equal nested(1023, []), result[1]
  end

  def test_contexts_are_garbage_collected
    ids = Array.new(50) do
      ctx = Trongoscript::Context.new
      ctx.eval("(def big (range 10000)) (def deep #{deep_list_source(1500)}) nil")
      ctx.object_id
    end
    GC.start
    assert_equal 50, ids.uniq.size
  end
end
