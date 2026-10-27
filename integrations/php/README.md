# trongoscript (PHP)

A PHP extension for [Trongoscript](../../README.md): evaluate documents,
render ERB-style templates, and expand `!trongo` snippets in YAML or JSON,
from PHP 8.1+. Written in Rust with [ext-php-rs](https://github.com/davidcole1340/ext-php-rs).

```php
use function Trongoscript\{evaluate, render, embed_yaml};

evaluate('{:port (+ 8000 80) :env (upcase "prod")}');
// ['env' => 'PROD', 'port' => 8080]

render('Hello <%= (upcase name) %>!', ['globals' => ['name' => 'ada']]);
// 'Hello ADA!'

embed_yaml(file_get_contents('config.yml'), ['root' => 'config']);
```

Functions: `evaluate`, `evaluate_file`, `render`, `embed`, `embed_yaml`, and
`dump` (a value as Trongoscript source), all in the `Trongoscript`
namespace.

## Contexts

The functions build a throwaway context each time. A `Trongoscript\Context`
keeps definitions between evaluations:

```php
$ctx = new Trongoscript\Context(['root' => 'scripts', 'limits' => 'strict']);
$ctx->evalFile('lib/helpers.trongo');
$ctx->eval('(render-service :api)');
$ctx->set('region', 'eu-west-1');   // globals, readable as plain symbols
$ctx->call('render-service', 'db'); // call a defn with data arguments
$ctx->usage();                      // ['steps' => 1834, 'allocated' => 9120, ...]
```

| Option | Meaning |
| --- | --- |
| `root` | Host directory scripts see as `/`. Enables `include`, `read-file`, `list-files`. |
| `mounts` | `['/vendor' => 'path/to/vendor']`. More directories in the sandbox. Needs `root`. |
| `files` | `['main.trongo' => '...']`. An in-memory sandbox instead of `root`. |
| `seed` | Seed for the `rng` module. Defaults to 0, so runs are reproducible. |
| `limits` | `'default'`, `'strict'` (untrusted input), `'unlimited'` (trusted only), or an array such as `['preset' => 'strict', 'max_steps' => 10000]`. |
| `sandbox_limits` | `max_file_bytes`, `max_include_depth`, `max_list_entries`, `max_glob_depth`. |
| `globals` | Initial bindings. |
| `functions` | Host functions, as `['name' => callable]`. |
| `objects` | Return maps as `stdClass` objects instead of arrays. |

`embed()` and `embed_yaml()` also take the markers `prefix`, `key`, and
`tag`; `Context::embed()` takes them as its second argument. Unknown
options raise `ValueError`.

## Host functions

Any callable can be exposed to scripts:

```php
$ctx->define('env', fn (string $name) => getenv($name))
    ->define('upper', 'strtoupper')
    ->define('now', time(...));
$ctx->eval('{:home (env "HOME") :at (now)}');
```

If the callable throws, the evaluation stops and the same exception object
propagates from `eval()`. Fatal errors inside a callable (memory limit,
`max_execution_time`, `E_USER_ERROR`) and `exit()` behave as they would in
plain PHP: the evaluation is wound down first, then the engine carries on
with its shutdown.

A host function cannot use the context that is running it (that throws
`ConcurrencyException`), but it can evaluate in another context.

## Values

| PHP | Trongoscript | Back to PHP |
| --- | --- | --- |
| `null`, `bool` | nil, booleans | same |
| `int` | integer | `int` (past `PHP_INT_MAX`: `float`) |
| `float` (finite) | float | `float` |
| `string` (UTF-8) | string | `string` |
| array with keys 0, 1, 2, ... | list | list |
| any other array, `stdClass` | map, keys as strings | array keyed as PHP would key it, or `stdClass` with `objects` |
| `JsonSerializable` | its `jsonSerialize()` | |

As with `json_encode`, `[]` is a list; use `new stdClass()` for an empty
map, and `'objects' => true` to keep `{}` and `[]` apart on the way out.
Map keys like `"1"` come back as integer keys, exactly as PHP stores
`$a["1"]`. Other objects and resources throw `TypeError`; non-UTF-8 strings,
`NAN`, and `INF` throw `ValueError`. Values may nest at most 1,024 levels in
either direction, which also catches recursive arrays and objects.

## Errors

Everything a script can cause is a `Trongoscript\Exception`:

| Class | Thrown when |
| --- | --- |
| `ParseException` | Source, a template, or YAML does not parse. |
| `EvalException` | The script is wrong: undefined symbol, type error, failing builtin. |
| `SandboxException` | A file operation is refused, or there is no sandbox. |
| `ResourceLimitException` | A fuel limit or the nesting limit was hit. |
| `CallbackException` | A host function failed without throwing. |
| `ConcurrencyException` | A context was used from its own host function. |

Bad arguments throw PHP's own `TypeError` or `ValueError`.

Evaluation runs on a native thread with a 256 MiB stack, so deep recursion
does not depend on PHP's stack. Host functions still run on PHP's thread.

## Development

Uses [devenv](https://devenv.sh) for PHP, Composer, Rust, and libclang:

```sh
cd integrations/php
devenv shell
composer install
composer test             # builds the extension, then runs PHPUnit
FUZZ_SEED=123 composer test
memcheck                  # the suite under valgrind (slow)
```

To use the extension elsewhere, `cargo build --release` and load
`target/release/libtrongoscript.so` with `extension=` in `php.ini`.

## License

MIT OR Apache-2.0.
