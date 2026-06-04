//! Filesystem sandbox.
//!
//! Trongoscript has **no** ambient filesystem access. To allow scripts to
//! `include` other Trongoscript files or to call `read-file`/`list-files`,
//! the embedding program must construct a [`Sandbox`] rooted at a specific
//! directory and pass it to the evaluator.
//!
//! The sandbox enforces:
//!   - All paths are resolved relative to the sandbox root.
//!   - Symbolic links are resolved via canonicalization, and the resolved
//!     real path must remain inside the sandbox root.
//!   - Absolute paths supplied by scripts are rejected; everything must be
//!     relative.
//!   - Path components `..` are allowed only insofar as they don't escape
//!     the root after resolution.
//!   - Per-call recursion depth and total bytes read are capped to defang
//!     pathological `include` chains and huge files.
//!
//! Even when a sandbox is provided, **listing or reading is the *only*
//! filesystem access available** — no writing, no executing, no network.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

/// A configured filesystem sandbox.
#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
    limits: SandboxLimits,
}

impl Sandbox {
    /// Create a sandbox rooted at `root`. The path is canonicalized; if it
    /// does not exist or is not a directory the function returns an error.
    pub fn new<P: AsRef<Path>>(root: P) -> Result<Self, SandboxError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|e| SandboxError::Io(e.to_string()))?;
        if !root.is_dir() {
            return Err(SandboxError::NotADirectory(root.display().to_string()));
        }
        Ok(Self {
            root,
            limits: SandboxLimits::default(),
        })
    }

    pub fn with_limits(mut self, limits: SandboxLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn limits(&self) -> &SandboxLimits {
        &self.limits
    }

    /// Resolve a script-supplied path against the sandbox root, asserting
    /// that the resolved path stays inside the root.
    ///
    /// `base` is the directory of the file the script is running from
    /// (so that relative includes work). If `base` is `None`, the sandbox
    /// root is used.
    pub fn resolve(&self, base: Option<&Path>, requested: &str) -> Result<PathBuf, SandboxError> {
        let req_path = Path::new(requested);
        if req_path.is_absolute() {
            return Err(SandboxError::AbsolutePath(requested.to_string()));
        }
        if requested.contains('\0') {
            return Err(SandboxError::Escape(requested.to_string()));
        }

        let starting = base.unwrap_or(&self.root);
        let mut candidate = starting.to_path_buf();
        candidate.push(req_path);

        // Normalize: collapse `.` and `..` lexically first so we don't trip
        // a `NotFound` from canonicalize() before checking containment.
        let normalized = normalize(&candidate);

        // Canonicalize the longest existing prefix to handle symlinks.
        let resolved = canonicalize_existing_prefix(&normalized)
            .map_err(|e| SandboxError::Io(e.to_string()))?;

        if !resolved.starts_with(&self.root) {
            return Err(SandboxError::Escape(requested.to_string()));
        }
        Ok(resolved)
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
    /// Path relative to the sandbox root, using `/` separators.
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
    /// The pattern is relative; `base` plays the same role it does in
    /// [`Self::resolve`]. Symlinks encountered during the walk are skipped.
    /// Returned paths are relative to the sandbox root and use `/` as the
    /// component separator regardless of host OS.
    pub fn glob(
        &self,
        base: Option<&Path>,
        pattern: &str,
    ) -> Result<Vec<GlobMatch>, SandboxError> {
        if pattern.is_empty() {
            return Ok(Vec::new());
        }
        if Path::new(pattern).is_absolute() || pattern.starts_with('/') {
            return Err(SandboxError::AbsolutePath(pattern.to_string()));
        }
        if pattern.contains('\0') {
            return Err(SandboxError::Escape(pattern.to_string()));
        }
        // Resolve the *starting directory* — the longest prefix of the
        // pattern that has no glob metacharacters. Anything fancier
        // (including `..`) is handled by the path resolver.
        let (literal_prefix, glob_tail) = split_literal_prefix(pattern);
        let start_dir = if literal_prefix.is_empty() {
            // Anchor at base or root.
            match base {
                Some(p) => p.to_path_buf(),
                None => self.root.clone(),
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

        // Compute the relative-prefix string so emitted paths can be
        // expressed relative to the sandbox root.
        let rel_prefix = start_dir
            .strip_prefix(&self.root)
            .map(|p| path_to_forward_slash(p))
            .unwrap_or_default();

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
    let mut literal = String::new();
    let mut rest_iter = pattern.split('/').peekable();
    while let Some(seg) = rest_iter.peek().copied() {
        if seg.contains('*') || seg.contains('?') || seg.contains('[') || seg == ".." {
            break;
        }
        let _ = rest_iter.next();
        if !literal.is_empty() {
            literal.push('/');
        }
        literal.push_str(seg);
    }
    let tail: Vec<&str> = rest_iter.collect();
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
    fn rejects_absolute_paths() {
        let dir = tmp_root();
        let sb = Sandbox::new(&dir.0).unwrap();
        let err = sb.resolve(None, "/etc/passwd").unwrap_err();
        assert!(matches!(err, SandboxError::AbsolutePath(_)));
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
    fn glob_rejects_absolute_pattern() {
        let dir = tmp_root();
        let sb = Sandbox::new(&dir.0).unwrap();
        let err = sb.glob(None, "/etc/passwd").unwrap_err();
        assert!(matches!(err, SandboxError::AbsolutePath(_)));
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