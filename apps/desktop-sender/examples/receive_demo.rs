//! Receive mode without a phone or a second screen.
//!
//! The real receive window, fed by a synthetic camera that films an in-memory
//! sender: a 1 MiB file (or the file named on the command line) at the Max
//! preset — four version 40 codes at 60 fps — drawn at 1280×960 with a slight
//! tilt and a drifting hand, missing one flip in ten.
//!
//! ```text
//! cargo run -p desktop-sender --features receive --release --example receive_demo [-- FILE]
//! ```

use std::sync::Arc;

use desktop_sender::{
    receive::{
        camera::CameraInfo,
        synthetic::{self, Framing},
        view::{FrameGuard, ReceiveView},
    },
    theme,
};
use eframe::egui;
use optical_core::{
    qr::Ecc,
    transfer::{Transfer, TransferSettings},
};

struct Demo {
    themed: bool,
    view: ReceiveView,
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.themed {
            theme::apply(ui.ctx());
            self.themed = true;
        }
        self.view.ui(ui);
    }
}

fn main() -> eframe::Result<()> {
    let (name, bytes) = match std::env::args_os().nth(1) {
        Some(path) => {
            let bytes = std::fs::read(&path).expect("the file could not be read");
            let name = std::path::Path::new(&path)
                .file_name()
                .map_or("transfer.bin".into(), |name| {
                    name.to_string_lossy().into_owned()
                });
            (name, bytes)
        }
        None => {
            let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
            let bytes = (0..1024 * 1024)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            ("random-1MiB.bin".to_owned(), bytes)
        }
    };
    let settings = TransferSettings {
        frame_bytes: 2953,
        frames_per_second: 60,
        ecc: Ecc::Low,
        grid_codes: 4,
    };
    let cameras = vec![CameraInfo {
        id: "synthetic".to_owned(),
        name: "Synthetic camera".to_owned(),
        continuity: true,
    }];
    let source = Arc::new(move |_: Option<&str>, sink| {
        let transfer =
            Transfer::from_file(&name, "application/octet-stream", &bytes, settings, 0x5eed)
                .map_err(|error| error.to_string())?;
        let framing = Framing {
            width: 1280,
            height: 960,
            columns: 2,
            pitch: 2.55,
            degrees: 1.0,
            offset: (0.0, 0.0),
        };
        let (guard, negotiated) = synthetic::film(transfer, framing, 60.0, 10, sink);
        Ok((Box::new(guard) as FrameGuard, negotiated))
    });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 860.0])
            .with_title("Optical Transfer — receive demo"),
        ..Default::default()
    };
    eframe::run_native(
        "optical-transfer-receive-demo",
        options,
        Box::new(move |_| {
            Ok(Box::new(Demo {
                themed: false,
                view: ReceiveView::with_source(cameras, source),
            }))
        }),
    )
}
