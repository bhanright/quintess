//! Quintess, the desktop PDF reader and editor.
//!
//! This is a library: the `quintess` program (crates/quintess-cli) starts
//! the app with `run_app` when it isn't given a command-line command, so
//! the app and the command-line tool share one executable and one copy of
//! quintess-core.

mod app;
mod crash;
mod desktop;
mod dialogs;
mod jobs;
mod render;
mod sidebar;
mod spell;
mod tab;
mod tags_panel;
mod theme;
mod measure_tool;
mod object_tool;
mod tool_dialogs;
mod viewer;

use std::path::PathBuf;
use std::process::ExitCode;

/// Opens the app's window with these files (none for an empty window) and
/// returns when it closes.
pub fn run_app(files: Vec<PathBuf>) -> ExitCode {
    crash::install_handler();
    match run_window(files) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Quintess couldn't open its window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_window(files: Vec<PathBuf>) -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default().with_title("Quintess").with_app_id("quintess").with_inner_size([1280.0, 860.0]).with_min_inner_size([640.0, 420.0]).with_drag_and_drop(true);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/quintess-256.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    // OpenGL ("glow") is the default: measured on a one-page document it
    // needs about half the memory of wgpu (about 200 MB against 400 MB),
    // which mostly goes to setting up a Vulkan or DirectX device. When a
    // computer's OpenGL driver can't start, wgpu takes over.
    // QUINTESS_RENDERER=wgpu chooses wgpu outright.
    let preference = quintess_core::settings::Settings::load().graphics;
    let choice = std::env::var("QUINTESS_RENDERER").unwrap_or(preference);
    let first_choice = if choice == "wgpu" { eframe::Renderer::Wgpu } else { eframe::Renderer::Glow };
    let options = |renderer| eframe::NativeOptions { renderer, viewport: viewport.clone(), ..Default::default() };
    // Before the window opens, so a document the Mac's Finder launched the
    // app with is caught (see desktop.rs).
    let desktop_events = desktop::DesktopEvents::install();
    let result = eframe::run_native("Quintess", options(first_choice), Box::new({
        let files = files.clone();
        let inbox = desktop_events.inbox();
        move |cc| Ok(Box::new(app::QuintessApp::new(cc, files, inbox)))
    }));
    match result {
        Err(error) if first_choice == eframe::Renderer::Glow => {
            eprintln!("OpenGL couldn't start ({error}); using wgpu instead");
            let inbox = desktop_events.inbox();
            eframe::run_native("Quintess", options(eframe::Renderer::Wgpu), Box::new(move |cc| Ok(Box::new(app::QuintessApp::new(cc, files, inbox)))))
        }
        other => other,
    }
}
