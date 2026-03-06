use crate::format_size;
use crate::model::color::ColorMap;

pub fn show(ui: &mut egui::Ui, extensions: &[(Box<str>, u64)], color_map: &ColorMap) {
    ui.heading("File Types");
    ui.separator();

    let total_size: u64 = extensions.iter().map(|(_, s)| s).sum();

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            egui::Grid::new("ext_grid")
                .num_columns(3)
                .spacing([8.0, 2.0])
                .striped(true)
                .show(ui, |ui| {
                    // Header
                    ui.strong("Ext");
                    ui.strong("Size");
                    ui.strong("%");
                    ui.end_row();

                    for (ext, size) in extensions.iter().take(50) {
                        let color = color_map.get(ext);
                        let pct = if total_size > 0 {
                            *size as f64 / total_size as f64 * 100.0
                        } else {
                            0.0
                        };

                        // Color swatch + extension name
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, color);
                            ui.label(&**ext);
                        });

                        ui.label(format_size(*size));
                        ui.label(format!("{:.1}%", pct));
                        ui.end_row();
                    }
                });
        });
}
