//! The egui user interface. All long running work runs on worker threads and reports back through
//! channels; the UI only polls.

use crate::bat::{self, Script};
use crate::catalog::{self, Catalog};
use crate::config::{self, Config};
use crate::download::{self, DownloadRequest};
use crate::hashing::{self, Hashes};
use crate::iso;
use crate::mct::{self, McStatus, McTask, Preset};
use crate::msdl;
use crate::util::{self, human_bytes, human_rate, norm_hex, Progress};
use anyhow::Result;
use eframe::egui;
use egui::{Color32, RichText};
use egui_extras::{Column, TableBuilder};
use reqwest::blocking::Client;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const EDITIONS: &[&str] = &[
    "",
    "Home",
    "HomeN",
    "Pro",
    "ProN",
    "Edu",
    "EduN",
    "Enterprise",
    "EnterpriseN",
    "ProfessionalWorkstation",
    "ProfessionalWorkstationN",
    "ProfessionalEducation",
    "ProfessionalEducationN",
    "CoreSingleLanguage",
    "CoreCountrySpecific",
];

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Catalog,
    MicrosoftIso,
    Validate,
    CreateMedia,
    Settings,
}

enum TaskOutput {
    Catalog(Catalog),
    Download { label: String, result: download::DownloadResult, expected: Option<(String, String)> },
    Hash { path: PathBuf, hashes: Hashes, info: Option<iso::MediaInfo> },
    MsEditions(Vec<(u32, String)>),
    MsLanguages(Vec<msdl::LanguageOption>),
    MsLinks { links: Vec<msdl::IsoLink>, hashes: Vec<msdl::HashRow> },
}

struct Task {
    id: u64,
    name: String,
    progress: Arc<Progress>,
    rx: mpsc::Receiver<Result<TaskOutput, String>>,
}

#[derive(Clone)]
struct CatRow {
    file_name: String,
    lang: String,
    channel: String,
    arch: String,
    editions: String,
    size: u64,
    hash_algo: String,
    hash: String,
    url: String,
}

#[derive(Clone)]
struct Completed {
    label: String,
    path: PathBuf,
    size: u64,
    hashes: Hashes,
    expected: Option<(String, String)>,
    verified: Option<bool>,
    at: String,
}

struct ValResult {
    path: PathBuf,
    hashes: Hashes,
    info: Option<iso::MediaInfo>,
    verdicts: Vec<(bool, String)>,
}

enum Action {
    LoadCatalog(String),
    DownloadEsd(CatRow),
    Copy(String),
    OpenUrl(String),
    OpenPath(PathBuf),
    MsDetect,
    MsLanguages,
    MsLinks,
    MsDownload(usize),
    Hash,
    BrowseFile,
    BrowseDownloadDir,
    BrowseWorkDir,
    SaveScript,
    McLaunch,
    McRefresh,
    ValidatePath(PathBuf),
    SaveConfig,
    CancelTask(u64),
}

pub struct App {
    cfg: Config,
    script: &'static Script,
    http: Client,
    dl_http: Client,
    log: Vec<String>,
    tab: Tab,
    tasks: Vec<Task>,
    next_task_id: u64,
    // catalog tab
    cat_vid: String,
    catalogs: BTreeMap<String, Catalog>,
    cat_lang: String,
    cat_channel: String,
    cat_x64_only: bool,
    cat_filter: String,
    cat_rows: Vec<CatRow>,
    cat_rows_key: String,
    completed: Vec<Completed>,
    // microsoft tab
    ms_product: usize,
    ms_live_editions: Vec<(u32, String)>,
    ms_live_selected: Option<usize>,
    ms_langs: Vec<msdl::LanguageOption>,
    ms_lang: usize,
    ms_links: Vec<msdl::IsoLink>,
    ms_hashes: Vec<msdl::HashRow>,
    // validate tab
    val_path: String,
    val_expected: String,
    val_result: Option<ValResult>,
    // media tab
    mct_choice: u32,
    mct_preset: Preset,
    mct_edition: String,
    mct_lang: String,
    mct_arch: String,
    mct_def: bool,
    mct_no_update: bool,
    mct_status: McStatus,
    mct_status_at: Option<Instant>,
    // settings
    settings_download_dir: String,
    settings_work_dir: String,
    settings_locale: String,
    status_line: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_zoom_factor(1.0);
        let cfg = config::load();
        let script = bat::script();
        let http = Client::builder()
            .user_agent(util::BROWSER_UA)
            .timeout(Duration::from_secs(60))
            .build()
            .expect("http client");
        let dl_http = Client::builder()
            .user_agent(util::BROWSER_UA)
            .timeout(None)
            .connect_timeout(Duration::from_secs(30))
            .build()
            .expect("download client");
        let default_choice = script.choice_by_vid(&cfg.last_vid).or_else(|| script.choice_by_index(script.default_index));
        let cat_vid = default_choice.map(|c| c.vid.clone()).unwrap_or_default();
        let mct_choice = default_choice.map(|c| c.index).unwrap_or(script.default_index);
        let mut app = Self {
            settings_download_dir: cfg.download_dir.display().to_string(),
            settings_work_dir: cfg.work_dir.display().to_string(),
            settings_locale: cfg.ms_locale.clone(),
            cfg,
            script,
            http,
            dl_http,
            log: Vec::new(),
            tab: Tab::Catalog,
            tasks: Vec::new(),
            next_task_id: 1,
            cat_vid,
            catalogs: BTreeMap::new(),
            cat_lang: String::new(),
            cat_channel: String::new(),
            cat_x64_only: true,
            cat_filter: String::new(),
            cat_rows: Vec::new(),
            cat_rows_key: String::new(),
            completed: Vec::new(),
            ms_product: 0,
            ms_live_editions: Vec::new(),
            ms_live_selected: None,
            ms_langs: Vec::new(),
            ms_lang: 0,
            ms_links: Vec::new(),
            ms_hashes: Vec::new(),
            val_path: String::new(),
            val_expected: String::new(),
            val_result: None,
            mct_choice,
            mct_preset: Preset::AutoIso,
            mct_edition: String::new(),
            mct_lang: String::new(),
            mct_arch: String::new(),
            mct_def: false,
            mct_no_update: false,
            mct_status: McStatus::default(),
            mct_status_at: None,
            status_line: String::new(),
        };
        app.log(format!(
            "Bundled MediaCreationTool.bat: {} ({} versions, newest {})",
            script.changelog,
            script.choices.len(),
            script.choices.first().map(|c| c.label()).unwrap_or_default()
        ));
        app.log(format!("Config file: {}", config::config_path().display()));
        app
    }

    fn log(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.status_line = msg.clone();
        self.log.push(format!("[{}] {}", util::timestamp(), msg));
        if self.log.len() > 500 {
            self.log.drain(..100);
        }
    }

    fn busy(&self, prefix: &str) -> bool {
        self.tasks.iter().any(|t| t.name.starts_with(prefix))
    }

    fn spawn<F>(&mut self, ctx: &egui::Context, name: impl Into<String>, f: F)
    where
        F: FnOnce(&Progress) -> Result<TaskOutput> + Send + 'static,
    {
        let name = name.into();
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let p2 = progress.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&p2)));
            let msg = match r {
                Ok(Ok(o)) => Ok(o),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(_) => Err("worker thread panicked".to_string()),
            };
            let _ = tx.send(msg);
            ctx2.request_repaint();
        });
        self.log(format!("Started: {name}"));
        self.tasks.push(Task { id: self.next_task_id, name, progress, rx });
        self.next_task_id += 1;
    }

    fn poll_tasks(&mut self) {
        let mut finished = Vec::new();
        for (i, t) in self.tasks.iter().enumerate() {
            if let Ok(msg) = t.rx.try_recv() {
                finished.push((i, t.name.clone(), msg));
            }
        }
        for (i, name, msg) in finished.into_iter().rev() {
            self.tasks.remove(i);
            match msg {
                Ok(out) => self.handle_output(name, out),
                Err(e) => self.log(format!("Failed: {name}: {e}")),
            }
        }
    }

    fn handle_output(&mut self, name: String, out: TaskOutput) {
        match out {
            TaskOutput::Catalog(c) => {
                self.log(format!("{}: {} entries ({} files) loaded", c.label, c.entries.len(), c.distinct_files().len()));
                self.cat_rows_key.clear();
                self.catalogs.insert(c.vid.clone(), c);
            }
            TaskOutput::Download { label, result, expected } => {
                if result.resumed_from > 0 && result.resumed_from < result.bytes {
                    self.log(format!("Resumed {label} from {}", human_bytes(result.resumed_from)));
                }
                let verified = expected.as_ref().map(|(algo, hex)| {
                    let got = match algo.as_str() {
                        "SHA-256" => &result.hashes.sha256,
                        "SHA-1" => &result.hashes.sha1,
                        _ => &result.hashes.md5,
                    };
                    got == &norm_hex(hex)
                });
                match verified {
                    Some(true) => self.log(format!("Verified OK: {label} ({})", human_bytes(result.bytes))),
                    Some(false) => self.log(format!("HASH MISMATCH: {label} - the file is corrupt or not the expected build")),
                    None => self.log(format!("Downloaded (no reference hash available): {label}")),
                }
                self.completed.push(Completed {
                    label,
                    path: result.path,
                    size: result.bytes,
                    hashes: result.hashes,
                    expected,
                    verified,
                    at: util::timestamp(),
                });
            }
            TaskOutput::Hash { path, hashes, info } => {
                let verdicts = self.verdicts_for(&hashes);
                self.log(format!("Hashed {}: SHA-256 {}", path.display(), hashes.sha256));
                self.val_result = Some(ValResult { path, hashes, info, verdicts });
            }
            TaskOutput::MsEditions(list) => {
                self.log(format!("Download page lists {} product edition(s): {}", list.len(), list.iter().map(|(id, n)| format!("{n} [{id}]")).collect::<Vec<_>>().join(", ")));
                self.ms_live_editions = list;
                self.ms_live_selected = Some(0);
            }
            TaskOutput::MsLanguages(l) => {
                self.log(format!("{} languages available", l.len()));
                self.ms_langs = l;
                self.ms_lang = self.ms_langs.iter().position(|l| l.language == "English").unwrap_or(0);
                self.ms_links.clear();
            }
            TaskOutput::MsLinks { links, hashes } => {
                self.log(format!("{} download link(s), {} official hash rows", links.len(), hashes.len()));
                self.ms_links = links;
                self.ms_hashes = hashes;
            }
        }
        let _ = name;
    }

    fn verdicts_for(&self, h: &Hashes) -> Vec<(bool, String)> {
        let mut v = Vec::new();
        let exp = norm_hex(&self.val_expected);
        if !exp.is_empty() {
            let got = match exp.len() {
                32 => Some(&h.md5),
                40 => Some(&h.sha1),
                64 => Some(&h.sha256),
                _ => None,
            };
            match got {
                Some(g) if g == &exp => v.push((true, "Matches the expected hash you entered".into())),
                Some(_) => v.push((false, "Does NOT match the expected hash you entered".into())),
                None => v.push((false, "Expected hash must be 32 (MD5), 40 (SHA-1) or 64 (SHA-256) hex characters".into())),
            }
        }
        for c in self.catalogs.values() {
            if let Some(e) = c.find_by_hash(&h.sha256).or_else(|| c.find_by_hash(&h.sha1)) {
                v.push((true, format!("Matches Microsoft's catalog for {}: {} ({} {} {})", c.label, e.file_name, e.lang_code, e.channel(), e.arch)));
            }
        }
        if let Some(row) = self.ms_hashes.iter().find(|r| r.sha256 == h.sha256) {
            v.push((true, format!("Matches Microsoft's published ISO hash: {}", row.label)));
        }
        if v.is_empty() {
            v.push((false, "No reference matched. Enter an expected hash, load a catalog, or fetch Microsoft's ISO hashes to compare.".into()));
        }
        v
    }

    fn rebuild_cat_rows(&mut self) {
        let key = format!("{}|{}|{}|{}|{}", self.cat_vid, self.cat_lang, self.cat_channel, self.cat_x64_only, self.cat_filter);
        if key == self.cat_rows_key {
            return;
        }
        self.cat_rows_key = key;
        self.cat_rows.clear();
        let Some(c) = self.catalogs.get(&self.cat_vid) else { return };
        let filter = self.cat_filter.to_ascii_lowercase();
        let mut by_file: BTreeMap<String, CatRow> = BTreeMap::new();
        for e in &c.entries {
            if !self.cat_lang.is_empty() && e.lang_code != self.cat_lang {
                continue;
            }
            if !self.cat_channel.is_empty() && e.channel() != self.cat_channel {
                continue;
            }
            if self.cat_x64_only && !e.arch.eq_ignore_ascii_case("x64") {
                continue;
            }
            let row = by_file.entry(e.file_name.clone()).or_insert_with(|| {
                let (algo, hash) = e.best_hash().map(|(a, h)| (a.to_string(), h.to_string())).unwrap_or_default();
                CatRow {
                    file_name: e.file_name.clone(),
                    lang: e.lang_code.clone(),
                    channel: e.channel().to_string(),
                    arch: e.arch.clone(),
                    editions: String::new(),
                    size: e.size,
                    hash_algo: algo,
                    hash,
                    url: e.url.clone(),
                }
            });
            if !row.editions.is_empty() {
                row.editions.push_str(", ");
            }
            row.editions.push_str(&e.edition);
        }
        self.cat_rows = by_file
            .into_values()
            .filter(|r| {
                filter.is_empty()
                    || r.editions.to_ascii_lowercase().contains(&filter)
                    || r.file_name.to_ascii_lowercase().contains(&filter)
                    || r.lang.contains(&filter)
            })
            .collect();
        self.cat_rows.sort_by_key(|r| (r.lang.clone(), r.channel.clone(), r.arch.clone()));
    }

    // ------------------------------------------------------------------ actions

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::LoadCatalog(vid) => {
                let Some(choice) = self.script.choice_by_vid(&vid).cloned() else { return };
                let http = self.http.clone();
                let script = self.script;
                let cache = self.cfg.download_dir.join("catalogs");
                self.spawn(ctx, format!("catalog {}", choice.vid), move |p| {
                    catalog::load(&choice, script, &http, &cache, p).map(TaskOutput::Catalog)
                });
            }
            Action::DownloadEsd(row) => {
                let dest = self.cfg.download_dir.join(&row.file_name);
                let http = self.dl_http.clone();
                let expected = if row.hash.is_empty() { None } else { Some((row.hash_algo.clone(), row.hash.clone())) };
                let label = row.file_name.clone();
                let url = row.url.clone();
                let size = row.size;
                self.spawn(ctx, format!("download {}", row.file_name), move |p| {
                    let req = DownloadRequest { url: &url, dest: &dest, expected_size: Some(size), referer: None };
                    let result = download::download(&http, &req, p)?;
                    Ok(TaskOutput::Download { label, result, expected })
                });
            }
            Action::Copy(s) => {
                ctx.copy_text(s);
                self.log("Copied to clipboard");
            }
            Action::OpenUrl(u) => {
                if let Err(e) = open::that(&u) {
                    self.log(format!("Could not open browser: {e}"));
                }
            }
            Action::OpenPath(p) => {
                if let Err(e) = open::that(&p) {
                    self.log(format!("Could not open {}: {e}", p.display()));
                }
            }
            Action::MsDetect => {
                let http = self.http.clone();
                let locale = self.cfg.ms_locale.clone();
                let page = msdl::PRODUCTS[self.ms_product].page;
                self.spawn(ctx, "microsoft editions", move |p| {
                    p.set_stage("Reading the download page");
                    msdl::detect_edition_ids(&http, &locale, page).map(TaskOutput::MsEditions)
                });
            }
            Action::MsLanguages => {
                let http = self.http.clone();
                let locale = self.cfg.ms_locale.clone();
                let ids: Vec<u32> = match self.ms_live_selected {
                    Some(i) if i < self.ms_live_editions.len() => vec![self.ms_live_editions[i].0],
                    _ => msdl::PRODUCTS[self.ms_product].edition_ids.to_vec(),
                };
                self.spawn(ctx, "microsoft languages", move |p| {
                    p.set_stage("Creating a download session and listing languages");
                    msdl::languages(&http, &locale, &ids).map(TaskOutput::MsLanguages)
                });
            }
            Action::MsLinks => {
                let Some(lang) = self.ms_langs.get(self.ms_lang).cloned() else { return };
                let http = self.http.clone();
                let locale = self.cfg.ms_locale.clone();
                let page = msdl::PRODUCTS[self.ms_product].page;
                self.spawn(ctx, "microsoft links", move |p| {
                    p.set_stage("Requesting download links");
                    let links = msdl::links(&http, &locale, &lang)?;
                    p.set_stage("Reading the official hash table");
                    let hashes = msdl::official_hashes(&http, &locale, page).unwrap_or_default();
                    Ok(TaskOutput::MsLinks { links, hashes })
                });
            }
            Action::MsDownload(i) => {
                let Some(link) = self.ms_links.get(i).cloned() else { return };
                let lang = self.ms_langs.get(self.ms_lang).map(|l| l.language.clone()).unwrap_or_default();
                let expected = msdl::expected_hash(&self.ms_hashes, &lang, &link.arch).map(|r| ("SHA-256".to_string(), r.sha256.clone()));
                let dest = self.cfg.download_dir.join(&link.file_name);
                let http = self.dl_http.clone();
                let label = link.file_name.clone();
                let url = link.url.clone();
                self.spawn(ctx, format!("download {}", link.file_name), move |p| {
                    let req = DownloadRequest { url: &url, dest: &dest, expected_size: None, referer: Some("https://www.microsoft.com/software-download/windows11") };
                    let result = download::download(&http, &req, p)?;
                    Ok(TaskOutput::Download { label, result, expected })
                });
            }
            Action::Hash => {
                let path = PathBuf::from(self.val_path.trim());
                if !path.is_file() {
                    self.log(format!("Not a file: {}", path.display()));
                    return;
                }
                self.spawn(ctx, format!("hash {}", path.display()), move |p| {
                    let info = iso::inspect(&path).ok();
                    let hashes = hashing::hash_file(&path, p)?;
                    Ok(TaskOutput::Hash { path, hashes, info })
                });
            }
            Action::BrowseFile => {
                if let Some(p) = rfd::FileDialog::new().add_filter("Disc images", &["iso", "esd", "wim", "img"]).pick_file() {
                    self.val_path = p.display().to_string();
                }
            }
            Action::BrowseDownloadDir => {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.settings_download_dir = p.display().to_string();
                }
            }
            Action::BrowseWorkDir => {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.settings_work_dir = p.display().to_string();
                }
            }
            Action::SaveScript => {
                if let Some(p) = rfd::FileDialog::new().set_file_name("MediaCreationTool.bat").save_file() {
                    match std::fs::write(&p, bat::SCRIPT.as_bytes()) {
                        Ok(_) => self.log(format!("Saved {}", p.display())),
                        Err(e) => self.log(format!("Could not save script: {e}")),
                    }
                }
            }
            Action::McLaunch => {
                let task = McTask {
                    choice_index: self.mct_choice,
                    preset: self.mct_preset,
                    edition: self.mct_edition.clone(),
                    lang: self.mct_lang.clone(),
                    arch: self.mct_arch.clone(),
                    def: self.mct_def,
                    no_update: self.mct_no_update,
                };
                let args = mct::build_args(&task);
                match mct::launch(&self.cfg.work_dir, &args) {
                    Ok(_) => self.log(format!("Launched MediaCreationTool.bat {} - accept the elevation prompt in the new window", args.join(" "))),
                    Err(e) => self.log(format!("Could not launch the script: {e:#}")),
                }
                self.mct_status_at = None;
            }
            Action::McRefresh => {
                self.mct_status = mct::status(&self.cfg.work_dir);
                self.mct_status_at = Some(Instant::now());
            }
            Action::ValidatePath(p) => {
                self.val_path = p.display().to_string();
                self.val_expected.clear();
                self.tab = Tab::Validate;
                self.apply(ctx, Action::Hash);
            }
            Action::SaveConfig => {
                self.cfg.download_dir = PathBuf::from(self.settings_download_dir.trim());
                self.cfg.work_dir = PathBuf::from(self.settings_work_dir.trim());
                self.cfg.ms_locale = self.settings_locale.trim().to_string();
                self.cfg.last_vid = self.cat_vid.clone();
                match config::save(&self.cfg) {
                    Ok(_) => self.log(format!("Settings saved to {}", config::config_path().display())),
                    Err(e) => self.log(format!("Could not save settings: {e:#}")),
                }
            }
            Action::CancelTask(id) => {
                if let Some(t) = self.tasks.iter().find(|t| t.id == id) {
                    t.progress.request_cancel();
                }
            }
        }
    }

    // ------------------------------------------------------------------ ui pieces

    fn ui_tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Windows ISO Validator");
            ui.separator();
            ui.selectable_value(&mut self.tab, Tab::Catalog, "ESD catalog");
            ui.selectable_value(&mut self.tab, Tab::MicrosoftIso, "Microsoft ISO");
            ui.selectable_value(&mut self.tab, Tab::Validate, "Validate");
            ui.selectable_value(&mut self.tab, Tab::CreateMedia, "Create media (MCT)");
            ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("script {}", self.script.changelog.split(' ').next().unwrap_or(""))).weak());
            });
        });
    }

    fn ui_catalog(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label("Version:");
            let current = self.script.choice_by_vid(&self.cat_vid).map(|c| c.label()).unwrap_or_default();
            egui::ComboBox::from_id_salt("cat_vid").selected_text(current).width(260.0).show_ui(ui, |ui| {
                for c in &self.script.choices {
                    ui.selectable_value(&mut self.cat_vid, c.vid.clone(), c.label());
                }
            });
            let loaded = self.catalogs.contains_key(&self.cat_vid);
            let busy = self.busy(&format!("catalog {}", self.cat_vid));
            let text = if loaded { "Reload catalog" } else { "Load catalog" };
            if ui.add_enabled(!busy, egui::Button::new(text)).clicked() {
                self.catalogs.remove(&self.cat_vid);
                actions.push(Action::LoadCatalog(self.cat_vid.clone()));
            }
            if let Some(c) = self.script.choice_by_vid(&self.cat_vid) {
                ui.label(RichText::new(&c.note).weak().small());
            }
        });
        let Some(cat) = self.catalogs.get(&self.cat_vid) else {
            ui.add_space(8.0);
            ui.label("Load the catalog to list the ESD files Microsoft publishes for this version. Downloads are verified against the catalog hash.");
            if let Some(c) = self.script.choice_by_vid(&self.cat_vid) {
                ui.label(RichText::new(format!("Source: {}", c.cab.as_deref().or(c.xml.as_deref()).unwrap_or("-"))).weak().small());
            }
            return;
        };
        let notes = cat.notes.clone();
        let languages = cat.languages();
        ui.label(RichText::new(format!("• Build {} base, catalog schema {}, source {}", cat.ver, cat.catalog_version, cat.source)).small());
        for n in &notes {
            ui.label(RichText::new(format!("• {n}")).small());
        }
        ui.horizontal(|ui| {
            ui.label("Language:");
            egui::ComboBox::from_id_salt("cat_lang").selected_text(if self.cat_lang.is_empty() { "All" } else { &self.cat_lang }).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.cat_lang, String::new(), "All");
                for l in &languages {
                    ui.selectable_value(&mut self.cat_lang, l.clone(), l);
                }
            });
            ui.label("Channel:");
            egui::ComboBox::from_id_salt("cat_chan").selected_text(if self.cat_channel.is_empty() { "All" } else { &self.cat_channel }).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.cat_channel, String::new(), "All");
                for c in ["Consumer", "Business", "China"] {
                    ui.selectable_value(&mut self.cat_channel, c.to_string(), c);
                }
            });
            ui.checkbox(&mut self.cat_x64_only, "x64 only");
            ui.label("Filter:");
            ui.add(egui::TextEdit::singleline(&mut self.cat_filter).desired_width(160.0).hint_text("edition or file name"));
        });
        self.rebuild_cat_rows();
        ui.label(RichText::new(format!("{} file(s) - Consumer = Home/Pro/Edu, Business = Pro VL/Enterprise", self.cat_rows.len())).weak());
        let rows = &self.cat_rows;
        let dl_dir = self.cfg.download_dir.clone();
        let height = ui.available_height() - 4.0;
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .max_scroll_height(height)
            .column(Column::auto().at_least(70.0))
            .column(Column::auto().at_least(70.0))
            .column(Column::auto().at_least(50.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::auto().at_least(70.0))
            .column(Column::auto().at_least(120.0))
            .column(Column::auto().at_least(150.0))
            .header(22.0, |mut h| {
                for t in ["Language", "Channel", "Arch", "Editions in this file", "Size", "Hash", "Actions"] {
                    h.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(24.0, rows.len(), |mut row| {
                    let r = &rows[row.index()];
                    row.col(|ui| {
                        ui.label(&r.lang);
                    });
                    row.col(|ui| {
                        ui.label(&r.channel);
                    });
                    row.col(|ui| {
                        ui.label(&r.arch);
                    });
                    row.col(|ui| {
                        ui.label(&r.editions).on_hover_text(&r.file_name);
                    });
                    row.col(|ui| {
                        ui.label(human_bytes(r.size));
                    });
                    row.col(|ui| {
                        let short = if r.hash.len() > 14 { format!("{} {}…", r.hash_algo, &r.hash[..14]) } else { r.hash.clone() };
                        ui.label(RichText::new(short).monospace()).on_hover_text(&r.hash);
                    });
                    row.col(|ui| {
                        if ui.button("Download").on_hover_text(dl_dir.join(&r.file_name).display().to_string()).clicked() {
                            actions.push(Action::DownloadEsd(r.clone()));
                        }
                        if ui.button("Copy link").clicked() {
                            actions.push(Action::Copy(r.url.clone()));
                        }
                    });
                });
            });
    }

    fn ui_microsoft(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.label("Official multi-edition ISO links straight from Microsoft's download page (the same method Rufus / Fido use). Links are valid for 24 hours and Microsoft rate-limits requests per IP address.");
        ui.horizontal(|ui| {
            ui.label("Product:");
            let name = msdl::PRODUCTS[self.ms_product].name;
            egui::ComboBox::from_id_salt("ms_product").selected_text(name).width(380.0).show_ui(ui, |ui| {
                for (i, p) in msdl::PRODUCTS.iter().enumerate() {
                    if ui.selectable_value(&mut self.ms_product, i, p.name).changed() {
                        self.ms_live_selected = None;
                        self.ms_langs.clear();
                        self.ms_links.clear();
                    }
                }
            });
            if ui.add_enabled(!self.busy("microsoft"), egui::Button::new("Detect editions on the page")).on_hover_text("Reads the product list currently offered on the download page").clicked() {
                actions.push(Action::MsDetect);
            }
        });
        if !self.ms_live_editions.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Edition from the page:");
                let sel = self.ms_live_selected.and_then(|i| self.ms_live_editions.get(i)).map(|(id, n)| format!("{n} [{id}]")).unwrap_or_else(|| "Use the built-in id list".into());
                egui::ComboBox::from_id_salt("ms_live").selected_text(sel).width(380.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.ms_live_selected, None, "Use the built-in id list");
                    for (i, (id, n)) in self.ms_live_editions.iter().enumerate() {
                        ui.selectable_value(&mut self.ms_live_selected, Some(i), format!("{n} [{id}]"));
                    }
                });
            });
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.busy("microsoft"), egui::Button::new("1. Fetch languages")).clicked() {
                actions.push(Action::MsLanguages);
            }
            if !self.ms_langs.is_empty() {
                ui.label("Language:");
                let sel = self.ms_langs.get(self.ms_lang).map(|l| format!("{} ({})", l.language, l.product_display_name)).unwrap_or_default();
                egui::ComboBox::from_id_salt("ms_lang").selected_text(sel).width(360.0).show_ui(ui, |ui| {
                    for (i, l) in self.ms_langs.iter().enumerate() {
                        ui.selectable_value(&mut self.ms_lang, i, format!("{} ({})", l.language, l.product_display_name)).on_hover_text(&l.localized);
                    }
                });
                if ui.add_enabled(!self.busy("microsoft"), egui::Button::new("2. Get download links")).clicked() {
                    actions.push(Action::MsLinks);
                }
            }
        });
        if !self.ms_links.is_empty() {
            ui.separator();
            let lang = self.ms_langs.get(self.ms_lang).map(|l| l.language.clone()).unwrap_or_default();
            ui.label(RichText::new(format!("Links expire {}. Official SHA-256 rows loaded: {}", self.ms_links[0].expires, self.ms_hashes.len())).weak());
            egui::Grid::new("ms_links").num_columns(5).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
                ui.strong("Arch");
                ui.strong("File");
                ui.strong("Official SHA-256");
                ui.strong("");
                ui.strong("");
                ui.end_row();
                for (i, l) in self.ms_links.iter().enumerate() {
                    ui.label(&l.arch);
                    ui.label(&l.file_name).on_hover_text(&l.name);
                    match msdl::expected_hash(&self.ms_hashes, &lang, &l.arch) {
                        Some(r) => {
                            ui.label(RichText::new(format!("{}…", &r.sha256[..16])).monospace()).on_hover_text(&r.sha256);
                        }
                        None => {
                            ui.label(RichText::new("not published").weak());
                        }
                    }
                    if ui.button("Download and verify").clicked() {
                        actions.push(Action::MsDownload(i));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Copy link").clicked() {
                            actions.push(Action::Copy(l.url.clone()));
                        }
                        if ui.button("Open in browser").clicked() {
                            actions.push(Action::OpenUrl(l.url.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
        }
        if !self.ms_hashes.is_empty() {
            ui.collapsing(format!("Microsoft's published hash table ({} rows)", self.ms_hashes.len()), |ui| {
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for r in &self.ms_hashes {
                        ui.horizontal(|ui| {
                            ui.label(&r.label);
                            ui.label(RichText::new(&r.sha256).monospace().small());
                        });
                    }
                });
            });
        }
    }

    fn ui_validate(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.label("Compute MD5, SHA-1 and SHA-256 of any ISO / ESD / WIM and compare them with an expected hash, with every loaded catalog, and with Microsoft's published ISO hashes.");
        ui.horizontal(|ui| {
            ui.label("File:");
            ui.add(egui::TextEdit::singleline(&mut self.val_path).desired_width(560.0));
            if ui.button("Browse…").clicked() {
                actions.push(Action::BrowseFile);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Expected hash (optional):");
            ui.add(egui::TextEdit::singleline(&mut self.val_expected).desired_width(560.0).hint_text("MD5, SHA-1 or SHA-256 hex"));
        });
        if ui.add_enabled(!self.busy("hash "), egui::Button::new("Compute and compare")).clicked() {
            actions.push(Action::Hash);
        }
        if let Some(r) = &self.val_result {
            ui.separator();
            ui.strong(r.path.display().to_string());
            if let Some(info) = &r.info {
                let mut s = format!("{} - {}", info.summary(), human_bytes(info.file_size));
                if let Some(vs) = info.volume_size {
                    if vs != info.file_size {
                        s.push_str(&format!(" (descriptor says {})", human_bytes(vs)));
                    }
                }
                ui.label(s);
            }
            egui::Grid::new("val_hashes").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
                for (name, value) in [("MD5", &r.hashes.md5), ("SHA-1", &r.hashes.sha1), ("SHA-256", &r.hashes.sha256)] {
                    ui.label(name);
                    ui.label(RichText::new(value).monospace());
                    if ui.small_button("Copy").clicked() {
                        actions.push(Action::Copy(value.clone()));
                    }
                    ui.end_row();
                }
            });
            for (ok, text) in &r.verdicts {
                let (icon, color) = if *ok { ("✔", Color32::from_rgb(70, 190, 90)) } else { ("✖", Color32::from_rgb(220, 80, 70)) };
                ui.label(RichText::new(format!("{icon} {text}")).color(color));
            }
        }
        if !self.completed.is_empty() {
            ui.separator();
            ui.strong("Downloads in this session");
            egui::Grid::new("completed").num_columns(5).spacing([12.0, 4.0]).striped(true).show(ui, |ui| {
                for c in self.completed.iter().rev() {
                    ui.label(&c.at);
                    ui.label(&c.label).on_hover_text(c.path.display().to_string());
                    ui.label(human_bytes(c.size));
                    let v = match (c.verified, &c.expected) {
                        (Some(true), Some((a, _))) => RichText::new(format!("✔ {a} verified")).color(Color32::from_rgb(70, 190, 90)),
                        (Some(false), Some((a, _))) => RichText::new(format!("✖ {a} MISMATCH")).color(Color32::from_rgb(220, 80, 70)),
                        _ => RichText::new("no reference hash").weak(),
                    };
                    ui.label(v).on_hover_text(format!("SHA-256 {}", c.hashes.sha256));
                    ui.horizontal(|ui| {
                        if ui.small_button("Re-check").clicked() {
                            actions.push(Action::ValidatePath(c.path.clone()));
                        }
                        if ui.small_button("Open folder").clicked() {
                            actions.push(Action::OpenPath(c.path.parent().map(|p| p.to_path_buf()).unwrap_or_default()));
                        }
                    });
                    ui.end_row();
                }
            });
        }
    }

    fn ui_media(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if !cfg!(windows) {
            ui.label(RichText::new("MediaCreationTool.bat only runs on Windows. The controls below still show the command line that would be used.").color(Color32::from_rgb(230, 160, 60)));
        }
        ui.label("Runs the bundled MediaCreationTool.bat with the selection below. The script opens its own console window, asks for elevation, drives Microsoft's Media Creation Tool and writes the ISO into the work folder.");
        egui::Grid::new("mct_form").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Version");
            let cur = self.script.choice_by_index(self.mct_choice).map(|c| c.label()).unwrap_or_default();
            egui::ComboBox::from_id_salt("mct_ver").selected_text(cur).width(300.0).show_ui(ui, |ui| {
                for c in &self.script.choices {
                    ui.selectable_value(&mut self.mct_choice, c.index, c.label());
                }
            });
            ui.end_row();
            ui.label("Preset");
            egui::ComboBox::from_id_salt("mct_preset").selected_text(self.mct_preset.label()).width(300.0).show_ui(ui, |ui| {
                for p in Preset::ALL {
                    ui.selectable_value(&mut self.mct_preset, p, p.label());
                }
            });
            ui.end_row();
            ui.label("Edition");
            egui::ComboBox::from_id_salt("mct_edition").selected_text(if self.mct_edition.is_empty() { "detected from this PC" } else { &self.mct_edition }).width(300.0).show_ui(ui, |ui| {
                for e in EDITIONS {
                    ui.selectable_value(&mut self.mct_edition, e.to_string(), if e.is_empty() { "detected from this PC" } else { e });
                }
            });
            ui.end_row();
            ui.label("Language");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.mct_lang).desired_width(120.0).hint_text("detected, e.g. en-US"));
                let vid = self.script.choice_by_index(self.mct_choice).map(|c| c.vid.clone()).unwrap_or_default();
                if let Some(cat) = self.catalogs.get(&vid) {
                    egui::ComboBox::from_id_salt("mct_lang_pick").selected_text("from catalog").show_ui(ui, |ui| {
                        for l in cat.languages() {
                            let shown = mct_lang_code(&l);
                            ui.selectable_value(&mut self.mct_lang, shown.clone(), shown);
                        }
                    });
                } else {
                    ui.label(RichText::new("load this version's catalog to pick from a list").weak().small());
                }
            });
            ui.end_row();
            ui.label("Architecture");
            egui::ComboBox::from_id_salt("mct_arch").selected_text(if self.mct_arch.is_empty() { "detected" } else { &self.mct_arch }).width(120.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.mct_arch, String::new(), "detected");
                ui.selectable_value(&mut self.mct_arch, "x64".into(), "x64");
                ui.selectable_value(&mut self.mct_arch, "x86".into(), "x86 (Windows 10 only)");
            });
            ui.end_row();
            ui.label("Options");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.mct_def, "def - untouched MCT media (no setup-check bypass, no auto.cmd)");
                ui.checkbox(&mut self.mct_no_update, "no_update - disable dynamic update");
            });
            ui.end_row();
        });
        let task = McTask {
            choice_index: self.mct_choice,
            preset: self.mct_preset,
            edition: self.mct_edition.clone(),
            lang: self.mct_lang.clone(),
            arch: self.mct_arch.clone(),
            def: self.mct_def,
            no_update: self.mct_no_update,
        };
        let args = mct::build_args(&task);
        ui.horizontal(|ui| {
            ui.label("Command:");
            ui.label(RichText::new(format!("MediaCreationTool.bat {}", args.join(" "))).monospace());
            let aliases: Vec<String> = self.script.aliases.iter().filter(|(i, _)| *i == self.mct_choice).map(|(_, a)| a.clone()).collect();
            if !aliases.is_empty() {
                ui.label(RichText::new(format!("(by name: {})", aliases.join(" / "))).weak().small())
                    .on_hover_text("Rename the saved script, e.g. \"auto 26H2 MediaCreationTool.bat\", to run it without this app");
            }
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(cfg!(windows), egui::Button::new("Write script and launch")).clicked() {
                actions.push(Action::McLaunch);
            }
            if ui.button("Open work folder").clicked() {
                let _ = std::fs::create_dir_all(&self.cfg.work_dir);
                actions.push(Action::OpenPath(self.cfg.work_dir.clone()));
            }
            if ui.button("Save a copy of MediaCreationTool.bat…").clicked() {
                actions.push(Action::SaveScript);
            }
            if ui.button("Refresh status").clicked() {
                actions.push(Action::McRefresh);
            }
        });
        ui.label(RichText::new(format!("Work folder: {}", self.cfg.work_dir.display())).weak().small());
        ui.separator();
        let stale = self.mct_status_at.map(|t| t.elapsed() > Duration::from_secs(3)).unwrap_or(true);
        if stale {
            actions.push(Action::McRefresh);
        }
        ui.horizontal(|ui| {
            ui.label(if self.mct_status.setup_running { "Media Creation Tool setup is running" } else { "Media Creation Tool is not running" });
            if self.mct_status.esd_dir_present {
                ui.label(RichText::new("- script work data in C:\\ESD\\MCT").weak());
            }
        });
        if self.mct_status.isos.is_empty() {
            ui.label(RichText::new("No ISO in the work folder yet.").weak());
        } else {
            egui::Grid::new("mct_isos").num_columns(4).spacing([12.0, 4.0]).striped(true).show(ui, |ui| {
                for f in &self.mct_status.isos {
                    ui.label(f.path.file_name().and_then(|s| s.to_str()).unwrap_or("?"));
                    ui.label(human_bytes(f.size));
                    ui.label(f.modified.map(|m| chrono::DateTime::<chrono::Local>::from(m).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default());
                    if ui.small_button("Validate").clicked() {
                        actions.push(Action::ValidatePath(f.path.clone()));
                    }
                    ui.end_row();
                }
            });
        }
        ui.add_space(6.0);
        ui.label(RichText::new("Tips: 24H2 and newer need a CPU with POPCNT / SSE4.2. Windows 11 media is x64 only. The ISO appears in the work folder when the script prints DONE; validate it here afterwards.").weak().small());
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Grid::new("settings").num_columns(3).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Download folder");
            ui.add(egui::TextEdit::singleline(&mut self.settings_download_dir).desired_width(520.0));
            if ui.button("Browse…").clicked() {
                actions.push(Action::BrowseDownloadDir);
            }
            ui.end_row();
            ui.label("MCT work folder");
            ui.add(egui::TextEdit::singleline(&mut self.settings_work_dir).desired_width(520.0));
            if ui.button("Browse…").clicked() {
                actions.push(Action::BrowseWorkDir);
            }
            ui.end_row();
            ui.label("Microsoft page locale");
            ui.add(egui::TextEdit::singleline(&mut self.settings_locale).desired_width(120.0));
            ui.label(RichText::new("e.g. en-US, de-DE").weak());
            ui.end_row();
        });
        if ui.button("Save settings").clicked() {
            actions.push(Action::SaveConfig);
        }
        ui.label(RichText::new(format!("Stored in {}", config::config_path().display())).weak().small());
        ui.separator();
        ui.strong("About");
        ui.label("Windows ISO Validator - a portable front end for MediaCreationTool.bat with native ESD / ISO downloading and hash validation.");
        ui.label(format!("Bundled script: MediaCreationTool.bat, changelog {}", self.script.changelog));
        ui.label(RichText::new(format!("Script versions: {}", self.script.versions.join(", "))).weak().small());
        ui.hyperlink_to("MediaCreationTool.bat by AveYo (script and bypass techniques)", "https://github.com/AveYo/MediaCreationTool.bat");
        ui.hyperlink_to("Official ISO link method after Fido by Pete Batard", "https://github.com/pbatard/Fido");
        ui.label(RichText::new("Downloads come from Microsoft servers only. The download page API is undocumented and Microsoft may change or rate-limit it.").weak().small());
    }

    fn ui_bottom(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if !self.tasks.is_empty() {
            for t in &self.tasks {
                ui.horizontal(|ui| {
                    let done = t.progress.done.load(Ordering::Relaxed);
                    let total = t.progress.total.load(Ordering::Relaxed);
                    let stage = t.progress.stage();
                    let text = if total > 0 {
                        format!("{} - {} / {} - {}", stage, human_bytes(done), human_bytes(total), human_rate(t.progress.rate()))
                    } else if done > 0 {
                        format!("{} - {}", stage, human_bytes(done))
                    } else {
                        stage
                    };
                    ui.label(RichText::new(&t.name).strong());
                    let bar = egui::ProgressBar::new(t.progress.fraction()).text(text).desired_width(ui.available_width() - 80.0);
                    ui.add(if total > 0 { bar } else { bar.animate(true) });
                    if ui.button("Cancel").clicked() {
                        actions.push(Action::CancelTask(t.id));
                    }
                });
            }
            ui.separator();
        }
        egui::CollapsingHeader::new(RichText::new(&self.status_line).small()).id_salt("log").show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                for l in &self.log {
                    ui.label(RichText::new(l).monospace().small());
                }
            });
        });
    }
}

fn mct_lang_code(catalog_code: &str) -> String {
    match catalog_code.split_once('-') {
        Some((l, r)) if r.len() == 2 => format!("{l}-{}", r.to_ascii_uppercase()),
        _ => catalog_code.to_string(),
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.poll_tasks();
        if !self.tasks.is_empty() || self.tab == Tab::CreateMedia {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        let mut actions: Vec<Action> = Vec::new();
        egui::Panel::top("tabs").show(root, |ui| {
            ui.add_space(4.0);
            self.ui_tabs(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").resizable(false).show(root, |ui| {
            ui.add_space(4.0);
            self.ui_bottom(ui, &mut actions);
            ui.add_space(2.0);
        });
        egui::CentralPanel::default().show(root, |ui| {
            ui.add_space(4.0);
            match self.tab {
                Tab::Catalog => self.ui_catalog(ui, &mut actions),
                Tab::MicrosoftIso => self.ui_microsoft(ui, &mut actions),
                Tab::Validate => self.ui_validate(ui, &mut actions),
                Tab::CreateMedia => self.ui_media(ui, &mut actions),
                Tab::Settings => self.ui_settings(ui, &mut actions),
            }
        });
        for a in actions {
            self.apply(&ctx, a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_codes_for_mct() {
        assert_eq!(mct_lang_code("en-us"), "en-US");
        assert_eq!(mct_lang_code("sr-latn-rs"), "sr-latn-rs");
        assert_eq!(mct_lang_code("zh-cn"), "zh-CN");
    }
}
