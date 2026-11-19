mod app;
mod content;
mod loaders;
mod runtime;
mod server;
mod workspace;
mod minecraft;
mod launcher;
mod profiles;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([980.0, 680.0])
            .with_title("Lunar Launcher"),
        ..Default::default()
    };

    eframe::run_native(
        "Lunar Launcher",
        options,
        Box::new(|cc| Ok(Box::new(app::LunarApp::new(cc)))),
    )
}
