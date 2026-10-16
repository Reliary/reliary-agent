//! Tee file storage for full output recovery.
//!
//! When sift compresses output, the LLM sees the compressed version. If
//! compression loses important detail, the LLM can `cat` the saved tee file
//! to recover the raw output. This is RTK's "tee" pattern — save full output
//! to disk, reference it in the compressed output.
//!
//! Cache safety: tee files are content-addressed by SHA-256 hash of the raw
//! text. Identical input → identical path. No TTL by default; tee files
//! persist until user cleans them up via `reliary-agent clean`.

use std::io::Write;
use std::path::PathBuf;

const TEE_DIR: &str = "/tmp/reliary-tee";

/// V14: Save raw output to a tee file. Returns the file path.
pub fn save_tee(raw: &str) -> std::io::Result<Option<String>> {
    if raw.is_empty() { return Ok(None); }
    // Opt-out: RELIARY_NO_TEE=1 disables tee writing (saves disk).
    if std::env::var("RELIARY_NO_TEE").is_ok_and(|v| v == "1") { return Ok(None); }

    let hash = sha256_hex(raw);
    let dir = PathBuf::from(TEE_DIR);
    // Tee files contain raw command output, which can include secrets (env
    // dumps, auth headers). On Unix create the directory 0700 so other local
    // users cannot read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&hash);
    // Write atomically: tmp file then rename.
    let tmp_path = dir.join(format!(".{}.tmp", hash));
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(raw.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, &path)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// V14: Read a tee file by hash.
/// F5 hardening: gated to `TEE_DIR.join(basename)` — an absolute or
/// `/`-containing input can no longer read arbitrary files. 10MB cap so a
/// huge tee file cannot OOM the reader.
pub fn read_tee(hash_or_path: &str) -> std::io::Result<String> {
    // Basename only: strip any directory the caller smuggled in.
    let base: &str = hash_or_path.rsplit('/').next().unwrap_or(hash_or_path);
    let base: &str = base.rsplit('\\').next().unwrap_or(base);
    if base.is_empty() || base == "." || base == ".." || base.starts_with('.') {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid tee name"));
    }
    // Hashes are hex; refuse anything else (no traversal alphabet at all).
    if !base.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "tee name must be hex"));
    }
    let path = PathBuf::from(TEE_DIR).join(base);
    let meta = std::fs::metadata(&path)?;
    if meta.len() > 10 * 1024 * 1024 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "tee file too large"));
    }
    std::fs::read_to_string(&path)
}

/// V14: List all tee files with sizes (for `reliary-agent tee --list`).
#[allow(dead_code)]
pub fn list_tee_files() -> std::io::Result<Vec<(String, u64)>> {
    let dir = PathBuf::from(TEE_DIR);
    if !dir.exists() { return Ok(Vec::new()); }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') { continue; } // skip tmp files
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push((name, size));
    }
    out.sort();
    Ok(out)
}

/// V14: Remove all tee files (for `reliary-agent clean`).
pub fn clean_tee() -> std::io::Result<u64> {
    let dir = PathBuf::from(TEE_DIR);
    if !dir.exists() { return Ok(0); }
    let mut removed = 0u64;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') { continue; }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(entry.path()).is_ok() { removed += size; }
    }
    Ok(removed)
}

/// Content address for a tee file. SHA-256 — matches the doc comment and is
/// stable across processes (DefaultHasher is not guaranteed to be).
fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}