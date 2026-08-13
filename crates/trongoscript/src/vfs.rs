//! The filesystem backend a [`Sandbox`](crate::sandbox::Sandbox) reads through.
//!
//! Everything the sandbox asks a filesystem for goes through one trait,
//! [`FileStore`]: does this path exist and what is it, how big is it, give me
//! its text, list a directory. Two backends ship here — [`RealFs`], which is
//! `std::fs` and is what every existing embedder gets by default, and
//! [`MemoryFs`], a `BTreeMap` of path to contents.
//!
//! `MemoryFs` exists so a host with no filesystem at all — a browser, a
//! `wasm32-unknown-unknown` build — can still run documents that `include`
//! each other. That is only possible because path *resolution* in the sandbox
//! is already almost purely lexical; the one exception is symlink
//! canonicalization, which is why [`FileStore::canonicalize_existing_prefix`]
//! is on the trait rather than being a free function the sandbox calls
//! directly.
//!
//! ```
//! use std::sync::Arc;
//! use trongoscript::vfs::{FileStore, MemoryFs, MEMORY_FS_ROOT};
//!
//! let mut fs = MemoryFs::new();
//! fs.insert(format!("{MEMORY_FS_ROOT}/hello.trongo"), "(+ 1 2)");
//!
//! let store: Arc<dyn FileStore> = Arc::new(fs);
//! assert_eq!(
//!     store.read_to_string(std::path::Path::new("/bundle/hello.trongo")).unwrap(),
//!     "(+ 1 2)",
//! );
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Lexically normalize a path: collapse `.` and `..` without consulting the
/// filesystem. Leaves the original path intact if it contains no special
/// components.
pub(crate) fn normalize(p: &Path) -> PathBuf {
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

/// What a [`FileStore`] knows about one path.
///
/// `len` is only meaningful when `is_file` is set — the sandbox reads it to
/// enforce [`SandboxLimits::max_file_bytes`](crate::sandbox::SandboxLimits),
/// and a directory's size is not a thing scripts can observe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    /// Whether the path names a regular file.
    pub is_file: bool,
    /// Whether the path names a directory.
    pub is_dir: bool,
    /// Size in bytes, for a regular file.
    pub len: u64,
}

/// One entry in a directory listing.
///
/// Symlinks never appear: filtering them out is the *backend's*
/// responsibility, so that "the store never reports a symlink" is an
/// invariant of the store rather than a check the sandbox has to remember to
/// repeat at each walk site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's file name, without any directory part.
    pub name: String,
    /// Whether the entry is itself a directory.
    pub is_dir: bool,
}

/// A read-only filesystem backend.
///
/// The trait is deliberately object-safe — the sandbox holds an
/// `Arc<dyn FileStore>` so cloning it stays cheap — so keep every method
/// taking `&self` and returning owned values. A generic method here would
/// silently make the trait un-object-safe.
///
/// `Send + Sync` because a [`Sandbox`](crate::sandbox::Sandbox) is `Clone`
/// and may cross threads on a native host. wasm32 is single-threaded, so the
/// bound costs nothing there.
pub trait FileStore: fmt::Debug + Send + Sync {
    /// Report what `path` is.
    ///
    /// # Errors
    ///
    /// A missing path must surface as [`io::ErrorKind::NotFound`]: the
    /// sandbox maps exactly that kind to
    /// [`SandboxError::NotFound`](crate::sandbox::SandboxError::NotFound) and
    /// everything else to
    /// [`SandboxError::Io`](crate::sandbox::SandboxError::Io).
    fn metadata(&self, path: &Path) -> io::Result<FileMeta>;

    /// Read `path`'s contents as UTF-8.
    ///
    /// # Errors
    ///
    /// Any I/O failure, including a missing path or invalid UTF-8.
    fn read_to_string(&self, path: &Path) -> io::Result<String>;

    /// List `path`'s immediate children, excluding symlinks.
    ///
    /// The whole listing is materialized rather than streamed, which
    /// collapses a mid-iteration failure into one honest `io::Result`. Every
    /// directory a sandbox walks is small, so the buffering is free.
    /// Ordering is not promised — the sandbox sorts what it gets.
    ///
    /// # Errors
    ///
    /// A missing path, or a path that is not a directory.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;

    /// Resolve symlinks in the longest existing prefix of `path`.
    ///
    /// This is the one method that exists purely because the real filesystem
    /// has symlinks. A store with no symlinks — an in-memory one — returns
    /// the lexically normalized path unchanged, which is both correct and the
    /// whole reason the browser backend works at all.
    ///
    /// The result is used for containment checking, not existence checking,
    /// so a path that does not exist must still come back rather than
    /// erroring, as long as *some* ancestor of it does.
    ///
    /// # Errors
    ///
    /// If no ancestor of `path` exists at all.
    fn canonicalize_existing_prefix(&self, path: &Path) -> io::Result<PathBuf>;
}

// ---------------------------------------------------------------------------
// RealFs
// ---------------------------------------------------------------------------

/// The `std::fs` backend — what [`Sandbox::new`](crate::sandbox::Sandbox::new)
/// uses, and what every embedder got before the trait existed.
///
/// # Examples
///
/// ```
/// use trongoscript::vfs::{FileStore, RealFs};
///
/// let meta = RealFs.metadata(std::path::Path::new("."));
/// assert!(meta.unwrap().is_dir);
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

impl FileStore for RealFs {
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        let meta = std::fs::metadata(path)?;
        Ok(FileMeta {
            is_file: meta.is_file(),
            is_dir: meta.is_dir(),
            len: meta.len(),
        })
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            // Skip symlinks defensively — following them could escape the
            // root via a TOCTOU race. The sandbox relies on this: it no
            // longer re-checks at its own walk sites.
            if ft.is_symlink() {
                continue;
            }
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                is_dir: ft.is_dir(),
            });
        }
        Ok(out)
    }

    fn canonicalize_existing_prefix(&self, path: &Path) -> io::Result<PathBuf> {
        canonicalize_existing_prefix(path)
    }
}

/// Canonicalize the longest existing prefix of `p` and append any
/// nonexistent tail. This lets us validate sandbox containment for paths
/// that don't (yet) exist while still honoring symlinks for the parts that
/// do.
fn canonicalize_existing_prefix(p: &Path) -> io::Result<PathBuf> {
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
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "no existing ancestor",
                    ));
                };
                let file_name = current
                    .file_name()
                    .map(|s| s.to_os_string())
                    .unwrap_or_else(|| std::ffi::OsString::from(""));
                tail.push(file_name);
                if parent.as_os_str().is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "no existing ancestor",
                    ));
                }
                current = parent;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryFs
// ---------------------------------------------------------------------------

/// The synthetic root path under which [`MemoryFs`] conventionally stores its
/// keys. Represents a bundle's top-level directory in sandbox-space.
///
/// Nothing enforces it — a `MemoryFs` will happily serve keys under any
/// absolute prefix — but a host that picks one root and states it in one place
/// is a host whose paths can be audited.
pub const MEMORY_FS_ROOT: &str = "/bundle";

/// A [`FileStore`] over a `BTreeMap` of path to contents.
///
/// `BTreeMap` rather than `HashMap` deliberately: [`FileStore::read_dir`] has
/// to find every key under a prefix, and sorted iteration makes that a range
/// scan instead of a full sweep. It also makes listings deterministically
/// ordered, which is what keeps a store-backed render reproducible.
///
/// **Directories are implied, never stored.** A path is a directory if any
/// key is strictly beneath it. A host handing over a flat path→contents map —
/// which is what a database projection or an upload naturally is — does not
/// have to synthesize directory rows that could drift out of sync with the
/// files themselves.
///
/// # Examples
///
/// ```
/// use trongoscript::vfs::{FileStore, MemoryFs};
///
/// let mut fs = MemoryFs::new();
/// fs.insert("/bundle/render/body.trongo", "{:kind \"circle\"}");
///
/// let entries = fs.read_dir(std::path::Path::new("/bundle")).unwrap();
/// assert_eq!(entries[0].name, "render");
/// assert!(entries[0].is_dir);
/// ```
#[derive(Debug, Default, Clone)]
pub struct MemoryFs {
    files: BTreeMap<PathBuf, String>,
}

impl MemoryFs {
    /// An empty store.
    ///
    /// Note that an empty store has no root: `/bundle` is a directory only
    /// once some key sits beneath it, so
    /// [`Sandbox::with_store`](crate::sandbox::Sandbox::with_store) rejects
    /// one until at least one file is inserted. A bundle with no files is an
    /// error anyway.
    ///
    /// # Examples
    ///
    /// ```
    /// use trongoscript::vfs::MemoryFs;
    ///
    /// let fs = MemoryFs::new();
    /// assert!(fs.is_empty());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `contents` at `path`, replacing whatever was there.
    ///
    /// The path is lexically normalized first, so
    /// `/bundle/render/../render/body.trongo` and `/bundle/render/body.trongo`
    /// land on the same key. That matters for include-cycle detection, which
    /// keys on the resolved path: two spellings of one file must collapse, or
    /// a cycle could slip through.
    ///
    /// # Examples
    ///
    /// ```
    /// use trongoscript::vfs::{FileStore, MemoryFs};
    ///
    /// let mut fs = MemoryFs::new();
    /// fs.insert("/bundle/./a.trongo", "first");
    /// fs.insert("/bundle/b/../a.trongo", "second");
    /// assert_eq!(fs.len(), 1);
    /// assert_eq!(fs.read_to_string(std::path::Path::new("/bundle/a.trongo")).unwrap(), "second");
    /// ```
    pub fn insert(&mut self, path: impl Into<PathBuf>, contents: impl Into<String>) {
        self.files.insert(normalize(&path.into()), contents.into());
    }

    /// Number of files stored.
    ///
    /// # Examples
    ///
    /// ```
    /// use trongoscript::vfs::MemoryFs;
    ///
    /// let mut fs = MemoryFs::new();
    /// fs.insert("/bundle/a.trongo", "");
    /// assert_eq!(fs.len(), 1);
    /// ```
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the store holds no files.
    ///
    /// # Examples
    ///
    /// ```
    /// use trongoscript::vfs::MemoryFs;
    ///
    /// assert!(MemoryFs::new().is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Every stored path, in sorted order.
    ///
    /// # Examples
    ///
    /// ```
    /// use trongoscript::vfs::MemoryFs;
    ///
    /// let mut fs = MemoryFs::new();
    /// fs.insert("/bundle/b.trongo", "");
    /// fs.insert("/bundle/a.trongo", "");
    /// let names: Vec<_> = fs.paths().map(|p| p.to_string_lossy().to_string()).collect();
    /// assert_eq!(names, ["/bundle/a.trongo", "/bundle/b.trongo"]);
    /// ```
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.files.keys().map(PathBuf::as_path)
    }

    /// Whether any key sits strictly beneath `path`, which is what makes
    /// `path` an implied directory.
    fn has_children(&self, path: &Path) -> bool {
        self.children(path).next().is_some()
    }

    /// Every key strictly beneath `path`, paired with its remainder.
    ///
    /// This is the range scan `BTreeMap` buys: keys sort lexicographically,
    /// so everything under a prefix is contiguous, and the iteration can stop
    /// at the first key that no longer matches.
    fn children<'a>(&'a self, path: &'a Path) -> impl Iterator<Item = (&'a Path, &'a Path)> + 'a {
        self.files
            .range(path.to_path_buf()..)
            .map_while(move |(key, _)| {
                key.strip_prefix(path)
                    .ok()
                    .map(|rest| (key.as_path(), rest))
            })
            .filter(|(_, rest)| rest.as_os_str() != std::ffi::OsStr::new(""))
    }
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("path not found in store: {}", path.display()),
    )
}

impl FileStore for MemoryFs {
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        let path = normalize(path);
        // An exact key wins over implied-directory status: a store should
        // never hold a file whose path is also a prefix of another file, and
        // if it somehow does, the file is the more specific answer.
        if let Some(contents) = self.files.get(&path) {
            return Ok(FileMeta {
                is_file: true,
                is_dir: false,
                len: contents.len() as u64,
            });
        }
        if self.has_children(&path) {
            return Ok(FileMeta {
                is_file: false,
                is_dir: true,
                len: 0,
            });
        }
        Err(not_found(&path))
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let path = normalize(path);
        match self.files.get(&path) {
            Some(contents) => Ok(contents.clone()),
            None if self.has_children(&path) => Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                format!("is a directory: {}", path.display()),
            )),
            None => Err(not_found(&path)),
        }
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let path = normalize(path);
        if self.files.contains_key(&path) {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("not a directory: {}", path.display()),
            ));
        }

        let mut out: Vec<DirEntry> = Vec::new();
        for (_, rest) in self.children(&path) {
            let mut segments = rest.components();
            let Some(first) = segments.next() else {
                continue;
            };
            let name = first.as_os_str().to_string_lossy().to_string();
            // More components left means this segment is an implied
            // subdirectory rather than a leaf file.
            let is_dir = segments.next().is_some();
            if out.last().is_some_and(|e| e.name == name) {
                continue;
            }
            out.push(DirEntry { name, is_dir });
        }

        if out.is_empty() {
            return Err(not_found(&path));
        }
        // Sorted key order gives sorted names already, but children of a
        // directory and children of its siblings interleave under `..`-laden
        // spellings, so sort rather than assume.
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out.dedup_by(|a, b| a.name == b.name);
        Ok(out)
    }

    fn canonicalize_existing_prefix(&self, path: &Path) -> io::Result<PathBuf> {
        // No symlinks exist here, so the lexically normalized path *is* the
        // canonical one. This is not a shortcut that weakens the sandbox:
        // symlink resolution exists to defeat escapes *through symlinks*, and
        // a store with no symlinks has no such escape. The `..`-climbing
        // escapes are still caught, because `resolve` checks containment
        // against the normalized result.
        Ok(normalize(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> MemoryFs {
        let mut fs = MemoryFs::new();
        fs.insert("/bundle/skeleton.trongo", "skeleton");
        fs.insert("/bundle/render/body.render.trongo", "body");
        fs.insert("/bundle/render/fin.render.trongo", "fin");
        fs
    }

    #[test]
    fn reads_a_stored_file() {
        assert_eq!(
            store()
                .read_to_string(Path::new("/bundle/skeleton.trongo"))
                .unwrap(),
            "skeleton"
        );
    }

    #[test]
    fn missing_file_is_not_found() {
        let err = store()
            .read_to_string(Path::new("/bundle/nope.trongo"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn reading_an_implied_directory_says_so() {
        let err = store()
            .read_to_string(Path::new("/bundle/render"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::IsADirectory);
    }

    #[test]
    fn read_dir_lists_an_implied_directory() {
        let entries = store().read_dir(Path::new("/bundle/render")).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["body.render.trongo", "fin.render.trongo"]);
        assert!(entries.iter().all(|e| !e.is_dir));
    }

    #[test]
    fn read_dir_mixes_files_and_implied_subdirectories() {
        let entries = store().read_dir(Path::new("/bundle")).unwrap();
        assert_eq!(
            entries,
            vec![
                DirEntry {
                    name: "render".into(),
                    is_dir: true
                },
                DirEntry {
                    name: "skeleton.trongo".into(),
                    is_dir: false
                },
            ]
        );
    }

    #[test]
    fn read_dir_on_a_file_is_not_a_directory() {
        let err = store()
            .read_dir(Path::new("/bundle/skeleton.trongo"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotADirectory);
    }

    #[test]
    fn read_dir_on_nothing_is_not_found() {
        let err = store().read_dir(Path::new("/bundle/nope")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn metadata_distinguishes_file_from_implied_directory() {
        let fs = store();
        let file = fs.metadata(Path::new("/bundle/skeleton.trongo")).unwrap();
        assert!(file.is_file && !file.is_dir && file.len == 8);

        let dir = fs.metadata(Path::new("/bundle/render")).unwrap();
        assert!(dir.is_dir && !dir.is_file && dir.len == 0);

        assert_eq!(
            fs.metadata(Path::new("/bundle/nope")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn insert_normalizes_so_two_spellings_are_one_key() {
        let mut fs = MemoryFs::new();
        fs.insert("/bundle/./a.trongo", "first");
        fs.insert("/bundle/render/../a.trongo", "second");
        assert_eq!(fs.len(), 1);
        assert_eq!(
            fs.read_to_string(Path::new("/bundle/a.trongo")).unwrap(),
            "second"
        );
    }

    #[test]
    fn canonicalize_normalizes_even_a_missing_path() {
        let got = store()
            .canonicalize_existing_prefix(Path::new("/bundle/render/../nope.trongo"))
            .unwrap();
        assert_eq!(got, Path::new("/bundle/nope.trongo"));
    }

    #[test]
    fn an_empty_store_has_no_root() {
        let err = MemoryFs::new()
            .metadata(Path::new(MEMORY_FS_ROOT))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn read_dir_does_not_leak_a_sibling_with_a_shared_prefix() {
        // `/bundle/render2` shares a string prefix with `/bundle/render` but
        // is not beneath it. A prefix comparison done on strings rather than
        // path components would wrongly pick it up.
        let mut fs = store();
        fs.insert("/bundle/render2/x.trongo", "x");
        let names: Vec<String> = fs
            .read_dir(Path::new("/bundle/render"))
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["body.render.trongo", "fin.render.trongo"]);
    }

    #[test]
    fn real_fs_reads_and_lists() {
        let dir = std::env::temp_dir().join(format!("trongo-vfs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hi").unwrap();

        assert_eq!(RealFs.read_to_string(&dir.join("a.txt")).unwrap(), "hi");
        assert!(RealFs.metadata(&dir).unwrap().is_dir);
        assert_eq!(RealFs.read_dir(&dir).unwrap()[0].name, "a.txt");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
