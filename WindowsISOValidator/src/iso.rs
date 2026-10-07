//! Lightweight identification of ISO / WIM / ESD files without external tools.

use anyhow::{Context, Result};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaKind {
    Iso9660,
    Wim,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub file_size: u64,
    /// ISO volume label (ISO 9660 primary volume descriptor).
    pub volume_label: Option<String>,
    /// ISO volume size in bytes according to the descriptor.
    pub volume_size: Option<u64>,
}

impl MediaInfo {
    pub fn summary(&self) -> String {
        match self.kind {
            MediaKind::Iso9660 => {
                format!("ISO 9660 image, label {}", self.volume_label.as_deref().unwrap_or("(none)"))
            }
            MediaKind::Wim => "WIM / ESD image (Windows imaging format)".to_string(),
            MediaKind::Unknown => "Unrecognised file type".to_string(),
        }
    }
}

pub fn inspect(path: &Path) -> Result<MediaInfo> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let file_size = f.metadata()?.len();
    let mut head = [0u8; 8];
    let n = f.read(&mut head)?;
    if n >= 8 && &head[..8] == b"MSWIM\0\0\0" {
        return Ok(MediaInfo { kind: MediaKind::Wim, file_size, volume_label: None, volume_size: None });
    }
    // ISO 9660: primary volume descriptor at sector 16 (offset 0x8000).
    if file_size >= 0x8000 + 2048 {
        f.seek(SeekFrom::Start(0x8000))?;
        let mut pvd = [0u8; 2048];
        f.read_exact(&mut pvd)?;
        if pvd[0] == 1 && &pvd[1..6] == b"CD001" {
            let label = String::from_utf8_lossy(&pvd[40..72]).trim_end().to_string();
            let blocks = u32::from_le_bytes([pvd[80], pvd[81], pvd[82], pvd[83]]) as u64;
            let block_size = u16::from_le_bytes([pvd[128], pvd[129]]) as u64;
            return Ok(MediaInfo {
                kind: MediaKind::Iso9660,
                file_size,
                volume_label: if label.is_empty() { None } else { Some(label) },
                volume_size: if block_size > 0 { Some(blocks * block_size) } else { None },
            });
        }
    }
    Ok(MediaInfo { kind: MediaKind::Unknown, file_size, volume_label: None, volume_size: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_iso_label() {
        let dir = std::env::temp_dir().join(format!("wiv_iso_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut data = vec![0u8; 0x8000 + 4096];
        data[0x8000] = 1;
        data[0x8001..0x8006].copy_from_slice(b"CD001");
        let label = b"CCCOMA_X64FRE_EN-US_DV9            ";
        data[0x8000 + 40..0x8000 + 40 + 32].copy_from_slice(&label[..32]);
        data[0x8000 + 80..0x8000 + 84].copy_from_slice(&100u32.to_le_bytes());
        data[0x8000 + 128..0x8000 + 130].copy_from_slice(&2048u16.to_le_bytes());
        let p = dir.join("t.iso");
        std::fs::write(&p, &data).unwrap();
        let info = inspect(&p).unwrap();
        assert_eq!(info.kind, MediaKind::Iso9660);
        assert_eq!(info.volume_label.as_deref(), Some("CCCOMA_X64FRE_EN-US_DV9"));
        assert_eq!(info.volume_size, Some(204800));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detects_wim() {
        let dir = std::env::temp_dir().join(format!("wiv_wim_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.esd");
        std::fs::write(&p, b"MSWIM\0\0\0 rest of header").unwrap();
        assert_eq!(inspect(&p).unwrap().kind, MediaKind::Wim);
        std::fs::remove_dir_all(&dir).ok();
    }
}
