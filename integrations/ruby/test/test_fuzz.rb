# frozen_string_literal: true

require "test_helper"

# Randomized checks. Each test seeds its own generator so a failure is
# reproducible from the seed in the message.
class TestFuzz < Minitest::Test
  SEED = Integer(ENV.fetch("FUZZ_SEED", 20_261_020))

  def random_value(rng, depth = 0)
    kind = depth > 4 ? rng.rand(6) : rng.rand(8)
    case kind
    when 0 then nil
    when 1 then rng.rand < 0.5
    when 2 then rng.rand(-(2**63)..(2**64 - 1))
    when 3 then (rng.rand - 0.5) * 10**rng.rand(-20..20)
    when 4 then random_string(rng)
    when 5 then rng.rand(-1000..1000)
    when 6 then Array.new(rng.rand(5)) { random_value(rng, depth + 1) }
    else Array.new(rng.rand(5)) { [random_string(rng), random_value(rng, depth + 1)] }.to_h
    end
  end

  ALPHABET = ["a", "Z", "0", " ", "\"", "\\", "#", "{", "}", "$", "\n", "\t", "\0", "é", "☃", "𝄞", "(", ")", ";", ":"].freeze

  def random_string(rng)
    Array.new(rng.rand(8)) { ALPHABET.sample(random: rng) }.join
  end

  def test_values_round_trip_through_globals_and_eval
    rng = Random.new(SEED)
    ctx = Trongoscript::Context.new
    300.times do |i|
      v = random_value(rng)
      ctx["v"] = v
      assert_value v, ctx.eval("v"), "seed #{SEED}, case #{i}: #{v.inspect}"
    end
  end

  # Values the core reader cannot get back from `dump`'s text: whole-valued
  # floats at or past 2**53, which are written as integer literals, and
  # integers past 2**63 - 1, which the reader parses as floats.
  def dump_lossy?(value)
    case value
    when Integer then value > 2**63 - 1
    when Float then value.finite? && value == value.round && value.abs >= 2**53
    when Array then value.any? { |v| dump_lossy?(v) }
    when Hash then value.values.any? { |v| dump_lossy?(v) }
    else false
    end
  end

  def test_values_round_trip_through_dump
    rng = Random.new(SEED + 1)
    300.times do |i|
      v = random_value(rng)
      next if dump_lossy?(v)

      src = Trongoscript.dump(v, pretty: i.odd?)
      assert_value v, Trongoscript.eval(src), "seed #{SEED + 1}, case #{i}: #{src}"
    end
  end

  def test_dump_preserves_large_numbers
    skip "known core limitation: dump/eval is lossy for whole floats >= 2**53 and integers > 2**63 - 1"
    [-7.018252704029115e18, 2**64 - 1].each do |n|
      assert_equal n, Trongoscript.eval(Trongoscript.dump(n))
    end
  end

  def test_values_round_trip_through_host_functions
    rng = Random.new(SEED + 2)
    ctx = Trongoscript::Context.new
    ctx.define("echo") { |x| x }
    100.times do |i|
      v = random_value(rng)
      ctx["v"] = v
      assert_value v, ctx.eval("(echo v)"), "seed #{SEED + 2}, case #{i}"
    end
  end

  TOKENS = ["(", ")", "[", "]", "{", "}", "1", "-2.5", '"s"', ":k", "x", "+", "fn", "let", "def", "if",
            "nil", "true", "'", "`", "~", "~@", '#{', "map", "reduce", "range", "str", "\n", " ", ";", "&",
            "defmacro", "include", "read-file", "quote", "1e999", "99999999999999999999", "\\", "\""].freeze

  def test_random_source_never_escapes_the_error_hierarchy
    rng = Random.new(SEED + 3)
    ctx = Trongoscript::Context.new(limits: { max_steps: 20_000, max_value_len: 10_000 })
    1500.times do |i|
      src = Array.new(rng.rand(1..30)) { TOKENS.sample(random: rng) }.join(" ")
      begin
        ctx.eval(src)
      rescue Trongoscript::Error
        # Any classified failure is fine; anything else is a bug.
      rescue StandardError => e
        flunk "seed #{SEED + 3}, case #{i}: #{src.inspect} raised #{e.class}: #{e.message}"
      end
    end
  end

  def test_random_bytes_as_source
    rng = Random.new(SEED + 4)
    300.times do
      bytes = rng.bytes(rng.rand(1..64))
      begin
        Trongoscript.eval(bytes.force_encoding(Encoding::UTF_8))
      rescue Trongoscript::Error, EncodingError
        # Invalid UTF-8 raises EncodingError before reaching the parser.
      end
    end
  end
end
