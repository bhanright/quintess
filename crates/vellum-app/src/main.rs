//! Vellum, the desktop PDF reader and editor.

// No console window behind the app in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod dialogs;
mod jobs;
mod render;
mod sidebar;
mod tab;
mod viewer;

fn main() -> eframe::Result {
    let files: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Vellum").with_inner_size([1280.0, 860.0]).with_min_inner_size([640.0, 420.0]).with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Vellum", options, Box::new(|cc| Ok(Box::new(app::VellumApp::new(cc, files)))))
}
