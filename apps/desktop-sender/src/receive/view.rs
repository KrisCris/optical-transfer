//! The Receive mode window.
//!
//! Laid out like the sender — controls on the left, the picture in the
//! middle, live numbers underneath — and showing what the web receiver shows:
//! the camera preview with a bracket on every code being read, progress, the
//! recovered-data rate, the source-block map and the full telemetry panel,
//! under the same labels so a number means the same thing in either app.
//!
//! Nothing here runs per decoded frame. The session updates its shared state
//! from its own threads; this reads it once per repaint, and the panel and
//! block map only refresh at the web receiver's 150 ms and 120 ms.

use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, Margin, Rect, RichText, Shape, Stroke, StrokeKind,
    TextureOptions, Vec2, pos2, vec2,
};
use optical_core::{container::safe_file_name, receiver::ReceiverStats, transfer::human_bytes};

use crate::{
    app::{format_duration, format_rate, notice},
    receive::{
        camera::{self, CameraInfo, FrameSink, LumaFrame, Negotiated},
        decoder,
        session::{Received, Session},
        telemetry::{HISTORY_BUCKETS, Snapshot},
        tracker::{Bounds, Mode},
    },
    theme,
};

const NO_SIGNAL_MS: f64 = 8000.0;
const PANEL_MS: f64 = 150.0;
const MAP_MS: f64 = 120.0;
/// Blocks solved this recently glow, so peeling cascades and elimination
/// show up as bursts across the map.
const GLOW_MS: f64 = 700.0;
const BRACKET_FADE_MS: f64 = 700.0;
/// One colour per code, assigned in reading order so a code keeps its colour
/// through dropouts: "the amber one is not decoding" beats counting boxes.
const CODE_COLORS: [Color32; 9] = [
    Color32::from_rgb(0x42, 0xe8, 0xff),
    Color32::from_rgb(0x54, 0xff, 0x7e),
    Color32::from_rgb(0xff, 0xd9, 0x4a),
    Color32::from_rgb(0xff, 0x6a, 0xd5),
    Color32::from_rgb(0xc0, 0x8b, 0xff),
    Color32::from_rgb(0xff, 0x9a, 0x4d),
    Color32::from_rgb(0x8d, 0xff, 0x4a),
    Color32::from_rgb(0xff, 0x5f, 0x5f),
    Color32::WHITE,
];

/// Whatever keeps frames flowing; dropping it stops them. A [`Camera`] in the
/// app, a synthetic source in the demo example.
pub type FrameGuard = Box<dyn Send>;

/// Something that starts delivering frames into a sink.
pub type FrameSource =
    Arc<dyn Fn(Option<&str>, FrameSink) -> Result<(FrameGuard, Negotiated), String> + Send + Sync>;

type Opened = Result<(FrameGuard, Negotiated), String>;

enum Stage {
    Idle,
    /// The camera opens on a thread of its own: the permission prompt and a
    /// phone waking up both take seconds.
    Opening {
        session: Session,
        opened: mpsc::Receiver<Opened>,
    },
    Running(Box<Running>),
    Done(Box<Finished>),
}

struct Running {
    session: Session,
    /// Held for its `Drop`, which stops the capture session.
    _camera: FrameGuard,
    negotiated: Negotiated,
    started: f64,
}

struct Finished {
    /// Kept for the final telemetry; its camera is already stopped.
    session: Session,
    negotiated: Negotiated,
    received: Received,
    text: Option<String>,
    saved: Option<Result<PathBuf, String>>,
    /// Frames drawn since completion. The save dialog is modal and blocks
    /// repaints, so it waits until the verified state has been on screen.
    shown: u8,
}

/// What one repaint reads from the session, copied out under the lock.
struct Reading {
    now: f64,
    regions: Vec<(Bounds, f64)>,
    mode: Mode,
    codes: usize,
    expected: usize,
    preview: Option<(Arc<LumaFrame>, u64)>,
    verdict: Option<String>,
    failure: Option<String>,
    last_accepted: Option<f64>,
    /// Colour codes read in the last two seconds.
    colour: bool,
}

pub struct ReceiveView {
    source: FrameSource,
    /// Cameras come from the system and can be listed again.
    system: bool,
    cameras: Vec<CameraInfo>,
    /// `None` is automatic: a Continuity Camera when there is one.
    chosen: Option<String>,
    workers: usize,
    cores: usize,
    stage: Stage,
    status: String,
    error: Option<String>,
    preview: Option<egui::TextureHandle>,
    preview_serial: u64,
    preview_size: [usize; 2],
    map: BlockMap,
    snapshot: Snapshot,
    series: Vec<f64>,
    last_panel: f64,
}

impl Default for ReceiveView {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiveView {
    pub fn new() -> Self {
        let mut view = Self::with_source(
            camera::cameras(),
            Arc::new(|id, sink| {
                camera::open(id, sink)
                    .map(|(camera, negotiated)| (Box::new(camera) as FrameGuard, negotiated))
            }),
        );
        view.system = true;
        view
    }

    /// A view fed by `source` instead of the system cameras, listing
    /// `cameras` as the choices.
    pub fn with_source(cameras: Vec<CameraInfo>, source: FrameSource) -> Self {
        let cores = thread::available_parallelism().map_or(4, usize::from);
        Self {
            source,
            system: false,
            cameras,
            chosen: None,
            // Every core up to eight, as the web receiver does: crops are
            // cheap, and a pool behind the camera drops frames the fountain
            // then has to wait a carousel for.
            workers: cores.clamp(1, 8),
            cores,
            stage: Stage::Idle,
            status: "Point a camera at the sending screen, then start.".to_owned(),
            error: None,
            preview: None,
            preview_serial: 0,
            preview_size: [0, 0],
            map: BlockMap::default(),
            snapshot: Snapshot::default(),
            series: Vec::new(),
            last_panel: f64::NEG_INFINITY,
        }
    }

    /// A camera is open or opening; switching modes would leave it running.
    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Opening { .. } | Stage::Running(_))
    }

    fn session(&self) -> Option<&Session> {
        match &self.stage {
            Stage::Idle => None,
            Stage::Opening { session, .. } => Some(session),
            Stage::Running(running) => Some(&running.session),
            Stage::Done(done) => Some(&done.session),
        }
    }

    fn start(&mut self) {
        self.error = None;
        // A phone often joins after the window opened. Automatic should find
        // it without a trip to Refresh.
        if self.system && self.chosen.is_none() {
            self.cameras = camera::cameras();
        }
        let chosen = self
            .chosen
            .clone()
            .filter(|id| self.cameras.iter().any(|camera| &camera.id == id))
            .or_else(|| camera::preferred(&self.cameras).map(|camera| camera.id.clone()));
        let session = Session::start(self.workers);
        let sink = session.sink();
        let (sender, opened) = mpsc::channel();
        let source = Arc::clone(&self.source);
        thread::spawn(move || {
            let _ = sender.send(source(chosen.as_deref(), sink));
        });
        self.preview_serial = 0;
        self.map = BlockMap::default();
        self.last_panel = f64::NEG_INFINITY;
        self.status = "Opening the camera…".to_owned();
        self.stage = Stage::Opening { session, opened };
    }

    pub fn stop(&mut self) {
        if self.busy() {
            self.status = "Stopped.".to_owned();
        }
        self.stage = Stage::Idle;
    }

    /// Move between stages on what the camera thread and the session report.
    fn advance(&mut self, ctx: &egui::Context) {
        match std::mem::replace(&mut self.stage, Stage::Idle) {
            Stage::Opening { session, opened } => match opened.try_recv() {
                Ok(Ok((camera, negotiated))) => {
                    self.status = format!("Looking for codes with {}…", negotiated.name);
                    let started = session.now();
                    self.stage = Stage::Running(Box::new(Running {
                        session,
                        _camera: camera,
                        negotiated,
                        started,
                    }));
                }
                Ok(Err(error)) => {
                    self.status.clear();
                    self.error = Some(error);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                    self.stage = Stage::Opening { session, opened };
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.error = Some("Opening the camera stopped unexpectedly.".to_owned());
                }
            },
            Stage::Running(running) => {
                let received = running.session.lock().result.take();
                match received {
                    Some(received) => {
                        let Running {
                            session,
                            negotiated,
                            ..
                        } = *running;
                        // The camera handle drops here, stopping capture.
                        let text = received
                            .is_text
                            .then(|| String::from_utf8_lossy(&received.bytes).into_owned());
                        self.status = "Received and verified.".to_owned();
                        self.last_panel = f64::NEG_INFINITY;
                        self.stage = Stage::Done(Box::new(Finished {
                            session,
                            negotiated,
                            received,
                            text,
                            saved: None,
                            shown: 0,
                        }));
                    }
                    None => {
                        ctx.request_repaint();
                        self.stage = Stage::Running(running);
                    }
                }
            }
            other => self.stage = other,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.advance(&ctx);
        if let Stage::Done(done) = &mut self.stage
            && done.shown < 2
        {
            done.shown += 1;
            ctx.request_repaint();
            if done.shown == 2 && done.text.is_none() {
                done.saved = save(&done.received);
            }
        }
        let reading = self.read();
        if let Some(reading) = &reading {
            self.refresh(&ctx, reading);
        }

        egui::Panel::left("rx-controls")
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
                    .show(ui, |ui| self.draw_controls(ui, reading.as_ref()));
            });
        egui::Panel::bottom("rx-progress")
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin {
                left: 12,
                right: 20,
                top: 8,
                bottom: 20,
            }))
            .show(ui, |ui| self.draw_progress(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin {
                left: 12,
                right: 20,
                top: 16,
                bottom: 8,
            }))
            .show(ui, |ui| self.draw_viewfinder(ui, reading.as_ref()));
    }

    fn read(&mut self) -> Option<Reading> {
        let session = self.session()?;
        let now = session.now();
        let core = session.lock();
        let tracker = &core.tracker;
        Some(Reading {
            now,
            regions: tracker
                .regions()
                .iter()
                .filter(|region| region.decoded)
                .map(|region| (region.bounds, region.seen))
                .collect(),
            mode: tracker.mode(),
            codes: tracker.decoded_count(),
            expected: tracker.expected(),
            preview: core
                .preview
                .as_ref()
                .map(|frame| (Arc::clone(frame), core.preview_serial)),
            verdict: core.verdict.clone(),
            failure: core.failure.clone(),
            last_accepted: core.last_accepted,
            colour: core.last_colour.is_some_and(|at| now - at < 2000.0),
        })
    }

    /// Upload a new preview frame, and at panel rate recompute the numbers,
    /// the chart and the block map.
    fn refresh(&mut self, ctx: &egui::Context, reading: &Reading) {
        if let Some((frame, serial)) = &reading.preview
            && *serial != self.preview_serial
            && !matches!(self.stage, Stage::Done(_))
        {
            self.preview_serial = *serial;
            self.preview_size = [frame.width, frame.height];
            let image = ColorImage::from_gray([frame.width, frame.height], &frame.pixels);
            match &mut self.preview {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => {
                    self.preview =
                        Some(ctx.load_texture("rx-preview", image, TextureOptions::LINEAR))
                }
            }
        }
        let now = reading.now;
        let done = matches!(self.stage, Stage::Done(_));
        let panel_due = now - self.last_panel >= PANEL_MS;
        let map_due = now - self.map.drawn_at >= MAP_MS;
        if !panel_due && !map_due {
            return;
        }
        let Some(session) = self.session() else {
            return;
        };
        let mut core = session.lock();
        let (snapshot, series) = if panel_due {
            (
                Some(core.telemetry.snapshot(now)),
                Some(core.telemetry.series(now)),
            )
        } else {
            (None, None)
        };
        let solved = map_due.then(|| {
            if done {
                core.stats
                    .as_ref()
                    .map(|stats| vec![1; stats.total_blocks])
                    .unwrap_or_default()
            } else {
                core.receiver.solved_map()
            }
        });
        drop(core);
        if let Some(snapshot) = snapshot {
            self.snapshot = snapshot;
            self.series = series.unwrap_or_default();
            self.last_panel = now;
        }
        if let Some(solved) = solved {
            self.map.update(ctx, &solved, now, done);
        }
    }

    // ------------------------------------------------------------------ left

    fn draw_controls(&mut self, ui: &mut egui::Ui, reading: Option<&Reading>) {
        let locked = self.busy();
        let mut refresh = false;
        theme::card(
            ui,
            "Camera",
            |ui| {
                refresh = ui
                    .add_enabled(!locked, egui::Button::new("Refresh").small())
                    .on_hover_text("List cameras again: a phone appears once it is nearby")
                    .clicked();
            },
            |ui| {
                ui.add_enabled_ui(!locked, |ui| self.draw_camera_choice(ui));
            },
        );
        if refresh && self.system {
            self.cameras = camera::cameras();
        }

        ui.add_space(12.0);
        match self.stage {
            Stage::Idle | Stage::Done(_) => {
                let label = if matches!(self.stage, Stage::Done(_)) {
                    "Receive another"
                } else {
                    "Start receiving"
                };
                if theme::primary_button(ui, label).clicked() {
                    self.start();
                }
            }
            Stage::Opening { .. } | Stage::Running(_) => {
                if theme::wide_button(ui, "Stop").clicked() {
                    self.stop();
                }
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
        let verdict =
            reading.and_then(|reading| reading.verdict.as_deref().or(reading.failure.as_deref()));
        if let Some(error) = self.error.as_deref().or(verdict) {
            ui.add_space(6.0);
            notice(ui, error, theme::DANGER);
        }
        if let (Stage::Running(running), Some(reading)) = (&self.stage, reading)
            && reading.last_accepted.is_none()
            && reading.now - running.started > NO_SIGNAL_MS
        {
            ui.add_space(6.0);
            notice(
                ui,
                "No codes yet. Fill most of the camera's view with the sending screen and hold \
                 steady; move closer if the codes look small. If they never lock, pick Reliable \
                 on the sender.",
                theme::WARN,
            );
        }

        if matches!(self.stage, Stage::Done(_)) {
            ui.add_space(12.0);
            self.draw_result(ui);
        }

        ui.add_space(12.0);
        self.draw_telemetry(ui, reading);
    }

    fn draw_camera_choice(&mut self, ui: &mut egui::Ui) {
        if self.cameras.is_empty() {
            ui.label(
                RichText::new(if cfg!(target_os = "macos") {
                    "No camera found. An iPhone signed in to the same Apple Account shows up \
                     here as a Continuity Camera once it is nearby, locked and on a stand."
                } else {
                    "Camera capture is only available in the macOS build so far."
                })
                .size(theme::SMALL)
                .color(theme::TEXT_DIM),
            );
            return;
        }
        let automatic = camera::preferred(&self.cameras).map_or("Automatic".to_owned(), |camera| {
            format!("Automatic ({})", camera.name)
        });
        let selected = self
            .chosen
            .as_ref()
            .and_then(|id| self.cameras.iter().find(|camera| &camera.id == id))
            .map_or(automatic.clone(), |camera| camera.name.clone());
        let width = ui.available_width();
        ui.label(
            RichText::new("Camera")
                .size(theme::SMALL)
                .color(theme::TEXT_DIM),
        );
        egui::ComboBox::from_id_salt("rx-camera-choice")
            .selected_text(selected)
            .width(width)
            .truncate()
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.chosen, None, automatic);
                for camera in &self.cameras {
                    let label = if camera.continuity {
                        format!("{} · Continuity", camera.name)
                    } else {
                        camera.name.clone()
                    };
                    ui.selectable_value(&mut self.chosen, Some(camera.id.clone()), label);
                }
            });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Decode threads")
                    .size(theme::SMALL)
                    .color(theme::TEXT_DIM),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("rx-workers")
                    .selected_text(self.workers.to_string())
                    .width(64.0)
                    .show_ui(ui, |ui| {
                        for count in 1..=self.cores.clamp(1, 8) {
                            ui.selectable_value(&mut self.workers, count, count.to_string());
                        }
                    });
            });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Continuity Cameras are picked automatically: on a Mac the built-in camera \
                 faces you, not the other screen.",
            )
            .size(theme::SMALL - 0.5)
            .color(theme::TEXT_FAINT),
        );
    }

    fn draw_result(&mut self, ui: &mut egui::Ui) {
        let snapshot = &self.snapshot;
        let Stage::Done(done) = &mut self.stage else {
            return;
        };
        let received = &done.received;
        let title = if received.is_text {
            "Text snippet".to_owned()
        } else {
            safe_file_name(&received.name)
        };
        theme::card(
            ui,
            "Received",
            |ui| {
                theme::pill(ui, "Verified", theme::ACCENT);
            },
            |ui| {
                ui.add(
                    egui::Label::new(RichText::new(&title).size(theme::BODY).strong()).truncate(),
                );
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {} avg",
                        human_bytes(received.bytes.len()),
                        format_duration(snapshot.elapsed),
                        format_rate(snapshot.average_kibs)
                    ))
                    .size(theme::SMALL)
                    .monospace()
                    .color(theme::TEXT_DIM),
                );
                ui.add_space(8.0);
                if let Some(text) = &done.text {
                    // Text from a screen this machine does not control: shown
                    // as text, selectable, never interpreted.
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .id_salt("rx-text")
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(text).monospace().size(theme::SMALL),
                                )
                                .selectable(true)
                                .wrap(),
                            );
                        });
                    ui.add_space(6.0);
                    if theme::wide_button(ui, "Copy text").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                } else {
                    if theme::wide_button(ui, "Save…").clicked() {
                        done.saved = save(received).or(done.saved.take());
                    }
                    match &done.saved {
                        Some(Ok(path)) => {
                            ui.label(
                                RichText::new(format!("Saved to {}", path.display()))
                                    .size(theme::SMALL)
                                    .color(theme::TEXT_DIM),
                            );
                        }
                        Some(Err(error)) => notice(ui, error, theme::DANGER),
                        None => {}
                    }
                }
            },
        );
    }

    fn draw_telemetry(&self, ui: &mut egui::Ui, reading: Option<&Reading>) {
        let snap = &self.snapshot;
        let stats = snap.stats.as_ref();
        let dash = || "—".to_owned();
        let stat = |f: &dyn Fn(&ReceiverStats) -> String| stats.map_or_else(dash, f);
        let totals = snap.totals;
        let ms = |value: f64| {
            if value > 0.0 {
                format!("{value:.1}")
            } else {
                "–".to_owned()
            }
        };
        let negotiated = match &self.stage {
            Stage::Running(running) => Some(&running.negotiated),
            Stage::Done(done) => Some(&done.negotiated),
            _ => None,
        };
        let running = matches!(self.stage, Stage::Running(_));
        // v4's counters mean RaptorQ's things; the labels follow the stream.
        let v3 = stats.is_none_or(|stats| stats.wire == 3);
        let transfer = [
            ("Payload", stat(&|stats| human_bytes(stats.total_bytes))),
            (
                "Format",
                stat(&|stats| {
                    let code = if stats.wire == 3 { "LT" } else { "RaptorQ" };
                    let colour = if reading.is_some_and(|reading| reading.colour) {
                        " · colour"
                    } else {
                        ""
                    };
                    format!("v{} · {code}{colour}", stats.wire)
                }),
            ),
            (
                if v3 { "Blocks (k)" } else { "Symbols (k)" },
                stat(&|stats| stats.total_blocks.to_string()),
            ),
            (
                if v3 { "Block size" } else { "Symbol size" },
                stat(&|stats| format!("{} B", stats.block_len)),
            ),
            (
                "Unique / dup",
                stat(&|stats| format!("{} / {}", stats.unique_frames, stats.duplicate_frames)),
            ),
            (
                "Redundant",
                stat(&|stats| stats.redundant_frames.to_string()),
            ),
            (
                if v3 { "Parked eqs" } else { "Repair held" },
                stat(&|stats| stats.pending_equations.to_string()),
            ),
            (
                if v3 { "Eliminations" } else { "Decode tries" },
                stat(&|stats| stats.eliminations.to_string()),
            ),
            (
                "Catch rate",
                snap.catch_rate
                    .map_or_else(dash, |rate| format!("{:.0}%", rate.min(1.0) * 100.0)),
            ),
            (
                "Overhead",
                snap.overhead
                    .map_or_else(dash, |overhead| format!("{overhead:.3}×")),
            ),
            (
                "Avg / peak",
                stat(&|_| {
                    format!(
                        "{} / {}",
                        format_rate(snap.average_kibs),
                        format_rate(snap.peak_kibs)
                    )
                }),
            ),
        ];
        let pipeline = [
            ("Capture", format!("{:.0} fps", snap.capture_fps)),
            (
                "Decode jobs",
                format!(
                    "{:.0}/s · {:.0}% busy",
                    snap.decode_fps,
                    snap.utilization * 100.0
                ),
            ),
            ("Codes read", format!("{:.0}/s", snap.symbol_fps)),
            (
                "Codes locked",
                reading.map_or_else(dash, |reading| {
                    format!("{} (max {})", reading.codes, reading.expected)
                }),
            ),
            (
                "Scan mode",
                match reading {
                    Some(reading) if running => {
                        format!("{} · {} scans", reading.mode.label(), totals.full_scans)
                    }
                    _ => dash(),
                },
            ),
            (
                "Workers",
                self.session()
                    .map_or(self.workers, Session::workers)
                    .to_string()
                    + &format!(" of {} cores", self.cores),
            ),
            ("Tracked hits", "n/a".to_owned()),
            (
                "Latency",
                if snap.latency.crop > 0.0 || snap.latency.full > 0.0 {
                    format!("{} · {}", ms(snap.latency.crop), ms(snap.latency.full))
                } else {
                    dash()
                },
            ),
            (
                "Dropped",
                if totals.captures > 0 {
                    format!(
                        "{} ({:.0}%)",
                        totals.dropped,
                        totals.dropped as f64 / totals.captures as f64 * 100.0
                    )
                } else {
                    dash()
                },
            ),
            (
                "Camera",
                negotiated.map_or_else(dash, |camera| {
                    format!("{}×{} @ {:.0}", camera.width, camera.height, camera.fps)
                }),
            ),
        ];
        theme::card(
            ui,
            "Telemetry",
            |ui| {
                ui.label(
                    RichText::new(decoder::ENGINE)
                        .size(theme::SMALL)
                        .monospace()
                        .color(theme::TEXT_FAINT),
                );
            },
            |ui| {
                let width = ui.available_width();
                theme::section(ui, "Transfer");
                for (label, value) in transfer {
                    let color = if value == "—" {
                        theme::TEXT_FAINT
                    } else {
                        theme::TEXT
                    };
                    theme::stat(ui, label, &value, color, width);
                }
                ui.add_space(10.0);
                theme::section(ui, "Pipeline");
                for (label, value) in pipeline {
                    let color = if value == "—" {
                        theme::TEXT_FAINT
                    } else {
                        theme::TEXT
                    };
                    let row = ui
                        .scope(|ui| theme::stat(ui, label, &value, color, width))
                        .response;
                    match label {
                        "Latency" => {
                            row.on_hover_text("Crop · full frame, milliseconds per decode");
                        }
                        "Tracked hits" => {
                            row.on_hover_text(
                                "The browser's decoder can re-read a locked code without \
                                 detecting it again. Upstream zxing-cpp has no such path, so \
                                 every crop here is a full detect-and-decode.",
                            );
                        }
                        "Camera" => {
                            if let Some(camera) = negotiated {
                                row.on_hover_text(&camera.name);
                            }
                        }
                        _ => {}
                    }
                }
            },
        );
    }

    // ---------------------------------------------------------------- centre

    fn draw_viewfinder(&self, ui: &mut egui::Ui, reading: Option<&Reading>) {
        let area = ui.available_rect_before_wrap();
        let painter = ui.painter();
        painter.rect(
            area,
            CornerRadius::same(theme::RADIUS),
            Color32::BLACK,
            Stroke::new(1.0, theme::LINE),
            StrokeKind::Inside,
        );
        let running = matches!(self.stage, Stage::Running(_));
        let [width, height] = self.preview_size;
        let showing = self.preview.is_some() && width > 0 && !matches!(self.stage, Stage::Idle);
        if !showing {
            let motif = Rect::from_center_size(area.center() - vec2(0.0, 22.0), Vec2::splat(44.0));
            let text = if matches!(self.stage, Stage::Opening { .. }) {
                egui::Spinner::new()
                    .size(28.0)
                    .paint_at(ui, motif.shrink(8.0));
                "Opening the camera…"
            } else {
                theme::paint_motif(painter, motif, theme::LINE_STRONG);
                "The camera preview appears here."
            };
            painter.text(
                area.center() + vec2(0.0, 22.0),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(theme::BODY),
                theme::TEXT_DIM,
            );
            return;
        }

        // Contain the frame in the stage, like the web viewfinder's
        // `object-fit: contain`.
        let inner = area.shrink(1.0);
        let scale = (inner.width() / width as f32).min(inner.height() / height as f32);
        let image =
            Rect::from_center_size(inner.center(), vec2(width as f32, height as f32) * scale);
        if let Some(texture) = &self.preview {
            let tint = if running {
                Color32::WHITE
            } else {
                Color32::from_gray(110)
            };
            painter.image(
                texture.id(),
                image,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                tint,
            );
        }
        let Some(reading) = reading else { return };
        if running {
            draw_brackets(
                &painter.with_clip_rect(image),
                image,
                scale,
                &reading.regions,
                reading.now,
            );
        }

        // HUD: what the scanner is doing, and how fast data is landing.
        let state = if !running {
            if matches!(self.stage, Stage::Done(_)) {
                "Done".to_owned()
            } else {
                "Starting".to_owned()
            }
        } else if reading.mode == Mode::Acquiring {
            "Searching".to_owned()
        } else {
            format!(
                "{} code{}{}",
                reading.codes,
                if reading.codes == 1 { "" } else { "s" },
                if reading.mode == Mode::Degraded {
                    format!(" of {}", reading.expected)
                } else {
                    String::new()
                }
            )
        };
        let rate = if self.snapshot.stats.is_some() && self.snapshot.live_kibs > 0.0 {
            format_rate(self.snapshot.live_kibs)
        } else {
            format!("{:.0} reads/s", self.snapshot.symbol_fps)
        };
        let tone = match reading.mode {
            Mode::Locked => theme::ACCENT,
            Mode::Degraded => theme::WARN,
            Mode::Acquiring => theme::TEXT_DIM,
        };
        hud_pill(
            painter,
            image.left_top() + vec2(12.0, 12.0),
            egui::Align2::LEFT_TOP,
            &state,
            tone,
        );
        if running {
            hud_pill(
                painter,
                image.right_top() + vec2(-12.0, 12.0),
                egui::Align2::RIGHT_TOP,
                &rate,
                theme::TEXT,
            );
        }
        let bar = Rect::from_min_max(image.left_bottom() - vec2(0.0, 4.0), image.right_bottom());
        painter.rect_filled(bar, CornerRadius::ZERO, Color32::from_black_alpha(160));
        let mut filled = bar;
        filled.set_width(bar.width() * self.fraction());
        painter.rect_filled(filled, CornerRadius::ZERO, theme::ACCENT);
    }

    fn fraction(&self) -> f32 {
        if matches!(self.stage, Stage::Done(_)) {
            return 1.0;
        }
        self.snapshot.stats.as_ref().map_or(0.0, |stats| {
            stats.solved_blocks as f32 / stats.total_blocks.max(1) as f32
        })
    }

    // ---------------------------------------------------------------- bottom

    fn draw_progress(&mut self, ui: &mut egui::Ui) {
        let done = matches!(self.stage, Stage::Done(_));
        let snap = &self.snapshot;
        let stats = snap.stats.as_ref();
        let (phase, tone) = if done {
            ("Verified", theme::ACCENT)
        } else if stats.is_some() && self.busy() {
            ("Receiving", theme::ACCENT)
        } else if matches!(self.stage, Stage::Running(_)) {
            ("Listening", theme::TEXT_DIM)
        } else {
            ("Waiting", theme::TEXT_FAINT)
        };
        let fraction = self.fraction();
        let percent = ((fraction * 100.0).floor() as u32).min(if done { 100 } else { 99 });
        theme::card(
            ui,
            "Transfer",
            |ui| {
                ui.label(
                    RichText::new(format!("{percent}%"))
                        .size(theme::HEADING)
                        .monospace()
                        .strong(),
                );
                theme::pill(ui, phase, tone);
            },
            |ui| {
                theme::track(ui, fraction, theme::ACCENT);
                ui.add_space(8.0);
                let columns = 3.0;
                let width = (ui.available_width() - 20.0 * (columns - 1.0)) / columns;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 20.0;
                    let now = if done {
                        format_rate(snap.average_kibs)
                    } else if stats.is_some() {
                        format_rate(snap.live_kibs)
                    } else {
                        "—".to_owned()
                    };
                    let remaining = if done {
                        "done".to_owned()
                    } else {
                        snap.eta
                            .map_or("—".to_owned(), |eta| format!("~{}", format_duration(eta)))
                    };
                    let elapsed = stats.map_or("—".to_owned(), |_| format_duration(snap.elapsed));
                    theme::stat(ui, "Now", &now, theme::TEXT, width);
                    theme::stat(ui, "Remaining", &remaining, theme::TEXT, width);
                    theme::stat(ui, "Elapsed", &elapsed, theme::TEXT, width);
                });
                ui.add_space(10.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 20.0;
                    let half = (ui.available_width() - 20.0) / 2.0;
                    ui.vertical(|ui| {
                        ui.set_width(half);
                        caption(
                            ui,
                            "Recovered data rate",
                            &if self.series.is_empty() {
                                String::new()
                            } else {
                                format!("peak {} · 60 s", format_rate(snap.peak_kibs))
                            },
                        );
                        draw_throughput(ui, &self.series, half);
                    });
                    ui.vertical(|ui| {
                        ui.set_width(half);
                        caption(
                            ui,
                            "Source blocks",
                            &stats.map_or("—".to_owned(), |stats| {
                                format!("{} / {}", stats.solved_blocks, stats.total_blocks)
                            }),
                        );
                        self.map.draw(ui, half);
                    });
                });
            },
        );
    }
}

/// Write a verified file where the user says. `None` when they cancel.
fn save(received: &Received) -> Option<Result<PathBuf, String>> {
    let name = safe_file_name(&received.name);
    let path = rfd::FileDialog::new()
        .set_title("Save the received file")
        .set_file_name(&name)
        .save_file()?;
    Some(
        std::fs::write(&path, &received.bytes)
            .map(|()| path)
            .map_err(|error| format!("{name} could not be saved: {error}.")),
    )
}

fn caption(ui: &mut egui::Ui, title: &str, aside: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .size(theme::SMALL)
                .color(theme::TEXT_DIM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(aside)
                    .size(theme::SMALL)
                    .monospace()
                    .color(theme::TEXT_FAINT),
            );
        });
    });
}

fn hud_pill(
    painter: &egui::Painter,
    at: egui::Pos2,
    anchor: egui::Align2,
    text: &str,
    color: Color32,
) {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        egui::FontId::monospace(theme::SMALL),
        color,
    );
    let size = galley.size() + vec2(20.0, 8.0);
    let rect = anchor.anchor_size(at, size);
    painter.rect_filled(
        rect,
        CornerRadius::same(u8::MAX),
        Color32::from_black_alpha(170),
    );
    painter.galley(rect.min + vec2(10.0, 4.0), galley, color);
}

fn reading_order(a: &Bounds, b: &Bounds) -> std::cmp::Ordering {
    let dy = a.y + a.h / 2.0 - (b.y + b.h / 2.0);
    if dy.abs() > a.h.max(b.h) / 2.0 {
        return dy.total_cmp(&0.0);
    }
    (a.x + a.w / 2.0).total_cmp(&(b.x + b.w / 2.0))
}

/// Corner brackets around each code being read, fading once it stops
/// decoding.
fn draw_brackets(
    painter: &egui::Painter,
    image: Rect,
    scale: f32,
    regions: &[(Bounds, f64)],
    now: f64,
) {
    let mut ordered: Vec<_> = regions.to_vec();
    ordered.sort_by(|a, b| reading_order(&a.0, &b.0));
    for (slot, (bounds, seen)) in ordered.iter().enumerate() {
        let age = now - seen;
        if age > BRACKET_FADE_MS {
            continue;
        }
        let alpha = (1.0 - age / BRACKET_FADE_MS) as f32;
        let color = CODE_COLORS[slot % CODE_COLORS.len()].gamma_multiply(alpha);
        let pad = 0.06 * bounds.w.max(bounds.h) * scale;
        let rect = Rect::from_min_size(
            image.min + vec2(bounds.x, bounds.y) * scale - Vec2::splat(pad),
            vec2(bounds.w, bounds.h) * scale + Vec2::splat(2.0 * pad),
        );
        let arm = 0.22 * rect.width().min(rect.height());
        let stroke = Stroke::new(2.5, color);
        for (corner, sx, sy) in [
            (rect.left_top(), 1.0, 1.0),
            (rect.right_top(), -1.0, 1.0),
            (rect.right_bottom(), -1.0, -1.0),
            (rect.left_bottom(), 1.0, -1.0),
        ] {
            painter.line(
                vec![
                    corner + vec2(0.0, sy * arm),
                    corner,
                    corner + vec2(sx * arm, 0.0),
                ],
                stroke,
            );
        }
    }
}

/// Area chart of KiB/s over a fixed one-minute axis, newest at the right.
fn draw_throughput(ui: &mut egui::Ui, points: &[f64], width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 96.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(8), theme::BG);
    for fraction in [0.25, 0.5, 0.75] {
        let y = rect.top() + rect.height() * fraction;
        painter.extend(Shape::dashed_line(
            &[pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, theme::LINE),
            2.0,
            4.0,
        ));
    }
    if points.len() < 2 {
        return;
    }
    let max = points.iter().copied().fold(8.0, f64::max) * 1.15;
    let step = rect.width() / (HISTORY_BUCKETS - 1) as f32;
    let offset = (HISTORY_BUCKETS - points.len()) as f32 * step;
    let at = |index: usize, value: f64| {
        pos2(
            rect.left() + offset + index as f32 * step,
            rect.bottom() - (value / max) as f32 * (rect.height() - 4.0) - 1.0,
        )
    };
    let fill = theme::ACCENT.linear_multiply(0.18);
    for index in 1..points.len() {
        let (a, b) = (at(index - 1, points[index - 1]), at(index, points[index]));
        painter.add(Shape::convex_polygon(
            vec![pos2(a.x, rect.bottom()), a, b, pos2(b.x, rect.bottom())],
            fill,
            Stroke::NONE,
        ));
    }
    let line: Vec<_> = points
        .iter()
        .enumerate()
        .map(|(index, value)| at(index, *value))
        .collect();
    painter.line(line, Stroke::new(1.5, theme::ACCENT));
}

/// One texel per source block, scaled up without smoothing.
#[derive(Default)]
struct BlockMap {
    texture: Option<egui::TextureHandle>,
    columns: usize,
    rows: usize,
    solved_at: Vec<f64>,
    drawn_at: f64,
}

impl BlockMap {
    /// Columns for `total` blocks at roughly a 4:1 aspect.
    fn columns(total: usize) -> usize {
        ((total as f64 * 4.0).sqrt().ceil() as usize).clamp(1, total.max(1))
    }

    /// `settled` draws without the glow: nothing repaints after completion
    /// to let it fade.
    fn update(&mut self, ctx: &egui::Context, solved: &[u8], now: f64, settled: bool) {
        self.drawn_at = now;
        let total = solved.len();
        if total == 0 {
            return;
        }
        if total != self.solved_at.len() {
            self.solved_at = vec![-1.0; total];
            self.columns = Self::columns(total);
            self.rows = total.div_ceil(self.columns);
        }
        let mut image = ColorImage::filled([self.columns, self.rows], theme::BG);
        for (index, &block) in solved.iter().enumerate() {
            if block != 0 && self.solved_at[index] < 0.0 {
                self.solved_at[index] = now;
            }
            image.pixels[index] = if block == 0 {
                theme::CELL
            } else {
                let heat = if settled {
                    0.0
                } else {
                    (1.0 - (now - self.solved_at[index]) / GLOW_MS).max(0.0) as f32
                };
                lerp(theme::ACCENT, Color32::WHITE, heat * 0.85)
            };
        }
        match &mut self.texture {
            Some(texture) => texture.set(image, TextureOptions::NEAREST),
            None => {
                self.texture = Some(ctx.load_texture("rx-blocks", image, TextureOptions::NEAREST))
            }
        }
    }

    fn draw(&self, ui: &mut egui::Ui, width: f32) {
        let height = if self.columns > 0 {
            (width * self.rows as f32 / self.columns as f32).clamp(24.0, 96.0)
        } else {
            96.0
        };
        let (rect, _) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(8), theme::BG);
        let Some(texture) = &self.texture else { return };
        let fit = (rect.width() / self.columns as f32).min(rect.height() / self.rows as f32);
        let size = vec2(self.columns as f32, self.rows as f32) * fit;
        painter.image(
            texture.id(),
            Rect::from_center_size(rect.center(), size),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

fn lerp(a: Color32, b: Color32, amount: f32) -> Color32 {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * amount).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}
