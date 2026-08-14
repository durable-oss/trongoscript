# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# Calling every builtin with the wrong number of arguments.
#
# `test_builtin_arguments.rb` attacks argument *values*. This file attacks
# argument *count*, which is a different bug class with a different cause.
# A builtin registered as variadic declares `min_args = 0`, and then its
# body reads `args[0]` because in practice it is always called with
# something. Nothing rejects the empty call on the way in, so the index
# panics.
#
# That is not a hypothetical. `(conj)` does exactly this today
# (`collection.rs:78`) and aborts the interpreter:
#
#     thread 'trongo-eval' panicked at collection.rs:78:35:
#     index out of bounds: the len is 0 but the index is 0
#
# `assoc` has the identical registration but re-validates its arity in the
# body, so it reports instead. The difference is a line of code in one
# builtin, which is precisely why this needs to be swept mechanically
# rather than checked by eye.
#
# The sweep is generated from the language reference, so a builtin added
# to the stdlib without an arity check is covered here the day it lands.
class TestBuiltinArity < StressTestCase
  # Builtins grouped by the module that has to be loaded first. Taken
  # from `trongo reference`; core needs no `(use ...)`.
  BUILTINS = {
    nil => %w[
      + - * / mod abs floor ceil round min max = not= < <= > >= not
      nil? bool? number? string? list? map? empty?
      str upcase downcase capitalize trim str-len split join substr
      replace starts-with? ends-with? contains-str?
      count first last rest reverse keys values nth get contains?
      cons list concat conj assoc merge range
      map mapcar mapc maplist mapl mapcan mapcon filter reduce
      every notevery some notany count-if count-if-not
      find-if find-if-not position-if position-if-not
      member-if member-if-not remove-if remove-if-not
      assoc-if assoc-if-not rassoc-if rassoc-if-not
      subst-if subst-if-not substitute-if substitute-if-not
      sort stable-sort funcall apply identity constantly complement
      remove-duplicates tree-equal union intersection
      set-difference set-exclusive-or
    ],
    "math" => %w[
      sqrt cbrt pow log log10 log2 exp sin cos tan asin acos atan atan2
      sign gcd lcm pi e parse-int parse-float
    ],
    "strings" => %w[
      lstrip rstrip truncate pad-left pad-right repeat chars lines
      str-reverse index-of replace-first to-uppercase to-lowercase
      to-titlecase to-sentencecase to-snakecase to-camelcase strip-html
      spintax
    ],
    "collections" => %w[
      sort uniq sum avg flatten partition zip take drop frequencies
      pluck group-by
    ],
    "finance" => %w[pmt fv pv npv nper rate],
    "serde" => %w[to-json to-yaml to-toml to-trongo parse-json parse-yaml parse-toml],
    "rng" => %w[rand rand-int choose dice-roll shuffle set-seed]
  }.freeze

  # Argument counts to try against every one of them. Zero is the case
  # that finds the `args[0]` bug; the large counts find builtins that
  # assume a maximum.
  ARG_COUNTS = [0, 1, 2, 3, 7].freeze

  # A panic in a builtin aborts the process, which the harness sees as a
  # signal — indistinguishable from the stack overflows the rest of this
  # suite exists to catch. Wrong arity has to be an error message.
  def test_every_builtin_survives_every_argument_count
    failures = []

    BUILTINS.each do |mod, names|
      prelude = mod ? "(use #{mod})\n" : ""
      names.each do |name|
        ARG_COUNTS.each do |argc|
          args = (["1"] * argc).join(" ")
          source = "#{prelude}(#{name}#{args.empty? ? "" : " #{args}"})"
          result = run_source(source)

          unless result.diagnosable?
            failures << "(#{name}) with #{argc} args: #{result.describe}"
            next
          end
          if result.stderr =~ /panicked|index out of bounds|unwrap/
            failures << "(#{name}) with #{argc} args panicked: " \
                        "#{result.stderr.lines.grep(/panicked/).first&.strip}"
          end
        end
      end
    end

    assert_empty failures,
                 "builtins that panicked or crashed on an argument count they " \
                 "were not written for. A variadic registration with " \
                 "`min_args = 0` whose body reads `args[0]` is the usual " \
                 "cause; the fix is an arity check in the body, as `assoc` " \
                 "has and `conj` does not:\n  #{failures.join("\n  ")}"
  end

  # The specific instance the sweep above was written for, called out on
  # its own so a regression names itself rather than appearing as one
  # line in a list of many.
  def test_conj_with_no_arguments_reports_rather_than_panicking
    result = run_source("(conj)")
    assert_diagnosable(result, "(conj) with no arguments")
    refute_match(/panicked|index out of bounds/, result.stderr,
                 "(conj) is registered variadic with min_args = 0 but reads " \
                 "args[0] unconditionally (collection.rs:78), so the empty " \
                 "call indexes an empty slice and aborts the process. It " \
                 "needs the arity check `assoc` already has")
  end

  # The same shape one level up: a user-defined function called with the
  # wrong number of arguments, including a rest parameter that receives
  # nothing.
  def test_user_defined_function_arity_mismatches_are_reported
    [
      '((fn [a b] a) 1)',
      '((fn [a] a) 1 2 3)',
      '((fn [] 1) 1)',
      '((fn [a & rest] rest))',
      '((fn [a & rest] rest) 1)',
      '(defn f [a b] a) (f)',
      '(defn f [a & rest] rest) (f)'
    ].each do |source|
      result = run_source(source)
      assert_diagnosable(result, "#{source} must not panic")
      refute_match(/panicked|index out of bounds|unwrap/, result.stderr,
                   "#{source} panicked on an arity mismatch")
    end
  end

  # Macros expand before evaluation, so an arity mismatch there is
  # handled by different code than a function call and needs its own
  # sweep — including the `predicates` module, which is written in
  # Trongoscript rather than Rust.
  def test_macro_arity_mismatches_are_reported
    macros = %w[inc dec zero? pos? neg? even? odd? between? non-nil?
                coalesce default]
    macros.each do |name|
      [0, 1, 2, 4].each do |argc|
        args = (["1"] * argc).join(" ")
        source = "(use predicates)\n(#{name}#{args.empty? ? "" : " #{args}"})"
        result = run_source(source)
        assert_diagnosable(result, "(#{name}) with #{argc} args must not crash")
        refute_match(/panicked|index out of bounds/, result.stderr,
                     "(#{name}) with #{argc} args panicked during expansion")
      end
    end
  end

  # Special forms are matched by head symbol before any arity check, so a
  # truncated one reaches the form handler with fewer parts than it
  # expects.
  def test_malformed_special_forms_are_reported
    [
      '(let)', '(let [])', '(let [x])', '(let [1 2] 3)',
      '(if)', '(if 1)', '(if 1 2 3 4)',
      '(when)', '(do)', '(def)', '(def x)', '(defn)', '(defn f)',
      '(fn)', '(fn 1)', '(fn [1] 1)', '(fn [& a b] 1)',
      '(defmacro)', '(defmacro m)', '(defmacro m [1] 1)',
      '(macroexpand)', '(macroexpand 1 2)',
      '(quote)', '(quote 1 2)', '(quasiquote)',
      '(unquote 1)', '(unquote-splicing 1)',
      '(->)', '(->>)', '(-> 1 2)', '(->> 1 2)',
      '(use)', '(use 1)', '(use nonexistent-module)'
    ].each do |source|
      result = run_source(source)
      assert_diagnosable(result, "#{source} must not crash")
      refute_match(/panicked|index out of bounds|unwrap/, result.stderr,
                   "#{source} panicked instead of reporting a syntax error")
    end
  end

  # The sandboxed I/O builtins error without a sandbox, which means the
  # no-sandbox path is the one most scripts hit and the one least likely
  # to have been exercised. Wrong arity on top of that is two untested
  # branches at once.
  def test_sandbox_builtins_without_a_sandbox_are_reported
    %w[include read-file list-files glob].each do |name|
      [0, 1, 2].each do |argc|
        args = (['"x"'] * argc).join(" ")
        source = "(#{name}#{args.empty? ? "" : " #{args}"})"
        result = run_source(source)
        assert_diagnosable(result, "#{source} must not crash without a sandbox")
        refute_match(/panicked|unwrap/, result.stderr,
                     "#{source} panicked rather than reporting no sandbox")
      end
    end
  end

  # Names reserved for a host that embeds the interpreter always error.
  # They are still reachable from a script, so they still have to error
  # cleanly rather than reach an `unimplemented!()`.
  def test_reserved_host_forms_are_reported
    %w[run-activity run-subprocess sleep-until].each do |name|
      [0, 1, 3].each do |argc|
        args = (["1"] * argc).join(" ")
        result = run_source("(#{name}#{args.empty? ? "" : " #{args}"})")
        assert_diagnosable(result, "(#{name}) must not crash")
        refute_match(/panicked|unimplemented|not yet implemented/, result.stderr,
                     "(#{name}) panicked instead of reporting that it is only " \
                     "valid inside a workflow")
      end
    end
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
