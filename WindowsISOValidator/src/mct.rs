//! Driving the bundled MediaCreationTool.bat (Windows only at run time; the rest compiles everywhere).

use crate::bat::SCRIPT;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    AutoUpgrade = 1,
    AutoIso = 2,
    AutoUsb = 3,
    Select = 4,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::AutoIso, Preset::AutoUsb, Preset::AutoUpgrade, Preset::Select];
    pub fn label(self) -> &'static str {
        match self {
            Preset::AutoUpgrade => "Auto Upgrade (upgrade this PC in place)",
            Preset::AutoIso => "Auto ISO (write the ISO into the work folder)",
            Preset::AutoUsb => "Auto USB (pick the USB drive in the MCT window)",
            Preset::Select => "Select (choose edition / language / arch in the MCT window)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct McTask {
    /// `:choice-N` index from the script.
    pub choice_index: u32,
    pub preset: Preset,
    pub edition: String,
    pub lang: String,
    pub arch: String,
    /// `def`: create untouched MCT media (no bypass, no auto.cmd).
    pub def: bool,
    /// `no_update`: disable dynamic update during setup.
    pub no_update: bool,
}

/// Command line understood by the script: the `N.P` token the script itself uses to carry its GUI
/// choice through self-elevation, followed by optional edition / language / arch / flags.
pub fn build_args(t: &McTask) -> Vec<String> {
    let mut v = vec![format!("{}.{}", t.choice_index, t.preset as u8)];
    for s in [&t.edition, &t.lang, &t.arch] {
        if !s.trim().is_empty() {
            v.push(s.trim().to_string());
        }
    }
    if t.def {
        v.push("def".into());
    }
    if t.no_update {
        v.push("no_update".into());
    }
    v
}

pub fn script_path(work_dir: &Path) -> PathBuf {
    work_dir.join("MediaCreationTool.bat")
}

/// Write the embedded script into the work folder (always refreshed so it matches this build).
pub fn write_script(work_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(work_dir).with_context(|| format!("creating {}", work_dir.display()))?;
    let path = script_path(work_dir);
    std::fs::write(&path, SCRIPT.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Launch the script in its own console window. It asks for elevation itself.
#[cfg(windows)]
pub fn launch(work_dir: &Path, args: &[String]) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let bat = write_script(work_dir)?;
    let mut cmd = std::process::Command::new("cmd.exe");
    cmd.arg("/d").arg("/c").arg("start").arg("MediaCreationTool").arg("/d").arg(work_dir).arg(&bat);
    for a in args {
        cmd.arg(a);
    }
    cmd.current_dir(work_dir).creation_flags(CREATE_NO_WINDOW);
    cmd.spawn().context("starting cmd.exe")?;
    Ok(())
}

#[cfg(not(windows))]
pub fn launch(work_dir: &Path, _args: &[String]) -> Result<()> {
    let _ = write_script(work_dir)?;
    anyhow::bail!("MediaCreationTool.bat can only run on Windows")
}

#[derive(Debug, Clone)]
pub struct IsoFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Default)]
pub struct McStatus {
    pub isos: Vec<IsoFile>,
    pub setup_running: bool,
    pub esd_dir_present: bool,
}

pub fn status(work_dir: &Path) -> McStatus {
    let mut isos = Vec::new();
    if let Ok(rd) = std::fs::read_dir(work_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("iso")).unwrap_or(false) {
                if let Ok(m) = e.metadata() {
                    isos.push(IsoFile { path: p, size: m.len(), modified: m.modified().ok() });
                }
            }
        }
    }
    isos.sort_by_key(|f| std::cmp::Reverse(f.modified));
    McStatus { isos, setup_running: setup_running(), esd_dir_present: esd_dir().map(|p| p.exists()).unwrap_or(false) }
}

pub fn esd_dir() -> Option<PathBuf> {
    let drive = std::env::var("SystemDrive").ok()?;
    Some(PathBuf::from(format!("{drive}\\ESD\\MCT")))
}

#[cfg(windows)]
fn setup_running() -> bool {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("tasklist.exe")
        .args(["/fi", "imagename eq SetupHost.exe", "/nh"])
        .creation_flags(0x0800_0000)
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_ascii_lowercase().contains("setuphost.exe"),
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn setup_running() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_script_arguments() {
        let t = McTask {
            choice_index: 20,
            preset: Preset::AutoIso,
            edition: "Pro".into(),
            lang: "en-US".into(),
            arch: "x64".into(),
            def: false,
            no_update: true,
        };
        assert_eq!(build_args(&t), vec!["20.2", "Pro", "en-US", "x64", "no_update"]);
        let t2 = McTask { preset: Preset::AutoUpgrade, edition: String::new(), lang: String::new(), arch: String::new(), def: true, no_update: false, ..t };
        assert_eq!(build_args(&t2), vec!["20.1", "def"]);
    }

    #[test]
    fn writes_the_embedded_script_unchanged() {
        let dir = std::env::temp_dir().join(format!("wiv_mct_test_{}", std::process::id()));
        let p = write_script(&dir).unwrap();
        let written = std::fs::read(&p).unwrap();
        assert_eq!(written, SCRIPT.as_bytes());
        assert!(written.windows(2).filter(|w| w == b"\r\n").count() > 1000, "CRLF endings must survive embedding");
        std::fs::remove_dir_all(&dir).ok();
    }
}
