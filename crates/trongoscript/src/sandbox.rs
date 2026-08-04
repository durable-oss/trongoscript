//! Filesystem sandbox.
//!
//! Trongoscript has **no** ambient filesystem access. To allow scripts to
//! `include` other Trongoscript files or to call `read-file`/`list-files`,
//! the embedding program must construct a [`Sandbox`] rooted at a specific
//! directory and pass it to the evaluator.
//!
//! # Virtual filesystem
//!
//! A sandbox presents scripts with a *virtual* filesystem rather than a
//! window onto the host one. The root directory passed to [`Sandbox::new`]
//! is mounted at the virtual path `/`; additional host directories can be
//! grafted in at other virtual paths with [`Sandbox::mount`]:
//!
//! ```text
//! Sandbox::new("./bob")?.mount("/tom", "./tom")?
//!
//!   /x       -> ./bob/x
//!   /tom/y   -> ./tom/y
//! ```
//!
//! Script-visible paths are therefore *not* host paths. An absolute path
//! like `/tom/y` is absolute in sandbox-space and says nothing about the
//! host layout; the host `/` is never reachable. Relative paths resolve
//! against the directory of the file currently being evaluated, exactly as
//! they did before mounts existed.
//!
//! The sandbox enforces:
//!   - Absolute paths are interpreted against the virtual root, and are
//!     routed to a mount by longest-prefix match.
//!   - Relative paths resolve against the current file's directory, and the
//!     result must land inside the mount that directory belongs to.
//!   - Symbolic links are resolved via canonicalization, and the resolved
//!     real path must remain inside the owning mount's host directory.
//!   - Path components `..` are allowed only insofar as they don't escape
//!     that mount after resolution. `..` never crosses a mount boundary,
//!     so a script under `/tom` cannot climb into the root mount.
//!   - Per-call recursion depth and total bytes read are capped to defang
//!     pathological `include` chains and huge files.
//!
//! When mounts overlap a real directory — a `/tom` mount when `./bob/tom`
//! also exists on disk — the mount wins and the real directory becomes
//! unreachable. Shadowing is resolved by longest virtual-prefix match, not
//! by consulting the filesystem, so it cannot vary with disk contents.
//!
//! Even when a sandbox is provided, **listing or reading is the *only*
//! filesystem access available** — no writing, no executing, no network.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

/// Errors raised by sandbox operations.
#[derive(Error, Debug)]
pub enum SandboxError {
    #[error("filesystem access not permitted (no sandbox root configured)")]
    NotPermitted,
    #[error("path escapes sandbox root: {0}")]
    Escape(String),
    #[error("path must be relative: {0}")]
    AbsolutePath(String),
    #[error("no mount covers path: {0}")]
    NoMount(String),
    #[error("mount point must be an absolute virtual path: {0}")]
    BadMountPoint(String),
    #[error("mount point already in use: {0}")]
    DuplicateMount(String),
    #[error("path not found: {0}")]
    NotFound(String),
    #[error("not a regular file: {0}")]
    NotAFile(String),
    #[error("not a directory: {0}")]
    NotADirectory(String),
    #[error("file too large: {path} ({size} bytes, limit {limit})")]
    FileTooLarge {
        path: String,
        size: u64,
        limit: u64,
    },
    #[error("include depth exceeded ({max})")]
    IncludeTooDeep { max: usize },
    #[error("include cycle detected: {0}")]
    IncludeCycle(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Per-sandbox limits.
#[derive(Debug, Clone)]
pub struct SandboxLimits {
    /// Maximum size of a single file in bytes (for `read-file` / `include`).
    pub max_file_bytes: u64,
    /// Maximum depth of nested `include` calls.
    pub max_include_depth: usize,
    /// Maximum number of entries returned by `list-files` / `glob`.
    pub max_list_entries: usize,
    /// Maximum directory depth a `**` segment may descend into.
    pub max_glob_depth: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 4 * 1024 * 1024, // 4 MiB
            max_include_depth: 16,
            max_list_entries: 4096,
            max_glob_depth: 16,
        }
    }
}

/// One entry in the sandbox mount table: a host directory grafted in at a
/// virtual path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Absolute virtual path, normalized, without a trailing slash. The
    /// root mount is the empty string (i.e. virtual `/`).
    virtual_path: String,
    /// Canonicalized host directory backing this mount.
    host_path: PathBuf,
}

impl Mount {
    /// The virtual path this mount is attached to, e.g. `/tom`. The root
    /// mount reports `/`.
    pub fn virtual_path(&self) -> &str {
        if self.virtual_path.is_empty() {
            "/"
        } else {
            &self.virtual_path
        }
    }

    /// The host directory backing this mount.
    pub fn host_path(&self) -> &Path {
        &self.host_path
    }
}

/// A configured filesystem sandbox.
///
/// Cloning is cheap: the mount table sits behind an [`Arc`], because the
/// evaluator clones the sandbox on every sandboxed I/O builtin call.
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// Mount table, kept sorted by descending virtual-path length so a
    /// linear scan yields the longest-prefix match first. Always contains
    /// at least the root mount at index `len - 1`.
    mounts: Arc<Vec<Mount>>,
    limits: SandboxLimits,
}

impl Sandbox {
    /// Create a sandbox rooted at `root`. The path is canonicalized; if it
    /// does not exist or is not a directory the function returns an error.
    ///
    /// The root is mounted at the virtual path `/`. Additional host
    /// directories can be attached with [`Self::mount`].
    pub fn new<P: AsRef<Path>>(root: P) -> Result<Self, SandboxError> {
        let root = canonical_dir(root.as_ref())?;
        Ok(Self {
            mounts: Arc::new(vec![Mount {
                virtual_path: String::new(),
                host_path: root,
            }]),
            limits: SandboxLimits::default(),
        })
    }

    pub fn with_limits(mut self, limits: SandboxLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Graft `host_path` into the virtual filesystem at `virtual_path`.
    ///
    /// `virtual_path` must be absolute in sandbox-space (`/tom`,
    /// `/vendor/lib`); it is normalized, so `/tom/` and `/tom` are the same
    /// mount point. Mounting at `/` replaces the root. Nesting a mount
    /// inside another mount's virtual subtree is allowed — the deeper one
    /// shadows the shallower for paths it covers.
    ///
    /// Fails if the mount point is not an absolute virtual path, if it is
    /// already mounted, or if `host_path` is not an existing directory.
    pub fn mount<P: AsRef<Path>>(
        mut self,
        virtual_path: &str,
        host_path: P,
    ) -> Result<Self, SandboxError> {
        let vpath = normalize_virtual(virtual_path)
            .ok_or_else(|| SandboxError::BadMountPoint(virtual_path.to_string()))?;
        let host = canonical_dir(host_path.as_ref())?;

        let mounts = Arc::make_mut(&mut self.mounts);
        if let Some(existing) = mounts.iter_mut().find(|m| m.virtual_path == vpath) {
            // Re-mounting the root is how a host swaps out the default;
            // any other duplicate is a configuration mistake.
            if vpath.is_empty() {
                existing.host_path = host;
                return Ok(self);
            }
            return Err(SandboxError::DuplicateMount(virtual_path.to_string()));
        }
        mounts.push(Mount {
            virtual_path: vpath,
            host_path: host,
        });
        // Longest virtual path first, so `find` is a longest-prefix match.
        mounts.sort_by(|a, b| b.virtual_path.len().cmp(&a.virtual_path.len()));
        Ok(self)
    }

    /// The host directory backing the virtual root.
    pub fn root(&self) -> &Path {
        // The root mount has the shortest virtual path, so it sorts last.
        &self.mounts[self.mounts.len() - 1].host_path
    }

    /// All configured mounts, longest virtual path first.
    pub fn mounts(&self) -> &[Mount] {
        &self.mounts
    }

    pub fn limits(&self) -> &SandboxLimits {
        &self.limits
    }

    /// Find the mount whose host directory contains `host` (longest match).
    fn mount_for_host(&self, host: &Path) -> Option<&Mount> {
        self.mounts
            .iter()
            .filter(|m| host.starts_with(&m.host_path))
            .max_by_key(|m| m.host_path.as_os_str().len())
    }

    /// Find the mount serving virtual path `vpath` (longest prefix match).
    fn mount_for_virtual<'a>(&self, vpath: &'a str) -> Option<(&Mount, &'a str)> {
        self.mounts.iter().find_map(|m| {
            let rest = virtual_strip_prefix(vpath, &m.virtual_path)?;
            Some((m, rest))
        })
    }

    /// Resolve a script-supplied path to a host path, asserting that the
    /// result stays inside the mount that serves it.
    ///
    /// An absolute path is interpreted against the virtual root and routed
    /// to a mount by longest-prefix match. A relative path resolves against
    /// `base` — the directory of the file the script is running from — and
    /// must stay within whichever mount `base` belongs to. If `base` is
    /// `None`, the root mount's host directory is used.
    pub fn resolve(&self, base: Option<&Path>, requested: &str) -> Result<PathBuf, SandboxError> {
        if requested.contains('\0') {
            return Err(SandboxError::Escape(requested.to_string()));
        }

        // Pick the starting host directory and the mount that constrains
        // the result. Absolute paths re-anchor at the virtual root and may
        // therefore land in a different mount than `base` sits in.
        let (starting, owner, rel) = if is_virtual_absolute(requested) {
            let vpath = normalize_virtual(requested)
                .ok_or_else(|| SandboxError::Escape(requested.to_string()))?;
            let (mount, rest) = self
                .mount_for_virtual(&vpath)
                .ok_or_else(|| SandboxError::NoMount(requested.to_string()))?;
            (mount.host_path.clone(), mount, rest.to_string())
        } else {
            let start = match base {
                Some(p) => p.to_path_buf(),
                None => self.root().to_path_buf(),
            };
            let mount = self
                .mount_for_host(&start)
                .ok_or_else(|| SandboxError::Escape(requested.to_string()))?;
            (start, mount, requested.to_string())
        };

        let mut candidate = starting;
        candidate.push(Path::new(&rel));

        // Normalize: collapse `.` and `..` lexically first so we don't trip
        // a `NotFound` from canonicalize() before checking containment.
        let normalized = normalize(&candidate);

        // Canonicalize the longest existing prefix to handle symlinks.
        let resolved = canonicalize_existing_prefix(&normalized)
            .map_err(|e| SandboxError::Io(e.to_string()))?;

        // Containment is checked against the owning mount, not the root, so
        // `..` cannot climb out of `/tom` into the root mount's tree.
        if !resolved.starts_with(&owner.host_path) {
            return Err(SandboxError::Escape(requested.to_string()));
        }
        Ok(resolved)
    }

    /// Map a resolved host path back to its virtual path, e.g.
    /// `./tom/y` -> `/tom/y`. Returns `None` if no mount covers it.
    ///
    /// When mounts nest, the deepest one wins, matching how [`Self::resolve`]
    /// routes virtual paths in the other direction.
    pub fn to_virtual(&self, host: &Path) -> Option<String> {
        let mount = self.mount_for_host(host)?;
        let rel = host.strip_prefix(&mount.host_path).ok()?;
        let rel = path_to_forward_slash(rel);
        let joined = if rel.is_empty() {
            mount.virtual_path.clone()
        } else {
            format!("{}/{}", mount.virtual_path, rel)
        };
        Some(if joined.is_empty() {
            "/".to_string()
        } else {
            joined
        })
    }

    /// Read a file as a UTF-8 string, enforcing the size limit.
    pub fn read_text(&self, base: Option<&Path>, requested: &str) -> Result<String, SandboxError> {
        let path = self.resolve(base, requested)?;
        let meta = std::fs::metadata(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => SandboxError::NotFound(path.display().to_string()),
            _ => SandboxError::Io(e.to_string()),
        })?;
        if !meta.is_file() {
            return Err(SandboxError::NotAFile(path.display().to_string()));
        }
        if meta.len() > self.limits.max_file_bytes {
            return Err(SandboxError::FileTooLarge {
                path: path.display().to_string(),
                size: meta.len(),
                limit: self.limits.max_file_bytes,
            });
        }
        std::fs::read_to_string(&path).map_err(|e| SandboxError::Io(e.to_string()))
    }

    /// List the names of regular files / directories in `requested` (a
    /// directory relative to the sandbox root).
    pub fn list(&self, base: Option<&Path>, requested: &str) -> Result<Vec<DirEntry>, SandboxError> {
        let path = self.resolve(base, requested)?;
        let meta = std::fs::metadata(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => SandboxError::NotFound(path.display().to_string()),
            _ => SandboxError::Io(e.to_string()),
        })?;
        if !meta.is_dir() {
            return Err(SandboxError::NotADirectory(path.display().to_string()));
        }
        let mut out = Vec::new();
        for entry in
            std::fs::read_dir(&path).map_err(|e| SandboxError::Io(e.to_string()))?
        {
            if out.len() >= self.limits.max_list_entries {
                break;
            }
            let entry = entry.map_err(|e| SandboxError::Io(e.to_string()))?;
            let ft = entry
                .file_type()
                .map_err(|e| SandboxError::Io(e.to_string()))?;
            // Skip symlinks defensively — following them could escape the
            // root via a TOCTOU race.
            if ft.is_symlink() {
                continue;
            }
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                is_dir: ft.is_dir(),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// One result returned by [`Sandbox::glob`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobMatch {
    /// Where the match lives in the virtual filesystem, using `/`
    /// separators. Relative to the sandbox root for patterns anchored in
    /// the root mount (`sub/a.yaml`), and an absolute virtual path when the
    /// pattern was itself absolute or reached into another mount
    /// (`/tom/y.yaml`). Either form can be fed back to `read-file`.
    pub path: String,
    pub is_dir: bool,
}

impl Sandbox {
    /// Expand a glob pattern against the sandbox tree.
    ///
    /// Pattern syntax:
    /// - `*`    — any run of characters within a single path component (does
    ///   not cross `/`, does not match leading `.`)
    /// - `?`    — exactly one character within a component
    /// - `[..]` — character class, optionally negated with `!` or `^`
    /// - `**`   — zero or more intermediate directories (must be a full
    ///   path segment, e.g. `src/**/*.rs`)
    ///
    /// The pattern may be relative or an absolute virtual path; `base`
    /// plays the same role it does in [`Self::resolve`]. Symlinks
    /// encountered during the walk are skipped, and the walk never leaves
    /// the mount it started in. Returned paths use `/` as the component
    /// separator regardless of host OS — see [`GlobMatch::path`] for
    /// whether they come back relative or absolute.
    pub fn glob(
        &self,
        base: Option<&Path>,
        pattern: &str,
    ) -> Result<Vec<GlobMatch>, SandboxError> {
        if pattern.is_empty() {
            return Ok(Vec::new());
        }
        if pattern.contains('\0') {
            return Err(SandboxError::Escape(pattern.to_string()));
        }
        // Resolve the *starting directory* — the longest prefix of the
        // pattern that has no glob metacharacters. Anything fancier
        // (including `..`) is handled by the path resolver.
        let (literal_prefix, glob_tail) = split_literal_prefix(pattern);
        let absolute = is_virtual_absolute(pattern);
        let start_dir = if literal_prefix.is_empty() {
            // Anchor at base or root.
            match base {
                Some(p) => p.to_path_buf(),
                None => self.root().to_path_buf(),
            }
        } else {
            self.resolve(base, &literal_prefix)?
        };

        // start_dir must exist and be a directory for the walk to mean
        // anything; missing → no matches (consistent with shell globs).
        let meta = match std::fs::metadata(&start_dir) {
            Ok(m) => m,
            Err(_) => return Ok(Vec::new()),
        };
        if !meta.is_dir() {
            return Ok(Vec::new());
        }

        let segments: Vec<&str> = glob_tail.split('/').filter(|s| !s.is_empty()).collect();

        // Compute the prefix that emitted paths hang off. A relative
        // pattern staying in the root mount keeps the old root-relative
        // form; anything else is reported as an absolute virtual path so
        // the caller can feed results straight back to `read-file`.
        let rel_prefix = if absolute {
            let v = self
                .to_virtual(&start_dir)
                .ok_or_else(|| SandboxError::NoMount(pattern.to_string()))?;
            if v == "/" { String::new() } else { v }
        } else {
            match start_dir.strip_prefix(self.root()) {
                Ok(p) => path_to_forward_slash(p),
                // Walking a non-root mount via a relative pattern: fall back
                // to the absolute virtual form, which is still addressable.
                Err(_) => self
                    .to_virtual(&start_dir)
                    .ok_or_else(|| SandboxError::NoMount(pattern.to_string()))?,
            }
        };

        let mut out: Vec<GlobMatch> = Vec::new();
        walk_glob(
            &start_dir,
            &rel_prefix,
            &segments,
            0,
            0,
            &mut out,
            self.limits.max_list_entries,
            self.limits.max_glob_depth,
        )?;
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out.dedup_by(|a, b| a.path == b.path);
        Ok(out)
    }
}

/// Split a glob pattern into (literal directory prefix, remaining glob).
/// A segment is literal if it contains none of `*?[`.
fn split_literal_prefix(pattern: &str) -> (String, String) {
    // An absolute virtual pattern must yield an absolute literal prefix, or
    // it would be re-resolved as a relative path against the wrong mount.
    let absolute = is_virtual_absolute(pattern);
    let mut literal = String::new();
    let mut rest_iter = pattern.split('/').peekable();
    while let Some(seg) = rest_iter.peek().copied() {
        if seg.contains('*') || seg.contains('?') || seg.contains('[') || seg == ".." {
            break;
        }
        let _ = rest_iter.next();
        // Skip the empty segment produced by a leading (or doubled) `/`.
        if seg.is_empty() {
            continue;
        }
        if !literal.is_empty() {
            literal.push('/');
        }
        literal.push_str(seg);
    }
    let tail: Vec<&str> = rest_iter.collect();
    if absolute {
        literal.insert(0, '/');
    }
    (literal, tail.join("/"))
}

fn path_to_forward_slash(p: &Path) -> String {
    p.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn join_rel(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", prefix, name)
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_glob(
    dir: &Path,
    rel: &str,
    segments: &[&str],
    seg_idx: usize,
    depth: usize,
    out: &mut Vec<GlobMatch>,
    max_entries: usize,
    max_depth: usize,
) -> Result<(), SandboxError> {
    if out.len() >= max_entries {
        return Ok(());
    }
    if depth > max_depth {
        return Ok(());
    }
    // No glob segments left and we're still in a dir: the literal prefix
    // itself was the match (e.g. `(glob "subdir")`).
    if seg_idx >= segments.len() {
        out.push(GlobMatch {
            path: rel.to_string(),
            is_dir: true,
        });
        return Ok(());
    }

    let seg = segments[seg_idx];

    if seg == "**" {
        // (a) Treat `**` as matching zero segments — try the next segment
        //     against this same directory.
        walk_glob(
            dir,
            rel,
            segments,
            seg_idx + 1,
            depth,
            out,
            max_entries,
            max_depth,
        )?;
        // (b) And/or descend one level, keeping `**` active.
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries {
            if out.len() >= max_entries {
                break;
            }
            let entry = entry.map_err(|e| SandboxError::Io(e.to_string()))?;
            let ft = entry
                .file_type()
                .map_err(|e| SandboxError::Io(e.to_string()))?;
            if ft.is_symlink() {
                continue;
            }
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // Same dotfile rule: `**` doesn't descend into hidden dirs.
            if name.starts_with('.') {
                continue;
            }
            let child_rel = join_rel(rel, &name);
            let child_path = entry.path();
            walk_glob(
                &child_path,
                &child_rel,
                segments,
                seg_idx,
                depth + 1,
                out,
                max_entries,
                max_depth,
            )?;
        }
        return Ok(());
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    let is_last = seg_idx + 1 == segments.len();

    for entry in entries {
        if out.len() >= max_entries {
            break;
        }
        let entry = entry.map_err(|e| SandboxError::Io(e.to_string()))?;
        let ft = entry
            .file_type()
            .map_err(|e| SandboxError::Io(e.to_string()))?;
        if ft.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !match_segment(seg, &name) {
            continue;
        }
        let child_rel = join_rel(rel, &name);
        if is_last {
            out.push(GlobMatch {
                path: child_rel,
                is_dir: ft.is_dir(),
            });
        } else if ft.is_dir() {
            let child_path = entry.path();
            walk_glob(
                &child_path,
                &child_rel,
                segments,
                seg_idx + 1,
                depth + 1,
                out,
                max_entries,
                max_depth,
            )?;
        }
    }
    Ok(())
}

/// Match a single path component against a glob segment.
fn match_segment(pat: &str, name: &str) -> bool {
    // POSIX-ish dotfile rule: a leading `.` in `name` is matched only when
    // the pattern also starts with a literal `.`.
    if name.starts_with('.') && !pat.starts_with('.') {
        return false;
    }
    let pat_bytes = pat.as_bytes();
    let name_bytes = name.as_bytes();
    glob_match(pat_bytes, 0, name_bytes, 0)
}

fn glob_match(pat: &[u8], mut pi: usize, name: &[u8], mut ni: usize) -> bool {
    let mut star_pi: Option<usize> = None;
    let mut star_ni: usize = 0;
    loop {
        if ni < name.len() {
            if pi < pat.len() {
                match pat[pi] {
                    b'*' => {
                        star_pi = Some(pi);
                        star_ni = ni;
                        pi += 1;
                        continue;
                    }
                    b'?' => {
                        pi += 1;
                        ni += 1;
                        continue;
                    }
                    b'[' => {
                        if let Some((matched, next_pi)) = match_class(pat, pi, name[ni]) {
                            if matched {
                                pi = next_pi;
                                ni += 1;
                                continue;
                            }
                        }
                        // Fall through to backtrack.
                    }
                    c if c == name[ni] => {
                        pi += 1;
                        ni += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            // mismatch — backtrack to last star (if any)
            if let Some(sp) = star_pi {
                pi = sp + 1;
                star_ni += 1;
                ni = star_ni;
            } else {
                return false;
            }
        } else {
            // name consumed — pattern must reduce to `*`s
            while pi < pat.len() && pat[pi] == b'*' {
                pi += 1;
            }
            return pi == pat.len();
        }
    }
}

/// Match a `[..]` character class against `c`, returning `(matched, position
/// after the closing `]`)`. Returns `None` for a malformed class (treated as
/// a non-match by the caller).
fn match_class(pat: &[u8], start: usize, c: u8) -> Option<(bool, usize)> {
    debug_assert_eq!(pat[start], b'[');
    let mut i = start + 1;
    if i >= pat.len() {
        return None;
    }
    let negated = matches!(pat[i], b'!' | b'^');
    if negated {
        i += 1;
    }
    let mut found = false;
    while i < pat.len() && pat[i] != b']' {
        let lo = pat[i];
        if i + 2 < pat.len() && pat[i + 1] == b'-' && pat[i + 2] != b']' {
            let hi = pat[i + 2];
            if c >= lo.min(hi) && c <= lo.max(hi) {
                found = true;
            }
            i += 3;
        } else {
            if c == lo {
                found = true;
            }
            i += 1;
        }
    }
    if i >= pat.len() {
        return None; // unterminated class
    }
    Some((found ^ negated, i + 1))
}

/// Canonicalize a host path and assert it is a directory.
fn canonical_dir(p: &Path) -> Result<PathBuf, SandboxError> {
    let canonical = p
        .canonicalize()
        .map_err(|e| SandboxError::Io(e.to_string()))?;
    if !canonical.is_dir() {
        return Err(SandboxError::NotADirectory(canonical.display().to_string()));
    }
    Ok(canonical)
}

/// Is `s` an absolute path in *virtual* space? Always `/`-prefixed,
/// regardless of host OS conventions — scripts are portable, so a Windows
/// host still uses `/tom/y`.
fn is_virtual_absolute(s: &str) -> bool {
    s.starts_with('/')
}

/// Normalize an absolute virtual path to the internal mount-key form: no
/// trailing slash, no `.` or empty segments, `..` collapsed. The virtual
/// root normalizes to the empty string so prefix joins concatenate cleanly.
///
/// Returns `None` if the path is not absolute or if `..` climbs above the
/// virtual root (there is nothing above it to reach).
fn normalize_virtual(s: &str) -> Option<String> {
    if !is_virtual_absolute(s) || s.contains('\0') {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                // Escaping above the virtual root is not an error to be
                // clamped away silently; there is no parent to go to.
                out.pop()?;
            }
            other => out.push(other),
        }
    }
    if out.is_empty() {
        return Some(String::new());
    }
    Some(format!("/{}", out.join("/")))
}

/// Strip a mount's virtual prefix from a normalized virtual path, returning
/// the remainder relative to that mount.
///
/// Matches only on whole segments, so the `/tomcat` mount does not capture
/// the path `/tom/y`.
fn virtual_strip_prefix<'a>(vpath: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        // Root mount: everything below `/` belongs to it.
        return Some(vpath.strip_prefix('/').unwrap_or(vpath));
    }
    let rest = vpath.strip_prefix(prefix)?;
    match rest.as_bytes().first() {
        None => Some(""),          // exact match: the mount point itself
        Some(b'/') => Some(&rest[1..]), // a path inside the mount
        _ => None,                 // partial segment match, e.g. /tomcat vs /tom
    }
}

/// Lexically normalize a path: collapse `.` and `..` without consulting the
/// filesystem. Leaves the original path intact if it contains no special
/// components.
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalize the longest existing prefix of `p` and append any
/// nonexistent tail. This lets us validate sandbox containment for paths
/// that don't (yet) exist while still honoring symlinks for the parts that
/// do.
fn canonicalize_existing_prefix(p: &Path) -> std::io::Result<PathBuf> {
    let mut current = p.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match current.canonicalize() {
            Ok(c) => {
                let mut out = c;
                for piece in tail.iter().rev() {
                    out.push(piece);
                }
                return Ok(out);
            }
            Err(_) => {
                let Some(parent) = current.parent().map(Path::to_path_buf) else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "no existing ancestor",
                    ));
                };
                let file_name = current
                    .file_name()
                    .map(|s| s.to_os_string())
                    .unwrap_or_else(|| std::ffi::OsString::from(""));
                tail.push(file_name);
                if parent.as_os_str().is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "no existing ancestor",
                    ));
                }
                current = parent;
            }
        }
    }
}

/// Tracker for include depth and cycle detection. The evaluator threads
/// one of these through nested includes.
#[derive(Debug, Default, Clone)]
pub struct IncludeStack {
    pub stack: Vec<PathBuf>,
    pub visited: HashSet<PathBuf>,
}

impl IncludeStack {
    pub fn enter(
        &mut self,
        path: PathBuf,
        max_depth: usize,
    ) -> Result<IncludeGuard<'_>, SandboxError> {
        if self.stack.len() >= max_depth {
            return Err(SandboxError::IncludeTooDeep { max: max_depth });
        }
        if !self.visited.insert(path.clone()) {
            return Err(SandboxError::IncludeCycle(path.display().to_string()));
        }
        self.stack.push(path);
        Ok(IncludeGuard { stack: self })
    }
}

/// RAII guard that pops the include stack when dropped.
pub struct IncludeGuard<'a> {
    stack: &'a mut IncludeStack,
}

impl<'a> Drop for IncludeGuard<'a> {
    fn drop(&mut self) {
        if let Some(top) = self.stack.stack.pop() {
            self.stack.visited.remove(&top);
        }
    }
}

/// Re-exports of this module's private, allocation-free helpers so the Kani
/// harnesses in `crate::verification` can reach them without widening the
/// public API. Compiled only under `cargo kani`.
#[cfg(kani)]
pub(crate) mod verification_shims {
    pub(crate) use super::{glob_match, match_class, virtual_strip_prefix};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_root() -> tempdir_like::Tmp {
        tempdir_like::Tmp::new()
    }

    // Minimal tempdir helper (no extra dep).
    mod tempdir_like {
        use std::path::PathBuf;
        pub struct Tmp(pub PathBuf);
        impl Tmp {
            pub fn new() -> Self {
                let mut p = std::env::temp_dir();
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                p.push(format!(
                    "trongo-sandbox-test-{}-{}",
                    std::process::id(),
                    nanos
                ));
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
        }
        impl Drop for Tmp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn absolute_paths_are_virtual_not_host() {
        let dir = tmp_root();
        let sb = Sandbox::new(&dir.0).unwrap();
        // `/etc/passwd` means "etc/passwd within the sandbox", so it stays
        // under the root and the host's real /etc is unreachable.
        let resolved = sb.resolve(None, "/etc/passwd").unwrap();
        assert!(resolved.starts_with(sb.root()));
        assert_ne!(resolved, Path::new("/etc/passwd"));
    }

    #[test]
    fn rejects_escape_via_dotdot() {
        let dir = tmp_root();
        let sb = Sandbox::new(&dir.0).unwrap();
        let err = sb.resolve(None, "../outside").unwrap_err();
        assert!(matches!(err, SandboxError::Escape(_)));
    }

    #[test]
    fn reads_files_under_root() {
        let dir = tmp_root();
        fs::write(dir.0.join("hello.txt"), "hi there").unwrap();
        let sb = Sandbox::new(&dir.0).unwrap();
        let txt = sb.read_text(None, "hello.txt").unwrap();
        assert_eq!(txt, "hi there");
    }

    #[test]
    fn lists_files_under_root() {
        let dir = tmp_root();
        fs::write(dir.0.join("a.txt"), "").unwrap();
        fs::write(dir.0.join("b.txt"), "").unwrap();
        let sb = Sandbox::new(&dir.0).unwrap();
        let entries = sb.list(None, ".").unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
    }

    #[test]
    fn glob_matches_files_by_extension() {
        let dir = tmp_root();
        fs::write(dir.0.join("a.yaml"), "").unwrap();
        fs::write(dir.0.join("b.yaml"), "").unwrap();
        fs::write(dir.0.join("c.txt"), "").unwrap();
        let sb = Sandbox::new(&dir.0).unwrap();
        let r = sb.glob(None, "*.yaml").unwrap();
        let paths: Vec<_> = r.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["a.yaml", "b.yaml"]);
    }

    #[test]
    fn glob_recursive_double_star() {
        let dir = tmp_root();
        fs::create_dir_all(dir.0.join("sub/deep")).unwrap();
        fs::write(dir.0.join("top.txt"), "").unwrap();
        fs::write(dir.0.join("sub/mid.txt"), "").unwrap();
        fs::write(dir.0.join("sub/deep/bottom.txt"), "").unwrap();
        let sb = Sandbox::new(&dir.0).unwrap();
        let r = sb.glob(None, "**/*.txt").unwrap();
        let paths: Vec<_> = r.iter().map(|m| m.path.as_str()).collect();
        assert!(paths.contains(&"top.txt"));
        assert!(paths.contains(&"sub/mid.txt"));
        assert!(paths.contains(&"sub/deep/bottom.txt"));
    }

    #[test]
    fn glob_char_class() {
        let dir = tmp_root();
        fs::write(dir.0.join("a.log"), "").unwrap();
        fs::write(dir.0.join("b.log"), "").unwrap();
        fs::write(dir.0.join("c.log"), "").unwrap();
        let sb = Sandbox::new(&dir.0).unwrap();
        let r = sb.glob(None, "[ab].log").unwrap();
        let paths: Vec<_> = r.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["a.log", "b.log"]);
    }

    #[test]
    fn glob_skips_dotfiles_by_default() {
        let dir = tmp_root();
        fs::write(dir.0.join("visible.yaml"), "").unwrap();
        fs::write(dir.0.join(".hidden.yaml"), "").unwrap();
        let sb = Sandbox::new(&dir.0).unwrap();
        let r = sb.glob(None, "*.yaml").unwrap();
        let paths: Vec<_> = r.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["visible.yaml"]);

        // But explicit leading-dot pattern picks them up.
        let r = sb.glob(None, ".*.yaml").unwrap();
        let paths: Vec<_> = r.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec![".hidden.yaml"]);
    }

    #[test]
    fn glob_absolute_pattern_is_virtual_not_host() {
        let dir = tmp_root();
        fs::create_dir_all(dir.0.join("etc")).unwrap();
        fs::write(dir.0.join("etc/passwd"), "sandboxed").unwrap();
        let sb = Sandbox::new(&dir.0).unwrap();
        // Matches the sandbox's own /etc/passwd, never the host's.
        let r = sb.glob(None, "/etc/*").unwrap();
        let paths: Vec<&str> = r.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["/etc/passwd"]);
        assert_eq!(sb.read_text(None, "/etc/passwd").unwrap(), "sandboxed");
    }

    #[test]
    fn glob_caps_results_at_max_list_entries() {
        let dir = tmp_root();
        for i in 0..50 {
            fs::write(dir.0.join(format!("f{}.txt", i)), "").unwrap();
        }
        let sb = Sandbox::new(&dir.0).unwrap().with_limits(SandboxLimits {
            max_list_entries: 10,
            ..SandboxLimits::default()
        });
        let r = sb.glob(None, "*.txt").unwrap();
        assert_eq!(r.len(), 10);
    }

    #[test]
    fn glob_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tmp_root();
        let outside = std::env::temp_dir().join(format!(
            "trongo-glob-outside-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&outside);
        std::fs::write(outside.join("secret.txt"), "shh").unwrap();
        symlink(&outside, dir.0.join("link")).unwrap();

        let sb = Sandbox::new(&dir.0).unwrap();
        let r = sb.glob(None, "**/*.txt").unwrap();
        let paths: Vec<_> = r.iter().map(|m| m.path.as_str()).collect();
        assert!(
            !paths.iter().any(|p| p.contains("secret")),
            "symlinked content leaked: {:?}",
            paths
        );

        let _ = std::fs::remove_dir_all(&outside);
    }

    // -----------------------------------------------------------------
    // Bind mounts.
    // -----------------------------------------------------------------

    /// Build `bob` (the root) and `tom` (a mount) as siblings under one
    /// temp dir, so `..` from inside tom would reach bob if containment
    /// were checked against the wrong boundary.
    fn bob_and_tom() -> (tempdir_like::Tmp, Sandbox) {
        let dir = tmp_root();
        fs::create_dir_all(dir.0.join("bob")).unwrap();
        fs::create_dir_all(dir.0.join("tom")).unwrap();
        fs::write(dir.0.join("bob/x"), "bob-x").unwrap();
        fs::write(dir.0.join("tom/y"), "tom-y").unwrap();
        let sb = Sandbox::new(dir.0.join("bob"))
            .unwrap()
            .mount("/tom", dir.0.join("tom"))
            .unwrap();
        (dir, sb)
    }

    #[test]
    fn mount_exposes_both_trees_at_virtual_paths() {
        let (_dir, sb) = bob_and_tom();
        // The user's stated goal: ./bob/x is /x, ./tom/y is /tom/y.
        assert_eq!(sb.read_text(None, "/x").unwrap(), "bob-x");
        assert_eq!(sb.read_text(None, "/tom/y").unwrap(), "tom-y");
    }

    #[test]
    fn relative_paths_still_work_without_mounts() {
        // Root-only sandbox keeps the pre-mount behavior.
        let dir = tmp_root();
        fs::write(dir.0.join("hello.txt"), "hi").unwrap();
        let sb = Sandbox::new(&dir.0).unwrap();
        assert_eq!(sb.read_text(None, "hello.txt").unwrap(), "hi");
        assert_eq!(sb.read_text(None, "/hello.txt").unwrap(), "hi");
    }

    #[test]
    fn dotdot_cannot_escape_a_mount_into_the_root() {
        let (_dir, sb) = bob_and_tom();
        let tom_dir = sb.resolve(None, "/tom").unwrap();
        // From inside /tom, `../bob/x` is a real host path — but it crosses
        // a mount boundary, so it must be refused.
        let err = sb.resolve(Some(&tom_dir), "../bob/x").unwrap_err();
        assert!(matches!(err, SandboxError::Escape(_)), "got {:?}", err);
    }

    #[test]
    fn dotdot_cannot_escape_the_root_mount() {
        let (_dir, sb) = bob_and_tom();
        let err = sb.resolve(None, "../tom/y").unwrap_err();
        assert!(matches!(err, SandboxError::Escape(_)), "got {:?}", err);
    }

    #[test]
    fn virtual_dotdot_cannot_climb_above_the_virtual_root() {
        let (_dir, sb) = bob_and_tom();
        let err = sb.resolve(None, "/../tom/y").unwrap_err();
        assert!(matches!(err, SandboxError::Escape(_)), "got {:?}", err);
    }

    #[test]
    fn absolute_path_does_not_reach_the_host_root() {
        let (_dir, sb) = bob_and_tom();
        // `/etc/passwd` is virtual: it means "etc/passwd inside the root
        // mount", not the host's /etc/passwd.
        let resolved = sb.resolve(None, "/etc/passwd").unwrap();
        assert!(resolved.starts_with(sb.root()));
        assert!(sb.read_text(None, "/etc/passwd").is_err());
    }

    #[test]
    fn mount_shadows_a_real_directory_of_the_same_name() {
        let dir = tmp_root();
        fs::create_dir_all(dir.0.join("bob/tom")).unwrap();
        fs::create_dir_all(dir.0.join("tom")).unwrap();
        // Same filename in both: whichever we read tells us who won.
        fs::write(dir.0.join("bob/tom/y"), "shadowed").unwrap();
        fs::write(dir.0.join("tom/y"), "mounted").unwrap();
        let sb = Sandbox::new(dir.0.join("bob"))
            .unwrap()
            .mount("/tom", dir.0.join("tom"))
            .unwrap();
        assert_eq!(sb.read_text(None, "/tom/y").unwrap(), "mounted");
    }

    #[test]
    fn mount_prefix_matches_whole_segments_only() {
        let dir = tmp_root();
        fs::create_dir_all(dir.0.join("root")).unwrap();
        fs::create_dir_all(dir.0.join("tomcat")).unwrap();
        fs::create_dir_all(dir.0.join("root/tom")).unwrap();
        fs::write(dir.0.join("tomcat/y"), "cat").unwrap();
        fs::write(dir.0.join("root/tom/y"), "plain").unwrap();
        let sb = Sandbox::new(dir.0.join("root"))
            .unwrap()
            .mount("/tomcat", dir.0.join("tomcat"))
            .unwrap();
        // `/tomcat` must not swallow `/tom/...`.
        assert_eq!(sb.read_text(None, "/tom/y").unwrap(), "plain");
        assert_eq!(sb.read_text(None, "/tomcat/y").unwrap(), "cat");
    }

    #[test]
    fn nested_mounts_resolve_deepest_first() {
        let dir = tmp_root();
        for d in ["root", "vendor", "vendor_lib"] {
            fs::create_dir_all(dir.0.join(d)).unwrap();
        }
        fs::write(dir.0.join("vendor/f"), "outer").unwrap();
        fs::write(dir.0.join("vendor_lib/f"), "inner").unwrap();
        let sb = Sandbox::new(dir.0.join("root"))
            .unwrap()
            .mount("/vendor", dir.0.join("vendor"))
            .unwrap()
            .mount("/vendor/lib", dir.0.join("vendor_lib"))
            .unwrap();
        assert_eq!(sb.read_text(None, "/vendor/f").unwrap(), "outer");
        assert_eq!(sb.read_text(None, "/vendor/lib/f").unwrap(), "inner");
    }

    #[test]
    fn symlink_out_of_a_mount_is_refused() {
        use std::os::unix::fs::symlink;
        let (dir, sb) = bob_and_tom();
        fs::write(dir.0.join("bob/secret"), "shh").unwrap();
        // A symlink inside the tom mount pointing into the root mount.
        symlink(dir.0.join("bob/secret"), dir.0.join("tom/link")).unwrap();
        let err = sb.read_text(None, "/tom/link").unwrap_err();
        assert!(matches!(err, SandboxError::Escape(_)), "got {:?}", err);
    }

    #[test]
    fn list_works_across_mounts() {
        let (_dir, sb) = bob_and_tom();
        let entries = sb.list(None, "/tom").unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["y"]);
    }

    #[test]
    fn glob_inside_a_mount_returns_addressable_paths() {
        let (dir, sb) = bob_and_tom();
        fs::write(dir.0.join("tom/a.yaml"), "").unwrap();
        fs::create_dir_all(dir.0.join("tom/sub")).unwrap();
        fs::write(dir.0.join("tom/sub/b.yaml"), "").unwrap();

        let r = sb.glob(None, "/tom/**/*.yaml").unwrap();
        let paths: Vec<&str> = r.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["/tom/a.yaml", "/tom/sub/b.yaml"]);

        // Results must round-trip back through read-file.
        for p in paths {
            sb.read_text(None, p)
                .unwrap_or_else(|e| panic!("glob result {} not readable: {}", p, e));
        }
    }

    #[test]
    fn glob_in_root_mount_keeps_relative_paths() {
        // Pre-mount behavior for root-anchored patterns is unchanged.
        let (dir, sb) = bob_and_tom();
        fs::write(dir.0.join("bob/a.yaml"), "").unwrap();
        let r = sb.glob(None, "*.yaml").unwrap();
        let paths: Vec<&str> = r.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["a.yaml"]);
    }

    #[test]
    fn glob_does_not_leak_across_mounts() {
        let (dir, sb) = bob_and_tom();
        fs::write(dir.0.join("bob/in-root.yaml"), "").unwrap();
        fs::write(dir.0.join("tom/in-tom.yaml"), "").unwrap();
        // A root-anchored recursive glob must not descend into other mounts;
        // they aren't inside the root's host tree.
        let r = sb.glob(None, "**/*.yaml").unwrap();
        let paths: Vec<&str> = r.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["in-root.yaml"]);
    }

    #[test]
    fn to_virtual_maps_host_paths_back() {
        let (dir, sb) = bob_and_tom();
        let tom_y = dir.0.join("tom/y").canonicalize().unwrap();
        assert_eq!(sb.to_virtual(&tom_y).as_deref(), Some("/tom/y"));
        let bob_x = dir.0.join("bob/x").canonicalize().unwrap();
        assert_eq!(sb.to_virtual(&bob_x).as_deref(), Some("/x"));
        assert_eq!(sb.to_virtual(&dir.0).as_deref(), None);
    }

    #[test]
    fn mount_rejects_relative_and_duplicate_points() {
        let (dir, sb) = bob_and_tom();
        let err = sb.clone().mount("tom", dir.0.join("tom")).unwrap_err();
        assert!(matches!(err, SandboxError::BadMountPoint(_)), "got {:?}", err);

        let err = sb.clone().mount("/tom", dir.0.join("tom")).unwrap_err();
        assert!(matches!(err, SandboxError::DuplicateMount(_)), "got {:?}", err);

        // Trailing slash is the same mount point, not a new one.
        let err = sb.clone().mount("/tom/", dir.0.join("tom")).unwrap_err();
        assert!(matches!(err, SandboxError::DuplicateMount(_)), "got {:?}", err);

        // Nonexistent host dir.
        let err = sb.clone().mount("/nope", dir.0.join("nope")).unwrap_err();
        assert!(matches!(err, SandboxError::Io(_)), "got {:?}", err);
    }

    #[test]
    fn mounting_at_slash_replaces_the_root() {
        let (dir, sb) = bob_and_tom();
        let sb = sb.mount("/", dir.0.join("tom")).unwrap();
        assert_eq!(sb.read_text(None, "/y").unwrap(), "tom-y");
        // And the old root is gone.
        assert!(sb.read_text(None, "/x").is_err());
    }

    #[test]
    fn null_byte_rejected_in_mounted_paths() {
        let (_dir, sb) = bob_and_tom();
        let err = sb.resolve(None, "/tom/\0y").unwrap_err();
        assert!(matches!(err, SandboxError::Escape(_)), "got {:?}", err);
    }

    #[test]
    fn size_limit_enforced() {
        let dir = tmp_root();
        fs::write(dir.0.join("big.bin"), vec![0u8; 100]).unwrap();
        let sb = Sandbox::new(&dir.0)
            .unwrap()
            .with_limits(SandboxLimits {
                max_file_bytes: 10,
                ..SandboxLimits::default()
            });
        let err = sb.read_text(None, "big.bin").unwrap_err();
        assert!(matches!(err, SandboxError::FileTooLarge { .. }));
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.