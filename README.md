# Trongoscript

A YAMLScript-inspired DSL for producing nested data structures.
Lisp-flavored surface syntax, written entirely in Rust, statically compiled.

```clojure
(def env "production")

{:app  "trongoscript-demo"
 :env  env
 :port (if (= env "production") 443 8080)
 :auth {:enabled true
        :scopes  ["read" "write"]}
 :db   (include "db.trongo")}
```

## Why

YAMLScript proved the value of embedding a small, evaluable expression
language inside YAML / JSON / TOML configuration files. Trongoscript carries
the same idea into a pure-Rust stack:

- **One binary, no runtime** — statically compiled; no JVM, no native shared
  libs, no Clojure interop.
- **Same DSL in three modes** — evaluate a `.trongo` document, render an
  ERB-style template, or substitute `!trongo` snippets inside an existing
  JSON/YAML/TOML tree.
- **Secure by default** — no ambient filesystem, network, or process
  capabilities. Hosts opt in to a sandboxed directory; nothing else is
  reachable.
- **Extensible by host** — additional functions can be registered with the
  [`inventory`](https://crates.io/crates/inventory) crate's distributed
  slice, or per-instance via `EvalConfig::with_host_builtin`.

## Crates

| Crate                  | Purpose                                                |
| ---------------------- | ------------------------------------------------------ |
| `trongoscript-parser`   | `nom`-based parser → AST                               |
| `trongoscript-stdlib`   | Built-in functions, registered with `inventory`        |
| `trongoscript`          | Evaluator, sandbox, template renderer, embed walker    |
| `trongoscript-cli`      | `trongo` CLI (eval / render / embed)                    |

## CLI

```sh
# Evaluate a document
trongo eval -i config.trongo --format yaml

# Sandboxed I/O (include / read-file / list-files)
trongo eval -i main.trongo --root ./scripts

# ERB-like template
trongo render -i page.erb --root ./views

# Substitute !trongo snippets in a YAML/JSON/TOML doc
trongo embed -i config.yaml --format yaml --root .

# Language reference
trongo reference
```

## Language at a glance

```text
;; Atoms
42  3.14  -7  true  false  nil
"string"   :keyword   symbol

;; Compound
(head arg ...)        ; list — function call or special form
[a b c]               ; vector
{:k v :k2 v2}         ; map (alternating keys/values; commas are whitespace)

;; Special forms
(let [n v ...] body...)
(if cond then else?)
(when cond body...)
(do  expr...)
(def name value)
(and ...)  (or ...)
(quote x)             ; also 'x
(quasiquote x)        ; also `x
(unquote y)           ; also ~y, only inside quasiquote
(unquote-splicing ys) ; also ~@ys, only inside quasiquote

;; First-class functions
(fn [x y]  (+ x y))               ; anonymous closure
(fn [head & tail] tail)           ; rest args collected into a list
(defn add [x y] (+ x y))          ; sugar for (def add (fn [x y] ...))
((fn [x] (* x x)) 5)              ; immediately invoked → 25

;; Higher-order builtins — core
(map    (fn [x] (* x 2)) [1 2 3])           ; → [2 4 6]
(filter (fn [n] (> n 0)) [-1 0 1 2])         ; → [1 2]
(reduce (fn [a x] (+ a x)) 0 [1 2 3 4])      ; → 10
(reduce (fn [a x] (+ a x)) [1 2 3 4])        ; seed = first elem → 10

;; Higher-order builtins — CL-style family
;;   mapcar, mapc, maplist, mapl, mapcan, mapcon
;;   every, some, notany, notevery
;;   count-if / -if-not, find-if / -if-not
;;   position-if / -if-not, member-if / -if-not
;;   remove-if  / -if-not
;;   assoc-if   / -if-not, rassoc-if / -if-not
;;   subst-if   / -if-not   (whole-tree)
;;   substitute-if / -if-not (flat sequence)
;;   sort, stable-sort      (pred is 2-arg less-than)
;;   funcall, apply, identity, constantly, complement
;;   remove-duplicates, tree-equal
;;   union, intersection, set-difference, set-exclusive-or
;;
;; Destructive variants (delete-if, nsubst-if, ...), `multiple-value-call`,
;; and CL's sequence `merge` are intentionally omitted — Trongoscript
;; values are immutable JSON, there are no multiple values, and `merge`
;; is already taken by the map-merging builtin.

;; Macros — defmacro defines a code-rewriting form. Parameters that
;; appear bare in the body are substituted with the corresponding
;; *unevaluated* argument; combine with `` ` ``, `~`, `~@` for richer
;; templates.
(defmacro unless [c body]    (if c nil body))
(defmacro inc    [x]         `(+ ~x 1))
(defmacro sum-of [& xs]      `(+ ~@xs))
(macroexpand '(unless cond expr))  ; → (if cond nil expr)

;; Sandboxed I/O — only available when the host configures a sandbox root
(include    "path.trongo")
(read-file  "data.txt")
(list-files "subdir")
```

## Library use

```rust
use trongoscript::{eval_source, Context, EvalConfig, Sandbox, Value};

let sb = Sandbox::new("./scripts")?;
let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));

let v: Value = eval_source(
    r#"{:count (+ 1 2 3) :name (upcase "alice")}"#,
    &mut ctx,
)?;
```

Hosts can extend the language with their own functions:

```rust
use std::sync::Arc;
use trongoscript::{Context, EvalConfig, BuiltinResult, Value};

let host = Arc::new(|_ctx: &mut Context, args: &[Value]| -> BuiltinResult {
    Ok(Value::String(format!("hello {}", args.len())))
});
let cfg = EvalConfig::default().with_host_builtin("hello", host);
```

## Security

`trongoscript` has no ambient capabilities:

- Cannot open sockets, run subprocesses, or touch the filesystem unless the
  embedder passes a [`Sandbox`](crates/trongoscript/src/sandbox.rs).
- Sandbox paths are canonicalized; symlinks that escape the root are refused.
  Absolute paths are refused. `..` traversal outside the root is refused.
- Per-file size, include depth, and listing-entry limits prevent
  pathological inputs.
- Include cycles are detected and rejected.

See [`crates/trongoscript/src/sandbox.rs`](crates/trongoscript/src/sandbox.rs)
for the full security model.

## License

MIT OR Apache-2.0.
