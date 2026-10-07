//! Resumable HTTP download with on-the-fly MD5 / SHA-1 / SHA-256 computation.

use crate::hashing::{hash_prefix, Hasher, Hashes, BUF_SIZE};
use crate::util::Progress;
use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
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

/// Download `req.url` to `req.dest`, resuming a previous `.part` file when the server supports ranges.
/// The returned hashes cover the complete file. A cancelled or interrupted download keeps its `.part`.
pub fn download(http: &Client, req: &DownloadRequest, progress: &Progress) -> Result<DownloadResult> {
    if let Some(dir) = req.dest.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    // Already complete: just hash it.
    if let Ok(m) = std::fs::metadata(req.dest) {
        if m.is_file() && (req.expected_size.is_none() || Some(m.len()) == req.expected_size) {
            let hashes = crate::hashing::hash_file(req.dest, progress)?;
            return Ok(DownloadResult { path: req.dest.to_path_buf(), bytes: m.len(), hashes, resumed_from: m.len() });
        }
    }
    let part = part_path(req.dest);
    let mut existing = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if let Some(exp) = req.expected_size {
        if existing >= exp {
            std::fs::remove_file(&part).ok();
            existing = 0;
        }
    }

    progress.set_stage("Connecting");
    let mut builder = http.get(req.url);
    if let Some(r) = req.referer {
        builder = builder.header(REFERER, r);
    }
    if existing > 0 {
        builder = builder.header(RANGE, format!("bytes={existing}-"));
    }
    let mut resp = builder.send().context("sending request")?;
    let status = resp.status();
    let resumed = match status {
        StatusCode::PARTIAL_CONTENT if existing > 0 => true,
        StatusCode::OK => false,
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
            bail!("server reports {total} bytes but the catalog lists {exp} bytes - link or catalog is stale");
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
    progress.set_stage("Downloading");

    let mut buf = vec![0u8; BUF_SIZE];
    let mut written = existing;
    loop {
        if progress.is_cancelled() {
            file.flush()?;
            bail!("cancelled - the partial file is kept for resuming");
        }
        let n = match resp.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                file.flush()?;
                return Err(e).context("connection interrupted - run the download again to resume");
            }
        };
        if n == 0 {
            break;
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
    if req.dest.exists() {
        std::fs::remove_file(req.dest).ok();
    }
    std::fs::rename(&part, req.dest).with_context(|| format!("renaming to {}", req.dest.display()))?;
    Ok(DownloadResult { path: req.dest.to_path_buf(), bytes: written, hashes: hasher.finalize(), resumed_from })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_path_appends_extension() {
        assert_eq!(part_path(Path::new("/x/a.esd")), PathBuf::from("/x/a.esd.part"));
    }
}
