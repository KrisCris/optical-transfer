//! The sender window.
//!
//! Layout follows the browser sender: what to send and how fast on the left,
//! the stage and its live numbers on the right. The stage is what matters, so
//! three of its properties are decode requirements rather than styling:
//!
//! * **Whole device pixels per module, then a smooth stretch.** The texture
//!   carries the largest whole number of device pixels per module that fits,
//!   and the GPU stretches the remaining fraction linearly, the way the browser
//!   sender does. Nearest-neighbour stretching would give neighbouring modules
//!   widths a pixel apart; an integer-only scale would waste up to half the
//!   stage when several codes share it.
//! * **The grid follows the window.** Several codes tile side by side on a
//!   wide window and stack on a tall one, whichever gives modules more pixels.
//! * **The display drives the clock.** While streaming the window repaints on
//!   every vsync and flips whatever has come due, like `requestAnimationFrame`
//!   in the browser. A timer-driven repaint lands a vsync late often enough to
//!   halve the effective frame rate.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, Margin, Rect, RichText, Stroke, StrokeKind,
    TextureOptions, Vec2, pos2, vec2,
};

use optical_core::{
    MAX_FILE_BYTES,
    bundle::{MAX_BUNDLE_FILES, SelectedFile},
    container::Compression,
    protocol::HEADER_LEN,
    qr::{Ecc, GRID_OPTIONS},
    snippet::MAX_SNIPPET_BYTES,
    transfer::{FRAME_BYTES_OPTIONS, TX_FPS_OPTIONS, TransferSettings, human_bytes},
};

use crate::{
    packing::{PackingJob, PackingState},
    stream::{FlipClock, Frame, FrameSource, best_grid},
    theme,
};

/// The one thing the UI must never stop saying.
const SECURITY_WARNING: &str = "Not encrypted";
const SECURITY_DETAIL: &str = "Anyone with a camera pointed at this screen can read the transfer.";

/// The browser sender's presets, so advice like "use Balanced" means the same
/// numbers on both.
pub struct Preset {
    pub name: &'static str,
    pub frame_bytes: usize,
    pub fps: u32,
    pub codes: usize,
}

pub const PRESETS: [Preset; 4] = [
    Preset {
        name: "Reliable",
        frame_bytes: 1465,
        fps: 24,
        codes: 1,
    },
    Preset {
        name: "Balanced",
        frame_bytes: 2331,
        fps: 30,
        codes: 2,
    },
    Preset {
        name: "Fast",
        frame_bytes: 2953,
        fps: 60,
        codes: 1,
    },
    Preset {
        name: "Max",
        frame_bytes: 2953,
        fps: 60,
        codes: 4,
    },
];
const DEFAULT_PRESET: usize = 2;

impl Preset {
    fn matches(&self, settings: &TransferSettings) -> bool {
        settings.frame_bytes == self.frame_bytes
            && settings.frames_per_second == self.fps
            && settings.grid_codes == self.codes
            && settings.ecc == Ecc::Low
    }

    fn apply(&self, settings: &mut TransferSettings) {
        settings.frame_bytes = self.frame_bytes;
        settings.frames_per_second = self.fps;
        settings.grid_codes = self.codes;
        settings.ecc = Ecc::Low;
    }
}

/// Which direction the window works in. Receive mode exists only in builds
/// with the `receive` feature.
#[cfg(feature = "receive")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Send,
    Receive,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Files,
    Text,
}

/// `Streaming` owns a worker and a texture; boxed so idling costs nothing.
enum Stage {
    Idle,
    Preparing(PackingJob),
    Streaming(Box<Live>),
}

/// A running stream: the frame worker, the flip clock and the stage texture.
struct Live {
    source: FrameSource,
    clock: FlipClock,
    cells: Vec<Option<Frame>>,
    dirty: Vec<bool>,
    /// Modules per side of one code, quiet zone included. Zero until the
    /// first frame arrives.
    cell: usize,
    version: u8,
    grid: (usize, usize),
    /// Device pixels per module baked into the texture; the GPU stretches
    /// the last fraction of a pixel smoothly.
    upscale: usize,
    texture: Option<egui::TextureHandle>,
    file_count: usize,
}

impl Live {
    fn new(source: FrameSource, file_count: usize) -> Self {
        let settings = source.info.settings;
        let codes = settings.grid_codes.max(1);
        Self {
            clock: FlipClock::new(settings.frames_per_second, codes, Instant::now()),
            source,
            cells: (0..codes).map(|_| None).collect(),
            dirty: vec![false; codes],
            cell: 0,
            version: 0,
            grid: (0, 0),
            upscale: 1,
            texture: None,
            file_count,
        }
    }

    /// Flip whatever is due. `Err` means the worker failed and the stream is over.
    fn tick(&mut self, now: Instant) -> Result<(), String> {
        let mut failure = None;
        let source = &self.source;
        let cells = &mut self.cells;
        let dirty = &mut self.dirty;
        self.clock.tick(
            now,
            || match source.try_next() {
                Some(Ok(frame)) => Some(frame),
                Some(Err(error)) => {
                    failure = Some(error);
                    None
                }
                None => None,
            },
            |index, frame| {
                cells[index] = Some(frame);
                dirty[index] = true;
            },
        );
        if let Some(frame) = self.cells.iter().flatten().next() {
            self.cell = frame.size;
            self.version = frame.version;
        }
        failure.map_or(Ok(()), Err)
    }

    /// Frame rate and code count change live; the stream keeps its place.
    fn reconfigure(&mut self, fps: u32, codes: usize) {
        let codes = codes.max(1);
        self.cells.resize_with(codes, || None);
        self.dirty = vec![true; codes];
        self.clock.configure(fps, codes);
        self.source.info.settings.frames_per_second = fps;
        self.source.info.settings.grid_codes = codes;
        self.grid = (0, 0);
    }

    /// Bring the texture up to date for `grid` at `upscale` texels per
    /// module: a full upload when either changed, otherwise one partial
    /// upload per cell that flipped.
    fn sync_texture(&mut self, ctx: &egui::Context, grid: (usize, usize), upscale: usize) {
        let side = self.cell * upscale;
        let (cols, rows) = grid;
        let size = [cols * side, rows * side];
        let origin = |index: usize| [(index % cols) * side, (index / cols) * side];
        let reshaped = grid != self.grid
            || upscale != self.upscale
            || self
                .texture
                .as_ref()
                .is_none_or(|texture| texture.size() != size);
        let cell = self.cell;
        let current = |frame: &Option<Frame>| {
            frame
                .as_ref()
                .filter(|frame| frame.size == cell)
                .map(|frame| frame.image.clone())
        };
        if reshaped {
            self.grid = grid;
            self.upscale = upscale;
            let mut image = ColorImage::filled(size, Color32::WHITE);
            for (index, frame) in self.cells.iter().enumerate() {
                if let Some(code) = current(frame) {
                    blit(&mut image, &code, origin(index), upscale);
                }
            }
            // Linear, not nearest: the texture is already whole pixels per
            // module, so filtering only ever smooths the final stretch of
            // under one pixel, which nearest would turn into uneven modules.
            match &mut self.texture {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => {
                    self.texture = Some(ctx.load_texture("qr-stage", image, TextureOptions::LINEAR))
                }
            }
            self.dirty.fill(false);
            return;
        }
        let Some(texture) = &mut self.texture else {
            return;
        };
        for (index, dirty) in self.dirty.iter_mut().enumerate() {
            if !std::mem::take(dirty) {
                continue;
            }
            let Some(code) = current(&self.cells[index]) else {
                continue;
            };
            if upscale == 1 {
                texture.set_partial(origin(index), code, TextureOptions::LINEAR);
            } else {
                let mut scaled = ColorImage::filled([side, side], Color32::WHITE);
                blit(&mut scaled, &code, [0, 0], upscale);
                texture.set_partial(origin(index), scaled, TextureOptions::LINEAR);
            }
        }
    }
}

/// Copy `code` into `target` at `at`, each module as an `upscale`-square block.
fn blit(target: &mut ColorImage, code: &ColorImage, at: [usize; 2], upscale: usize) {
    let [side, _] = code.size;
    let width = target.size[0];
    let mut row = vec![Color32::WHITE; side * upscale];
    for y in 0..side {
        for (x, pixel) in code.pixels[y * side..(y + 1) * side].iter().enumerate() {
            row[x * upscale..(x + 1) * upscale].fill(*pixel);
        }
        for repeat in 0..upscale {
            let start = (at[1] + y * upscale + repeat) * width + at[0];
            target.pixels[start..start + row.len()].copy_from_slice(&row);
        }
    }
}

pub struct SenderApp {
    themed: bool,
    tab: Tab,
    settings: TransferSettings,
    files: Vec<SelectedFile>,
    text: String,
    stage: Stage,
    status: String,
    error: Option<String>,
    fullscreen: bool,
    /// Set once the OS reports the window fullscreen, so leaving through the
    /// window's own controls also leaves the stage-only view.
    os_fullscreen: bool,
    pending_count: usize,
    #[cfg(feature = "receive")]
    mode: Mode,
    /// Built on first use: listing cameras wakes AVFoundation, which a
    /// session that only sends should not pay for.
    #[cfg(feature = "receive")]
    receive: Option<crate::receive::view::ReceiveView>,
}

impl Default for SenderApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SenderApp {
    pub fn new() -> Self {
        let mut settings = TransferSettings::default();
        PRESETS[DEFAULT_PRESET].apply(&mut settings);
        Self {
            themed: false,
            tab: Tab::Files,
            settings,
            files: Vec::new(),
            text: String::new(),
            stage: Stage::Idle,
            status: "Choose what to send, then start.".to_owned(),
            error: None,
            fullscreen: false,
            os_fullscreen: false,
            pending_count: 0,
            #[cfg(feature = "receive")]
            mode: Mode::Send,
            #[cfg(feature = "receive")]
            receive: None,
        }
    }

    #[cfg(feature = "receive")]
    fn receiving(&self) -> bool {
        self.mode == Mode::Receive
    }

    #[cfg(not(feature = "receive"))]
    fn receiving(&self) -> bool {
        false
    }

    /// Start with files or folders already chosen, as passed on the command
    /// line: `desktop-sender report.pdf photos/`.
    pub fn with_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut app = Self::new();
        let paths: Vec<_> = paths.into_iter().collect();
        if !paths.is_empty() {
            app.add_paths(paths);
        }
        app
    }

    fn busy(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }

    /// Sixteen random bits, redrawn on every start and never zero, so a
    /// restarted sender reshuffles the stream instead of replaying the last
    /// session into a receiver that is still watching.
    fn new_session_id() -> u16 {
        use rand::RngExt as _;
        rand::rng().random_range(1..=u16::MAX)
    }

    fn start(&mut self) {
        self.error = None;
        let session_id = Self::new_session_id();
        let job = match self.tab {
            Tab::Files => {
                if self.files.is_empty() {
                    self.error = Some("Choose at least one file first.".to_owned());
                    return;
                }
                self.pending_count = self.files.len();
                let files = self.files.iter().map(copy_file).collect();
                PackingJob::files(files, self.settings, session_id)
            }
            Tab::Text => {
                if self.text.trim().is_empty() {
                    self.error = Some("Type or paste some text first.".to_owned());
                    return;
                }
                if self.text.len() > MAX_SNIPPET_BYTES {
                    self.error =
                        Some("Text is limited to 4 MiB; send it as a file instead.".to_owned());
                    return;
                }
                self.pending_count = 1;
                PackingJob::text(self.text.clone(), self.settings, session_id)
            }
        };
        self.status = format!("Preparing {}…", job.label());
        self.stage = Stage::Preparing(job);
    }

    fn stop(&mut self, ctx: &egui::Context) {
        if self.busy() {
            self.status = "Stopped.".to_owned();
        }
        self.stage = Stage::Idle;
        self.set_fullscreen(ctx, false);
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if self.fullscreen == on {
            return;
        }
        self.fullscreen = on;
        self.os_fullscreen = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    // ------------------------------------------------------------- selection

    fn add_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        if self.busy() {
            self.error = Some("Stop sending before changing the selection.".to_owned());
            return;
        }
        self.tab = Tab::Files;
        self.error = None;
        for path in paths {
            let result = if path.is_dir() {
                self.add_folder(&path)
            } else {
                self.add_file(&path, None)
            };
            if let Err(error) = result {
                self.error = Some(error);
                break;
            }
        }
    }

    fn add_file(&mut self, path: &Path, relative_path: Option<String>) -> Result<(), String> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "transfer.bin".to_owned());
        if self.files.len() >= MAX_BUNDLE_FILES {
            return Err(format!("Choose at most {MAX_BUNDLE_FILES} files."));
        }
        let size = std::fs::metadata(path)
            .map_err(|error| format!("{name} could not be read: {error}."))?
            .len() as usize;
        if self.total_bytes() + size > MAX_FILE_BYTES {
            return Err("That is over the 64 MiB limit, including ZIP overhead.".to_owned());
        }
        let bytes =
            std::fs::read(path).map_err(|error| format!("{name} could not be read: {error}."))?;
        let media_type = mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_owned();
        self.files.push(SelectedFile {
            name,
            media_type,
            bytes,
            relative_path,
        });
        Ok(())
    }

    /// Every file under `root`, with paths that start at the folder's own name
    /// — what a browser's folder picker reports, so both senders produce the
    /// same ZIP. Hidden entries and symbolic links are skipped: `.DS_Store` is
    /// nobody's intent, and a link can point back up the tree.
    fn add_folder(&mut self, root: &Path) -> Result<(), String> {
        let base = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut pending = vec![(root.to_path_buf(), base)];
        while let Some((directory, prefix)) = pending.pop() {
            let mut entries: Vec<_> = std::fs::read_dir(&directory)
                .map_err(|error| format!("{prefix} could not be read: {error}."))?
                .filter_map(Result::ok)
                .collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if name.starts_with('.') || kind.is_symlink() {
                    continue;
                }
                let relative = format!("{prefix}/{name}");
                if kind.is_dir() {
                    pending.push((entry.path(), relative));
                } else if kind.is_file() {
                    self.add_file(&entry.path(), Some(relative))?;
                }
            }
        }
        Ok(())
    }

    fn total_bytes(&self) -> usize {
        self.files.iter().map(|file| file.bytes.len()).sum()
    }

    // ------------------------------------------------------------- per frame

    fn poll_packing(&mut self, ctx: &egui::Context) {
        let Stage::Preparing(job) = &mut self.stage else {
            return;
        };
        match job.poll() {
            PackingState::Busy => ctx.request_repaint_after(Duration::from_millis(50)),
            PackingState::Ready(transfer) => {
                let source = FrameSource::spawn(*transfer);
                self.status = format!(
                    "Sending {}. Stop once the receiver shows the file.",
                    source.info.name
                );
                self.stage = Stage::Streaming(Box::new(Live::new(source, self.pending_count)));
            }
            PackingState::Failed(error) => {
                self.error = Some(error);
                self.status.clear();
                self.stage = Stage::Idle;
            }
            PackingState::Lost => {
                self.error = Some("Preparing the transfer stopped unexpectedly.".to_owned());
                self.stage = Stage::Idle;
            }
        }
    }

    /// Settings edited mid-stream: bytes per code and ECC are baked into every
    /// frame and need a fresh stream; rate and grid only change the display.
    fn apply_live_settings(&mut self) {
        let Stage::Streaming(live) = &mut self.stage else {
            return;
        };
        let current = live.source.info.settings;
        if current.frame_bytes != self.settings.frame_bytes || current.ecc != self.settings.ecc {
            self.start();
        } else if current.frames_per_second != self.settings.frames_per_second
            || current.grid_codes != self.settings.grid_codes
        {
            live.reconfigure(self.settings.frames_per_second, self.settings.grid_codes);
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let (dropped, escape, f_key, os_fullscreen) = ctx.input(|input| {
            (
                input
                    .raw
                    .dropped_files
                    .iter()
                    .map(|file| file.path().to_path_buf())
                    .collect::<Vec<_>>(),
                input.key_pressed(egui::Key::Escape),
                input.key_pressed(egui::Key::F),
                input.viewport().fullscreen,
            )
        });
        if !dropped.is_empty() && !self.receiving() {
            self.add_paths(dropped);
        }
        if self.fullscreen {
            match os_fullscreen {
                Some(true) => self.os_fullscreen = true,
                Some(false) if self.os_fullscreen => self.set_fullscreen(ctx, false),
                _ => {}
            }
        }
        if escape {
            self.set_fullscreen(ctx, false);
        }
        let streaming = matches!(self.stage, Stage::Streaming(_));
        if f_key && streaming && !ctx.egui_wants_keyboard_input() {
            self.set_fullscreen(ctx, !self.fullscreen);
        }
    }
}

fn copy_file(file: &SelectedFile) -> SelectedFile {
    SelectedFile {
        name: file.name.clone(),
        media_type: file.media_type.clone(),
        bytes: file.bytes.clone(),
        relative_path: file.relative_path.clone(),
    }
}

impl eframe::App for SenderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.themed {
            theme::apply(&ctx);
            self.themed = true;
        }
        self.handle_input(&ctx);
        self.poll_packing(&ctx);

        if let Stage::Streaming(live) = &mut self.stage {
            if let Err(error) = live.tick(Instant::now()) {
                self.error = Some(error);
                self.stop(&ctx);
            } else {
                ctx.request_repaint();
            }
        }

        if self.fullscreen && matches!(self.stage, Stage::Streaming(_)) {
            self.draw_fullscreen(ui);
            return;
        }
        if self.fullscreen {
            self.set_fullscreen(&ctx, false);
        }

        self.draw_top_bar(ui);
        #[cfg(feature = "receive")]
        if self.mode == Mode::Receive {
            self.receive
                .get_or_insert_with(crate::receive::view::ReceiveView::new)
                .ui(ui);
            return;
        }
        egui::Panel::left("controls")
            .resizable(false)
            .exact_size(372.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin {
                left: 20,
                right: 8,
                top: 16,
                bottom: 16,
            }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.draw_controls(ui));
            });
        egui::Panel::bottom("stream")
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin {
                left: 12,
                right: 20,
                top: 8,
                bottom: 20,
            }))
            .show(ui, |ui| self.draw_stream_stats(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin {
                left: 12,
                right: 20,
                top: 16,
                bottom: 8,
            }))
            .show(ui, |ui| self.draw_stage(ui));

        self.apply_live_settings();
        draw_drop_overlay(&ctx);
    }
}

// ------------------------------------------------------------------ drawing

impl SenderApp {
    fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top")
            .resizable(false)
            .show_separator_line(true)
            .frame(
                egui::Frame::new()
                    .fill(theme::BG)
                    .inner_margin(Margin::symmetric(20, 10)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::qr_motif(ui, 20.0, theme::ACCENT);
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Optical Transfer")
                            .size(theme::BODY + 1.0)
                            .strong(),
                    );
                    self.draw_mode_switch(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        theme::pill(ui, &format!("⚠ {SECURITY_WARNING}"), theme::WARN)
                            .on_hover_text(SECURITY_DETAIL);
                    });
                });
            });
    }

    /// Send or Receive, locked while either direction has work running.
    #[cfg(feature = "receive")]
    fn draw_mode_switch(&mut self, ui: &mut egui::Ui) {
        let locked = self.busy()
            || self
                .receive
                .as_ref()
                .is_some_and(crate::receive::view::ReceiveView::busy);
        ui.add_space(8.0);
        ui.add_enabled_ui(!locked, |ui| {
            theme::segmented(
                ui,
                &mut self.mode,
                &[(Mode::Send, "Send"), (Mode::Receive, "Receive")],
            );
        })
        .response
        .on_disabled_hover_text("Stop first to switch direction");
    }

    #[cfg(not(feature = "receive"))]
    fn draw_mode_switch(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Sender")
                .size(theme::BODY + 1.0)
                .color(theme::TEXT_FAINT),
        );
    }

    fn draw_controls(&mut self, ui: &mut egui::Ui) {
        let locked = self.busy();
        let mut tab = self.tab;
        theme::card(
            ui,
            "Send",
            |ui| {
                ui.add_enabled_ui(!locked, |ui| {
                    theme::segmented(ui, &mut tab, &[(Tab::Text, "Text"), (Tab::Files, "Files")]);
                });
            },
            |ui| {
                ui.add_enabled_ui(!locked, |ui| match self.tab {
                    Tab::Files => self.draw_files(ui),
                    Tab::Text => self.draw_text(ui),
                });
            },
        );
        if tab != self.tab {
            self.tab = tab;
            self.error = None;
        }

        ui.add_space(12.0);
        let wire = f64::from(self.settings.frames_per_second)
            * self.settings.grid_codes as f64
            * (self.settings.frame_bytes - HEADER_LEN) as f64
            / 1024.0;
        theme::card(
            ui,
            "Speed",
            |ui| {
                ui.label(
                    RichText::new(format!("{} on the wire", format_rate(wire)))
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
            },
            |ui| self.draw_speed(ui, matches!(self.stage, Stage::Preparing(_))),
        );

        ui.add_space(12.0);
        match self.stage {
            Stage::Idle => {
                if theme::primary_button(ui, "Start sending").clicked() {
                    self.start();
                }
            }
            Stage::Preparing(_) | Stage::Streaming(_) => {
                let streaming = matches!(self.stage, Stage::Streaming(_));
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 8.0) / 2.0;
                    if ui
                        .add_sized(
                            [width, 40.0],
                            egui::Button::new(RichText::new("Stop").size(theme::BODY).strong()),
                        )
                        .clicked()
                    {
                        self.stop(ui.ctx());
                    }
                    let full = ui.add_enabled(
                        streaming,
                        egui::Button::new(RichText::new("Full screen").size(theme::BODY).strong())
                            .min_size(vec2(width, 40.0)),
                    );
                    if full
                        .on_hover_text("F to enter, Esc or a click to leave")
                        .clicked()
                    {
                        self.set_fullscreen(ui.ctx(), true);
                    }
                });
            }
        }
        if !self.status.is_empty() {
            ui.add_space(4.0);
            ui.label(
                RichText::new(&self.status)
                    .size(theme::SMALL)
                    .color(theme::TEXT_DIM),
            );
        }
        if let Some(error) = &self.error {
            ui.add_space(6.0);
            notice(ui, error, theme::DANGER);
        }
    }

    fn draw_files(&mut self, ui: &mut egui::Ui) {
        let hovering = ui.ctx().input(|input| !input.raw.hovered_files.is_empty());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 108.0), egui::Sense::click());
        let painter = ui.painter();
        let accent = hovering || (response.hovered() && ui.is_enabled());
        painter.rect(
            rect,
            CornerRadius::same(10),
            if accent {
                theme::ACCENT.linear_multiply(0.06)
            } else {
                theme::BG
            },
            Stroke::new(
                1.0,
                if accent {
                    theme::ACCENT
                } else {
                    theme::LINE_STRONG
                },
            ),
            StrokeKind::Inside,
        );
        let center = rect.center();
        painter.text(
            center - vec2(0.0, 12.0),
            egui::Align2::CENTER_CENTER,
            "Choose files or drop them here",
            egui::FontId::proportional(theme::BODY),
            if ui.is_enabled() {
                theme::TEXT
            } else {
                theme::TEXT_FAINT
            },
        );
        painter.text(
            center + vec2(0.0, 12.0),
            egui::Align2::CENTER_CENTER,
            "Folders too · several travel as one zip · 64 MiB",
            egui::FontId::proportional(theme::SMALL - 1.0),
            theme::TEXT_FAINT,
        );
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
            && let Some(paths) = rfd::FileDialog::new().pick_files()
        {
            self.add_paths(paths);
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Add a folder…").clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                self.add_paths([path]);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!self.files.is_empty(), egui::Button::new("Clear"))
                    .clicked()
                {
                    self.files.clear();
                }
            });
        });
        let zipped =
            self.files.len() > 1 || self.files.iter().any(|file| file.relative_path.is_some());
        let summary = match self.files.len() {
            0 => "Nothing selected".to_owned(),
            count => format!(
                "{count} file{} · {}{}",
                if count == 1 { "" } else { "s" },
                human_bytes(self.total_bytes()),
                if zipped { " → transfer.zip" } else { "" }
            ),
        };
        ui.label(
            RichText::new(summary)
                .size(theme::SMALL)
                .monospace()
                .color(theme::TEXT_DIM),
        );

        if !self.files.is_empty() {
            let mut remove = None;
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .id_salt("files")
                .show(ui, |ui| {
                    for (index, file) in self.files.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let name = file.relative_path.as_deref().unwrap_or(&file.name);
                            ui.add(
                                egui::Label::new(RichText::new(name).size(theme::SMALL)).truncate(),
                            )
                            .on_hover_text(name);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("×").on_hover_text("Remove").clicked() {
                                        remove = Some(index);
                                    }
                                    ui.label(
                                        RichText::new(human_bytes(file.bytes.len()))
                                            .size(theme::SMALL)
                                            .monospace()
                                            .color(theme::TEXT_FAINT),
                                    );
                                },
                            );
                        });
                    }
                });
            if let Some(index) = remove {
                self.files.remove(index);
            }
        }
    }

    fn draw_text(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::multiline(&mut self.text)
                .desired_rows(8)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace)
                .hint_text("Type or paste anything…"),
        );
        let bytes = self.text.len();
        ui.label(
            RichText::new(format!("{} of 4 MiB", human_bytes(bytes)))
                .size(theme::SMALL)
                .monospace()
                .color(if bytes > MAX_SNIPPET_BYTES {
                    theme::DANGER
                } else {
                    theme::TEXT_FAINT
                }),
        );
    }

    fn draw_speed(&mut self, ui: &mut egui::Ui, locked: bool) {
        ui.add_enabled_ui(!locked, |ui| {
            let width = (ui.available_width() - 8.0) / 2.0;
            for pair in PRESETS.chunks(2) {
                ui.horizontal(|ui| {
                    for preset in pair {
                        let detail = format!(
                            "{} code{} · {} fps · {} B",
                            preset.codes,
                            if preset.codes == 1 { "" } else { "s" },
                            preset.fps,
                            preset.frame_bytes
                        );
                        if theme::tile(
                            ui,
                            preset.matches(&self.settings),
                            preset.name,
                            &detail,
                            width,
                        )
                        .clicked()
                        {
                            preset.apply(&mut self.settings);
                        }
                    }
                });
            }
            ui.add_space(2.0);
            egui::CollapsingHeader::new(
                RichText::new("Fine-tune")
                    .size(theme::SMALL + 0.5)
                    .color(theme::TEXT_DIM),
            )
            .id_salt("fine-tune")
            .show(ui, |ui| {
                egui::Grid::new("settings")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        let settings = &mut self.settings;
                        combo(
                            ui,
                            "Bytes per code",
                            "bytes",
                            &mut settings.frame_bytes,
                            &FRAME_BYTES_OPTIONS,
                            |v| format!("{v} B"),
                        );
                        combo(
                            ui,
                            "Frames per second",
                            "fps",
                            &mut settings.frames_per_second,
                            &TX_FPS_OPTIONS,
                            |v| v.to_string(),
                        );
                        combo(
                            ui,
                            "Codes on screen",
                            "codes",
                            &mut settings.grid_codes,
                            &GRID_OPTIONS,
                            |v| v.to_string(),
                        );
                        combo(
                            ui,
                            "Error correction",
                            "ecc",
                            &mut settings.ecc,
                            &Ecc::ALL,
                            |v| v.label().to_owned(),
                        );
                    });
                ui.label(
                    RichText::new(
                        "Bytes and error correction restart the stream. \
                             Frame rate and code count change it live.",
                    )
                    .size(theme::SMALL - 0.5)
                    .color(theme::TEXT_FAINT),
                );
            });
        });
    }

    fn draw_stage(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let painter = ui.painter();
        match &mut self.stage {
            Stage::Streaming(live) if live.cell > 0 => {
                painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), Color32::WHITE);
                paint_codes(ui, live, rect.shrink(20.0));
            }
            stage => {
                painter.rect(
                    rect,
                    CornerRadius::same(theme::RADIUS),
                    theme::SURFACE,
                    Stroke::new(1.0, theme::LINE),
                    StrokeKind::Inside,
                );
                let motif =
                    Rect::from_center_size(rect.center() - vec2(0.0, 22.0), Vec2::splat(44.0));
                let text = if matches!(stage, Stage::Idle) {
                    theme::paint_motif(painter, motif, theme::LINE_STRONG);
                    "The code stream appears here."
                } else {
                    egui::Spinner::new()
                        .size(28.0)
                        .paint_at(ui, motif.shrink(8.0));
                    "Compressing and hashing…"
                };
                painter.text(
                    rect.center() + vec2(0.0, 22.0),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(theme::BODY),
                    theme::TEXT_DIM,
                );
            }
        }
    }

    fn draw_stream_stats(&mut self, ui: &mut egui::Ui) {
        let live = match &self.stage {
            Stage::Streaming(live) => Some(live.as_ref()),
            _ => None,
        };
        let session = live.map(|live| format!("session {:04x}", live.source.info.session_id));
        theme::card(
            ui,
            "Stream",
            |ui| {
                if let Some(session) = &session {
                    ui.label(RichText::new(session).monospace().color(theme::TEXT_DIM));
                }
            },
            |ui| {
                let stats = live.map(StreamStats::of);
                let fraction = stats.as_ref().map_or(0.0, |stats| stats.cycle_fraction);
                theme::track(ui, fraction, theme::ACCENT);
                ui.add_space(2.0);
                ui.label(
                    RichText::new(
                        stats
                            .as_ref()
                            .map_or("—".to_owned(), |stats| stats.cycle.clone()),
                    )
                    .size(theme::SMALL)
                    .color(theme::TEXT_DIM),
                );
                let rows = stats.map(|stats| stats.rows).unwrap_or_default();
                let labels = [
                    "Payload",
                    "Compression",
                    "Code",
                    "Blocks (k)",
                    "Raw rate",
                    "Best case",
                    "Frames shown",
                    "Flip rate",
                    "Display",
                ];
                let columns = if ui.available_width() > 760.0 { 3 } else { 2 };
                let width = (ui.available_width() - 20.0 * (columns - 1) as f32) / columns as f32;
                egui::Grid::new("stream-stats")
                    .num_columns(columns)
                    .spacing([20.0, 0.0])
                    .show(ui, |ui| {
                        for (index, label) in labels.iter().enumerate() {
                            let (value, color) = rows
                                .get(index)
                                .cloned()
                                .unwrap_or_else(|| ("—".to_owned(), theme::TEXT_FAINT));
                            theme::stat(ui, label, &value, color, width);
                            if index % columns == columns - 1 {
                                ui.end_row();
                            }
                        }
                    });
                if let Some(live) = live
                    && live.clock.worst_stall > Duration::from_secs(1)
                {
                    ui.add_space(8.0);
                    notice(
                        ui,
                        &format!(
                            "This window stalled for {:.1} s. Keep it visible and in front: \
                             a hidden or throttled window freezes the stream mid-flip.",
                            live.clock.worst_stall.as_secs_f32()
                        ),
                        theme::WARN,
                    );
                }
            },
        );
    }

    /// Codes edge to edge on white with nothing else on screen. Any click,
    /// Escape or F returns.
    fn draw_fullscreen(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let clicked = ctx.input(|input| input.pointer.any_click());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::WHITE))
            .show(ui, |ui| {
                if let Stage::Streaming(live) = &mut self.stage
                    && live.cell > 0
                {
                    let rect = ui.max_rect().shrink(8.0);
                    paint_codes(ui, live, rect);
                }
            });
        if clicked {
            self.set_fullscreen(&ctx, false);
        }
    }
}

/// The figures under the stage, worked out once per frame.
struct StreamStats {
    cycle: String,
    cycle_fraction: f32,
    rows: Vec<(String, Color32)>,
}

impl StreamStats {
    fn of(live: &Live) -> Self {
        let info = &live.source.info;
        let settings = info.settings;
        let codes = live.cells.len();
        let k = u64::from(info.source_blocks.max(1));
        let cycle = u64::from(info.cycle_frames.max(1));
        let position = live.clock.shown % cycle;
        let carousel = live.clock.shown / cycle + 1;
        let cycle_text = if position < k {
            format!(
                "Systematic sweep {} / {k} · carousel {carousel}",
                position + 1
            )
        } else {
            format!(
                "Repair frames {} / {} · carousel {carousel}",
                position - k + 1,
                cycle - k
            )
        };
        let per_second = f64::from(settings.frames_per_second) * codes as f64;
        let best = k as f64 / per_second;
        let target = per_second.round() as usize;
        let flips = live.clock.flip_rate();
        let slow = flips < target * 9 / 10 && live.clock.shown > target as u64;
        let plain = theme::TEXT;
        let rows = vec![
            (
                format!(
                    "{}{}",
                    human_bytes(info.original_size),
                    if live.file_count > 1 {
                        format!(" · {} files", live.file_count)
                    } else {
                        String::new()
                    }
                ),
                plain,
            ),
            (
                match info.compression {
                    Compression::Gzip => format!("gzip → {}", human_bytes(info.transmitted_size)),
                    Compression::None => "none (incompressible)".to_owned(),
                },
                plain,
            ),
            (
                format!("V{} ×{codes} · ECC {}", live.version, settings.ecc.label()),
                plain,
            ),
            (format!("{k} × {} B", info.block_len), plain),
            (
                format!(
                    "{} · {target} codes/s",
                    format_rate(per_second * info.block_len as f64 / 1024.0)
                ),
                plain,
            ),
            (
                if best < 0.1 {
                    "one frame".to_owned()
                } else {
                    format_duration(best)
                },
                plain,
            ),
            (live.clock.shown.to_string(), plain),
            (
                format!("{flips} / {target} per s"),
                if slow { theme::WARN } else { plain },
            ),
            (
                live.clock
                    .refresh_hz()
                    .map_or("—".to_owned(), |hz| format!("{hz:.0} Hz")),
                plain,
            ),
        ];
        Self {
            cycle: cycle_text,
            cycle_fraction: position as f32 / cycle as f32,
            rows,
        }
    }
}

/// Paint the grid as large as `area` allows, at a whole number of device
/// pixels per module, with its corner on the device pixel grid.
fn paint_codes(ui: &mut egui::Ui, live: &mut Live, area: Rect) {
    let ctx = ui.ctx().clone();
    let codes = live.cells.len();
    let grid = best_grid(codes, live.cell, area.width(), area.height());
    let ppp = ctx.pixels_per_point();
    let modules = [grid.0 * live.cell, grid.1 * live.cell];
    // Device pixels per module, fractional; the whole part goes in the texture.
    let fit = (area.width() * ppp / modules[0] as f32)
        .min(area.height() * ppp / modules[1] as f32)
        .max(1.0);
    let upscale = fit.floor() as usize;
    live.sync_texture(&ctx, grid, upscale);
    let Some(texture) = &live.texture else { return };
    let size = vec2(modules[0] as f32, modules[1] as f32) * fit / ppp;
    let corner = area.center() - size / 2.0;
    let corner = pos2(
        (corner.x * ppp).round() / ppp,
        (corner.y * ppp).round() / ppp,
    );
    ui.painter().image(
        texture.id(),
        Rect::from_min_size(corner, size),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

fn draw_drop_overlay(ctx: &egui::Context) {
    if ctx.input(|input| input.raw.hovered_files.is_empty()) {
        return;
    }
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("drop"),
    ));
    painter.rect_filled(screen, CornerRadius::ZERO, theme::BG.gamma_multiply(0.8));
    painter.rect_stroke(
        screen.shrink(16.0),
        CornerRadius::same(16),
        Stroke::new(2.0, theme::ACCENT),
        StrokeKind::Inside,
    );
    painter.text(
        screen.center(),
        egui::Align2::CENTER_CENTER,
        "Drop to add files or folders",
        egui::FontId::proportional(20.0),
        theme::TEXT,
    );
}

pub(crate) fn notice(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.1))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.4)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(color).size(theme::SMALL));
        });
}

fn combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    label: &str,
    id: &str,
    value: &mut T,
    options: &[T],
    show: impl Fn(T) -> String,
) {
    ui.label(
        RichText::new(label)
            .size(theme::SMALL)
            .color(theme::TEXT_DIM),
    );
    egui::ComboBox::from_id_salt(id)
        .selected_text(show(*value))
        .show_ui(ui, |ui| {
            for option in options {
                ui.selectable_value(value, *option, show(*option));
            }
        });
    ui.end_row();
}

pub fn format_rate(kib_per_second: f64) -> String {
    if !kib_per_second.is_finite() || kib_per_second <= 0.0 {
        "0 KiB/s".to_owned()
    } else if kib_per_second >= 1024.0 {
        format!("{:.2} MiB/s", kib_per_second / 1024.0)
    } else if kib_per_second < 100.0 {
        format!("{kib_per_second:.1} KiB/s")
    } else {
        format!("{kib_per_second:.0} KiB/s")
    }
}

pub fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() {
        return "—".to_owned();
    }
    if seconds < 10.0 {
        return format!("{:.1} s", seconds.max(0.0));
    }
    let whole = seconds.round() as u64;
    if whole < 60 {
        format!("{whole} s")
    } else {
        format!("{} min {:02} s", whole / 60, whole % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_turns_each_module_into_a_whole_pixel_block() {
        let mut code = ColorImage::filled([2, 2], Color32::WHITE);
        code.pixels[1] = Color32::BLACK; // top right
        let mut target = ColorImage::filled([8, 7], Color32::RED);
        blit(&mut target, &code, [2, 1], 3);
        let at = |x: usize, y: usize| target.pixels[y * 8 + x];
        for y in 1..7 {
            for x in 2..8 {
                let dark = x >= 5 && y < 4;
                assert_eq!(
                    at(x, y),
                    if dark { Color32::BLACK } else { Color32::WHITE },
                    "({x}, {y})"
                );
            }
        }
        assert_eq!(
            at(0, 0),
            Color32::RED,
            "nothing outside the block is touched"
        );
        assert_eq!(at(1, 3), Color32::RED);
    }
}
