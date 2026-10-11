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
mod tags_panel;
mod theme;
mod measure_tool;
mod object_tool;
mod tool_dialogs;
mod viewer;

fn main() -> eframe::Result {
    crash::install_handler();
    let files: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
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
    let result = eframe::run_native("Quintess", options(first_choice), Box::new({
        let files = files.clone();
        move |cc| Ok(Box::new(app::QuintessApp::new(cc, files)))
    }));
    match result {
        Err(error) if first_choice == eframe::Renderer::Glow => {
            eprintln!("OpenGL couldn't start ({error}); using wgpu instead");
            eframe::run_native("Quintess", options(eframe::Renderer::Wgpu), Box::new(move |cc| Ok(Box::new(app::QuintessApp::new(cc, files)))))
        }
        other => other,
    }
}
