//! Resumable HTTP download with on-the-fly MD5 / SHA-1 / SHA-256 computation.

use crate::hashing::{hash_file, hash_prefix, Hasher, Hashes, BUF_SIZE};
use crate::util::Progress;
use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, Response};
use reqwest::header::{CONTENT_RANGE, RANGE, REFERER};
use reqwest::StatusCode;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub path: PathBuf,
    pub bytes: u64,
    pub hashes: Hashes,
    pub resumed_from: u64,
    /// The destination already existed with the expected size and was only hashed, not fetched.
    pub reused_existing: bool,
}

pub struct DownloadRequest<'a> {
    pub url: &'a str,
    pub dest: &'a Path,
    pub expected_size: Option<u64>,
    pub referer: Option<&'a str>,
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// Maximum consecutive read timeouts (each `DL_READ_TIMEOUT_SECS`) before a stalled transfer is abandoned.
pub const MAX_STALLS: u32 = 6;
/// Per-read timeout configured on the download client; keeps Cancel responsive on stalled connections.
pub const DL_READ_TIMEOUT_SECS: u64 = 20;

fn is_timeout(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::TimedOut {
        return true;
    }
    e.get_ref().and_then(|inner| inner.downcast_ref::<reqwest::Error>()).map(|r| r.is_timeout()).unwrap_or(false)
}

fn send(http: &Client, req: &DownloadRequest, range_from: u64) -> Result<Response> {
    let mut builder = http.get(req.url);
    if let Some(r) = req.referer {
        builder = builder.header(REFERER, r);
    }
    if range_from > 0 {
        builder = builder.header(RANGE, format!("bytes={range_from}-"));
    }
    builder.send().context("sending request")
}

fn finish(part: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        std::fs::remove_file(dest).with_context(|| format!("replacing {}", dest.display()))?;
    }
    std::fs::rename(part, dest).with_context(|| format!("renaming to {}", dest.display()))
}

/// Download `req.url` to `req.dest`, resuming a previous `.part` file when the server supports ranges.
/// The returned hashes cover the complete file. A cancelled or interrupted download keeps its `.part`.
pub fn download(http: &Client, req: &DownloadRequest, progress: &Progress) -> Result<DownloadResult> {
    if let Some(dir) = req.dest.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    // A finished file with the right size is only hashed. A finished file with another size is stale
    // (Microsoft reuses ISO names across refreshes) and gets replaced.
    if let Ok(m) = std::fs::metadata(req.dest) {
        if m.is_file() {
            match req.expected_size {
                Some(exp) if exp != m.len() => {
                    progress.set_stage("Replacing a stale file with a different size");
                    std::fs::remove_file(req.dest).with_context(|| format!("removing stale {}", req.dest.display()))?;
                }
                _ => {
                    let hashes = hash_file(req.dest, progress)?;
                    return Ok(DownloadResult {
                        path: req.dest.to_path_buf(),
                        bytes: m.len(),
                        hashes,
                        resumed_from: m.len(),
                        reused_existing: true,
                    });
                }
            }
        }
    }
    let part = part_path(req.dest);
    let mut existing = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if let Some(exp) = req.expected_size {
        if existing == exp {
            // The previous attempt fetched everything but did not get to rename.
            let hashes = hash_file(&part, progress)?;
            finish(&part, req.dest)?;
            return Ok(DownloadResult { path: req.dest.to_path_buf(), bytes: exp, hashes, resumed_from: exp, reused_existing: false });
        }
        if existing > exp {
            std::fs::remove_file(&part).ok();
            existing = 0;
        }
    }

    progress.set_stage("Connecting");
    let mut resp = send(http, req, existing)?;
    if resp.status() == StatusCode::RANGE_NOT_SATISFIABLE && existing > 0 {
        // The partial file is longer than what the server has now: start over.
        progress.set_stage("Partial file no longer matches the server, restarting");
        std::fs::remove_file(&part).ok();
        existing = 0;
        resp = send(http, req, 0)?;
    }
    let status = resp.status();
    let resumed = match status {
        StatusCode::PARTIAL_CONTENT if existing > 0 => true,
        s if s.is_success() => false,
        s => bail!("server answered {s}"),
    };
    let total = if resumed {
        resp.headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next().and_then(|t| t.parse::<u64>().ok()))
            .or(req.expected_size)
            .unwrap_or(0)
    } else {
        resp.content_length().or(req.expected_size).unwrap_or(0)
    };
    if let (Some(exp), true) = (req.expected_size, total > 0) {
        if exp != total {
            bail!("server reports {total} bytes but {exp} bytes were expected - link or catalog is stale");
        }
    }

    let mut file;
    let mut hasher;
    if resumed {
        progress.start(total);
        progress.set_stage("Verifying the partial download");
        hasher = hash_prefix(&part, existing, progress).context("re-hashing partial file")?;
        file = std::fs::OpenOptions::new().append(true).open(&part)?;
        file.seek(SeekFrom::End(0))?;
    } else {
        existing = 0;
        progress.start(total);
        hasher = Hasher::new();
        file = std::fs::File::create(&part)?;
    }
    let resumed_from = existing;
    progress.mark_rate_base();
    progress.set_stage("Downloading");

    let mut buf = vec![0u8; BUF_SIZE];
    let mut written = existing;
    let mut stalls = 0u32;
    loop {
        if progress.is_cancelled() {
            file.flush()?;
            bail!("cancelled - the partial file is kept for resuming");
        }
        let n = match resp.read(&mut buf) {
            Ok(n) => n,
            Err(e) if is_timeout(&e) => {
                stalls += 1;
                if progress.is_cancelled() {
                    file.flush()?;
                    bail!("cancelled - the partial file is kept for resuming");
                }
                if stalls >= MAX_STALLS {
                    file.flush()?;
                    bail!("no data received for {} seconds - run the download again to resume", MAX_STALLS as u64 * DL_READ_TIMEOUT_SECS);
                }
                progress.set_stage(format!("Waiting for data ({stalls})"));
                continue;
            }
            Err(e) => {
                file.flush()?;
                return Err(e).context("connection interrupted - run the download again to resume");
            }
        };
        if n == 0 {
            break;
        }
        if stalls > 0 {
            stalls = 0;
            progress.set_stage("Downloading");
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        written += n as u64;
        progress.add(n as u64);
    }
    file.flush()?;
    drop(file);
    if total > 0 && written != total {
        bail!("connection closed after {written} of {total} bytes - run the download again to resume");
    }
    finish(&part, req.dest)?;
    Ok(DownloadResult { path: req.dest.to_path_buf(), bytes: written, hashes: hasher.finalize(), resumed_from, reused_existing: false })
}

/// Size announced by the server for a URL (HEAD request), when it tells us.
pub fn remote_size(http: &Client, url: &str, referer: Option<&str>) -> Option<u64> {
    let mut b = http.head(url);
    if let Some(r) = referer {
        b = b.header(REFERER, r);
    }
    let resp = b.send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.content_length().filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_path_appends_extension() {
        assert_eq!(part_path(Path::new("/x/a.esd")), PathBuf::from("/x/a.esd.part"));
    }

    #[test]
    fn timeout_detection() {
        assert!(is_timeout(&std::io::Error::new(std::io::ErrorKind::TimedOut, "x")));
        assert!(!is_timeout(&std::io::Error::new(std::io::ErrorKind::Other, "x")));
    }
}
