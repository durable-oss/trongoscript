//! Trongoscript CLI.
//!
//! Subcommands:
//!   trongo eval [-i FILE] [--root DIR] [--format json|yaml|toml]
//!     Evaluate a Trongoscript document and print the value.
//!
//!   trongo render [-i FILE] [--root DIR]
//!     Render an ERB-like template (raw text + Trongoscript blocks).
//!
//!   trongo embed --in FILE [--root DIR] [--format json|yaml|toml]
//!     Walk a JSON/YAML/TOML file and substitute embedded Trongoscript
//!     snippets in place.
//!
//! `--root DIR` activates the filesystem sandbox so scripts can use
//! `include`, `read-file`, and `list-files`. Without it, those builtins
//! fail with "filesystem access not configured".
//!
//! `--mount VIRTUAL=HOST` (repeatable) grafts additional host directories
//! into the sandbox's virtual filesystem beside the root, so
//! `--root ./bob --mount /tom=./tom` exposes `./bob/x` as `/x` and
//! `./tom/y` as `/tom/y`. Access stays read-only.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Context as _, Result};
use clap::{Parser, Subcommand, ValueEnum};
use trongoscript::{
    eval_source, evaluate_embedded, evaluate_embedded_yaml, render_template, Context, EvalConfig,
    Sandbox, Value,
};

#[derive(Parser, Debug)]
#[command(name = "trongo", version, about = "Trongoscript CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Evaluate a Trongoscript document.
    Eval(EvalArgs),
    /// Render an ERB-like template (raw text with embedded Trongoscript blocks).
    Render(RenderArgs),
    /// Walk a JSON/YAML/TOML document and substitute embedded Trongoscript.
    Embed(EmbedArgs),
    /// Print the source language reference.
    Reference,
}

#[derive(Parser, Debug)]
struct EvalArgs {
    /// Source file. Use `-` for stdin (the default if omitted).
    #[arg(short = 'i', long = "in")]
    input: Option<PathBuf>,
    /// Filesystem sandbox root (enables include / read-file / list-files).
    #[arg(long)]
    root: Option<PathBuf>,
    /// Graft an extra directory into the sandbox as VIRTUAL=HOST, e.g.
    /// `--mount /tom=./tom` makes ./tom/y readable as /tom/y. Repeatable.
    #[arg(long = "mount", value_name = "VIRTUAL=HOST")]
    mounts: Vec<String>,
    /// Initial seed for the `rng` module. Defaults to the current
    /// timestamp (millis since epoch) — pass an explicit value for
    /// reproducible runs.
    #[arg(long)]
    seed: Option<u64>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Json)]
    format: Format,
}

#[derive(Parser, Debug)]
struct RenderArgs {
    #[arg(short = 'i', long = "in")]
    input: Option<PathBuf>,
    #[arg(long)]
    root: Option<PathBuf>,
    /// Graft an extra directory into the sandbox as VIRTUAL=HOST. Repeatable.
    #[arg(long = "mount", value_name = "VIRTUAL=HOST")]
    mounts: Vec<String>,
    #[arg(long)]
    seed: Option<u64>,
}

#[derive(Parser, Debug)]
struct EmbedArgs {
    /// Input document.
    #[arg(short = 'i', long = "in")]
    input: PathBuf,
    /// Document format (defaults to file extension).
    #[arg(long, value_enum)]
    format: Option<Format>,
    #[arg(long)]
    root: Option<PathBuf>,
    /// Graft an extra directory into the sandbox as VIRTUAL=HOST. Repeatable.
    #[arg(long = "mount", value_name = "VIRTUAL=HOST")]
    mounts: Vec<String>,
    #[arg(long)]
    seed: Option<u64>,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Format {
    Json,
    Yaml,
    Toml,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Eval(a) => cmd_eval(a),
        Command::Render(a) => cmd_render(a),
        Command::Embed(a) => cmd_embed(a),
        Command::Reference => {
            print_help();
            Ok(())
        }
    }
}

fn cmd_eval(a: EvalArgs) -> Result<()> {
    let source = read_source(a.input.as_deref())?;
    let mut ctx = make_context(a.root.as_deref(), &a.mounts, a.seed)?;
    let value = eval_source(&source, &mut ctx).map_err(|e| anyhow!("{}", e))?;
    write_value(&value, a.format)
}

fn cmd_render(a: RenderArgs) -> Result<()> {
    let source = read_source(a.input.as_deref())?;
    let mut ctx = make_context(a.root.as_deref(), &a.mounts, a.seed)?;
    let out = render_template(&source, &mut ctx).map_err(|e| anyhow!("{}", e))?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(out.as_bytes())?;
    Ok(())
}

fn cmd_embed(a: EmbedArgs) -> Result<()> {
    let format = a
        .format
        .or_else(|| guess_format_from_path(&a.input))
        .ok_or_else(|| {
            anyhow!("could not infer format from extension; pass --format json|yaml|toml")
        })?;
    let raw = std::fs::read_to_string(&a.input)
        .with_context(|| format!("reading {}", a.input.display()))?;
    let mut ctx = make_context(a.root.as_deref(), &a.mounts, a.seed)?;
    let out = match format {
        Format::Yaml => {
            // Use the YAML-aware path so `!trongo` tags are honored as real
            // YAML tags (rather than being lost in the json conversion).
            let y: serde_yaml::Value = serde_yaml::from_str(&raw).context("parsing YAML")?;
            evaluate_embedded_yaml(y, &mut ctx).map_err(|e| anyhow!("{}", e))?
        }
        _ => {
            let value = parse_document(&raw, format)?;
            evaluate_embedded(value, &mut ctx).map_err(|e| anyhow!("{}", e))?
        }
    };
    write_value(&out, format)
}

/// Split a `--mount VIRTUAL=HOST` argument. Splits on the *first* `=` so
/// host paths containing `=` survive.
fn parse_mount(spec: &str) -> Result<(&str, &str)> {
    let (virtual_path, host) = spec
        .split_once('=')
        .ok_or_else(|| anyhow!("--mount expects VIRTUAL=HOST, got {:?}", spec))?;
    if virtual_path.is_empty() || host.is_empty() {
        return Err(anyhow!("--mount expects VIRTUAL=HOST, got {:?}", spec));
    }
    if !virtual_path.starts_with('/') {
        return Err(anyhow!(
            "--mount point must be an absolute virtual path starting with '/', got {:?}",
            virtual_path
        ));
    }
    Ok((virtual_path, host))
}

fn make_context(
    root: Option<&std::path::Path>,
    mounts: &[String],
    seed: Option<u64>,
) -> Result<Context> {
    let mut cfg = EvalConfig::default();
    if let Some(r) = root {
        let mut sb = Sandbox::new(r)
            .with_context(|| format!("opening sandbox root {}", r.display()))?;
        for spec in mounts {
            let (virtual_path, host) = parse_mount(spec)?;
            sb = sb
                .mount(virtual_path, host)
                .with_context(|| format!("mounting {} at {}", host, virtual_path))?;
        }
        cfg = cfg.with_sandbox(sb);
    } else if !mounts.is_empty() {
        return Err(anyhow!("--mount requires --root"));
    }
    let resolved_seed = seed.unwrap_or_else(default_seed);
    cfg = cfg.with_rng_seed(resolved_seed);
    Ok(Context::new(cfg))
}

/// CLI default seed: millis since epoch. Reads the clock once at startup
/// and falls back to 0 if for some reason the clock is unavailable
/// (genuinely shouldn't happen on any supported platform).
fn default_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_source(input: Option<&std::path::Path>) -> Result<String> {
    match input {
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(buf)
        }
        Some(p) if p.as_os_str() == "-" => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(buf)
        }
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading {}", p.display())),
    }
}

fn parse_document(raw: &str, format: Format) -> Result<Value> {
    match format {
        Format::Json => serde_json::from_str(raw).context("parsing JSON"),
        Format::Yaml => serde_yaml::from_str(raw).context("parsing YAML"),
        Format::Toml => {
            let t: toml::Value = toml::from_str(raw).context("parsing TOML")?;
            // Convert toml::Value → serde_json::Value via serde.
            let j = serde_json::to_value(t).context("converting TOML to JSON")?;
            Ok(j)
        }
    }
}

fn write_value(v: &Value, format: Format) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    match format {
        Format::Json => {
            serde_json::to_writer_pretty(&mut stdout, v)?;
            stdout.write_all(b"\n")?;
        }
        Format::Yaml => {
            serde_yaml::to_writer(&mut stdout, v)?;
        }
        Format::Toml => {
            // TOML's root must be a table; flatten if needed.
            let table: toml::Value = serde_json::from_value(v.clone())
                .and_then(|s: serde_json::Value| serde_json::from_value(s))
                .ok()
                .and_then(|s: serde_json::Value| toml_from_json(s))
                .ok_or_else(|| anyhow!("TOML root must be a table"))?;
            let s = toml::to_string_pretty(&table).context("serializing TOML")?;
            stdout.write_all(s.as_bytes())?;
        }
    }
    Ok(())
}

fn toml_from_json(v: serde_json::Value) -> Option<toml::Value> {
    match v {
        serde_json::Value::Null => Some(toml::Value::String(String::new())),
        serde_json::Value::Bool(b) => Some(toml::Value::Boolean(b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml::Value::Integer(i))
            } else {
                n.as_f64().map(toml::Value::Float)
            }
        }
        serde_json::Value::String(s) => Some(toml::Value::String(s)),
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(toml_from_json(it)?);
            }
            Some(toml::Value::Array(out))
        }
        serde_json::Value::Object(map) => {
            let mut t = toml::map::Map::new();
            for (k, v) in map {
                t.insert(k, toml_from_json(v)?);
            }
            Some(toml::Value::Table(t))
        }
    }
}

fn guess_format_from_path(p: &std::path::Path) -> Option<Format> {
    match p.extension().and_then(|s| s.to_str()) {
        Some("json") => Some(Format::Json),
        Some("yaml") | Some("yml") => Some(Format::Yaml),
        Some("toml") => Some(Format::Toml),
        _ => None,
    }
}

/// The `--help` text. Kept as a raw string rather than a `print!` argument so
/// the `{...}` map and spintax syntax below need no brace doubling.
const HELP: &str = r#"Trongoscript — a Lisp-flavored DSL for nested data.

Atoms:
  42  3.14  -7   true  false  nil
  "string"       :keyword       symbol-name

Compound:
  (head arg ...)          ; list (call or special form)
  [a b c]                 ; vector
  {:k v :k2 v2}           ; map (alternating keys/values)

Special forms:
  (let [n v ...] body...)
  (if cond then else?)
  (when cond body...)
  (do expr...)
  (def name value)
  (defmacro name [params] body...)        ; & rest params supported
  (macroexpand 'form)
  (and ...)  (or ...)
  (quote x)               also 'x
  (quasiquote x)          also `x
  (unquote x)             also ~x    (only inside quasiquote)
  (unquote-splicing xs)   also ~@xs  (only inside quasiquote)
  (use module-name ...)   ; enable optional stdlib modules

I/O (requires --root sandbox):
  (include "path.trongo")
  (read-file "data.txt")
  (list-files "subdir")
  (glob "**/*.yaml")      ; ** = recurse; *, ?, [abc] supported

  Paths are virtual, not host paths. --root is mounted at /, and each
  --mount VIRTUAL=HOST grafts another directory in beside it:

    trongo eval --root ./bob --mount /tom=./tom -i main.trongo
      (read-file "/x")      -> ./bob/x
      (read-file "/tom/y")  -> ./tom/y

  Relative paths resolve against the current file's directory and stay
  inside its own mount, so ".." cannot climb from /tom into the root.
  Everything is read-only; there is no write builtin.

Optional stdlib modules (opt in with `(use ...)`):
  math        sqrt cbrt pow log log10 log2 exp sin cos tan asin acos atan
              atan2 sign gcd lcm pi e parse-int parse-float
  strings     lstrip rstrip truncate pad-left pad-right repeat chars lines
              str-reverse index-of replace-first
              to-uppercase to-lowercase to-titlecase to-sentencecase
              to-snakecase to-camelcase strip-html
              spintax                          ; (spintax "{a|b|c}" seed)
  collections sort uniq sum avg flatten partition zip take drop frequencies
              pluck group-by
  finance     pmt fv pv npv nper rate          ; Excel-style cashflow funcs
  predicates  inc dec zero? pos? neg? even? odd? between? non-nil?
              coalesce default                 ; defined as macros
  rng         rand rand-int choose dice-roll shuffle set-seed
              spintax           ; 1-arg form: uses shared context seed
              ; seeded by --seed (default: current millis); host programs
              ; pass EvalConfig::with_rng_seed(...)

Examples:
  trongo eval -i config.trongo --format yaml
  trongo eval --root ./scripts -i main.trongo
  trongo render -i template.erb --root ./views
  trongo embed -i config.yaml --root .  # process !trongo snippets in YAML
  trongo eval --root ./bob --mount /tom=./tom -i main.trongo
"#;

fn print_help() {
    print!("{HELP}");
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.