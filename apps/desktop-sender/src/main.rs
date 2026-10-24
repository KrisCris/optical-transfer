#![forbid(unsafe_code)]
// A GUI program should not also open a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use desktop_sender::app::SenderApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 840.0])
            .with_min_inner_size([820.0, 600.0])
            .with_title("Optical Transfer")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "optical-transfer-sender",
        options,
        Box::new(|_creation| {
            Ok(Box::new(SenderApp::with_paths(
                std::env::args_os().skip(1).map(Into::into),
            )))
        }),
    )
}
