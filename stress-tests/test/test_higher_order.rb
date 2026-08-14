# frozen_string_literal: true

require_relative "../lib/stress_test_case"

# The higher-order layer in `crates/trongoscript/src/higher_order.rs`.
#
# These functions are dispatched separately from the stdlib inventory and
# are available with no `(use ...)` at all, which makes them reachable by
# every script the interpreter will ever evaluate — including the hostile
# ones. Two properties make the layer worth its own file.
#
# First, several of them are O(n^2) by construction: `union`,
# `intersection`, `set-difference`, `set-exclusive-or` and
# `remove-duplicates` all scan a list per element with deep `Value`
# equality. That is the shape of README bug #10, and it has exactly the
# same consequence — the whole scan happens inside one dispatch, so the
# step counter sees a single-digit number while the process spends
# minutes.
#
# Second, unlike the inventory builtins, this layer does not run its
# results through `charge_value`. Allocation is charged only indirectly,
# through the per-element eval steps of the closure being applied, so a
# function that builds a large list without calling back into the
# evaluator per output element is charged for far less than it allocates.
class TestHigherOrder < StressTestCase
  # Deep equality against every element kept so far. Measured on the
  # release build: 30k took 1.0s, 60k 4.0s, 120k 16.6s — a clean 4x per
  # doubling — and every one of those runs reported `steps=4`.
  #
  # No step budget can bound this. The scan needs a hash-based lookup for
  # scalars, the way `coll:uniq` got one, or a fuel charge sized to the
  # input.
  def test_remove_duplicates_over_distinct_elements_terminates
    assert_diagnosable(
      run_source(
        "(count (remove-duplicates (range 300000)))",
        limits: { max_steps: 10_000_000 }
      ),
      "(remove-duplicates) over 300k distinct elements did not come back. " \
      "Its scan is O(n^2) and costs 4 steps, so no step budget can stop it"
    )
  end

  # The set operations share that scan, so they share the failure. 100k
  # against 100k took 24.2s at `steps=6`.
  def test_set_operations_over_large_lists_terminate
    %w[union intersection set-difference set-exclusive-or].each do |fn|
      assert_diagnosable(
        run_source(
          "(count (#{fn} (range 200000) (range 200000)))",
          limits: { max_steps: 10_000_000 }
        ),
        "(#{fn}) over two 200k lists did not come back — the pairwise scan " \
        "is invisible to the step counter"
      )
    end
  end

  # Same asymptotics, but with composite elements the comparison is no
  # longer O(1): deep equality walks each element. n^2 comparisons of
  # size-k values is O(n^2 * k) from an input the caller sizes.
  def test_set_operations_with_composite_elements_terminate
    assert_diagnosable(
      run_source(
        '(count (union (map (fn [i] [i [i i] {:a i}]) (range 20000))
                       (map (fn [i] [i [i i] {:a i}]) (range 20000))))',
        limits: { max_steps: 10_000_000 }
      ),
      "pairwise deep equality over composite elements must terminate"
    )
  end

  # `maplist` applies its function to every suffix of the list, so the
  # suffixes alone are O(n^2) of allocation. This one *is* bounded — it
  # has one of the layer's three `charge_alloc` calls — and the test
  # pins that, because it is the model the rest of the file should follow.
  def test_maplist_suffix_allocation_is_charged
    assert_source_limited(
      "(count (maplist (fn [x] x) (range 20000)))",
      /alloc/i,
      # The step budget has to stay out of the way: the suffixes cost
      # O(n^2) steps as well, and this test is about the allocation.
      limits: { max_steps: 10_000_000_000 },
      msg: "maplist allocates n suffixes totalling O(n^2) elements; that " \
           "has to be charged against the allocation budget"
    )
  end

  # `mapcan` concatenates whatever each call returns, so output size is
  # chosen by the closure rather than by the input length. 20k calls each
  # returning 1000 elements is 20M elements from a two-line script.
  def test_mapcan_output_amplification_is_bounded
    assert_diagnosable(
      run_source(
        "(count (mapcan (fn [x] (range 10000)) (range 200000)))",
        limits: { max_steps: 10_000_000 }
      ),
      "mapcan's output size is the closure's choice, not the input's — " \
      "2e9 elements must be stopped by the allocation budget"
    )
  end

  # `mapcon` is `mapcan` over suffixes: the amplification of the former
  # on top of the quadratic allocation of the latter.
  def test_mapcon_terminates
    assert_diagnosable(
      run_source(
        "(count (mapcon (fn [x] x) (range 30000)))",
        limits: { max_steps: 10_000_000 }
      ),
      "mapcon over 30k elements must terminate"
    )
  end

  # `subst-if` walks a tree recursively. A value built at runtime is not
  # parsed, so the parser's nesting guard never saw it, and the walk is
  # native recursion that `--max-call-depth` cannot bound.
  def test_subst_if_on_deep_tree_does_not_overflow_the_stack
    assert_diagnosable(
      run_source(
        '(count (subst-if 9 (fn [x] (number? x))
                          (reduce (fn [acc x] [acc]) [1] (range 100000))))',
        limits: { max_steps: 10_000_000 }
      ),
      "subst-if's tree walk over a 100k-deep runtime value must not " \
      "overflow the native stack"
    )
  end

  # `tree-equal` recurses down two values in step, so a pair of deep
  # runtime-built values is the same native-stack exposure.
  def test_tree_equal_on_deep_values_does_not_overflow_the_stack
    assert_diagnosable(
      run_source(
        '(let [deep (reduce (fn [acc x] [acc]) [1] (range 100000))]
           (tree-equal deep deep))',
        limits: { max_steps: 10_000_000 }
      ),
      "tree-equal over two 100k-deep values must not overflow the stack"
    )
  end

  # The higher-order `sort` is an insertion sort with a closure call per
  # comparison — O(n^2) calls on its worst case. Unlike the set
  # operations above, each of those comparisons crosses the evaluator, so
  # the step counter sees them and the budget applies. That contrast is
  # the point: same asymptotics, bounded, because the work goes through
  # the meter.
  #
  # The input is reversed, which is insertion sort's worst case. Sorting
  # `(range n)` directly is its *best* case — already ordered, O(n)
  # comparisons — so a test written against it would pass no matter how
  # the algorithm behaved on real input.
  def test_higher_order_sort_comparisons_are_charged
    assert_source_limited(
      "(count (sort (fn [a b] (< a b)) (reverse (range 2000))))",
      /step/i,
      limits: { max_steps: 100_000 },
      msg: "insertion sort makes O(n^2) closure calls on reversed input " \
           "(measured: 1.5M steps at n=1000); each is a step, so the " \
           "budget must stop it"
    )
  end

  # A closure that recurses through `funcall`/`apply` reaches the
  # evaluator's call-depth guard by a different route than a direct
  # self-call, and `apply` splices its last argument, so it is also a way
  # to build a very wide argument list.
  def test_recursion_laundered_through_apply_is_bounded
    [
      '(def f (fn [n] (if (< n 1) 0 (funcall f (- n 1))))) (f 1000000)',
      '(def f (fn [n] (if (< n 1) 0 (apply f [(- n 1)])))) (f 1000000)'
    ].each do |source|
      assert_diagnosable(
        run_source(source, limits: { max_steps: 10_000_000 }),
        "recursion through funcall/apply must hit the call-depth guard"
      )
    end
  end

  # `apply` splices its final argument into the call, so a large list
  # becomes a large argument vector — a path to a huge frame that does
  # not go through a literal the parser could have bounded.
  def test_apply_with_a_huge_spliced_argument_list_is_bounded
    assert_diagnosable(
      run_source(
        "(apply + (range 5000000))",
        limits: { max_steps: 10_000_000 }
      ),
      "splicing 5M elements into one call must be bounded"
    )
  end

  # `constantly` and `complement` synthesize closures with a rest
  # parameter. Building them in a loop is closure allocation the budget
  # has to see.
  def test_synthesized_closures_are_bounded
    assert_diagnosable(
      run_source(
        '(count (map (fn [i] (funcall (constantly i))) (range 200000)))',
        limits: { max_steps: 1_000_000 }
      ),
      "200k synthesized closures must be charged"
    )
  end

  # Every predicate-taking function calls back into the evaluator once
  # per element, so all of them should be step-bounded. Running them
  # over one oversized input at a tight budget proves none has a path
  # that skips the meter.
  #
  # Half of these short-circuit — `every` stops at the first false,
  # `some` at the first truthy, `find-if` when it finds one. Each is
  # therefore given the predicate that makes it traverse the whole list,
  # because a test whose predicate lets the function exit at element
  # zero measures nothing.
  PREDICATE_FUNCTIONS = {
    # Never short-circuit: they must visit every element.
    "filter" => "false", "count-if" => "false", "count-if-not" => "false",
    "remove-if" => "false", "remove-if-not" => "false",
    # Stop at the first false / non-match, so the predicate must hold.
    "every" => "true", "notevery" => "true",
    "find-if-not" => "true", "position-if-not" => "true",
    "member-if-not" => "true",
    # Stop at the first truthy, so the predicate must never hold.
    "some" => "false", "notany" => "false",
    "find-if" => "false", "position-if" => "false", "member-if" => "false"
  }.freeze

  def test_predicate_functions_are_step_bounded
    PREDICATE_FUNCTIONS.each do |fn, pred|
      result = run_source(
        "(#{fn} (fn [x] #{pred}) (range 5000000))",
        limits: { max_steps: 50_000 }
      )
      assert_resource_limited(
        result,
        "(#{fn}) over 5M elements at a 50k step budget must stop on the " \
        "budget — one closure call per element has to be charged"
      )
    end
  end

  # The alist variants index into each element. An alist whose entries
  # are not the pairs they are assumed to be is the panic shape.
  def test_alist_functions_reject_malformed_entries
    %w[assoc-if assoc-if-not rassoc-if rassoc-if-not].each do |fn|
      ['[1 2 3]', '[[] [] []]', '[nil nil]', '[{:a 1}]', '["ab" "cd"]'].each do |alist|
        result = run_source("(#{fn} (fn [x] true) #{alist})")
        assert_diagnosable(result, "(#{fn} ... #{alist}) must not panic")
        refute_match(/panicked|unwrap|index out of bounds/, result.stderr,
                     "(#{fn} ... #{alist}) panicked on a malformed alist")
      end
    end
  end

  # `reduce` without an initial value on an empty list has nothing to
  # return, and the arity is overloaded — (f xs) and (f init xs) — so the
  # two-argument case has to distinguish them without indexing blindly.
  def test_reduce_arity_and_empty_input_are_reported
    ['(reduce (fn [a b] a) [])', '(reduce (fn [a b] a))', '(reduce)',
     '(reduce (fn [a b] a) 0 [])', '(reduce 5 [1 2])'].each do |source|
      result = run_source(source)
      assert_diagnosable(result, "#{source} must not panic")
      refute_match(/panicked|unwrap|index out of bounds/, result.stderr,
                   "#{source} panicked instead of reporting an error")
    end
  end

  # Every higher-order function given a non-function where a function
  # belongs, and a non-list where a list belongs. The claim is only that
  # the dispatch reports rather than unwrapping a closure handle that
  # isn't there.
  NON_FUNCTIONS = ['nil', '42', '"str"', '[1 2]', '{:a 1}', 'true'].freeze

  def test_higher_order_functions_reject_non_function_arguments
    %w[map filter every some count-if find-if remove-if mapcan
       sort stable-sort funcall apply subst-if].each do |fn|
      NON_FUNCTIONS.each do |arg|
        source = fn == "subst-if" ? "(#{fn} 1 #{arg} [1 2])" : "(#{fn} #{arg} [1 2])"
        result = run_source(source)
        assert_diagnosable(result, "#{source} must not panic")
        refute_match(/panicked|unwrap|index out of bounds/, result.stderr,
                     "#{source} panicked instead of reporting a type error")
      end
    end
  end

  def test_higher_order_functions_reject_non_list_collections
    %w[map filter every count-if find-if remove-if mapcan maplist
       remove-duplicates].each do |fn|
      NON_FUNCTIONS.each do |arg|
        source = fn == "remove-duplicates" ? "(#{fn} #{arg})" : "(#{fn} (fn [x] x) #{arg})"
        result = run_source(source)
        assert_diagnosable(result, "#{source} must not panic")
        refute_match(/panicked|unwrap|index out of bounds/, result.stderr,
                     "#{source} panicked on a non-list collection")
      end
    end
  end

  # A closure that returns a bigger value than it was given, applied
  # across a list, is amplification the per-element step charge cannot
  # see — the growth is in the value, not the call count.
  def test_map_returning_large_values_is_charged_for_allocation
    assert_diagnosable(
      run_source(
        '(use strings)
         (count (map (fn [i] (repeat "x" 100000)) (range 100000)))',
        limits: { max_steps: 1_000_000 }
      ),
      "100k calls each returning 100 KB is 10 GB; the allocation budget " \
      "has to catch what the step count cannot"
    )
  end

  # Nesting higher-order calls multiplies the element count without
  # nesting the source, so the parser's depth guard never applies.
  def test_nested_mapping_is_bounded
    assert_diagnosable(
      run_source(
        '(count (map (fn [i] (map (fn [j] (map (fn [k] [i j k]) (range 200)))
                                  (range 200)))
                     (range 200)))',
        limits: { max_steps: 10_000_000 }
      ),
      "three nested maps over 200 elements each is 8M closure calls"
    )
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
