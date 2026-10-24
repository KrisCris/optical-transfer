//! Camera frames → decode jobs → the shared receiver → a verified file.
//!
//! A coordinator thread owns the scheduling, mirroring the web receiver's
//! main thread: each camera frame is planned by the [`RegionTracker`] and
//! handed to the [`DecodePool`], and each finished decode updates the tracker
//! and feeds [`ReceiverState`]. The UI never sees a frame event; it reads the
//! shared [`Core`] at its own render rate.
//!
//! Frames come from anything that can call [`Session::push`] — the camera's
//! capture queue in the app, rendered codes in the tests.

use std::{
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

use optical_core::receiver::{ReceiverEvent, ReceiverState, ReceiverStats};

use crate::receive::{
    camera::{FrameSink, LumaFrame},
    pool::{DecodePool, Done, Job},
    telemetry::{DecodeReport, Telemetry},
    tracker::{Crop, RegionTracker},
};

/// Frames waiting for the coordinator before the camera starts dropping them.
/// Planning a frame takes microseconds, so this only matters when the
/// coordinator is stalled behind a UI lock.
const MAX_QUEUED_FRAMES: usize = 2;

/// A verified file or snippet. Both checksums passed before this existed.
#[derive(Clone, Debug)]
pub struct Received {
    pub name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub is_text: bool,
}

/// Everything the receive UI reads, behind one lock.
pub struct Core {
    pub tracker: RegionTracker,
    pub telemetry: Telemetry,
    pub receiver: ReceiverState,
    pub stats: Option<ReceiverStats>,
    /// An incompatible sender, latched until a frame of ours is accepted.
    pub verdict: Option<String>,
    /// The last stream that ended without verifying.
    pub failure: Option<String>,
    /// The verified result, until the UI takes it.
    pub result: Option<Received>,
    /// Latched at completion; taking `result` does not restart the session.
    pub finished: bool,
    /// Session milliseconds of the last frame the receiver accepted.
    pub last_accepted: Option<f64>,
    /// The newest camera frame, for the viewfinder.
    pub preview: Option<Arc<LumaFrame>>,
    /// Bumped on every new preview frame.
    pub preview_serial: u64,
}

impl Core {
    pub fn completed(&self) -> bool {
        self.finished
    }

    fn plan(&mut self, now: f64, frame: &Arc<LumaFrame>, pool: &DecodePool) -> Vec<Job> {
        self.preview = Some(Arc::clone(frame));
        self.preview_serial += 1;
        if self.completed() {
            return Vec::new();
        }
        let slots = pool.free();
        if slots == 0 {
            self.telemetry.dropped(now);
            return Vec::new();
        }
        let plan = self.tracker.plan(now, slots, frame.width, frame.height);
        if !plan.full && plan.crops.is_empty() {
            return Vec::new();
        }
        self.telemetry.capture(now, plan.full, plan.crops.len());
        let whole = Crop {
            x: 0,
            y: 0,
            w: frame.width,
            h: frame.height,
        };
        plan.full
            .then_some((whole, true))
            .into_iter()
            .chain(plan.crops.into_iter().map(|crop| (crop, false)))
            .map(|(area, full)| Job {
                frame: Arc::clone(frame),
                area,
                full,
                captured_at: now,
            })
            .collect()
    }

    fn decoded(&mut self, now: f64, done: Done) {
        if self.completed() {
            return;
        }
        let reply = done.reply;
        self.telemetry.reply(
            now,
            DecodeReport {
                full: done.full,
                symbols: reply.symbols.len(),
                sightings: reply.sightings.len(),
                decode_ms: reply.decode_ms,
            },
        );
        for sighting in reply.sightings {
            self.tracker.note_sighting(sighting, now);
        }
        for symbol in reply.symbols {
            self.tracker.note_decoded(symbol.bounds, now);
            if !self.completed() {
                self.frame(&symbol.bytes, done.captured_at, now);
            }
        }
    }

    fn frame(&mut self, bytes: &[u8], captured_at: f64, now: f64) {
        match self.receiver.add_frame(bytes) {
            ReceiverEvent::Ignored => {}
            // The one failure that must not be silent: a version mismatch
            // otherwise looks exactly like bad lighting.
            ReceiverEvent::Incompatible { message } => self.verdict = Some(message),
            ReceiverEvent::Failed { message } => {
                self.failure = Some(message);
                self.stats = None;
                self.telemetry.reset_transfer();
            }
            ReceiverEvent::Progress { new_stream, stats } => {
                self.accept(new_stream, stats, captured_at, now);
            }
            ReceiverEvent::Complete {
                new_stream,
                stats,
                name,
                media_type,
                bytes,
                is_text,
            } => {
                self.accept(new_stream, stats, captured_at, now);
                self.telemetry.completed(now);
                self.finished = true;
                self.result = Some(Received {
                    name,
                    media_type,
                    bytes,
                    is_text,
                });
            }
        }
    }

    /// A frame carried information when it brought a sequence number not
    /// seen before that covered at least one block not yet solved — the same
    /// test the browser receiver's `USEFUL` flag makes.
    fn accept(&mut self, new_stream: bool, stats: ReceiverStats, captured_at: f64, now: f64) {
        let (unique, redundant) = match (&self.stats, new_stream) {
            (Some(before), false) => (before.unique_frames, before.redundant_frames),
            _ => (0, 0),
        };
        let useful = stats.unique_frames > unique && stats.redundant_frames == redundant;
        if new_stream {
            self.failure = None;
        }
        self.telemetry
            .accepted(now, captured_at, useful, new_stream, stats.block_len);
        self.telemetry.set_stats(&stats);
        self.stats = Some(stats);
        self.last_accepted = Some(now);
        self.verdict = None;
    }
}

enum Input {
    Frame(Arc<LumaFrame>, Instant),
    Decoded(Done),
    /// The pool's reply senders keep the inbox open for as long as the pool
    /// lives, so the coordinator cannot wait for the channel to close.
    Stop,
}

struct Shared {
    core: Mutex<Core>,
    clock: Instant,
    queued: AtomicUsize,
}

impl Shared {
    fn ms(&self, at: Instant) -> f64 {
        at.saturating_duration_since(self.clock).as_secs_f64() * 1000.0
    }
}

/// One receive session: from Start to a verified file or Stop.
pub struct Session {
    shared: Arc<Shared>,
    input: Option<mpsc::Sender<Input>>,
    coordinator: Option<thread::JoinHandle<()>>,
    workers: usize,
}

impl Session {
    pub fn start(workers: usize) -> Self {
        let workers = workers.max(1);
        let shared = Arc::new(Shared {
            core: Mutex::new(Core {
                tracker: RegionTracker::new(),
                telemetry: Telemetry::new(workers),
                receiver: ReceiverState::new(),
                stats: None,
                verdict: None,
                failure: None,
                result: None,
                finished: false,
                last_accepted: None,
                preview: None,
                preview_serial: 0,
            }),
            clock: Instant::now(),
            queued: AtomicUsize::new(0),
        });
        let (input, inbox) = mpsc::channel();
        let replies = input.clone();
        let state = Arc::clone(&shared);
        let coordinator = thread::Builder::new()
            .name("receive".to_owned())
            .spawn(move || {
                let pool = DecodePool::new(workers, move |done| {
                    let _ = replies.send(Input::Decoded(done));
                });
                for message in inbox {
                    match message {
                        Input::Frame(frame, at) => {
                            state.queued.fetch_sub(1, Ordering::AcqRel);
                            let now = state.ms(at);
                            let jobs = lock(&state.core).plan(now, &frame, &pool);
                            for job in jobs {
                                if !pool.submit(job) {
                                    break;
                                }
                            }
                        }
                        Input::Decoded(done) => {
                            let now = state.ms(Instant::now());
                            lock(&state.core).decoded(now, done);
                        }
                        Input::Stop => break,
                    }
                }
                // Dropping the pool here waits out the jobs in flight.
            })
            .expect("the receive thread could not start");
        Self {
            shared,
            input: Some(input),
            coordinator: Some(coordinator),
            workers,
        }
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Milliseconds on the session clock: the timebase of every telemetry
    /// and tracker timestamp.
    pub fn now(&self) -> f64 {
        self.shared.ms(Instant::now())
    }

    /// Offer one camera frame. Dropped, and counted as dropped, when the
    /// coordinator is behind.
    pub fn push(&self, frame: Arc<LumaFrame>) {
        push(&self.shared, self.input.as_ref(), frame);
    }

    /// A frame callback for the camera's capture queue.
    pub fn sink(&self) -> FrameSink {
        let shared = Arc::clone(&self.shared);
        let input = self.input.clone();
        Box::new(move |frame| push(&shared, input.as_ref(), frame))
    }

    pub fn lock(&self) -> MutexGuard<'_, Core> {
        lock(&self.shared.core)
    }
}

fn push(shared: &Shared, input: Option<&mpsc::Sender<Input>>, frame: Arc<LumaFrame>) {
    let Some(input) = input else { return };
    let at = Instant::now();
    if shared.queued.fetch_add(1, Ordering::AcqRel) >= MAX_QUEUED_FRAMES {
        shared.queued.fetch_sub(1, Ordering::AcqRel);
        lock(&shared.core).telemetry.dropped(shared.ms(at));
        return;
    }
    if input.send(Input::Frame(frame, at)).is_err() {
        shared.queued.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A panic on another thread poisons nothing worth refusing to read.
fn lock(core: &Mutex<Core>) -> MutexGuard<'_, Core> {
    core.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(input) = self.input.take() {
            let _ = input.send(Input::Stop);
        }
        if let Some(coordinator) = self.coordinator.take() {
            let _ = coordinator.join();
        }
    }
}
