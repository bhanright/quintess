//! Quintess, the desktop PDF reader and editor.

// No console window behind the app in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod crash;
mod dialogs;
mod jobs;
mod render;
mod sidebar;
mod spell;
mod tab;
mod theme;
mod viewer;

fn main() -> eframe::Result {
    crash::install_handler();
    let files: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    let mut viewport = egui::ViewportBuilder::default().with_title("Quintess").with_inner_size([1280.0, 860.0]).with_min_inner_size([640.0, 420.0]).with_drag_and_drop(true);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/quintess-256.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("Quintess", options, Box::new(|cc| Ok(Box::new(app::QuintessApp::new(cc, files)))))
}
