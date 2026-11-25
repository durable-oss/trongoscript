# trongoscript (Ruby)

Ruby bindings for [Trongoscript](../../README.md): evaluate documents,
render ERB-style templates, and expand `!trongo` snippets in YAML or JSON,
from Ruby. The interpreter is the Rust one, loaded as a native extension.

```ruby
require "trongoscript"

Trongoscript.eval('{:port (+ 8000 80) :env (upcase "prod")}')
#=> {"env" => "PROD", "port" => 8080}

Trongoscript.render("Hello <%= (upcase name) %>!", globals: { name: "ada" })
#=> "Hello ADA!"

Trongoscript.embed_yaml(File.read("config.yml"), root: "config")
```

## Contexts

The module functions build a throwaway context each time. A
`Trongoscript::Context` keeps definitions between evaluations, so a library
can be loaded once:

```ruby
ctx = Trongoscript::Context.new(root: "scripts", limits: :strict)
ctx.eval_file("lib/helpers.trongo")
ctx.eval("(render-service :api)")
ctx["region"] = "eu-west-1"     # globals, readable as plain symbols
ctx.call("render-service", "db") # call a defn with data arguments
ctx.usage                        #=> {steps: 1834, allocated: 9120, ...}
```

| Option | Meaning |
| --- | --- |
| `root:` | Host directory scripts see as `/`. Enables `include`, `read-file`, `list-files`. |
| `mounts:` | `{ "/vendor" => "path/to/vendor" }`. More directories in the sandbox. Needs `root:`. |
| `files:` | `{ "main.trongo" => "..." }`. An in-memory sandbox instead of `root:`. |
| `seed:` | Seed for the `rng` module. Defaults to 0, so runs are reproducible. |
| `limits:` | `:default`, `:strict` (untrusted input), `:unlimited` (trusted only), or a hash such as `{ preset: :strict, max_steps: 10_000 }`. |
| `sandbox_limits:` | `max_file_bytes`, `max_include_depth`, `max_list_entries`, `max_glob_depth`. |
| `globals:` | Initial bindings. |
| `functions:` | Host functions, as `{ "name" => callable }`. |
| `symbolize_names:` | Return map keys as Symbols. |

Without `root:` or `files:` a script has no filesystem access at all.

## Host functions

Any callable can be exposed to scripts:

```ruby
ctx.define("env") { |name| ENV.fetch(name) }
ctx.define("now", -> { Time.now.to_i })
ctx.eval('{:home (env "HOME") :at (now)}')
```

Arguments arrive as Ruby values and the return value is converted back. If
the callable raises, the evaluation stops and the same exception object is
re-raised from `eval`, so `rescue KeyError` works as you would expect.
`throw` and `Thread#kill` pass through too.

A host function cannot use the context that is running it (that raises
`ConcurrencyError`), but it can evaluate in another context.

## Values

| Ruby | Trongoscript | Back to Ruby |
| --- | --- | --- |
| `nil`, `true`, `false` | nil, booleans | same |
| `Integer` (64-bit, signed or unsigned) | integer | `Integer` |
| `Float` (finite) | float | `Float` |
| `String` | string, transcoded to UTF-8 | `String` (UTF-8) |
| `Symbol` | string | `String` |
| `Array` | list | `Array` |
| `Hash` with String or Symbol keys | map | `Hash` with String keys, sorted |

Anything else raises `TypeError`; integers past 64 bits raise `RangeError`,
`NaN` and infinities raise `FloatDomainError`, and binary strings that are
not text raise `EncodingError`. Values may nest at most 1,024 levels in
either direction, which also catches self-referential structures. A script
may build deeper values internally; it just cannot hand them over.

Function values (from `(fn ...)`) come back as small hashes that only mean
something to the context that made them. Pass them to `Context#call`.

## Errors

Everything a script can cause is a `Trongoscript::Error`:

| Class | Raised when |
| --- | --- |
| `ParseError` | Source, a template, or YAML does not parse. |
| `EvalError` | The script is wrong: undefined symbol, type error, failing builtin. |
| `SandboxError` | A file operation is refused, or there is no sandbox. |
| `ResourceLimitError` | A fuel limit or the nesting limit was hit. Larger limits may help. |
| `CallbackError` | A host function failed without an exception of its own to re-raise. Rare. |
| `ConcurrencyError` | A context was used from two threads, or from its own host function. |

Bad options raise Ruby's own `ArgumentError` or `TypeError`.

## Threads and interrupts

Evaluation runs on a native thread with a 256 MiB stack, so deep recursion
works the same from any Ruby thread, including ones with Ruby's small
default stack. The GVL is released while a script runs and reacquired to run
host functions, so separate contexts evaluate in parallel.

`Timeout.timeout`, `Thread#raise`, `Thread#kill`, and Ctrl-C are delivered
once the evaluator stops. A script that calls host functions stops at its
next call; one that does not runs until it finishes, which the step budget
bounds. Use `limits:` rather than `Timeout` to cap untrusted scripts.

## Development

Uses [devenv](https://devenv.sh) for Ruby, Rust, and libclang:

```sh
cd integrations/ruby
devenv shell
bundle install
bundle exec rake          # compile, then run the tests
FUZZ_SEED=123 bundle exec rake test
```

The shared Rust layer the extension is built on lives in
[`integrations/host`](../host) and has its own `cargo test` suite.

### Packaging

The extension depends on the `trongoscript-host` crate from crates.io, so
an installed gem builds without this repository. Inside the checkout,
`.cargo/config.toml` patches that dependency to `../host`, so local builds
pick up changes to the host crate and the core crates it depends on.

A release of the gem needs a release of `trongoscript-host` that matches the
version in `ext/trongoscript/Cargo.toml` to be on crates.io first.

## License

MIT OR Apache-2.0.
