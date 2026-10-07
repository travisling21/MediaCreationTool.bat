//! Parser for the embedded MediaCreationTool.bat.
//!
//! The script is the single source of truth for version choices, Microsoft download links and the
//! condensed ESD link tables. Parsing it at runtime keeps the GUI in sync with the script it bundles.

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use std::sync::OnceLock;

/// The bundled script, byte for byte (CRLF line endings preserved).
pub const SCRIPT: &str = include_str!("../../MediaCreationTool.bat");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionChoice {
    /// Index used by the script's `:choice-N` labels and the `N.P` elevation token.
    pub index: u32,
    /// Base build number, e.g. 26300.
    pub ver: u32,
    /// Version id, e.g. `11_26H2` or `22H2`.
    pub vid: String,
    /// Full ESD build string, e.g. `26300.9457.260913-1737.26h2_ge_release_svc_refresh`.
    pub cb: String,
    /// Release folder used by the Windows Update hosted links, e.g. `2026/09/`.
    pub ct: String,
    /// Catalog schema version the script writes into products.xml.
    pub cc: String,
    pub cab: Option<String>,
    pub xml: Option<String>,
    pub exe: Option<String>,
    /// The script author's one-line note after `goto process`.
    pub note: String,
}

impl VersionChoice {
    pub fn is_windows_11(&self) -> bool {
        self.ver >= 22000
    }
    /// Human friendly label such as `Windows 11 26H2 (26300.9457)`.
    pub fn label(&self) -> String {
        let family = if self.is_windows_11() { "Windows 11" } else { "Windows 10" };
        let release = self.vid.trim_start_matches("11_");
        let build = self.cb.split('.').take(2).collect::<Vec<_>>().join(".");
        format!("{family} {release} ({build})")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UupRow {
    pub ver: u32,
    /// `ret` (consumer), `vol` (business) or `chn` (China consumer).
    pub client: String,
    pub lang: String,
    pub size: u64,
    pub sha256: String,
    pub guid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessRow {
    pub ver: u32,
    pub client: String,
    pub lang: String,
    pub size_x64: Option<u64>,
    pub size_x86: Option<u64>,
    pub sha1_x64: String,
    pub sha1_x86: String,
    pub dir_x64: String,
    pub dir_x86: String,
}

#[derive(Debug, Clone)]
pub struct Script {
    /// Dialog order of version ids (the `VERSIONS` variable).
    pub versions: Vec<String>,
    /// Default choice index (`dV`).
    pub default_index: u32,
    /// Version choices, newest first.
    pub choices: Vec<VersionChoice>,
    /// Command-line aliases accepted by the script, e.g. (20, "26H2").
    pub aliases: Vec<(u32, String)>,
    pub uup: Vec<UupRow>,
    pub business: Vec<BusinessRow>,
    /// Date stamp from the changelog header line.
    pub changelog: String,
}

impl Script {
    pub fn choice_by_vid(&self, vid: &str) -> Option<&VersionChoice> {
        self.choices.iter().find(|c| c.vid.eq_ignore_ascii_case(vid))
    }
    pub fn choice_by_index(&self, index: u32) -> Option<&VersionChoice> {
        self.choices.iter().find(|c| c.index == index)
    }
    pub fn uup_rows(&self, ver: u32) -> Vec<&UupRow> {
        self.uup.iter().filter(|r| r.ver == ver).collect()
    }
    pub fn business_rows(&self, ver: u32) -> Vec<&BusinessRow> {
        self.business.iter().filter(|r| r.ver == ver).collect()
    }
}

static PARSED: OnceLock<Script> = OnceLock::new();

/// Parse the embedded script once.
pub fn script() -> &'static Script {
    PARSED.get_or_init(|| parse(SCRIPT).expect("embedded MediaCreationTool.bat must parse"))
}

pub fn parse(text: &str) -> Result<Script> {
    let text = text.replace("\r\n", "\n");

    let versions_line = text
        .lines()
        .find(|l| l.starts_with("set VERSIONS="))
        .ok_or_else(|| anyhow!("VERSIONS line not found"))?;
    let versions: Vec<String> =
        versions_line["set VERSIONS=".len()..].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

    let default_index = text
        .lines()
        .find_map(|l| l.strip_prefix("set /a dV=").map(|v| v.trim().parse::<u32>().ok()))
        .flatten()
        .ok_or_else(|| anyhow!("dV not found"))?;

    let changelog = text
        .lines()
        .find_map(|l| l.strip_prefix(":: Changelog: ").map(|s| s.trim().to_string()))
        .unwrap_or_default();

    // Alias map: `for %%V in (1.1507 2.1511 ... 20.11_26H2) do for %%s in`
    let aliases = {
        let start = text.find("for %%V in (").ok_or_else(|| anyhow!("alias map not found"))?;
        let rest = &text[start + "for %%V in (".len()..];
        let end = rest.find(") do").ok_or_else(|| anyhow!("alias map end not found"))?;
        rest[..end]
            .split_whitespace()
            .filter_map(|tok| {
                let (idx, alias) = tok.split_once('.')?;
                Some((idx.parse::<u32>().ok()?, alias.to_string()))
            })
            .collect::<Vec<_>>()
    };

    // :choice-N blocks
    let set_re = Regex::new(r#"set "([A-Z0-9]+)=([^"]*)""#).unwrap();
    let label_re = Regex::new(r"(?m)^:choice-(\d+)\s*$").unwrap();
    let mut choices = Vec::new();
    let labels: Vec<(usize, usize, u32)> =
        label_re.captures_iter(&text).map(|c| (c.get(0).unwrap().start(), c.get(0).unwrap().end(), c[1].parse().unwrap())).collect();
    for (i, (_, end, index)) in labels.iter().enumerate() {
        let block_end = labels.get(i + 1).map(|l| l.0).unwrap_or(text.len());
        let block = &text[*end..block_end];
        let mut ver = None;
        let mut vid = None;
        let mut cb = None;
        let mut ct = None;
        let mut cc = None;
        let mut cab = None;
        let mut xml = None;
        let mut exe = None;
        let mut note = String::new();
        for line in block.lines() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with(':') {
                break;
            }
            if trimmed.starts_with("rem ") {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("goto process") {
                note = rest.trim().trim_start_matches("::#").trim().to_string();
                break;
            }
            // Conditional assignments are only taken when they are the script's defaults
            // (INSERT_BUSINESS=1); other conditionals describe host specific overrides.
            if trimmed.starts_with("if ") && !trimmed.starts_with("if %INSERT_BUSINESS%0 gtr 1") {
                continue;
            }
            for cap in set_re.captures_iter(trimmed) {
                let value = cap[2].to_string();
                match &cap[1] {
                    "VER" => ver = value.parse::<u32>().ok(),
                    "VID" => vid = Some(value),
                    "CB" => cb = Some(value),
                    "CT" => ct = Some(value),
                    "CC" => cc = Some(value),
                    "CAB" => cab = Some(value),
                    "XML" => xml = Some(value),
                    "EXE" => exe = Some(value),
                    _ => {}
                }
            }
        }
        let (Some(ver), Some(vid), Some(cb)) = (ver, vid, cb) else {
            continue;
        };
        choices.push(VersionChoice {
            index: *index,
            ver,
            vid,
            cb,
            ct: ct.unwrap_or_default(),
            cc: cc.unwrap_or_default(),
            cab,
            xml,
            exe,
            note,
        });
    }
    choices.sort_by_key(|c| std::cmp::Reverse(c.index));
    if choices.is_empty() {
        return Err(anyhow!("no :choice- blocks found"));
    }

    let uup = parse_uup(&text).context("parsing UUP csv")?;
    let business = parse_business(&text).context("parsing business csv")?;

    Ok(Script { versions, default_index, choices, aliases, uup, business, changelog })
}

fn csv_block<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    // The block ends at the next separator line or at the end of the file.
    let end = rest.find("\n::----").unwrap_or(rest.len());
    Some(&rest[..end])
}

fn parse_uup(text: &str) -> Result<Vec<UupRow>> {
    let Some(block) = csv_block(text, ":PS_INSERT_UUP_CSV:") else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for line in block.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() != 7 || f[0] != "::#" {
            continue;
        }
        rows.push(UupRow {
            ver: f[1].parse().with_context(|| format!("bad UUP row {line}"))?,
            client: f[2].to_string(),
            lang: f[3].to_string(),
            size: f[4].parse().with_context(|| format!("bad UUP size in {line}"))?,
            sha256: f[5].to_ascii_lowercase(),
            guid: f[6].to_string(),
        });
    }
    Ok(rows)
}

fn parse_business(text: &str) -> Result<Vec<BusinessRow>> {
    let Some(block) = csv_block(text, ":PS_INSERT_BUSINESS_CSV:") else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for line in block.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() != 10 || f[0] != "::#" {
            continue;
        }
        // The script maps the historical `sr-rs` code to the code Microsoft uses in the catalog.
        let lang = if f[3] == "sr-rs" { "sr-latn-rs".to_string() } else { f[3].to_string() };
        rows.push(BusinessRow {
            ver: f[1].parse().with_context(|| format!("bad business row {line}"))?,
            client: f[2].to_string(),
            lang,
            size_x64: f[4].parse().ok(),
            size_x86: f[5].parse().ok(),
            sha1_x64: f[6].to_ascii_lowercase(),
            sha1_x86: f[7].to_ascii_lowercase(),
            dir_x64: f[8].to_string(),
            dir_x86: f[9].to_string(),
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_embedded_script() {
        let s = script();
        assert!(s.choices.len() >= 20, "expected at least 20 choices, got {}", s.choices.len());
        assert_eq!(s.versions.len() as u32, s.choices.iter().map(|c| c.index).max().unwrap());
        assert_eq!(s.default_index, 20);

        let c20 = s.choice_by_index(20).expect("choice 20");
        assert_eq!(c20.vid, "11_26H2");
        assert_eq!(c20.ver, 26300);
        assert!(c20.cb.starts_with("26300."));
        assert_eq!(c20.cc, "2.1");
        assert!(c20.cab.as_deref().unwrap_or("").contains("Products-Win11-24H2"));
        assert!(c20.exe.as_deref().unwrap_or("").ends_with("MediaCreationTool.exe"));

        let c18 = s.choice_by_vid("11_24H2").expect("24H2");
        assert_eq!(c18.ver, 26100);
        assert!(c18.cab.is_some());

        let c1 = s.choice_by_index(1).expect("choice 1");
        assert_eq!(c1.ver, 10240);
        assert!(c1.xml.is_some());
        // the final unconditional EXE assignment wins
        assert!(c1.exe.as_deref().unwrap().contains("CF9862F9"));

        // INSERT_BUSINESS defaults apply: 21H1 uses the refreshed build
        let c12 = s.choice_by_vid("21H1").expect("21H1");
        assert!(c12.cb.starts_with("19043.1348"), "{}", c12.cb);

        assert!(s.aliases.contains(&(20, "26H2".to_string())));
        assert!(s.aliases.contains(&(20, "11_26H2".to_string())));
        assert!(s.aliases.contains(&(18, "24H2".to_string())));

        assert_eq!(s.uup_rows(26200).len(), 77);
        assert_eq!(s.uup_rows(26300).len(), 77);
        let en = s.uup.iter().find(|r| r.ver == 26300 && r.client == "ret" && r.lang == "en-us").unwrap();
        assert_eq!(en.sha256.len(), 64);
        assert_eq!(en.guid.len(), 36);
        assert!(en.size > 5_000_000_000);

        assert!(s.business_rows(19043).len() >= 70);
        assert!(s.business.iter().any(|r| r.lang == "sr-latn-rs"));
        assert!(!s.business.iter().any(|r| r.lang == "sr-rs"));
    }

    #[test]
    fn labels_are_friendly() {
        let s = script();
        assert_eq!(s.choice_by_index(20).unwrap().label(), "Windows 11 26H2 (26300.9457)");
        assert_eq!(s.choice_by_vid("22H2").unwrap().label(), "Windows 10 22H2 (19045.2965)");
    }
}
