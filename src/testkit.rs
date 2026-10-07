//! Scratch paths for tests.
//!
//! A test that writes files leaves them in the temp dir. A name that does not
//! change between runs goes wrong twice: two `cargo test` runs share the name
//! and delete each other's fixtures, and every run leaves its files behind.
//! `Scratch` gives a test a directory named for this run and removes it when the
//! test ends, however it ends; `path` gives a file name the same way.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A path under the temp dir, unique to this run so two runs cannot collide.
fn unique(tag: &str, extension: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let name = format!(
        "omaread-{tag}-{}-{}{extension}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    );
    std::env::temp_dir().join(name)
}

/// A temporary file path. The caller removes the file; the name is unique, so a
/// leftover cannot be mistaken for another run's fixture.
pub fn path(tag: &str, extension: &str) -> PathBuf {
    unique(tag, extension)
}

/// A temporary directory, removed when the value is dropped.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Self {
        let dir = unique(tag, "");
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl std::ops::Deref for Scratch {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
