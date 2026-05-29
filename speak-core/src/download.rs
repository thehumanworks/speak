//! Ad-hoc model download from Hugging Face.
//!
//! The Supertonic-3 weights are not bundled in the binary; they live in a
//! per-user cache (see [`crate::ModelLocator`]). When files are missing this
//! module fetches them from the public Hugging Face repo, into the exact cache
//! layout the rest of the crate expects, so the first run "just works" without
//! a separate setup step. All progress is written to stderr.

use std::fs;
use std::fs::TryLockError;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::{ModelLocator, BUILTIN_VOICES, REQUIRED_ONNX_FILES};

/// The Hugging Face repository holding the Supertonic-3 weights.
const MODEL_REPO: &str = "Supertone/supertonic-3";

/// Pinned model revision: an immutable commit, not a moving branch, so every
/// build fetches byte-identical weights and an upstream change can never alter
/// behavior silently.
///
/// To update the model version, replace this with a newer commit hash. Find one
/// at <https://huggingface.co/Supertone/supertonic-3/commits/main> or via
/// `git ls-remote https://huggingface.co/Supertone/supertonic-3 HEAD`. If a new
/// revision adds or renames files, also update `REQUIRED_ONNX_FILES` /
/// `BUILTIN_VOICES` in lib.rs, which drive the download manifest below.
const MODEL_REVISION: &str = "3cadd1ee6394adea1bd021217a0e650ede09a323";

/// Raw-file download URL for a path relative to the cache base. The remote path
/// equals the cache-relative path, so this is a direct mapping.
fn file_url(rel: &str) -> String {
    format!("https://huggingface.co/{MODEL_REPO}/resolve/{MODEL_REVISION}/{rel}")
}

/// Build the list of files that make up a complete install, as paths relative
/// to the cache base directory, from the same constants the loader validates
/// against. One source of truth means the download set and the required set
/// cannot drift apart.
fn manifest() -> Vec<String> {
    let mut files: Vec<String> = REQUIRED_ONNX_FILES
        .iter()
        .map(|f| format!("onnx/{f}"))
        .collect();
    files.extend(BUILTIN_VOICES.iter().map(|v| format!("voice_styles/{v}.json")));
    files
}

/// Manifest files not yet present in the cache.
fn missing_files(locator: &ModelLocator) -> Vec<String> {
    let base = locator.base();
    manifest()
        .into_iter()
        .filter(|rel| !base.join(rel).exists())
        .collect()
}

/// Download any missing model files into the cache. A no-op (and silent) when
/// everything is already present, so it is cheap to call on every run.
pub fn ensure_models(locator: &ModelLocator) -> Result<()> {
    // Fast path: a warm cache needs neither a lock nor the network.
    if missing_files(locator).is_empty() {
        return Ok(());
    }

    let base = locator.base();
    fs::create_dir_all(base)
        .with_context(|| format!("creating cache directory {}", base.display()))?;

    // Serialize cold starts so two processes don't fetch the same files at
    // once. Best-effort: if locking can't be set up we proceed anyway (worst
    // case is redundant downloads, never corruption). Held until this returns.
    let _lock = acquire_download_lock(base);

    // Re-check under the lock: a process we waited on may have done the work.
    let missing = missing_files(locator);
    if missing.is_empty() {
        return Ok(());
    }

    eprintln!(
        "Supertonic models not found in cache; downloading {} file(s) from Hugging Face to {} ...",
        missing.len(),
        base.display()
    );

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .redirects(10)
        .build();

    for rel in &missing {
        let dest = base.join(rel);
        download_file(&agent, &file_url(rel), &dest, rel)
            .with_context(|| format!("failed to download {rel}"))?;
    }

    eprintln!("Model download complete.");
    Ok(())
}

/// Take an exclusive, cross-process advisory lock on a lock file in the cache.
/// The returned handle holds the lock until it drops, which the OS guarantees
/// even if the process crashes. Returns `None` if a lock can't be set up, so
/// the caller proceeds unsynchronized rather than failing. Uses std file
/// locking, stable since Rust 1.89.
fn acquire_download_lock(base: &Path) -> Option<fs::File> {
    let lock_path = base.join(".download.lock");
    let file = fs::File::create(&lock_path).ok()?;
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(TryLockError::WouldBlock) => {
            eprintln!("Waiting for another process to finish downloading the model ...");
            file.lock().ok().map(|()| file)
        }
        // Locking unsupported on this filesystem; carry on without it.
        Err(TryLockError::Error(_)) => None,
    }
}

/// Download one file to `dest`, streaming to a sibling temp file first and
/// renaming into place so a partial download never looks like a complete one.
fn download_file(agent: &ureq::Agent, url: &str, dest: &Path, label: &str) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    let resp = agent.get(url).call().with_context(|| format!("GET {url}"))?;
    let total: Option<u64> = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok());

    let file_name = dest
        .file_name()
        .context("download destination has no file name")?
        .to_string_lossy()
        .into_owned();
    let tmp = dest.with_file_name(format!(".{file_name}.part"));

    let mut reader = resp.into_reader();
    let written = {
        let mut out = fs::File::create(&tmp)
            .with_context(|| format!("creating temp file {}", tmp.display()))?;
        let n = copy_with_progress(&mut reader, &mut out, total, label)?;
        out.flush().ok();
        out.sync_all().ok();
        n
    };

    if let Some(expected) = total {
        if written != expected {
            let _ = fs::remove_file(&tmp);
            bail!("incomplete download: got {written} of {expected} bytes");
        }
    }

    fs::rename(&tmp, dest)
        .with_context(|| format!("moving {} into place", tmp.display()))?;
    Ok(())
}

/// Stream `reader` into `writer`, reporting progress on stderr. On a terminal
/// this is a single updating line per file; otherwise it prints one line when
/// the file finishes so logs stay readable.
fn copy_with_progress(
    reader: &mut impl Read,
    writer: &mut impl Write,
    total: Option<u64>,
    label: &str,
) -> Result<u64> {
    let tty = io::stderr().is_terminal();
    let mut buf = vec![0u8; 256 * 1024];
    let mut written: u64 = 0;
    let mut last_drawn: u64 = 0;

    loop {
        let n = reader.read(&mut buf).context("reading response body")?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .context("writing to cache file")?;
        written += n as u64;
        if tty && written - last_drawn >= 1 << 20 {
            draw_progress(label, written, total);
            last_drawn = written;
        }
    }

    if tty {
        draw_progress(label, written, total);
        eprintln!();
    } else {
        eprintln!("  {label}  {}", human(written));
    }
    Ok(written)
}

/// Render a single carriage-return progress line to stderr.
fn draw_progress(label: &str, written: u64, total: Option<u64>) {
    match total {
        Some(total) if total > 0 => {
            let pct = (written as f64 / total as f64 * 100.0).round() as u64;
            eprint!(
                "\r  {label}  {pct:>3}%  ({}/{})   ",
                human(written),
                human(total)
            );
        }
        _ => eprint!("\r  {label}  {}   ", human(written)),
    }
    let _ = io::stderr().flush();
}

/// Human-readable byte count (MiB for anything non-trivial).
fn human(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= MIB {
        format!("{:.1} MB", b / MIB)
    } else if b >= KIB {
        format!("{:.0} KB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}
