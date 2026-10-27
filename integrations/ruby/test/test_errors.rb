# frozen_string_literal: true

require "test_helper"

class TestErrors < Minitest::Test
  ERROR_CLASSES = [
    Trongoscript::ParseError, Trongoscript::EvalError, Trongoscript::SandboxError,
    Trongoscript::ResourceLimitError, Trongoscript::CallbackError, Trongoscript::ConcurrencyError
  ].freeze

  def test_hierarchy
    assert_operator Trongoscript::Error, :<, StandardError
    ERROR_CLASSES.each { |c| assert_operator c, :<, Trongoscript::Error }
  end

  def test_parse_errors
    ["(+ 1", "[1 2", "{:a}", ")", '"unterminated'].each do |src|
      e = assert_raises(Trongoscript::ParseError, src) { Trongoscript.eval(src) }
      refute_empty e.message
    end
    assert_raises(Trongoscript::ParseError) { Trongoscript.render("<%= (+ 1 %>") }
    assert_raises(Trongoscript::ParseError) { Trongoscript.embed_yaml("a: [1") }
    assert_raises(Trongoscript::ParseError) { Trongoscript.embed({ "a" => "!trongo (" }) }
  end

  def test_eval_errors
    e = assert_raises(Trongoscript::EvalError) { Trongoscript.eval("undefined-thing") }
    assert_includes e.message, "undefined-thing"
    assert_raises(Trongoscript::EvalError) { Trongoscript.eval("(no-such-function 1)") }
    assert_raises(Trongoscript::EvalError) { Trongoscript.eval('(+ 1 "a")') }
    assert_raises(Trongoscript::EvalError) { Trongoscript.eval("((fn [x] x))") }
  end

  def test_sandbox_errors_without_a_sandbox
    ['(read-file "x")', '(include "x")', '(list-files ".")'].each do |src|
      e = assert_raises(Trongoscript::SandboxError, src) { Trongoscript.eval(src) }
      assert_match(/sandbox|not configured/, e.message)
    end
  end

  def test_resource_limit_errors
    assert_raises(Trongoscript::ResourceLimitError) do
      Trongoscript.eval("(defn f [n] (f n)) (f 1)", limits: { max_steps: 10_000 })
    end
    assert_raises(Trongoscript::ResourceLimitError) do
      Trongoscript.eval("(defn f [n] (+ 1 (f n))) (f 1)", limits: :strict)
    end
    assert_raises(Trongoscript::ResourceLimitError) do
      Trongoscript.eval("(range 100)", limits: { max_value_len: 10 })
    end
    assert_raises(Trongoscript::ResourceLimitError) do
      Trongoscript.eval("#{"[" * 50_000}1#{"]" * 50_000}")
    end
  end

  def test_a_failed_evaluation_leaves_the_context_usable
    ctx = Trongoscript::Context.new(limits: { max_steps: 10_000 })
    ctx.eval("(def kept 1)")
    assert_raises(Trongoscript::ParseError) { ctx.eval("(") }
    assert_raises(Trongoscript::EvalError) { ctx.eval("(nope)") }
    assert_raises(Trongoscript::ResourceLimitError) { ctx.eval("(defn f [] (f)) (f)") }
    assert_equal 1, ctx.eval("kept")
  end

  def test_rescuing_the_base_class
    assert_raises(Trongoscript::Error) { Trongoscript.eval("(") }
    assert_raises(Trongoscript::Error) { Trongoscript.eval("x") }
  end

  def test_option_errors
    assert_raises(ArgumentError) { Trongoscript::Context.new(root: ".", files: { "a" => "" }) }
    assert_raises(ArgumentError) { Trongoscript::Context.new(mounts: { "/x" => "." }) }
    assert_raises(ArgumentError) { Trongoscript::Context.new(nope: 1) }
    assert_raises(ArgumentError) { Trongoscript::Context.new(limits: :loose) }
    assert_raises(ArgumentError) { Trongoscript::Context.new(limits: { max_stepz: 1 }) }
    assert_raises(ArgumentError) { Trongoscript::Context.new(sandbox_limits: { max_files: 1 }) }
    assert_raises(ArgumentError) { Trongoscript::Context.new(globals: { "a b" => 1 }) }
    assert_raises(ArgumentError) { Trongoscript::Context.new(files: {}) }
    assert_raises(ArgumentError) { Trongoscript::Context.new(files: { "../x" => "" }) }
    assert_raises(TypeError) { Trongoscript::Context.new(limits: 5) }
    assert_raises(TypeError) { Trongoscript::Context.new(globals: [1]) }
    assert_raises(TypeError) { Trongoscript::Context.new(files: { "a" => 1 }) }
    assert_raises(TypeError) { Trongoscript::Context.new(seed: "1") }
    assert_raises(RangeError) { Trongoscript::Context.new(seed: -1) }
    assert_raises(RangeError) { Trongoscript::Context.new(limits: { max_steps: -1 }) }
    assert_raises(TypeError) { Trongoscript::Context.new(limits: { max_steps: "many" }) }
  end

  def test_limit_presets_accept_strings_and_symbols
    %w[default strict unlimited].each do |preset|
      assert_equal 3, Trongoscript.eval("(+ 1 2)", limits: preset)
      assert_equal 3, Trongoscript.eval("(+ 1 2)", limits: preset.to_sym)
      assert_equal 3, Trongoscript.eval("(+ 1 2)", limits: { "preset" => preset })
    end
    assert_equal 3, Trongoscript.eval("(+ 1 2)", limits: nil)
  end

  def test_bad_sandbox_roots_are_sandbox_errors
    e = assert_raises(Trongoscript::SandboxError) { Trongoscript::Context.new(root: "/definitely/not/here") }
    assert_includes e.message, "/definitely/not/here"
    Dir.mktmpdir do |dir|
      assert_raises(Trongoscript::SandboxError) { Trongoscript::Context.new(root: dir, mounts: { "relative" => dir }) }
      assert_raises(Trongoscript::SandboxError) { Trongoscript::Context.new(root: dir, mounts: { "/x" => "/definitely/not/here" }) }
    end
  end

  def test_invalid_global_names
    ctx = Trongoscript::Context.new
    ["", "a b", "(x)", "x;y"].each do |name|
      assert_raises(ArgumentError, name.inspect) { ctx[name] = 1 }
    end
  end
end
