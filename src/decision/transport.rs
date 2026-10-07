//! How a decision request travels.
//!
//! The protocol is one `POST` of JSON and a body in answer. Everything above
//! this file is about questions and answers; everything below is about bytes
//! on a wire. Keeping the two apart is what makes the layer reusable: a
//! different program can hand in its own `Transport` — a real HTTP client, a
//! recorded tape, a fake that never leaves the process — and keep every type
//! the rest of `decision` is built from.
//!
//! `Curl` is the transport omaread ships with. The reader builds as Rust only,
//! with no TLS stack and no C toolchain, and pulling one in for a side feature
//! was not worth it, so the request is handed to the curl on the machine. The
//! trade is a runtime dependency on curl; the shape of the trait is what makes
//! that easy to change later.

use anyhow::{Context, Result, bail};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Sends one decision request and returns the body it answered with.
pub trait Transport {
    /// `headers` are already complete, `Authorization` among them. A transport
    /// that can keep them out of a process list should: `Curl` does.
    fn post(
        &self,
        url: &str,
        body: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<String>;
}

/// The transport omaread ships with: curl, run as a child process.
pub struct Curl;

impl Transport for Curl {
    fn post(
        &self,
        url: &str,
        body: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<String> {
        let dir = scratch_dir()?;
        let headers_path = dir.join("headers");
        let body_path = dir.join("body.json");
        write_private(&headers_path, &header_lines(headers))?;
        write_private(&body_path, body)?;

        let seconds = timeout.as_secs().max(1).to_string();
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", &seconds])
            .args(["--request", "POST"])
            .arg(url)
            .arg("--header")
            .arg(format!("@{}", headers_path.display()))
            .arg("--data-binary")
            .arg(format!("@{}", body_path.display()))
            .output();
        // The scratch directory holds the key in a header file; it goes as soon
        // as curl has read it, whatever curl said.
        let _ = std::fs::remove_dir_all(&dir);

        let output = output.context(
            "cannot run curl: install it, or point omaread at a decision model another way",
        )?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr);
            bail!("curl failed: {}", message.trim());
        }
        String::from_utf8(output.stdout).context("the decision model answered with non-UTF-8 bytes")
    }
}

/// One `Name: value` per line, the form curl reads from `--header @file`.
fn header_lines(headers: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        // A header a file cannot hold is a header we must not send: curl would
        // read the rest of the line as another header.
        let value = value.replace(['\r', '\n'], "");
        let _ = writeln!(out, "{name}: {value}");
    }
    out
}

/// A private directory under the temp dir, unique to this call.
fn scratch_dir() -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "omaread-decision-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let dir = std::env::temp_dir().join(name);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot make {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("cannot lock {}", dir.display()))?;
    Ok(dir)
}

/// Writes a file only its owner can read, which is what a header file must be.
fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot make {}", path.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))
}
