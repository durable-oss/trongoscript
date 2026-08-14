# frozen_string_literal: true

require "minitest/autorun"
require_relative "trongo_harness"

# Base class for resource-metering stress tests.
#
# The assertions here encode what "the fuel system works" actually means,
# so individual tests read as claims about behavior rather than as pokes at
# exit codes.
class StressTestCase < Minitest::Test
  include TrongoHarness

  make_my_diffs_pretty! if respond_to?(:make_my_diffs_pretty!)

  # Verify the binary exists once, with a useful message, rather than
  # letting every test fail with the same spawn error.
  def setup
    TrongoHarness.binary
  rescue TrongoHarness::BinaryMissing => e
    skip e.message
  end

  # The central claim: hostile input never crashes the process and never
  # hangs. It always comes back with something a caller can act on.
  #
  # This is stricter than "an error happened" on purpose. A SIGSEGV from a
  # blown stack and a SIGKILL from the OOM killer both mean the interpreter
  # lost control of its own resource usage.
  def assert_diagnosable(result, msg = nil)
    context = "#{msg}\ncmd: #{result.cmd.join(" ")}\n#{result.describe}"
    refute result.timed_out,
           "expected the interpreter to terminate on its own, but it hung.\n#{context}"
    refute result.crashed?,
           "expected a clean exit, but the process was killed by " \
           "#{result.signal_name} — a resource guard is missing or too loose.\n#{context}"
    result
  end

  # The script hit a resource ceiling and said so.
  def assert_resource_limited(result, msg = nil)
    assert_diagnosable(result, msg)
    assert_equal TrongoHarness::EXIT_RESOURCE_LIMIT, result.exitstatus,
                 "#{msg}\nexpected exit #{TrongoHarness::EXIT_RESOURCE_LIMIT} " \
                 "(resource limit), got #{result.describe}\ncmd: #{result.cmd.join(" ")}"
    result
  end

  # The mentioned limit is the one that tripped.
  def assert_limit_message(result, pattern, msg = nil)
    assert_resource_limited(result, msg)
    assert_match pattern, result.stderr,
                 "#{msg}\nwrong limit reported.\nstderr: #{result.stderr}"
    result
  end

  # The script ran to completion. Used to prove the default ceilings do not
  # get in a legitimate script's way.
  def assert_completes(result, msg = nil)
    assert_diagnosable(result, msg)
    assert_equal TrongoHarness::EXIT_OK, result.exitstatus,
                 "#{msg}\nexpected success, got #{result.describe}\n" \
                 "cmd: #{result.cmd.join(" ")}"
    result
  end

  # Convenience: run and assert in one step.
  def assert_source_limited(source, pattern, limits: {}, msg: nil, **kw)
    result = run_source(source, limits: limits, **kw)
    assert_limit_message(result, pattern, msg)
  end

  # Build a deeply nested expression without recursing in Ruby.
  def nested(open, close, depth, core = "1")
    (open * depth) + core + (close * depth)
  end

  # A literal list of `n` integers, for tests that need a big input value
  # without relying on a particular builtin existing.
  def literal_list(n)
    "[#{(0...n).to_a.join(" ")}]"
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
