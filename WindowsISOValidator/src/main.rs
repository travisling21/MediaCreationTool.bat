#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod bat;
mod catalog;
mod config;
mod download;
mod hashing;
mod iso;
mod mct;
mod msdl;
mod util;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1140.0, 780.0])
            .with_min_inner_size([920.0, 600.0])
            .with_title("Windows ISO Validator"),
        ..Default::default()
    };
    eframe::run_native("Windows ISO Validator", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
