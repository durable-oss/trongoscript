# frozen_string_literal: true

require "test_helper"

class TestEval < Minitest::Test
  def test_scalars
    assert_equal 42, Trongoscript.eval("42")
    assert_equal(-7, Trongoscript.eval("-7"))
    assert_in_delta 3.5, Trongoscript.eval("3.5")
    assert_kind_of Float, Trongoscript.eval("3.5")
    assert_equal true, Trongoscript.eval("true")
    assert_equal false, Trongoscript.eval("false")
    assert_nil Trongoscript.eval("nil")
    assert_equal "s", Trongoscript.eval('"s"')
    assert_equal "kw", Trongoscript.eval(":kw")
  end

  def test_integers_stay_integers
    assert_kind_of Integer, Trongoscript.eval("(+ 1 2)")
    assert_kind_of Integer, Trongoscript.eval("(/ 4 2)")
    assert_in_delta 0.5, Trongoscript.eval("(/ 1 2)")
  end

  def test_collections
    assert_equal [1, [2, [3]], []], Trongoscript.eval("[1 [2 [3]] []]")
    assert_equal({ "a" => 1, "b" => { "c" => [true] } }, Trongoscript.eval("{:a 1 :b {:c [true]}}"))
    assert_equal({}, Trongoscript.eval("{}"))
  end

  def test_map_keys_come_back_sorted
    assert_equal %w[a b c], Trongoscript.eval("{:c 1 :a 2 :b 3}").keys
  end

  def test_value_of_the_last_form
    assert_equal 3, Trongoscript.eval("1 2 3")
    assert_nil Trongoscript.eval("")
    assert_nil Trongoscript.eval("   ; just a comment\n")
  end

  def test_strings_are_utf8
    s = Trongoscript.eval('"héllo ☃ 𝄞"')
    assert_equal "héllo ☃ 𝄞", s
    assert_equal Encoding::UTF_8, s.encoding
    assert_predicate s, :valid_encoding?
    assert_equal "HÉLLO", Trongoscript.eval('(upcase "héllo")')
  end

  def test_string_interpolation
    assert_equal "hi ada", Trongoscript.eval('"hi #{name}"', globals: { name: "ada" })
  end

  def test_source_in_other_encodings_is_transcoded
    source = '"café"'.encode(Encoding::ISO_8859_1)
    assert_equal "café", Trongoscript.eval(source)
  end

  def test_source_must_be_a_string
    assert_raises(TypeError) { Trongoscript.eval(42) }
    assert_raises(TypeError) { Trongoscript.eval(nil) }
  end

  def test_one_shot_calls_do_not_share_state
    Trongoscript.eval("(def x 1)")
    assert_raises(Trongoscript::EvalError) { Trongoscript.eval("x") }
  end

  def test_context_keeps_definitions
    ctx = Trongoscript::Context.new
    ctx.eval("(def base 40)")
    ctx.eval("(defn add-base [n] (+ base n))")
    ctx.eval("(defmacro twice [x] `(* 2 ~x))")
    assert_equal 42, ctx.eval("(add-base 2)")
    assert_equal 84, ctx.eval("(twice (add-base 2))")
  end

  def test_modules_stay_enabled
    ctx = Trongoscript::Context.new(seed: 1)
    ctx.eval("(use rng)")
    assert_kind_of Integer, ctx.eval("(rand-int 0 10)")
  end

  def test_seed_controls_randomness
    roll = ->(seed) { Trongoscript.eval("(use rng) [(rand-int 0 1000000) (rand-int 0 1000000)]", seed: seed) }
    assert_equal roll.(5), roll.(5)
    refute_equal roll.(5), roll.(6)
    assert_equal roll.(0), roll.(nil), "the default seed is 0"
  end

  def test_globals_option
    ctx = Trongoscript::Context.new(globals: { "env" => "prod", port: 443, tags: %w[a b] })
    assert_equal ["prod", 443, %w[a b]], ctx.eval("[env port tags]")
  end

  def test_global_accessors
    ctx = Trongoscript::Context.new
    ctx[:b] = { "x" => [1, 2] }
    ctx["a"] = 1
    assert_equal({ "x" => [1, 2] }, ctx[:b])
    assert_equal 1, ctx.eval("a")
    assert ctx.key?("a")
    refute ctx.key?("zzz")
    assert_nil ctx["zzz"]
    assert_equal %w[a b], ctx.globals.keys
    assert ctx.delete(:a)
    refute ctx.delete(:a)
    refute ctx.key?(:a)
  end

  def test_a_nil_global_exists
    ctx = Trongoscript::Context.new(globals: { "n" => nil })
    assert ctx.key?("n")
    assert_nil ctx.eval("n")
  end

  def test_definitions_are_visible_as_globals
    ctx = Trongoscript::Context.new
    ctx.eval('(def greeting "hi")')
    assert_equal "hi", ctx["greeting"]
  end

  def test_returned_values_are_copies
    ctx = Trongoscript::Context.new(globals: { "h" => { "x" => [1] } })
    got = ctx["h"]
    got["x"] << 2
    got["y"] = true
    assert_equal({ "x" => [1] }, ctx["h"])
    assert_equal({ "x" => [1] }, ctx.eval("h"))
  end

  def test_inputs_are_copied
    input = { "x" => [1] }
    ctx = Trongoscript::Context.new
    ctx["h"] = input
    input["x"] << 2
    assert_equal({ "x" => [1] }, ctx["h"])
  end

  def test_symbolize_names
    ctx = Trongoscript::Context.new(symbolize_names: true, globals: { "g" => 1 })
    assert_equal({ a: { b: [{ c: "d" }] } }, ctx.eval('{:a {:b [{:c "d"}]}}'))
    assert_equal({ g: 1 }, ctx.globals)
    assert_equal "d", ctx.eval('"d"'), "string values are not symbolized"
  end

  def test_call_by_name
    ctx = Trongoscript::Context.new
    ctx.eval('(defn greet [who punct] (str "hi " who punct))')
    assert_equal "hi bo!", ctx.call("greet", "bo", "!")
    assert_equal "hi bo?", ctx.call(:greet, "bo", "?")
    assert_equal "ABC", ctx.call("upcase", "abc")
    assert_raises(Trongoscript::EvalError) { ctx.call("missing") }
    assert_raises(ArgumentError) { ctx.call("not a name") }
  end

  def test_call_arguments_are_data
    ctx = Trongoscript::Context.new
    ctx.eval("(defn ident [x] x)")
    ["(boom)", '#{x}', "${x}", { "k" => "(boom)" }, [1, nil, "x"], {}, [], -0.25, 2**63, -(2**63)].each do |v|
      assert_equal v, ctx.call("ident", v)
    end
  end

  def test_call_a_function_value
    ctx = Trongoscript::Context.new
    adder = ctx.eval("(let [k 10] (fn [x] (+ x k)))")
    assert_equal 15, ctx.call(adder, 5)
    ctx.eval("(defn mul [a b] (* a b))")
    assert_equal 12, ctx.call(ctx["mul"], 3, 4)
    assert_raises(Trongoscript::EvalError) { ctx.call({ "not" => "a function" }) }
  end

  def test_function_values_with_symbolized_names
    ctx = Trongoscript::Context.new(symbolize_names: true)
    f = ctx.eval("(fn [x] (* x x))")
    assert_kind_of Hash, f
    assert_equal 49, ctx.call(f, 7)
  end

  def test_eval_file
    ctx = Trongoscript::Context.new(files: {
                                      "app/main.trongo" => '{:part (include "part.trongo")}',
                                      "app/part.trongo" => "[1 2]"
                                    })
    assert_equal({ "part" => [1, 2] }, ctx.eval_file("app/main.trongo"))
    assert_equal({ "part" => [1, 2] }, ctx.eval_file(Pathname("/app/main.trongo")))
    assert_equal [1, 2], Trongoscript.eval_file("x.trongo", files: { "x.trongo" => "[1 2]" })
  end

  def test_render
    out = Trongoscript.render("Hello <%= (upcase name) %>!<% (def n 3) %> n=<%= n %>", globals: { name: "Ada" })
    assert_equal "Hello ADA! n=3", out
    assert_equal Encoding::UTF_8, out.encoding
    assert_equal "plain ☃", Trongoscript.render("plain ☃")
    assert_equal "", Trongoscript.render("")
  end

  def test_render_keeps_definitions_in_a_context
    ctx = Trongoscript::Context.new
    ctx.render("<% (def x 5) %>")
    assert_equal 5, ctx.eval("x")
  end

  def test_dump_round_trips
    value = { "a" => [1, 2.5, "q\"uote", 'i#{nterp}', "$", "\n\t", nil, true, false], "b" => {}, "c" => [] }
    [false, true].each do |pretty|
      src = Trongoscript.dump(value, pretty: pretty)
      assert_kind_of String, src
      assert_equal value, Trongoscript.eval(src), src
    end
    assert_includes Trongoscript.dump({ "a" => { "b" => [1] } }, pretty: true), "\n"
    assert_equal "nil", Trongoscript.dump(nil)
    assert_raises(TypeError) { Trongoscript.dump(Object.new) }
  end

  def test_usage
    ctx = Trongoscript::Context.new
    ctx.eval("(reduce (fn [a x] (+ a x)) 0 (range 1000))")
    big = ctx.usage
    assert_equal %i[allocated peak_call_depth peak_eval_depth steps], big.keys.sort
    assert_operator big[:steps], :>, 1000
    assert_operator big[:peak_call_depth], :>=, 1
    ctx.eval("1")
    assert_operator ctx.usage[:steps], :<, 10
  end

  def test_inspect
    ctx = Trongoscript::Context.new
    ctx.define("f") { 1 }
    assert_equal '#<Trongoscript::Context functions=["f"]>', ctx.inspect
  end

  def test_version
    assert_match(/\A\d+\.\d+\.\d+/, Trongoscript::VERSION)
  end
end
