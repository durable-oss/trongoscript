# frozen_string_literal: true

require_relative "trongoscript/version"
require_relative "trongoscript/errors"

begin
  # Precompiled gems ship one binary per Ruby minor version.
  ruby_version = RUBY_VERSION[/\d+\.\d+/]
  require_relative "trongoscript/#{ruby_version}/trongoscript"
rescue LoadError
  require_relative "trongoscript/trongoscript"
end

require_relative "trongoscript/context"

# Ruby bindings for Trongoscript.
#
# The module functions are one-shot conveniences: each builds a fresh
# Context from the options it is given (see Context#initialize) and
# discards it afterwards. Use a Context directly to keep definitions
# between evaluations or to expose Ruby functions to scripts.
#
#   Trongoscript.eval('{:port (+ 8000 80)}')         #=> {"port" => 8080}
#   Trongoscript.render("Hi <%= (upcase name) %>", globals: { name: "ada" })
#   Trongoscript.embed_yaml("port: !trongo (+ 8000 80)")
module Trongoscript
  class << self
    def eval(source, **options)
      Context.new(**options).eval(source)
    end

    def eval_file(path, **options)
      Context.new(**options).eval_file(path)
    end

    def render(template, **options)
      Context.new(**options).render(template)
    end

    def embed(document, prefix: nil, key: nil, **options)
      Context.new(**options).embed(document, prefix: prefix, key: key)
    end

    def embed_yaml(yaml, tag: nil, prefix: nil, key: nil, **options)
      Context.new(**options).embed_yaml(yaml, tag: tag, prefix: prefix, key: key)
    end

    # Format a Ruby value as Trongoscript source that evaluates back to it.
    def dump(value, pretty: false)
      Native.dump(value, pretty ? true : false)
    end
  end
end
