#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod content;
mod loaders;
mod runtime;
mod server;
mod workspace;
mod downloads;
mod minecraft;
mod launcher;
mod profiles;

fn main() -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!(concat!(env!("OUT_DIR"), "/moon.png")))
        .ok()
        .map(std::sync::Arc::new);
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([980.0, 680.0])
        .with_title("Lunar Launcher");
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("Lunar Launcher", options, Box::new(|cc| Ok(Box::new(app::LunarApp::new(cc)))))
}
