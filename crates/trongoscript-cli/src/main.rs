//! Trongoscript CLI.
//!
//! Subcommands:
//!   trongo eval [-i FILE] [--root DIR] [--format json|yaml|toml|trongo]
//!     Evaluate a Trongoscript document and print the value.
//!
//!   trongo render [-i FILE] [--root DIR]
//!     Render an ERB-like template (raw text + Trongoscript blocks).
//!
//!   trongo embed --in FILE [--root DIR] [--format json|yaml|toml]
//!     Walk a JSON/YAML/TOML file and substitute embedded Trongoscript
//!     snippets in place.
//!
//!   trongo convert [-i FILE] [--from json|yaml|toml] [--to json|yaml|toml|trongo]
//!     Convert a data document between formats without evaluating it.
//!     Defaults to `--to trongo`, so `trongo convert -i config.yaml`
//!     prints the equivalent Trongoscript literal.
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
    FuelLimits, Sandbox, Value,
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
    /// Convert a data document between formats without evaluating it.
    Convert(ConvertArgs),
    /// Print the source language reference.
    Reference,
}

/// Resource ceilings, shared by every command that evaluates.
///
/// Defaults come from [`FuelLimits::default`] and are generous enough that
/// ordinary scripts never notice. Lower them when evaluating input you did
/// not write.
#[derive(Parser, Debug, Clone)]
struct FuelArgs {
    /// Apply tight resource limits suitable for untrusted input.
    /// Individual --max-* flags still override the preset.
    #[arg(long)]
    untrusted: bool,
    /// Remove the step and allocation budgets. Recursion depth stays
    /// capped: exceeding the native stack crashes the process instead of
    /// erroring, so it is not something a flag can turn off. A script run
    /// this way can loop forever; only use it on input you trust.
    #[arg(long, conflicts_with = "untrusted")]
    no_limits: bool,
    /// Maximum evaluation steps before aborting.
    #[arg(long, value_name = "N")]
    max_steps: Option<u64>,
    /// Maximum nested function call depth. Capped at what the native
    /// stack can survive, since exceeding it would crash rather than error.
    #[arg(long, value_name = "N")]
    max_call_depth: Option<usize>,
    /// Maximum expression/data nesting depth. Capped for the same reason
    /// as --max-call-depth.
    #[arg(long, value_name = "N")]
    max_eval_depth: Option<usize>,
    /// Maximum elements (or string bytes) in a single value.
    #[arg(long, value_name = "N")]
    max_value_len: Option<usize>,
    /// Maximum cumulative allocation in bytes.
    #[arg(long, value_name = "BYTES")]
    max_alloc: Option<u64>,
    /// Print steps and bytes consumed to stderr after evaluating.
    #[arg(long)]
    report_fuel: bool,
}

impl FuelArgs {
    fn limits(&self) -> FuelLimits {
        let mut l = if self.no_limits {
            FuelLimits::unlimited()
        } else if self.untrusted {
            FuelLimits::strict()
        } else {
            FuelLimits::default()
        };
        if let Some(v) = self.max_steps {
            l.max_steps = v;
        }
        if let Some(v) = self.max_call_depth {
            l.max_call_depth = v;
        }
        if let Some(v) = self.max_eval_depth {
            l.max_eval_depth = v;
        }
        if let Some(v) = self.max_value_len {
            l.max_value_len = v;
        }
        if let Some(v) = self.max_alloc {
            l.max_total_alloc = v;
        }
        // Depth flags are clamped to what the native stack can survive.
        // A caller asking for a million frames is asking for a SIGSEGV,
        // which no exit code can report — so the request is capped. Step
        // and allocation budgets are left exactly as given: those fail
        // safely, and how long a trusted script may run is the operator's
        // decision, not ours.
        l.clamp_to_stack_safe()
    }
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
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    format: OutputFormat,
    #[command(flatten)]
    fuel: FuelArgs,
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
    #[command(flatten)]
    fuel: FuelArgs,
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
    #[command(flatten)]
    fuel: FuelArgs,
}

#[derive(Parser, Debug)]
struct ConvertArgs {
    /// Input document. Use `-` for stdin (the default if omitted).
    #[arg(short = 'i', long = "in")]
    input: Option<PathBuf>,
    /// Input format. Defaults to the file extension; required for stdin.
    #[arg(long, value_enum)]
    from: Option<Format>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Trongo)]
    to: OutputFormat,
    /// Indent nested collections instead of emitting one line. Only
    /// affects `--to trongo`; the other formats are always pretty-printed.
    #[arg(long)]
    pretty: bool,
}

/// A format that can be both read and written.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Format {
    Json,
    Yaml,
    Toml,
}

/// A format that can be written. Trongoscript is emit-only here: reading it
/// back is `eval`'s job, not a data-document parse.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Json,
    Yaml,
    Toml,
    /// Trongoscript source text.
    Trongo,
}

impl From<Format> for OutputFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Json => OutputFormat::Json,
            Format::Yaml => OutputFormat::Yaml,
            Format::Toml => OutputFormat::Toml,
        }
    }
}

/// Stack reserved for the interpreter thread.
///
/// The evaluator recurses on the native stack, so the depth a script may
/// reach is bounded by whatever stack it runs on. The main thread's is
/// whatever the OS chose (commonly 8 MiB, but as little as 512 KiB under
/// some launchers), and a debug build's frames are large enough that even
/// 8 MiB only affords a few hundred frames.
///
/// Running evaluation on a thread with an explicit, generous stack makes
/// the usable recursion depth a property of this program rather than of
/// the build profile and the launcher — so the fuel ceilings can be set
/// where they serve users, not where the weakest possible stack forces
/// them.
/// Stack sizes to try for the interpreter thread, largest first.
///
/// A thread's stack is reserved address space, so the largest size fails
/// outright under a tight `RLIMIT_AS` — in a memory-capped container, for
/// instance. Falling back keeps the CLI usable there instead of refusing
/// to start; the only cost is a shallower safe recursion depth, which the
/// fuel limits already bound independently.
const INTERPRETER_STACK_SIZES: [usize; 4] = [
    256 * 1024 * 1024,
    64 * 1024 * 1024,
    16 * 1024 * 1024,
    4 * 1024 * 1024,
];

fn main() -> std::process::ExitCode {
    // Evaluate on an explicitly-sized stack; see INTERPRETER_STACK_SIZES.
    let mut last_err = None;
    for size in INTERPRETER_STACK_SIZES {
        match std::thread::Builder::new()
            .name("trongo-eval".into())
            .stack_size(size)
            .spawn(run)
        {
            Ok(handle) => {
                return match handle.join() {
                    Ok(code) => code,
                    // The interpreter thread panicked; it has already
                    // printed its own message via the default hook.
                    Err(_) => std::process::ExitCode::FAILURE,
                };
            }
            Err(e) => last_err = Some(e),
        }
    }
    // Every size failed — the process is too constrained to run at all.
    // Fall back to the main thread rather than refusing outright.
    if let Some(e) = last_err {
        eprintln!(
            "warning: could not reserve an interpreter stack ({e}); running on the main thread"
        );
    }
    run()
}

fn run() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Eval(a) => cmd_eval(a),
        Command::Render(a) => cmd_render(a),
        Command::Embed(a) => cmd_embed(a),
        Command::Convert(a) => cmd_convert(a),
        Command::Reference => {
            print_help();
            Ok(())
        }
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {:#}", e);
            if e.chain().any(|c| c.is::<ResourceLimitHit>()) {
                std::process::ExitCode::from(EXIT_RESOURCE_LIMIT as u8)
            } else {
                std::process::ExitCode::FAILURE
            }
        }
    }
}

fn cmd_eval(a: EvalArgs) -> Result<()> {
    let source = read_source(a.input.as_deref())?;
    let mut ctx = make_context(a.root.as_deref(), &a.mounts, a.seed, &a.fuel)?;
    let value = eval_source(&source, &mut ctx).map_err(classify_eval_error)?;
    report_fuel(&ctx, &a.fuel);
    write_value(&value, a.format)
}

fn cmd_render(a: RenderArgs) -> Result<()> {
    let source = read_source(a.input.as_deref())?;
    let mut ctx = make_context(a.root.as_deref(), &a.mounts, a.seed, &a.fuel)?;
    let out = render_template(&source, &mut ctx).map_err(classify_eval_error)?;
    report_fuel(&ctx, &a.fuel);
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(out.as_bytes())?;
    Ok(())
}

/// Exit code used when evaluation stopped because it hit a resource
/// ceiling rather than because the script was wrong. Distinct from the
/// generic failure code so callers can tell "too expensive" from "broken"
/// without parsing the message.
const EXIT_RESOURCE_LIMIT: i32 = 3;

/// Error carrying the fact that evaluation stopped on a resource ceiling.
///
/// Kept as its own type (rather than an `anyhow` context string) so `main`
/// can recognize it by type and pick the exit code without matching on
/// message text.
#[derive(Debug)]
struct ResourceLimitHit(String);

impl std::fmt::Display for ResourceLimitHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "resource limit exceeded: {}", self.0)
    }
}

impl std::error::Error for ResourceLimitHit {}

/// Wrap an evaluation error, tagging resource-limit failures so `main` can
/// give them their own exit code.
fn classify_eval_error(e: trongoscript::EvalError) -> anyhow::Error {
    if e.is_resource_limit() {
        anyhow::Error::new(ResourceLimitHit(e.to_string()))
    } else {
        anyhow!("{}", e)
    }
}

/// Print consumption to stderr when `--report-fuel` was passed. Goes to
/// stderr so it never contaminates the evaluated output on stdout.
fn report_fuel(ctx: &Context, fuel: &FuelArgs) {
    if !fuel.report_fuel {
        return;
    }
    eprintln!(
        "fuel: steps={} allocated={} peak_call_depth={} peak_eval_depth={}",
        ctx.fuel.steps(),
        ctx.fuel.allocated(),
        ctx.fuel.peak_call_depth(),
        ctx.fuel.peak_eval_depth(),
    );
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
    let mut ctx = make_context(a.root.as_deref(), &a.mounts, a.seed, &a.fuel)?;
    let out = match format {
        Format::Yaml => {
            // Use the YAML-aware path so `!trongo` tags are honored as real
            // YAML tags (rather than being lost in the json conversion).
            let y: serde_yaml::Value = serde_yaml::from_str(&raw).context("parsing YAML")?;
            evaluate_embedded_yaml(y, &mut ctx).map_err(classify_eval_error)?
        }
        _ => {
            let value = parse_document(&raw, format)?;
            evaluate_embedded(value, &mut ctx).map_err(classify_eval_error)?
        }
    };
    report_fuel(&ctx, &a.fuel);
    write_value(&out, format.into())
}

/// Convert a data document between formats. No evaluation happens — this is
/// a pure reformat, so an input containing Trongoscript snippets keeps them
/// as the strings they are.
fn cmd_convert(a: ConvertArgs) -> Result<()> {
    let from = a
        .from
        .or_else(|| a.input.as_deref().and_then(guess_format_from_path))
        .ok_or_else(|| {
            anyhow!("could not infer input format from extension; pass --from json|yaml|toml")
        })?;
    let raw = read_source(a.input.as_deref())?;
    let value = parse_document(&raw, from)?;
    write_value_with(&value, a.to, a.pretty)
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
    fuel: &FuelArgs,
) -> Result<Context> {
    let mut cfg = EvalConfig::default().with_fuel_limits(fuel.limits());
    if let Some(r) = root {
        let mut sb =
            Sandbox::new(r).with_context(|| format!("opening sandbox root {}", r.display()))?;
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
        Some(p) => std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display())),
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

fn write_value(v: &Value, format: OutputFormat) -> Result<()> {
    write_value_with(v, format, false)
}

/// Write `v` in `format`. `pretty` only applies to Trongoscript output —
/// the other three formats have a single canonical spelling here.
fn write_value_with(v: &Value, format: OutputFormat, pretty: bool) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    match format {
        OutputFormat::Trongo => {
            let s = if pretty {
                trongoscript::to_trongo_source_pretty(v)
            } else {
                trongoscript::to_trongo_source(v)
            };
            stdout.write_all(s.as_bytes())?;
            stdout.write_all(b"\n")?;
        }
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut stdout, v)?;
            stdout.write_all(b"\n")?;
        }
        OutputFormat::Yaml => {
            serde_yaml::to_writer(&mut stdout, v)?;
        }
        OutputFormat::Toml => {
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
  (-> x (f a) g ...)      ; thread x as first arg of each step
  (->> x (f a) g ...)     ; thread x as last arg of each step
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
  serde       to-json to-yaml to-toml          ; 2nd arg true => pretty
              parse-json parse-yaml parse-toml
              to-trongo                        ; value => Trongoscript text
              ; to-toml needs a map root; TOML has no null
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
  trongo convert -i config.yaml         # YAML => Trongoscript literal
  trongo convert -i data.json --pretty  # indented, one key per line
  trongo convert -i x.toml --to yaml    # any format pair, no evaluation
"#;

fn print_help() {
    print!("{HELP}");
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
