mod app;
mod content;
mod loaders;
mod minecraft;
mod launcher;
mod profiles;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([900.0, 620.0])
            .with_title("LUNAR"),
        ..Default::default()
    };

    eframe::run_native(
        "LUNAR",
        options,
        Box::new(|cc| Ok(Box::new(app::LunarApp::new(cc)))),
    )
}
