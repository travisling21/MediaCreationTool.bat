//! Microsoft software-download page API (the method used by Fido / Rufus) for official ISO links,
//! plus the official SHA-256 table embedded in the Windows 11 download page.

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::REFERER;
use serde_json::Value;
use std::time::Duration;

pub const PROFILE_ID: &str = "606624d44113";
pub const ORG_ID: &str = "y6jn8c31";
pub const INSTANCE_ID: &str = "560dc9f3-1aa5-4a2f-b63c-9e18f8d0e175";
const API: &str = "https://www.microsoft.com/software-download-connector/api/";

#[derive(Debug, Clone)]
pub struct Product {
    pub name: &'static str,
    /// Download page slug, e.g. `windows11` or `windows10ISO`.
    pub page: &'static str,
    /// Product edition ids (Microsoft treats x64 and ARM64 as separate ids).
    pub edition_ids: &'static [u32],
}

/// Known product edition ids, after Fido's table. The current Windows 11 release id can also be
/// detected live from the download page with [`detect_edition_ids`].
pub const PRODUCTS: &[Product] = &[
    Product { name: "Windows 11 - current release (multi-edition, x64 and ARM64)", page: "windows11", edition_ids: &[3813, 3816] },
    Product { name: "Windows 11 Home China - current release", page: "windows11", edition_ids: &[3814, 3817] },
    Product { name: "Windows 11 Pro China - current release", page: "windows11", edition_ids: &[3815, 3818] },
    Product { name: "Windows 10 22H2 (multi-edition)", page: "windows10ISO", edition_ids: &[2618] },
    Product { name: "Windows 10 Home China 22H2", page: "windows10ISO", edition_ids: &[2378] },
];

#[derive(Debug, Clone)]
pub struct SkuRef {
    pub session_id: String,
    pub sku_id: String,
}

#[derive(Debug, Clone)]
pub struct LanguageOption {
    pub language: String,
    pub localized: String,
    pub product_display_name: String,
    pub skus: Vec<SkuRef>,
}

#[derive(Debug, Clone)]
pub struct IsoLink {
    pub arch: String,
    pub name: String,
    pub url: String,
    pub file_name: String,
    pub expires: String,
}

#[derive(Debug, Clone)]
pub struct HashRow {
    pub label: String,
    pub sha256: String,
}

pub fn page_url(locale: &str, page: &str) -> String {
    format!("https://www.microsoft.com/{locale}/software-download/{page}")
}

fn get_text(http: &Client, url: &str) -> Result<String> {
    let resp = http.get(url).send().with_context(|| format!("requesting {url}"))?;
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        bail!("{url} answered {status}");
    }
    Ok(text)
}

/// Read the product edition ids offered on a download page right now.
pub fn detect_edition_ids(http: &Client, locale: &str, page: &str) -> Result<Vec<(u32, String)>> {
    let html = get_text(http, &page_url(locale, page))?;
    let re = Regex::new(r#"<option value="(\d+)"[^>]*>([^<]+)</option>"#).unwrap();
    let mut out = Vec::new();
    for cap in re.captures_iter(&html) {
        let id: u32 = cap[1].parse().unwrap_or(0);
        let name = decode_entities(cap[2].trim());
        if id > 0 && name.to_ascii_lowercase().contains("windows") {
            out.push((id, name));
        }
    }
    if out.is_empty() {
        bail!("no product editions found on the page (layout changed?)");
    }
    Ok(out)
}

/// Create a session id and run Microsoft's whitelisting handshake for it.
fn new_session(http: &Client, locale: &str) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let _ = locale;
    get_text(http, &format!("https://vlscppe.microsoft.com/tags?org_id={ORG_ID}&session_id={id}")).context("session tag")?;
    let mdt = get_text(http, &format!("https://ov-df.microsoft.com/mdt.js?instanceId={INSTANCE_ID}&PageId=si&session_id={id}"))
        .context("mdt.js")?;
    let w = Regex::new(r"[?&]w=([A-F0-9]+)").unwrap().captures(&mdt).map(|c| c[1].to_string());
    let rticks = Regex::new(r#"rticks=\"\+?(\d+)"#).unwrap().captures(&mdt).map(|c| c[1].to_string());
    let (Some(w), Some(rticks)) = (w, rticks) else {
        bail!("could not read the anti-automation parameters from mdt.js (Microsoft changed the handshake?)");
    };
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    get_text(
        http,
        &format!("https://ov-df.microsoft.com/?session_id={id}&CustomerId={INSTANCE_ID}&PageId=si&w={w}&mdt={now_ms}&rticks={rticks}"),
    )
    .context("handshake reply")?;
    Ok(id)
}

fn api_errors(v: &Value) -> Option<(i64, String)> {
    let list = v.get("Errors").and_then(|e| e.as_array()).cloned().or_else(|| {
        v.get("ValidationContainer").and_then(|c| c.get("Errors")).and_then(|e| e.as_array()).cloned()
    })?;
    let first = list.first()?;
    let t = first.get("Type").and_then(|t| t.as_i64()).unwrap_or(0);
    let msg = first.get("Value").and_then(|m| m.as_str()).unwrap_or("unknown error").to_string();
    Some((t, msg))
}

/// Languages (SKUs) offered for a product. One session per edition id, as Microsoft requires.
pub fn languages(http: &Client, locale: &str, edition_ids: &[u32]) -> Result<Vec<LanguageOption>> {
    let mut out: Vec<LanguageOption> = Vec::new();
    for &edition in edition_ids {
        let session = new_session(http, locale)?;
        let url = format!(
            "{API}getskuinformationbyproductedition?profile={PROFILE_ID}&productEditionId={edition}&SKU=undefined&friendlyFileName=undefined&Locale={locale}&sessionID={session}"
        );
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_secs(2));
            }
            let text = match get_text(http, &url) {
                Ok(t) => t,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    last_err = Some(anyhow!("unexpected answer from Microsoft: {e}"));
                    continue;
                }
            };
            if let Some((_, msg)) = api_errors(&v) {
                last_err = Some(anyhow!("{msg}"));
                continue;
            }
            let skus = v.get("Skus").and_then(|s| s.as_array()).cloned().unwrap_or_default();
            if skus.is_empty() {
                last_err = Some(anyhow!("no languages returned for edition {edition}"));
                continue;
            }
            for s in skus {
                let language = s.get("Language").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let sku_id = s.get("Id").and_then(|x| x.as_str().map(|s| s.to_string()).or_else(|| x.as_i64().map(|n| n.to_string()))).unwrap_or_default();
                if language.is_empty() || sku_id.is_empty() {
                    continue;
                }
                let localized = s.get("LocalizedLanguage").and_then(|x| x.as_str()).unwrap_or(&language).to_string();
                let pdn = s.get("ProductDisplayName").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if let Some(existing) = out.iter_mut().find(|l| l.language == language) {
                    existing.skus.push(SkuRef { session_id: session.clone(), sku_id });
                } else {
                    out.push(LanguageOption {
                        language,
                        localized,
                        product_display_name: pdn,
                        skus: vec![SkuRef { session_id: session.clone(), sku_id }],
                    });
                }
            }
            last_err = None;
            break;
        }
        if let Some(e) = last_err {
            return Err(e.context(format!("fetching languages for edition {edition}")));
        }
    }
    Ok(out)
}

fn arch_from_type(t: i64) -> &'static str {
    match t {
        0 => "x86",
        1 => "x64",
        2 => "ARM64",
        _ => "unknown",
    }
}

/// Time-limited ISO links for a language (one request per SKU / architecture family).
pub fn links(http: &Client, locale: &str, lang: &LanguageOption) -> Result<Vec<IsoLink>> {
    let mut out = Vec::new();
    for sku in &lang.skus {
        let url = format!(
            "{API}GetProductDownloadLinksBySku?profile={PROFILE_ID}&productEditionId=undefined&SKU={}&friendlyFileName=undefined&Locale={locale}&sessionID={}",
            sku.sku_id, sku.session_id
        );
        let resp = http
            .get(&url)
            .header(REFERER, "https://www.microsoft.com/software-download/windows11")
            .send()
            .context("requesting download links")?;
        let text = resp.text()?;
        let v: Value = serde_json::from_str(&text).map_err(|e| anyhow!("unexpected answer from Microsoft: {e}"))?;
        if let Some((t, msg)) = api_errors(&v) {
            if t == 9 {
                bail!(
                    "Microsoft has temporarily banned this IP address for ISO downloads (message code 715-123130, session {}). \
                     This usually clears after 24 hours; a different network or a VPN also works. Original message: {msg}",
                    sku.session_id
                );
            }
            bail!("{msg}");
        }
        let expires = v.get("DownloadExpirationDatetime").and_then(|x| x.as_str()).unwrap_or("").to_string();
        for o in v.get("ProductDownloadOptions").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let link = o.get("Uri").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if link.is_empty() {
                continue;
            }
            let file_name = url::Url::parse(&link)
                .ok()
                .and_then(|u| u.path_segments().and_then(|mut s| s.next_back().map(|s| s.to_string())))
                .unwrap_or_else(|| "download.iso".to_string());
            out.push(IsoLink {
                arch: arch_from_type(o.get("DownloadType").and_then(|x| x.as_i64()).unwrap_or(-1)).to_string(),
                name: o.get("Name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                url: link,
                file_name,
                expires: expires.clone(),
            });
        }
    }
    if out.is_empty() {
        bail!("Microsoft returned no download links");
    }
    Ok(out)
}

/// The official SHA-256 table on the download page ("Hash values for the ISO files for Each Language").
pub fn official_hashes(http: &Client, locale: &str, page: &str) -> Result<Vec<HashRow>> {
    let html = get_text(http, &page_url(locale, page))?;
    Ok(parse_hash_table(&html))
}

pub fn parse_hash_table(html: &str) -> Vec<HashRow> {
    let tr = Regex::new(r"(?s)<tr[^>]*>(.*?)</tr>").unwrap();
    let td = Regex::new(r"(?s)<t[dh][^>]*>(.*?)</t[dh]>").unwrap();
    let tag = Regex::new(r"<[^>]+>").unwrap();
    let hex64 = Regex::new(r"^[0-9A-Fa-f]{64}$").unwrap();
    let mut out = Vec::new();
    for row in tr.captures_iter(html) {
        let cells: Vec<String> =
            td.captures_iter(&row[1]).map(|c| decode_entities(tag.replace_all(&c[1], "").trim())).collect();
        if cells.len() >= 2 && hex64.is_match(&cells[1]) {
            out.push(HashRow { label: cells[0].clone(), sha256: cells[1].to_ascii_lowercase() });
        }
    }
    out
}

/// Map an API language name and architecture to the label used in the hash table.
pub fn expected_hash<'a>(rows: &'a [HashRow], language: &str, arch: &str) -> Option<&'a HashRow> {
    let base = match language {
        "English (United Kingdom)" => "English International".to_string(),
        "Chinese (Simplified)" => "Chinese Simplified".to_string(),
        "Chinese (Traditional)" => "Chinese Traditional".to_string(),
        other => other.to_string(),
    };
    let suffix = match arch {
        "x64" => "64-bit",
        "x86" => "32-bit",
        "ARM64" => "ARM64",
        _ => return None,
    };
    let want = format!("{base} {suffix}").to_ascii_lowercase();
    rows.iter().find(|r| r.label.to_ascii_lowercase() == want)
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&").replace("&#61;", "=").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hash_table() {
        let html = r#"<p>Hash values for the ISO files for Each Language</p><table><tr><th>Country Locale</th><th>Hash Code</th></tr>
<tr><td>English 64-bit</td><td>A75AE3F36CB9FFFFEDCB2D7DED9CEF83FFB710EB1376610D2220684D987CB9A5</td></tr>
<tr><td><strong>English International 64-bit</strong></td><td> fe219a43a408534fb79b170925e750289de0d80ca52d297b0977dcf5e71b6d87 </td></tr></table>"#;
        let rows = parse_hash_table(html);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "English 64-bit");
        assert_eq!(rows[0].sha256, "a75ae3f36cb9ffffedcb2d7ded9cef83ffb710eb1376610d2220684d987cb9a5");
        assert_eq!(expected_hash(&rows, "English (United Kingdom)", "x64").unwrap().label, "English International 64-bit");
        assert_eq!(expected_hash(&rows, "English", "x64").unwrap().sha256, rows[0].sha256);
        assert!(expected_hash(&rows, "English", "ARM64").is_none());
    }

    #[test]
    fn reads_api_errors() {
        let v: Value = serde_json::from_str(r#"{"Errors":[{"Key":"x","Value":"Sentinel marked this request as rejected.","Type":8}]}"#).unwrap();
        assert_eq!(api_errors(&v).unwrap().0, 8);
        let v: Value = serde_json::from_str(r#"{"ValidationContainer":{"Errors":[]},"Skus":[]}"#).unwrap();
        assert!(api_errors(&v).is_none());
    }
}
