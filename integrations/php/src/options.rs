//! Parsing the `$options` arrays the PHP API accepts.

use ext_php_rs::types::{ZendHashTable, Zval};
use trongoscript_host::{fuel_limits, sandbox_limits, Markers, Options, SandboxSpec};

use crate::convert::{from_php, key_string, type_error, type_name, utf8, value_error, Fail};

/// Everything a `Context` is built from.
pub struct ContextOptions {
    pub options: Options,
    pub functions: Vec<(String, Zval)>,
    pub objects: bool,
}

/// Parse `$options`. With `markers`, the embed marker keys (`prefix`,
/// `key`, `tag`) are accepted too, for the one-shot embed functions.
pub fn parse(
    opts: Option<&ZendHashTable>,
    with_markers: bool,
) -> Result<(ContextOptions, Markers), Fail> {
    let mut root = None;
    let mut mounts = Vec::new();
    let mut files = None;
    let mut options = Options::default();
    let mut preset = None;
    let mut limit_overrides = Vec::new();
    let mut functions = Vec::new();
    let mut objects = false;
    let mut markers = Markers::default();

    for (key, value) in entries(opts, "options")? {
        let value = value.dereference();
        match key.as_str() {
            "root" => root = Some(utf8("root", value)?),
            "mounts" => {
                for (v, h) in entries(array("mounts", value)?, "mounts")? {
                    mounts.push((v, utf8("mount path", h)?.into()));
                }
            }
            "files" => {
                let mut list = Vec::new();
                for (path, contents) in entries(array("files", value)?, "files")? {
                    list.push((path, utf8("file contents", contents)?));
                }
                files = Some(list);
            }
            "seed" => options.seed = Some(non_negative("seed", value)?),
            "limits" => {
                if value.is_string() {
                    preset = Some(utf8("limits", value)?);
                } else {
                    for (k, v) in entries(array("limits", value)?, "limits")? {
                        if k == "preset" {
                            preset = Some(utf8("limits.preset", v)?);
                        } else {
                            limit_overrides.push((k.clone(), non_negative(&k, v)?));
                        }
                    }
                }
            }
            "sandbox_limits" => {
                let mut overrides = Vec::new();
                for (k, v) in entries(array("sandbox_limits", value)?, "sandbox_limits")? {
                    overrides.push((k.clone(), non_negative(&k, v)?));
                }
                options.sandbox_limits = sandbox_limits(&overrides).map_err(argument)?;
            }
            "globals" => {
                for (name, v) in entries(array("globals", value)?, "globals")? {
                    options.globals.push((name, from_php(v)?));
                }
            }
            "functions" => {
                for (name, f) in entries(array("functions", value)?, "functions")? {
                    if !f.is_callable() {
                        return Err(type_error(format!(
                            "functions[{name:?}] must be callable, got {}",
                            type_name(f)
                        )));
                    }
                    functions.push((name, f.shallow_clone()));
                }
            }
            "objects" => {
                objects = value.bool().ok_or_else(|| {
                    type_error(format!("objects must be a bool, got {}", type_name(value)))
                })?;
            }
            "prefix" if with_markers => markers.prefix = utf8("prefix", value)?,
            "key" if with_markers => markers.key = utf8("key", value)?,
            "tag" if with_markers => markers.tag = utf8("tag", value)?,
            other => return Err(value_error(format!("unknown option {other:?}"))),
        }
    }

    options.sandbox = match (root, files) {
        (Some(_), Some(_)) => return Err(value_error("pass root or files, not both")),
        (Some(root), None) => SandboxSpec::Dir {
            root: root.into(),
            mounts,
        },
        (None, files) => {
            if !mounts.is_empty() {
                return Err(value_error("mounts needs a root"));
            }
            files.map_or(SandboxSpec::None, |files| SandboxSpec::Memory { files })
        }
    };
    options.limits = fuel_limits(preset.as_deref(), &limit_overrides).map_err(argument)?;
    Ok((
        ContextOptions {
            options,
            functions,
            objects,
        },
        markers,
    ))
}

/// Embed markers alone, for `Context::embed()`.
pub fn markers(opts: Option<&ZendHashTable>) -> Result<Markers, Fail> {
    let mut m = Markers::default();
    for (key, value) in entries(opts, "markers")? {
        match key.as_str() {
            "prefix" => m.prefix = utf8("prefix", value)?,
            "key" => m.key = utf8("key", value)?,
            "tag" => m.tag = utf8("tag", value)?,
            other => return Err(value_error(format!("unknown marker {other:?}"))),
        }
    }
    Ok(m)
}

fn argument(e: trongoscript_host::HostError) -> Fail {
    value_error(e.message)
}

fn array<'a>(what: &str, z: &'a Zval) -> Result<Option<&'a ZendHashTable>, Fail> {
    z.array()
        .map(Some)
        .ok_or_else(|| type_error(format!("{what} must be an array, got {}", type_name(z))))
}

/// Entries with string keys. Integer keys are refused: every option array
/// here is keyed by a name or a path.
fn entries<'a>(ht: Option<&'a ZendHashTable>, what: &str) -> Result<Vec<(String, &'a Zval)>, Fail> {
    let Some(ht) = ht else {
        return Ok(Vec::new());
    };
    let mut it = ht.iter();
    let mut out = Vec::with_capacity(ht.len());
    // `next_zval` rather than plain iteration, which panics on a key that
    // is not UTF-8; see `convert::raw_entries`.
    while let Some((k, v)) = it.next_zval() {
        if let Some(i) = k.long() {
            return Err(type_error(format!(
                "{what} must be keyed by strings, got the integer key {i}"
            )));
        }
        out.push((key_string(&k)?, v.dereference()));
    }
    Ok(out)
}

fn non_negative(what: &str, z: &Zval) -> Result<u64, Fail> {
    let n = z
        .long()
        .ok_or_else(|| type_error(format!("{what} must be an int, got {}", type_name(z))))?;
    u64::try_from(n).map_err(|_| value_error(format!("{what} must not be negative, got {n}")))
}

// Copyright (c) 2026 Durable Programming, LLC.
