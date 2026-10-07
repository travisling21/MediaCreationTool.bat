//! Microsoft Media Creation Tool catalogs (products.cab / products.xml), processed the same way
//! MediaCreationTool.bat processes them so the GUI lists exactly the ESDs the script would use.

use crate::bat::{Script, VersionChoice};
use crate::util::Progress;
use anyhow::{anyhow, bail, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const DELIVERY_BASE: &str = "http://dl.delivery.mp.microsoft.com/filestreamingservice/files/";
pub const WU_BASE: &str = "http://b1.download.windowsupdate.com/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsdEntry {
    pub file_name: String,
    pub lang_code: String,
    pub language: String,
    pub edition: String,
    pub arch: String,
    pub size: u64,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub url: String,
    pub edition_loc: String,
    pub retail_only: bool,
}

impl EsdEntry {
    /// Consumer / Business / China, derived from the ESD file name like the script does.
    pub fn channel(&self) -> &'static str {
        let n = self.file_name.to_ascii_uppercase();
        if n.contains("_CLIENTBUSINESS_") {
            "Business"
        } else if n.contains("_CLIENTCHINA_") {
            "China"
        } else if n.contains("_CLIENTCONSUMER_") {
            "Consumer"
        } else {
            "Other"
        }
    }
    pub fn best_hash(&self) -> Option<(&'static str, &str)> {
        if let Some(h) = &self.sha256 {
            Some(("SHA-256", h.as_str()))
        } else {
            self.sha1.as_deref().map(|h| ("SHA-1", h))
        }
    }
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub vid: String,
    pub ver: u32,
    pub label: String,
    pub catalog_version: String,
    pub entries: Vec<EsdEntry>,
    pub source: String,
    pub notes: Vec<String>,
}

impl Catalog {
    pub fn languages(&self) -> Vec<String> {
        let mut v: Vec<String> = self.entries.iter().map(|e| e.lang_code.clone()).collect();
        v.sort();
        v.dedup();
        v
    }
    /// Distinct downloadable files (many editions share one ESD).
    pub fn distinct_files(&self) -> Vec<&EsdEntry> {
        let mut seen = std::collections::HashSet::new();
        self.entries.iter().filter(|e| seen.insert(e.file_name.clone())).collect()
    }
    pub fn find_by_hash(&self, hex: &str) -> Option<&EsdEntry> {
        let hex = hex.to_ascii_lowercase();
        self.entries.iter().find(|e| e.sha256.as_deref() == Some(hex.as_str()) || e.sha1.as_deref() == Some(hex.as_str()))
    }
}

pub fn cache_file(cache_dir: &Path, choice: &VersionChoice) -> PathBuf {
    let ext = if choice.cab.is_some() { "cab" } else { "xml" };
    cache_dir.join(format!("products_{}.{}", choice.vid, ext))
}

/// Download (if needed), extract, parse and post-process the catalog for a version choice.
pub fn load(
    choice: &VersionChoice,
    script: &Script,
    http: &reqwest::blocking::Client,
    cache_dir: &Path,
    progress: &Progress,
    force_download: bool,
) -> Result<Catalog> {
    std::fs::create_dir_all(cache_dir).ok();
    let path = cache_file(cache_dir, choice);
    if force_download {
        std::fs::remove_file(&path).ok();
    }
    let url = choice
        .cab
        .as_deref()
        .or(choice.xml.as_deref())
        .ok_or_else(|| anyhow!("{} has no catalog link in the script", choice.vid))?;
    let fresh = matches!(std::fs::metadata(&path), Ok(m) if m.len() > 1024);
    if !fresh {
        progress.set_stage(format!("Downloading catalog for {}", choice.vid));
        fetch_small(http, url, &path, progress).with_context(|| format!("downloading {url}"))?;
    }
    progress.set_stage(format!("Reading catalog for {}", choice.vid));
    let xml = if choice.cab.is_some() {
        extract_products_xml(&path).with_context(|| format!("extracting {}", path.display()))?
    } else {
        std::fs::read_to_string(&path)?
    };
    let (catalog_version, mut entries) = parse_products_xml(&xml).context("parsing products.xml")?;
    let mut notes = Vec::new();
    notes.push(format!("{} file entries listed by Microsoft (catalog schema {})", entries.len(), catalog_version));
    let n = prefilter(&mut entries, choice);
    if n > 0 {
        notes.push(format!("{n} ARM64 / China entries dropped for this Windows 10 version, as the script does"));
    }
    let n = apply_uup(&mut entries, choice, script);
    if n > 0 {
        notes.push(format!(
            "{} entries rewritten to the {} ESD links from the script's embedded table (the official MCT fetches these from the update service; x64 only)",
            n, choice.vid
        ));
    }
    let n = apply_business(&mut entries, choice, script);
    if n > 0 {
        notes.push(format!("{} entries updated to the refreshed build links from the script's business table", n));
    }
    if choice.ver == 14393 || choice.ver == 15063 {
        notes.push("Enterprise links for 1607 / 1703 are only inserted by the script itself, not shown here".to_string());
    }
    Ok(Catalog {
        vid: choice.vid.clone(),
        ver: choice.ver,
        label: choice.label(),
        catalog_version: if choice.cc.is_empty() { catalog_version } else { choice.cc.clone() },
        entries,
        source: url.to_string(),
        notes,
    })
}

fn fetch_small(http: &reqwest::blocking::Client, url: &str, dest: &Path, progress: &Progress) -> Result<()> {
    let mut resp = http.get(url).send()?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    progress.start(total);
    let tmp = dest.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if progress.is_cancelled() {
            drop(file);
            std::fs::remove_file(&tmp).ok();
            bail!("cancelled");
        }
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        progress.add(n as u64);
    }
    file.flush()?;
    drop(file);
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Pull products.xml out of a Microsoft products.cab (MSZIP or LZX compressed).
pub fn extract_products_xml(cab_path: &Path) -> Result<String> {
    let file = std::fs::File::open(cab_path)?;
    let mut cabinet = cab::Cabinet::new(file).context("not a valid cabinet file")?;
    let name = cabinet
        .folder_entries()
        .flat_map(|f| f.file_entries())
        .map(|e| e.name().to_string())
        .find(|n| n.to_ascii_lowercase().ends_with(".xml"))
        .ok_or_else(|| anyhow!("cabinet contains no xml file"))?;
    let mut reader = cabinet.read_file(&name)?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes_to_xml_string(&bytes))
}

fn bytes_to_xml_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let u16s: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u16s);
    }
    let s = String::from_utf8_lossy(bytes);
    s.trim_start_matches('\u{feff}').to_string()
}

fn child_text(node: roxmltree::Node, name: &str) -> Option<String> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .and_then(|c| c.text())
        .map(|t| t.trim().to_string())
}

/// Parse a products.xml (either the `<MCT>` wrapped form or a bare `<PublishedMedia>`).
pub fn parse_products_xml(xml: &str) -> Result<(String, Vec<EsdEntry>)> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let catalog_version = root
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Catalog")
        .and_then(|n| n.attribute("version"))
        .unwrap_or("1.0")
        .to_string();
    let mut entries = Vec::new();
    for file in root.descendants().filter(|n| n.is_element() && n.tag_name().name() == "File") {
        let Some(file_name) = child_text(file, "FileName") else { continue };
        let size = child_text(file, "Size").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        entries.push(EsdEntry {
            file_name,
            lang_code: child_text(file, "LanguageCode").unwrap_or_default(),
            language: child_text(file, "Language").unwrap_or_default(),
            edition: child_text(file, "Edition").unwrap_or_default(),
            arch: child_text(file, "Architecture").unwrap_or_default(),
            size,
            sha1: child_text(file, "Sha1").map(|s| s.to_ascii_lowercase()).filter(|s| !s.is_empty()),
            sha256: child_text(file, "Sha256").map(|s| s.to_ascii_lowercase()).filter(|s| !s.is_empty()),
            url: child_text(file, "FilePath").unwrap_or_default(),
            edition_loc: child_text(file, "Edition_Loc").unwrap_or_default(),
            retail_only: child_text(file, "IsRetailOnly").map(|s| s.eq_ignore_ascii_case("true")).unwrap_or(false),
        });
    }
    Ok((catalog_version, entries))
}

/// Mirror of the script's first products.xml pass for Windows 10 versions: it removes ARM64 entries
/// and the `%BASE_CHINA%` entries, which the later business rewrite would otherwise mislabel.
/// Windows 11 catalogs keep their ARM64 entries here (the script drops them too, but they are valid
/// downloads for validation purposes and the UI hides them by default).
pub fn prefilter(entries: &mut Vec<EsdEntry>, choice: &VersionChoice) -> usize {
    if choice.ver >= 22000 {
        return 0;
    }
    let before = entries.len();
    entries.retain(|e| !e.arch.eq_ignore_ascii_case("ARM64") && e.edition_loc != "%BASE_CHINA%");
    before - entries.len()
}

/// Mirror of the script's `11 25H2+` block: rewrite 24H2 template entries with the links from the UUP table.
/// Returns the number of rewritten entries. Entries without a matching row are dropped, as in the script.
pub fn apply_uup(entries: &mut Vec<EsdEntry>, choice: &VersionChoice, script: &Script) -> usize {
    if choice.ver < 26200 {
        return 0;
    }
    let rows = script.uup_rows(choice.ver);
    let prefix = format!("{}.", choice.ver);
    let mut rewritten = 0;
    entries.retain_mut(|e| {
        if e.file_name.starts_with(&prefix) {
            return true; // already lists the requested build
        }
        let Some(i) = e.file_name.find("_CLIENT") else { return false };
        if i < 1 || !e.arch.eq_ignore_ascii_case("x64") {
            return false;
        }
        let suffix = e.file_name[i..].to_string();
        let upper = suffix.to_ascii_uppercase();
        let client = if upper.starts_with("_CLIENTBUSINESS_") {
            "vol"
        } else if upper.starts_with("_CLIENTCHINA_") {
            "chn"
        } else {
            "ret"
        };
        let Some(row) = rows.iter().find(|r| r.client == client && r.lang.eq_ignore_ascii_case(&e.lang_code)) else {
            return false;
        };
        let name = format!("{}{}", choice.cb, suffix);
        e.url = format!("{}{}/{}", DELIVERY_BASE, row.guid, name);
        e.file_name = name;
        e.size = row.size;
        e.sha256 = Some(row.sha256.clone());
        e.sha1 = None;
        rewritten += 1;
        true
    });
    rewritten
}

/// Mirror of the script's `update existing FilePath entries` block for 1909 / 2004 / 20H2 / 21H1.
pub fn apply_business(entries: &mut Vec<EsdEntry>, choice: &VersionChoice, script: &Script) -> usize {
    if choice.ver <= 15063 || ![18363, 19041, 19042, 19043].contains(&choice.ver) {
        return 0;
    }
    let rows = script.business_rows(choice.ver);
    if rows.is_empty() {
        return 0;
    }
    let mut updated = 0;
    entries.retain_mut(|e| {
        let business = e.edition == "Enterprise" || e.edition == "EnterpriseN";
        let (chan, cli) = if business { ("vol", "_CLIENTBUSINESS_") } else { ("ret", "_CLIENTCONSUMER_") };
        let Some(row) = rows.iter().find(|r| r.client == chan && r.lang.eq_ignore_ascii_case(&e.lang_code)) else {
            return true;
        };
        let (size, sha1, dir) = match e.arch.to_ascii_lowercase().as_str() {
            "x64" => (row.size_x64, &row.sha1_x64, &row.dir_x64),
            "x86" => (row.size_x86, &row.sha1_x86, &row.dir_x86),
            _ => return true,
        };
        let Some(size) = size else { return false };
        let name = format!("{}{}{}_{}FRE_{}", choice.cb, cli, chan.to_ascii_uppercase(), e.arch, e.lang_code);
        e.url = format!("{}{}/upgr/{}{}_{}.esd", WU_BASE, dir, choice.ct, name.to_ascii_lowercase(), sha1);
        e.file_name = format!("{name}.esd");
        e.size = size;
        e.sha1 = Some(sha1.clone());
        e.sha256 = None;
        updated += 1;
        true
    });
    updated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bat;

    const TEMPLATE: &str = r#"<MCT><Catalogs><Catalog version="2.0"><PublishedMedia id="" release=""><Files>
<File id=""><FileName>26100.4349.250607-1500.ge_release_svc_refresh_CLIENTCONSUMER_RET_x64FRE_en-us.esd</FileName><LanguageCode>en-us</LanguageCode><Language>English</Language><Edition>Professional</Edition><Architecture>x64</Architecture><Size>1</Size><Sha1>aa</Sha1><FilePath>http://x/1.esd</FilePath><Key /><Architecture_Loc>%ARCH_64%</Architecture_Loc><Edition_Loc>%PRO%</Edition_Loc><IsRetailOnly>False</IsRetailOnly></File>
<File id=""><FileName>26100.4349.250607-1500.ge_release_svc_refresh_CLIENTBUSINESS_VOL_x64FRE_en-us.esd</FileName><LanguageCode>en-us</LanguageCode><Language>English</Language><Edition>Enterprise</Edition><Architecture>x64</Architecture><Size>2</Size><Sha1>bb</Sha1><FilePath>http://x/2.esd</FilePath><Key /><Architecture_Loc>%ARCH_64%</Architecture_Loc><Edition_Loc>%ENTERPRISE%</Edition_Loc><IsRetailOnly>True</IsRetailOnly></File>
<File id=""><FileName>26100.4349.250607-1500.ge_release_svc_refresh_CLIENTCONSUMER_RET_A64FRE_en-us.esd</FileName><LanguageCode>en-us</LanguageCode><Language>English</Language><Edition>Professional</Edition><Architecture>ARM64</Architecture><Size>3</Size><Sha1>cc</Sha1><FilePath>http://x/3.esd</FilePath><Key /><Architecture_Loc>%ARCH_ARM64%</Architecture_Loc><Edition_Loc>%PRO%</Edition_Loc><IsRetailOnly>False</IsRetailOnly></File>
<File id=""><FileName>26100.4349.250607-1500.ge_release_svc_refresh_CLIENTCHINA_RET_x64FRE_zh-cn.esd</FileName><LanguageCode>zh-cn</LanguageCode><Language>Chinese (China)</Language><Edition>CoreCountrySpecific</Edition><Architecture>x64</Architecture><Size>4</Size><Sha1>dd</Sha1><FilePath>http://x/4.esd</FilePath><Key /><Architecture_Loc>%ARCH_64%</Architecture_Loc><Edition_Loc>%BASE_CHINA%</Edition_Loc><IsRetailOnly>False</IsRetailOnly></File>
</Files></PublishedMedia></Catalog></Catalogs></MCT>"#;

    #[test]
    fn parses_template() {
        let (ver, entries) = parse_products_xml(TEMPLATE).unwrap();
        assert_eq!(ver, "2.0");
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[1].edition, "Enterprise");
        assert!(entries[1].retail_only);
        assert_eq!(entries[0].sha1.as_deref(), Some("aa"));
        assert_eq!(entries[0].channel(), "Consumer");
        assert_eq!(entries[3].channel(), "China");
    }

    #[test]
    fn rewrites_26h2_like_the_script() {
        let script = bat::script();
        let choice = script.choice_by_vid("11_26H2").unwrap();
        let (_, mut entries) = parse_products_xml(TEMPLATE).unwrap();
        let n = apply_uup(&mut entries, choice, script);
        assert_eq!(n, 3, "x64 consumer, business and china rewritten; arm64 dropped");
        assert_eq!(entries.len(), 3);
        let ret = script.uup.iter().find(|r| r.ver == 26300 && r.client == "ret" && r.lang == "en-us").unwrap();
        let e = &entries[0];
        assert_eq!(e.file_name, format!("{}_CLIENTCONSUMER_RET_x64FRE_en-us.esd", choice.cb));
        assert_eq!(e.url, format!("{}{}/{}", DELIVERY_BASE, ret.guid, e.file_name));
        assert_eq!(e.size, ret.size);
        assert_eq!(e.sha256.as_deref(), Some(ret.sha256.as_str()));
        assert!(e.sha1.is_none());
        let vol = script.uup.iter().find(|r| r.ver == 26300 && r.client == "vol" && r.lang == "en-us").unwrap();
        assert_eq!(entries[1].sha256.as_deref(), Some(vol.sha256.as_str()));
        let chn = script.uup.iter().find(|r| r.ver == 26300 && r.client == "chn" && r.lang == "zh-cn").unwrap();
        assert_eq!(entries[2].sha256.as_deref(), Some(chn.sha256.as_str()));
    }

    #[test]
    fn leaves_24h2_untouched() {
        let script = bat::script();
        let choice = script.choice_by_vid("11_24H2").unwrap();
        let (_, mut entries) = parse_products_xml(TEMPLATE).unwrap();
        assert_eq!(apply_uup(&mut entries, choice, script), 0);
        assert_eq!(apply_business(&mut entries, choice, script), 0);
        assert_eq!(entries.len(), 4);
    }

    #[test]
    fn business_update_builds_wu_links() {
        let script = bat::script();
        let choice = script.choice_by_vid("21H1").unwrap();
        let xml = TEMPLATE
            .replace("26100.4349.250607-1500.ge_release_svc_refresh", "19043.1288.211006-0459.21h1_release_svc_refresh")
            .replace("<Architecture>ARM64</Architecture>", "<Architecture>x86</Architecture>");
        let (_, mut entries) = parse_products_xml(&xml).unwrap();
        assert_eq!(prefilter(&mut entries, choice), 1, "the %BASE_CHINA% entry is dropped for Windows 10");
        let n = apply_business(&mut entries, choice, script);
        assert_eq!(n, 3, "updated {n}");
        let e = &entries[0];
        let row = script.business.iter().find(|r| r.ver == 19043 && r.client == "ret" && r.lang == "en-us").unwrap();
        assert_eq!(e.file_name, format!("{}_CLIENTCONSUMER_RET_x64FRE_en-us.esd", choice.cb));
        assert!(e.url.starts_with(WU_BASE));
        assert!(e.url.ends_with(&format!("_{}.esd", row.sha1_x64)));
        assert!(e.url.contains("/upgr/2021/11/"));
        assert_eq!(e.sha1.as_deref(), Some(row.sha1_x64.as_str()));
    }

    /// Needs network: downloads the real 24H2 catalog and checks the 26H2 rewrite end to end.
    #[test]
    #[ignore]
    fn live_24h2_cab_rewrites_to_26h2() {
        let script = bat::script();
        let choice = script.choice_by_vid("11_26H2").unwrap();
        let http = reqwest::blocking::Client::builder().user_agent(crate::util::BROWSER_UA).build().unwrap();
        let dir = std::env::temp_dir().join("wiv_live_catalog_test");
        let cat = load(choice, script, &http, &dir, &Progress::default(), false).unwrap();
        assert_eq!(cat.entries.len(), 990);
        assert!(cat.entries.iter().all(|e| e.arch == "x64" && e.file_name.starts_with("26300.")));
        let en = cat.entries.iter().find(|e| e.lang_code == "en-us" && e.edition == "Professional").unwrap();
        assert_eq!(en.sha256.as_deref().unwrap().len(), 64);
        assert!(en.url.starts_with(DELIVERY_BASE));
        let cat24 = load(script.choice_by_vid("11_24H2").unwrap(), script, &http, &dir, &Progress::default(), false).unwrap();
        assert_eq!(cat24.entries.len(), 1978);
        assert_eq!(cat24.catalog_version, "2.0");
    }
}
