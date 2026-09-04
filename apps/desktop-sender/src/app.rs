//! The sender window.
//!
//! Two things here are load-bearing rather than cosmetic.
//!
//! **The cadence is absolute, not incremental.** Frames are due at
//! `started_at + n * interval`, never `last_frame + interval`. An incremental
//! clock accumulates every scheduling delay, so a window that stalls for a
//! moment permanently runs slow — and the receiver, which is counting on a
//! steady carousel, has no way to tell that from a slow sender.
//!
//! **Frames are generated ahead of the deadline.** Encoding a fountain frame
//! and rasterising a QR code is a millisecond or two; doing it at the moment a
//! frame is due means every frame is late by however long it took to make. A
//! small queue absorbs that.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use eframe::egui;

use crate::{
    packing::{PackingJob, PackingState},
    qr::{Ecc, GRID_OPTIONS, QrFrame, grid_dims},
    transfer::{FRAME_BYTES_OPTIONS, TX_FPS_OPTIONS, Transfer, TransferSettings, human_bytes},
};

/// Frames held ahead of the deadline, per code on screen.
const LOOKAHEAD_PER_CODE: usize = 3;

/// A repaint this far ahead of a deadline is close enough; asking egui for a
/// sub-millisecond wake-up just burns a wake-up.
const REPAINT_SLACK: Duration = Duration::from_micros(500);

/// The one thing the UI must never stop saying.
const SECURITY_WARNING: &str = "Not encrypted: any camera can read this transfer.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    File,
    Text,
}

/// A file the user chose, held as bytes so packing can move to another thread.
struct Selection {
    name: String,
    media_type: String,
    bytes: Vec<u8>,
}

/// `Stream` is much larger than the other two variants -- it owns the encoder's
/// block buffer, which is the whole payload -- so it is boxed. Idle is the
/// common state and should not cost a payload-sized allocation to sit in.
enum Stage {
    Idle,
    Preparing(PackingJob),
    Streaming(Box<Stream>),
}

/// A running stream and everything the display loop needs to keep it steady.
struct Stream {
    transfer: Transfer,
    renderer: crate::qr::FrameRenderer,
    /// Ready frames, oldest first.
    queue: VecDeque<QrFrame>,
    /// One painted frame per grid cell, so a repaint that is not a flip still
    /// has something to draw.
    cells: Vec<Option<QrFrame>>,
    cell_cursor: usize,
    cols: usize,
    rows: usize,
    started_at: Instant,
    /// Flips performed. The next is due at `started_at + flips * sub_interval`.
    flips: u64,
    sub_interval: Duration,
    /// Set when frame generation fails mid-stream, which should be impossible
    /// after the pre-flight render but must not spin the UI if it happens.
    generator_error: Option<String>,
    /// Longest gap observed between due-frame checks, so a stalled window can
    /// say so instead of silently freezing on an unreadable half-drawn frame.
    worst_stall: Duration,
    last_tick: Instant,
}

pub struct SenderApp {
    tab: Tab,
    settings: TransferSettings,
    selection: Option<Selection>,
    text: String,
    stage: Stage,
    error: Option<String>,
    /// Kept across restarts so the texture is reused rather than reallocated.
    texture: Option<egui::TextureHandle>,
    fullscreen_stage: bool,
}

impl Default for SenderApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SenderApp {
    pub fn new() -> Self {
        Self {
            tab: Tab::File,
            settings: TransferSettings::default(),
            selection: None,
            text: String::new(),
            stage: Stage::Idle,
            error: None,
            texture: None,
            fullscreen_stage: false,
        }
    }

    fn streaming(&self) -> bool {
        matches!(self.stage, Stage::Streaming(_))
    }

    fn busy(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }

    /// A session id is 16 random bits, redrawn on every start, and never zero —
    /// so restarting the sender genuinely reshuffles the stream rather than
    /// replaying the previous session's frames into a receiver that is still
    /// watching.
    fn new_session_id() -> u16 {
        use rand::RngExt as _;
        rand::rng().random_range(1..=u16::MAX)
    }

    fn start(&mut self) {
        self.error = None;
        let session_id = Self::new_session_id();
        let job = match self.tab {
            Tab::File => {
                let Some(selection) = &self.selection else {
                    self.error = Some("Choose a file first.".to_owned());
                    return;
                };
                PackingJob::file(
                    selection.name.clone(),
                    selection.media_type.clone(),
                    selection.bytes.clone(),
                    self.settings,
                    session_id,
                )
            }
            Tab::Text => {
                if self.text.trim().is_empty() {
                    self.error = Some("Type or paste some text first.".to_owned());
                    return;
                }
                PackingJob::text(self.text.clone(), self.settings, session_id)
            }
        };
        self.stage = Stage::Preparing(job);
    }

    fn stop(&mut self) {
        self.stage = Stage::Idle;
        self.fullscreen_stage = false;
    }

    fn choose_file(&mut self) {
        let Some(path) = rfd::FileDialog::new().pick_file() else {
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "transfer.bin".to_owned());
        match std::fs::read(&path) {
            Ok(bytes) => {
                let media_type = mime_guess::from_path(&path)
                    .first_raw()
                    .unwrap_or("application/octet-stream")
                    .to_owned();
                self.error = None;
                self.selection = Some(Selection {
                    name,
                    media_type,
                    bytes,
                });
            }
            Err(error) => self.error = Some(format!("That file could not be read: {error}.")),
        }
    }

    /// Advance the preparation job, promoting it to a stream when it finishes.
    fn poll_packing(&mut self, ctx: &egui::Context) {
        let Stage::Preparing(job) = &mut self.stage else {
            return;
        };
        match job.poll() {
            PackingState::Busy => {
                // The worker owns the clock here; a modest repaint keeps the
                // spinner honest without polling in a tight loop.
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            PackingState::Ready(transfer) => {
                self.stage = Stage::Streaming(Box::new(Stream::new(*transfer)));
            }
            PackingState::Failed(error) => {
                self.error = Some(error.to_string());
                self.stage = Stage::Idle;
            }
            PackingState::Lost => {
                self.error = Some("Preparing the transfer stopped unexpectedly.".to_owned());
                self.stage = Stage::Idle;
            }
        }
    }
}

impl Stream {
    fn new(transfer: Transfer) -> Self {
        let settings = transfer.settings();
        let (cols, rows) = grid_dims(settings.grid_codes).unwrap_or((1, 1));
        let codes = cols * rows;
        // Every cell refreshes at the chosen rate, but cell j flips at phase
        // j/N of the interval rather than all N flipping together: a camera
        // exposure straddling a flip then catches at most one code mid
        // transition, and the other N-1 sit stable under it.
        let sub_interval = Duration::from_secs_f64(
            1.0 / f64::from(settings.frames_per_second.max(1)) / codes as f64,
        );
        let renderer = transfer.renderer();
        let now = Instant::now();
        let mut stream = Self {
            transfer,
            renderer,
            queue: VecDeque::new(),
            cells: (0..codes).map(|_| None).collect(),
            cell_cursor: 0,
            cols,
            rows,
            started_at: now,
            flips: 0,
            sub_interval,
            generator_error: None,
            worst_stall: Duration::ZERO,
            last_tick: now,
        };
        stream.pump(LOOKAHEAD_PER_CODE * codes);
        stream
    }

    fn lookahead(&self) -> usize {
        LOOKAHEAD_PER_CODE * self.cells.len()
    }

    /// Generate up to `max` frames, stopping at the lookahead.
    fn pump(&mut self, max: usize) {
        if self.generator_error.is_some() {
            return;
        }
        for _ in 0..max {
            if self.queue.len() >= self.lookahead() {
                break;
            }
            let payload = self.transfer.next_payload();
            match self.renderer.render(&payload) {
                Ok(frame) => self.queue.push_back(frame),
                Err(error) => {
                    self.generator_error = Some(format!("This frame could not be drawn: {error}."));
                    break;
                }
            }
        }
    }

    /// When the next flip is due, measured from the stream's own start.
    fn next_deadline(&self) -> Instant {
        self.started_at + self.sub_interval * self.flips as u32
    }

    /// Flip every cell that has come due. Returns true if anything changed.
    fn advance(&mut self, now: Instant) -> bool {
        if self.generator_error.is_some() {
            return false;
        }
        let since_last = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        if since_last > Duration::from_secs(1) {
            self.worst_stall = self.worst_stall.max(since_last);
            // A long stall left a backlog no camera ever saw. Restart the
            // cadence from here rather than bursting it out at once.
            self.started_at = now;
            self.flips = 0;
        }

        let mut painted = false;
        // Bounded by the reset above: the debt can never exceed one interval,
        // so this flips at most one full grid.
        while now >= self.next_deadline() {
            let Some(frame) = self.queue.pop_front() else {
                // Starved. Give the generator a beat rather than spinning.
                self.started_at = now;
                self.flips = 0;
                break;
            };
            self.pump(1);
            self.cells[self.cell_cursor] = Some(frame);
            self.cell_cursor = (self.cell_cursor + 1) % self.cells.len();
            self.flips += 1;
            painted = true;
        }
        painted
    }

    /// The whole grid as one image, quiet zones included.
    ///
    /// One `Color32` per module: scaling happens at draw time with nearest
    /// filtering, because a smoothed QR code is an undecodable one.
    fn compose(&self) -> Option<egui::ColorImage> {
        let cell = self.cells.iter().flatten().next().map(|frame| frame.size)?;
        let width = cell * self.cols;
        let height = cell * self.rows;
        let mut pixels = vec![egui::Color32::WHITE; width * height];
        for (index, frame) in self.cells.iter().enumerate() {
            let Some(frame) = frame else { continue };
            let origin_x = (index % self.cols) * cell;
            let origin_y = (index / self.cols) * cell;
            for y in 0..frame.size {
                let row = (origin_y + y) * width + origin_x;
                for x in 0..frame.size {
                    if frame.pixels[y * frame.size + x] {
                        pixels[row + x] = egui::Color32::BLACK;
                    }
                }
            }
        }
        Some(egui::ColorImage {
            size: [width, height],
            pixels,
            source_size: egui::vec2(width as f32, height as f32),
        })
    }
}

impl eframe::App for SenderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.poll_packing(ctx);

        if let Stage::Streaming(stream) = &mut self.stage {
            let now = Instant::now();
            let changed = stream.advance(now);
            if let Some(error) = stream.generator_error.clone() {
                self.error = Some(error);
                self.stage = Stage::Idle;
            } else {
                if changed && let Some(image) = stream.compose() {
                    match &mut self.texture {
                        Some(texture) => texture.set(image, egui::TextureOptions::NEAREST),
                        None => {
                            self.texture = Some(ctx.load_texture(
                                "qr-stage",
                                image,
                                egui::TextureOptions::NEAREST,
                            ));
                        }
                    }
                }
                // Ask for exactly the next deadline. egui sleeps until then
                // instead of repainting at the display's rate for no reason.
                let wait = stream
                    .next_deadline()
                    .saturating_duration_since(Instant::now());
                ctx.request_repaint_after(wait.saturating_sub(REPAINT_SLACK));
            }
        }

        if self.fullscreen_stage && self.streaming() {
            self.draw_fullscreen(ui);
            return;
        }

        egui::Panel::bottom("warning")
            .resizable(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("⚠").color(egui::Color32::from_rgb(0xd9, 0x8a, 0x00)),
                    );
                    ui.label(
                        egui::RichText::new(SECURITY_WARNING)
                            .color(egui::Color32::from_rgb(0xd9, 0x8a, 0x00))
                            .strong(),
                    );
                });
                ui.add_space(4.0);
            });

        egui::Panel::left("controls")
            .resizable(false)
            .exact_size(340.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.draw_controls(ui));
            });

        egui::CentralPanel::default().show(ui, |ui| self.draw_stage(ui));
    }
}

impl SenderApp {
    fn draw_controls(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.heading("Optical transfer");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::File, "File");
            ui.selectable_value(&mut self.tab, Tab::Text, "Text");
        });
        ui.separator();

        let locked = self.busy();
        ui.add_enabled_ui(!locked, |ui| match self.tab {
            Tab::File => {
                if ui.button("Choose file…").clicked() {
                    self.choose_file();
                }
                match &self.selection {
                    Some(selection) => {
                        ui.label(egui::RichText::new(&selection.name).strong());
                        ui.label(format!(
                            "{} · {}",
                            human_bytes(selection.bytes.len()),
                            selection.media_type
                        ));
                    }
                    None => {
                        ui.label(egui::RichText::new("No file chosen").weak());
                    }
                }
            }
            Tab::Text => {
                ui.label("Text to send");
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .desired_rows(6)
                        .desired_width(f32::INFINITY)
                        .hint_text("Type or paste anything…"),
                );
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new("Transmission").strong());
        ui.add_enabled_ui(!locked, |ui| {
            egui::Grid::new("settings")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Bytes / frame");
                    egui::ComboBox::from_id_salt("frame-bytes")
                        .selected_text(self.settings.frame_bytes.to_string())
                        .show_ui(ui, |ui| {
                            for option in FRAME_BYTES_OPTIONS {
                                ui.selectable_value(
                                    &mut self.settings.frame_bytes,
                                    option,
                                    option.to_string(),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label("Frames / second");
                    egui::ComboBox::from_id_salt("fps")
                        .selected_text(self.settings.frames_per_second.to_string())
                        .show_ui(ui, |ui| {
                            for option in TX_FPS_OPTIONS {
                                ui.selectable_value(
                                    &mut self.settings.frames_per_second,
                                    option,
                                    option.to_string(),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label("Error correction");
                    egui::ComboBox::from_id_salt("ecc")
                        .selected_text(self.settings.ecc.label())
                        .show_ui(ui, |ui| {
                            for option in Ecc::ALL {
                                ui.selectable_value(&mut self.settings.ecc, option, option.label());
                            }
                        });
                    ui.end_row();

                    ui.label("Codes on screen");
                    egui::ComboBox::from_id_salt("grid")
                        .selected_text(self.settings.grid_codes.to_string())
                        .show_ui(ui, |ui| {
                            for option in GRID_OPTIONS {
                                ui.selectable_value(
                                    &mut self.settings.grid_codes,
                                    option,
                                    option.to_string(),
                                );
                            }
                        });
                    ui.end_row();
                });
        });

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if self.streaming() || matches!(self.stage, Stage::Preparing(_)) {
                if ui.button("Stop").clicked() {
                    self.stop();
                }
            } else if ui.button("Start sending").clicked() {
                self.start();
            }
            if self.streaming() && ui.button("Fullscreen").clicked() {
                self.fullscreen_stage = true;
            }
        });

        if let Some(error) = &self.error {
            ui.add_space(8.0);
            ui.colored_label(egui::Color32::from_rgb(0xc0, 0x30, 0x30), error);
        }

        if let Stage::Streaming(stream) = &self.stage {
            ui.add_space(12.0);
            ui.separator();
            ui.label(egui::RichText::new("Stream").strong());
            egui::Grid::new("stats")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Sending");
                    ui.label(stream.transfer.name());
                    ui.end_row();
                    ui.label("Frame");
                    ui.label(format!(
                        "{} of {} per cycle",
                        stream.transfer.sent_frames() % stream.transfer.cycle_frames().max(1),
                        stream.transfer.cycle_frames()
                    ));
                    ui.end_row();
                    ui.label("Cycles");
                    ui.label(stream.transfer.cycles_completed().to_string());
                    ui.end_row();
                    ui.label("Payload");
                    ui.label(format!(
                        "{} in {} blocks",
                        human_bytes(stream.transfer.transmitted_size()),
                        stream.transfer.source_blocks()
                    ));
                    ui.end_row();
                    ui.label("Compression");
                    ui.label(match stream.transfer.compression() {
                        optical_core::container::Compression::Gzip => format!(
                            "gzip, {} → {}",
                            human_bytes(stream.transfer.original_size()),
                            human_bytes(stream.transfer.transmitted_size())
                        ),
                        optical_core::container::Compression::None => "none".to_owned(),
                    });
                    ui.end_row();
                    ui.label("Code");
                    ui.label(format!(
                        "V{} · ECC {}{}",
                        stream.renderer.locked_version().unwrap_or(0),
                        stream.transfer.settings().ecc.label(),
                        if stream.cells.len() > 1 {
                            format!(" · ×{}", stream.cells.len())
                        } else {
                            String::new()
                        }
                    ));
                    ui.end_row();
                    ui.label("Session");
                    ui.label(format!("{:#06x}", stream.transfer.session_id()));
                    ui.end_row();
                });
            if stream.worst_stall > Duration::from_secs(1) {
                ui.add_space(6.0);
                ui.colored_label(
                    egui::Color32::from_rgb(0xd9, 0x8a, 0x00),
                    format!(
                        "This window stalled for {:.1}s. Keep it visible and in front — a \
                         throttled window freezes the stream mid-frame.",
                        stream.worst_stall.as_secs_f32()
                    ),
                );
            }
        }
    }

    fn draw_stage(&mut self, ui: &mut egui::Ui) {
        match &self.stage {
            Stage::Idle => {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new(
                            "Choose a file or type some text, then start sending.\n\
                             Point the receiving phone's camera at the code.",
                        )
                        .weak(),
                    );
                });
            }
            Stage::Preparing(job) => {
                ui.centered_and_justified(|ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spinner();
                        ui.label(format!("Preparing {}…", job.label()));
                    });
                });
            }
            Stage::Streaming(_) => {
                if let Some(texture) = &self.texture {
                    paint_codes(ui, texture);
                }
            }
        }
    }

    /// Codes edge to edge, on white, with nothing else on screen.
    ///
    /// The point of the mode is "as big as this display goes", so the layout
    /// deliberately ignores every other control rather than reserving room for
    /// them. Any click or Escape returns.
    fn draw_fullscreen(&mut self, ui: &mut egui::Ui) {
        let dismissed = ui
            .ctx()
            .input(|input| input.key_pressed(egui::Key::Escape) || input.pointer.any_click());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::WHITE))
            .show(ui, |ui| {
                if let Some(texture) = &self.texture {
                    paint_codes(ui, texture);
                }
            });
        if dismissed {
            self.fullscreen_stage = false;
        }
    }
}

/// Draw the grid as large as it goes without smoothing or fractional modules.
///
/// The integer scale is the point: at a fractional scale some modules get one
/// pixel and their neighbours get two, which is the same defect as smoothing
/// wearing a different hat.
fn paint_codes(ui: &mut egui::Ui, texture: &egui::TextureHandle) {
    let available = ui.available_size();
    let [width, height] = texture.size();
    if width == 0 || height == 0 {
        return;
    }
    let scale = (available.x / width as f32)
        .min(available.y / height as f32)
        .floor()
        .max(1.0);
    let size = egui::vec2(width as f32 * scale, height as f32 * scale);
    ui.centered_and_justified(|ui| {
        ui.add(
            egui::Image::new(texture)
                .fit_to_exact_size(size)
                .texture_options(egui::TextureOptions::NEAREST),
        );
    });
}
