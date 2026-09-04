#![forbid(unsafe_code)]
// A GUI program should not also open a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use desktop_sender::app::SenderApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 780.0])
            .with_min_inner_size([720.0, 560.0])
            .with_title("Optical Transfer — Sender"),
        ..Default::default()
    };
    eframe::run_native(
        "optical-transfer-sender",
        options,
        Box::new(|_creation| Ok(Box::new(SenderApp::new()))),
    )
}
