# frozen_string_literal: true

module Trongoscript
  # Base class for every error a script can cause. Invalid options raise
  # Ruby's own ArgumentError or TypeError instead, since those are bugs in
  # the calling code rather than in the script.
  class Error < StandardError; end

  # The source, a template, or a YAML document does not parse.
  class ParseError < Error; end

  # The script is wrong: an undefined symbol, a type error, a failing
  # builtin.
  class EvalError < Error; end

  # A sandboxed file operation was refused, or the script used `include`,
  # `read-file`, or `list-files` without a sandbox configured.
  class SandboxError < Error; end

  # The script exceeded a fuel limit: steps, recursion depth, value size,
  # total allocation, or the nesting depth of a value crossing into Ruby.
  # Retrying with larger limits may succeed.
  class ResourceLimitError < Error; end

  # A host function failed without an exception of its own to re-raise. A
  # host function that raises has that exception re-raised instead, so this
  # is rare.
  class CallbackError < Error; end

  # A context was used while it was already evaluating: from a second
  # thread, or from inside one of its own host functions.
  class ConcurrencyError < Error; end
end
