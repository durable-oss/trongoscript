# frozen_string_literal: true

$LOAD_PATH.unshift File.expand_path("../lib", __dir__)
require "trongoscript"

require "minitest/autorun"
require "fileutils"
require "pathname"
require "timeout"
require "tmpdir"

module TrongoTestHelpers
  # assert_equal that also accepts nil, which Minitest wants spelled
  # assert_nil.
  def assert_value(expected, actual, msg = nil)
    expected.nil? ? assert_nil(actual, msg) : assert_equal(expected, actual, msg)
  end

  # An array nested `depth` levels deep around `leaf`. Built iteratively so
  # the helper itself has no depth limit.
  def nested(depth, leaf = 1)
    depth.times.reduce(leaf) { |acc, _| [acc] }
  end

  # Script that builds a list `depth` levels deep, equal to
  # `nested(depth - 1, [])`, using almost no eval depth.
  def deep_list_source(depth)
    "(reduce (fn [acc x] [acc]) [] (range #{depth - 1}))"
  end

  def with_tree(files)
    Dir.mktmpdir("trongo") do |dir|
      files.each do |path, contents|
        full = File.join(dir, path)
        FileUtils.mkdir_p(File.dirname(full))
        File.write(full, contents)
      end
      yield dir
    end
  end
end

Minitest::Test.include(TrongoTestHelpers)
