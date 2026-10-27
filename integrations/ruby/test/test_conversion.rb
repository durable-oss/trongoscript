# frozen_string_literal: true

require "test_helper"
require "set"

# Values crossing between Ruby and Trongoscript, in both directions.
class TestConversion < Minitest::Test
  def setup
    @ctx = Trongoscript::Context.new
  end

  def round_trip(value)
    @ctx["v"] = value
    via_eval = @ctx.eval("v")
    via_get = @ctx["v"]
    assert_value via_eval, via_get
    via_eval
  end

  def test_round_trips_supported_values
    [
      nil, true, false, 0, 1, -1, 2**62, -(2**63), 2**63 - 1, 2**63, 2**64 - 1,
      0.0, 1.5, -2.25, 1e300, -1e-300, Float::MAX, Float::MIN,
      "", "s", "a\0b", "héllo ☃ 𝄞", "line\nbreak", "x" * 100_000,
      [], [1, [2, [3]]], {}, { "a" => { "b" => [nil, { "c" => "d" }] } },
      (1..1000).to_a, (1..1000).to_h { |i| ["k#{i}", i] }
    ].each do |v|
      got = round_trip(v)
      assert_value v, got, v.inspect[0, 80]
      assert_equal v.class, got.class, v.inspect[0, 80]
    end
  end

  def test_large_unsigned_integers_survive
    assert_equal 2**64 - 1, round_trip(2**64 - 1)
    assert_equal 2**63, round_trip(2**63)
  end

  def test_floats_with_integral_values_stay_floats
    got = round_trip(2.0)
    assert_equal 2.0, got
    assert_kind_of Float, got
  end

  def test_negative_zero
    got = round_trip(-0.0)
    assert_equal 0.0, got
    assert_kind_of Float, got
  end

  def test_symbols_become_strings
    assert_equal "sym", round_trip(:sym)
    assert_equal({ "a" => "b" }, round_trip({ a: :b }))
  end

  def test_subclasses_are_accepted
    hash_class = Class.new(Hash)
    array_class = Class.new(Array)
    string_class = Class.new(String)
    h = hash_class.new
    h["a"] = array_class.new([string_class.new("x")])
    assert_equal({ "a" => ["x"] }, round_trip(h))
  end

  def test_frozen_inputs
    v = { "a" => ["b"].freeze }.freeze
    assert_equal v, round_trip(v)
  end

  def test_results_are_mutable
    got = @ctx.eval('{:a ["x"]}')
    refute_predicate got, :frozen?
    refute_predicate got["a"], :frozen?
    refute_predicate got["a"][0], :frozen?
    got["a"][0] << "y"
    assert_equal "xy", got["a"][0]
  end

  def test_integers_beyond_64_bits_are_refused
    [2**64, -(2**63) - 1, 10**30].each do |n|
      e = assert_raises(RangeError) { @ctx["v"] = n }
      assert_includes e.message, "64 bits"
    end
  end

  def test_non_finite_floats_are_refused
    [Float::NAN, Float::INFINITY, -Float::INFINITY].each do |f|
      assert_raises(FloatDomainError) { @ctx["v"] = f }
    end
  end

  def test_strings_are_transcoded_to_utf8
    latin1 = "caf\xE9".dup.force_encoding(Encoding::ISO_8859_1)
    assert_equal "café", round_trip(latin1)
    sjis = "日本".encode(Encoding::Shift_JIS)
    assert_equal "日本", round_trip(sjis)
  end

  def test_ascii_only_binary_strings_are_accepted
    assert_equal "abc", round_trip("abc".b)
  end

  def test_strings_that_are_not_text_are_refused
    assert_raises(EncodingError) { @ctx["v"] = "\xFF\xFE".b }
    assert_raises(EncodingError) { @ctx["v"] = "\xFF".dup.force_encoding(Encoding::UTF_8) }
    assert_raises(EncodingError) { @ctx["v"] = { "\xFF".b => 1 } }
  end

  def test_unsupported_types_are_refused
    [Object.new, Time.now, 1r, 1i, Set[1], 1..2, Struct.new(:a).new(1), -> {}, Class, BasicObject.new].each do |v|
      e = assert_raises(TypeError) { @ctx["v"] = v }
      assert_includes e.message, "cannot convert"
    end
  end

  def test_unsupported_types_nested_deep_inside_are_refused
    assert_raises(TypeError) { @ctx["v"] = { "a" => [1, { "b" => [Object.new] }] } }
  end

  def test_hash_keys_must_be_strings_or_symbols
    [1, nil, 1.5, [1], { a: 1 }].each do |k|
      e = assert_raises(TypeError) { @ctx["v"] = { k => 1 } }
      assert_includes e.message, "hash keys"
    end
  end

  def test_keys_that_collide_after_conversion_are_refused
    e = assert_raises(ArgumentError) { @ctx["v"] = { "a" => 1, a: 2 } }
    assert_includes e.message, "duplicate key"
  end

  def test_self_referential_structures_are_refused
    a = []
    a << a
    assert_raises(ArgumentError) { @ctx["v"] = a }
    h = {}
    h["self"] = h
    assert_raises(ArgumentError) { @ctx["v"] = h }
  end

  def test_shared_but_acyclic_references_are_fine
    shared = [1, 2]
    assert_equal [[1, 2], [1, 2]], round_trip([shared, shared])
  end

  def test_nesting_limit_on_the_way_in
    @ctx["v"] = nested(1024)
    assert_equal nested(1024), @ctx["v"]
    e = assert_raises(ArgumentError) { @ctx["v"] = nested(1025) }
    assert_includes e.message, "1024"
  end

  def test_nesting_limit_on_the_way_out
    assert_equal nested(1023, []), @ctx.eval(deep_list_source(1024))
    e = assert_raises(Trongoscript::ResourceLimitError) { @ctx.eval(deep_list_source(1025)) }
    assert_includes e.message, "1024"
  end

  def test_scripts_may_keep_deep_values_internally
    @ctx.eval("(def deep #{deep_list_source(2000)}) nil")
    assert_equal 1, @ctx.eval("(count deep)")
    assert @ctx.key?("deep")
    assert_raises(Trongoscript::ResourceLimitError) { @ctx["deep"] }
    assert_raises(Trongoscript::ResourceLimitError) { @ctx.globals }
    assert @ctx.delete("deep")
  end

  def test_validation_does_not_partially_apply
    @ctx["v"] = 1
    assert_raises(TypeError) { @ctx["v"] = [1, Object.new] }
    assert_equal 1, @ctx["v"]
  end

  def test_large_collections
    big = Array.new(200_000) { |i| i }
    assert_equal big, round_trip(big)
    wide = (1..50_000).to_h { |i| ["key#{i}", { "n" => i }] }
    assert_equal wide, round_trip(wide)
  end

  def test_conversion_survives_gc_stress
    value = { "list" => (1..20).map { |i| { "i" => i, "s" => "str#{i}", "f" => i / 3.0 } } }
    GC.stress = true
    begin
      got = round_trip(value)
    ensure
      GC.stress = false
    end
    assert_equal value, got
  end
end
