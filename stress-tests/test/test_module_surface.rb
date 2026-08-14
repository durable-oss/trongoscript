# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# Modules the rest of the suite never loads: `rng`, `finance`, and
# `predicates`.
#
# `rng` is the one that matters most, for a structural reason. Its
# dispatch in `rng.rs` runs *before* the inventory lookup and returns
# without calling `charge_value`, so nothing a random builtin allocates is
# charged against the allocation budget. `(shuffle (range 3000000))`
# copies a three-million-element list and reports 1.6 MB allocated,
# `steps=5`. Running early also means these names shadow same-named
# builtins once the module is loaded, which is its own hazard: `spintax`
# is a two-argument function in `strings` and a one-argument function
# here, and after `(use rng)` the one-argument form wins.
#
# `finance` is small but numerically nasty: every function funnels
# through `powf` with a caller-chosen exponent, and `rate` is the only
# iterative solver in the codebase.
class TestModuleSurface < StressTestCase
  # --- rng ----------------------------------------------------------

  # Shuffle copies its input, so the copy is memory a script chooses the
  # size of. `rng` dispatch runs before the inventory lookup and returns
  # without calling `charge_value` itself, so the charge has to come from
  # somewhere else — today the `(range ...)` that built the input is
  # charged, which covers this case because shuffle's output is the same
  # size as its input.
  #
  # That is worth pinning rather than assuming: it holds only for
  # rng functions whose output is no larger than their input. A random
  # builtin that generated a list from a count would not be covered by
  # anything, and this test is where that gap would show up.
  def test_shuffle_output_is_charged_for_allocation
    result = run_source(
      "(use rng)\n(count (shuffle (range 2000000)))",
      limits: { max_alloc: 100_000 }
    )
    assert_diagnosable(result, "shuffling a 2M-element list")
    refute_equal TrongoHarness::EXIT_OK, result.exitstatus,
                 "(shuffle) copied a 2M-element list under a 100 KB " \
                 "allocation budget without being stopped"
  end

  # `dice-roll` has its own hard cap of a million rolls. That cap is CPU
  # the fuel system does not see: the rolls happen inside one dispatch.
  def test_dice_roll_at_its_internal_cap_is_bounded
    assert_diagnosable(
      run_source(
        "(use rng)\n(dice-roll 1000000 1000000)",
        limits: { max_steps: 1_000 }
      ),
      "a million rolls inside one call must still come back"
    )
  end

  # Rolling repeatedly at the cap multiplies that uncharged CPU by the
  # loop count, which the step budget *does* see — one step per call.
  def test_repeated_dice_rolls_are_charged_per_call
    assert_source_limited(
      "(use rng)\n(count (map (fn [i] (dice-roll 1000000 6)) (range 100000)))",
      /step/i,
      limits: { max_steps: 5_000 },
      msg: "each dice-roll call must cost a step even though the rolls " \
           "inside it are uncharged"
    )
  end

  # `rand-int` computes `(hi - lo) as u64` from two caller-supplied
  # integers, guarded only by `lo < hi`. The full i64 span overflows that
  # subtraction. It wraps in release; a debug build would panic.
  def test_rand_int_over_the_full_i64_span_is_reported
    [
      "(rand-int -9223372036854775808 9223372036854775807)",
      "(rand-int -9223372036854775808 0)",
      "(rand-int -1e300 1e300)",
      "(rand-int 0 1e308)",
      "(rand-int -1e308 -1)"
    ].each do |expr|
      result = run_source("(use rng)\n#{expr}")
      assert_diagnosable(result, "#{expr} must not crash")
      refute_match(/panicked|overflow/, result.stderr,
                   "#{expr} overflowed the `hi - lo` subtraction in rng.rs " \
                   "rather than rejecting the span")
    end
  end

  # Degenerate and non-integral bounds, empty and wrong-typed inputs.
  def test_rng_argument_domains_are_reported
    [
      "(rand-int 5 5)", "(rand-int 5 1)", "(rand-int 1.5 2.5)",
      "(rand-int nil 1)", '(rand-int "a" "b")',
      "(choose [])", "(choose nil)", '(choose "abc")', "(choose 42)",
      "(dice-roll 0 6)", "(dice-roll -1 6)", "(dice-roll 1 0)",
      "(dice-roll 1 -1)", "(dice-roll 1e308 1e308)", "(dice-roll 1.5 6)",
      "(shuffle nil)", "(shuffle 42)", '(shuffle "abc")',
      "(set-seed 1e308)", "(set-seed -1e308)", "(set-seed nil)",
      "(set-seed 1.5)"
    ].each do |expr|
      result = run_source("(use rng)\n#{expr}")
      assert_diagnosable(result, "#{expr} must not crash")
      refute_match(/panicked|unwrap|overflow/, result.stderr,
                   "#{expr} panicked instead of reporting an argument error")
    end
  end

  # `spintax` is registered in two modules with different arities, and
  # rng's dispatch runs first. Loading both must not make either form
  # crash — whichever one wins, the loser has to be an arity error.
  def test_spintax_name_collision_between_modules_is_stable
    [
      '(use rng)(spintax "{a|b}")',
      '(use strings)(spintax "{a|b}" 1)',
      '(use rng strings)(spintax "{a|b}")',
      '(use rng strings)(spintax "{a|b}" 1)',
      '(use strings rng)(spintax "{a|b}" 1)'
    ].each do |source|
      result = run_source(source)
      assert_diagnosable(result, "#{source} must not crash")
      refute_match(/panicked|index out of bounds/, result.stderr,
                   "#{source} panicked — rng's dispatch shadows the strings " \
                   "builtin of the same name and the arities differ")
    end
  end

  # The one-argument spintax draws its seed from the shared context
  # rather than an argument, but expands the same pattern grammar — so
  # the nesting cap that bounds the two-argument form has to apply here
  # too. Without it this is native-stack recursion with no guard.
  def test_context_seeded_spintax_nesting_is_bounded
    assert_diagnosable(
      run_source(
        '(use rng strings)
         (str-len (spintax (repeat "{" 1000000)))',
        limits: { max_steps: 10_000_000 },
        timeout: 30
      ),
      "1M nested braces through the rng spintax overload must not overflow " \
      "the stack — it shares the expander with the strings form"
    )
  end

  # --- finance ------------------------------------------------------

  # Every finance function raises `(1 + rate)` to the period count. The
  # period count is a caller-supplied float, so the result overflows to
  # infinity, and `num_from_f64` turns that into a silent `null` rather
  # than an error. This pins the behavior the way the math-domain test
  # does, so changing it is deliberate.
  def test_finance_functions_survive_extreme_arguments
    [
      "(pmt 0.05 1e308 1000)", "(pmt 1e308 12 1000)", "(pmt 0 12 1000)",
      "(pmt -1 12 1000)", "(pmt 0.05 0 1000)",
      "(fv 1e308 12 100)", "(fv 0.05 1e308 100)", "(fv -1 12 100)",
      "(pv 1e308 12 100)", "(pv 0.05 1e308 100)", "(pv -1 12 100)",
      "(npv 0.05 100)", "(npv -1 100 200)", "(npv 1e308 100 200)",
      "(nper 0.05 -100 1000)", "(nper 0 -100 1000)", "(nper 1e308 -100 1000)",
      "(nper 0.05 100 1000)",
      "(rate 12 -100 1000)", "(rate 1e308 -100 1000)",
      "(rate 12 0 0)", "(rate 12 -100 1000 0 0 1e308)"
    ].each do |expr|
      result = run_source("(use finance)\n#{expr}")
      assert_diagnosable(result, "#{expr} must not crash")
      refute_match(/panicked|unwrap/, result.stderr, "#{expr} panicked")
    end
  end

  # `rate` is Newton's method, capped at 64 iterations. The cap is what
  # keeps a non-converging input from spinning, and it is invisible to
  # the step counter — so a loop of them is the amplification shape.
  def test_repeated_rate_solves_are_charged_per_call
    # Arguments chosen because they converge. A non-converging input
    # errors out on the iteration cap before the step budget is reached,
    # which would make this test pass for the wrong reason.
    assert_source_limited(
      "(use finance)\n(count (map (fn [i] (rate 60 -200 10000)) (range 100000)))",
      /step/i,
      limits: { max_steps: 5_000 },
      msg: "each Newton solve must cost a step; the 64 iterations inside " \
           "one call are not otherwise metered"
    )
  end

  def test_finance_functions_reject_wrong_types
    %w[pmt fv pv nper].each do |fn|
      ['nil', '"str"', '[1 2]', '{:a 1}', 'true'].each do |arg|
        source = "(use finance)\n(#{fn} #{arg} #{arg} #{arg})"
        result = run_source(source)
        assert_diagnosable(result, "#{source} must not crash")
        refute_match(/panicked|unwrap/, result.stderr, "#{source} panicked")
      end
    end
  end

  # --- predicates ---------------------------------------------------

  # `predicates` is written in Trongoscript, not Rust, so its functions
  # are macros that expand into ordinary calls. A macro argument is
  # substituted as an unevaluated form, which means an argument used
  # twice in the body is *evaluated* twice — expensive arguments
  # multiply, and the expansion itself has to stay bounded.
  def test_predicate_macros_do_not_amplify_their_arguments
    assert_diagnosable(
      run_source(
        '(use predicates collections strings)
         (between? (count (uniq (range 100000))) 0 1e9)',
        limits: { max_steps: 1_000_000 }
      ),
      "a macro that substitutes its argument more than once re-evaluates " \
      "it; an expensive argument must still be bounded"
    )
  end

  def test_predicate_macros_reject_wrong_types
    %w[inc dec zero? pos? neg? even? odd? non-nil?].each do |fn|
      ['nil', '"str"', '[1 2]', '{:a 1}', 'true'].each do |arg|
        source = "(use predicates)\n(#{fn} #{arg})"
        result = run_source(source)
        assert_diagnosable(result, "#{source} must not crash")
        refute_match(/panicked|unwrap/, result.stderr, "#{source} panicked")
      end
    end
  end

  # --- module loading -----------------------------------------------

  # `use` is idempotent per context, but a script can still ask for every
  # module many times, and for names that do not exist.
  def test_repeated_and_unknown_module_loads_are_bounded
    many = "(use math)\n" * 50_000
    assert_diagnosable(
      run_source("#{many}(sqrt 4)", limits: { max_steps: 1_000_000 }),
      "50k repeated (use math) forms must be bounded"
    )

    all_modules = "(use math strings collections finance serde rng predicates)"
    assert_completes(
      run_source("#{all_modules}\n(+ 1 1)"),
      "loading every module at once must work"
    )

    ['(use nonexistent)', '(use math nonexistent)', '(use "math")',
     "(use 'math)", '(use :math)', '(use nil)', '(use 42)'].each do |source|
      result = run_source(source)
      assert_diagnosable(result, "#{source} must not crash")
      refute_match(/panicked|unwrap/, result.stderr, "#{source} panicked")
    end
  end

  # Loading every module at once puts every same-named builtin in scope
  # simultaneously. The dispatch order decides which wins; the test only
  # claims that no combination produces a crash.
  def test_all_modules_loaded_together_do_not_collide_into_a_crash
    prelude = "(use math strings collections finance serde rng predicates)\n"
    ["(sort [3 1 2])", '(spintax "{a|b}" 1)', '(spintax "{a|b}")',
     '(repeat "a" 3)', "(pow 2 3)", "(count (take 2 [1 2 3]))"].each do |expr|
      result = run_source(prelude + expr)
      assert_diagnosable(result, "#{prelude}#{expr} must not crash")
      refute_match(/panicked|index out of bounds/, result.stderr,
                   "#{expr} panicked with every module loaded")
    end
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
