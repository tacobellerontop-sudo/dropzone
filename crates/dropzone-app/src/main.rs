// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod archive_ops;
mod common;
mod details;
mod nav;
mod queue_panel;
mod rename_win;
mod sidebar;
mod thumbs;
mod tool_glue;
mod tools;
mod view;

use eframe::egui;

fn main() -> eframe::Result {
    // Files passed on the command line ("Open with", "Send to") are added on startup.
    let initial: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(std::path::PathBuf::from).filter(|p| p.exists()).collect();

    let mut viewport =
        egui::ViewportBuilder::default().with_title("Dropzone").with_inner_size([1200.0, 760.0]).with_min_inner_size([820.0, 520.0]).with_drag_and_drop(true);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Glow, ..Default::default() };
    eframe::run_native("Dropzone", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, initial)))))
}
