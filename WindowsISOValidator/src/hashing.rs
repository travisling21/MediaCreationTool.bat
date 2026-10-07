//! Streaming MD5 / SHA-1 / SHA-256 computation with progress and cancellation.

use crate::util::Progress;
use anyhow::{bail, Context, Result};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hashes {
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
}

/// Incremental hasher computing all three digests in one pass.
pub struct Hasher {
    md5: Md5,
    sha1: Sha1,
    sha256: Sha256,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    pub fn new() -> Self {
        Self { md5: Md5::new(), sha1: Sha1::new(), sha256: Sha256::new() }
    }
    pub fn update(&mut self, data: &[u8]) {
        self.md5.update(data);
        self.sha1.update(data);
        self.sha256.update(data);
    }
    pub fn finalize(self) -> Hashes {
        Hashes {
            md5: hex::encode(self.md5.finalize()),
            sha1: hex::encode(self.sha1.finalize()),
            sha256: hex::encode(self.sha256.finalize()),
        }
    }
}

pub const BUF_SIZE: usize = 1 << 20;

/// Hash a whole file, reporting progress and honouring cancellation.
pub fn hash_file(path: &Path, progress: &Progress) -> Result<Hashes> {
    let mut file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let len = file.metadata()?.len();
    progress.start(len);
    progress.set_stage(format!("Hashing {}", path.file_name().and_then(|s| s.to_str()).unwrap_or("file")));
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        if progress.is_cancelled() {
            bail!("cancelled");
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        progress.add(n as u64);
    }
    Ok(hasher.finalize())
}

/// Hash only the first `limit` bytes of a file (used to resume a partial download with correct digests).
pub fn hash_prefix(path: &Path, limit: u64, progress: &Progress) -> Result<Hasher> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; BUF_SIZE];
    let mut remaining = limit;
    while remaining > 0 {
        if progress.is_cancelled() {
            bail!("cancelled");
        }
        let want = remaining.min(BUF_SIZE as u64) as usize;
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            bail!("partial file is shorter than expected");
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
        progress.add(n as u64);
    }
    Ok(hasher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        let mut h = Hasher::new();
        h.update(b"abc");
        let r = h.finalize();
        assert_eq!(r.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(r.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(r.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn hashes_a_file_with_progress() {
        let dir = std::env::temp_dir().join(format!("wiv_hash_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("abc.bin");
        std::fs::write(&p, b"abc").unwrap();
        let progress = Progress::default();
        let r = hash_file(&p, &progress).unwrap();
        assert_eq!(r.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(progress.done.load(std::sync::atomic::Ordering::Relaxed), 3);
        std::fs::remove_dir_all(&dir).ok();
    }
}
