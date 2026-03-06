fn main() -> eframe::Result<()> {
    let initial_path = std::env::args().nth(1);

    let icon = load_icon();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("MacDirStat")
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "MacDirStat",
        options,
        Box::new(move |cc| Ok(Box::new(macdirstat::app::App::new(cc, initial_path)))),
    )
}

fn load_icon() -> egui::IconData {
    let png_bytes = include_bytes!("../launcher_icon.png");
    let img = image::load_from_memory(png_bytes)
        .expect("Failed to decode app icon")
        .into_rgba8();
    let (w, h) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width: w,
        height: h,
    }
}
