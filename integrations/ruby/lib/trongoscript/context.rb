# frozen_string_literal: true

module Trongoscript
  # An evaluation context. Definitions made by one evaluation (`def`,
  # `defn`, `defmacro`, `use`) stay visible to the next, so a context can
  # load a library once and evaluate many documents against it.
  #
  # Each evaluation gets a fresh fuel budget.
  #
  # A context is not thread-safe. Using one from two threads at once, or
  # from inside one of its own host functions, raises ConcurrencyError.
  # Separate contexts evaluate in parallel: the GVL is released while a
  # script runs.
  #
  #   ctx = Trongoscript::Context.new(files: { "lib.trongo" => "(defn twice [x] (* 2 x))" })
  #   ctx.eval('(include "lib.trongo")')
  #   ctx.eval("(twice 21)") #=> 42
  class Context
    # @param root [String, Pathname, nil] host directory exposed to scripts
    #   as the virtual root `/`. Enables `include`, `read-file`, and
    #   `list-files`. Mutually exclusive with `files:`.
    # @param mounts [Hash{String => String, Pathname}] more host directories,
    #   keyed by the absolute virtual path to graft them at. Needs `root:`.
    # @param files [Hash{String => String}, nil] an in-memory file tree,
    #   keyed by virtual path. Mutually exclusive with `root:`.
    # @param seed [Integer, nil] seed for the `rng` module. Defaults to 0,
    #   so runs are reproducible unless you pass something else.
    # @param limits [Symbol, Hash] `:default`, `:strict` (for untrusted
    #   input), or `:unlimited` (trusted input only), or a Hash of
    #   overrides with an optional `:preset` key, e.g.
    #   `{ preset: :strict, max_steps: 10_000 }`. Keys: `max_steps`,
    #   `max_call_depth`, `max_eval_depth`, `max_value_len`,
    #   `max_total_alloc`.
    # @param sandbox_limits [Hash] overrides for `max_file_bytes`,
    #   `max_include_depth`, `max_list_entries`, `max_glob_depth`.
    # @param globals [Hash] initial bindings, visible to scripts as symbols.
    # @param functions [Hash{String => #call}] host functions; see #define.
    # @param symbolize_names [Boolean] return map keys as Symbols.
    def initialize(root: nil, mounts: {}, files: nil, seed: nil, limits: :default,
                   sandbox_limits: {}, globals: {}, functions: {}, symbolize_names: false)
      raise ArgumentError, "pass root: or files:, not both" if root && files
      raise ArgumentError, "mounts: needs a root:" if root.nil? && !mounts.empty?

      preset, overrides = split_limits(limits)
      @symbolize_names = symbolize_names ? true : false
      @functions = {}
      @dispatch = method(:dispatch)
      @native = Native.new(
        {
          root: root && File.path(root),
          mounts: pairs(mounts, "mounts") { |v| File.path(v) },
          files: files && pairs(files, "files"),
          seed: seed,
          preset: preset,
          limits: overrides,
          sandbox_limits: pairs(sandbox_limits, "sandbox_limits"),
          globals: pairs(globals, "globals"),
          symbolize_names: @symbolize_names
        }
      )
      pairs(functions, "functions").each { |name, fn| define(name, fn) }
    end

    # Evaluate a document and return the value of its last form.
    def eval(source)
      @native.eval(source, @dispatch)
    end

    # Evaluate a file from the sandbox, as `(include path)` would.
    # Relative includes inside it resolve against its directory.
    def eval_file(path)
      @native.eval_file(path.to_s, @dispatch)
    end

    # Render an ERB-style template: `<%= expr %>` inserts a value,
    # `<% form %>` evaluates for effect.
    def render(template)
      @native.render(template, @dispatch)
    end

    # Replace Trongoscript snippets embedded in a parsed document (Hash,
    # Array, or scalar) with their values: strings starting with `prefix`
    # (default `"!trongo "`) and single-key hashes keyed by `key` (default
    # `"!trongo"`).
    def embed(document, prefix: nil, key: nil)
      @native.embed(document, { prefix: prefix, key: key }, @dispatch)
    end

    # Like #embed, for YAML text, which additionally recognizes nodes
    # tagged with `tag` (default `!trongo`).
    def embed_yaml(yaml, tag: nil, prefix: nil, key: nil)
      @native.embed_yaml(yaml, { tag: tag, prefix: prefix, key: key }, @dispatch)
    end

    # Call a function. `function` is a name (a `defn`, builtin, macro, or
    # host function) or a function value a script returned. Arguments are
    # passed as data and never evaluated as code.
    def call(function, *args)
      case function
      when String, Symbol then @native.call(function.to_s, args, @dispatch)
      else @native.call_value(function, args, @dispatch)
      end
    end

    # Expose a Ruby callable to scripts as the function `name`. Arguments
    # arrive converted to Ruby; the return value is converted back. An
    # exception raised by the callable aborts the evaluation and is
    # re-raised from the evaluating method unchanged.
    #
    # Host functions shadow builtins of the same name.
    #
    #   ctx.define("env") { |name| ENV.fetch(name) }
    #   ctx.define("now", -> { Time.now.to_i })
    def define(name, callable = nil, &block)
      raise ArgumentError, "pass a callable or a block, not both" if callable && block

      fn = callable || block
      raise ArgumentError, "define needs a callable or a block" unless fn
      raise TypeError, "#{fn.class} does not respond to #call" unless fn.respond_to?(:call)

      name = name.to_s
      @native.define(name)
      @functions[name] = fn
      self
    end

    # Remove a host function. Returns whether it was defined.
    def undefine(name)
      @functions.delete(name.to_s)
      @native.undefine(name.to_s)
    end

    # Names of the host functions, sorted.
    def functions
      @native.functions
    end

    # A global's value, or nil.
    def [](name)
      @native.get(name.to_s)
    end

    def []=(name, value)
      @native.set(name.to_s, value)
    end

    def key?(name)
      @native.key?(name.to_s)
    end

    # Remove a global. Returns whether it existed.
    def delete(name)
      @native.unset(name.to_s)
    end

    # Every global, as a Hash sorted by name. Keys are Symbols with
    # `symbolize_names: true`.
    def globals
      all = @native.globals
      @symbolize_names ? all.transform_keys(&:to_sym) : all
    end

    # What the last evaluation consumed: `steps`, `allocated` (bytes),
    # `peak_call_depth`, and `peak_eval_depth`.
    def usage
      @native.usage
    end

    def inspect
      "#<#{self.class.name} functions=#{functions.inspect}>"
    end

    private

    def dispatch(name, args)
      @functions.fetch(name).call(*args)
    end

    def split_limits(limits)
      case limits
      when nil then [nil, []]
      when Symbol, String then [limits.to_s, []]
      when Hash
        rest = limits.transform_keys(&:to_sym)
        preset = rest.delete(:preset)
        [preset&.to_s, rest.map { |k, v| [k.to_s, v] }]
      else
        raise TypeError, "limits: expected a preset name or a Hash, got #{limits.class}"
      end
    end

    def pairs(hash, option)
      raise TypeError, "#{option}: expected a Hash, got #{hash.class}" unless hash.is_a?(Hash)

      hash.map { |k, v| [k.to_s, block_given? ? yield(v) : v] }
    end
  end
end
