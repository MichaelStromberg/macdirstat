fn main() -> eframe::Result<()> {
    let initial_path = std::env::args().nth(1);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("MacDirStat"),
        ..Default::default()
    };

    eframe::run_native(
        "MacDirStat",
        options,
        Box::new(move |cc| Ok(Box::new(macdirstat::app::App::new(cc, initial_path)))),
    )
}
