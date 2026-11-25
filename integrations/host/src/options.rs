//! Translating binding-level options into an [`EvalConfig`].
//!
//! The bindings parse their language's keyword arguments or option arrays
//! into plain Rust values and hand them to [`Options`]; validation happens
//! here so both bindings reject the same inputs with the same messages.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use trongoscript::vfs::{MemoryFs, MEMORY_FS_ROOT};
use trongoscript::{EvalConfig, FuelLimits, Sandbox, SandboxLimits, Value};

use crate::error::HostError;

/// Where `include`, `read-file`, and `list-files` read from.
#[derive(Debug, Clone, Default)]
pub enum SandboxSpec {
    /// No filesystem access at all. The default.
    #[default]
    None,
    /// A host directory as the virtual root, plus extra host directories
    /// grafted in at absolute virtual paths.
    Dir {
        root: PathBuf,
        mounts: Vec<(String, PathBuf)>,
    },
    /// An in-memory file tree, keyed by virtual path (`"lib/a.trongo"` or
    /// `"/lib/a.trongo"`; both name the same file).
    Memory { files: Vec<(String, String)> },
}

/// Everything needed to build a [`trongoscript::Context`].
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub sandbox: SandboxSpec,
    pub sandbox_limits: SandboxLimits,
    /// Seed for the `rng` module. `None` means 0, which keeps runs
    /// reproducible; the bindings do not pick a time-based seed on the
    /// caller's behalf.
    pub seed: Option<u64>,
    pub limits: FuelLimits,
    /// Initial global bindings, visible to scripts as plain symbols.
    pub globals: Vec<(String, Value)>,
}

impl Options {
    pub(crate) fn into_config(self) -> Result<(EvalConfig, Vec<(String, Value)>), HostError> {
        let mut cfg = EvalConfig::default().with_fuel_limits(self.limits);
        if let Some(seed) = self.seed {
            cfg = cfg.with_rng_seed(seed);
        }
        if let Some(sb) = build_sandbox(self.sandbox)? {
            cfg = cfg.with_sandbox(sb.with_limits(self.sandbox_limits));
        }
        for (name, _) in &self.globals {
            validate_name(name)?;
        }
        Ok((cfg, self.globals))
    }
}

fn build_sandbox(spec: SandboxSpec) -> Result<Option<Sandbox>, HostError> {
    match spec {
        SandboxSpec::None => Ok(None),
        SandboxSpec::Dir { root, mounts } => {
            let mut sb = Sandbox::new(&root).map_err(|e| {
                HostError::new(
                    crate::ErrorKind::Sandbox,
                    format!("sandbox root {}: {e}", root.display()),
                )
            })?;
            for (virtual_path, host) in mounts {
                sb = sb.mount(&virtual_path, &host).map_err(|e| {
                    HostError::new(
                        crate::ErrorKind::Sandbox,
                        format!("mount {virtual_path} => {}: {e}", host.display()),
                    )
                })?;
            }
            Ok(Some(sb))
        }
        SandboxSpec::Memory { files } => {
            if files.is_empty() {
                return Err(HostError::argument(
                    "files: an in-memory sandbox needs at least one file",
                ));
            }
            let mut fs = MemoryFs::new();
            for (path, contents) in files {
                fs.insert(memory_path(&path)?, contents);
            }
            let sb = Sandbox::with_store(MEMORY_FS_ROOT, Arc::new(fs))?;
            Ok(Some(sb))
        }
    }
}

/// Place a virtual path under [`MEMORY_FS_ROOT`].
///
/// `..` is refused outright rather than normalized: a key that climbs out
/// of the tree would land outside the sandbox root, where no script could
/// ever read it, and a silently unreachable file is a harder bug to find
/// than an error.
fn memory_path(key: &str) -> Result<PathBuf, HostError> {
    let rel = Path::new(key.trim_start_matches('/'));
    let mut has_name = false;
    for c in rel.components() {
        match c {
            Component::Normal(_) => has_name = true,
            Component::CurDir => {}
            _ => {
                return Err(HostError::argument(format!(
                    "files: path {key:?} must not contain `..`"
                )))
            }
        }
    }
    if !has_name {
        return Err(HostError::argument(format!(
            "files: path {key:?} does not name a file"
        )));
    }
    Ok(Path::new(MEMORY_FS_ROOT).join(rel))
}

/// A global's name must be something a script can actually refer to.
pub(crate) fn validate_name(name: &str) -> Result<(), HostError> {
    let ok = !name.is_empty()
        && !name.chars().any(|c| {
            c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '{' | '}' | '"' | ';' | ',')
        });
    if ok {
        Ok(())
    } else {
        Err(HostError::argument(format!(
            "{name:?} is not a valid Trongoscript symbol"
        )))
    }
}

/// Build [`FuelLimits`] from a preset name plus per-field overrides.
///
/// Presets are `"default"`, `"strict"` (for untrusted input), and
/// `"unlimited"` (trusted input only). Overrides use the field names of
/// [`FuelLimits`]. Depths are clamped to what the interpreter thread's stack
/// survives, exactly as the CLI does: a deeper request would turn into a
/// crash the host cannot catch.
pub fn fuel_limits(
    preset: Option<&str>,
    overrides: &[(String, u64)],
) -> Result<FuelLimits, HostError> {
    let mut l = match preset.unwrap_or("default") {
        "default" => FuelLimits::default(),
        "strict" => FuelLimits::strict(),
        "unlimited" => FuelLimits::unlimited(),
        other => {
            return Err(HostError::argument(format!(
                "unknown limits preset {other:?} (expected default, strict, or unlimited)"
            )))
        }
    };
    for (key, v) in overrides {
        let v = *v;
        match key.as_str() {
            "max_steps" => l.max_steps = v,
            "max_call_depth" => l.max_call_depth = to_usize(key, v)?,
            "max_eval_depth" => l.max_eval_depth = to_usize(key, v)?,
            "max_value_len" => l.max_value_len = to_usize(key, v)?,
            "max_total_alloc" => l.max_total_alloc = v,
            other => {
                return Err(HostError::argument(format!(
                    "unknown limit {other:?} (expected max_steps, max_call_depth, \
                     max_eval_depth, max_value_len, or max_total_alloc)"
                )))
            }
        }
    }
    Ok(l.clamp_to_stack_safe())
}

/// Build [`SandboxLimits`] from overrides on the defaults.
pub fn sandbox_limits(overrides: &[(String, u64)]) -> Result<SandboxLimits, HostError> {
    let mut l = SandboxLimits::default();
    for (key, v) in overrides {
        let v = *v;
        match key.as_str() {
            "max_file_bytes" => l.max_file_bytes = v,
            "max_include_depth" => l.max_include_depth = to_usize(key, v)?,
            "max_list_entries" => l.max_list_entries = to_usize(key, v)?,
            "max_glob_depth" => l.max_glob_depth = to_usize(key, v)?,
            other => {
                return Err(HostError::argument(format!(
                    "unknown sandbox limit {other:?} (expected max_file_bytes, \
                     max_include_depth, max_list_entries, or max_glob_depth)"
                )))
            }
        }
    }
    Ok(l)
}

fn to_usize(key: &str, v: u64) -> Result<usize, HostError> {
    usize::try_from(v).map_err(|_| HostError::argument(format!("{key} is too large: {v}")))
}

// Copyright (c) 2026 Durable Programming, LLC.
