# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# Argument values a builtin is unlikely to have been written for.
#
# The tests above this one attack size and time. These attack the
# *domain*: indices past the end, counts that overflow a usize, negative
# lengths, NaN, infinities, and empty inputs. None of them should produce
# a large allocation, so none is expected to trip a fuel limit — the
# claim is narrower and, for a sandboxed interpreter, just as important:
# every one comes back with a message rather than a panic.
#
# A Rust panic in a builtin unwinds to whatever the embedder set up. In
# the CLI that is an abort with a backtrace, which the harness sees as a
# signal — indistinguishable from the stack overflows the rest of this
# suite exists to catch. So `assert_diagnosable` is the right assertion
# even where the expected outcome is an ordinary error.
class TestBuiltinArguments < StressTestCase
  # A builtin taking an index or count must handle values past the end of
  # a usize, past the end of the collection, and negative.
  EXTREME_COUNTS = [
    "999999999999999",   # past any real collection length
    "-1",                # negative where a count is expected
    "0",                 # boundary
    "1e308",             # past f64->usize conversion range
    "-1e308"
  ].freeze

  def test_index_and_count_builtins_survive_extreme_values
    cases = {
      "nth" => '(nth [1 2 3] %s)',
      "substr" => '(substr "abcdef" %s)',
      "take" => '(use collections)(count (take (range 10) %s))',
      "drop" => '(use collections)(count (drop (range 10) %s))',
      "truncate" => '(use strings)(truncate "abcdef" %s)',
      "repeat" => '(use strings)(str-len (repeat "ab" %s))',
      "pad-left" => '(use strings)(str-len (pad-left "ab" %s "x"))',
      "partition" => '(use collections)(count (partition (range 10) %s))',
      "round" => '(round 1.23456 %s)'
    }

    cases.each do |name, template|
      EXTREME_COUNTS.each do |count|
        assert_diagnosable(
          run_source(format(template, count)),
          "(#{name} ... #{count}) must report rather than panic"
        )
      end
    end
  end

  # Empty collections and empty strings are the other end of the same
  # axis: a builtin that indexes without checking is as likely to panic
  # on nothing as on too much.
  def test_collection_builtins_survive_empty_input
    [
      '(first [])', '(last [])', '(rest [])', '(nth [] 0)',
      '(use collections)(sum [])', '(use collections)(avg [])',
      '(use collections)(sort [])', '(use collections)(uniq [])',
      '(use collections)(flatten [])', '(use collections)(frequencies [])',
      '(use collections)(zip [] [])', '(use collections)(partition [] 2)',
      '(use collections)(take [] 5)', '(use collections)(drop [] 5)',
      '(join [] ",")', '(reverse [])', '(count [])',
      '(use strings)(chars "")', '(use strings)(lines "")',
      '(use strings)(str-reverse "")', '(use strings)(to-titlecase "")',
      '(use strings)(strip-html "")', '(use strings)(spintax "" 0)',
      '(split "" ",")', '(trim "")', '(str-len "")'
    ].each do |source|
      assert_diagnosable(
        run_source(source),
        "empty input to #{source.inspect} must not panic"
      )
    end
  end

  # `avg` and `sum` over an empty list, and division generally, are the
  # obvious division-by-zero sites. So is `mod` by zero.
  def test_division_and_modulo_by_zero_are_reported
    ['(/ 1 0)', '(mod 1 0)', '(use collections)(avg [])',
     '(/ 0 0)', '(mod 0 0)', '(/ 1.5 0.0)'].each do |source|
      result = run_source(source)
      assert_diagnosable(result, "#{source} must not panic")
      refute_match(/panicked/, result.stderr,
                   "#{source} panicked instead of returning an error")
    end
  end

  # Math functions have restricted domains. Outside them, f64 yields NaN
  # or an infinity — neither of which is representable in JSON, so a
  # builtin that returns one either has to reject it or produce something
  # the serializer accepts. Silently emitting `null` for infinity loses
  # the error; this test pins the behavior so a change is deliberate.
  #
  # `sqrt`, `log`, `asin` and `acos` check their domain before computing.
  # `pow` and `exp` cannot: whether they overflow depends on both
  # arguments together, so they check the result instead. Either way the
  # failure has to reach the caller as an error rather than as a null
  # indistinguishable from a legitimate one.
  def test_math_domain_errors_are_reported_not_silently_null
    {
      "(sqrt -1)" => "square root of a negative",
      "(log 0)" => "log of zero",
      "(log -1)" => "log of a negative",
      "(asin 2)" => "asin outside [-1, 1]",
      "(acos -2)" => "acos outside [-1, 1]",
      "(pow 10 100000)" => "overflow to infinity",
      "(pow 0 -1)" => "zero to a negative power",
      "(exp 100000)" => "exp overflow"
    }.each do |expr, why|
      result = run_source("(use math)\n#{expr}")
      assert_diagnosable(result, "#{why}: #{expr} must not panic")
      refute_match(/panicked/, result.stderr, "#{expr} panicked")
      refute_equal TrongoHarness::EXIT_OK, result.exitstatus,
                   "#{why}: #{expr} succeeded — the failure was lost"
      refute_match(/\Anull\s*\z/, result.stdout,
                   "#{why}: #{expr} returned null, which is " \
                   "indistinguishable from a legitimate result")
    end
  end

  # The overflow check must not reject results that are merely large, or
  # negative, or fractional. Only a genuine infinity is an error.
  def test_math_functions_still_return_ordinary_results
    {
      "(pow 2 10)" => "1024",
      "(pow 2 -1)" => "0.5",
      "(pow 10 308)" => nil,   # large but finite
      "(exp 1)" => nil,
      "(exp 0)" => "1",
      "(exp -100000)" => "0"   # underflow to zero is representable
    }.each do |expr, expected|
      result = run_source("(use math)\n#{expr}")
      assert_equal TrongoHarness::EXIT_OK, result.exitstatus,
                   "#{expr} was rejected but is perfectly representable"
      assert_equal expected, result.stdout.strip if expected
    end
  end

  # `range` with a step that does not advance, or one small enough that
  # the count overflows, is the arithmetic form of an infinite loop.
  def test_range_with_degenerate_steps_is_bounded
    [
      "(count (range 0 1000000 0))",       # zero step: never terminates
      "(count (range 0 1000000 -1))",      # wrong-direction step
      "(count (range 0 1 0.0000000001))",  # 1e10 elements from tiny source
      "(count (range 1e308 -1e308 -1))"    # count overflows f64->u64
    ].each do |source|
      assert_diagnosable(
        run_source(source, limits: { max_steps: 1_000_000 }),
        "#{source} must terminate — range's predicted size guard has to " \
        "handle steps that do not advance or that overflow the count"
      )
    end
  end

  # Type confusion: every builtin gets an argument of every wrong shape.
  # The claim is only that the interpreter reports a type error rather
  # than unwrapping something that isn't there.
  WRONG_TYPES = ['nil', 'true', '42', '"str"', '[1 2]', '{:a 1}'].freeze

  def test_builtins_reject_wrong_argument_types_without_panicking
    templates = [
      '(count %s)', '(first %s)', '(str-len %s)', '(upcase %s)',
      '(reverse %s)', '(join %s ",")', '(split %s ",")', '(keys %s)',
      '(use collections)(sort %s)', '(use collections)(sum %s)',
      '(use collections)(flatten %s)', '(use math)(sqrt %s)',
      '(use math)(parse-int %s)', '(use strings)(chars %s)',
      '(use strings)(spintax %s 1)', '(use serde)(to-json %s)',
      '(use serde)(parse-json %s)'
    ]

    templates.each do |template|
      WRONG_TYPES.each do |arg|
        source = format(template, arg)
        result = run_source(source)
        assert_diagnosable(result, "#{source} must not panic")
        refute_match(/panicked|unwrap/, result.stderr,
                     "#{source} panicked instead of reporting a type error")
      end
    end
  end

  # Unicode is where string builtins that index by byte rather than by
  # character panic: slicing a UTF-8 string at a non-boundary aborts.
  def test_string_builtins_handle_multibyte_input
    # Combining marks, astral-plane characters, and a right-to-left mark.
    tricky = '"é́\U0001F600‏abc"'
    [
      "(substr #{tricky} 1)", "(substr #{tricky} 1 3)",
      "(str-len #{tricky})", "(upcase #{tricky})", "(downcase #{tricky})",
      "(capitalize #{tricky})", "(reverse #{tricky})", "(trim #{tricky})",
      "(use strings)(chars #{tricky})",
      "(use strings)(str-reverse #{tricky})",
      "(use strings)(truncate #{tricky} 2)",
      "(use strings)(to-titlecase #{tricky})",
      "(use strings)(to-snakecase #{tricky})",
      "(use strings)(pad-left #{tricky} 10 \"x\")",
      "(use strings)(index-of #{tricky} \"a\")"
    ].each do |source|
      result = run_source(source)
      assert_diagnosable(result, "#{source} must not panic on multibyte input")
      refute_match(/panicked|byte index|char boundary/, result.stderr,
                   "#{source} sliced a UTF-8 string at a non-boundary")
    end
  end

  # An empty separator makes split and replace degenerate: splitting on
  # nothing yields one element per character, and replacing nothing with
  # something can loop.
  def test_empty_separator_and_pattern_are_bounded
    [
      '(count (split "abcdef" ""))',
      '(str-len (replace "abcdef" "" "X"))',
      '(use strings)(replace-first "abcdef" "" "X")',
      '(use strings)(index-of "abcdef" "")',
      '(count (split "" ""))'
    ].each do |source|
      assert_diagnosable(
        run_source(source, limits: { max_steps: 1_000_000 }),
        "#{source} must terminate"
      )
    end
  end

  # Replacing a short pattern with a long one is amplification driven by
  # the number of matches, which the caller controls.
  def test_replace_amplification_is_bounded
    assert_diagnosable(
      run_source(
        '(use strings)
         (str-len (replace (repeat "a" 1000000) "a" (repeat "b" 1000)))',
        limits: { max_steps: 1_000_000 }
      ),
      "1M matches each expanding 1000x is a gigabyte from two small inputs"
    )
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
